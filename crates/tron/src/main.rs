//! tron: a GPU accelerated terminal emulator.

mod clipboard;
mod input;
mod mouse;
mod terminfo;

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;
use parking_lot::Mutex;
use winit::application::ApplicationHandler;
use winit::cursor::CursorIcon;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ButtonSource, ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{
    ImeCapabilities, ImeEnableRequest, ImeRequest, ImeRequestData, UserAttentionType, Window, WindowAttributes,
    WindowId,
};

use tron_config::{Action, Animation, Bell, BindKey, Binding, Blinking, Config, KeyCombo, Osc52, Paths, Watcher};
use tron_core::{
    CursorShape, LinkMatch, Modes, Palette, Parser, SearchMatch, Selection, SelectionKind, Snapshot, TermEvent,
    Terminal,
};
use tron_font::{CellMetrics, FontSystem};
use tron_pty::{Pty, SpawnOptions, WindowSize};
use tron_render::{Gpu, LinkHighlight, Overlay, PostShader, Renderer, Theme};

use clipboard::Clipboard;

/// Delay before re-checking a frame held back by synchronized output.
const SYNC_POLL: Duration = Duration::from_millis(8);
/// Maximum time between clicks of a double or triple click.
const MULTI_CLICK: Duration = Duration::from_millis(400);
/// Duration of the visual bell flash.
const FLASH: Duration = Duration::from_millis(150);
/// Link schemes opened on Ctrl+click.
const LINK_SCHEMES: [&str; 9] =
    ["http://", "https://", "file://", "mailto:", "ftp://", "sftp://", "ssh://", "git://", "gemini://"];

const HELP: &str = "tron: GPU accelerated terminal emulator

Usage: tron [options] [-e program [args...]]

Options:
  -e, --command <program> [args...]  Run a program instead of the shell
  -d, --working-directory <dir>      Start in this directory
      --config-dir <dir>             Use this configuration directory
  -h, --help                         Show this help
  -V, --version                      Show the version
";

#[derive(Default, Clone)]
struct Cli {
    command: Option<Vec<String>>,
    working_directory: Option<PathBuf>,
    config_dir: Option<PathBuf>,
}

/// Parses arguments. `Ok(None)` means help or version was printed.
fn parse_cli() -> Result<Option<Cli>, String> {
    let mut cli = Cli::default();
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("-e" | "--command") => {
                let command: Vec<String> = args.by_ref().map(|a| a.to_string_lossy().into_owned()).collect();
                if command.is_empty() {
                    return Err("-e needs a program".into());
                }
                cli.command = Some(command);
            }
            Some("-d" | "--working-directory") => {
                cli.working_directory = Some(args.next().ok_or("--working-directory needs a directory")?.into());
            }
            Some("--config-dir") => cli.config_dir = Some(args.next().ok_or("--config-dir needs a directory")?.into()),
            Some("-h" | "--help") => {
                print!("{HELP}");
                return Ok(None);
            }
            Some("-V" | "--version") => {
                println!("tron {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            _ => return Err(format!("unknown argument {arg:?}, see --help")),
        }
    }
    Ok(Some(cli))
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::new().filter_level(log::LevelFilter::Warn).parse_env(env_logger::Env::default()).init();
    let cli = match parse_cli() {
        Ok(Some(cli)) => cli,
        Ok(None) => return Ok(()),
        Err(error) => {
            eprintln!("tron: {error}");
            std::process::exit(2);
        }
    };
    let gpu = thread::Builder::new().name("gpu-init".into()).spawn(|| pollster::block_on(Gpu::new())).ok();
    let paths = match (Paths::discover(), &cli.config_dir) {
        (Some(paths), Some(dir)) => Some(Paths::with_dirs(dir.clone(), paths.data_dir)),
        (None, Some(dir)) => Some(Paths::with_dirs(dir.clone(), dir.join("data"))),
        (paths, None) => paths,
    };
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

    event_loop.run_app(App { cli, paths, config, proxy, config_dirty, _watcher: watcher, gpu, session: None })?;
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
    cli: Cli,
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
    close_on_exit: bool,
    bell: Bell,
    blinking: Blinking,
    blink_interval: Duration,
    open_command: String,
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
            close_on_exit: config.window.close_on_exit,
            bell: config.bell.mode,
            blinking: config.cursor.blinking,
            blink_interval: Duration::from_millis(config.cursor.blink_interval_ms.max(50)),
            open_command: config.links.open_command.clone(),
        }
    }
}

