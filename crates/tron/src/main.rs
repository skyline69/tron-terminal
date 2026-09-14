//! tron: a GPU accelerated terminal emulator.

mod clipboard;
mod input;
mod mouse;
mod terminfo;

use std::fs::File;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;
use parking_lot::Mutex;
use winit::application::ApplicationHandler;
use winit::cursor::CursorIcon;
use winit::dpi::LogicalSize;
use winit::event::{ButtonSource, ElementState, Ime, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{UserAttentionType, Window, WindowAttributes, WindowId};

use tron_config::{Animation, Config, Osc52, Paths, Watcher};
use tron_core::{CursorShape, Modes, Palette, Parser, Selection, SelectionKind, Snapshot, TermEvent, Terminal};
use tron_font::{CellMetrics, FontSystem};
use tron_pty::{Pty, SpawnOptions, WindowSize};
use tron_render::{Gpu, PostShader, Renderer, Theme};

use clipboard::Clipboard;

/// Delay before re-checking a frame held back by synchronized output.
const SYNC_POLL: Duration = Duration::from_millis(8);
/// Maximum time between clicks of a double or triple click.
const MULTI_CLICK: Duration = Duration::from_millis(400);

fn main() -> anyhow::Result<()> {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Warn)
        .parse_env(env_logger::Env::default())
        .init();
    let gpu = thread::Builder::new()
        .name("gpu-init".into())
        .spawn(|| pollster::block_on(Gpu::new()))
        .ok();
    let paths = Paths::discover();
    if paths.is_none() {
        log::warn!("no home directory found, using the default configuration");
    }
    let config = match paths.as_ref().map(Config::load) {
        Some(Ok(config)) => config,
        Some(Err(error)) => {
            log::error!("{error}");
            Config::default()
        }
        None => Config::default(),
    };

    let event_loop = EventLoop::new().context("failed to create event loop")?;
    let proxy = event_loop.create_proxy();
    let config_dirty = Arc::new(AtomicBool::new(false));
    let watcher = paths.as_ref().and_then(|paths| {
        let dirty = config_dirty.clone();
        let proxy = proxy.clone();
        Watcher::new(paths, move || {
            if !dirty.swap(true, Ordering::AcqRel) {
                proxy.wake_up();
            }
        })
        .map_err(|error| log::warn!("config hot reload disabled: {error}"))
        .ok()
    });

    event_loop.run_app(App { paths, config, proxy, config_dirty, _watcher: watcher, gpu, session: None })?;
    Ok(())
}

/// State shared between the UI thread and the pty reader thread.
struct Shared {
    term: Mutex<Terminal>,
    /// Set by the reader when it woke the event loop and the wake is unhandled.
    wake_pending: AtomicBool,
    exited: AtomicBool,
}

struct App {
    paths: Option<Paths>,
    config: Config,
    proxy: EventLoopProxy,
    config_dirty: Arc<AtomicBool>,
    _watcher: Option<Watcher>,
    /// GPU initialization started at launch, joined when the window exists.
    gpu: Option<thread::JoinHandle<Result<Gpu, tron_render::RenderError>>>,
    session: Option<Session>,
}

/// Config values the event handlers need.
#[derive(Clone)]
struct Settings {
    title: String,
    padding: (u16, u16),
    font_size: f32,
    scroll_multiplier: f32,
    copy_on_select: bool,
    osc52: Osc52,
}

impl Settings {
    fn new(config: &Config) -> Self {
        Self {
            title: config.window.title.clone(),
            padding: (config.window.padding_x, config.window.padding_y),
            font_size: config.font.size,
            scroll_multiplier: config.scrollback.multiplier,
            copy_on_select: config.selection.copy_on_select,
            osc52: config.clipboard.osc52,
        }
    }
}

#[derive(Default)]
struct MouseState {
    position: (f64, f64),
    /// Buttons held while reporting to the application, as a bit mask.
    buttons: u8,
    last_cell: Option<(usize, usize)>,
    /// Time, cell and count of the last left click.
    click: Option<(Instant, usize, usize, u8)>,
    selecting: bool,
    /// A single click that has not been dragged. Released without a drag, it clears the selection.
    pending_click: bool,
}

