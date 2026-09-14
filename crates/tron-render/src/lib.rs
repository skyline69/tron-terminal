//! GPU renderer.
//!
//! A frame is drawn in up to five steps inside one render pass: images below
//! cell backgrounds, backgrounds and cursor, images below text, text and
//! decorations, images above text. With user shaders configured, the pass
//! renders into a texture that the shader chain then processes.
//!
//! Cell instances are cached per row and rebuilt only for damaged rows.

mod atlas;
mod cells;
mod images;
mod post;

use std::path::PathBuf;
use std::time::Instant;

use tron_core::Snapshot;
use tron_font::{CellMetrics, FontSystem};

use cells::CellPipeline;
use images::{ImagePipeline, Layer};
use post::{PostChain, PostUniforms};

pub use post::PostShader;

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("failed to create surface: {0}")]
    Surface(#[from] wgpu::CreateSurfaceError),
    #[error("no compatible GPU adapter: {0}")]
    Adapter(#[from] wgpu::RequestAdapterError),
    #[error("failed to create GPU device: {0}")]
    Device(#[from] wgpu::RequestDeviceError),
    #[error("the window surface is not supported by the GPU adapter")]
    Unsupported,
}

/// Colors the terminal palette does not cover. sRGB bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub cursor_text: Option<[u8; 3]>,
    pub selection_background: [u8; 3],
    pub selection_foreground: Option<[u8; 3]>,
    /// Opacity of the default background. Fixed at creation.
    pub opacity: f32,
}

impl Default for Theme {
    fn default() -> Self {
        Self { cursor_text: None, selection_background: [0x1f, 0x4a, 0x6b], selection_foreground: None, opacity: 1.0 }
    }
}

/// An initialized GPU device, not yet bound to a window.
pub struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl Gpu {
    /// Opens the GPU. This is the slowest part of startup, so it can run on a
    /// background thread before the window exists.
    pub async fn new() -> Result<Self, RenderError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: false,
                compatible_surface: None,
                apply_limit_buckets: false,
            })
            .await?;
        let info = adapter.get_info();
        log::info!("GPU: {} ({:?}, {})", info.name, info.backend, info.driver);
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("tron"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::Off,
            })
            .await?;
        Ok(Self { instance, adapter, device, queue })
    }
}

pub struct Renderer {
    _instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    srgb_output: bool,
    cells: CellPipeline,
    images: ImagePipeline,
    post: PostChain,
    theme: Theme,
    background: [u8; 3],
    focused: bool,
    started: Instant,
    post_frame: u32,
    capture: Option<PathBuf>,
}

impl Renderer {
    /// Creates a renderer for a window. `padding` is in physical pixels.
    pub async fn new(
        target: impl Into<wgpu::SurfaceTarget<'static>>,
        width: u32,
        height: u32,
        metrics: CellMetrics,
        padding: [f32; 2],
        theme: Theme,
    ) -> Result<Self, RenderError> {
        let gpu = Gpu::new().await?;
        Self::with_gpu(gpu, target, width, height, metrics, padding, theme)
    }

