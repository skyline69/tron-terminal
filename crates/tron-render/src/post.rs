//! User WGSL post-processing shaders, applied in order to the rendered terminal.
//!
//! Shaders that call `previous(uv)` read the final output of the previous frame.
//! For those, the last pass renders into one of two history textures, which is
//! then copied to the surface; the textures swap roles every frame. Without
//! such a shader no history is kept.
//!
//! Pipelines are compiled by [`crate::compile::Compiler`] in the background. A
//! chain keeps drawing its current shaders until the requested ones are ready.

use std::mem::size_of;

use bytemuck::{Pod, Zeroable};

use crate::compile::{Compiled, Compiler};

pub const PRELUDE: &str = include_str!("post_prelude.wgsl");

pub const BLIT: &str = r"
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) index: u32) -> VertexOut {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VertexOut;
    out.position = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs(v: VertexOut) -> @location(0) vec4<f32> {
    return textureSampleLevel(source, source_sampler, v.uv, 0.0);
}
";

/// A user shader to compile.
#[derive(Clone, Debug)]
pub struct PostShader {
    pub name: String,
    pub source: String,
}

/// Matches `TronUniforms` in the prelude. 112 bytes, aligned to 16.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Default)]
pub struct PostUniforms {
    pub resolution: [f32; 2],
    pub time: f32,
    pub frame: u32,
    pub cursor: [f32; 4],
    pub cell_size: [f32; 2],
    pub focused: f32,
    pub _padding: f32,
    pub background: [f32; 4],
    pub previous_cursor: [f32; 4],
    pub cursor_change_time: f32,
    pub scene: u32,
    pub _padding2: [f32; 2],
    pub params: [f32; 4],
}

/// Layouts shared by every chain and the compiler.
#[derive(Clone)]
pub struct Layouts {
    pub format: wgpu::TextureFormat,
    pub bind_group: wgpu::BindGroupLayout,
    pub pipeline: wgpu::PipelineLayout,
    pub blit_bind_group: wgpu::BindGroupLayout,
    pub blit_pipeline: wgpu::PipelineLayout,
    pub cache: Option<wgpu::PipelineCache>,
}

impl Layouts {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, cache: Option<&wgpu::PipelineCache>) -> Self {
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let sampler = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let bind_group = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture(1),
                sampler(2),
                texture(3),
            ],
        });
        let pipeline = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("post"),
            bind_group_layouts: &[Some(&bind_group)],
            immediate_size: 0,
        });
        let blit_bind_group = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post blit"),
            entries: &[texture(0), sampler(1)],
        });
        let blit_pipeline = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("post blit"),
            bind_group_layouts: &[Some(&blit_bind_group)],
            immediate_size: 0,
        });
        Self { format, bind_group, pipeline, blit_bind_group, blit_pipeline, cache: cache.cloned() }
    }
}

struct Target {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

pub struct PostChain {
    format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    blit_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    passes: Vec<wgpu::RenderPipeline>,
    /// Intermediate render targets, the first one receives the terminal.
    targets: Vec<Target>,
    /// History textures, only when a shader reads the previous frame.
    history: Vec<Target>,
    /// History texture written by the last frame.
    history_current: usize,
    /// Bind groups indexed by `[target][history read index]`.
    bind_groups: Vec<Vec<wgpu::BindGroup>>,
    blit: Option<wgpu::RenderPipeline>,
    blit_bind_groups: Vec<wgpu::BindGroup>,
    size: (u32, u32),
    animated: bool,
    uses_previous: bool,
    uses_cursor_motion: bool,
    /// Counts requests, so results of superseded ones are dropped.
    generation: u64,
    animation: Option<bool>,
}

impl PostChain {
    pub fn new(device: &wgpu::Device, layouts: &Layouts) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("post"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("post uniforms"),
            size: size_of::<PostUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            format: layouts.format,
            layout: layouts.bind_group.clone(),
            blit_layout: layouts.blit_bind_group.clone(),
            sampler,
            uniforms,
            passes: Vec::new(),
            targets: Vec::new(),
            history: Vec::new(),
            history_current: 0,
            bind_groups: Vec::new(),
            blit: None,
            blit_bind_groups: Vec::new(),
            size: (0, 0),
            animated: false,
            uses_previous: false,
            uses_cursor_motion: false,
            generation: 0,
            animation: None,
        }
    }

    pub fn is_active(&self) -> bool {
        !self.passes.is_empty()
    }

    /// Whether the chain must redraw every frame.
    pub fn is_animated(&self) -> bool {
        self.is_active() && self.animated
    }

    /// Whether a shader reads the cursor change uniforms, and so animates for a
    /// while after the cursor moves.
    pub fn uses_cursor_motion(&self) -> bool {
        self.is_active() && self.uses_cursor_motion
    }