struct Session {
    // Declared before `window`: it must be dropped before the Wayland display.
    clipboard: Clipboard,
    window: Arc<dyn Window>,
    renderer: Renderer,
    fonts: FontSystem,
    font_family: String,
    shared: Arc<Shared>,
    pty: Pty,
    input: mpsc::Sender<Vec<u8>>,
    settings: Settings,
    modifiers: ModifiersState,
    mouse: MouseState,
    /// Mouse pointer shape currently set on the window.
    pointer_icon: CursorIcon,
    focused: bool,
    scale_factor: f64,
    font_size: f32,
    frame_interval: Duration,
    last_frame: Instant,
    snapshot: Snapshot,
    /// Debug aid: `TRON_SCREENSHOT=path` saves a frame after `TRON_SCREENSHOT_DELAY_MS` and exits.
    screenshot: Option<(std::path::PathBuf, Instant)>,
    scroll_accumulator: f32,
}

impl App {
    fn create_session(&mut self, event_loop: &dyn ActiveEventLoop) -> anyhow::Result<Session> {
        let config = &self.config;
        let started = Instant::now();
        let mut fonts = FontSystem::new(&config.font.family, config.font.size, 1.0)?;
        log::debug!("startup: fonts loaded after {:?}", started.elapsed());
        let metrics = fonts.metrics();
        let logical = LogicalSize::new(
            u32::from(config.window.columns) * metrics.width + 2 * u32::from(config.window.padding_x),
            u32::from(config.window.rows) * metrics.height + 2 * u32::from(config.window.padding_y),
        );
        let attributes = WindowAttributes::default()
            .with_title(config.window.title.clone())
            .with_surface_size(logical)
            .with_transparent(config.window.opacity < 1.0)
            .with_decorations(config.window.decorations);
        let window: Arc<dyn Window> = Arc::from(event_loop.create_window(attributes)?);
        window.set_cursor(CursorIcon::Text.into());
        log::debug!("startup: window created after {:?}", started.elapsed());

        let scale_factor = window.scale_factor();
        fonts.set_size(config.font.size, scale_factor);
        let size = window.surface_size();
        let settings = Settings::new(config);
        let window_padding = padding(settings.padding, scale_factor);

        // Start the shell before the renderer so the prompt is ready by the first frame.
        let metrics = fonts.metrics();
        let cols = ((size.width as f32 - 2.0 * window_padding[0]) / metrics.width as f32).max(1.0) as usize;
        let rows = ((size.height as f32 - 2.0 * window_padding[1]) / metrics.height as f32).max(1.0) as usize;
        let mut term = Terminal::new(cols, rows, config.scrollback.lines);
        // Images sent before the first frame must be sized with the real cell size.
        term.set_cell_pixels(metrics.width, metrics.height);
        let pty = Pty::spawn(&self.spawn_options(), window_size(cols, rows, metrics))?;
        let shared = Arc::new(Shared {
            term: Mutex::new(term),
            wake_pending: AtomicBool::new(false),
            exited: AtomicBool::new(false),
        });
        let input = spawn_writer(pty.writer()?)?;
        spawn_reader(pty.reader()?, shared.clone(), self.proxy.clone(), input.clone())?;
        log::debug!("startup: shell spawned after {:?}", started.elapsed());

        let theme = Theme { opacity: config.window.opacity.clamp(0.0, 1.0), ..Theme::default() };
        let preloaded = self.gpu.take().and_then(|handle| handle.join().ok());
        let renderer = match preloaded {
            Some(Ok(gpu)) => Renderer::with_gpu(gpu, window.clone(), size.width, size.height, metrics, window_padding, theme.clone()),
            Some(Err(error)) => Err(error),
            None => Err(tron_render::RenderError::Unsupported),
        };
        let renderer = match renderer {
            Ok(renderer) => renderer,
            Err(error) => {
                log::debug!("preloaded GPU unusable ({error}), initializing again");
                pollster::block_on(Renderer::new(window.clone(), size.width, size.height, metrics, window_padding, theme))?
            }
        };
        log::debug!("startup: renderer ready after {:?}", started.elapsed());

        #[allow(deprecated)]
        window.set_ime_allowed(true);

        let mut session = Session {
            clipboard: Clipboard::new(window.as_ref()),
            frame_interval: frame_interval(window.as_ref()),
            last_frame: Instant::now() - Duration::from_secs(1),
            snapshot: Snapshot::default(),
            screenshot: None,
            window,
            renderer,
            fonts,
            font_family: config.font.family.clone(),
            shared,
            pty,
            input,
            settings,
            modifiers: ModifiersState::empty(),
            mouse: MouseState::default(),
            pointer_icon: CursorIcon::Text,
            focused: true,
            scale_factor,
            font_size: config.font.size,
            scroll_accumulator: 0.0,
        };
        session.apply_config(config, self.paths.as_ref());
        if let Some(path) = std::env::var_os("TRON_SCREENSHOT") {
            let delay = std::env::var("TRON_SCREENSHOT_DELAY_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(1500);
            if session.renderer.enable_capture() {
                session.screenshot = Some((path.into(), Instant::now() + Duration::from_millis(delay)));
            }
        }
        Ok(session)
    }

    fn spawn_options(&self) -> SpawnOptions {
        let shell = &self.config.shell;
        let mut options = SpawnOptions {
            program: shell.program.clone(),
            args: shell.args.clone(),
            term: shell.term.clone(),
            cwd: None,
            env: Vec::new(),
        };
        if options.term == "xterm-tron" {
            match self.paths.as_ref().and_then(|p| terminfo::install(&p.data_dir)) {
                Some(dir) => options.env.push(("TERMINFO_DIRS".into(), terminfo::search_path(&dir))),
                None => {
                    log::warn!("xterm-tron terminfo unavailable, using TERM=xterm-256color");
                    options.term = "xterm-256color".into();
                }
            }
        }
        options.env.extend(shell.env.iter().map(|(k, v)| (k.into(), v.into())));
        options
    }

    fn reload_config(&mut self) {
        let Some(paths) = &self.paths else { return };
        match Config::load(paths) {
            Ok(config) => {
                log::info!("configuration reloaded");
                self.config = config;
                if let Some(session) = &mut self.session {
                    session.apply_config(&self.config, self.paths.as_ref());
                }
            }
            Err(error) => log::error!("{error}"),
        }
    }
}

impl ApplicationHandler for App {
    fn can_create_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        if self.session.is_some() {
            return;
        }
        match self.create_session(event_loop) {
            Ok(session) => {
                // Output may have arrived while the session was being created.
                session.shared.wake_pending.store(false, Ordering::Release);
                session.window.request_redraw();
                self.session = Some(session);
            }
            Err(error) => {
                log::error!("{error:#}");
                event_loop.exit();
            }
        }
    }