    /// Creates a renderer from a GPU initialized earlier, typically on a
    /// background thread while the window was being created.
    pub fn with_gpu(
        gpu: Gpu,
        target: impl Into<wgpu::SurfaceTarget<'static>>,
        width: u32,
        height: u32,
        metrics: CellMetrics,
        padding: [f32; 2],
        theme: Theme,
    ) -> Result<Self, RenderError> {
        let Gpu { instance, adapter, device, queue } = gpu;
        let surface = instance.create_surface(target)?;
        if !adapter.is_surface_supported(&surface) {
            return Err(RenderError::Unsupported);
        }
        let caps = surface.get_capabilities(&adapter);
        // 8-bit non-sRGB output blends text in gamma space like most terminals,
        // which keeps glyph weight consistent across displays.
        let preferred = [wgpu::TextureFormat::Bgra8Unorm, wgpu::TextureFormat::Rgba8Unorm];
        let format = preferred
            .into_iter()
            .find(|f| caps.formats.contains(f))
            .or_else(|| caps.formats.iter().copied().find(|f| !f.is_srgb()))
            .or_else(|| caps.formats.first().copied())
            .ok_or(RenderError::Unsupported)?;
        log::info!("surface format {format:?}, available {:?}", caps.formats);
        let mut config = surface
            .get_default_config(&adapter, width.max(1), height.max(1))
            .ok_or(RenderError::Unsupported)?;
        config.format = format;
        config.view_formats.clear();
        config.present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        };
        config.alpha_mode = if theme.opacity < 1.0 && caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied) {
            wgpu::CompositeAlphaMode::PreMultiplied
        } else if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
            wgpu::CompositeAlphaMode::Opaque
        } else {
            caps.alpha_modes[0]
        };
        config.desired_maximum_frame_latency = 1;
        surface.configure(&device, &config);

        let cells = CellPipeline::new(&device, format, metrics, padding);
        let images = ImagePipeline::new(&device, format);
        let mut post = PostChain::new(&device, format);
        post.resize(&device, config.width, config.height);

        Ok(Self {
            srgb_output: format.is_srgb(),
            _instance: instance,
            surface,
            device,
            queue,
            config,
            cells,
            images,
            post,
            theme,
            background: [0; 3],
            focused: true,
            started: Instant::now(),
            post_frame: 0,
            capture: None,
        })
    }

    /// Allows reading presented frames back. Returns false when the surface does not support it.
    pub fn enable_capture(&mut self) -> bool {
        self.config.usage |= wgpu::TextureUsages::COPY_SRC;
        self.surface.configure(&self.device, &self.config);
        true
    }

    /// Saves the next presented frame as a PNG file.
    pub fn capture_next_frame(&mut self, path: PathBuf) {
        self.capture = Some(path);
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        if width == self.config.width && height == self.config.height {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.post.resize(&self.device, width, height);
        self.cells.invalidate();
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Grid size in cells that fits the surface.
    pub fn grid_size(&self) -> (usize, usize) {
        let m = self.cells.metrics();
        let padding = self.cells.padding();
        let usable_w = (self.config.width as f32 - 2.0 * padding[0]).max(0.0);
        let usable_h = (self.config.height as f32 - 2.0 * padding[1]).max(0.0);
        (((usable_w / m.width as f32) as usize).max(1), ((usable_h / m.height as f32) as usize).max(1))
    }

    /// Viewport cell under a pixel position, clamped to the grid.
    pub fn cell_at(&self, x: f64, y: f64) -> (usize, usize) {
        let m = self.cells.metrics();
        let padding = self.cells.padding();
        let (cols, rows) = self.grid_size();
        let col = ((x as f32 - padding[0]) / m.width as f32).max(0.0) as usize;
        let row = ((y as f32 - padding[1]) / m.height as f32).max(0.0) as usize;
        (row.min(rows - 1), col.min(cols - 1))
    }

    /// Call after the font size, font or scale factor changed.
    pub fn set_metrics(&mut self, metrics: CellMetrics, padding: [f32; 2]) {
        self.cells.set_metrics(&self.device, metrics, padding);
    }

    pub fn set_theme(&mut self, theme: Theme) {
        if theme != self.theme {
            self.theme = theme;
            self.cells.invalidate();
        }
    }

    /// Replaces the post-processing chain. Returns compile errors.
    /// `animation`: `Some(true)` always redraw, `Some(false)` never, `None` auto.
    pub fn set_shaders(&mut self, shaders: &[PostShader], animation: Option<bool>) -> Vec<String> {
        let errors = self.post.set_shaders(&self.device, shaders, animation);
        self.post.resize(&self.device, self.config.width, self.config.height);
        self.post_frame = 0;
        errors
    }

    /// Whether frames should be drawn continuously for shader animation.
    pub fn is_animated(&self) -> bool {
        self.post.is_animated()
    }

    /// Builds the frame from a terminal snapshot.
    pub fn prepare(&mut self, snapshot: &Snapshot, fonts: &mut FontSystem, focused: bool) {
        let viewport = [self.config.width as f32, self.config.height as f32];
        self.focused = focused;
        self.background = snapshot.palette.background;
        self.cells.prepare(&self.device, &self.queue, snapshot, fonts, &self.theme, focused, self.srgb_output, viewport);
        self.images.prepare(&self.device, &self.queue, snapshot, self.cells.metrics(), self.cells.padding(), self.srgb_output);
    }

    /// Presents the prepared frame.
    pub fn render(&mut self) {
        let (frame, reconfigure) = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => (frame, false),
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => (frame, true),
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                log::error!("surface validation error");
                return;
            }
        };
        let viewport = [self.config.width as f32, self.config.height as f32];
        self.cells.upload(&self.device, &self.queue);
        self.images.upload_instances(&self.device, &self.queue, viewport);

        let background = cells::to_rgba(self.background, self.srgb_output);
        let alpha = self.theme.opacity.clamp(0.0, 1.0);
        let clear = wgpu::Color {
            r: f64::from(background[0] * alpha),
            g: f64::from(background[1] * alpha),
            b: f64::from(background[2] * alpha),
            a: f64::from(alpha),
        };
        let surface_view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
        {
            let target = self.post.input_view().unwrap_or(&surface_view);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("terminal"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(clear), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            let (backgrounds, text) = self.cells.ranges();
            self.images.draw(&mut pass, Layer::BelowBackground);
            self.cells.draw(&mut pass, backgrounds);
            self.images.draw(&mut pass, Layer::BelowText);
            self.cells.draw(&mut pass, text);
            self.images.draw(&mut pass, Layer::AboveText);
        }
        if self.post.is_active() {
            let m = self.cells.metrics();
            let uniforms = PostUniforms {
                resolution: viewport,
                time: self.started.elapsed().as_secs_f32(),
                frame: self.post_frame,
                cursor: self.cells.cursor_rect(),
                cell_size: [m.width as f32, m.height as f32],
                focused: if self.focused { 1.0 } else { 0.0 },
                _padding: 0.0,
                background: [clear.r as f32, clear.g as f32, clear.b as f32, clear.a as f32],
            };
            self.post.run(&self.queue, &mut encoder, &surface_view, &uniforms);
            self.post_frame = self.post_frame.wrapping_add(1);
        }
        let capture = self.capture.take().map(|path| {
            let (width, height) = (self.config.width, self.config.height);
            let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let padded = (4 * width).div_ceil(align) * align;
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("capture"),
                size: u64::from(padded) * u64::from(height),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            encoder.copy_texture_to_buffer(
                frame.texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(height) },
                },
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
            (path, buffer, padded)
        });
        self.queue.submit([encoder.finish()]);
        if let Some((path, buffer, padded)) = capture {
            match self.save_capture(&buffer, padded, &path) {
                Ok(()) => log::info!("saved frame to {}", path.display()),
                Err(error) => log::error!("frame capture failed: {error}"),
            }
        }
        self.queue.present(frame);
        if reconfigure {
            self.surface.configure(&self.device, &self.config);
        }
    }

    fn save_capture(&self, buffer: &wgpu::Buffer, padded_row: u32, path: &std::path::Path) -> Result<(), String> {
        let (width, height) = (self.config.width as usize, self.config.height as usize);
        let slice = buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| e.to_string())?;
        receiver.recv().map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
        let data = slice.get_mapped_range().map_err(|e| e.to_string())?;
        let format = self.config.format;
        let mut rgba = Vec::with_capacity(width * height * 4);
        for row in data.chunks(padded_row as usize).take(height) {
            for pixel in row[..width * 4].as_chunks::<4>().0 {
                let rgb = match format {
                    wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => [pixel[2], pixel[1], pixel[0]],
                    wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => [pixel[0], pixel[1], pixel[2]],
                    wgpu::TextureFormat::Rgb10a2Unorm => {
                        let v = u32::from_le_bytes(*pixel);
                        [(v >> 2) as u8, (v >> 12) as u8, (v >> 22) as u8]
                    }
                    other => return Err(format!("capture does not support {other:?}")),
                };
                rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width as u32, height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&rgba).map_err(|e| e.to_string())
    }
}
