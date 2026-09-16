//! Compiles post-processing pipelines on a background thread.
//!
//! Creating a render pipeline compiles its shader for the GPU, which takes about
//! a tenth of a second per shader on Metal. On the event loop that froze the
//! window, for example while scrolling through shaders on the startup screen.
//! Pipelines are kept by source, so a shader shown before switches instantly.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use crate::post::{BLIT, BLUR_SOURCE, DOWNSAMPLE, Layouts, PRELUDE, PostShader};

/// Compiled pipelines kept before the cache is cleared. tron ships about 50 shaders.
const MAX_CACHED: usize = 128;
/// Cache key of the pipeline that copies the history texture to the output.
const BLIT_KEY: &str = "\0blit";
/// Cache key of the pipeline that fills the mip levels `terminal_blur` reads.
const DOWNSAMPLE_KEY: &str = "\0downsample";

/// What a shader reads, which decides when its chain redraws.
#[derive(Copy, Clone, Default)]
pub struct Reads {
    /// `tron.time` or `tron.frame`.
    pub time: bool,
    /// `previous(uv)`.
    pub previous: bool,
    /// `tron.cursor_change_time` or `tron.previous_cursor`.
    pub cursor_motion: bool,
    /// Seconds a cursor effect lasts, from `const TRON_CURSOR_DURATION`.
    pub cursor_duration: Option<f32>,
    /// `terminal_blur(`.
    pub blur: bool,
    /// Defines `blur_source`, applied to the terminal before it is blurred.
    pub blur_source: bool,
}

impl Reads {
    fn of(source: &str) -> Self {
        let reads = |name: &str| source.contains(name);
        let cursor_motion = reads("tron.cursor_change_time") || reads("tron.previous_cursor");
        Self {
            // Cursor shaders animate only after the cursor moves, even when they read the time.
            time: !cursor_motion && (reads("tron.time") || reads("tron.frame")),
            previous: reads("previous("),
            cursor_motion,
            cursor_duration: cursor_duration(source),
            blur: reads("terminal_blur("),
            blur_source: reads("fn blur_source("),
        }
    }
}

/// The value of `const TRON_CURSOR_DURATION: f32 = <seconds>;`, when declared.
fn cursor_duration(source: &str) -> Option<f32> {
    const NAME: &str = "TRON_CURSOR_DURATION";
    let (at, _) = source.match_indices(NAME).find(|(at, _)| {
        let before = source[..*at].trim_end();
        before.ends_with("const") && before.len() < *at
    })?;
    let rest = source[at + NAME.len()..].trim_start();
    let rest = rest.strip_prefix(':').map_or(rest, |typed| typed.trim_start().strip_prefix("f32").unwrap_or(typed));
    let value = rest.trim_start().strip_prefix('=')?;
    let value = value[..value.find(';')?].trim();
    let seconds: f32 = value.strip_suffix('f').unwrap_or(value).parse().ok()?;
    (seconds.is_finite() && seconds >= 0.0).then_some(seconds)
}

/// One shader of a chain.
pub struct Pass {
    pub pipeline: wgpu::RenderPipeline,
    pub reads: Reads,
    /// Draws the first blurred level through the shader's `blur_source`.
    pub blur_source: Option<wgpu::RenderPipeline>,
}

/// A chain compiled for [`Compiler::compile`].
pub struct Compiled {
    pub chain: usize,
    pub generation: u64,
    pub passes: Vec<Pass>,
    /// Present when a pass reads the previous frame.
    pub blit: Option<wgpu::RenderPipeline>,
    /// Present when a pass reads `terminal_blur`.
    pub downsample: Option<wgpu::RenderPipeline>,
    pub errors: Vec<String>,
}

enum Request {
    Chain { chain: usize, generation: u64, shaders: Vec<PostShader> },
    Warm(Vec<PostShader>),
}

type Notify = Box<dyn Fn() + Send>;