    fn new_events(&mut self, _event_loop: &dyn ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached { .. } = cause
            && let Some(session) = &self.session
        {
            session.window.request_redraw();
        }
    }

    fn proxy_wake_up(&mut self, event_loop: &dyn ActiveEventLoop) {
        if self.config_dirty.swap(false, Ordering::AcqRel) {
            self.reload_config();
        }
        let Some(session) = self.session.as_mut() else { return };
        session.shared.wake_pending.store(false, Ordering::Release);
        if session.shared.exited.load(Ordering::Acquire) {
            event_loop.exit();
            return;
        }
        let (events, modes) = {
            let mut term = session.shared.term.lock();
            (term.take_events(), term.modes())
        };
        session.handle_events(events);
        session.update_pointer_icon(modes);
        session.schedule_redraw(event_loop);
    }

    fn window_event(&mut self, event_loop: &dyn ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(session) = self.session.as_mut() else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => session.redraw(event_loop),
            WindowEvent::SurfaceResized(size) => {
                session.renderer.resize(size.width, size.height);
                session.resize_grid();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                session.scale_factor = scale_factor;
                session.frame_interval = frame_interval(session.window.as_ref());
                session.set_font_size(session.font_size);
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                session.modifiers = modifiers.state();
                let modes = session.shared.term.lock().modes();
                session.update_pointer_icon(modes);
            }
            WindowEvent::Focused(focused) => {
                session.focused = focused;
                if session.shared.term.lock().modes().contains(Modes::FOCUS_EVENTS) {
                    session.send(if focused { b"\x1b[I".to_vec() } else { b"\x1b[O".to_vec() });
                }
                session.window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if session.handle_shortcut(&event.logical_key) {
                    return;
                }
                let app_cursor = session.shared.term.lock().modes().contains(Modes::APP_CURSOR);
                if let Some(bytes) = input::encode(&event, session.modifiers, app_cursor) {
                    session.prepare_input();
                    session.send(bytes);
                }
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                session.prepare_input();
                session.send(text.into_bytes());
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * session.settings.scroll_multiplier,
                    MouseScrollDelta::PixelDelta(position) => position.y as f32 / session.fonts.metrics().height as f32,
                    _ => return,
                };
                session.scroll(lines);
            }
            WindowEvent::PointerMoved { position, .. } => session.pointer_moved(position.x, position.y),
            WindowEvent::PointerButton { state, position, button: ButtonSource::Mouse(button), .. } => {
                session.mouse.position = (position.x, position.y);
                session.pointer_button(state == ElementState::Pressed, button);
            }
            _ => {}
        }
    }
}

