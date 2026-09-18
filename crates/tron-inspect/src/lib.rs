//! The inspector: a window of its own showing what tron is doing, drawn with egui.
//!
//! The window never touches the terminal. Once a frame the session copies its
//! readings into a [`Report`], which is all the interface draws from.

mod input;
mod report;
mod ui;

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_wgpu::wgpu;
use winit::event::WindowEvent;
use winit::window::{Window, WindowId};

pub use report::*;
pub use ui::Tab;

/// How often the inspector redraws: readings move as the terminal does, so they
/// are drawn at about the rate of a display, focused or not.
const REFRESH: Duration = Duration::from_millis(16);

/// How often it redraws while the readings are frozen and nothing can change.
const REFRESH_PAUSED: Duration = Duration::from_millis(200);

/// What the inspector wants from the session after an event.
#[derive(Default)]
pub struct Outcome {
    /// The window asked to close.
    pub close: bool,
    /// A frame should be drawn.
    pub redraw: bool,
}

#[derive(Debug)]
pub enum Error {
    Surface(wgpu::CreateSurfaceError),
    Unsupported,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Surface(error) => write!(f, "inspector surface: {error}"),
            Self::Unsupported => write!(f, "the GPU cannot draw the inspector window"),
        }
    }
}

impl std::error::Error for Error {}

pub struct Inspector {
    window: Arc<dyn Window>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    painter: egui_wgpu::Renderer,
    context: egui::Context,
    input: input::Input,
    state: ui::State,
    started: Instant,
    /// The pointer the window shows, so it is only set when it changes.
    cursor: egui::CursorIcon,
    /// Text the interface asked to put on the clipboard.
    copied: Option<String>,
}

impl Inspector {
    /// Opens the inspector on `window`, drawing with the window's own GPU.
    pub fn new(gpu: &tron_render::Gpu, window: Arc<dyn Window>) -> Result<Self, Error> {
        let (instance, adapter, device, queue) = gpu.handles();
        let surface = instance.create_surface(window.clone()).map_err(Error::Surface)?;
        let size = window.surface_size();
        let mut config =
            surface.get_default_config(adapter, size.width.max(1), size.height.max(1)).ok_or(Error::Unsupported)?;
        // egui blends in gamma space, like the terminal, and warns about sRGB surfaces.
        let caps = surface.get_capabilities(adapter);
        let preferred = [wgpu::TextureFormat::Bgra8Unorm, wgpu::TextureFormat::Rgba8Unorm];
        if let Some(format) = preferred.into_iter().find(|format| caps.formats.contains(format)) {
            config.format = format;
            config.view_formats.clear();
        }
        config.present_mode = wgpu::PresentMode::AutoVsync;
        config.desired_maximum_frame_latency = 1;
        surface.configure(device, &config);

        let context = egui::Context::default();
        context.set_pixels_per_point(window.scale_factor() as f32);
        let options = egui_wgpu::RendererOptions { ..Default::default() };
        let painter = egui_wgpu::Renderer::new(device, config.format, options);
        Ok(Self {
            window,
            device: device.clone(),
            queue: queue.clone(),
            surface,
            config,
            painter,
            context,
            input: input::Input::default(),
            state: ui::State::default(),
            started: Instant::now(),
            cursor: egui::CursorIcon::Default,
            copied: None,
        })
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    pub fn window(&self) -> &Arc<dyn Window> {
        &self.window
    }

    /// Whether readings still have to be collected, which pausing stops.
    pub fn wants_report(&self) -> bool {
        !self.state.paused || self.state.frozen.is_none()
    }

    /// Text pasted into the interface.
    pub fn paste(&mut self, text: String) {
        self.input.paste(text);
    }

    /// Takes a window event. The session acts on what comes back.
    pub fn window_event(&mut self, event: &WindowEvent) -> Outcome {
        let mut outcome = Outcome::default();
        match event {
            WindowEvent::CloseRequested => outcome.close = true,
            WindowEvent::SurfaceResized(size) => {
                self.config.width = size.width.max(1);
                self.config.height = size.height.max(1);
                self.surface.configure(&self.device, &self.config);
                outcome.redraw = true;
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.context.set_pixels_per_point(*scale_factor as f32);
                outcome.redraw = true;
            }
            WindowEvent::RedrawRequested => outcome.redraw = true,
            event => {
                let scale = self.window.scale_factor() as f32;
                outcome.redraw = self.input.push(event, scale);
                outcome.close = self.input.wants_close();
            }
        }
        outcome
    }

    /// Draws a frame from `report`, or from the frozen one while paused. Returns
    /// when the next frame is due.
    pub fn draw(&mut self, report: Option<Report>) -> Instant {
        if let Some(report) = report {
            self.state.frozen = Some(report);
        }
        let report = self.state.frozen.clone().unwrap_or_default();
        let scale = self.context.pixels_per_point();
        let size = (self.config.width, self.config.height);
        let raw_input = self.input.take(size, scale, self.started.elapsed().as_secs_f64());
        let state = &mut self.state;
        let output = self.context.run_ui(raw_input, |ui| ui::draw(ui, state, &report));

        for command in &output.platform_output.commands {
            if let egui::OutputCommand::CopyText(text) = command {
                self.copied = Some(text.clone());
            }
        }
        if output.platform_output.cursor_icon != self.cursor {
            self.cursor = output.platform_output.cursor_icon;
            self.window.set_cursor(input::cursor_icon(self.cursor).unwrap_or_default().into());
        }
        let jobs = self.context.tessellate(output.shapes, output.pixels_per_point);
        let mut textures = output.textures_delta;
        for (id, deltas) in &textures.set {
            for delta in deltas {
                self.painter.update_texture(&self.device, &self.queue, *id, delta);
            }
        }
        self.paint(&jobs, output.pixels_per_point);
        for id in &textures.free {
            self.painter.free_texture(id);
        }
        textures.clear();
        // Frozen readings have nothing to follow; an unfocused window still does.
        let interval = match self.state.paused {
            true => REFRESH_PAUSED,
            false => REFRESH,
        };
        let repaint = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(interval, |viewport| viewport.repaint_delay.min(interval));
        Instant::now() + repaint
    }

    /// Takes what the interface asked to put on the clipboard, if anything.
    pub fn take_copied(&mut self) -> Option<String> {
        self.copied.take()
    }

    fn paint(&mut self, jobs: &[egui::ClippedPrimitive], pixels_per_point: f32) {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            other => {
                log::debug!("inspector surface: {other:?}");
                return;
            }
        };
        let descriptor =
            egui_wgpu::ScreenDescriptor { size_in_pixels: [self.config.width, self.config.height], pixels_per_point };
        let mut encoder =
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("inspector") });
        let buffers = self.painter.update_buffers(&self.device, &self.queue, &mut encoder, jobs, &descriptor);
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("inspector"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.painter.render(&mut pass.forget_lifetime(), jobs, &descriptor);
        }
        self.queue.submit(buffers.into_iter().chain([encoder.finish()]));
        self.window.pre_present_notify();
        self.queue.present(frame);
    }
}
