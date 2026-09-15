//! tron: a GPU accelerated terminal emulator.

mod accessibility;
mod cli;
mod clipboard;
mod harness;
mod input;
mod launcher;
#[cfg(target_os = "macos")]
mod macos;
mod menu;
mod mouse;
mod panel;
mod terminfo;

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;
use parking_lot::Mutex;
use winit::application::ApplicationHandler;
use winit::cursor::CursorIcon;
use winit::data_transfer::{DataTransferId, TypeHint, TypedData};
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ButtonSource, ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, DndAction, EventLoop, EventLoopProxy};
use winit::icon::{Icon, RgbaIcon};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{
    ImeCapabilities, ImeEnableRequest, ImeRequest, ImeRequestData, Theme as WindowTheme, UserAttentionType, Window,
    WindowAttributes, WindowId,
};

use tron_config::{
    Action, Animation, Bell, BindKey, Binding, Blinking, Config, KeyCombo, NotifyMode, Osc52, Paths, Watcher,
};
use tron_core::{
    CursorShape, LinkMatch, Modes, NotifyWhen, Palette, Parser, SearchMatch, Selection, SelectionKind, Snapshot,
    TermEvent, Terminal,
};
use tron_font::{CellMetrics, FontSystem};
use tron_pty::{Pty, SpawnOptions, WindowSize};
use tron_render::{GlowLine, Gpu, LinkHighlight, Overlay, PostShader, Renderer, Scrollbar, Theme};

use clipboard::Clipboard;

/// Delay before re-checking a frame held back by synchronized output.
const SYNC_POLL: Duration = Duration::from_millis(8);
/// Delay before retrying a frame skipped because the GPU was still busy.
const GPU_BUSY_POLL: Duration = Duration::from_millis(2);
/// Maximum time between clicks of a double or triple click.
const MULTI_CLICK: Duration = Duration::from_millis(400);
/// Debug aid: a token accepted for startup screen commands in any session.
const DEBUG_STARTUP_TOKEN_ENV: &str = "TRON_DEBUG_STARTUP_TOKEN";
/// Longest the startup screen's shader may run without being turned off.
/// The startup screen sends a keepalive every second; this long without one ends its shader.
const STARTUP_SHADER_IDLE: Duration = Duration::from_secs(5);
/// How long configuration errors stay on screen.
const CONFIG_ERROR_TIME: Duration = Duration::from_secs(15);
/// Duration of the visual bell flash.
const FLASH: Duration = Duration::from_millis(150);
/// Frame rate of continuous shader animations while the window has no focus.
const UNFOCUSED_FPS: u32 = 10;
/// How long the overlay scrollbar stays after scrolling stops, and how long it fades.
const SCROLLBAR_VISIBLE: Duration = Duration::from_millis(900);
const SCROLLBAR_FADE: Duration = Duration::from_millis(250);
/// How often the shell's directory is read for the folder icon beside the title.
#[cfg(target_os = "macos")]
const REPRESENTED_DIRECTORY_POLL: Duration = Duration::from_millis(500);
/// Link schemes opened on Ctrl+click.
const LINK_SCHEMES: [&str; 9] =
    ["http://", "https://", "file://", "mailto:", "ftp://", "sftp://", "ssh://", "git://", "gemini://"];

use cli::Cli;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::new().filter_level(log::LevelFilter::Warn).parse_env(env_logger::Env::default()).init();
    let cli = <Cli as clap::Parser>::parse();
    match &cli.subcommand {
        Some(cli::Command::Completions { shell }) => {
            cli::print_completions(*shell);
            return Ok(());
        }
        Some(cli::Command::StartupScreen { shell }) => tron_startup::run(shell),
        // Tab commands such as `tron settings` open the startup screen below.
        _ => {}
    }
    // Inside a tron window the startup screen takes over that window instead of opening another.
    if (cli.startup || cli.startup_tab().is_some()) && tron_startup::inside_tron() {
        let config = Paths::discover().and_then(|paths| Config::load(&paths).ok()).unwrap_or_default();
        return Ok(tron_startup::run_here(config.startup_animations != Some(false), cli.startup_tab())?);
    }
    let gpu = thread::Builder::new().name("gpu-init".into()).spawn(|| pollster::block_on(Gpu::new())).ok();
    let paths = match (Paths::discover(), &cli.config_dir) {
        (Some(paths), Some(dir)) => Some(Paths::with_dirs(dir.clone(), paths.data_dir)),
        (None, Some(dir)) => Some(Paths::with_dirs(dir.clone(), dir.join("data"))),
        (paths, None) => paths,
    };
    match &paths {
        None => log::warn!("no home directory found, using the default configuration"),
        Some(paths) => match paths.create_config() {
            Ok(true) => log::info!("wrote {}", paths.config_file.display()),
            Ok(false) => {}
            Err(error) => log::warn!("cannot write {}: {error}", paths.config_file.display()),
        },
    }
    let mut config_error = None;
    let config = match paths.as_ref().map(Config::load) {
        Some(Ok(config)) => config,
        Some(Err(error)) => {
            log::error!("{error}");
            config_error = Some(error.to_string());
            Config::default()
        }
        None => Config::default(),
    };

    let marker_exists = paths.as_ref().is_none_or(|p| p.data_dir.join(cli::STARTUP_MARKER).exists());
    let show_startup = cli.show_startup(&config, marker_exists, std::env::var_os("TRON_SCREENSHOT").is_some());
    let startup_token = random_token();

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

    event_loop.run_app(App {
        cli,
        paths,
        config,
        config_error,
        show_startup,
        startup_token,
        preview: None,
        proxy,
        config_dirty,
        _watcher: watcher,
        gpu,
        sessions: HashMap::new(),
        active: None,
        max_fps: None,
    })?;
    Ok(())
}

/// State shared between the UI thread and the pty reader thread.
struct Shared {
    term: Mutex<Terminal>,
    /// Set by the reader when it woke the event loop and the wake is unhandled.
    wake_pending: AtomicBool,
    exited: AtomicBool,
}

/// What a new window runs.
#[derive(Default)]
struct Launch {
    /// A program instead of the shell (`tron -e`).
    command: Option<Vec<String>>,
    cwd: Option<PathBuf>,
    /// The startup screen runs before the shell, on `tab` when given.
    startup: bool,
    tab: Option<&'static str>,
}

/// Something a window asks the `App` to do, from its command palette.
enum AppRequest {
    ReloadConfig,
    OpenSettings,
}

/// A window's shell, set aside while another program such as Settings… runs in its place.
struct Suspended {
    pty: Pty,
    shared: Arc<Shared>,
    input: mpsc::Sender<Vec<u8>>,
}

/// Where a window's terminal is and how its cells are laid out, for Look Up.
#[cfg(target_os = "macos")]
struct LookUpTarget(std::cell::RefCell<LookUpLayout>);

#[cfg(target_os = "macos")]
struct LookUpLayout {
    shared: Arc<Shared>,
    /// Top left of the cell grid in physical pixels.
    origin: [f32; 2],
    metrics: tron_font::CellMetrics,
    grid: (usize, usize),
    scale_factor: f64,
    font_family: String,
    font_size: f32,
}

#[cfg(target_os = "macos")]
impl macos::WordAt for LookUpTarget {
    fn word_at(&self, x: f64, y: f64) -> Option<macos::LookUpWord> {
        let layout = self.0.borrow();
        let (width, height) = (f64::from(layout.metrics.width), f64::from(layout.metrics.height));
        let [left, top] = layout.origin.map(f64::from);
        let col = ((x * layout.scale_factor - left) / width).floor();
        let row = ((y * layout.scale_factor - top) / height).floor();
        let (cols, rows) = layout.grid;
        if col < 0.0 || row < 0.0 || col >= cols as f64 || row >= rows as f64 {
            return None;
        }
        let (text, start) = layout.shared.term.lock().word_at(row as usize, col as usize)?;
        Some(macos::LookUpWord {
            text,
            baseline: (
                (left + start as f64 * width) / layout.scale_factor,
                (top + row * height + f64::from(layout.metrics.baseline)) / layout.scale_factor,
            ),
            font_family: layout.font_family.clone(),
            font_size: f64::from(layout.font_size),
        })
    }
}

struct App {
    cli: Cli,
    paths: Option<Paths>,
    config: Config,
    /// Why the configuration file could not be loaded at startup.
    config_error: Option<String>,
    /// Run the startup screen before the shell.
    show_startup: bool,
    /// Secret the startup screen sends with its commands.
    startup_token: String,
    /// TOML the startup screen is previewing on top of `config`.
    preview: Option<String>,
    proxy: EventLoopProxy,
    config_dirty: Arc<AtomicBool>,
    _watcher: Option<Watcher>,
    /// GPU initialization started at launch, joined when the window exists.
    gpu: Option<thread::JoinHandle<Result<Gpu, tron_render::RenderError>>>,
    /// Every window runs in this one process, so macOS shows one app in the Dock.
    sessions: HashMap<WindowId, Session>,
    /// The window menu commands act on: the one focused last.
    active: Option<WindowId>,
    /// Highest refresh rate of the connected monitors, for the startup screen's frame rate choices.
    max_fps: Option<u32>,
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
    notify_mode: NotifyMode,
    notify_command: String,
    #[cfg(target_os = "macos")]
    option_as_alt: tron_config::OptionAsAlt,
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
            notify_mode: config.notifications.mode,
            notify_command: config.notifications.command.clone(),
            #[cfg(target_os = "macos")]
            option_as_alt: config.window.option_as_alt,
        }
    }
}

/// What the startup screen asked for with its config preview.
enum PreviewRequest {
    /// Apply this TOML over the configuration, without saving it.
    Show(String),
    /// Go back to the saved configuration.
    Restore,
    /// Keep what was saved to disk and stop previewing.
    Commit,
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
    /// Last position reported in SGR-Pixels mode.
    last_pixel: Option<(u32, u32)>,
    /// Time, cell and count of the last left click.
    click: Option<(Instant, usize, usize, u8)>,
    selecting: bool,
    /// A single click that has not been dragged. Released without a drag, it clears the selection.
    pending_click: bool,
}

/// A drag and drop operation over the window.
struct DropState {
    id: DataTransferId,
    /// Data was requested from the source.
    requested: bool,
    /// Text to insert, once received.
    text: Option<String>,
    dropped: bool,
}

/// A requested shader chain: (name, source) per shader and the animation mode.
type AppliedShaders = (Vec<(String, String)>, Option<bool>);

/// An overlay scrollbar in the macOS style: shown while scrolling, faded out after,
/// wider while the pointer is over it, and draggable.
struct ScrollbarState {
    /// Drawn on macOS, and elsewhere with `TRON_SCROLLBAR=1`.
    enabled: bool,
    /// When scrolling last showed it.
    shown_at: Option<Instant>,
    hovered: bool,
    /// While the thumb is dragged: where the pointer holds it, from the thumb's top.
    grab: Option<f32>,
    /// Thumb top and height, and track top and bottom, of the last frame.
    thumb: Option<(f32, f32)>,
    track: (f32, f32),
    scrollback: usize,
    offset: usize,
    visible: bool,
}