impl Session {
    fn send(&self, bytes: Vec<u8>) {
        let _ = self.input.send(bytes);
    }

    /// Scrolls back to the prompt and clears the selection before sending typed input.
    fn prepare_input(&mut self) {
        let mut term = self.shared.term.lock();
        term.grid_mut().reset_display_offset();
        if term.selection().is_some() {
            term.set_selection(None);
            drop(term);
            self.window.request_redraw();
        }
    }

    fn apply_config(&mut self, config: &Config, paths: Option<&Paths>) {
        self.settings = Settings::new(config);

        if config.font.family != self.font_family {
            match FontSystem::new(&config.font.family, config.font.size, self.scale_factor) {
                Ok(fonts) => {
                    self.fonts = fonts;
                    self.font_family = config.font.family.clone();
                }
                Err(error) => log::error!("{error}"),
            }
        }
        self.fonts.set_fallback(&config.font.fallback);
        self.fonts.set_features(&config.font.shaping_features());
        self.set_font_size(config.font.size);

        match config.colors(paths) {
            Ok(colors) => {
                let palette = Palette::from_ansi(
                    colors.foreground.to_array(),
                    colors.background.to_array(),
                    colors.cursor.to_array(),
                    colors.ansi(),
                );
                self.shared.term.lock().set_default_palette(palette);
                self.renderer.set_theme(Theme {
                    cursor_text: colors.cursor_text.map(|c| c.to_array()),
                    selection_background: colors.selection_background.to_array(),
                    selection_foreground: colors.selection_foreground.map(|c| c.to_array()),
                    opacity: config.window.opacity.clamp(0.0, 1.0),
                });
            }
            Err(error) => log::error!("{error}"),
        }

        let shaders: Vec<PostShader> = config
            .shader_sources(paths)
            .into_iter()
            .filter_map(|source| match source {
                Ok(source) => Some(PostShader { name: source.name, source: source.source }),
                Err(error) => {
                    log::error!("{error}");
                    None
                }
            })
            .collect();
        let animation = match config.shader.animation {
            Animation::Auto => None,
            Animation::Always => Some(true),
            Animation::Never => Some(false),
        };
        for error in self.renderer.set_shaders(&shaders, animation) {
            log::error!("{error}");
        }

        {
            let mut term = self.shared.term.lock();
            term.set_scrollback_limit(config.scrollback.lines);
            term.set_word_separators(&config.selection.word_separators);
            term.set_default_cursor_shape(match config.cursor.shape {
                tron_config::CursorShape::Block => CursorShape::Block,
                tron_config::CursorShape::Beam => CursorShape::Beam,
                tron_config::CursorShape::Underline => CursorShape::Underline,
            });
            let limit = config.images.memory_limit as usize * 1024 * 1024;
            term.graphics_mut().set_limits(limit, config.images.file_transfer);
            term.grid_mut().damage_all();
        }
        self.window.request_redraw();
    }

    fn handle_events(&mut self, events: Vec<TermEvent>) {
        for event in events {
            match event {
                TermEvent::Bell => {
                    if !self.focused {
                        self.window.request_user_attention(Some(UserAttentionType::Informational));
                    }
                }
                TermEvent::ClipboardStore { primary, text } => {
                    if self.settings.osc52 != Osc52::Disabled {
                        self.clipboard.store(primary, text);
                    }
                }
                TermEvent::ClipboardLoad { primary, terminator } => {
                    if self.settings.osc52 == Osc52::CopyPaste {
                        let text = self.clipboard.load(primary).unwrap_or_default();
                        let reply = {
                            let mut term = self.shared.term.lock();
                            term.clipboard_reply(primary, &text, terminator);
                            term.take_responses()
                        };
                        if let Some(reply) = reply {
                            self.send(reply);
                        }
                    }
                }
            }
        }
    }