    /// Asks for `shaders` to replace the chain once compiled. An empty list
    /// takes effect at once.
    ///
    /// `animation` forces continuous redraws on or off. `None` animates when a
    /// shader reads `tron.time` or `tron.frame`. Shaders that read
    /// `tron.cursor_change_time` only animate after cursor moves.
    pub fn request(
        &mut self,
        device: &wgpu::Device,
        compiler: &Compiler,
        chain: usize,
        shaders: &[PostShader],
        animation: Option<bool>,
    ) {
        self.generation += 1;
        self.animation = animation;
        if shaders.is_empty() {
            let empty =
                Compiled { chain, generation: self.generation, passes: Vec::new(), blit: None, errors: Vec::new() };
            self.apply(device, empty);
        } else {
            compiler.compile(chain, self.generation, shaders.to_vec());
        }
    }

    /// Installs a compiled chain. Returns its compile errors, or `None` when a
    /// newer request superseded it.
    pub fn apply(&mut self, device: &wgpu::Device, compiled: Compiled) -> Option<Vec<String>> {
        if compiled.generation != self.generation {
            return None;
        }
        let reads = compiled.passes.iter().map(|(_, reads)| *reads);
        self.animated = self.animation.unwrap_or(reads.clone().any(|r| r.time));
        self.uses_cursor_motion = self.animation.is_none() && reads.clone().any(|r| r.cursor_motion);
        self.uses_previous = compiled.blit.is_some();
        self.passes = compiled.passes.into_iter().map(|(pipeline, _)| pipeline).collect();
        self.blit = compiled.blit;
        let size = self.size;
        self.size = (0, 0);
        self.resize(device, size.0, size.1);
        Some(compiled.errors)
    }

    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let size = (width.max(1), height.max(1));
        if !self.is_active() {
            self.targets.clear();
            self.history.clear();
            self.bind_groups.clear();
            self.blit_bind_groups.clear();
            self.size = size;
            return;
        }
        if size == self.size && !self.targets.is_empty() {
            return;
        }
        self.size = size;
        let texture = |label| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            Target { _texture: texture, view }
        };
        let count = if self.passes.len() > 1 { 2 } else { 1 };
        self.targets = (0..count).map(|i| texture(if i == 0 { "post target a" } else { "post target b" })).collect();
        // New history textures start zeroed: the previous frame reads as transparent.
        self.history =
            if self.uses_previous { vec![texture("post history a"), texture("post history b")] } else { Vec::new() };
        self.history_current = 0;

        let bind_group = |input: &wgpu::TextureView, previous: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("post"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.uniforms.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(input) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(previous) },
                ],
            })
        };
        self.bind_groups = self
            .targets
            .iter()
            .map(|target| {
                if self.history.is_empty() {
                    // Nothing reads binding 3; the input view fills it at no cost.
                    vec![bind_group(&target.view, &target.view)]
                } else {
                    self.history.iter().map(|history| bind_group(&target.view, &history.view)).collect()
                }
            })
            .collect();
        self.blit_bind_groups = self
            .history
            .iter()
            .map(|history| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("post blit"),
                    layout: &self.blit_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&history.view),
                        },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                    ],
                })
            })
            .collect();
    }

    /// Where the terminal is rendered when the chain is active.
    pub fn input_view(&self) -> Option<&wgpu::TextureView> {
        self.targets.first().map(|t| &t.view)
    }

    pub fn run(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
        uniforms: &PostUniforms,
    ) {
        if self.targets.is_empty() {
            return;
        }
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(uniforms));
        let with_history = self.history.len() == 2;
        let read = self.history_current;
        let write = 1 - read;
        let last = self.passes.len() - 1;
        for (i, pipeline) in self.passes.iter().enumerate() {
            let input = i % self.targets.len();
            let view = match (i == last, with_history) {
                (true, true) => &self.history[write].view,
                (true, false) => output,
                (false, _) => &self.targets[(i + 1) % self.targets.len()].view,
            };
            let bind_group = &self.bind_groups[input][if with_history { read } else { 0 }];
            draw(encoder, view, pipeline, bind_group);
        }
        if with_history && let Some(blit) = &self.blit {
            draw(encoder, output, blit, &self.blit_bind_groups[write]);
            self.history_current = write;
        }
    }
}

fn draw(
    encoder: &mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("post"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
        })],
        ..Default::default()
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind_group, &[]);
    pass.draw(0..3, 0..1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniforms_match_the_prelude_layout() {
        assert_eq!(size_of::<PostUniforms>(), 112);
        assert_eq!(std::mem::offset_of!(PostUniforms, scene), 84);
        assert_eq!(std::mem::offset_of!(PostUniforms, params), 96);
        assert_eq!(std::mem::offset_of!(PostUniforms, background), 48);
        assert_eq!(std::mem::offset_of!(PostUniforms, previous_cursor), 64);
        assert_eq!(std::mem::offset_of!(PostUniforms, cursor_change_time), 80);
        assert!(PRELUDE.contains("previous_cursor: vec4<f32>") && PRELUDE.contains("fn previous("));
    }
}