impl ScrollbarState {
    fn new() -> Self {
        Self {
            enabled: cfg!(target_os = "macos") || std::env::var_os("TRON_SCROLLBAR").is_some_and(|value| value == "1"),
            shown_at: None,
            hovered: false,
            grab: None,
            thumb: None,
            track: (0.0, 0.0),
            scrollback: 0,
            offset: 0,
            visible: false,
        }
    }
}

/// How long the harness line takes to sweep from the center out past both sides.
const HARNESS_SWEEP: Duration = Duration::from_millis(1100);

/// The glowing line that sweeps from the center of the top edge to both sides
/// when a coding agent harness starts.
struct HarnessLine {
    /// The running harness, by command name.
    name: Option<String>,
    color: [u8; 3],
    /// When the sweep started, while it runs.
    started_at: Option<Instant>,
}

impl HarnessLine {
    fn new() -> Self {
        Self { name: None, color: [0; 3], started_at: None }
    }
}

/// Where the heads of a sweep are after `elapsed`, as a distance from the center:
/// they start at the center and ease out until the tails, `tail` long, have left
/// a window whose sides are `half` from the center. `None` once the sweep is over.
fn sweep_head(elapsed: Duration, half: f32, tail: f32) -> Option<f32> {
    let t = elapsed.as_secs_f32() / HARNESS_SWEEP.as_secs_f32();
    (t < 1.0).then(|| (1.0 - (1.0 - t).powi(2)) * (half + tail))
}

struct Session {
    // Declared before `window`: it must be dropped before the Wayland display.
    clipboard: Clipboard,
    accessibility: Option<accessibility::Accessibility>,
    drop: Option<DropState>,
    window: Arc<dyn Window>,
    renderer: Renderer,
    fonts: FontSystem,
    font_family: String,
    shared: Arc<Shared>,
    pty: Pty,
    input: mpsc::Sender<Vec<u8>>,
    settings: Settings,
    /// Token accepted for startup screen commands, from this window's processes.
    startup_token: Option<String>,
    /// When the startup screen last sent a command while its shader runs, and how
    /// many prompt marks existed when the shader was turned on.
    startup_shader: Option<(Instant, usize)>,
    /// A config preview change from the startup screen, carried out by the `App`.
    preview_request: Option<PreviewRequest>,
    /// Font settings last applied, to skip rebuilding glyphs when they did not change.
    applied_font: String,
    /// Shader chain last compiled, with its animation mode and compile errors.
    applied_shaders: Option<AppliedShaders>,
    /// Compile errors of the installed shader chain.
    shader_errors: Vec<String>,
    /// Configuration problems besides shader compile errors, from the last apply.
    config_problems: Vec<String>,
    /// Every available shader was sent to be compiled ahead of use.
    shaders_warmed: bool,
    /// A screenshot waits for the next presented frame, then tron exits.
    capture_requested: bool,
    /// When the next frame is due, kept on a fixed cadence while animating.
    next_frame: Instant,
    /// Window transparency and blur last set.
    applied_translucency: Option<(bool, bool)>,
    /// Configuration problems shown at the top of the window, and until when.
    config_errors: Vec<String>,
    config_errors_until: Option<Instant>,
    modifiers: ModifiersState,
    /// Hyper is held. winit's modifier state does not include it.
    hyper: bool,
    /// Left and right Option keys held.
    #[cfg(target_os = "macos")]
    option_keys: (bool, bool),
    mouse: MouseState,
    /// Mouse pointer shape currently set on the window.
    pointer_icon: CursorIcon,
    /// Pointer shape the application asked for while it reads the mouse (OSC 22).
    app_pointer: Option<CursorIcon>,
    bindings: Vec<Binding>,
    search: Option<SearchState>,
    palette: Option<panel::PaletteState>,
    /// Something only the `App` can do, asked for from the command palette.
    app_request: Option<AppRequest>,
    /// Uncommitted IME text.
    preedit: Option<String>,
    hovered_link: Option<LinkMatch>,
    blink_epoch: Instant,
    flash_until: Option<Instant>,
    /// The shell exited but the window stays open.
    exited: bool,
    ime_area: Option<[f32; 4]>,
    /// Physical pixels at the top covered by window decorations, see [`top_inset`].
    top_inset: f32,
    /// `[shader] fps`: frame rate cap of continuous animations, 0 for none.
    animation_fps: u32,
    /// The window is hidden, minimized or covered: animations stop.
    occluded: bool,
    scrollbar: ScrollbarState,
    /// Which coding agent harness the shell runs, found on a thread.
    harness_watch: Arc<harness::Watch>,
    harness_line: HarnessLine,
    harness_config: tron_config::HarnessConfig,
    /// The folder shown beside the title, and when the shell's directory was last read.
    #[cfg(target_os = "macos")]
    represented_directory: (Option<PathBuf>, Instant),
    #[cfg(target_os = "macos")]
    look_up: std::rc::Rc<LookUpTarget>,
    /// The title a program set, none when it was never set or set empty.
    program_title: Option<String>,
    /// When this window needs the event loop to wake it next.
    wake_at: Option<Instant>,
    /// A window this one asked for, opened by the `App`.
    window_request: Option<Launch>,
    /// The shell, while Settings… runs in its place.
    suspended: Option<Suspended>,
    /// `TRON_TRACE_LATENCY`: log the time from a key press to the frame showing its effect.
    trace_latency: bool,
    pending_key: Option<Instant>,
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
    fn create_session(&mut self, event_loop: &dyn ActiveEventLoop, launch: Launch) -> anyhow::Result<Session> {
        self.max_fps = max_refresh_rate(event_loop);
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
            .with_decorations(config.window.decorations)
            .with_window_icon(window_icon());
        #[cfg(target_os = "macos")]
        let attributes = macos::window_attributes(attributes, config.window.option_as_alt);
        #[cfg(not(target_os = "macos"))]
        let attributes = with_app_id(attributes, event_loop);
        let window: Arc<dyn Window> = Arc::from(event_loop.create_window(attributes)?);
        window.set_cursor(CursorIcon::Text.into());
        log::debug!("startup: window created after {:?}", started.elapsed());

        let scale_factor = window.scale_factor();
        fonts.set_size(config.font.size, scale_factor);
        let inset = top_inset(window.as_ref());
        let mut size = window.surface_size();
        if inset > 0.0 {
            // Content under a macOS title bar: grow the window so the configured rows still fit.
            let taller = PhysicalSize::new(size.width, size.height + inset.ceil() as u32);
            size = window.request_surface_size(taller.into()).unwrap_or(taller);
        }
        let settings = Settings::new(config);
        let window_padding = padding(settings.padding, scale_factor);

        // Start the shell before the renderer so the prompt is ready by the first frame.
        let metrics = fonts.metrics();
        let cols = ((size.width as f32 - 2.0 * window_padding[0]) / metrics.width as f32).max(1.0) as usize;
        let rows = ((size.height as f32 - 2.0 * window_padding[1] - inset) / metrics.height as f32).max(1.0) as usize;
        let mut term = Terminal::new(cols, rows, config.scrollback.lines);
        // Images sent before the first frame must be sized with the real cell size.
        term.set_cell_pixels(metrics.width, metrics.height);
        let pty = Pty::spawn(&self.spawn_options(&launch), window_size(cols, rows, metrics))?;
        let shared = Arc::new(Shared {
            term: Mutex::new(term),
            wake_pending: AtomicBool::new(false),
            exited: AtomicBool::new(false),
        });
        let input = spawn_writer(pty.writer()?)?;
        spawn_reader(pty.reader()?, shared.clone(), self.proxy.clone(), input.clone())?;
        // In a Flatpak the shell's processes are on the host, out of sight.
        let harness_watch = Arc::new(harness::Watch::default());
        harness_watch.set_names(harness::names(&config.harness));
        if !tron_pty::in_flatpak()
            && let Some(tty) = pty.tty_path()
        {
            harness::spawn(tty, pty.child_id(), &harness_watch, self.proxy.clone());
        }
        log::debug!("startup: shell spawned after {:?}", started.elapsed());

        let theme = Theme { opacity: config.window.opacity.clamp(0.0, 1.0), ..Theme::default() };
        // Later windows share the first window's GPU instead of opening their own.
        let preloaded = self
            .gpu
            .take()
            .and_then(|handle| handle.join().ok())
            .or_else(|| self.sessions.values().next().map(|session| Ok(session.renderer.gpu())));
        let renderer = match preloaded {
            Some(Ok(gpu)) => {
                Renderer::with_gpu(gpu, window.clone(), size.width, size.height, metrics, window_padding, theme.clone())
            }
            Some(Err(error)) => Err(error),
            None => Err(tron_render::RenderError::Unsupported),
        };
        let mut renderer = match renderer {
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
        renderer.set_padding(window_padding, inset);
        log::debug!("startup: renderer ready after {:?}", started.elapsed());
        let proxy = self.proxy.clone();
        renderer.set_shader_notify(move || proxy.wake_up());

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

        // TRON_ACCESSIBILITY=0 turns screen reader support off.
        let accessibility = (std::env::var_os("TRON_ACCESSIBILITY").is_none_or(|v| v != "0"))
            .then(|| accessibility::Accessibility::new(self.proxy.clone(), window.as_ref()))
            .flatten();
        #[cfg(target_os = "macos")]
        let look_up = std::rc::Rc::new(LookUpTarget(std::cell::RefCell::new(LookUpLayout {
            shared: shared.clone(),
            origin: [window_padding[0], window_padding[1] + inset],
            metrics,
            grid: (cols, rows),
            scale_factor,
            font_family: config.font.family.clone(),
            font_size: config.font.size,
        })));
        let mut session = Session {
            clipboard: Clipboard::new(window.as_ref()),
            accessibility,
            drop: None,
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
            hyper: false,
            #[cfg(target_os = "macos")]
            option_keys: (false, false),
            mouse: MouseState::default(),
            pointer_icon: CursorIcon::Text,
            app_pointer: None,
            bindings: Vec::new(),
            search: None,
            palette: None,
            app_request: None,
            preedit: None,
            hovered_link: None,
            blink_epoch: Instant::now(),
            flash_until: None,
            exited: false,
            ime_area: None,
            top_inset: inset,
            animation_fps: config.shader.fps,
            occluded: false,
            scrollbar: ScrollbarState::new(),
            harness_watch,
            harness_line: HarnessLine::new(),
            harness_config: config.harness.clone(),
            #[cfg(target_os = "macos")]
            represented_directory: (None, Instant::now() - REPRESENTED_DIRECTORY_POLL),
            #[cfg(target_os = "macos")]
            look_up,
            program_title: None,
            wake_at: None,
            window_request: None,
            suspended: None,
            trace_latency: std::env::var_os("TRON_TRACE_LATENCY").is_some(),
            pending_key: None,
            focused: true,
            scale_factor,
            font_size: config.font.size,
            scroll_accumulator: 0.0,
            config_errors: Vec::new(),
            config_errors_until: None,
            startup_token: Some(
                std::env::var(DEBUG_STARTUP_TOKEN_ENV)
                    .ok()
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| self.startup_token.clone()),
            ),
            startup_shader: None,
            preview_request: None,
            applied_font: String::new(),
            applied_shaders: None,
            shader_errors: Vec::new(),
            config_problems: Vec::new(),
            shaders_warmed: false,
            capture_requested: false,
            next_frame: Instant::now(),
            applied_translucency: None,
        };
        session.apply_config(config, self.paths.as_ref());
        #[cfg(target_os = "macos")]
        {
            let target: std::rc::Rc<dyn macos::WordAt> = session.look_up.clone();
            macos::install_look_up(session.window.as_ref(), &target);
        }
        if let Some(error) = self.config_error.take() {
            session.show_config_errors(vec![error]);
        }
        if let Some(path) = std::env::var_os("TRON_SCREENSHOT")
            && self.sessions.is_empty()
        {
            let delay = std::env::var("TRON_SCREENSHOT_DELAY_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(1500);
            if session.renderer.enable_capture() {
                session.screenshot = Some((path.into(), Instant::now() + Duration::from_millis(delay)));
            }
        }
        Ok(session)
    }

    fn spawn_options(&self, launch: &Launch) -> SpawnOptions {
        let shell = &self.config.shell;
        let (program, args) = match &launch.command {
            Some(command) => (Some(command[0].clone()), command[1..].to_vec()),
            None => (shell.program.clone(), shell.args.clone()),
        };
        let mut options = SpawnOptions {
            program,
            args,
            term: shell.term.clone(),
            cwd: launch.cwd.clone(),
            env: Vec::new(),
            remove_env: vec![DEBUG_STARTUP_TOKEN_ENV.into()],
        };
        let data_dir = self.paths.as_ref().map(|p| p.data_dir.as_path());
        if options.term == "xterm-tron" {
            match data_dir.and_then(terminfo::install) {
                Some(dir) => options.env.push(("TERMINFO_DIRS".into(), terminfo::search_path(&dir))),
                None => {
                    log::warn!("xterm-tron terminfo unavailable, using TERM=xterm-256color");
                    options.term = "xterm-256color".into();
                }
            }
        }
        // `tron` in the shell runs this tron, and `ssh` copies the terminfo entry to hosts.
        if let Some(bin) = data_dir.and_then(|dir| launcher::install(dir, options.term == "xterm-tron")) {
            options.env.push(("PATH".into(), terminfo::path_with(&bin)));
        }
        options.env.extend(shell.env.iter().map(|(k, v)| (k.into(), v.into())));
        if let Some(fps) = self.max_fps {
            options.env.push((tron_startup::MAX_FPS_ENV.into(), fps.to_string().into()));
        }
        // Lets `tron --startup` in this window talk to it.
        options.env.push((tron_startup::TOKEN_ENV.into(), self.startup_token.clone().into()));
        if let Some(paths) = &self.paths {
            options.env.push((tron_startup::CONFIG_DIR_ENV.into(), paths.config_dir.clone().into()));
        }
        // In a Flatpak, shells and commands run on the host, where the user's tools are.
        // The startup screen below stays in the sandbox, where tron is.
        if tron_pty::in_flatpak() {
            let (program, args) = tron_pty::host_command(&options);
            options.program = Some(program);
            options.args = args;
        }
        if launch.startup
            && let Ok(exe) = std::env::current_exe()
        {
            // The startup screen runs first and then replaces itself with the shell.
            let program = options
                .program
                .take()
                .or_else(|| std::env::var("SHELL").ok().filter(|s| !s.is_empty()))
                .unwrap_or_else(|| "/bin/sh".to_owned());
            let mut args = vec!["startup-screen".to_owned(), "--".to_owned(), program];
            args.append(&mut options.args);
            options.args = args;
            options.program = Some(exe.to_string_lossy().into_owned());
            if let Some(paths) = &self.paths {
                options.env.push((tron_startup::MARKER_ENV.into(), paths.data_dir.join(cli::STARTUP_MARKER).into()));
            }
            if self.config.startup_animations == Some(false) {
                options.env.push((tron_startup::ANIMATIONS_ENV.into(), "0".into()));
            }
            if let Some(tab) = launch.tab {
                options.env.push((tron_startup::TAB_ENV.into(), tab.into()));
            }
        }
        options
    }

    fn reload_config(&mut self) {
        let Some(paths) = &self.paths else { return };
        match Config::load(paths) {
            Ok(config) => {
                log::info!("configuration reloaded");
                self.config = config;
                // A running preview stays on top of the reloaded file.
                let previewed =
                    self.preview.as_deref().and_then(|overlay| Config::with_overlay(Some(paths), overlay).ok());
                for session in self.sessions.values_mut() {
                    session.apply_config(previewed.as_ref().unwrap_or(&self.config), self.paths.as_ref());
                }
            }
            Err(error) => {
                log::error!("{error}");
                for session in self.sessions.values_mut() {
                    session.show_config_errors(vec![error.to_string()]);
                }
            }
        }
    }
}

impl App {
    /// Carries out a config preview request from the startup screen.
    fn preview(&mut self, request: PreviewRequest) {
        match request {
            PreviewRequest::Show(overlay) => match Config::with_overlay(self.paths.as_ref(), &overlay) {
                Ok(config) => {
                    let started = Instant::now();
                    for session in self.sessions.values_mut() {
                        session.warm_shaders(self.paths.as_ref());
                        session.apply_config(&config, self.paths.as_ref());
                    }
                    log::debug!("preview applied in {:?}", started.elapsed());
                    self.preview = Some(overlay);
                }
                Err(error) => log::warn!("ignoring startup screen preview: {error}"),
            },
            PreviewRequest::Restore => {
                if self.preview.take().is_some() {
                    for session in self.sessions.values_mut() {
                        session.apply_config(&self.config, self.paths.as_ref());
                    }
                }
            }
            PreviewRequest::Commit => {
                self.preview = None;
                self.reload_config();
            }
        }
    }
}

#[cfg(target_os = "macos")]
impl App {
    /// Carries out a command chosen from the menu bar, as its key binding would.
    fn run_menu_command(&mut self, command: menu::MenuCommand) {
        match command {
            menu::MenuCommand::Action(Action::ReloadConfig) => self.reload_config(),
            menu::MenuCommand::Action(action) => {
                let Some(session) = self.active.and_then(|id| self.sessions.get_mut(&id)) else { return };
                // While searching, Find moves to the next match like its key binding does.
                if action == Action::Search && session.search.is_some() {
                    session.search_step(true, false);
                } else {
                    session.run_action(action);
                }
            }
            menu::MenuCommand::OpenSettings => self.open_settings(),
            menu::MenuCommand::OpenUrl(url) => {
                if let Some(session) = self.active.and_then(|id| self.sessions.get(&id)) {
                    session.open_link(url);
                }
            }
        }
    }
}

impl App {
    /// Runs the startup screen's Settings tab in the active window, in place of its
    /// shell until it closes, as `tron settings` typed there does.
    fn open_settings(&mut self) {
        let Ok(exe) = std::env::current_exe() else { return };
        let mut options = self.spawn_options(&Launch::default());
        // tron itself, also in a Flatpak where the shell's command runs on the host.
        options.program = Some(exe.to_string_lossy().into_owned());
        options.args = vec!["settings".to_owned()];
        let Some(session) = self.active.and_then(|id| self.sessions.get_mut(&id)) else { return };
        if let Err(error) = session.run_in_place(&options, &self.proxy, &self.config, self.paths.as_ref()) {
            log::error!("cannot open settings: {error:#}");
        }
    }
}

impl ApplicationHandler for App {
    fn can_create_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        if !self.sessions.is_empty() {
            return;
        }
        let launch = Launch {
            command: self.cli.command.clone(),
            cwd: self.cli.working_directory.clone(),
            startup: self.show_startup,
            tab: self.cli.startup_tab(),
        };
        self.open_window(event_loop, launch);
    }