    /// Redraws at most once per display refresh. Output that arrives faster is
    /// batched into the next frame instead of rendering every chunk.
    fn schedule_redraw(&mut self, event_loop: &dyn ActiveEventLoop) {
        let next = self.last_frame + self.frame_interval;
        if Instant::now() >= next {
            self.window.request_redraw();
        } else {
            event_loop.set_control_flow(ControlFlow::WaitUntil(next));
        }
    }

    fn redraw(&mut self, event_loop: &dyn ActiveEventLoop) {
        {
            let mut term = self.shared.term.lock();
            if term.sync_blocked() {
                event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + SYNC_POLL));
                return;
            }
            if let Some(title) = term.take_title() {
                self.window.set_title(if title.is_empty() { &self.settings.title } else { &title });
            }
            term.snapshot(&mut self.snapshot);
        }
        // Built without the lock, so the reader keeps parsing meanwhile.
        self.renderer.prepare(&self.snapshot, &mut self.fonts, self.focused);
        let capture_now = self.screenshot.as_ref().is_some_and(|(_, due)| Instant::now() >= *due);
        if capture_now && let Some((path, _)) = self.screenshot.take() {
            self.renderer.capture_next_frame(path);
        }
        self.renderer.render();
        self.last_frame = Instant::now();
        if capture_now {
            event_loop.exit();
        } else if let Some((_, due)) = &self.screenshot {
            event_loop.set_control_flow(ControlFlow::WaitUntil(*due));
        } else if self.renderer.is_animated() {
            event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + self.frame_interval));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }

    fn resize_grid(&mut self) {
        let (cols, rows) = self.renderer.grid_size();
        let changed = {
            let mut term = self.shared.term.lock();
            let changed = term.cols() != cols || term.rows() != rows;
            term.resize(cols, rows);
            changed
        };
        if changed && let Err(error) = self.pty.resize(window_size(cols, rows, self.fonts.metrics())) {
            log::warn!("failed to resize pty: {error}");
        }
        self.window.request_redraw();
    }

    fn set_font_size(&mut self, size: f32) {
        self.font_size = size.clamp(4.0, 96.0);
        self.fonts.set_size(self.font_size, self.scale_factor);
        let metrics = self.fonts.metrics();
        self.renderer.set_metrics(metrics, padding(self.settings.padding, self.scale_factor));
        self.shared.term.lock().set_cell_pixels(metrics.width, metrics.height);
        self.resize_grid();
    }

    /// Handles terminal level key bindings. Returns true when the key was consumed.
    fn handle_shortcut(&mut self, key: &Key) -> bool {
        let mods = self.modifiers;
        let (ctrl, shift, alt) = (mods.control_key(), mods.shift_key(), mods.alt_key());
        if let Key::Character(c) = key {
            if ctrl && shift && !alt {
                if c.eq_ignore_ascii_case("c") {
                    self.copy_selection(false);
                    return true;
                }
                if c.eq_ignore_ascii_case("v") {
                    self.paste_from(false);
                    return true;
                }
            }
            if ctrl && !shift && !alt {
                let size = match c.as_str() {
                    "=" | "+" => self.font_size + 1.0,
                    "-" => self.font_size - 1.0,
                    "0" => self.settings.font_size,
                    _ => return false,
                };
                self.set_font_size(size);
                return true;
            }
        }
        if shift && !ctrl && !alt {
            match key {
                Key::Named(NamedKey::PageUp) => {
                    let page = self.shared.term.lock().rows() as f32;
                    self.scroll_history(page);
                }
                Key::Named(NamedKey::PageDown) => {
                    let page = self.shared.term.lock().rows() as f32;
                    self.scroll_history(-page);
                }
                Key::Named(NamedKey::Insert) => self.paste_from(true),
                _ => return false,
            }
            return true;
        }
        false
    }

    fn copy_selection(&self, primary: bool) {
        if let Some(text) = self.shared.term.lock().selection_text() {
            self.clipboard.store(primary, text);
        }
    }

    fn paste_from(&mut self, primary: bool) {
        if let Some(text) = self.clipboard.load(primary) {
            self.paste(&text);
        }
    }

    fn paste(&mut self, text: &str) {
        let bracketed = self.shared.term.lock().modes().contains(Modes::BRACKETED_PASTE);
        let bytes = if bracketed {
            let mut bytes = b"\x1b[200~".to_vec();
            bytes.extend_from_slice(text.replace("\x1b[201~", "").as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
            bytes
        } else {
            text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
        };
        self.prepare_input();
        self.send(bytes);
    }

    /// Arrow while the application receives mouse events, I-beam while the
    /// mouse selects text (including Shift held over a mouse-reporting app).
    fn update_pointer_icon(&mut self, modes: Modes) {
        let icon = if self.mouse_reporting(modes) { CursorIcon::Default } else { CursorIcon::Text };
        if icon != self.pointer_icon {
            self.pointer_icon = icon;
            self.window.set_cursor(icon.into());
        }
    }

    fn mouse_reporting(&self, modes: Modes) -> bool {
        modes.intersects(Modes::MOUSE_TRACKING) && !self.modifiers.shift_key()
    }

    fn pointer_button(&mut self, pressed: bool, button: MouseButton) {
        let (x, y) = self.mouse.position;
        let (row, col) = self.renderer.cell_at(x, y);
        let modes = self.shared.term.lock().modes();
        if self.mouse_reporting(modes) {
            let Some(code) = mouse::button_code(button) else { return };
            if pressed {
                self.mouse.buttons |= 1 << code;
            } else {
                self.mouse.buttons &= !(1 << code);
            }
            let sgr = modes.contains(Modes::MOUSE_SGR);
            if let Some(bytes) = mouse::encode(code, pressed, false, row, col, self.modifiers, sgr) {
                self.send(bytes);
            }
            self.mouse.last_cell = Some((row, col));
            return;
        }

        match (button, pressed) {
            (MouseButton::Left, true) => {
                let now = Instant::now();
                let count = match self.mouse.click {
                    Some((time, r, c, n)) if now - time < MULTI_CLICK && (r, c) == (row, col) => n % 3 + 1,
                    _ => 1,
                };
                self.mouse.click = Some((now, row, col, count));
                let mut term = self.shared.term.lock();
                let point = term.viewport_point(row, col);
                if self.modifiers.shift_key() && term.selection().is_some() {
                    term.update_selection(point);
                    self.mouse.pending_click = false;
                } else {
                    let kind = match count {
                        1 if self.modifiers.alt_key() => SelectionKind::Block,
                        1 => SelectionKind::Simple,
                        2 => SelectionKind::Word,
                        _ => SelectionKind::Line,
                    };
                    term.set_selection(Some(Selection::new(kind, point)));
                    self.mouse.pending_click = count == 1;
                }
                self.mouse.selecting = true;
                drop(term);
                self.window.request_redraw();
            }
            (MouseButton::Left, false) => {
                self.mouse.selecting = false;
                let text = {
                    let mut term = self.shared.term.lock();
                    if self.mouse.pending_click {
                        term.set_selection(None);
                        None
                    } else {
                        term.selection_text()
                    }
                };
                if let Some(text) = text
                    && self.settings.copy_on_select
                {
                    self.clipboard.store(true, text);
                }
                self.window.request_redraw();
            }
            (MouseButton::Middle, true) => self.paste_from(true),
            _ => {}
        }
    }

    fn pointer_moved(&mut self, x: f64, y: f64) {
        self.mouse.position = (x, y);
        let (row, col) = self.renderer.cell_at(x, y);
        let modes = self.shared.term.lock().modes();
        if self.mouse_reporting(modes) {
            let report = modes.contains(Modes::MOUSE_ANY) || (modes.contains(Modes::MOUSE_BUTTON) && self.mouse.buttons != 0);
            if report && self.mouse.last_cell != Some((row, col)) {
                let code = (0..3).find(|b| self.mouse.buttons & (1 << b) != 0).unwrap_or(mouse::NO_BUTTON);
                let sgr = modes.contains(Modes::MOUSE_SGR);
                if let Some(bytes) = mouse::encode(code, true, true, row, col, self.modifiers, sgr) {
                    self.send(bytes);
                }
            }
            self.mouse.last_cell = Some((row, col));
            return;
        }
        if !self.mouse.selecting {
            return;
        }
        let edge = f64::from(self.settings.padding.1) * self.scale_factor;
        let height = f64::from(self.renderer.size().1);
        let mut term = self.shared.term.lock();
        if y < edge {
            term.scroll_display(1);
        } else if y > height - edge {
            term.scroll_display(-1);
        }
        let point = term.viewport_point(row, col);
        if term.selection().is_some_and(|s| s.head != point) {
            self.mouse.pending_click = false;
            term.update_selection(point);
        }
        drop(term);
        self.window.request_redraw();
    }

    /// Wheel input: reports to the application, sends arrows on the alternate
    /// screen, or scrolls history.
    fn scroll(&mut self, lines: f32) {
        self.scroll_accumulator += lines;
        let whole = self.scroll_accumulator.trunc();
        self.scroll_accumulator -= whole;
        if whole == 0.0 {
            return;
        }
        let count = whole.abs() as usize;
        let term = self.shared.term.lock();
        let modes = term.modes();
        let alt_screen = term.is_alt_screen();
        drop(term);

        if self.mouse_reporting(modes) {
            let (row, col) = self.renderer.cell_at(self.mouse.position.0, self.mouse.position.1);
            let code = if whole > 0.0 { mouse::WHEEL_UP } else { mouse::WHEEL_DOWN };
            let sgr = modes.contains(Modes::MOUSE_SGR);
            if let Some(bytes) = mouse::encode(code, true, false, row, col, self.modifiers, sgr) {
                self.send(bytes.repeat(count));
            }
        } else if alt_screen {
            if modes.contains(Modes::ALTERNATE_SCROLL) {
                let key: &[u8] = match (whole > 0.0, modes.contains(Modes::APP_CURSOR)) {
                    (true, true) => b"\x1bOA",
                    (true, false) => b"\x1b[A",
                    (false, true) => b"\x1bOB",
                    (false, false) => b"\x1b[B",
                };
                self.send(key.repeat(count));
            }
        } else {
            self.scroll_history(whole);
        }
    }

    fn scroll_history(&mut self, lines: f32) {
        self.shared.term.lock().scroll_display(lines as isize);
        self.window.request_redraw();
    }
}