pub struct Compiler {
    sender: Sender<Request>,
    results: Arc<Mutex<Vec<Compiled>>>,
    notify: Arc<Mutex<Option<Notify>>>,
    thread: Option<JoinHandle<()>>,
}

impl Compiler {
    pub fn new(device: wgpu::Device, layouts: Layouts) -> Self {
        let (sender, receiver) = channel();
        let results = Arc::new(Mutex::new(Vec::new()));
        let notify = Arc::new(Mutex::new(None));
        let worker =
            Worker { device, layouts, pipelines: HashMap::new(), results: results.clone(), notify: notify.clone() };
        let thread = std::thread::Builder::new()
            .name("shader-compiler".into())
            .spawn(move || worker.run(receiver))
            .map_err(|error| log::error!("cannot start the shader compiler, shaders are disabled: {error}"))
            .ok();
        Self { sender, results, notify, thread }
    }

    /// Called from the compiler thread whenever a chain is ready to be taken.
    pub fn set_notify(&self, notify: impl Fn() + Send + 'static) {
        *lock(&self.notify) = Some(Box::new(notify));
    }

    /// Compiles a chain. Only the newest request of each chain is compiled.
    pub fn compile(&self, chain: usize, generation: u64, shaders: Vec<PostShader>) {
        let _ = self.sender.send(Request::Chain { chain, generation, shaders });
    }

    /// Compiles shaders ahead of use, whenever no chain is waiting.
    pub fn warm(&self, shaders: Vec<PostShader>) {
        let _ = self.sender.send(Request::Warm(shaders));
    }

    pub fn take_results(&self) -> Vec<Compiled> {
        std::mem::take(&mut *lock(&self.results))
    }
}