    fn new_events(&mut self, _event_loop: &dyn ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached { .. } = cause {
            let now = Instant::now();
            for session in self.sessions.values_mut() {
                if session.wake_at.is_some_and(|at| at <= now) {
                    session.wake_at = None;
                    session.window.request_redraw();
                }
            }
        }
    }

    /// Sleeps until the earliest window needs to wake.
    fn about_to_wait(&mut self, event_loop: &dyn ActiveEventLoop) {
        let next = self.sessions.values().filter_map(|session| session.wake_at).min();
        event_loop.set_control_flow(next.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }

    fn proxy_wake_up(&mut self, event_loop: &dyn ActiveEventLoop) {
        if self.config_dirty.swap(false, Ordering::AcqRel) {
            self.reload_config();
        }
        #[cfg(target_os = "macos")]
        for command in menu::take_commands() {
            self.run_menu_command(command);
        }
        let mut closed = Vec::new();
        let mut previews = Vec::new();
        for (&id, session) in &mut self.sessions {
            // The program run in the shell's place ended: the shell comes back.
            if session.suspended.is_some() && session.shared.exited.load(Ordering::Acquire) {
                session.restore_shell(&self.config, self.paths.as_ref());
            }
            session.shared.wake_pending.store(false, Ordering::Release);
            session.poll_shaders();
            session.update_harness();
            if session.shared.exited.load(Ordering::Acquire) && !session.exited {
                if session.settings.close_on_exit {
                    closed.push(id);
                    continue;
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
            session.schedule_redraw();
            previews.extend(session.preview_request.take());
        }
        for id in closed {
            self.close_window(event_loop, id);
        }
        for request in previews {
            self.preview(request);
        }
        self.open_requested_windows(event_loop);
    }

    fn window_event(&mut self, event_loop: &dyn ActiveEventLoop, id: WindowId, event: WindowEvent) {
        self.session_event(event_loop, id, event);
        self.open_requested_windows(event_loop);
    }
}

impl App {
    fn open_window(&mut self, event_loop: &dyn ActiveEventLoop, launch: Launch) {
        match self.create_session(event_loop, launch) {
            Ok(session) => {
                // Output may have arrived while the session was being created.
                session.shared.wake_pending.store(false, Ordering::Release);
                session.window.request_redraw();
                #[cfg(target_os = "macos")]
                if self.sessions.is_empty() {
                    menu::install(self.proxy.clone(), &session.bindings);
                }
                let id = session.window.id();
                self.active = Some(id);
                self.sessions.insert(id, session);
            }
            Err(error) => {
                log::error!("{error:#}");
                if self.sessions.is_empty() {
                    event_loop.exit();
                }
            }
        }
    }

    /// Closes a window, and quits after the last one.
    fn close_window(&mut self, event_loop: &dyn ActiveEventLoop, id: WindowId) {
        self.sessions.remove(&id);
        if self.active == Some(id) {
            self.active = self.sessions.keys().next().copied();
        }
        if self.sessions.is_empty() {
            event_loop.exit();
        }
    }

    /// Carries out what windows asked the `App` for: windows opened with New
    /// Window, and commands from their command palettes.
    fn open_requested_windows(&mut self, event_loop: &dyn ActiveEventLoop) {
        let requests: Vec<Launch> =
            self.sessions.values_mut().filter_map(|session| session.window_request.take()).collect();
        let app_requests: Vec<(WindowId, AppRequest)> =
            self.sessions.iter_mut().filter_map(|(&id, session)| Some((id, session.app_request.take()?))).collect();
        for launch in requests {
            self.open_window(event_loop, launch);
        }
        for (id, request) in app_requests {
            match request {
                AppRequest::ReloadConfig => self.reload_config(),
                AppRequest::OpenSettings => {
                    self.active = Some(id);
                    self.open_settings();
                }
            }
        }
    }

    fn session_event(&mut self, event_loop: &dyn ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(session) = self.sessions.get_mut(&id) else { return };
        match event {
            WindowEvent::CloseRequested => self.close_window(event_loop, id),
            WindowEvent::RedrawRequested => {
                session.redraw(event_loop);
                if let Some(request) = session.preview_request.take() {
                    self.preview(request);
                    return;
                }
                let Some(session) = self.sessions.get_mut(&id) else { return };
                if session.renderer.is_device_lost() {
                    let config = &self.config;
                    if let Err(error) = session.recreate_renderer(config, self.paths.as_ref(), &self.proxy) {
                        log::error!("cannot recover from GPU device loss: {error:#}");
                        self.close_window(event_loop, id);
                    }
                }
            }
            WindowEvent::SurfaceResized(size) => {
                session.renderer.resize(size.width, size.height);
                session.update_top_inset();
                session.resize_grid();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                session.scale_factor = scale_factor;
                session.frame_interval = frame_interval(session.window.as_ref());
                session.update_top_inset();
                session.set_font_size(session.font_size);
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                session.modifiers = modifiers.state();
                #[cfg(target_os = "macos")]
                {
                    session.option_keys = macos::option_keys(&modifiers);
                }
                session.update_hover();
            }
            WindowEvent::Occluded(occluded) => {
                session.occluded = occluded;
                if !occluded {
                    session.window.request_redraw();
                }
            }
            WindowEvent::Focused(focused) => {
                // Screenshots show the window as it looks focused, even when the compositor gives no focus.
                let focused = focused || session.screenshot.is_some();
                session.focused = focused;
                if focused {
                    self.active = Some(id);
                }
                if let Some(accessibility) = &mut session.accessibility {
                    accessibility.set_focused(focused);
                }
                if session.shared.term.lock().modes().contains(Modes::FOCUS_EVENTS) {
                    session.send(if focused { b"\x1b[I".to_vec() } else { b"\x1b[O".to_vec() });
                }
                session.window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                #[allow(deprecated)] // winit's xkb backend still reports Hyper_L/R as NamedKey::Hyper.
                if event.logical_key == Key::Named(NamedKey::Hyper) {
                    session.hyper = pressed;
                }
                if pressed {
                    if session.exited {
                        self.close_window(event_loop, id);
                        return;
                    }
                    session.blink_epoch = Instant::now();
                    if session.trace_latency && session.pending_key.is_none() {
                        session.pending_key = Some(session.blink_epoch);
                    }
                    let action = session.binding_for(&event);
                    if action == Some(Action::ReloadConfig) {
                        self.reload_config();
                        return;
                    }
                    if session.palette.is_some() {
                        match action {
                            Some(Action::CommandPalette) => session.close_palette(),
                            _ => session.palette_key(&event),
                        }
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
                } else if session.search.is_some() || session.palette.is_some() {
                    return;
                }
                let key_modes = {
                    let term = session.shared.term.lock();
                    input::KeyModes {
                        app_cursor: term.modes().contains(Modes::APP_CURSOR),
                        app_keypad: term.modes().contains(Modes::APP_KEYPAD),
                        kitty_flags: term.keyboard_flags(),
                    }
                };
                let mods = input::Mods::new(session.key_modifiers(&event), session.hyper, false);
                if let Some(bytes) = input::encode(&event, mods, key_modes) {
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
                if session.palette.is_some() {
                    session.palette_text(&text);
                } else if session.search.is_some() {
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
            WindowEvent::DragEntered { id, .. } | WindowEvent::DragPosition { id, .. } => {
                session.drag_over(event_loop, id)
            }
            WindowEvent::DragDropped { id, .. } => session.drag_dropped(id),
            WindowEvent::DragLeft { id } => session.drag_left(id),
            WindowEvent::DataTransferReceived { id, value, .. } => session.drag_data(id, value.as_ref()),
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

    /// Modifiers for encoding keys.
    #[cfg(not(target_os = "macos"))]
    fn key_modifiers(&self, _event: &KeyEvent) -> ModifiersState {
        self.modifiers
    }

    /// Modifiers for encoding keys. Option keys not configured as Alt compose
    /// characters, which are sent as they are, except with keys where Option
    /// composes nothing, such as Backspace: Option+Backspace deletes a word.
    #[cfg(target_os = "macos")]
    fn key_modifiers(&self, event: &KeyEvent) -> ModifiersState {
        let mut modifiers = self.modifiers;
        if !macos::option_is_alt(self.settings.option_as_alt, self.option_keys)
            && input::option_composes(&event.logical_key)
        {
            modifiers.remove(ModifiersState::ALT);
        }
        modifiers
    }

    /// The modifier held to open links: Ctrl, or Command on macOS where Ctrl+click is a right click.
    fn link_modifier(&self) -> bool {
        #[cfg(not(target_os = "macos"))]
        return self.modifiers.control_key();
        #[cfg(target_os = "macos")]
        return self.modifiers.meta_key();
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
        self.animation_fps = config.shader.fps;
        self.harness_watch.set_names(harness::names(&config.harness));
        self.harness_config = config.harness.clone();
        if let Some(name) = &self.harness_line.name {
            self.harness_line.color = harness::color(name, &config.harness).unwrap_or(self.harness_line.color);
        }
        let mut problems = Vec::new();
        let (bindings, errors) = config.bindings();
        for error in errors {
            problems.push(format!("keybindings: {error}"));
        }
        self.bindings = bindings;
        #[cfg(target_os = "macos")]
        menu::update(&self.bindings);

        // Font changes rebuild the glyph atlases, which is slow. Previews that only
        // change colors or shaders, like browsing themes, keep them.
        let font_key = format!("{:?} {} {}", config.font, config.window.padding_x, config.window.padding_y);
        if font_key != self.applied_font {
            self.applied_font = font_key;
            self.apply_font_config(config, &mut problems);
        }
        self.renderer.set_bidi(config.font.bidi);

        match config.colors(paths) {
            Ok(colors) => {
                let palette = Palette::from_ansi(
                    colors.foreground.to_array(),
                    colors.background.to_array(),
                    colors.cursor.to_array(),
                    colors.ansi(),
                );
                self.shared.term.lock().set_default_palette(palette);
                // Decorations follow the background: light title text on dark themes, and on
                // macOS a matching blur material.
                self.window.set_theme(Some(window_theme(colors.background.to_array())));
                self.renderer.set_theme(Theme {
                    cursor_text: colors.cursor_text.map(|c| c.to_array()),
                    selection_background: colors.selection_background.to_array(),
                    selection_foreground: colors.selection_foreground.map(|c| c.to_array()),
                    opacity: config.window.opacity.clamp(0.0, 1.0),
                    bold_is_bright: colors.bold_is_bright,
                });
            }
            Err(error) => problems.push(error.to_string()),
        }

        let translucent = config.window.opacity < 1.0 && self.renderer.supports_transparency();
        let blur = translucent && config.window.blur;
        if self.applied_translucency != Some((translucent, blur)) {
            self.applied_translucency = Some((translucent, blur));
            self.window.set_transparent(translucent);
            self.window.set_blur(blur);
        }
        #[cfg(target_os = "macos")]
        macos::set_option_as_alt(self.window.as_ref(), config.window.option_as_alt);

        let shaders: Vec<PostShader> = config
            .shader_sources(paths)
            .into_iter()
            .filter_map(|source| match source {
                Ok(source) => Some(PostShader { name: source.name, source: source.source }),
                Err(error) => {
                    problems.push(error.to_string());
                    None
                }
            })
            .collect();
        let animation = match config.shader.animation {
            Animation::Auto => None,
            Animation::Always => Some(true),
            Animation::Never => Some(false),
        };
        // Pipelines compile in the background; an unchanged chain keeps its pipelines and errors.
        let chain: Vec<(String, String)> = shaders.iter().map(|s| (s.name.clone(), s.source.clone())).collect();
        if self
            .applied_shaders
            .as_ref()
            .is_none_or(|(applied, applied_animation)| *applied != chain || *applied_animation != animation)
        {
            self.renderer.set_shaders(&shaders, animation);
            self.applied_shaders = Some((chain, animation));
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
        self.config_problems = problems.clone();
        problems.extend(self.shader_errors.iter().cloned());
        self.show_config_errors(problems);
        self.window.request_redraw();
    }

    /// Installs shaders compiled in the background and shows their errors.
    fn poll_shaders(&mut self) {
        let installed = self.renderer.poll_shaders();
        if !installed.any {
            return;
        }
        if let Some(errors) = installed.user_errors {
            self.shader_errors = errors;
            let mut problems = self.config_problems.clone();
            problems.extend(self.shader_errors.iter().cloned());
            if problems != self.config_errors {
                self.show_config_errors(problems);
            }
        }
        self.window.request_redraw();
    }

    /// Compiles every available shader in the background once, so switching
    /// between them on the startup screen does not wait for the GPU compiler.
    fn warm_shaders(&mut self, paths: Option<&Paths>) {
        if std::mem::replace(&mut self.shaders_warmed, true) {
            return;
        }
        let files = tron_config::shader_names(paths);
        let config = Config { shader: tron_config::ShaderConfig { files, ..Default::default() }, ..Config::default() };
        let shaders = config
            .shader_sources(paths)
            .into_iter()
            .flatten()
            .map(|source| PostShader { name: source.name, source: source.source })
            .collect();
        self.renderer.warm_shaders(shaders);
    }

    /// Loads the fonts, features and size from `config`.
    fn apply_font_config(&mut self, config: &Config, problems: &mut Vec<String>) {
        if config.font.family != self.font_family {
            match FontSystem::new(&config.font.family, config.font.size, self.scale_factor) {
                Ok(fonts) => {
                    self.fonts = fonts;
                    self.font_family = config.font.family.clone();
                }
                Err(error) => problems.push(error.to_string()),
            }
        }
        self.fonts.set_fallback(&config.font.fallback);
        self.fonts.set_features(&config.font.shaping_features());
        self.fonts.set_style_families(
            config.font.bold_family.as_deref(),
            config.font.italic_family.as_deref(),
            config.font.bold_italic_family.as_deref(),
        );
        let variations: Vec<(String, f32)> =
            config.font.variations.iter().map(|(tag, value)| (tag.clone(), *value)).collect();
        self.fonts.set_variations(&variations);
        self.fonts.set_hinting(match config.font.hinting {
            tron_config::Hinting::Auto => tron_font::Hinting::Auto,
            tron_config::Hinting::On => tron_font::Hinting::On,
            tron_config::Hinting::Off => tron_font::Hinting::Off,
        });
        self.set_font_size(config.font.size);
    }

    /// Logs configuration problems and shows them at the top of the window for a while.    /// An empty list removes earlier ones.
    fn show_config_errors(&mut self, errors: Vec<String>) {
        for error in &errors {
            log::error!("{error}");
        }
        self.config_errors_until = (!errors.is_empty()).then(|| Instant::now() + CONFIG_ERROR_TIME);
        self.config_errors = errors;
        self.update_overlays();
        self.window.request_redraw();
    }

    /// Commands from the startup screen, accepted only with its token: `shader=on`,
    /// `shader=off`, `scene=<n>` and `params=<x>:<y>:<z>:<w>`, separated by commas.
    fn startup_command(&mut self, token: &str, payload: &str) {
        if self.startup_token.as_deref() != Some(token) {
            log::debug!("ignoring a startup screen command without the right token");
            return;
        }
        let (mut scene, mut params) = self.renderer.startup_params();
        if let Some((last, _)) = &mut self.startup_shader {
            *last = Instant::now();
        }
        for command in payload.split(',') {
            match command {
                "alive" => continue,
                "restore" => {
                    self.preview_request = Some(PreviewRequest::Restore);
                    continue;
                }
                "commit" => {
                    self.preview_request = Some(PreviewRequest::Commit);
                    continue;
                }
                _ => {}
            }
            match command.split_once('=') {
                Some(("preview", encoded)) => {
                    use base64::Engine;
                    match base64::engine::general_purpose::STANDARD.decode(encoded).map(String::from_utf8) {
                        Ok(Ok(toml)) => self.preview_request = Some(PreviewRequest::Show(toml)),
                        _ => log::debug!("startup screen sent an undecodable preview"),
                    }
                }
                Some(("shader", "on")) => {
                    if self.renderer.set_startup_shader(true) && self.startup_shader.is_none() {
                        let marks = self.shared.term.lock().command_marks().count();
                        self.startup_shader = Some((Instant::now(), marks));
                    }
                }
                Some(("shader", "off")) => {
                    self.renderer.set_startup_shader(false);
                    self.startup_shader = None;
                }
                Some(("scene", value)) => scene = value.parse().unwrap_or(scene),
                Some(("params", values)) => {
                    for (slot, value) in params.iter_mut().zip(values.split(':')) {
                        if let Ok(value) = value.parse::<f32>()
                            && value.is_finite()
                        {
                            *slot = value.clamp(-100.0, 100.0);
                        }
                    }
                }
                _ => log::debug!("unknown startup screen command {command:?}"),
            }
        }
        self.renderer.set_startup_params(scene, params);
        self.window.request_redraw();
    }

    /// Shows a desktop notification when the user's settings and the application allow it.
    fn notify(&self, title: &str, body: &str, when: Option<NotifyWhen>) {
        let focused_ok = match (self.settings.notify_mode, when) {
            (NotifyMode::Never, _) => return,
            (NotifyMode::Always, None | Some(NotifyWhen::Always)) => true,
            _ => false,
        };
        // Visibility is not known, so "invisible" counts as unfocused.
        if self.focused && !focused_ok {
            return;
        }
        #[cfg(not(target_os = "macos"))]
        let command = {
            let mut command = Command::new(&self.settings.notify_command);
            command.args(["--app-name", "tron", "--", title, body]);
            command
        };
        #[cfg(target_os = "macos")]
        let command = macos::notification(&self.settings.notify_command, title, body);
        spawn_detached(command);
    }

    /// Output of the command at the top of a scrolled view, or of the last command.
    fn command_output(&self) -> Option<(tron_core::Point, tron_core::Point)> {
        let term = self.shared.term.lock();
        let grid = term.grid();
        if grid.display_offset() > 0 {
            let top = grid.viewport_line(0);
            // After jumping to a prompt, the top row is the prompt and output starts below it.
            term.command_output(Some(top)).or_else(|| term.command_output(Some(top + 1)))
        } else {
            term.command_output(None)
        }
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
                TermEvent::Notification { title, body, when } => self.notify(&title, &body, when),
                TermEvent::StartupScreen { token, payload } => self.startup_command(&token, &payload),
                TermEvent::PointerShape(name) => self.app_pointer = pointer_icon(&name),
                TermEvent::ColumnsChanged(cols) => {
                    // DECCOLM: resize the window to fit the new width, keeping the height.
                    let metrics = self.fonts.metrics();
                    let padding = padding(self.settings.padding, self.scale_factor);
                    let width = cols as f32 * metrics.width as f32 + 2.0 * padding[0];
                    let height = self.window.surface_size().height;
                    let _ = self.window.request_surface_size(PhysicalSize::new(width.ceil() as u32, height).into());
                }
            }
        }
    }

    /// Redraws at most once per display refresh. Output that arrives faster is
    /// batched into the next frame instead of rendering every chunk.
    fn schedule_redraw(&mut self) {
        if Instant::now() >= self.next_frame {
            self.window.request_redraw();
        } else {
            self.wake_at = Some(self.wake_at.map_or(self.next_frame, |at| at.min(self.next_frame)));
        }
    }

    fn redraw(&mut self, event_loop: &dyn ActiveEventLoop) {
        #[cfg(target_os = "macos")]
        {
            self.update_represented_directory();
            self.update_look_up();
        }
        {
            let mut term = self.shared.term.lock();
            if term.sync_blocked() {
                self.wake_at = Some(Instant::now() + SYNC_POLL);
                return;
            }
            if let Some(title) = term.take_title() {
                self.program_title = (!title.is_empty()).then_some(title);
                self.window.set_title(&window_title(
                    self.program_title.as_deref(),
                    self.title_directory(),
                    std::env::home_dir().as_deref(),
                    &self.settings.title,
                ));
            }
            term.snapshot(&mut self.snapshot);
            let grid = term.grid();
            let (scrollback, offset) = (grid.scrollback_len(), grid.display_offset());
            // Scrolling through history shows the scrollbar; output growing the history
            // under a scrolled view moves the offset too, but by as much as the history.
            let bar = &mut self.scrollbar;
            if bar.enabled && offset != bar.offset && offset.abs_diff(bar.offset) != scrollback.abs_diff(bar.scrollback)
            {
                bar.shown_at = Some(Instant::now());
            }
            (bar.scrollback, bar.offset) = (scrollback, offset);
            // The startup screen's shader ends when the shell shows its first prompt,
            // or when the screen stopped sending commands without turning it off.
            if let Some((since, marks)) = self.startup_shader
                && (since.elapsed() > STARTUP_SHADER_IDLE || term.command_marks().count() > marks)
            {
                self.startup_shader = None;
                self.renderer.set_startup_shader(false);
                self.preview_request = Some(PreviewRequest::Restore);
            }
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

        // Configuration errors disappear after a while.
        match self.config_errors_until {
            Some(until) if until <= now => {
                self.config_errors.clear();
                self.config_errors_until = None;
                self.update_overlays();
            }
            Some(until) => wake_at(until),
            None => {}
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

        // Animated images.
        if let Some(due) = self.snapshot.next_frame_due {
            wake_at(due.max(now + Duration::from_millis(1)));
        }

        if let Some(due) = self.update_scrollbar(now, self.snapshot.rows()) {
            wake_at(due);
        }
        if let Some(due) = self.update_harness_line(now) {
            wake_at(due);
        }

        // Built without the lock, so the reader keeps parsing meanwhile.
        self.renderer.prepare(&self.snapshot, &mut self.fonts, self.focused);
        let capture_now = self.screenshot.as_ref().is_some_and(|(_, due)| now >= *due);
        if capture_now && let Some((path, _)) = self.screenshot.take() {
            self.renderer.capture_next_frame(path);
            self.capture_requested = true;
        }
        let presented = self.renderer.render();
        if !presented {
            wake_at(now + GPU_BUSY_POLL);
        }
        self.last_frame = Instant::now();
        // The next frame is due one interval after this one was, so timer slop does
        // not add up to a lower frame rate. Early or late frames restart the cadence.
        self.next_frame = match now.checked_duration_since(self.next_frame) {
            Some(late) if late < self.frame_interval => self.next_frame + self.frame_interval,
            _ => now + self.frame_interval,
        };
        if let Some(pressed) = self.pending_key
            && presented
            && self.snapshot.damaged.iter().any(|&damaged| damaged)
        {
            log::info!(
                "latency: key press to presented frame {:.2} ms",
                (self.last_frame - pressed).as_secs_f64() * 1000.0
            );
            self.pending_key = None;
        }
        self.update_ime_area();
        if let Some(accessibility) = &mut self.accessibility {
            let metrics = self.fonts.metrics();
            let [pad_x, pad_y] = padding(self.settings.padding, self.scale_factor);
            let layout = accessibility::Layout {
                cell_width: f64::from(metrics.width),
                cell_height: f64::from(metrics.height),
                padding: [f64::from(pad_x), f64::from(pad_y + self.top_inset)],
            };
            if let Some(due) = accessibility.update(&self.snapshot, layout) {
                wake_at(due);
            }
        }

        // The capture is saved with the next presented frame, which a busy GPU can delay.
        if self.capture_requested && presented {
            event_loop.exit();
            return;
        }
        if let Some((_, due)) = &self.screenshot {
            wake_at(*due);
        }
        // Animations stop while nobody can see them. Cursor effects and the startup
        // screen keep the display's rate; backgrounds run at `[shader] fps`.
        if !self.occluded {
            if self.renderer.has_smooth_animation() {
                wake_at(self.next_frame);
            } else if self.renderer.has_ambient_animation() {
                wake_at(self.last_frame + animation_interval(self.animation_fps, self.focused, self.frame_interval));
            }
        }
        self.wake_at = next_wake;
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

    /// Follows the height of a title bar drawn over the content, which changes in full screen.
    fn update_top_inset(&mut self) {
        let inset = top_inset(self.window.as_ref());
        if inset != self.top_inset {
            self.top_inset = inset;
            self.renderer.set_padding(padding(self.settings.padding, self.scale_factor), inset);
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
        // The Find box and the command palette sit at the right edge, which moved.
        if changed && (self.search.is_some() || self.palette.is_some()) {
            self.update_overlays();
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
                self.palette = None;
                self.search = Some(SearchState::default());
                self.update_overlays();
                self.window.request_redraw();
            }
            Action::CommandPalette => {
                self.search = None;
                self.palette = Some(panel::PaletteState::default());
                self.update_overlays();
                self.window.request_redraw();
            }
            Action::NewWindow => self.new_window(),
            Action::SendText(text) => {
                self.prepare_input();
                self.send(text.into_bytes());
            }
            Action::ScrollToPreviousPrompt | Action::ScrollToNextPrompt => {
                self.shared.term.lock().scroll_to_prompt(action == Action::ScrollToPreviousPrompt);
                self.window.request_redraw();
            }
            Action::SelectCommandOutput | Action::CopyCommandOutput => {
                let Some((start, end)) = self.command_output() else { return };
                let text = {
                    let mut term = self.shared.term.lock();
                    term.set_selection(Some(Selection { kind: SelectionKind::Simple, anchor: start, head: end }));
                    term.scroll_to_line(start.line);
                    term.selection_text()
                };
                if let Some(text) = text {
                    if action == Action::CopyCommandOutput {
                        self.clipboard.store(false, text);
                    } else if self.settings.copy_on_select {
                        self.clipboard.store(true, text);
                    }
                }
                self.window.request_redraw();
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

    fn palette_key(&mut self, event: &KeyEvent) {
        let rows = self.renderer.grid_size().1;
        let Some(palette) = &mut self.palette else { return };
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => return self.close_palette(),
            Key::Named(NamedKey::Enter) => return self.run_palette_item(),
            Key::Named(NamedKey::ArrowUp) => palette.move_selection(-1, rows),
            Key::Named(NamedKey::ArrowDown | NamedKey::Tab) => palette.move_selection(1, rows),
            Key::Named(NamedKey::Backspace) => palette.pop_char(),
            _ => match event.text.clone() {
                Some(text) if !self.modifiers.control_key() && !self.modifiers.meta_key() => palette.push_text(&text),
                _ => return,
            },
        }
        self.update_overlays();
        self.window.request_redraw();
    }

    fn palette_text(&mut self, text: &str) {
        if let Some(palette) = &mut self.palette {
            palette.push_text(text);
            self.update_overlays();
            self.window.request_redraw();
        }
    }

    fn close_palette(&mut self) {
        self.palette = None;
        self.update_overlays();
        self.window.request_redraw();
    }

    /// What the pointer is over on the command palette, or `None` while it is closed.
    fn palette_hit(&self) -> Option<panel::PaletteHit> {
        let palette = self.palette.as_ref()?;
        let (row, col) = self.renderer.cell_at(self.mouse.position.0, self.mouse.position.1);
        let (cols, rows) = self.renderer.grid_size();
        Some(panel::palette_hit(palette, cols, rows, row, col))
    }

    /// Closes the command palette and runs the highlighted command.
    fn run_palette_item(&mut self) {
        let item = self.palette.as_ref().and_then(panel::PaletteState::selected_item);
        self.close_palette();
        let Some(item) = item else { return };
        match item.command() {
            menu::MenuCommand::Action(Action::ReloadConfig) => self.app_request = Some(AppRequest::ReloadConfig),
            menu::MenuCommand::Action(action) => self.run_action(action),
            menu::MenuCommand::OpenSettings => self.app_request = Some(AppRequest::OpenSettings),
            menu::MenuCommand::OpenUrl(url) => self.open_link(url),
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

    /// Rebuilds the search box, command palette, config error and IME preedit overlays.
    fn update_overlays(&mut self) {
        let mut overlays = Vec::new();
        let (rows, cols, palette, cursor) = {
            let term = self.shared.term.lock();
            (term.rows(), term.cols(), *term.palette(), term.cursor())
        };
        if let Some(search) = &self.search {
            let status = match (&search.current, search.query.is_empty()) {
                (_, true) => " esc close ",
                (Some(_), false) => " ⏎ older  ⇧⏎ newer  esc close ",
                (None, false) => " no matches ",
            };
            overlays.extend(panel::search_overlays(&search.query, status, cols, rows, &palette));
        }
        if let Some(state) = &self.palette {
            overlays.extend(panel::palette_overlays(state, &self.bindings, cols, rows, &palette));
        }
        const SHOWN_ERRORS: usize = 4;
        for (row, error) in self.config_errors.iter().take(SHOWN_ERRORS).enumerate() {
            let first_line = error.lines().next().unwrap_or_default();
            let more = if row + 1 == SHOWN_ERRORS && self.config_errors.len() > SHOWN_ERRORS {
                " (more in the log)"
            } else {
                ""
            };
            overlays.push(Overlay {
                row,
                col: 0,
                text: format!(" config: {first_line}{more} "),
                fg: [0xff, 0xff, 0xff],
                bg: [0xb0, 0x30, 0x40],
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

    /// The shell's working directory: reported through OSC 7, else read from the
    /// shell process, since shells without integration do not report it.
    fn working_directory(&self) -> Option<PathBuf> {
        let (shared, pty) = match &self.suspended {
            Some(shell) => (&shell.shared, &shell.pty),
            None => (&self.shared, &self.pty),
        };
        let reported = shared.term.lock().cwd().map(percent_decode).map(PathBuf::from);
        reported.or_else(|| pty.cwd()).filter(|path| path.is_dir())
    }

    /// Runs `options` in this window in place of its shell, which keeps running
    /// out of sight and comes back when the program exits.
    fn run_in_place(
        &mut self,
        options: &SpawnOptions,
        proxy: &EventLoopProxy,
        config: &Config,
        paths: Option<&Paths>,
    ) -> anyhow::Result<()> {
        if self.suspended.is_some() || self.exited {
            return Ok(());
        }
        let (cols, rows) = self.renderer.grid_size();
        let metrics = self.fonts.metrics();
        let mut term = Terminal::new(cols, rows, 0);
        term.set_cell_pixels(metrics.width, metrics.height);
        let pty = Pty::spawn(options, window_size(cols, rows, metrics))?;
        let shared = Arc::new(Shared {
            term: Mutex::new(term),
            wake_pending: AtomicBool::new(false),
            exited: AtomicBool::new(false),
        });
        let input = spawn_writer(pty.writer()?)?;
        spawn_reader(pty.reader()?, shared.clone(), proxy.clone(), input.clone())?;
        self.suspended = Some(Suspended {
            pty: std::mem::replace(&mut self.pty, pty),
            shared: std::mem::replace(&mut self.shared, shared),
            input: std::mem::replace(&mut self.input, input),
        });
        self.search = None;
        // Every row of the new terminal is drawn, not only the ones it changes.
        self.snapshot = Snapshot::default();
        // A new terminal starts with the built-in colors: give it the theme and settings.
        self.apply_config(config, paths);
        Ok(())
    }

    /// Brings the shell back after the program run in its place exited. Settings
    /// changed there applied to that program's terminal, so the shell's gets them now.
    fn restore_shell(&mut self, config: &Config, paths: Option<&Paths>) {
        let Some(shell) = self.suspended.take() else { return };
        self.pty = shell.pty;
        self.shared = shell.shared;
        self.input = shell.input;
        self.search = None;
        self.snapshot = Snapshot::default();
        self.apply_config(config, paths);
        // The window may have been resized meanwhile.
        self.resize_grid();
        self.program_title = None;
        let home = std::env::home_dir();
        self.window.set_title(&window_title(None, self.title_directory(), home.as_deref(), &self.settings.title));
    }

    /// Gives Look Up the current terminal and cell layout.
    #[cfg(target_os = "macos")]
    fn update_look_up(&self) {
        let mut layout = self.look_up.0.borrow_mut();
        if !Arc::ptr_eq(&layout.shared, &self.shared) {
            layout.shared = self.shared.clone();
        }
        let [left, top] = padding(self.settings.padding, self.scale_factor);
        layout.origin = [left, top + self.top_inset];
        layout.metrics = self.fonts.metrics();
        layout.grid = self.renderer.grid_size();
        layout.scale_factor = self.scale_factor;
        if layout.font_family != self.font_family {
            layout.font_family.clone_from(&self.font_family);
        }
        layout.font_size = self.font_size;
    }

    /// Follows the shell's directory with the folder icon beside the title.
    #[cfg(target_os = "macos")]
    fn update_represented_directory(&mut self) {
        if self.represented_directory.1.elapsed() < REPRESENTED_DIRECTORY_POLL {
            return;
        }
        self.represented_directory.1 = Instant::now();
        let directory = self.working_directory();
        if directory != self.represented_directory.0 {
            macos::set_represented_directory(self.window.as_ref(), directory.as_deref());
            self.represented_directory.0 = directory;
            if self.program_title.is_none() {
                let home = std::env::home_dir();
                self.window.set_title(&window_title(
                    None,
                    self.title_directory(),
                    home.as_deref(),
                    &self.settings.title,
                ));
            }
        }
    }

    /// The folder a window without a program title is named after: the shell's
    /// directory on macOS, beside its folder icon, and none elsewhere.
    fn title_directory(&self) -> Option<&Path> {
        #[cfg(target_os = "macos")]
        return self.represented_directory.0.as_deref();
        #[cfg(not(target_os = "macos"))]
        None
    }

    /// Asks for a window in the shell's directory, opened in this process.
    fn new_window(&mut self) {
        self.window_request = Some(Launch { cwd: self.working_directory(), ..Launch::default() });
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
        let link = if self.link_modifier() && !self.mouse_reporting(modes) {
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
    fn recreate_renderer(
        &mut self,
        config: &Config,
        paths: Option<&Paths>,
        proxy: &EventLoopProxy,
    ) -> anyhow::Result<()> {
        let size = self.window.surface_size();
        let theme = Theme { opacity: config.window.opacity.clamp(0.0, 1.0), ..Theme::default() };
        let metrics = self.fonts.metrics();
        let padding = padding(self.settings.padding, self.scale_factor);
        self.renderer =
            pollster::block_on(Renderer::new(self.window.clone(), size.width, size.height, metrics, padding, theme))?;
        self.renderer.set_padding(padding, self.top_inset);
        let proxy = proxy.clone();
        self.renderer.set_shader_notify(move || proxy.wake_up());
        // The new renderer has no pipelines: request the shaders again.
        self.applied_shaders = None;
        self.shaders_warmed = false;
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

    /// Accepts a drag over the window and asks for its data early: Wayland
    /// ends the transfer as soon as the drop happens.
    fn drag_over(&mut self, event_loop: &dyn ActiveEventLoop, id: DataTransferId) {
        if self.drop.as_ref().is_none_or(|drop| drop.id != id) {
            self.drop = Some(DropState { id, requested: false, text: None, dropped: false });
        }
        if let Err(error) = event_loop.set_valid_dnd_actions(id, &[DndAction::Copy]) {
            log::debug!("cannot accept drag: {error}");
        }
        let Some(drop) = &mut self.drop else { return };
        if drop.requested {
            return;
        }
        drop.requested = [TypeHint::UriList, TypeHint::Plaintext]
            .iter()
            .any(|hint| event_loop.fetch_data_transfer(id, hint).is_ok());
    }

    fn drag_data(&mut self, id: DataTransferId, value: &dyn TypedData) {
        let Some(drop) = self.drop.as_mut().filter(|drop| drop.id == id) else { return };
        if drop.text.is_none() {
            drop.text = dropped_text(value);
        }
        if drop.dropped {
            self.finish_drop();
        }
    }

    fn drag_dropped(&mut self, id: DataTransferId) {
        let Some(drop) = self.drop.as_mut().filter(|drop| drop.id == id) else { return };
        drop.dropped = true;
        if drop.text.is_some() {
            self.finish_drop();
        }
    }

    fn drag_left(&mut self, id: DataTransferId) {
        // Wayland also reports leaving after a drop, before the data arrives.
        if self.drop.as_ref().is_some_and(|drop| drop.id == id && !drop.dropped) {
            self.drop = None;
        }
    }

    fn finish_drop(&mut self) {
        if let Some(text) = self.drop.take().and_then(|drop| drop.text)
            && !text.is_empty()
        {
            self.paste(&text);
            self.window.request_redraw();
        }
    }

    /// Whether `y` (physical pixels) lies in a title bar drawn over the content (macOS).
    fn in_title_bar(&self, y: f64) -> bool {
        y < f64::from(self.top_inset)
    }

    /// Arrow over the title bar and while the application receives mouse events,
    /// I-beam while the mouse selects text (including Shift held over a
    /// mouse-reporting app).
    fn update_pointer_icon(&mut self, modes: Modes) {
        if !modes.intersects(Modes::MOUSE_TRACKING) {
            self.app_pointer = None;
        }
        let over_palette = self.palette_hit().is_some_and(|hit| hit != panel::PaletteHit::Outside);
        let icon = if self.in_title_bar(self.mouse.position.1) || self.scrollbar.hovered || over_palette {
            CursorIcon::Default
        } else if self.hovered_link.is_some() {
            CursorIcon::Pointer
        } else if self.mouse_reporting(modes) {
            self.app_pointer.unwrap_or(CursorIcon::Default)
        } else {
            CursorIcon::Text
        };
        if icon != self.pointer_icon {
            self.pointer_icon = icon;
            self.window.set_cursor(icon.into());
        }
    }

    /// Encodes a mouse report in the format the application selected.
    fn mouse_report(
        &self,
        modes: Modes,
        code: u8,
        pressed: bool,
        motion: bool,
        row: usize,
        col: usize,
    ) -> Option<Vec<u8>> {
        if modes.contains(Modes::MOUSE_SGR_PIXELS) {
            let (x, y) = self.grid_pixel(self.mouse.position.0, self.mouse.position.1);
            return mouse::encode_pixels(code, pressed, motion, x, y, self.modifiers);
        }
        mouse::encode(code, pressed, motion, row, col, self.modifiers, modes.contains(Modes::MOUSE_SGR))
    }

    /// Pointer position in pixels from the top left of the cell grid, clamped to the grid.
    fn grid_pixel(&self, x: f64, y: f64) -> (u32, u32) {
        let [pad_x, pad_y] = padding(self.settings.padding, self.scale_factor);
        let pad_y = pad_y + self.top_inset;
        let metrics = self.fonts.metrics();
        let (cols, rows) = self.renderer.grid_size();
        let width = (cols as u32 * metrics.width).max(1) - 1;
        let height = (rows as u32 * metrics.height).max(1) - 1;
        let clamp = |value: f64, pad: f32, max: u32| (value as f32 - pad).max(0.0).min(max as f32) as u32;
        (clamp(x, pad_x, width), clamp(y, pad_y, height))
    }

    fn mouse_reporting(&self, modes: Modes) -> bool {
        modes.intersects(Modes::MOUSE_TRACKING) && !self.modifiers.shift_key()
    }

    fn pointer_button(&mut self, pressed: bool, button: MouseButton) {
        // The command palette takes clicks on it, before an application reading the
        // mouse sees them. Clicking a command runs it; clicking elsewhere closes it.
        if let Some(hit) = self.palette_hit() {
            match hit {
                panel::PaletteHit::Entry(index) if pressed && button == MouseButton::Left => {
                    if let Some(palette) = &mut self.palette {
                        palette.selected = index;
                    }
                    self.run_palette_item();
                }
                panel::PaletteHit::Outside if pressed => self.close_palette(),
                _ => {}
            }
            return;
        }
        if button == MouseButton::Left {
            if !pressed && self.scrollbar.grab.take().is_some() {
                self.scrollbar.shown_at = Some(Instant::now());
                return;
            }
            if pressed && self.over_scrollbar(self.mouse.position.0) {
                if let Some((top, height)) = self.scrollbar.thumb {
                    // Grabbing the thumb keeps where it was held; clicking beside it centers it there.
                    let y = self.mouse.position.1 as f32;
                    self.scrollbar.grab = Some(if (top..top + height).contains(&y) { y - top } else { height / 2.0 });
                    self.drag_scrollbar(self.mouse.position.1);
                }
                return;
            }
        }
        // Clicks on the title bar move the window; they select nothing.
        if pressed && self.in_title_bar(self.mouse.position.1) {
            return;
        }
        if pressed
            && button == MouseButton::Left
            && self.link_modifier()
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
            if let Some(bytes) = self.mouse_report(modes, code, pressed, false, row, col) {
                self.send(bytes);
            }
            self.mouse.last_cell = Some((row, col));
            return;
        }

        // A right click or Control-click opens the context menu, for this window.
        #[cfg(target_os = "macos")]
        if pressed && (button == MouseButton::Right || (button == MouseButton::Left && self.modifiers.control_key())) {
            self.window.focus_window();
            let has_selection = self.shared.term.lock().selection_text().is_some();
            menu::show_context_menu(self.window.as_ref(), self.mouse.position, has_selection);
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
        if self.scrollbar.grab.is_some() {
            self.drag_scrollbar(y);
            return;
        }
        // Over the command palette, the pointer highlights the command under it.
        if let Some(hit) = self.palette_hit() {
            if let panel::PaletteHit::Entry(index) = hit
                && let Some(palette) = &mut self.palette
                && palette.selected != index
            {
                palette.selected = index;
                self.update_overlays();
                self.window.request_redraw();
            }
            let modes = self.shared.term.lock().modes();
            self.update_pointer_icon(modes);
            if hit != panel::PaletteHit::Outside {
                return;
            }
        }
        let hovered = self.over_scrollbar(x);
        if hovered != self.scrollbar.hovered {
            self.scrollbar.hovered = hovered;
            if !hovered {
                // Fades out after the pointer leaves, as after scrolling.
                self.scrollbar.shown_at = Some(Instant::now());
            }
            self.window.request_redraw();
        }
        if hovered {
            let modes = self.shared.term.lock().modes();
            self.update_pointer_icon(modes);
            return;
        }
        if self.link_modifier() || self.hovered_link.is_some() {
            self.update_hover();
        }
        let (row, col) = self.renderer.cell_at(x, y);
        let modes = self.shared.term.lock().modes();
        if self.top_inset > 0.0 {
            // Arrow over the title bar, the usual pointer below it.
            self.update_pointer_icon(modes);
            if self.in_title_bar(y) && !self.mouse.selecting {
                return;
            }
        }
        if self.mouse_reporting(modes) {
            let report =
                modes.contains(Modes::MOUSE_ANY) || (modes.contains(Modes::MOUSE_BUTTON) && self.mouse.buttons != 0);
            let pixels = modes.contains(Modes::MOUSE_SGR_PIXELS);
            let pixel = self.grid_pixel(x, y);
            let moved =
                if pixels { self.mouse.last_pixel != Some(pixel) } else { self.mouse.last_cell != Some((row, col)) };
            if report && moved {
                let code = (0..3).find(|b| self.mouse.buttons & (1 << b) != 0).unwrap_or(mouse::NO_BUTTON);
                if let Some(bytes) = self.mouse_report(modes, code, true, true, row, col) {
                    self.send(bytes);
                }
            }
            self.mouse.last_cell = Some((row, col));
            self.mouse.last_pixel = Some(pixel);
            return;
        }
        if !self.mouse.selecting {
            return;
        }
        let edge = f64::from(self.settings.padding.1) * self.scale_factor + f64::from(self.top_inset);
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
        // The wheel over the command palette scrolls its list.
        if self.palette_hit().is_some_and(|hit| hit != panel::PaletteHit::Outside) {
            let rows = self.renderer.grid_size().1;
            if let Some(palette) = &mut self.palette {
                palette.scroll(-(whole as isize), rows);
            }
            self.update_overlays();
            self.window.request_redraw();
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
            if let Some(bytes) = self.mouse_report(modes, code, true, false, row, col) {
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
            self.show_scrollbar();
            self.scroll_history(whole);
        }
    }

    /// Shows the overlay scrollbar, which fades out again after a moment.
    fn show_scrollbar(&mut self) {
        if self.scrollbar.enabled {
            self.scrollbar.shown_at = Some(Instant::now());
            self.window.request_redraw();
        }
    }

    /// Whether the pointer at `x` is over a visible overlay scrollbar.
    fn over_scrollbar(&self, x: f64) -> bool {
        let zone = 16.0 * self.scale_factor as f32;
        self.scrollbar.visible && x as f32 >= self.renderer.size().0 as f32 - zone
    }

    /// Lays out the overlay scrollbar for the frame and fades it. Returns when it
    /// needs the next frame.
    fn update_scrollbar(&mut self, now: Instant, rows: usize) -> Option<Instant> {
        if !self.scrollbar.enabled {
            return None;
        }
        let scale = self.scale_factor as f32;
        let (width, height) = self.renderer.size();
        let margin = 3.0 * scale;
        let track = (self.top_inset + margin, height as f32 - margin);
        let thumb = scrollbar_thumb(track, rows, self.scrollbar.scrollback, self.scrollbar.offset, 24.0 * scale);
        let active = self.scrollbar.hovered || self.scrollbar.grab.is_some();
        let (alpha, wake) = match self.scrollbar.shown_at {
            _ if active => (1.0, None),
            Some(at) => match now.duration_since(at) {
                elapsed if elapsed < SCROLLBAR_VISIBLE => (1.0, Some(at + SCROLLBAR_VISIBLE)),
                elapsed if elapsed < SCROLLBAR_VISIBLE + SCROLLBAR_FADE => {
                    let faded = (elapsed - SCROLLBAR_VISIBLE).as_secs_f32() / SCROLLBAR_FADE.as_secs_f32();
                    (1.0 - faded, Some(now + self.frame_interval))
                }
                _ => {
                    self.scrollbar.shown_at = None;
                    (0.0, None)
                }
            },
            None => (0.0, None),
        };
        self.scrollbar.track = track;
        self.scrollbar.thumb = thumb;
        self.scrollbar.visible = alpha > 0.0 && thumb.is_some();
        let bar = thumb.filter(|_| alpha > 0.0).map(|(top, thumb_height)| {
            let bar_width = if active { 11.0 } else { 7.0 } * scale;
            let color = match window_theme(self.snapshot.palette.background) {
                WindowTheme::Dark => [255, 255, 255],
                WindowTheme::Light => [0, 0, 0],
            };
            Scrollbar {
                x: width as f32 - bar_width - margin,
                y: top,
                width: bar_width,
                height: thumb_height,
                color,
                alpha: alpha * 0.55,
            }
        });
        self.renderer.set_scrollbar(bar);
        wake
    }

    /// Starts the harness line's sweep when a coding agent harness starts.
    fn update_harness(&mut self) {
        let found = self.harness_watch.found();
        if found == self.harness_line.name {
            return;
        }
        if let Some(color) = found.as_deref().and_then(|name| harness::color(name, &self.harness_config)) {
            self.harness_line.color = color;
            self.harness_line.started_at = Some(Instant::now());
            self.window.request_redraw();
        }
        self.harness_line.name = found;
    }

    /// Draws the harness line's sweep. Returns when the next frame is due while it runs.
    fn update_harness_line(&mut self, now: Instant) -> Option<Instant> {
        let width = self.renderer.size().0 as f32;
        let scale = self.scale_factor as f32;
        let tail = (width * 0.3).max(48.0 * scale);
        let head = self
            .harness_line
            .started_at
            .filter(|_| self.harness_config.line)
            .and_then(|started| sweep_head(now.saturating_duration_since(started), width / 2.0, tail));
        if head.is_none() {
            self.harness_line.started_at = None;
        }
        self.renderer.set_glow_line(head.map(|head| GlowLine {
            center: width / 2.0,
            head,
            tail,
            top: 0.0,
            thickness: (1.5 * scale).max(1.0),
            glow: 12.0 * scale,
            color: self.harness_line.color,
        }));
        head.map(|_| now + self.frame_interval)
    }

    /// Scrolls so the dragged scrollbar thumb follows the pointer.
    fn drag_scrollbar(&mut self, y: f64) {
        let (Some(grab), Some((_, height))) = (self.scrollbar.grab, self.scrollbar.thumb) else { return };
        let (top, bottom) = self.scrollbar.track;
        let range = (bottom - top - height).max(1.0);
        let position = ((y as f32 - grab - top) / range).clamp(0.0, 1.0);
        let target = ((1.0 - position) * self.scrollbar.scrollback as f32).round() as isize;
        let mut term = self.shared.term.lock();
        let current = term.grid().display_offset() as isize;
        term.scroll_display(target - current);
        drop(term);
        self.window.request_redraw();
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

/// Text inserted for dropped data: shell-quoted file paths, or plain text.
fn dropped_text(value: &dyn TypedData) -> Option<String> {
    match value.type_().hint() {
        Some(TypeHint::UriList) => {
            let items: Vec<String> = match value.try_as_file_paths() {
                Ok(paths) if !paths.is_empty() => paths.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
                _ => value.try_as_uris().ok()?,
            };
            Some(items.iter().map(|item| shell_quote(item)).collect::<Vec<_>>().join(" "))
        }
        _ => value.try_as_string().ok(),
    }
}

/// Quotes `text` for POSIX shells: single quotes, with `'` written as `'\''`.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Pointer for a CSS cursor name, or one of the common X cursor names. `None`
/// for the default or unknown names.
fn pointer_icon(name: &str) -> Option<CursorIcon> {
    let css = match name {
        "left_ptr" | "arrow" | "top_left_arrow" => "default",
        "hand" | "hand1" | "hand2" | "pointing_hand" => "pointer",
        "xterm" | "ibeam" => "text",
        "watch" => "wait",
        "fleur" => "move",
        "sb_h_double_arrow" => "ew-resize",
        "sb_v_double_arrow" => "ns-resize",
        other => other,
    };
    css.parse().ok()
}

/// A random hex token from the kernel, or from the clock when that fails.
fn random_token() -> String {
    let mut bytes = [0u8; 16];
    if File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut bytes)).is_err() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        bytes = (nanos ^ u128::from(std::process::id()) << 64).to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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

/// Names the window after the desktop entry (Wayland app id, X11 `WM_CLASS`), so
/// launchers, task bars and desktop search match windows to it and show its icon.
#[cfg(not(target_os = "macos"))]
fn with_app_id(attributes: WindowAttributes, event_loop: &dyn ActiveEventLoop) -> WindowAttributes {
    use winit::platform::wayland::{ActiveEventLoopExtWayland, WindowAttributesWayland};
    use winit::platform::x11::{ActiveEventLoopExtX11, WindowAttributesX11};
    const APP_ID: &str = "dev.tron.Terminal";
    if event_loop.is_wayland() {
        attributes.with_platform_attributes(Box::new(WindowAttributesWayland::default().with_name(APP_ID, "tron")))
    } else if event_loop.is_x11() {
        attributes.with_platform_attributes(Box::new(WindowAttributesX11::default().with_name(APP_ID, "tron")))
    } else {
        attributes
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

/// Light or dark window decorations for a background color, so the title text
/// contrasts with the terminal drawn under a transparent title bar.
fn window_theme([r, g, b]: [u8; 3]) -> WindowTheme {
    let luma = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
    if luma < 128.0 { WindowTheme::Dark } else { WindowTheme::Light }
}

/// Physical pixels at the top of the surface covered by window decorations: the
/// transparent macOS title bar (see `macos::window_attributes`), zero elsewhere
/// and in full screen.
fn top_inset(window: &dyn Window) -> f32 {
    window.safe_area().top as f32
}

fn padding(padding: (u16, u16), scale_factor: f64) -> [f32; 2] {
    let scale = scale_factor as f32;
    [f32::from(padding.0) * scale, f32::from(padding.1) * scale]
}

/// Time between frames of continuous shader animations: the display's rate for
/// `fps` 0, otherwise `fps` frames per second, at most [`UNFOCUSED_FPS`] without
/// focus, and never faster than the display.
fn animation_interval(fps: u32, focused: bool, frame_interval: Duration) -> Duration {
    let fps = match (fps, focused) {
        (0, _) => return frame_interval,
        (fps, true) => fps,
        (fps, false) => fps.min(UNFOCUSED_FPS),
    };
    Duration::from_secs_f64(1.0 / f64::from(fps)).max(frame_interval)
}

/// The overlay scrollbar thumb's top and height for a track from `track.0` to
/// `track.1`: as tall as the visible share of the history, at least `min_height`,
/// and at the bottom when the view shows the newest lines. `None` without history.
/// The window title: the one a program set, else `directory` with the home
/// directory written as `~`, else `default`, the `title` setting.
fn window_title(program: Option<&str>, directory: Option<&Path>, home: Option<&Path>, default: &str) -> String {
    if let Some(title) = program {
        return title.to_owned();
    }
    let Some(directory) = directory else { return default.to_owned() };
    match home.and_then(|home| directory.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => directory.display().to_string(),
    }
}

fn scrollbar_thumb(
    track: (f32, f32),
    rows: usize,
    scrollback: usize,
    offset: usize,
    min_height: f32,
) -> Option<(f32, f32)> {
    if scrollback == 0 {
        return None;
    }
    let length = (track.1 - track.0).max(0.0);
    let height = (length * rows as f32 / (rows + scrollback) as f32).max(min_height).min(length);
    let position = 1.0 - offset.min(scrollback) as f32 / scrollback as f32;
    Some((track.0 + (length - height) * position, height))
}

/// Highest refresh rate any connected monitor supports, in hertz.
fn max_refresh_rate(event_loop: &dyn ActiveEventLoop) -> Option<u32> {
    event_loop
        .available_monitors()
        .flat_map(|monitor| monitor.video_modes().chain(monitor.current_video_mode()))
        .filter_map(|mode| mode.refresh_rate_millihertz())
        .map(|millihertz| millihertz.get().div_ceil(1000))
        .max()
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

/// The app icon, for compositors that take it from the window, like KDE Plasma
/// on Wayland and X11 window managers. Others find it through the desktop entry.
fn window_icon() -> Option<Icon> {
    const PNG: &[u8] = include_bytes!("../../../dist/dev.tron.Terminal.png");
    let mut reader = png::Decoder::new(std::io::Cursor::new(PNG)).read_info().ok()?;
    let mut rgba = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut rgba).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    rgba.truncate(info.buffer_size());
    RgbaIcon::new(rgba, info.width, info.height).ok().map(Icon::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_icon_decodes() {
        assert!(window_icon().is_some());
    }

    #[test]
    fn harness_sweep_runs_from_the_center_past_the_sides() {
        let (half, tail) = (500.0, 150.0);
        assert_eq!(sweep_head(Duration::ZERO, half, tail), Some(0.0));
        let middle = sweep_head(HARNESS_SWEEP / 2, half, tail).unwrap();
        assert!(middle > half / 2.0 && middle < half + tail, "eases out: {middle}");
        let late = sweep_head(HARNESS_SWEEP.mul_f32(0.99), half, tail).unwrap();
        assert!(late - tail > half * 0.95, "the tail has nearly left the window: {late}");
        assert_eq!(sweep_head(HARNESS_SWEEP, half, tail), None);
    }

    #[test]
    fn window_title_prefers_the_program_then_the_folder() {
        let home = Some(Path::new("/Users/efe"));
        let projects = Some(Path::new("/Users/efe/Programming"));
        assert_eq!(window_title(Some("vim"), projects, home, "tron"), "vim");
        assert_eq!(window_title(None, projects, home, "tron"), "~/Programming");
        assert_eq!(window_title(None, home, home, "tron"), "~");
        assert_eq!(window_title(None, Some(Path::new("/tmp")), home, "tron"), "/tmp");
        assert_eq!(window_title(None, None, home, "tron"), "tron");
    }

    #[test]
    fn scrollbar_thumb_follows_the_view() {
        assert_eq!(scrollbar_thumb((0.0, 100.0), 10, 0, 0, 5.0), None, "no history, no scrollbar");
        assert_eq!(scrollbar_thumb((0.0, 100.0), 10, 90, 0, 5.0), Some((90.0, 10.0)), "newest lines at the bottom");
        assert_eq!(scrollbar_thumb((0.0, 100.0), 10, 90, 90, 5.0), Some((0.0, 10.0)), "oldest lines at the top");
        assert_eq!(scrollbar_thumb((20.0, 120.0), 10, 90, 45, 5.0), Some((65.0, 10.0)));
        assert_eq!(scrollbar_thumb((0.0, 100.0), 10, 100_000, 0, 20.0).map(|(_, height)| height), Some(20.0));
    }

    #[test]
    fn animations_slow_down_without_focus() {
        let display = Duration::from_micros(16_667);
        assert_eq!(animation_interval(30, true, display), Duration::from_secs_f64(1.0 / 30.0));
        assert_eq!(animation_interval(0, true, display), display, "0 follows the display");
        assert_eq!(animation_interval(240, true, display), display, "never faster than the display");
        assert_eq!(animation_interval(30, false, display), Duration::from_millis(100));
        assert_eq!(animation_interval(0, false, display), display, "no cap: no slowdown either");
        assert_eq!(animation_interval(5, false, display), Duration::from_millis(200));
    }

    #[test]
    fn decorations_contrast_with_the_background() {
        assert_eq!(window_theme([0x0a, 0x0e, 0x14]), WindowTheme::Dark, "tron");
        assert_eq!(window_theme([0xf4, 0xf7, 0xfa]), WindowTheme::Light, "tron-light");
        assert_eq!(window_theme([0x1e, 0x1e, 0x2e]), WindowTheme::Dark, "catppuccin-mocha");
        assert_eq!(window_theme([0xfd, 0xf6, 0xe3]), WindowTheme::Light, "solarized-light");
    }

    #[test]
    fn pointer_names_map_to_icons() {
        assert_eq!(pointer_icon("pointer"), Some(CursorIcon::Pointer));
        assert_eq!(pointer_icon("hand2"), Some(CursorIcon::Pointer));
        assert_eq!(pointer_icon("default"), Some(CursorIcon::Default));
        assert_eq!(pointer_icon("not-allowed"), Some(CursorIcon::NotAllowed));
        assert_eq!(pointer_icon(""), None);
        assert_eq!(pointer_icon("sparkles"), None);
    }

    #[test]
    fn dropped_paths_are_shell_quoted() {
        assert_eq!(shell_quote("/tmp/a b.txt"), "'/tmp/a b.txt'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
    }
}