fn spawn_writer(mut writer: File) -> io::Result<mpsc::Sender<Vec<u8>>> {
    let (sender, receiver) = mpsc::channel::<Vec<u8>>();
    thread::Builder::new().name("pty-writer".into()).spawn(move || {
        for bytes in receiver {
            if let Err(error) = writer.write_all(&bytes) {
                log::warn!("pty write failed: {error}");
                break;
            }
        }
    })?;
    Ok(sender)
}

fn spawn_reader(
    mut reader: File,
    shared: Arc<Shared>,
    proxy: EventLoopProxy,
    responses: mpsc::Sender<Vec<u8>>,
) -> io::Result<()> {
    thread::Builder::new().name("pty-reader".into()).spawn(move || {
        let mut parser = Parser::new();
        let mut buffer = vec![0u8; 1 << 18];
        loop {
            let n = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => n,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    if !tron_pty::is_closed_error(&error) {
                        log::error!("pty read failed: {error}");
                    }
                    break;
                }
            };
            let reply = {
                let mut term = shared.term.lock();
                parser.advance(&mut *term, &buffer[..n]);
                term.take_responses()
            };
            if let Some(reply) = reply {
                let _ = responses.send(reply);
            }
            if !shared.wake_pending.swap(true, Ordering::AcqRel) {
                proxy.wake_up();
            }
        }
        shared.exited.store(true, Ordering::Release);
        proxy.wake_up();
    })?;
    Ok(())
}

fn padding(padding: (u16, u16), scale_factor: f64) -> [f32; 2] {
    let scale = scale_factor as f32;
    [f32::from(padding.0) * scale, f32::from(padding.1) * scale]
}

fn frame_interval(window: &dyn Window) -> Duration {
    window
        .current_monitor()
        .and_then(|monitor| monitor.current_video_mode())
        .and_then(|mode| mode.refresh_rate_millihertz())
        .map_or(Duration::from_micros(16_667), |mhz| Duration::from_secs_f64(1000.0 / f64::from(mhz.get())))
}

fn window_size(cols: usize, rows: usize, metrics: CellMetrics) -> WindowSize {
    let clamp = |v: usize| v.min(usize::from(u16::MAX)) as u16;
    WindowSize {
        cols: clamp(cols),
        rows: clamp(rows),
        cell_width: clamp(metrics.width as usize),
        cell_height: clamp(metrics.height as usize),
    }
}