#[derive(Default)]
struct SearchState {
    query: String,
    current: Option<SearchMatch>,
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
    bindings: Vec<Binding>,
    search: Option<SearchState>,
    /// Uncommitted IME text.
    preedit: Option<String>,
    hovered_link: Option<LinkMatch>,
    blink_epoch: Instant,
    flash_until: Option<Instant>,
    /// The shell exited but the window stays open.
    exited: bool,
    ime_area: Option<[f32; 4]>,
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
            Some(Ok(gpu)) => {
                Renderer::with_gpu(gpu, window.clone(), size.width, size.height, metrics, window_padding, theme.clone())
            }
            Some(Err(error)) => Err(error),
            None => Err(tron_render::RenderError::Unsupported),
        };
        let renderer = match renderer {
            Ok(renderer) => renderer,
            Err(error) => {
                log::debug!("preloaded GPU unusable ({error}), initializing again");
                pollster::block_on(Renderer::new(
                    window.clone(),
                    size.width,
                    size.height,
                    metrics,
                    window_padding,
                    theme,
                ))?
            }
        };
        log::debug!("startup: renderer ready after {:?}", started.elapsed());

        let ime = ImeEnableRequest::new(
            ImeCapabilities::new().with_cursor_area(),
            ImeRequestData::default()
                .with_cursor_area(PhysicalPosition::new(0.0, 0.0).into(), PhysicalSize::new(1.0, 1.0).into()),
        );
        if let Some(request) = ime
            && let Err(error) = window.request_ime_update(ImeRequest::Enable(request))
        {
            log::debug!("IME unavailable: {error}");
        }

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
            bindings: Vec::new(),
            search: None,
            preedit: None,
            hovered_link: None,
            blink_epoch: Instant::now(),
            flash_until: None,
            exited: false,
            ime_area: None,
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
        let (program, args) = match &self.cli.command {
            Some(command) => (Some(command[0].clone()), command[1..].to_vec()),
            None => (shell.program.clone(), shell.args.clone()),
        };
        let mut options = SpawnOptions {
            program,
            args,
            term: shell.term.clone(),
            cwd: self.cli.working_directory.clone(),
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
        if session.shared.exited.load(Ordering::Acquire) && !session.exited {
            if session.settings.close_on_exit {
                event_loop.exit();
                return;
            }
            session.exited = true;
            let mut term = session.shared.term.lock();
            Parser::new().advance(&mut *term, b"\r\n\x1b[0;2m[process exited, press any key to close]\x1b[0m");
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
            WindowEvent::RedrawRequested => {
                session.redraw(event_loop);
                if session.renderer.is_device_lost() {
                    let config = &self.config;
                    if let Err(error) = session.recreate_renderer(config, self.paths.as_ref()) {
                        log::error!("cannot recover from GPU device loss: {error:#}");
                        event_loop.exit();
                    }
                }
            }
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
                session.update_hover();
            }
            WindowEvent::Focused(focused) => {
                session.focused = focused;
                if session.shared.term.lock().modes().contains(Modes::FOCUS_EVENTS) {
                    session.send(if focused { b"\x1b[I".to_vec() } else { b"\x1b[O".to_vec() });
                }
                session.window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                if pressed {
                    if session.exited {
                        event_loop.exit();
                        return;
                    }
                    session.blink_epoch = Instant::now();
                    let action = session.binding_for(&event);
                    if action == Some(Action::ReloadConfig) {
                        self.reload_config();
                        return;
                    }
                    if session.search.is_some() {
                        match action {
                            Some(Action::Search) => session.search_step(true, false),
                            _ => session.search_key(&event),
                        }
                        return;
                    }
                    if let Some(action) = action {
                        session.run_action(action);
                        return;
                    }
                } else if session.search.is_some() {
                    return;
                }
                let (app_cursor, kitty_flags) = {
                    let term = session.shared.term.lock();
                    (term.modes().contains(Modes::APP_CURSOR), term.keyboard_flags())
                };
                if let Some(bytes) = input::encode(&event, session.modifiers, app_cursor, kitty_flags) {
                    if pressed {
                        session.prepare_input();
                    }
                    session.send(bytes);
                }
            }
            WindowEvent::Ime(Ime::Preedit(text, _)) => {
                session.preedit = (!text.is_empty()).then_some(text);
                session.update_overlays();
                session.window.request_redraw();
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                session.preedit = None;
                session.update_overlays();
                if session.search.is_some() {
                    session.search_text(&text);
                } else {
                    session.prepare_input();
                    session.send(text.into_bytes());
                }
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
        let (bindings, errors) = config.bindings();
        for error in errors {
            log::error!("keybindings: {error}");
        }
        self.bindings = bindings;

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
                    bold_is_bright: colors.bold_is_bright,
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
                    if matches!(self.settings.bell, Bell::Attention | Bell::Both) && !self.focused {
                        self.window.request_user_attention(Some(UserAttentionType::Informational));
                    }
                    if matches!(self.settings.bell, Bell::Visual | Bell::Both) {
                        self.flash_until = Some(Instant::now() + FLASH);
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
        let now = Instant::now();
        let mut next_wake: Option<Instant> = None;
        let mut wake_at = |at: Instant| next_wake = Some(next_wake.map_or(at, |n| n.min(at)));

        // Cursor blinking.
        let blinking = self.focused
            && match self.settings.blinking {
                Blinking::App => self.snapshot.cursor().blinking,
                Blinking::Always => true,
                Blinking::Never => false,
            };
        let interval = self.settings.blink_interval;
        let phase = now.duration_since(self.blink_epoch).as_millis() / interval.as_millis().max(1);
        self.renderer.set_cursor_hidden(blinking && phase % 2 == 1);
        if blinking {
            wake_at(self.blink_epoch + interval * (phase as u32 + 1));
        }

        // Visual bell.
        match self.flash_until {
            Some(until) if until > now => {
                self.renderer.set_flash(until.duration_since(now).as_secs_f32() / FLASH.as_secs_f32());
                wake_at(now + self.frame_interval);
            }
            Some(_) => {
                self.flash_until = None;
                self.renderer.set_flash(0.0);
            }
            None => {}
        }

        // Built without the lock, so the reader keeps parsing meanwhile.
        self.renderer.prepare(&self.snapshot, &mut self.fonts, self.focused);
        let capture_now = self.screenshot.as_ref().is_some_and(|(_, due)| now >= *due);
        if capture_now && let Some((path, _)) = self.screenshot.take() {
            self.renderer.capture_next_frame(path);
        }
        self.renderer.render();
        self.last_frame = Instant::now();
        self.update_ime_area();

        if capture_now {
            event_loop.exit();
            return;
        }
        if let Some((_, due)) = &self.screenshot {
            wake_at(*due);
        }
        if self.renderer.is_animated() {
            wake_at(self.last_frame + self.frame_interval);
        }
        event_loop.set_control_flow(next_wake.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }

    /// Tells the input method where the cursor is, so its popup appears there.
    fn update_ime_area(&mut self) {
        let rect = self.renderer.cursor_rect();
        if rect[2] <= 0.0 || self.ime_area == Some(rect) {
            return;
        }
        self.ime_area = Some(rect);
        let data = ImeRequestData::default().with_cursor_area(
            PhysicalPosition::new(f64::from(rect[0]), f64::from(rect[1])).into(),
            PhysicalSize::new(f64::from(rect[2]), f64::from(rect[3])).into(),
        );
        if let Err(error) = self.window.request_ime_update(ImeRequest::Update(data)) {
            log::trace!("IME update failed: {error}");
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

    /// Action bound to a key event, if any.
    fn binding_for(&self, event: &KeyEvent) -> Option<Action> {
        let mods = self.modifiers;
        let combo = |shift: bool, key: BindKey| KeyCombo {
            ctrl: mods.control_key(),
            shift,
            alt: mods.alt_key(),
            super_key: mods.meta_key(),
            key,
        };
        let base = bind_key(&event.key_without_modifiers);
        let logical = bind_key(&event.logical_key);
        let mut candidates = Vec::with_capacity(3);
        if let Some(base) = &base {
            candidates.push(combo(mods.shift_key(), base.clone()));
        }
        if let Some(logical) = logical {
            candidates.push(combo(mods.shift_key(), logical.clone()));
            // Shifted symbols such as `+` also match bindings written without shift.
            if base.as_ref() != Some(&logical) {
                candidates.push(combo(false, logical));
            }
        }
        candidates.iter().find_map(|c| self.bindings.iter().find(|b| &b.combo == c).map(|b| b.action.clone()))
    }

    fn run_action(&mut self, action: Action) {
        match action {
            Action::Copy => self.copy_selection(false),
            Action::Paste => self.paste_from(false),
            Action::PasteSelection => self.paste_from(true),
            Action::IncreaseFontSize => self.set_font_size(self.font_size + 1.0),
            Action::DecreaseFontSize => self.set_font_size(self.font_size - 1.0),
            Action::ResetFontSize => self.set_font_size(self.settings.font_size),
            Action::ScrollLineUp => self.scroll_history(1.0),
            Action::ScrollLineDown => self.scroll_history(-1.0),
            Action::ScrollPageUp | Action::ScrollPageDown => {
                let page = self.shared.term.lock().rows() as f32;
                self.scroll_history(if action == Action::ScrollPageUp { page } else { -page });
            }
            Action::ScrollToTop => self.scroll_history(f32::from(u16::MAX) * 1000.0),
            Action::ScrollToBottom => {
                self.shared.term.lock().grid_mut().reset_display_offset();
                self.window.request_redraw();
            }
            Action::ClearScrollback => {
                self.shared.term.lock().grid_mut().clear_scrollback();
                self.window.request_redraw();
            }
            Action::Search => {
                self.search = Some(SearchState::default());
                self.update_overlays();
                self.window.request_redraw();
            }
            Action::NewWindow => self.new_window(),
            Action::SendText(text) => {
                self.prepare_input();
                self.send(text.into_bytes());
            }
            Action::ReloadConfig | Action::None => {}
        }
    }

    fn search_key(&mut self, event: &KeyEvent) {
        let Some(search) = &mut self.search else { return };
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.search = None;
                self.shared.term.lock().set_selection(None);
                self.update_overlays();
                self.window.request_redraw();
            }
            Key::Named(NamedKey::Enter) => {
                let backwards = !self.modifiers.shift_key();
                self.search_step(backwards, false);
            }
            Key::Named(NamedKey::Backspace) => {
                search.query.pop();
                self.search_step(true, true);
            }
            _ => {
                if let Some(text) = event.text.clone()
                    && !self.modifiers.control_key()
                {
                    self.search_text(&text);
                }
            }
        }
    }

    fn search_text(&mut self, text: &str) {
        if let Some(search) = &mut self.search {
            search.query.extend(text.chars().filter(|c| !c.is_control()));
            self.search_step(true, true);
        }
    }

    /// Moves to the next match. `restart` searches again from the bottom after the query changed.
    fn search_step(&mut self, backwards: bool, restart: bool) {
        let Some(search) = &mut self.search else { return };
        {
            let mut term = self.shared.term.lock();
            let from = if restart { None } else { search.current.map(|m| m.start) };
            search.current = if search.query.is_empty() { None } else { term.search(&search.query, from, backwards) };
            match search.current {
                Some(found) => {
                    term.set_selection(Some(Selection {
                        kind: SelectionKind::Simple,
                        anchor: found.start,
                        head: found.end,
                    }));
                    term.scroll_to_line(found.start.line);
                }
                None => term.set_selection(None),
            }
        }
        self.update_overlays();
        self.window.request_redraw();
    }

    /// Rebuilds the search bar and IME preedit overlays.
    fn update_overlays(&mut self) {
        let mut overlays = Vec::new();
        let (rows, palette, cursor) = {
            let term = self.shared.term.lock();
            (term.rows(), *term.palette(), term.cursor())
        };
        if let Some(search) = &self.search {
            let status = match (&search.current, search.query.is_empty()) {
                (_, true) => String::new(),
                (Some(_), false) => "  Enter: older, Shift+Enter: newer, Esc: close".into(),
                (None, false) => "  no matches".into(),
            };
            overlays.push(Overlay {
                row: rows.saturating_sub(1),
                col: 0,
                text: format!(" Search: {}▏{status} ", search.query),
                fg: palette.background,
                bg: palette.cursor,
                underline: false,
            });
        }
        if let Some(preedit) = &self.preedit {
            overlays.push(Overlay {
                row: cursor.row,
                col: cursor.col,
                text: preedit.clone(),
                fg: palette.foreground,
                bg: palette.background,
                underline: true,
            });
        }
        self.renderer.set_overlays(overlays);
    }

    fn new_window(&self) {
        let cwd = self.shared.term.lock().cwd().map(percent_decode).map(PathBuf::from).filter(|p| p.is_dir());
        let Ok(exe) = std::env::current_exe() else { return };
        let mut command = Command::new(exe);
        if let Some(cwd) = cwd.or_else(|| std::env::current_dir().ok()) {
            command.arg("--working-directory").arg(cwd);
        }
        spawn_detached(command);
    }

    fn open_link(&self, uri: &str) {
        let lower = uri.to_ascii_lowercase();
        if !LINK_SCHEMES.iter().any(|scheme| lower.starts_with(scheme)) {
            log::warn!("not opening link with unsupported scheme: {uri}");
            return;
        }
        let mut command = Command::new(&self.settings.open_command);
        command.arg(uri);
        spawn_detached(command);
    }

    /// Underlines the link under the pointer while Ctrl is held.
    fn update_hover(&mut self) {
        let modes = self.shared.term.lock().modes();
        let link = if self.modifiers.control_key() && !self.mouse_reporting(modes) {
            let (row, col) = self.renderer.cell_at(self.mouse.position.0, self.mouse.position.1);
            let term = self.shared.term.lock();
            term.link_at(term.viewport_point(row, col))
        } else {
            None
        };
        if link != self.hovered_link {
            self.renderer.set_link_highlight(link.as_ref().map(|l| LinkHighlight {
                start: l.start,
                end: l.end,
                id: l.id,
            }));
            self.hovered_link = link;
            self.window.request_redraw();
        }
        self.update_pointer_icon(modes);
    }

    /// Creates a new renderer after the GPU device was lost.
    fn recreate_renderer(&mut self, config: &Config, paths: Option<&Paths>) -> anyhow::Result<()> {
        let size = self.window.surface_size();
        let theme = Theme { opacity: config.window.opacity.clamp(0.0, 1.0), ..Theme::default() };
        let metrics = self.fonts.metrics();
        let padding = padding(self.settings.padding, self.scale_factor);
        self.renderer =
            pollster::block_on(Renderer::new(self.window.clone(), size.width, size.height, metrics, padding, theme))?;
        self.apply_config(config, paths);
        log::warn!("renderer recreated after GPU device loss");
        Ok(())
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
        let icon = if self.hovered_link.is_some() {
            CursorIcon::Pointer
        } else if self.mouse_reporting(modes) {
            CursorIcon::Default
        } else {
            CursorIcon::Text
        };
        if icon != self.pointer_icon {
            self.pointer_icon = icon;
            self.window.set_cursor(icon.into());
        }
    }

    fn mouse_reporting(&self, modes: Modes) -> bool {
        modes.intersects(Modes::MOUSE_TRACKING) && !self.modifiers.shift_key()
    }

    fn pointer_button(&mut self, pressed: bool, button: MouseButton) {
        if pressed
            && button == MouseButton::Left
            && self.modifiers.control_key()
            && let Some(link) = self.hovered_link.clone()
        {
            self.open_link(&link.uri);
            return;
        }
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
        if self.modifiers.control_key() || self.hovered_link.is_some() {
            self.update_hover();
        }
        let (row, col) = self.renderer.cell_at(x, y);
        let modes = self.shared.term.lock().modes();
        if self.mouse_reporting(modes) {
            let report =
                modes.contains(Modes::MOUSE_ANY) || (modes.contains(Modes::MOUSE_BUTTON) && self.mouse.buttons != 0);
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

fn bind_key(key: &Key) -> Option<BindKey> {
    match key {
        Key::Character(text) => {
            let mut chars = text.chars();
            let c = chars.next()?;
            chars.next().is_none().then(|| BindKey::Char(c.to_lowercase().next().unwrap_or(c)))
        }
        Key::Named(named) => {
            let function = match named {
                NamedKey::F1 => Some(1),
                NamedKey::F2 => Some(2),
                NamedKey::F3 => Some(3),
                NamedKey::F4 => Some(4),
                NamedKey::F5 => Some(5),
                NamedKey::F6 => Some(6),
                NamedKey::F7 => Some(7),
                NamedKey::F8 => Some(8),
                NamedKey::F9 => Some(9),
                NamedKey::F10 => Some(10),
                NamedKey::F11 => Some(11),
                NamedKey::F12 => Some(12),
                _ => None,
            };
            if let Some(n) = function {
                return Some(BindKey::Named(format!("f{n}")));
            }
            let name = match named {
                NamedKey::Enter => "enter",
                NamedKey::Tab => "tab",
                NamedKey::Backspace => "backspace",
                NamedKey::Escape => "escape",
                NamedKey::Insert => "insert",
                NamedKey::Delete => "delete",
                NamedKey::Home => "home",
                NamedKey::End => "end",
                NamedKey::PageUp => "page_up",
                NamedKey::PageDown => "page_down",
                NamedKey::ArrowUp => "up",
                NamedKey::ArrowDown => "down",
                NamedKey::ArrowLeft => "left",
                NamedKey::ArrowRight => "right",
                _ => return None,
            };
            Some(BindKey::Named(name.to_owned()))
        }
        _ => None,
    }
}

/// Runs a program without waiting for it, reaping it on a helper thread.
fn spawn_detached(mut command: Command) {
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    match command.spawn() {
        Ok(mut child) => {
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(error) => log::error!("failed to run {:?}: {error}", command.get_program()),
    }
}

/// Decodes `%XX` escapes in paths reported through OSC 7.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(value) =
                std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(value);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
