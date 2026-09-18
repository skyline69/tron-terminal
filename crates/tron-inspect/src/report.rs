//! One reading of a window: everything the inspector shows, copied out of the
//! session while it holds its locks, so the inspector never touches the terminal.

use std::time::Duration;

use tron_core::Stats;
use tron_render::RenderStats;

/// Samples kept for the graphs, one per inspector frame.
pub const HISTORY: usize = 180;

#[derive(Clone, Default)]
pub struct Report {
    pub session: SessionInfo,
    pub grid: GridInfo,
    pub cursor: CursorInfo,
    /// Terminal modes, on and off, in the order they are shown.
    pub modes: Vec<Mode>,
    pub parser: Stats,
    pub io: Io,
    pub render: RenderStats,
    pub frames: Frames,
    /// Recent key presses, newest last.
    pub keys: Vec<KeyRecord>,
    pub input: InputState,
    /// Cursor rectangle the shaders are given: x, y, width, height in pixels.
    pub cursor_rect: [f32; 4],
    pub font: FontInfo,
    pub colors: Colors,
    pub shaders: Vec<ShaderInfo>,
}

#[derive(Clone, Default)]
pub struct SessionInfo {
    /// Title of the window the inspector follows.
    pub title: String,
    /// Title an application set, if it set one.
    pub program_title: Option<String>,
    pub shell: String,
    pub pid: Option<u32>,
    pub tty: Option<String>,
    pub working_directory: Option<String>,
    /// Coding agent harness found in the window, if any.
    pub harness: Option<String>,
    pub uptime: Duration,
    pub exited: bool,
}

#[derive(Clone, Default)]
pub struct GridInfo {
    pub cols: usize,
    pub rows: usize,
    /// Cell size in physical pixels.
    pub cell: (u32, u32),
    pub surface: (u32, u32),
    pub scale: f64,
    pub padding: (f32, f32),
    pub scrollback: usize,
    pub scrollback_limit: usize,
    /// Absolute line numbers of the top and the bottom viewport row.
    pub top_line: i64,
    pub bottom_line: i64,
    /// Lines of history above the bottom, fractional while smooth scrolling.
    pub offset: f32,
    pub alt_screen: bool,
    pub selection: Option<String>,
    /// Shell prompts and command output the terminal has marked.
    pub marks: usize,
    pub images: usize,
    pub mouse: Mouse,
}

#[derive(Clone, Default)]
pub struct Mouse {
    pub position: (f64, f64),
    pub cell: (usize, usize),
    pub reporting: &'static str,
    pub hovered_link: Option<String>,
}

#[derive(Clone, Default)]
pub struct CursorInfo {
    pub row: usize,
    pub col: usize,
    pub shape: &'static str,
    pub visible: bool,
    pub blinking: bool,
    /// Kitty keyboard protocol flags of the active screen.
    pub keyboard_flags: u8,
}

#[derive(Clone)]
pub struct Mode {
    pub name: &'static str,
    /// What the mode does, shown when the pointer rests on it.
    pub help: &'static str,
    pub on: bool,
}

/// Bytes between the shell and the window.
#[derive(Clone, Default)]
pub struct Io {
    pub read: u64,
    pub written: u64,
    /// Bytes a second, from the last samples.
    pub read_rate: f64,
    pub write_rate: f64,
    /// Bytes a second over each sample, oldest first.
    pub read_history: Vec<f32>,
}

#[derive(Clone, Default)]
pub struct Frames {
    /// Frames a second over the recent samples, none while the window is idle.
    pub fps: Option<f32>,
    pub last_ms: f32,
    /// Seconds since the terminal window last drew.
    pub since_last: f32,
    /// One frame at the window's refresh rate, in milliseconds.
    pub target_ms: f32,
    /// Milliseconds per frame, oldest first.
    pub history: Vec<f32>,
    pub animated: bool,
    pub animations_paused: bool,
    pub occluded: bool,
    pub focused: bool,
}

/// What the window is doing with the keyboard and the pointer.
#[derive(Clone, Default)]
pub struct InputState {
    /// Modifier keys held, such as `ctrl+shift`.
    pub modifiers: String,
    /// Text the input method has not committed yet.
    pub preedit: Option<String>,
    pub bindings: usize,
    pub search_open: bool,
    pub palette_open: bool,
}

#[derive(Clone, Default)]
pub struct KeyRecord {
    pub key: String,
    pub mods: String,
    /// What the window sent to the shell, escaped.
    pub bytes: String,
    /// The binding that ran instead of sending bytes.
    pub action: Option<String>,
}

#[derive(Clone, Default)]
pub struct FontInfo {
    pub family: String,
    pub size: f32,
    pub cell: (u32, u32),
    pub baseline: u32,
    pub ligatures: bool,
    pub bidi: bool,
}

#[derive(Clone, Default)]
pub struct Colors {
    pub theme: String,
    pub background: [u8; 3],
    pub foreground: [u8; 3],
    pub cursor: [u8; 3],
    pub cursor_text: [u8; 3],
    pub selection: [u8; 3],
    /// The 16 ANSI colors: eight normal, then eight bright.
    pub ansi: [[u8; 3]; 16],
    pub opacity: f32,
}

#[derive(Clone, Default)]
pub struct ShaderInfo {
    pub name: String,
    pub animated: bool,
}