impl Drop for Compiler {
    /// Waits for the worker to release its device and pipelines. A worker still
    /// running when `main` returns destroys the Vulkan device while the driver's
    /// exit handlers run, which crashes the NVIDIA driver.
    fn drop(&mut self) {
        // Disconnect the channel so the worker returns after its current compile.
        self.sender = channel().0;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Worker {
    device: wgpu::Device,
    layouts: Layouts,
    /// Pipeline or compile error by shader source.
    pipelines: HashMap<String, Result<wgpu::RenderPipeline, String>>,
    results: Arc<Mutex<Vec<Compiled>>>,
    notify: Arc<Mutex<Option<Notify>>>,
}

impl Worker {
    fn run(mut self, receiver: Receiver<Request>) {
        let mut warm: Vec<PostShader> = Vec::new();
        loop {
            // Block for work only when there is nothing left to warm.
            let mut next = if warm.is_empty() {
                match receiver.recv() {
                    Ok(request) => Some(request),
                    Err(_) => return,
                }
            } else {
                None
            };
            let mut chains: Vec<(usize, u64, Vec<PostShader>)> = Vec::new();
            loop {
                match next.take() {
                    Some(Request::Chain { chain, generation, shaders }) => {
                        chains.retain(|(queued, _, _)| *queued != chain);
                        chains.push((chain, generation, shaders));
                    }
                    Some(Request::Warm(shaders)) => warm.extend(shaders),
                    None => {}
                }
                match receiver.try_recv() {
                    Ok(request) => next = Some(request),
                    Err(TryRecvError::Empty) => break,
                    // The renderer is gone.
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            for (chain, generation, shaders) in chains {
                let compiled = self.chain(chain, generation, &shaders);
                lock(&self.results).push(compiled);
                if let Some(notify) = &*lock(&self.notify) {
                    notify();
                }
            }
            // One shader at a time, so a new chain request waits at most one compile.
            if let Some(shader) = warm.pop() {
                let _ = self.pipeline(&shader);
            }
        }
    }

    fn chain(&mut self, chain: usize, generation: u64, shaders: &[PostShader]) -> Compiled {
        let mut passes = Vec::with_capacity(shaders.len());
        let mut errors = Vec::new();
        for shader in shaders {
            let reads = Reads::of(&shader.source);
            let pass = self.pipeline(shader).and_then(|pipeline| {
                let blur_source = if reads.blur && reads.blur_source { Some(self.blur_source(shader)?) } else { None };
                Ok(Pass { pipeline, reads, blur_source })
            });
            match pass {
                Ok(pass) => passes.push(pass),
                Err(error) => errors.push(format!(
                    "shader `{}` failed to compile (line numbers include {} prelude lines):\n{error}",
                    shader.name,
                    PRELUDE.lines().count()
                )),
            }
        }
        let blit =
            if passes.iter().any(|pass| pass.reads.previous) { self.shared(BLIT_KEY, "post blit", BLIT) } else { None };
        let downsample = if passes.iter().any(|pass| pass.reads.blur) {
            self.shared(DOWNSAMPLE_KEY, "post downsample", DOWNSAMPLE)
        } else {
            None
        };
        Compiled { chain, generation, passes, blit, downsample, errors }
    }

    fn pipeline(&mut self, shader: &PostShader) -> Result<wgpu::RenderPipeline, String> {
        self.cached(shader.source.clone(), |worker| {
            let source = format!("{PRELUDE}{}", shader.source);
            worker.build(&shader.name, &source, "tron_vs", "tron_fs", &worker.layouts.pipeline)
        })
    }

    /// The pipeline that draws a shader's `blur_source` into the first blurred level.
    fn blur_source(&mut self, shader: &PostShader) -> Result<wgpu::RenderPipeline, String> {
        self.cached(format!("\0blur source\0{}", shader.source), |worker| {
            let source = format!("{PRELUDE}{}{BLUR_SOURCE}", shader.source);
            worker.build(&shader.name, &source, "tron_vs", "tron_blur_source_fs", &worker.layouts.pipeline)
        })
    }

    fn cached(
        &mut self,
        key: String,
        build: impl FnOnce(&Self) -> Result<wgpu::RenderPipeline, String>,
    ) -> Result<wgpu::RenderPipeline, String> {
        if let Some(cached) = self.pipelines.get(&key) {
            return cached.clone();
        }
        if self.pipelines.len() >= MAX_CACHED {
            self.pipelines.clear();
        }
        let result = build(self);
        self.pipelines.insert(key, result.clone());
        result
    }

    /// A pipeline of tron's own that reads one texture, shared by every chain.
    fn shared(&mut self, key: &str, label: &str, source: &str) -> Option<wgpu::RenderPipeline> {
        if !self.pipelines.contains_key(key) {
            let result = self.build(label, source, "vs", "fs", &self.layouts.blit_pipeline);
            self.pipelines.insert(key.to_owned(), result);
        }
        match &self.pipelines[key] {
            Ok(pipeline) => Some(pipeline.clone()),
            Err(error) => {
                log::error!("{label} failed to compile: {error}");
                None
            }
        }
    }

    fn build(
        &self,
        label: &str,
        source: &str,
        vertex: &str,
        fragment: &str,
        layout: &wgpu::PipelineLayout,
    ) -> Result<wgpu::RenderPipeline, String> {
        // Error scopes belong to the thread that pushes them.
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some(vertex),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some(fragment),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: self.layouts.format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: self.layouts.cache.as_ref(),
        });
        match pollster::block_on(scope.pop()) {
            Some(error) => Err(error.to_string()),
            None => Ok(pipeline),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_duration_is_read_from_the_constant() {
        assert_eq!(cursor_duration("const TRON_CURSOR_DURATION: f32 = 0.2;"), Some(0.2));
        assert_eq!(cursor_duration("const TRON_CURSOR_DURATION = 1.5f;"), Some(1.5));
        assert_eq!(cursor_duration("const  TRON_CURSOR_DURATION :f32= 0.09 ;"), Some(0.09));
        assert_eq!(cursor_duration("const TRON_CURSOR_DURATION: f32 = DURATION;"), None);
        assert_eq!(cursor_duration("const TRON_CURSOR_DURATION: f32 = -1.0;"), None);
        assert_eq!(cursor_duration("const DURATION: f32 = 0.2;"), None);
    }
}
