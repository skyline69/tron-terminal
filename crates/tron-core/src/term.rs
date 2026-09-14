//! Terminal state: applies parsed escape sequences to the screen.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use bitflags::bitflags;
use unicode_width::UnicodeWidthChar;

use base64::Engine;

use crate::cell::{Cell, Color, Extended, ExtendedTable, Flags, TextSize};
use crate::graphics::{self, Graphics, InlineImageArgs};
use crate::grid::{Grid, LineSize};
use crate::palette::{Palette, format_color_spec, parse_color_spec};
use crate::parser::{Groups, Params, Perform};
use crate::selection::{Point, Selection, SelectionRange};

/// How long synchronized output (mode 2026) may hold back rendering.
const SYNC_TIMEOUT: Duration = Duration::from_millis(150);
/// Most shell prompts remembered for jumping between them.
const MAX_COMMAND_MARKS: usize = 4096;
/// Longest notification title or body kept, in bytes.
const MAX_NOTIFICATION_TEXT: usize = 4096;
/// Largest iTerm2 multipart image, base64 encoded.
const MAX_INLINE_IMAGE: usize = 64 * 1024 * 1024;

bitflags! {
    /// Terminal modes set through SM/RM and DECSET/DECRST.
    #[derive(Copy, Clone, PartialEq, Eq, Debug)]
    pub struct Modes: u32 {
        const AUTOWRAP         = 1 << 0;
        const ORIGIN           = 1 << 1;
        const INSERT           = 1 << 2;
        const LINEFEED_NEWLINE = 1 << 3;
        const CURSOR_VISIBLE   = 1 << 4;
        const APP_CURSOR       = 1 << 5;
        const APP_KEYPAD       = 1 << 6;
        const BRACKETED_PASTE  = 1 << 7;
        const FOCUS_EVENTS     = 1 << 8;
        const MOUSE_X10        = 1 << 9;
        const MOUSE_NORMAL     = 1 << 10;
        const MOUSE_BUTTON     = 1 << 11;
        const MOUSE_ANY        = 1 << 12;
        const MOUSE_SGR        = 1 << 13;
        const ALT_SCREEN       = 1 << 14;
        const SYNC_OUTPUT      = 1 << 15;
        const REVERSE_VIDEO    = 1 << 16;
        const CURSOR_BLINK     = 1 << 17;
        const ALTERNATE_SCROLL = 1 << 18;
        /// Report dark/light color scheme changes (mode 2031).
        const COLOR_SCHEME_UPDATES = 1 << 19;
        /// SGR mouse reports carry pixel positions instead of cells (mode 1016).
        const MOUSE_SGR_PIXELS = 1 << 20;
        /// Width is measured per grapheme cluster instead of per code point (mode 2027).
        const GRAPHEME_CLUSTERS = 1 << 21;
        /// DECCOLM (mode 3) may switch between 80 and 132 columns (mode 40).
        const ALLOW_COLUMN_SWITCH = 1 << 22;

        const MOUSE_TRACKING = Self::MOUSE_X10.bits()
            | Self::MOUSE_NORMAL.bits()
            | Self::MOUSE_BUTTON.bits()
            | Self::MOUSE_ANY.bits();
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Beam,
}

/// Cursor as the renderer needs it.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct CursorState {
    pub row: usize,
    pub col: usize,
    pub visible: bool,
    pub shape: CursorShape,
    pub blinking: bool,
}

/// A hyperlink target set with OSC 8.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hyperlink {
    pub id: Option<String>,
    pub uri: String,
}

/// A link under the pointer: an OSC 8 hyperlink or a detected URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkMatch {
    pub uri: String,
    /// Inclusive cell range.
    pub start: Point,
    pub end: Point,
    /// Set for OSC 8 links: every cell with this id belongs to the link.
    pub id: Option<u16>,
}

enum DcsRequest {
    Decrqss(Vec<u8>),
    Xtgettcap(Vec<u8>),
    Sixel(Box<crate::sixel::SixelDecoder>),
}

/// Something the application embedding the terminal must act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TermEvent {
    Bell,
    /// OSC 52 write.
    ClipboardStore {
        primary: bool,
        text: String,
    },
    /// OSC 52 read. Reply with [`Terminal::clipboard_reply`].
    ClipboardLoad {
        primary: bool,
        terminator: &'static str,
    },
    /// A desktop notification (OSC 9, OSC 777 or OSC 99).
    Notification {
        title: String,
        body: String,
        /// When the application wants it shown. `None` leaves it to the user's settings.
        when: Option<NotifyWhen>,
    },
    /// DECCOLM switched the screen width. The window should resize to fit.
    ColumnsChanged(usize),
    /// The application asked for a mouse pointer shape (`OSC 22`), by CSS or X
    /// cursor name. Empty for the default.
    PointerShape(String),
    /// A command from tron's startup screen (`OSC 7777 ; token ; payload`).
    /// The application must check the token.
    StartupScreen {
        token: String,
        payload: String,
    },
}

/// When a notification should be shown (kitty's `o` key).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum NotifyWhen {
    Always,
    /// Only when the window does not have focus.
    Unfocused,
    /// Only when the window is not visible.
    Invisible,
}

/// A shell prompt and the output of the command run from it, from OSC 133 marks.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CommandMark {
    /// Absolute line where the prompt starts.
    pub prompt: i64,
    /// First line of output, once the command started.
    pub output_start: Option<i64>,
    /// Last line of output, once the command finished. Below `output_start`
    /// when the command printed nothing.
    pub output_end: Option<i64>,
}

/// A kitty notification (OSC 99) being received in chunks.
#[derive(Default)]
struct PendingNotification {
    id: String,
    title: String,
    body: String,
    when: Option<NotifyWhen>,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
enum Charset {
    #[default]
    Ascii,
    DecSpecial,
}

#[derive(Copy, Clone, Debug, Default)]
struct Cursor {
    row: usize,
    col: usize,
    /// Template for newly written cells: colors and attributes.
    pen: Cell,
    /// The last column was written. The next printable character wraps first.
    pending_wrap: bool,
    underline_color: Color,
    /// Current hyperlink id, 0 for none.
    link: u16,
    /// Extended attributes index for `link` with no other attributes.
    link_extended: u16,
}

#[derive(Copy, Clone, Debug)]
struct SavedCursor {
    cursor: Cursor,
    origin: bool,
    charsets: [Charset; 2],
    active_charset: usize,
}

const PRIMARY: usize = 0;
const ALTERNATE: usize = 1;

pub struct Terminal {
    grids: [Grid; 2],
    active: usize,
    cursor: Cursor,
    saved: [Option<SavedCursor>; 2],
    scroll_top: usize,
    scroll_bottom: usize,
    modes: Modes,
    tabs: Vec<bool>,
    charsets: [Charset; 2],
    active_charset: usize,
    default_cursor_shape: CursorShape,
    cursor_shape: CursorShape,
    title: String,
    title_dirty: bool,
    responses: Vec<u8>,
    events: Vec<TermEvent>,
    last_char: Option<char>,
    /// Cell that combining characters attach to.
    last_cluster: Option<(usize, usize)>,
    last_was_zwj: bool,
    sync_started: Option<Instant>,
    max_scrollback: usize,
    default_palette: Palette,
    palette: Palette,
    palette_generation: u64,
    graphics: Graphics,
    cell_pixels: (u32, u32),
    selection: Option<Selection>,
    word_separators: String,
    cwd: Option<String>,
    /// Absolute line where the shell's current prompt starts (OSC 133;A),
    /// cleared when a command runs (OSC 133;C or D).
    prompt_line: Option<i64>,
    /// Prompts and command output on the primary screen, oldest first.
    marks: VecDeque<CommandMark>,
    notification: Option<PendingNotification>,
    /// iTerm2 image arriving with `MultipartFile`, still base64 encoded.
    multipart_image: Option<(InlineImageArgs, Vec<u8>)>,
    /// Scaled text (OSC 66) was written, so writes must check for it.
    multicell: bool,
    /// Kitty keyboard protocol flag stacks for the primary and alternate screen.
    keyboard: [Vec<u8>; 2],
    dcs: Option<DcsRequest>,
    links: Vec<Hyperlink>,
    link_ids: std::collections::HashMap<(Option<String>, String), u16>,
    title_stack: Vec<String>,
    extended: ExtendedTable,
}

impl Terminal {
    pub fn new(cols: usize, rows: usize, max_scrollback: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Self {
            grids: [Grid::new(cols, rows, max_scrollback), Grid::new(cols, rows, 0)],
            active: PRIMARY,
            cursor: Cursor::default(),
            saved: [None, None],
            scroll_top: 0,
            scroll_bottom: rows - 1,
            modes: Modes::AUTOWRAP | Modes::CURSOR_VISIBLE | Modes::ALTERNATE_SCROLL | Modes::GRAPHEME_CLUSTERS,
            tabs: default_tabs(cols),
            charsets: [Charset::Ascii; 2],
            active_charset: 0,
            default_cursor_shape: CursorShape::Block,
            cursor_shape: CursorShape::Block,
            title: String::new(),
            title_dirty: false,
            responses: Vec::new(),
            events: Vec::new(),
            last_char: None,
            last_cluster: None,
            last_was_zwj: false,
            sync_started: None,
            max_scrollback,
            default_palette: Palette::default(),
            palette: Palette::default(),
            palette_generation: 0,
            graphics: Graphics::new(),
            cell_pixels: (8, 16),
            selection: None,
            word_separators: ",│`|:\"'()[]{}<>".to_owned(),
            cwd: None,
            prompt_line: None,
            marks: VecDeque::new(),
            notification: None,
            multipart_image: None,
            multicell: false,
            keyboard: [Vec::new(), Vec::new()],
            dcs: None,
            links: Vec::new(),
            link_ids: std::collections::HashMap::new(),
            title_stack: Vec::new(),
            extended: ExtendedTable::new(),
        }
    }

    #[inline]
    pub fn cols(&self) -> usize {
        self.grids[self.active].cols()
    }

    #[inline]
    pub fn rows(&self) -> usize {
        self.grids[self.active].rows()
    }

    /// The screen currently shown (primary or alternate).
    pub fn grid(&self) -> &Grid {
        &self.grids[self.active]
    }

    pub fn grid_mut(&mut self) -> &mut Grid {
        &mut self.grids[self.active]
    }

    pub fn modes(&self) -> Modes {
        self.modes
    }

    pub fn is_alt_screen(&self) -> bool {
        self.active == ALTERNATE
    }

    pub fn cursor(&self) -> CursorState {
        CursorState {
            row: self.cursor.row,
            col: self.cursor.col,
            visible: self.modes.contains(Modes::CURSOR_VISIBLE),
            shape: self.cursor_shape,
            blinking: self.modes.contains(Modes::CURSOR_BLINK),
        }
    }

    pub fn set_default_cursor_shape(&mut self, shape: CursorShape) {
        if self.cursor_shape == self.default_cursor_shape {
            self.cursor_shape = shape;
        }
        self.default_cursor_shape = shape;
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the title if it changed since the last call.
    pub fn take_title(&mut self) -> Option<String> {
        std::mem::take(&mut self.title_dirty).then(|| self.title.clone())
    }

    /// Bytes the terminal wants to send back to the application (DSR, DA, ...).
    pub fn take_responses(&mut self) -> Option<Vec<u8>> {
        (!self.responses.is_empty()).then(|| std::mem::take(&mut self.responses))
    }

    /// Copies what the renderer needs into `snapshot` and clears damage.
    /// Only rows damaged since the last snapshot are copied.
    pub fn snapshot(&mut self, snapshot: &mut crate::Snapshot) {
        let selection = self.selection_range();
        let grid = &self.grids[self.active];
        let rows = grid.rows();
        let resized = snapshot.rows.len() != rows || snapshot.cols != grid.cols();
        snapshot.rows.resize_with(rows, || crate::grid::Row::new(0));
        snapshot.damaged.resize(rows, false);
        for y in 0..rows {
            let damaged = resized || grid.is_damaged(y);
            snapshot.damaged[y] = damaged;
            if damaged {
                snapshot.rows[y].copy_from(grid.visible_row(y));
            }
        }
        snapshot.cols = grid.cols();
        snapshot.top_line = grid.viewport_line(0);
        snapshot.display_offset = grid.display_offset();
        snapshot.cursor = Some(self.cursor());
        snapshot.modes = Some(self.modes);
        snapshot.alt_screen = self.active == ALTERNATE;
        snapshot.palette = self.palette;
        snapshot.palette_generation = self.palette_generation;
        let known = snapshot.extended.len().min(self.extended.entries().len());
        snapshot.extended.truncate(known);
        snapshot.extended.extend_from_slice(&self.extended.entries()[known..]);
        snapshot.selection = selection;
        snapshot.next_frame_due = self.graphics.tick(Instant::now());
        if snapshot.graphics_generation != self.graphics.generation() {
            snapshot.graphics_generation = self.graphics.generation();
            snapshot.placements.clone_from(&self.graphics.placements().to_vec());
            snapshot.images = self.graphics.images().iter().map(|(id, image)| (*id, image.clone())).collect();
        }
        self.grids[self.active].clear_damage();
    }

    /// Active kitty keyboard protocol flags.
    pub fn keyboard_flags(&self) -> u8 {
        self.keyboard[self.active].last().copied().unwrap_or(0)
    }

    /// Extended attributes of a cell, from its `extended` index.
    #[inline]
    pub fn extended(&self, id: u16) -> &Extended {
        self.extended.get(id)
    }

    /// Target of hyperlink `id` from [`Extended::link`].
    pub fn hyperlink(&self, id: u16) -> Option<&Hyperlink> {
        id.checked_sub(1).and_then(|i| self.links.get(usize::from(i)))
    }

    /// The link at an absolute cell position: OSC 8 hyperlinks first, then URLs in the text.
    pub fn link_at(&self, point: Point) -> Option<LinkMatch> {
        let grid = self.grid();
        let logical = crate::text::LogicalLine::at(grid, point.line)?;
        let row = grid.line(point.line)?;
        let link = self.extended.get(row.cells.get(point.col)?.extended).link;
        if let Some(target) = self.hyperlink(link) {
            let same = |p: &Point| {
                grid.line(p.line)
                    .and_then(|r| r.cells.get(p.col))
                    .is_some_and(|c| self.extended.get(c.extended).link == link)
            };
            let index = logical.points.iter().position(|p| *p >= point)?;
            let mut first = index;
            while first > 0 && same(&logical.points[first - 1]) {
                first -= 1;
            }
            let mut last = index;
            while last + 1 < logical.points.len() && same(&logical.points[last + 1]) {
                last += 1;
            }
            return Some(LinkMatch {
                uri: target.uri.clone(),
                start: logical.points[first],
                end: logical.points[last],
                id: Some(link),
            });
        }
        let (first, last) = logical.url_at(point)?;
        Some(LinkMatch {
            uri: logical.text(first, last),
            start: logical.points[first],
            end: logical.points[last],
            id: None,
        })
    }

    /// Searches the screen and scrollback. See [`crate::text::search`].
    pub fn search(&self, query: &str, from: Option<Point>, backwards: bool) -> Option<crate::SearchMatch> {
        let grid = self.grid();
        let from = from.unwrap_or(Point::new(grid.last_line(), grid.cols() - 1));
        crate::text::search(grid, query, from, backwards)
    }

    /// Scrolls the viewport to show absolute line `line`.
    pub fn scroll_to_line(&mut self, line: i64) {
        self.grids[self.active].scroll_to_line(line);
    }

    fn graphics_context(&self) -> graphics::Context {
        let grid = &self.grids[self.active];
        graphics::Context {
            cursor_line: grid.screen_line(self.cursor.row),
            cursor_col: self.cursor.col,
            screen_top: grid.screen_line(0),
            rows: grid.rows(),
            cols: grid.cols(),
            cell_width: self.cell_pixels.0,
            cell_height: self.cell_pixels.1,
            alt_screen: self.active == ALTERNATE,
        }
    }

    /// Whether the background color counts as dark, for color scheme reports.
    fn is_dark(&self) -> bool {
        let [r, g, b] = self.palette.background.map(f32::from);
        0.2126 * r + 0.7152 * g + 0.0722 * b < 128.0
    }

    fn intern_link(&mut self, id: Option<String>, uri: String) -> u16 {
        let key = (id, uri);
        if let Some(&link) = self.link_ids.get(&key) {
            return link;
        }
        if self.links.len() >= usize::from(u16::MAX - 1) {
            return 0;
        }
        self.links.push(Hyperlink { id: key.0.clone(), uri: key.1.clone() });
        let link = self.links.len() as u16;
        self.link_ids.insert(key, link);
        link
    }

    /// Shell integration marks (OSC 133): A prompt start, C command start, D command end.
    fn shell_mark(&mut self, kind: Option<u8>) {
        if self.active != PRIMARY {
            return;
        }
        let line = self.grids[PRIMARY].screen_line(self.cursor.row);
        match kind {
            Some(b'A') => {
                self.prompt_line = Some(line);
                // A prompt redrawn in place, or after the screen was cleared, replaces later marks.
                while self.marks.back().is_some_and(|m| m.prompt >= line) {
                    self.marks.pop_back();
                }
                if self.marks.len() == MAX_COMMAND_MARKS {
                    self.marks.pop_front();
                }
                self.marks.push_back(CommandMark { prompt: line, output_start: None, output_end: None });
            }
            Some(b'C') => {
                self.prompt_line = None;
                if let Some(mark) = self.marks.back_mut()
                    && mark.output_start.is_none()
                {
                    mark.output_start = Some(line);
                }
            }
            Some(b'D') => {
                self.prompt_line = None;
                if let Some(mark) = self.marks.back_mut()
                    && let (Some(start), None) = (mark.output_start, mark.output_end)
                {
                    let end = if self.cursor.col == 0 { line - 1 } else { line };
                    mark.output_end = Some(end.max(start - 1));
                }
            }
            _ => {}
        }
    }

    /// Prompts and command output marked by the shell, oldest first.
    pub fn command_marks(&self) -> impl DoubleEndedIterator<Item = &CommandMark> {
        self.marks.iter()
    }

    /// Scrolls the viewport so the previous or next prompt is at the top.
    /// Past the last prompt, it scrolls to the bottom. Returns whether it scrolled.
    pub fn scroll_to_prompt(&mut self, previous: bool) -> bool {
        if self.active != PRIMARY {
            return false;
        }
        let grid = &mut self.grids[PRIMARY];
        let (top, oldest) = (grid.viewport_line(0), grid.oldest_line());
        let mut prompts = self.marks.iter().map(|m| m.prompt).filter(|&line| line >= oldest);
        let target = if previous { prompts.rfind(|&line| line < top) } else { prompts.find(|&line| line > top) };
        match target {
            Some(line) => grid.scroll_to_top(line),
            None if !previous => grid.reset_display_offset(),
            None => return false,
        }
        true
    }

    /// First and last cell of a command's output: the command whose output
    /// includes `line`, or with `None` the latest command that printed something.
    pub fn command_output(&self, line: Option<i64>) -> Option<(Point, Point)> {
        if self.active != PRIMARY {
            return None;
        }
        let grid = &self.grids[PRIMARY];
        let cursor_line = grid.screen_line(self.cursor.row);
        let running_end = if self.cursor.col == 0 { cursor_line - 1 } else { cursor_line };
        let range = |mark: &CommandMark| {
            let start = mark.output_start?.max(grid.oldest_line());
            let end = mark.output_end.unwrap_or(running_end);
            (end >= start).then_some((start, end))
        };
        let (start, end) = match line {
            Some(line) => self.marks.iter().rev().filter_map(range).find(|&(s, e)| (s..=e).contains(&line))?,
            None => self.marks.iter().rev().filter(|m| m.output_end.is_some()).find_map(range)?,
        };
        Some((Point::new(start, 0), Point::new(end, grid.cols() - 1)))
    }

    fn notify(&mut self, title: String, body: String, when: Option<NotifyWhen>) {
        let clean = |text: String| {
            let mut text: String = text.chars().filter(|c| !c.is_control()).collect();
            if text.len() > MAX_NOTIFICATION_TEXT {
                let end = (0..=MAX_NOTIFICATION_TEXT).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
                text.truncate(end);
            }
            text
        };
        let (title, body) = (clean(title), clean(body));
        if title.is_empty() && body.is_empty() {
            return;
        }
        // A notification needs a title; a lone body becomes the title.
        let (title, body) = if title.is_empty() { (body, String::new()) } else { (title, body) };
        self.events.push(TermEvent::Notification { title, body, when });
    }

    /// Kitty desktop notifications: <https://sw.kovidgoyal.net/kitty/desktop-notifications/>
    fn kitty_notification(&mut self, metadata: &[u8], payload: &[u8], terminator: &str) {
        let mut id = String::new();
        let mut done = true;
        let mut kind: &[u8] = b"title";
        let mut encoded = false;
        let mut when = None;
        for pair in metadata.split(|&b| b == b':') {
            let [key, b'=', value @ ..] = pair else { continue };
            match key {
                b'i' => {
                    id = value
                        .iter()
                        .filter(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'+' | b'.'))
                        .map(|&b| char::from(b))
                        .take(64)
                        .collect();
                }
                b'd' => done = value != b"0",
                b'p' => kind = value,
                b'e' => encoded = value == b"1",
                b'o' => {
                    when = match value {
                        b"always" => Some(NotifyWhen::Always),
                        b"unfocused" => Some(NotifyWhen::Unfocused),
                        b"invisible" => Some(NotifyWhen::Invisible),
                        _ => None,
                    }
                }
                _ => {}
            }
        }
        if kind == b"?" {
            let reply = format!("\x1b]99;i={id}:p=?;p=title,body:o=always,unfocused,invisible{terminator}");
            self.respond(reply.as_bytes());
            return;
        }
        let text = if encoded {
            match base64::engine::general_purpose::STANDARD.decode(payload) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(_) => return,
            }
        } else {
            String::from_utf8_lossy(payload).into_owned()
        };
        let pending = match &mut self.notification {
            Some(pending) if pending.id == id => pending,
            slot => slot.insert(PendingNotification { id, ..PendingNotification::default() }),
        };
        if when.is_some() {
            pending.when = when;
        }
        let target = match kind {
            b"title" => &mut pending.title,
            b"body" => &mut pending.body,
            _ => return,
        };
        if target.len() + text.len() <= MAX_NOTIFICATION_TEXT {
            target.push_str(&text);
        }
        if done && let Some(pending) = self.notification.take() {
            self.notify(pending.title, pending.body, pending.when);
        }
    }

    /// iTerm2 inline images: `File=args:data`, or `MultipartFile=args`, `FilePart=data`... `FileEnd`.
    fn iterm2(&mut self, command: &[u8]) {
        if let Some(rest) = command.strip_prefix(b"File=") {
            if let Some(colon) = memchr::memchr(b':', rest) {
                self.inline_image(&InlineImageArgs::parse(&rest[..colon]), &rest[colon + 1..]);
            }
        } else if let Some(args) = command.strip_prefix(b"MultipartFile=") {
            self.multipart_image = Some((InlineImageArgs::parse(args), Vec::new()));
        } else if let Some(part) = command.strip_prefix(b"FilePart=") {
            if let Some((_, data)) = &mut self.multipart_image {
                if data.len() + part.len() > MAX_INLINE_IMAGE {
                    self.multipart_image = None;
                } else {
                    data.extend_from_slice(part);
                }
            }
        } else if command == b"FileEnd"
            && let Some((args, data)) = self.multipart_image.take()
        {
            self.inline_image(&args, &data);
        }
    }

    fn inline_image(&mut self, args: &InlineImageArgs, encoded: &[u8]) {
        if !args.inline {
            return;
        }
        let Ok(data) = graphics::BASE64.decode(encoded) else {
            log::debug!("inline image: bad base64");
            return;
        };
        let ctx = self.graphics_context();
        match self.graphics.add_inline_image(&data, args, &ctx) {
            // Like kitty: the cursor ends on the image's last row, after its right edge.
            Ok((cols, rows)) => {
                for _ in 1..rows {
                    self.linefeed();
                }
                self.cursor.col = (self.cursor.col + cols as usize).min(self.cols() - 1);
                self.cursor.pending_wrap = false;
            }
            Err(error) => log::debug!("inline image: {error}"),
        }
    }

    fn decrqss(&mut self, request: &[u8]) {
        let reply = match request {
            b" q" => {
                let blinking = self.modes.contains(Modes::CURSOR_BLINK);
                let style = match (self.cursor_shape, blinking) {
                    (CursorShape::Block, true) => 1,
                    (CursorShape::Block, false) => 2,
                    (CursorShape::Underline, true) => 3,
                    (CursorShape::Underline, false) => 4,
                    (CursorShape::Beam, true) => 5,
                    (CursorShape::Beam, false) => 6,
                };
                Some(format!("{style} q"))
            }
            b"r" => Some(format!("{};{}r", self.scroll_top + 1, self.scroll_bottom + 1)),
            b"m" => {
                let pen = self.cursor.pen;
                let mut attrs = vec!["0".to_string()];
                for (flag, code) in [
                    (Flags::BOLD, "1"),
                    (Flags::DIM, "2"),
                    (Flags::ITALIC, "3"),
                    (Flags::UNDERLINE, "4"),
                    (Flags::BLINK, "5"),
                    (Flags::INVERSE, "7"),
                    (Flags::HIDDEN, "8"),
                    (Flags::STRIKETHROUGH, "9"),
                ] {
                    if pen.flags.contains(flag) {
                        attrs.push(code.into());
                    }
                }
                for (color, base) in [(pen.fg, 38), (pen.bg, 48)] {
                    match color.kind() {
                        crate::ColorKind::Indexed(i) => attrs.push(format!("{base}:5:{i}")),
                        crate::ColorKind::Rgb(r, g, b) => attrs.push(format!("{base}:2::{r}:{g}:{b}")),
                        crate::ColorKind::Default => {}
                    }
                }
                Some(format!("{}m", attrs.join(";")))
            }
            _ => None,
        };
        match reply {
            Some(reply) => self.respond(format!("\x1bP1$r{reply}\x1b\\").as_bytes()),
            None => self.respond(b"\x1bP0$r\x1b\\"),
        }
    }

    fn xtgettcap(&mut self, request: &[u8]) {
        for hex in request.split(|&b| b == b';') {
            let name = decode_hex(hex).and_then(|bytes| String::from_utf8(bytes).ok());
            let hex = String::from_utf8_lossy(hex).into_owned();
            match name.as_deref().and_then(capability) {
                Some("") => self.respond(format!("\x1bP1+r{hex}\x1b\\").as_bytes()),
                Some(value) => {
                    let encoded: String = value.bytes().map(|b| format!("{b:02X}")).collect();
                    self.respond(format!("\x1bP1+r{hex}={encoded}\x1b\\").as_bytes());
                }
                None => self.respond(format!("\x1bP0+r{hex}\x1b\\").as_bytes()),
            }
        }
    }

    /// Events for the embedding application, oldest first.
    pub fn take_events(&mut self) -> Vec<TermEvent> {
        std::mem::take(&mut self.events)
    }

    /// Answers an OSC 52 clipboard read.
    pub fn clipboard_reply(&mut self, primary: bool, text: &str, terminator: &str) {
        let target = if primary { 'p' } else { 'c' };
        let encoded = base64::engine::general_purpose::STANDARD.encode(text);
        self.respond(format!("\x1b]52;{target};{encoded}{terminator}").as_bytes());
    }

    pub fn set_scrollback_limit(&mut self, lines: usize) {
        self.max_scrollback = lines;
        self.grids[PRIMARY].set_max_scrollback(lines);
    }

    /// Current colors, including changes made by applications.
    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// Changes whenever [`Terminal::palette`] changes.
    pub fn palette_generation(&self) -> u64 {
        self.palette_generation
    }

    /// Sets the configured colors. Application overrides are discarded.
    pub fn set_default_palette(&mut self, palette: Palette) {
        let was_dark = self.is_dark();
        self.default_palette = palette;
        self.palette = palette;
        self.palette_generation += 1;
        if self.modes.contains(Modes::COLOR_SCHEME_UPDATES) && self.is_dark() != was_dark {
            let scheme = if self.is_dark() { 1 } else { 2 };
            self.respond(format!("\x1b[?997;{scheme}n").as_bytes());
        }
    }

    pub fn graphics(&self) -> &Graphics {
        &self.graphics
    }

    pub fn graphics_mut(&mut self) -> &mut Graphics {
        &mut self.graphics
    }

    /// Cell size in pixels, used to size images.
    pub fn set_cell_pixels(&mut self, width: u32, height: u32) {
        self.cell_pixels = (width.max(1), height.max(1));
    }

    /// Working directory reported by the shell (OSC 7).
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    pub fn set_word_separators(&mut self, separators: &str) {
        self.word_separators = separators.to_owned();
    }

    /// Absolute position of a viewport cell.
    pub fn viewport_point(&self, row: usize, col: usize) -> Point {
        let grid = self.grid();
        Point::new(grid.viewport_line(row.min(grid.rows() - 1)), col.min(grid.cols() - 1))
    }

    pub fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    pub fn set_selection(&mut self, selection: Option<Selection>) {
        self.selection = selection;
    }

    pub fn update_selection(&mut self, point: Point) {
        if let Some(selection) = &mut self.selection {
            selection.update(point);
        }
    }

    pub fn selection_range(&self) -> Option<SelectionRange> {
        self.selection.as_ref()?.range(self.grid(), &self.word_separators)
    }

    pub fn selection_text(&self) -> Option<String> {
        let text = self.selection_range()?.text(self.grid());
        (!text.is_empty()).then_some(text)
    }

    /// True while synchronized output holds back rendering. Expires on its own.
    pub fn sync_blocked(&mut self) -> bool {
        match self.sync_started {
            Some(start) if start.elapsed() < SYNC_TIMEOUT => true,
            Some(_) => {
                self.sync_started = None;
                self.modes.remove(Modes::SYNC_OUTPUT);
                false
            }
            None => false,
        }
    }

    /// Scrolls the viewport through history. Positive `delta` goes back in time.
    pub fn scroll_display(&mut self, delta: isize) {
        self.grids[self.active].scroll_display(delta);
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols() && rows == self.rows() {
            return;
        }
        // Shells that mark their prompt redraw it after SIGWINCH from where they
        // expect it to start, clearing to the end of the screen first. Keep the
        // cursor there relative to the prompt start. The reflowed prompt stays
        // visible until the shell's redraw replaces it, so it does not flicker.
        // Only a width change rewraps the prompt, and shells such as fish only
        // redraw on width changes. A height-only resize keeps the prompt as is.
        let prompt = if cols != self.cols() { self.prompt_redraw_start() } else { None };
        for index in [PRIMARY, ALTERNATE] {
            let reflow = index == PRIMARY;
            if index == self.active {
                if let (Some((prompt_row, below)), PRIMARY) = (prompt, index) {
                    let (mut row, _) = self.grids[index].resize(cols, rows, (prompt_row, 0), reflow);
                    // The cursor stays below the prompt start, as far as the new height allows.
                    let below = below.min(rows - 1);
                    let excess = (row + below).saturating_sub(rows - 1);
                    if excess > 0 {
                        self.grids[index].scroll_up(0, rows - 1, excess, Cell::BLANK, true);
                        row -= excess;
                    }
                    self.cursor.row = row + below;
                    self.cursor.col = 0;
                    self.prompt_line = Some(self.grids[index].screen_line(row));
                    continue;
                }
                let position = (self.cursor.row, self.cursor.col);
                (self.cursor.row, self.cursor.col) = self.grids[index].resize(cols, rows, position, reflow);
            } else {
                let position = self.saved[index].map_or((0, 0), |s| (s.cursor.row, s.cursor.col));
                let (row, col) = self.grids[index].resize(cols, rows, position, reflow);
                if let Some(saved) = &mut self.saved[index] {
                    (saved.cursor.row, saved.cursor.col) = (row, col);
                }
            }
        }
        // Reflow renumbers lines: keep images, marks and the selection with their text.
        let line_map = self.grids[PRIMARY].take_line_map();
        if let Some(map) = &line_map {
            self.graphics.remap_lines(|line| map.map(line));
            if prompt.is_none() {
                self.prompt_line = self.prompt_line.and_then(|line| map.map(line));
            }
            self.marks.retain_mut(|mark| {
                let Some(prompt) = map.map(mark.prompt) else { return false };
                mark.prompt = prompt;
                mark.output_start = mark.output_start.and_then(|line| map.map(line));
                mark.output_end = mark.output_end.and_then(|line| map.map(line));
                true
            });
        }
        if prompt.is_some()
            && let (Some(line), Some(mark)) = (self.prompt_line, self.marks.back_mut())
            && mark.output_start.is_none()
        {
            mark.prompt = line;
        }
        for saved in self.saved.iter_mut().flatten() {
            saved.cursor.col = saved.cursor.col.min(cols - 1);
            saved.cursor.row = saved.cursor.row.min(rows - 1);
        }
        self.selection = match self.selection.take() {
            Some(mut selection) if self.active == PRIMARY => {
                let map_point = |point: Point| match &line_map {
                    Some(map) => map.map_point(point.line, point.col).map(|(line, col)| Point::new(line, col)),
                    None => Some(Point::new(point.line, point.col.min(cols - 1))),
                };
                match (map_point(selection.anchor), map_point(selection.head)) {
                    (Some(anchor), Some(head)) => {
                        selection.anchor = anchor;
                        selection.head = head;
                        Some(selection)
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        self.cursor.pending_wrap = false;
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.tabs = default_tabs(cols);
    }

    /// Screen row of the prompt start and the cursor's distance below it, when
    /// the shell is showing a marked prompt that is fully on screen.
    fn prompt_redraw_start(&self) -> Option<(usize, usize)> {
        if self.active != PRIMARY {
            return None;
        }
        let line = self.prompt_line?;
        let grid = &self.grids[PRIMARY];
        let top = grid.screen_line(0);
        let cursor_line = grid.screen_line(self.cursor.row);
        if line < top || line > cursor_line {
            return None;
        }
        let row = (line - top) as usize;
        Some((row, self.cursor.row - row))
    }

    // ----- cursor movement ------------------------------------------------

    fn goto(&mut self, row: usize, col: usize) {
        let (min, max) = if self.modes.contains(Modes::ORIGIN) {
            (self.scroll_top, self.scroll_bottom)
        } else {
            (0, self.rows() - 1)
        };
        self.cursor.row = (min + row).min(max);
        self.cursor.col = col.min(self.line_cols(self.cursor.row) - 1);
        self.cursor.pending_wrap = false;
    }

    /// Columns usable on screen row `row`: half the screen on double size lines.
    #[inline]
    fn line_cols(&self, row: usize) -> usize {
        let grid = &self.grids[self.active];
        match grid.row(row).line_size {
            LineSize::Single => grid.cols(),
            _ => (grid.cols() / 2).max(1),
        }
    }

    fn clamp_cursor_to_line(&mut self) {
        let cols = self.line_cols(self.cursor.row);
        if self.cursor.col >= cols {
            self.cursor.col = cols - 1;
        }
    }

    fn move_up(&mut self, n: usize) {
        let top = if self.cursor.row >= self.scroll_top { self.scroll_top } else { 0 };
        self.cursor.row = self.cursor.row.saturating_sub(n).max(top);
        self.cursor.pending_wrap = false;
    }

    fn move_down(&mut self, n: usize) {
        let bottom = if self.cursor.row <= self.scroll_bottom { self.scroll_bottom } else { self.rows() - 1 };
        self.cursor.row = (self.cursor.row + n).min(bottom);
        self.cursor.pending_wrap = false;
    }

    fn move_right(&mut self, n: usize) {
        self.cursor.col = (self.cursor.col + n).min(self.line_cols(self.cursor.row) - 1);
        self.cursor.pending_wrap = false;
    }

    fn move_left(&mut self, n: usize) {
        self.cursor.col = self.cursor.col.saturating_sub(n);
        self.cursor.pending_wrap = false;
    }

    fn tab_forward(&mut self, n: usize) {
        let cols = self.line_cols(self.cursor.row);
        for _ in 0..n {
            let next = (self.cursor.col + 1..cols).find(|&c| self.tabs[c]);
            self.cursor.col = next.unwrap_or(cols - 1);
        }
        self.cursor.pending_wrap = false;
    }

    fn tab_back(&mut self, n: usize) {
        for _ in 0..n {
            let prev = (0..self.cursor.col).rev().find(|&c| self.tabs[c]);
            self.cursor.col = prev.unwrap_or(0);
        }
        self.cursor.pending_wrap = false;
    }

    fn linefeed(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.cursor.row + 1 < self.rows() {
            self.cursor.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.scroll_top {
            self.scroll_down(1);
        } else if self.cursor.row > 0 {
            self.cursor.row -= 1;
        }
    }

    fn wrap(&mut self) {
        let row = self.cursor.row;
        self.grids[self.active].row_mut(row).wrapped = true;
        self.cursor.col = 0;
        self.linefeed();
    }

    fn blank(&self) -> Cell {
        Cell::erased(&self.cursor.pen)
    }

    fn scroll_up(&mut self, n: usize) {
        let blank = self.blank();
        let save = self.active == PRIMARY;
        let grid = &mut self.grids[self.active];
        grid.scroll_up(self.scroll_top, self.scroll_bottom, n, blank, save);
        let oldest = grid.oldest_line();
        self.graphics.prune(oldest, self.active == ALTERNATE);
        if save && !self.marks.is_empty() {
            while self.marks.front().is_some_and(|m| m.prompt.max(m.output_end.unwrap_or(m.prompt)) < oldest) {
                self.marks.pop_front();
            }
        }
    }

    fn scroll_down(&mut self, n: usize) {
        let blank = self.blank();
        self.grids[self.active].scroll_down(self.scroll_top, self.scroll_bottom, n, blank);
    }

    // ----- writing ------------------------------------------------------------

    /// Clears the other half of wide characters cut by writing `width` cells at `col`.
    fn clear_wide_overlap(&mut self, row: usize, col: usize, width: usize) {
        repair_wide(self.grids[self.active].row_mut(row), col, width);
    }

    fn translate(&self, c: char) -> char {
        if self.charsets[self.active_charset] != Charset::DecSpecial {
            return c;
        }
        match c {
            '`' => '◆',
            'a' => '▒',
            'b' => '␉',
            'c' => '␌',
            'd' => '␍',
            'e' => '␊',
            'f' => '°',
            'g' => '±',
            'h' => '␤',
            'i' => '␋',
            'j' => '┘',
            'k' => '┐',
            'l' => '┌',
            'm' => '└',
            'n' => '┼',
            'o' => '⎺',
            'p' => '⎻',
            'q' => '─',
            'r' => '⎼',
            's' => '⎽',
            't' => '├',
            'u' => '┤',
            'v' => '┴',
            'w' => '┬',
            'x' => '│',
            'y' => '≤',
            'z' => '≥',
            '{' => 'π',
            '|' => '≠',
            '}' => '£',
            '~' => '·',
            _ => c,
        }
    }

    /// Whether a printable character continues the previous cluster: after a
    /// zero width joiner, or as the second regional indicator of a flag.
    fn joins_cluster(&self, c: char) -> bool {
        self.modes.contains(Modes::GRAPHEME_CLUSTERS)
            && self.last_cluster.is_some()
            && ((self.last_was_zwj && !c.is_ascii()) || (is_regional_indicator(c) && self.pending_regional_indicator()))
    }

    /// Whether the last written cell holds a lone regional indicator (first half of a flag).
    fn pending_regional_indicator(&self) -> bool {
        let Some((row, col)) = self.last_cluster else { return false };
        let grid = &self.grids[self.active];
        if row >= grid.rows() || col >= grid.cols() {
            return false;
        }
        let cell = grid.row(row).cells[col];
        is_regional_indicator(cell.ch) && !cell.flags.intersects(Flags::GRAPHEME | Flags::WIDE)
    }

    /// Adds a zero width or joined character to the previous cell.
    fn append_to_cluster(&mut self, c: char) {
        let Some((row, col)) = self.last_cluster else {
            self.last_was_zwj = false;
            return;
        };
        let cols = self.cols();
        if row >= self.rows() || col >= cols {
            return;
        }
        let line = self.grids[self.active].row_mut(row);
        let widen = (c == '\u{FE0F}' || is_regional_indicator(c))
            && self.modes.contains(Modes::GRAPHEME_CLUSTERS)
            && !line.cells[col].flags.contains(Flags::WIDE)
            && col + 1 < cols;
        line.push_combining(col, c);
        if widen {
            if col + 2 < cols && line.cells[col + 1].flags.contains(Flags::WIDE) {
                line.cells[col + 2].flags.remove(Flags::CONTENT_MASK);
                line.cells[col + 2].ch = '\0';
            }
            let base = line.cells[col];
            line.cells[col].flags.insert(Flags::WIDE);
            line.touch(col + 1, col + 2);
            line.cells[col + 1] =
                Cell { ch: '\0', flags: (base.flags - Flags::CONTENT_MASK) | Flags::WIDE_SPACER, ..base };
            if self.cursor.row == row && self.cursor.col == col + 1 && !self.cursor.pending_wrap {
                if col + 2 >= cols {
                    self.cursor.col = cols - 1;
                    self.cursor.pending_wrap = true;
                } else {
                    self.cursor.col = col + 2;
                }
            }
        }
        self.last_was_zwj = c == '\u{200D}';
    }

    fn write_char(&mut self, c: char, width: usize) {
        if width > self.cols() {
            return;
        }
        let autowrap = self.modes.contains(Modes::AUTOWRAP);
        if self.cursor.pending_wrap && autowrap {
            self.wrap();
        }
        let cols = self.line_cols(self.cursor.row);
        if width > cols {
            return;
        }
        self.clamp_cursor_to_line();
        if width == 2 && self.cursor.col + 1 >= cols {
            if autowrap {
                let (row, col) = (self.cursor.row, self.cursor.col);
                let blank = self.blank();
                let grid = &mut self.grids[self.active];
                grid.erase(row, col..cols, blank);
                grid.row_mut(row).cells[cols - 1].flags.insert(Flags::WIDE_SPACER);
                self.wrap();
            } else {
                self.cursor.col = cols - 2;
            }
        }

        let (row, col) = (self.cursor.row, self.cursor.col);
        if self.modes.contains(Modes::INSERT) {
            self.insert_blanks(width);
        }
        if self.multicell {
            self.clear_multicells(row, col, width);
        }

        let pen = self.cursor.pen;
        let line = self.grids[self.active].row_mut(row);
        repair_wide(line, col, width);
        line.touch(col, col + width);
        if width == 2 {
            line.cells[col] = Cell { ch: c, flags: pen.flags | Flags::WIDE, ..pen };
            line.cells[col + 1] = Cell { ch: '\0', flags: pen.flags | Flags::WIDE_SPACER, ..pen };
        } else {
            line.cells[col] = Cell { ch: c, ..pen };
        }
        self.last_cluster = Some((row, col));
        self.last_was_zwj = false;

        if col + width >= cols {
            self.cursor.col = cols - 1;
            self.cursor.pending_wrap = true;
        } else {
            self.cursor.col = col + width;
            self.cursor.pending_wrap = false;
        }
    }

    /// Clears scaled text blocks (OSC 66) that overlap `width` cells at `row`, `col`.
    fn clear_multicells(&mut self, row: usize, col: usize, width: usize) {
        let grid = &self.grids[self.active];
        let (rows, cols) = (grid.rows(), grid.cols());
        let blocks: Vec<(usize, usize, TextSize)> = (col..(col + width).min(cols))
            .filter_map(|x| {
                let size = self.extended.get(grid.row(row).cells[x].extended).size?;
                Some((row.checked_sub(usize::from(size.dy))?, x.checked_sub(usize::from(size.dx))?, size))
            })
            .collect();
        for (top, left, size) in blocks {
            let (block_cols, block_rows) = size.cells();
            for y in top..(top + block_rows).min(rows) {
                let line = self.grids[self.active].row_mut(y);
                for x in left..(left + block_cols).min(cols) {
                    let cell = &mut line.cells[x];
                    let extended = *self.extended.get(cell.extended);
                    if extended.size.is_some() {
                        cell.extended = self.extended.intern(Extended { size: None, ..extended });
                        cell.ch = '\0';
                        cell.flags.remove(Flags::CONTENT_MASK);
                    }
                }
                line.touch(left, left + 1);
            }
        }
    }

    /// Kitty text sizing: <https://sw.kovidgoyal.net/kitty/text-sizing-protocol/>
    fn text_sizing(&mut self, metadata: &[u8], text: &[u8]) {
        let mut size =
            TextSize { scale: 1, width: 0, numerator: 0, denominator: 0, vertical: 0, horizontal: 0, dx: 0, dy: 0 };
        for pair in metadata.split(|&b| b == b':') {
            let [key, b'=', value @ ..] = pair else { continue };
            let Some(number) = parse_number(value) else { continue };
            match key {
                b's' => size.scale = number.clamp(1, 7) as u8,
                b'w' => size.width = number.min(7) as u8,
                b'n' => size.numerator = number.min(15) as u8,
                b'd' => size.denominator = number.min(15) as u8,
                b'v' => size.vertical = number.min(2) as u8,
                b'h' => size.horizontal = number.min(2) as u8,
                _ => {}
            }
        }
        let text = String::from_utf8_lossy(&text[..text.len().min(4096)]);
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        let fractional = size.numerator > 0 && size.denominator > size.numerator;
        if size.scale == 1 && size.width == 0 && !fractional {
            for c in text.chars() {
                self.print(c);
            }
            return;
        }
        if size.width > 0 {
            self.write_multicell(&text, size);
            return;
        }
        // Without a width, every grapheme gets its own block, as wide as the grapheme.
        let mut clusters: Vec<(String, u8)> = Vec::new();
        let mut after_joiner = false;
        for c in text.chars() {
            let width = char_width(c).unwrap_or(0) as u8;
            let flag_pair = is_regional_indicator(c)
                && clusters.last().is_some_and(|(s, _)| s.chars().count() == 1 && s.starts_with(is_regional_indicator));
            match clusters.last_mut() {
                Some((cluster, cluster_width)) if after_joiner || width == 0 || flag_pair => {
                    cluster.push(c);
                    if c == '\u{FE0F}' || flag_pair {
                        *cluster_width = 2;
                    }
                }
                _ => clusters.push((c.to_string(), width.max(1))),
            }
            after_joiner = c == '\u{200D}';
        }
        for (cluster, width) in clusters {
            self.write_multicell(&cluster, TextSize { width, ..size });
        }
    }

    /// Writes `text` into one block of `size.scale * size.width` columns and `size.scale` rows.
    fn write_multicell(&mut self, text: &str, size: TextSize) {
        let mut chars = text.chars();
        let Some(first) = chars.next() else { return };
        let (block_cols, block_rows) = size.cells();
        let (rows, cols) = (self.rows(), self.cols());
        let (top, bottom) = if self.in_scroll_region() { (self.scroll_top, self.scroll_bottom) } else { (0, rows - 1) };
        if block_cols > cols || block_rows > bottom + 1 - top {
            return;
        }
        let autowrap = self.modes.contains(Modes::AUTOWRAP);
        if self.cursor.pending_wrap && autowrap {
            self.wrap();
        }
        if self.cursor.col + block_cols > cols {
            if autowrap {
                self.wrap();
            } else {
                self.cursor.col = cols - block_cols;
            }
        }
        let overflow = (self.cursor.row + block_rows).saturating_sub(bottom + 1);
        if overflow > 0 {
            self.scroll_up(overflow);
            self.cursor.row -= overflow;
        }
        let (row, col) = (self.cursor.row, self.cursor.col);
        self.multicell = true;
        let pen = self.cursor.pen;
        let attributes = *self.extended.get(pen.extended);
        for dy in 0..block_rows {
            self.clear_multicells(row + dy, col, block_cols);
            for dx in 0..block_cols {
                let part = TextSize { dx: dx as u8, dy: dy as u8, ..size };
                let extended = self.extended.intern(Extended { size: Some(part), ..attributes });
                let line = self.grids[self.active].row_mut(row + dy);
                repair_wide(line, col + dx, 1);
                line.touch(col + dx, col + dx + 1);
                line.cells[col + dx] = if (dx, dy) == (0, 0) {
                    Cell { ch: first, extended, ..pen }
                } else {
                    Cell { ch: '\0', flags: pen.flags | Flags::WIDE_SPACER, extended, ..pen }
                };
            }
        }
        let line = self.grids[self.active].row_mut(row);
        for c in chars {
            line.push_combining(col, c);
        }
        self.last_cluster = None;
        self.last_char = None;
        if col + block_cols >= cols {
            self.cursor.col = cols - 1;
            self.cursor.pending_wrap = true;
        } else {
            self.cursor.col = col + block_cols;
            self.cursor.pending_wrap = false;
        }
    }

    fn insert_blanks(&mut self, n: usize) {
        let (row, col) = (self.cursor.row, self.cursor.col);
        let blank = self.blank();
        self.clear_wide_overlap(row, col, 1);
        self.grids[self.active].row_mut(row).insert_cells(col, n, blank);
        self.cursor.pending_wrap = false;
    }

    fn delete_chars(&mut self, n: usize) {
        let (row, col) = (self.cursor.row, self.cursor.col);
        let blank = self.blank();
        let width = self.cols();
        self.clear_wide_overlap(row, col, (col + n).min(width) - col);
        self.grids[self.active].row_mut(row).delete_cells(col, n, blank);
        self.cursor.pending_wrap = false;
    }

    fn erase_display(&mut self, mode: u16) {
        let blank = self.blank();
        let (rows, cols) = (self.rows(), self.cols());
        let (row, col) = (self.cursor.row, self.cursor.col);
        let grid = &mut self.grids[self.active];
        match mode {
            0 => {
                grid.erase(row, col..cols, blank);
                for r in row + 1..rows {
                    grid.erase_row(r, blank);
                }
            }
            1 => {
                for r in 0..row {
                    grid.erase_row(r, blank);
                }
                grid.erase(row, 0..col + 1, blank);
            }
            2 => {
                for r in 0..rows {
                    grid.erase_row(r, blank);
                }
                let top = grid.screen_line(0);
                self.graphics.clear_screen(top, rows, self.active == ALTERNATE);
            }
            3 => {
                grid.clear_scrollback();
                let oldest = grid.oldest_line();
                self.graphics.prune(oldest, self.active == ALTERNATE);
                self.selection = None;
            }
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: u16) {
        let blank = self.blank();
        let cols = self.cols();
        let (row, col) = (self.cursor.row, self.cursor.col);
        let range = match mode {
            0 => col..cols,
            1 => 0..col + 1,
            2 => 0..cols,
            _ => return,
        };
        self.grids[self.active].erase(row, range, blank);
    }

    fn in_scroll_region(&self) -> bool {
        (self.scroll_top..=self.scroll_bottom).contains(&self.cursor.row)
    }

    fn insert_lines(&mut self, n: usize) {
        if !self.in_scroll_region() {
            return;
        }
        let blank = self.blank();
        let (row, bottom) = (self.cursor.row, self.scroll_bottom);
        self.grids[self.active].scroll_down(row, bottom, n, blank);
        self.cursor.col = 0;
        self.cursor.pending_wrap = false;
    }

    fn delete_lines(&mut self, n: usize) {
        if !self.in_scroll_region() {
            return;
        }
        let blank = self.blank();
        let (row, bottom) = (self.cursor.row, self.scroll_bottom);
        self.grids[self.active].scroll_up(row, bottom, n, blank, false);
        self.cursor.col = 0;
        self.cursor.pending_wrap = false;
    }

    // ----- state save / restore -----------------------------------------------

    fn save_cursor(&mut self) {
        self.saved[self.active] = Some(SavedCursor {
            cursor: self.cursor,
            origin: self.modes.contains(Modes::ORIGIN),
            charsets: self.charsets,
            active_charset: self.active_charset,
        });
    }

    fn restore_cursor(&mut self) {
        match self.saved[self.active] {
            Some(saved) => {
                self.cursor = saved.cursor;
                self.cursor.row = self.cursor.row.min(self.rows() - 1);
                self.cursor.col = self.cursor.col.min(self.cols() - 1);
                self.modes.set(Modes::ORIGIN, saved.origin);
                self.charsets = saved.charsets;
                self.active_charset = saved.active_charset;
            }
            None => {
                self.cursor = Cursor::default();
                self.modes.remove(Modes::ORIGIN);
            }
        }
    }

    fn switch_screen(&mut self, alternate: bool, clear: bool) {
        let target = if alternate { ALTERNATE } else { PRIMARY };
        if target != self.active {
            self.active = target;
            self.modes.set(Modes::ALT_SCREEN, alternate);
            self.grids[target].damage_all();
            self.selection = None;
            if !alternate {
                self.graphics.clear_alt_screen();
                self.keyboard[ALTERNATE].clear();
            }
        }
        if clear && alternate {
            let (rows, cols) = (self.rows(), self.cols());
            let blank = self.blank();
            for r in 0..rows {
                self.grids[ALTERNATE].erase(r, 0..cols, blank);
            }
        }
    }

    fn reset(&mut self) {
        let title = std::mem::take(&mut self.title);
        let shape = self.default_cursor_shape;
        let palette = self.default_palette;
        let generation = self.palette_generation + 1;
        let cell_pixels = self.cell_pixels;
        let separators = std::mem::take(&mut self.word_separators);
        // Snapshots keep a copy of the table, so indexes must stay valid.
        let extended = std::mem::take(&mut self.extended);
        *self = Self::new(self.cols(), self.rows(), self.max_scrollback);
        self.extended = extended;
        self.default_cursor_shape = shape;
        self.cursor_shape = shape;
        self.title = title;
        self.default_palette = palette;
        self.palette = palette;
        self.palette_generation = generation;
        self.cell_pixels = cell_pixels;
        self.word_separators = separators;
    }

    // ----- modes ----------------------------------------------------------------

    fn set_ansi_mode(&mut self, mode: u16, on: bool) {
        match mode {
            4 => self.modes.set(Modes::INSERT, on),
            20 => self.modes.set(Modes::LINEFEED_NEWLINE, on),
            _ => log::debug!("unhandled ANSI mode {mode}"),
        }
    }

    fn set_dec_mode(&mut self, mode: u16, on: bool) {
        match mode {
            1 => self.modes.set(Modes::APP_CURSOR, on),
            // DECCOLM: with mode 40 the screen switches to 132 or 80 columns and
            // the application is asked to resize the window. Either way, like
            // xterm, the screen is cleared, margins reset and the cursor homed.
            3 => {
                let cols = if on { 132 } else { 80 };
                if self.modes.contains(Modes::ALLOW_COLUMN_SWITCH) && cols != self.cols() {
                    let rows = self.rows();
                    self.resize(cols, rows);
                    self.events.push(TermEvent::ColumnsChanged(cols));
                }
                self.erase_display(2);
                self.scroll_top = 0;
                self.scroll_bottom = self.rows() - 1;
                self.goto(0, 0);
            }
            5 => {
                self.modes.set(Modes::REVERSE_VIDEO, on);
                self.grids[self.active].damage_all();
            }
            6 => {
                self.modes.set(Modes::ORIGIN, on);
                self.goto(0, 0);
            }
            7 => self.modes.set(Modes::AUTOWRAP, on),
            12 => self.modes.set(Modes::CURSOR_BLINK, on),
            25 => self.modes.set(Modes::CURSOR_VISIBLE, on),
            9 | 1000 | 1002 | 1003 => {
                let flag = match mode {
                    9 => Modes::MOUSE_X10,
                    1000 => Modes::MOUSE_NORMAL,
                    1002 => Modes::MOUSE_BUTTON,
                    _ => Modes::MOUSE_ANY,
                };
                self.modes.remove(Modes::MOUSE_TRACKING);
                self.modes.set(flag, on);
            }
            1004 => self.modes.set(Modes::FOCUS_EVENTS, on),
            1006 => self.modes.set(Modes::MOUSE_SGR, on),
            1016 => self.modes.set(Modes::MOUSE_SGR_PIXELS, on),
            2027 => self.modes.set(Modes::GRAPHEME_CLUSTERS, on),
            40 => self.modes.set(Modes::ALLOW_COLUMN_SWITCH, on),
            1007 => self.modes.set(Modes::ALTERNATE_SCROLL, on),
            47 | 1047 => self.switch_screen(on, mode == 1047 && on),
            1048 => {
                if on {
                    self.save_cursor();
                } else {
                    self.restore_cursor();
                }
            }
            1049 => {
                if on {
                    self.save_cursor();
                    self.switch_screen(true, true);
                } else {
                    self.switch_screen(false, false);
                    self.restore_cursor();
                }
            }
            2004 => self.modes.set(Modes::BRACKETED_PASTE, on),
            2031 => self.modes.set(Modes::COLOR_SCHEME_UPDATES, on),
            2026 => {
                self.modes.set(Modes::SYNC_OUTPUT, on);
                self.sync_started = on.then(Instant::now);
            }
            _ => log::debug!("unhandled DEC mode {mode}"),
        }
    }

    fn dec_mode_state(&self, mode: u16) -> Option<bool> {
        let flag = match mode {
            3 => return Some(self.cols() == 132),
            1 => Modes::APP_CURSOR,
            5 => Modes::REVERSE_VIDEO,
            6 => Modes::ORIGIN,
            7 => Modes::AUTOWRAP,
            12 => Modes::CURSOR_BLINK,
            25 => Modes::CURSOR_VISIBLE,
            9 => Modes::MOUSE_X10,
            1000 => Modes::MOUSE_NORMAL,
            1002 => Modes::MOUSE_BUTTON,
            1003 => Modes::MOUSE_ANY,
            1004 => Modes::FOCUS_EVENTS,
            1006 => Modes::MOUSE_SGR,
            1016 => Modes::MOUSE_SGR_PIXELS,
            1007 => Modes::ALTERNATE_SCROLL,
            2027 => Modes::GRAPHEME_CLUSTERS,
            40 => Modes::ALLOW_COLUMN_SWITCH,
            47 | 1047 | 1049 => Modes::ALT_SCREEN,
            2004 => Modes::BRACKETED_PASTE,
            2026 => Modes::SYNC_OUTPUT,
            2031 => Modes::COLOR_SCHEME_UPDATES,
            _ => return None,
        };
        Some(self.modes.contains(flag))
    }

    fn sgr(&mut self, params: &Params) {
        let plain = self.cursor.link_extended;
        let mut underline_color = self.cursor.underline_color;
        let mut extended_changed = false;
        let pen = &mut self.cursor.pen;
        if params.is_empty() {
            *pen = Cell { extended: plain, ..Cell::BLANK };
            self.cursor.underline_color = Color::DEFAULT;
            return;
        }
        let mut groups = params.groups();
        while let Some(group) = groups.next() {
            match group[0] {
                0 => {
                    *pen = Cell { extended: plain, ..Cell::BLANK };
                    underline_color = Color::DEFAULT;
                    extended_changed = true;
                }
                1 => pen.flags.insert(Flags::BOLD),
                2 => pen.flags.insert(Flags::DIM),
                3 => pen.flags.insert(Flags::ITALIC),
                4 => {
                    pen.flags.remove(Flags::ANY_UNDERLINE);
                    match group.get(1).copied().unwrap_or(1) {
                        0 => {}
                        2 => pen.flags.insert(Flags::DOUBLE_UNDERLINE),
                        3 => pen.flags.insert(Flags::CURLY_UNDERLINE),
                        4 => pen.flags.insert(Flags::DOTTED_UNDERLINE),
                        5 => pen.flags.insert(Flags::DASHED_UNDERLINE),
                        _ => pen.flags.insert(Flags::UNDERLINE),
                    }
                }
                5 | 6 => pen.flags.insert(Flags::BLINK),
                7 => pen.flags.insert(Flags::INVERSE),
                8 => pen.flags.insert(Flags::HIDDEN),
                9 => pen.flags.insert(Flags::STRIKETHROUGH),
                21 => {
                    pen.flags.remove(Flags::ANY_UNDERLINE);
                    pen.flags.insert(Flags::DOUBLE_UNDERLINE);
                }
                22 => pen.flags.remove(Flags::BOLD | Flags::DIM),
                23 => pen.flags.remove(Flags::ITALIC),
                24 => pen.flags.remove(Flags::ANY_UNDERLINE),
                25 => pen.flags.remove(Flags::BLINK),
                27 => pen.flags.remove(Flags::INVERSE),
                28 => pen.flags.remove(Flags::HIDDEN),
                29 => pen.flags.remove(Flags::STRIKETHROUGH),
                n @ 30..=37 => pen.fg = Color::indexed((n - 30) as u8),
                38 | 48 | 58 => {
                    if let Some(color) = extended_color(group, &mut groups) {
                        match group[0] {
                            38 => pen.fg = color,
                            48 => pen.bg = color,
                            _ => {
                                underline_color = color;
                                extended_changed = true;
                            }
                        }
                    }
                }
                39 => pen.fg = Color::DEFAULT,
                n @ 40..=47 => pen.bg = Color::indexed((n - 40) as u8),
                49 => pen.bg = Color::DEFAULT,
                53 => pen.flags.insert(Flags::OVERLINE),
                55 => pen.flags.remove(Flags::OVERLINE),
                59 => {
                    underline_color = Color::DEFAULT;
                    extended_changed = true;
                }
                n @ 90..=97 => pen.fg = Color::indexed((n - 90 + 8) as u8),
                n @ 100..=107 => pen.bg = Color::indexed((n - 100 + 8) as u8),
                n => log::debug!("unhandled SGR {n}"),
            }
        }
        if extended_changed {
            self.cursor.underline_color = underline_color;
            if underline_color.is_default() {
                self.cursor.pen.extended = plain;
            } else {
                self.update_pen_extended();
            }
        }
    }

    /// Recomputes the pen's extended attributes index after its link or underline color changed.
    fn update_pen_extended(&mut self) {
        let cursor = &mut self.cursor;
        cursor.link_extended = self.extended.intern(Extended { link: cursor.link, ..Extended::DEFAULT });
        cursor.pen.extended = if cursor.underline_color.is_default() {
            cursor.link_extended
        } else {
            self.extended.intern(Extended { underline_color: cursor.underline_color, link: cursor.link, size: None })
        };
    }

    fn respond(&mut self, bytes: &[u8]) {
        self.responses.extend_from_slice(bytes);
    }
}

/// Parses `38;5;n`, `38;2;r;g;b` and their colon forms. The semicolon forms
/// take their values from the following groups, which are consumed on success.
fn extended_color(group: &[u16], rest: &mut Groups<'_>) -> Option<Color> {
    let byte = |v: u16| v.min(255) as u8;
    if group.len() > 1 {
        return match group[1] {
            5 => group.get(2).map(|&n| Color::indexed(byte(n))),
            2 => {
                let values = &group[2..];
                let rgb = match values.len() {
                    3 => values,
                    n if n >= 4 => &values[1..4],
                    _ => return None,
                };
                Some(Color::rgb(byte(rgb[0]), byte(rgb[1]), byte(rgb[2])))
            }
            _ => None,
        };
    }
    let mut ahead = rest.clone();
    let color = match ahead.next()?[0] {
        5 => Color::indexed(byte(ahead.next()?[0])),
        2 => {
            let (r, g, b) = (ahead.next()?[0], ahead.next()?[0], ahead.next()?[0]);
            Color::rgb(byte(r), byte(g), byte(b))
        }
        _ => return None,
    };
    *rest = ahead;
    Some(color)
}

fn default_tabs(cols: usize) -> Vec<bool> {
    (0..cols).map(|c| c % 8 == 0).collect()
}

/// Writes one cell per byte: a copy of `pen` holding that ASCII character.
#[inline]
fn fill_ascii(cells: &mut [Cell], bytes: &[u8], pen: Cell) {
    #[cfg(target_endian = "little")]
    {
        // SAFETY: `Cell` is `repr(C)`, 16 bytes without padding, and starts with
        // `ch`, so as a little endian `u128` the character is in the low 32 bits.
        let template = unsafe { std::mem::transmute::<Cell, u128>(Cell { ch: '\0', ..pen }) };
        for (cell, &b) in cells.iter_mut().zip(bytes) {
            // SAFETY: the same layout, with an ASCII byte as the character.
            *cell = unsafe { std::mem::transmute::<u128, Cell>(template | u128::from(b)) };
        }
    }
    #[cfg(not(target_endian = "little"))]
    for (cell, &b) in cells.iter_mut().zip(bytes) {
        *cell = Cell { ch: b as char, ..pen };
    }
}

/// Clears the other half of wide characters cut by writing `width` cells at `col`.
#[inline]
fn repair_wide(line: &mut crate::grid::Row, col: usize, width: usize) {
    let cols = line.cells.len();
    if col > 0 && line.cells[col].flags.contains(Flags::WIDE_SPACER) && line.cells[col - 1].flags.contains(Flags::WIDE)
    {
        let cell = &mut line.cells[col - 1];
        cell.ch = '\0';
        cell.flags.remove(Flags::CONTENT_MASK);
    }
    let end = col + width - 1;
    if end + 1 < cols && line.cells[end].flags.contains(Flags::WIDE) {
        let cell = &mut line.cells[end + 1];
        cell.ch = '\0';
        cell.flags.remove(Flags::CONTENT_MASK);
    }
}

/// Display width with fast paths for the most common ranges.
#[inline(always)]
fn char_width(c: char) -> Option<usize> {
    match u32::from(c) {
        0x20..=0x7e | 0xa0..=0x2ff => Some(1),
        0x4e00..=0x9fff => Some(2),
        _ => c.width(),
    }
}

fn decode_hex(hex: &[u8]) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    hex.chunks(2).map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()).collect()
}

/// Terminfo capabilities reported through XTGETTCAP. Empty strings are booleans.
fn capability(name: &str) -> Option<&'static str> {
    Some(match name {
        "TN" | "name" => "xterm-tron",
        "Co" | "colors" => "256",
        "RGB" => "8/8/8",
        "Tc" => "",
        "Smulx" => "\x1b[4:%p1%dm",
        "Setulc" => "\x1b[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%d%;m",
        "setrgbf" => "\x1b[38:2::%p1%d:%p2%d:%p3%dm",
        "setrgbb" => "\x1b[48:2::%p1%d:%p2%d:%p3%dm",
        "Ss" => "\x1b[%p1%d q",
        "Se" => "\x1b[2 q",
        "Sync" => "\x1b[?2026%?%p1%{1}%-%tl%eh%;",
        "indn" => "\x1b[%p1%dS",
        "rin" => "\x1b[%p1%dT",
        "BE" => "\x1b[?2004h",
        "BD" => "\x1b[?2004l",
        "PS" => "\x1b[200~",
        "PE" => "\x1b[201~",
        "fe" => "\x1b[?1004h",
        "fd" => "\x1b[?1004l",
        "kxIN" => "\x1b[I",
        "kxOUT" => "\x1b[O",
        _ => return None,
    })
}

fn parse_number(bytes: &[u8]) -> Option<usize> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

fn is_regional_indicator(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

impl Perform for Terminal {
    fn print(&mut self, c: char) {
        let c = self.translate(c);
        let Some(width) = char_width(c) else { return };
        if width == 0 || self.joins_cluster(c) {
            self.append_to_cluster(c);
            return;
        }
        self.last_char = Some(c);
        self.write_char(c, width);
    }

    fn print_ascii(&mut self, bytes: &[u8]) {
        if self.charsets[self.active_charset] != Charset::Ascii || self.modes.contains(Modes::INSERT) {
            for &b in bytes {
                self.print(b as char);
            }
            return;
        }
        let cols = self.cols();
        let autowrap = self.modes.contains(Modes::AUTOWRAP);
        let mut rest = bytes;
        while !rest.is_empty() {
            if self.cursor.pending_wrap && autowrap {
                self.wrap();
            }
            let (row, col) = (self.cursor.row, self.cursor.col);
            if self.grids[self.active].row(row).line_size != LineSize::Single {
                for &b in rest {
                    self.print(char::from(b));
                }
                return;
            }
            let chunk = if self.cursor.pending_wrap {
                // No auto-wrap: everything lands on the last column, the last byte wins.
                let last = &rest[rest.len() - 1..];
                rest = &[];
                last
            } else {
                let n = (cols - col).min(rest.len());
                let (head, tail) = rest.split_at(n);
                rest = tail;
                head
            };
            let n = chunk.len();
            let end = col + n;
            if self.multicell {
                self.clear_multicells(row, col, n);
            }
            let pen = self.cursor.pen;
            let line = self.grids[self.active].row_mut(row);
            repair_wide(line, col, n);
            line.touch(col, end);
            fill_ascii(&mut line.cells[col..end], chunk, pen);
            self.last_cluster = Some((row, col + n - 1));
            if col + n >= cols {
                self.cursor.col = cols - 1;
                self.cursor.pending_wrap = true;
            } else {
                self.cursor.col = col + n;
            }
        }
        self.last_char = bytes.last().map(|&b| b as char);
        self.last_was_zwj = false;
    }

    fn print_str(&mut self, text: &str) {
        if self.charsets[self.active_charset] != Charset::Ascii || self.modes.contains(Modes::INSERT) {
            for c in text.chars() {
                self.print(c);
            }
            return;
        }
        let cols = self.cols();
        let autowrap = self.modes.contains(Modes::AUTOWRAP);
        let mut chars = text.chars();
        let mut next = chars.next();
        while let Some(first) = next {
            // Fast segment: write characters straight into the current row until
            // something needs the general path (wrapping, clusters, overwriting
            // wide characters or graphemes).
            if autowrap
                && !self.cursor.pending_wrap
                && !(self.last_was_zwj && self.last_cluster.is_some())
                && !self.multicell
                && self.grids[self.active].row(self.cursor.row).line_size == LineSize::Single
            {
                let (row, start) = (self.cursor.row, self.cursor.col);
                let pen = self.cursor.pen;
                let line = self.grids[self.active].row_mut(row);
                let mut col = start;
                let mut current = Some(first);
                let mut last = None;
                while let Some(c) = current {
                    let width = match char_width(c) {
                        Some(width @ 1..=2) if !is_regional_indicator(c) => width,
                        _ => break,
                    };
                    if col + width > cols
                        || line.cells[col..col + width].iter().any(|cell| cell.flags.intersects(Flags::CONTENT_MASK))
                    {
                        break;
                    }
                    if width == 2 {
                        line.cells[col] = Cell { ch: c, flags: pen.flags | Flags::WIDE, ..pen };
                        line.cells[col + 1] = Cell { ch: '\0', flags: pen.flags | Flags::WIDE_SPACER, ..pen };
                    } else {
                        line.cells[col] = Cell { ch: c, ..pen };
                    }
                    last = Some((c, col));
                    col += width;
                    current = chars.next();
                    if col >= cols {
                        break;
                    }
                }
                if let Some((c, cluster_col)) = last {
                    line.touch(start, col);
                    self.last_cluster = Some((row, cluster_col));
                    self.last_char = Some(c);
                    self.last_was_zwj = false;
                    if col >= cols {
                        self.cursor.col = cols - 1;
                        self.cursor.pending_wrap = true;
                    } else {
                        self.cursor.col = col;
                    }
                    next = current;
                    continue;
                }
            }

            // General path for one character.
            let c = first;
            next = chars.next();
            let width = match char_width(c) {
                Some(width @ 1..=2) => width,
                _ => {
                    self.print(c);
                    continue;
                }
            };
            if self.joins_cluster(c) {
                self.append_to_cluster(c);
                continue;
            }
            self.last_char = Some(c);
            self.write_char(c, width);
        }
    }

    fn execute(&mut self, byte: u8) {
        self.last_cluster = None;
        self.last_was_zwj = false;
        match byte {
            0x07 => self.events.push(TermEvent::Bell),
            0x08 => self.move_left(1),
            0x09 => self.tab_forward(1),
            0x0a..=0x0c => {
                self.linefeed();
                if self.modes.contains(Modes::LINEFEED_NEWLINE) {
                    self.cursor.col = 0;
                }
            }
            0x0d => {
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
            }
            0x0e => self.active_charset = 1,
            0x0f => self.active_charset = 0,
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: u8) {
        self.last_cluster = None;
        self.last_was_zwj = false;
        if ignore {
            return;
        }
        let n = |i: usize| params.get_or(i, 1) as usize;
        let mode = |i: usize| params.raw(i).unwrap_or(0);
        match (intermediates, action) {
            ([], b'@') => self.insert_blanks(n(0)),
            ([], b'A') => self.move_up(n(0)),
            ([], b'B' | b'e') => self.move_down(n(0)),
            ([], b'C' | b'a') => self.move_right(n(0)),
            ([], b'D') => self.move_left(n(0)),
            ([], b'E') => {
                self.move_down(n(0));
                self.cursor.col = 0;
            }
            ([], b'F') => {
                self.move_up(n(0));
                self.cursor.col = 0;
            }
            ([], b'G' | b'`') => {
                self.cursor.col = (n(0) - 1).min(self.line_cols(self.cursor.row) - 1);
                self.cursor.pending_wrap = false;
            }
            ([], b'H' | b'f') => self.goto(n(0) - 1, n(1) - 1),
            ([], b'I') => self.tab_forward(n(0)),
            ([] | [b'?'], b'J') => self.erase_display(mode(0)),
            ([] | [b'?'], b'K') => self.erase_line(mode(0)),
            ([], b'L') => self.insert_lines(n(0)),
            ([], b'M') => self.delete_lines(n(0)),
            ([], b'P') => self.delete_chars(n(0)),
            ([], b'S') => self.scroll_up(n(0)),
            ([], b'T') if params.len() <= 1 => self.scroll_down(n(0)),
            ([], b'X') => {
                let (row, col) = (self.cursor.row, self.cursor.col);
                let blank = self.blank();
                self.grids[self.active].erase(row, col..col + n(0), blank);
            }
            ([], b'Z') => self.tab_back(n(0)),
            ([], b'b') => {
                if let Some(c) = self.last_char {
                    for _ in 0..n(0).min(u16::MAX as usize) {
                        self.print(c);
                    }
                }
            }
            // VT220 with Sixel graphics (4) and ANSI color (22).
            ([], b'c') if mode(0) == 0 => self.respond(b"\x1b[?62;4;22c"),
            ([b'?'], b'S') => {
                let item = mode(0);
                let reply = match (item, params.raw(1).unwrap_or(0)) {
                    (1, 1 | 4) => "\x1b[?1;0;256S".to_string(),
                    (2, 1 | 4) => {
                        let (cell_w, cell_h) = self.cell_pixels;
                        let width = (self.cols() as u32 * cell_w).min(4096);
                        let height = (self.rows() as u32 * cell_h).min(4096);
                        format!("\x1b[?2;0;{width};{height}S")
                    }
                    _ => format!("\x1b[?{item};3;0S"),
                };
                self.respond(reply.as_bytes());
            }
            ([b'>'], b'c') => self.respond(b"\x1b[>1;10;0c"),
            ([], b'd') => {
                let col = self.cursor.col;
                self.goto(n(0) - 1, col);
            }
            ([], b'g') => match mode(0) {
                0 => {
                    let col = self.cursor.col;
                    self.tabs[col] = false;
                }
                3 => self.tabs.fill(false),
                _ => {}
            },
            ([], b'h' | b'l') => {
                for group in params.groups() {
                    self.set_ansi_mode(group[0], action == b'h');
                }
            }
            ([b'?'], b'h' | b'l') => {
                for group in params.groups() {
                    self.set_dec_mode(group[0], action == b'h');
                }
            }
            ([], b'm') => self.sgr(params),
            ([], b'n') => match mode(0) {
                5 => self.respond(b"\x1b[0n"),
                6 => {
                    let origin = if self.modes.contains(Modes::ORIGIN) { self.scroll_top } else { 0 };
                    let reply = format!("\x1b[{};{}R", self.cursor.row - origin + 1, self.cursor.col + 1);
                    self.respond(reply.as_bytes());
                }
                _ => {}
            },
            ([], b'r') => {
                let rows = self.rows();
                let top = n(0) - 1;
                let bottom = (params.get_or(1, rows as u16) as usize).min(rows) - 1;
                if top < bottom {
                    self.scroll_top = top;
                    self.scroll_bottom = bottom;
                    self.goto(0, 0);
                }
            }
            ([], b's') => self.save_cursor(),
            ([], b'u') => self.restore_cursor(),
            ([b'?'], b'u') => {
                let flags = self.keyboard_flags();
                self.respond(format!("\x1b[?{flags}u").as_bytes());
            }
            ([b'>'], b'u') => {
                let stack = &mut self.keyboard[self.active];
                if stack.len() >= 16 {
                    stack.remove(0);
                }
                stack.push(mode(0) as u8 & 0x1f);
            }
            ([b'<'], b'u') => {
                let stack = &mut self.keyboard[self.active];
                stack.truncate(stack.len().saturating_sub(n(0)));
            }
            ([b'='], b'u') => {
                let flags = mode(0) as u8 & 0x1f;
                let how = params.get_or(1, 1);
                let stack = &mut self.keyboard[self.active];
                let current = stack.last().copied().unwrap_or(0);
                let value = match how {
                    2 => current | flags,
                    3 => current & !flags,
                    _ => flags,
                };
                match stack.last_mut() {
                    Some(top) => *top = value,
                    None => stack.push(value),
                }
            }
            ([b'>'], b'q') if mode(0) == 0 => {
                self.respond(format!("\x1bP>|tron({})\x1b\\", env!("CARGO_PKG_VERSION")).as_bytes());
            }
            ([b'?'], b'n') => match mode(0) {
                6 => {
                    let origin = if self.modes.contains(Modes::ORIGIN) { self.scroll_top } else { 0 };
                    let reply = format!("\x1b[?{};{}R", self.cursor.row - origin + 1, self.cursor.col + 1);
                    self.respond(reply.as_bytes());
                }
                996 => {
                    let scheme = if self.is_dark() { 1 } else { 2 };
                    self.respond(format!("\x1b[?997;{scheme}n").as_bytes());
                }
                _ => {}
            },
            ([], b't') => {
                let (cell_w, cell_h) = self.cell_pixels;
                let (rows, cols) = (self.rows() as u32, self.cols() as u32);
                match mode(0) {
                    14 => self.respond(format!("\x1b[4;{};{}t", rows * cell_h, cols * cell_w).as_bytes()),
                    16 => self.respond(format!("\x1b[6;{cell_h};{cell_w}t").as_bytes()),
                    18 => self.respond(format!("\x1b[8;{rows};{cols}t").as_bytes()),
                    22 => {
                        if self.title_stack.len() < 32 {
                            self.title_stack.push(self.title.clone());
                        }
                    }
                    23 => {
                        if let Some(title) = self.title_stack.pop() {
                            self.title = title;
                            self.title_dirty = true;
                        }
                    }
                    _ => {}
                }
            }
            ([b' '], b'q') => {
                let (shape, blinking) = match mode(0) {
                    0 => (self.default_cursor_shape, true),
                    1 => (CursorShape::Block, true),
                    2 => (CursorShape::Block, false),
                    3 => (CursorShape::Underline, true),
                    4 => (CursorShape::Underline, false),
                    5 => (CursorShape::Beam, true),
                    6 => (CursorShape::Beam, false),
                    _ => return,
                };
                self.cursor_shape = shape;
                self.modes.set(Modes::CURSOR_BLINK, blinking);
            }
            ([b'?', b'$'], b'p') => {
                let m = mode(0);
                let state = match self.dec_mode_state(m) {
                    Some(true) => 1,
                    Some(false) => 2,
                    None => 0,
                };
                self.respond(format!("\x1b[?{m};{state}$y").as_bytes());
            }
            _ => log::debug!("unhandled CSI {:?} {}", String::from_utf8_lossy(intermediates), action as char),
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        self.last_cluster = None;
        self.last_was_zwj = false;
        if ignore {
            return;
        }
        match (intermediates, byte) {
            ([], b'7') => self.save_cursor(),
            ([], b'8') => self.restore_cursor(),
            ([b'#'], b'8') => {
                let rows = self.rows();
                let fill = Cell { ch: 'E', ..Cell::BLANK };
                for r in 0..rows {
                    self.grids[self.active].erase_row(r, fill);
                }
            }
            ([b'#'], size @ (b'3' | b'4' | b'5' | b'6')) => {
                let row = self.cursor.row;
                self.grids[self.active].row_mut(row).line_size = match size {
                    b'3' => LineSize::DoubleHeightTop,
                    b'4' => LineSize::DoubleHeightBottom,
                    b'6' => LineSize::DoubleWidth,
                    _ => LineSize::Single,
                };
                self.clamp_cursor_to_line();
            }
            ([], b'D') => self.linefeed(),
            ([], b'E') => {
                self.cursor.col = 0;
                self.linefeed();
            }
            ([], b'H') => {
                let col = self.cursor.col;
                self.tabs[col] = true;
            }
            ([], b'M') => self.reverse_index(),
            ([], b'c') => self.reset(),
            ([], b'=') => self.modes.insert(Modes::APP_KEYPAD),
            ([], b'>') => self.modes.remove(Modes::APP_KEYPAD),
            ([slot @ (b'(' | b')')], set) => {
                let charset = if set == b'0' { Charset::DecSpecial } else { Charset::Ascii };
                self.charsets[usize::from(*slot == b')')] = charset;
            }
            ([], b'\\') => {}
            _ => log::debug!("unhandled ESC {:?} {}", String::from_utf8_lossy(intermediates), byte as char),
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        let terminator = if bell_terminated { "\x07" } else { "\x1b\\" };
        match params {
            [b"0" | b"2", title @ ..] => {
                self.title = String::from_utf8_lossy(&title.join(&b';')).into_owned();
                self.title_dirty = true;
            }
            [b"4", pairs @ ..] => {
                for pair in pairs.as_chunks::<2>().0 {
                    let Some(index) = parse_number(pair[0]).filter(|&i| i < 256) else { continue };
                    if pair[1] == b"?" {
                        let spec = format_color_spec(self.palette.colors[index]);
                        self.respond(format!("\x1b]4;{index};{spec}{terminator}").as_bytes());
                    } else if let Some(color) = parse_color_spec(pair[1]) {
                        self.palette.colors[index] = color;
                        self.palette_generation += 1;
                    }
                }
            }
            [b"104"] => {
                self.palette.colors = self.default_palette.colors;
                self.palette_generation += 1;
            }
            [b"104", indices @ ..] => {
                for index in indices.iter().filter_map(|i| parse_number(i)).filter(|&i| i < 256) {
                    self.palette.colors[index] = self.default_palette.colors[index];
                }
                self.palette_generation += 1;
            }
            [kind @ (b"10" | b"11" | b"12"), specs @ ..] => {
                let first = parse_number(kind).unwrap_or(10);
                for (offset, spec) in specs.iter().enumerate() {
                    let number = first + offset;
                    let slot = match number {
                        10 => &mut self.palette.foreground,
                        11 => &mut self.palette.background,
                        12 => &mut self.palette.cursor,
                        _ => break,
                    };
                    if *spec == b"?" {
                        let reply = format!("\x1b]{number};{}{terminator}", format_color_spec(*slot));
                        self.respond(reply.as_bytes());
                    } else if let Some(color) = parse_color_spec(spec) {
                        *slot = color;
                        self.palette_generation += 1;
                    }
                }
            }
            [b"110", ..] => {
                self.palette.foreground = self.default_palette.foreground;
                self.palette_generation += 1;
            }
            [b"111", ..] => {
                self.palette.background = self.default_palette.background;
                self.palette_generation += 1;
            }
            [b"112", ..] => {
                self.palette.cursor = self.default_palette.cursor;
                self.palette_generation += 1;
            }
            [b"52", target, data] => {
                let primary = target.contains(&b'p') && !target.contains(&b'c');
                if *data == b"?" {
                    self.events.push(TermEvent::ClipboardLoad { primary, terminator });
                } else if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    self.events.push(TermEvent::ClipboardStore { primary, text });
                }
            }
            [b"8", link_params, uri @ ..] => {
                let uri = uri.join(&b';');
                if uri.is_empty() || uri.len() > 4096 {
                    self.cursor.link = 0;
                } else {
                    let id = link_params
                        .split(|&b| b == b':')
                        .find_map(|p| p.strip_prefix(b"id="))
                        .map(|id| String::from_utf8_lossy(id).into_owned());
                    let uri = String::from_utf8_lossy(&uri).into_owned();
                    self.cursor.link = self.intern_link(id, uri);
                }
                self.update_pen_extended();
            }
            [b"133", kind, ..] => self.shell_mark(kind.first().copied()),
            // `OSC 9 ; 4 ; ...` is ConEmu's progress report, not a notification.
            [b"9", rest @ ..] if rest.first() != Some(&&b"4"[..]) => {
                let body = String::from_utf8_lossy(&rest.join(&b';')).into_owned();
                self.notify(String::new(), body, None);
            }
            [b"777", b"notify", title, body @ ..] => {
                let title = String::from_utf8_lossy(title).into_owned();
                let body = String::from_utf8_lossy(&body.join(&b';')).into_owned();
                self.notify(title, body, None);
            }
            [b"99", metadata, payload @ ..] => self.kitty_notification(metadata, &payload.join(&b';'), terminator),
            [b"1337", rest @ ..] => self.iterm2(&rest.join(&b';')),
            [b"66", metadata, text @ ..] => self.text_sizing(metadata, &text.join(&b';')),
            [b"22", name] if name.len() <= 64 => {
                self.events.push(TermEvent::PointerShape(String::from_utf8_lossy(name).into_owned()));
            }
            [b"7777", token, payload @ ..] => self.events.push(TermEvent::StartupScreen {
                token: String::from_utf8_lossy(token).into_owned(),
                payload: String::from_utf8_lossy(&payload.join(&b';')).into_owned(),
            }),
            [b"7", uri, ..] => {
                let uri = String::from_utf8_lossy(uri);
                let path = uri
                    .strip_prefix("file://")
                    .and_then(|rest| rest.find('/').map(|i| rest[i..].to_owned()))
                    .unwrap_or_else(|| uri.into_owned());
                self.cwd = Some(path);
            }
            [kind, ..] => log::debug!("unhandled OSC {}", String::from_utf8_lossy(kind)),
            [] => {}
        }
    }

    fn hook(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: u8) {
        self.dcs = match (intermediates, action, ignore) {
            ([b'$'], b'q', false) => Some(DcsRequest::Decrqss(Vec::new())),
            ([b'+'], b'q', false) => Some(DcsRequest::Xtgettcap(Vec::new())),
            ([], b'q', false) => Some(DcsRequest::Sixel(Box::new(crate::sixel::SixelDecoder::new(params)))),
            _ => None,
        };
    }

    fn put(&mut self, byte: u8) {
        match &mut self.dcs {
            Some(DcsRequest::Decrqss(buffer) | DcsRequest::Xtgettcap(buffer)) if buffer.len() < 4096 => {
                buffer.push(byte)
            }
            Some(DcsRequest::Sixel(decoder)) => decoder.put(byte),
            _ => {}
        }
    }

    fn unhook(&mut self) {
        match self.dcs.take() {
            Some(DcsRequest::Decrqss(request)) => self.decrqss(&request),
            Some(DcsRequest::Xtgettcap(request)) => self.xtgettcap(&request),
            Some(DcsRequest::Sixel(decoder)) => {
                if let Some((width, height, rgba)) = decoder.finish(self.palette.background) {
                    let ctx = self.graphics_context();
                    let (_, rows) = self.graphics.add_image(width, height, rgba, &ctx);
                    // Leave the cursor on the line below the image, in the same column.
                    let col = self.cursor.col;
                    for _ in 0..rows {
                        self.linefeed();
                    }
                    self.cursor.col = col;
                }
            }
            None => {}
        }
    }

    fn apc_dispatch(&mut self, data: &[u8]) {
        let Some(payload) = data.strip_prefix(b"G") else { return };
        let ctx = self.graphics_context();
        let outcome = self.graphics.handle(payload, &ctx);
        if let Some(response) = outcome.response {
            self.respond(&response);
        }
        if let Some((cols, rows)) = outcome.cursor_advance {
            for _ in 1..rows {
                self.linefeed();
            }
            self.cursor.col = (self.cursor.col + cols as usize).min(self.cols() - 1);
            self.cursor.pending_wrap = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::ColorKind;
    use crate::parser::Parser;

    fn term(cols: usize, rows: usize, input: &[u8]) -> Terminal {
        let mut t = Terminal::new(cols, rows, 100);
        Parser::new().advance(&mut t, input);
        t
    }

    fn line(t: &Terminal, row: usize) -> String {
        let mut out = String::new();
        let r = t.grid().row(row);
        for col in 0..r.cells.len() {
            r.push_cell_text(col, &mut out);
        }
        out.trim_end().to_string()
    }

    #[test]
    fn fast_width_ranges_match_unicode_width() {
        // The soft hyphen takes a cell, as in glibc's wcwidth and other terminals.
        let mismatches: Vec<String> = (0..0x1_0000u32)
            .filter_map(char::from_u32)
            .filter(|&c| c != '\u{AD}' && char_width(c) != c.width())
            .map(|c| format!("U+{:04X}", u32::from(c)))
            .collect();
        assert!(mismatches.is_empty(), "{mismatches:?}");
    }

    #[test]
    fn sgr_semicolon_colors_consume_their_values() {
        let t = term(10, 1, b"\x1b[38;5;196;1;48;2;1;2;3;4mX\x1b[38;5mY");
        let row = t.grid().row(0);
        assert_eq!(row.cells[0].fg.kind(), ColorKind::Indexed(196));
        assert_eq!(row.cells[0].bg.kind(), ColorKind::Rgb(1, 2, 3));
        assert!(row.cells[0].flags.contains(Flags::BOLD | Flags::UNDERLINE));
        assert!(row.cells[1].flags.contains(Flags::BLINK));
    }

    #[test]
    fn prints_and_wraps() {
        let t = term(5, 3, b"hello world");
        assert_eq!(line(&t, 0), "hello");
        assert_eq!(line(&t, 1), " worl");
        assert_eq!(line(&t, 2), "d");
        assert!(t.grid().row(0).wrapped);
    }

    #[test]
    fn pending_wrap_does_not_scroll_early() {
        let t = term(5, 2, b"abcde");
        assert_eq!(t.cursor().row, 0);
        assert_eq!(t.cursor().col, 4);
    }

    #[test]
    fn newline_scrolls_into_scrollback() {
        let t = term(10, 2, b"one\r\ntwo\r\nthree");
        assert_eq!(line(&t, 0), "two");
        assert_eq!(line(&t, 1), "three");
        assert_eq!(t.grid().scrollback_len(), 1);
    }

    #[test]
    fn cursor_position_and_erase() {
        let t = term(10, 3, b"aaaaaaaaaa\x1b[1;4H\x1b[K");
        assert_eq!(line(&t, 0), "aaa");
        let t = term(10, 3, b"xxxxx\r\nyyyyy\x1b[2J");
        assert_eq!(line(&t, 0), "");
        assert_eq!(line(&t, 1), "");
    }

    #[test]
    fn sgr_truecolor_both_forms() {
        let t = term(10, 1, b"\x1b[38;2;1;2;3;48:2::4:5:6;4:3mX");
        let cell = t.grid().row(0).cells[0];
        assert_eq!(cell.fg.kind(), ColorKind::Rgb(1, 2, 3));
        assert_eq!(cell.bg.kind(), ColorKind::Rgb(4, 5, 6));
        assert!(cell.flags.contains(Flags::CURLY_UNDERLINE));
    }

    #[test]
    fn wide_characters_take_two_cells() {
        let t = term(4, 2, "a中b".as_bytes());
        let row = t.grid().row(0);
        assert!(row.cells[1].flags.contains(Flags::WIDE));
        assert!(row.cells[2].flags.contains(Flags::WIDE_SPACER));
        assert_eq!(row.cells[3].ch, 'b');
    }

    #[test]
    fn wide_character_wraps_at_last_column() {
        let t = term(3, 2, "ab中".as_bytes());
        assert_eq!(line(&t, 0), "ab");
        assert!(t.grid().row(1).cells[0].flags.contains(Flags::WIDE));
    }

    #[test]
    fn alternate_screen_preserves_primary() {
        let t = term(10, 2, b"primary\x1b[?1049hALT\x1b[?1049l");
        assert!(!t.is_alt_screen());
        assert_eq!(line(&t, 0), "primary");
        assert_eq!(t.cursor().col, 7);
    }

    #[test]
    fn scroll_region_insert_delete_lines() {
        let t = term(5, 4, b"1\r\n2\r\n3\r\n4\x1b[2;3r\x1b[2;1H\x1b[M");
        assert_eq!(line(&t, 0), "1");
        assert_eq!(line(&t, 1), "3");
        assert_eq!(line(&t, 2), "");
        assert_eq!(line(&t, 3), "4");
    }

    #[test]
    fn device_status_report() {
        let mut t = term(10, 5, b"\x1b[3;4H\x1b[6n\x1b[c");
        assert_eq!(t.take_responses().unwrap(), b"\x1b[3;4R\x1b[?62;4;22c");
    }

    #[test]
    fn title_and_mode_query() {
        let mut t = term(10, 2, b"\x1b]2;my;title\x07\x1b[?2026h\x1b[?2026$p");
        assert_eq!(t.take_title().as_deref(), Some("my;title"));
        assert_eq!(t.take_responses().unwrap(), b"\x1b[?2026;1$y");
        assert!(t.sync_blocked());
    }

    #[test]
    fn dec_special_graphics() {
        let t = term(5, 1, b"\x1b(0lqk\x1b(Bq");
        assert_eq!(line(&t, 0), "┌─┐q");
    }

    #[test]
    fn combining_marks_and_emoji_sequences() {
        let t = term(10, 1, "e\u{301}x 👨\u{200D}👩 🇩🇪".as_bytes());
        let row = t.grid().row(0);
        assert_eq!(row.combining(0), Some("\u{301}"));
        assert_eq!(row.cells[1].ch, 'x');
        assert!(row.cells[3].flags.contains(Flags::WIDE));
        assert_eq!(row.combining(3), Some("\u{200D}👩"));
        assert!(row.cells[6].flags.contains(Flags::WIDE));
        assert_eq!(row.combining(6), Some("🇪"));
        assert_eq!(t.cursor().col, 8);
    }

    #[test]
    fn variation_selector_widens() {
        let t = term(10, 1, "❤\u{FE0F}a".as_bytes());
        let row = t.grid().row(0);
        assert!(row.cells[0].flags.contains(Flags::WIDE));
        assert_eq!(row.cells[2].ch, 'a');
    }

    #[test]
    fn osc_colors_and_clipboard() {
        let mut t = term(10, 1, b"\x1b]11;?\x07\x1b]4;1;#ff0000\x1b\\\x1b]52;c;aGVsbG8=\x07");
        assert_eq!(t.take_responses().unwrap(), b"\x1b]11;rgb:0000/0000/0000\x07");
        assert_eq!(t.palette().colors[1], [255, 0, 0]);
        assert_eq!(t.take_events(), vec![TermEvent::ClipboardStore { primary: false, text: "hello".into() }]);
    }

    #[test]
    fn selection_text_joins_wrapped_lines() {
        let mut t = term(5, 4, b"hello world\r\nnext");
        let start = t.viewport_point(0, 0);
        let end = t.viewport_point(3, 4);
        t.set_selection(Some(Selection { kind: crate::SelectionKind::Simple, anchor: start, head: end }));
        assert_eq!(t.selection_text().unwrap(), "hello world\nnext");
        let word = t.viewport_point(1, 2);
        t.set_selection(Some(Selection::new(crate::SelectionKind::Word, word)));
        assert_eq!(t.selection_text().unwrap(), "world");
    }

    #[test]
    fn reflow_on_resize() {
        let mut t = term(10, 3, b"0123456789abc");
        t.resize(20, 3);
        assert_eq!(line(&t, 0), "0123456789abc");
        assert_eq!((t.cursor().row, t.cursor().col), (0, 13));
    }

    #[test]
    fn kitty_image_is_placed_at_cursor() {
        let mut t = term(10, 5, b"\x1b[2;3H\x1b_Ga=T,f=24,s=1,v=1,i=5;AAAA\x1b\\");
        assert_eq!(t.take_responses().unwrap(), b"\x1b_Gi=5;OK\x1b\\");
        let p = &t.graphics().placements()[0];
        assert_eq!((p.line, p.col), (1, 2));
        assert_eq!(t.cursor().col, 3);
    }

    #[test]
    fn snapshot_copies_only_damaged_rows() {
        let mut t = term(10, 3, b"a\r\nb");
        let mut snapshot = crate::Snapshot::default();
        t.snapshot(&mut snapshot);
        assert!(snapshot.damaged.iter().all(|&d| d));
        assert_eq!(snapshot.rows[1].cells[0].ch, 'b');
        Parser::new().advance(&mut t, b"c");
        t.snapshot(&mut snapshot);
        assert_eq!(snapshot.damaged, vec![false, true, false]);
        assert_eq!(snapshot.rows[1].cells[1].ch, 'c');
        assert_eq!(snapshot.cursor().col, 2);
    }

    /// Feeds every character through `print`, bypassing the batched fast paths.
    struct CharByChar<'a>(&'a mut Terminal);

    impl Perform for CharByChar<'_> {
        fn print(&mut self, c: char) {
            self.0.print(c);
        }
        fn print_ascii(&mut self, bytes: &[u8]) {
            for &b in bytes {
                self.0.print(b as char);
            }
        }
        fn execute(&mut self, byte: u8) {
            self.0.execute(byte);
        }
        fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: u8) {
            self.0.csi_dispatch(params, intermediates, ignore, action);
        }
        fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
            self.0.esc_dispatch(intermediates, ignore, byte);
        }
    }

    #[test]
    fn fast_paths_match_character_by_character_printing() {
        const PIECES: &[&str] = &[
            "a",
            "bc",
            " ",
            "中",
            "文字",
            "é",
            "e\u{301}",
            "\u{200D}",
            "👨",
            "🇩",
            "🇪",
            "❤",
            "\u{FE0F}",
            "→",
            "✓",
            "\r\n",
            "\n",
            "\x1b[2D",
            "\x1b[H",
            "\x1b[1;31m",
            "\x1b[K",
            "\t",
            "\x1b[3@",
            "λ",
        ];
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        for round in 0..300 {
            let cols = 3 + round % 9;
            let mut fast = Terminal::new(cols, 4, 50);
            let mut slow = Terminal::new(cols, 4, 50);
            let (mut fast_parser, mut slow_parser) = (Parser::new(), Parser::new());
            let mut input = String::new();
            for _ in 0..60 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let piece = PIECES[(seed % PIECES.len() as u64) as usize];
                input.push_str(piece);
                fast_parser.advance(&mut fast, piece.as_bytes());
                slow_parser.advance(&mut CharByChar(&mut slow), piece.as_bytes());
                for row in 0..4 {
                    assert_eq!(
                        fast.grid().row(row).cells,
                        slow.grid().row(row).cells,
                        "cols {cols}, row {row} after {input:?}"
                    );
                    for col in 0..cols {
                        assert_eq!(
                            fast.grid().row(row).combining(col),
                            slow.grid().row(row).combining(col),
                            "{input:?}"
                        );
                    }
                }
                assert_eq!(fast.cursor(), slow.cursor(), "cols {cols}, cursor after {input:?}");
            }
        }
    }

    /// Random mixes of the newer sequences, resizes and selections must never panic.
    #[test]
    fn random_sequences_and_resizes_do_not_panic() {
        const PIECES: &[&str] = &[
            "text ",
            "中文",
            "\u{5D0}\u{5D1}",
            "e\u{301}",
            "👨\u{200D}👩",
            "\r\n",
            "\x1b[H",
            "\x1b[5;30H",
            "\x1b[2J",
            "\x1b[K",
            "\x1b[3L",
            "\x1b[2M",
            "\x1b[4@",
            "\x1b[3P",
            "\x1b[2;4r",
            "\x1b[r",
            "\x1b#3",
            "\x1b#4",
            "\x1b#6",
            "\x1b#8",
            "\x1b]66;s=3;Big\x07",
            "\x1b]66;s=7:w=7;x\x07",
            "\x1b]66;n=1:d=2:w=1;half\x07",
            "\x1b]133;A\x07$ \x1b]133;B\x07",
            "\x1b]133;C\x07",
            "\x1b]133;D\x07",
            "\x1b]99;i=1:d=0;t\x1b\\",
            "\x1b]1337;File=inline=1:iVBORw0KGgo=\x07",
            "\x1b[?40h\x1b[?3h",
            "\x1b[?3l",
            "\x1b[?2027l",
            "\x1b[?1049h",
            "\x1b[?1049l",
            "\x1bc",
        ];
        let mut seed: u64 = 0x1234_5678_9abc_def1;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..200 {
            let mut t = Terminal::new(10 + (next() % 30) as usize, 3 + (next() % 8) as usize, 20);
            let mut parser = Parser::new();
            for _ in 0..80 {
                match next() % 12 {
                    0 => t.resize(1 + (next() % 40) as usize, 1 + (next() % 12) as usize),
                    1 => {
                        let (rows, cols) = (t.rows(), t.cols());
                        let start = t.viewport_point((next() % rows as u64) as usize, (next() % cols as u64) as usize);
                        let end = t.viewport_point(rows - 1, cols - 1);
                        t.set_selection(Some(Selection {
                            kind: crate::SelectionKind::Simple,
                            anchor: start,
                            head: end,
                        }));
                        let _ = t.selection_text();
                    }
                    2 => {
                        t.scroll_to_prompt(next() % 2 == 0);
                        let _ = t.command_output(None);
                    }
                    _ => parser.advance(&mut t, PIECES[(next() % PIECES.len() as u64) as usize].as_bytes()),
                }
                let mut snapshot = crate::Snapshot::default();
                t.snapshot(&mut snapshot);
            }
        }
    }

    /// Mimics how fish 4 repaints its prompt after SIGWINCH: carriage return,
    /// a cursor up when its last paint ended with a newline, then the prompt,
    /// truncated with an ellipsis when it does not fit.
    #[test]
    fn shell_prompt_redraw_survives_repeated_resizes() {
        const PROMPT: &str = "skyline@fedora ~/P/tron-terminal (main)> ";
        let paint = |cols: usize, below: usize| -> (String, usize) {
            let mut out = String::from("\r");
            out.push_str(&"\x1b[A".repeat(below));
            out.push_str("\x1b]133;A;click_events=1\x07");
            let len = PROMPT.chars().count();
            if len < cols {
                out.push_str(PROMPT);
                out.push_str(&format!("\x1b]133;B\x07\x1b[J\r\x1b[{len}C"));
                (out, 0)
            } else {
                let tail: String = PROMPT.chars().skip(len - (cols - 1)).collect();
                out.push('…');
                out.push_str(&tail);
                out.push_str("\x1b]133;B\x07\r\n\x1b[J");
                (out, 1)
            }
        };
        let mut t = Terminal::new(80, 6, 100);
        let mut parser = Parser::new();
        let (first, mut below) = paint(80, 0);
        parser.advance(&mut t, format!("line one\r\n{first}").as_bytes());
        for cols in [50, 80, 35, 90, 40, 85, 30, 100, 38, 80] {
            t.resize(cols, 6);
            let (repaint, next_below) = paint(cols, below);
            parser.advance(&mut t, repaint.as_bytes());
            below = next_below;
            let lines: Vec<String> = (0..6).map(|row| line(&t, row)).collect();
            assert_eq!(lines[0], "line one", "cols {cols}: {lines:?}");
            let prompts = lines.iter().filter(|l| l.contains("(main)>")).count();
            assert_eq!(prompts, 1, "cols {cols}: {lines:?}");
            assert!(lines[1].ends_with("(main)>"), "cols {cols}: {lines:?}");
            assert!(lines[2..].iter().all(String::is_empty), "cols {cols}: {lines:?}");
        }
    }

    #[test]
    fn images_follow_their_text_through_reflow() {
        let mut t = Terminal::new(10, 6, 100);
        t.set_cell_pixels(10, 20);
        // Two wrapped rows of text, then a one cell image on the next row.
        Parser::new().advance(&mut t, b"0123456789abc\r\n\x1b_Ga=T,f=24,s=1,v=1,q=2;AAAA\x1b\\\r\n");
        let image_row = |t: &Terminal| t.graphics().placements()[0].line - t.grid().screen_line(0);
        assert_eq!(image_row(&t), 2);
        t.resize(20, 6);
        assert_eq!(line(&t, 0), "0123456789abc");
        assert_eq!(image_row(&t), 1);
        t.resize(5, 6);
        assert_eq!(image_row(&t), 3);
    }

    #[test]
    fn height_only_resize_keeps_the_prompt() {
        let mut t = term(20, 4, b"out\r\n\x1b]133;A\x07$ \x1b]133;B\x07");
        t.resize(20, 8);
        assert_eq!(line(&t, 0), "out");
        assert_eq!(line(&t, 1), "$");
        assert_eq!((t.cursor().row, t.cursor().col), (1, 2));
        t.resize(20, 3);
        assert_eq!(line(&t, 1), "$");
    }

    #[test]
    fn resize_during_command_output_still_reflows() {
        let mut t = term(10, 4, b"\x1b]133;A\x07$ \x1b]133;B\x07\x1b]133;C\x07\r\n0123456789abc");
        t.resize(20, 4);
        assert_eq!(line(&t, 0), "$");
        assert_eq!(line(&t, 1), "0123456789abc");
    }

    #[test]
    fn command_marks_jump_between_prompts_and_select_output() {
        let mut t = Terminal::new(20, 4, 100);
        let mut parser = Parser::new();
        for command in ["one", "two", "three"] {
            let bytes =
                format!("\x1b]133;A\x07$ \x1b]133;B\x07{command}\r\n\x1b]133;C\x07out {command}\r\n\x1b]133;D\x07");
            parser.advance(&mut t, bytes.as_bytes());
        }
        parser.advance(&mut t, b"\x1b]133;A\x07$ ");
        assert_eq!(t.command_marks().count(), 4);
        let (start, end) = t.command_output(None).unwrap();
        // Prompts are on lines 0, 2, 4 and 6, output on 1, 3 and 5.
        assert_eq!((start.line, end.line), (5, 5));
        t.set_selection(Some(Selection { kind: crate::SelectionKind::Simple, anchor: start, head: end }));
        assert_eq!(t.selection_text().unwrap(), "out three");
        assert_eq!(t.grid().viewport_line(0), 3);
        assert!(t.scroll_to_prompt(true));
        assert_eq!(t.grid().viewport_line(0), 2);
        assert!(t.scroll_to_prompt(true));
        assert_eq!(t.grid().viewport_line(0), 0);
        assert!(!t.scroll_to_prompt(true));
        assert_eq!(t.command_output(Some(3)).map(|(s, _)| s.line), Some(3));
        assert!(t.scroll_to_prompt(false));
        assert_eq!(t.grid().viewport_line(0), 2);
    }

    #[test]
    fn notifications_from_osc_9_777_and_99() {
        let mut t = term(
            10,
            2,
            b"\x1b]9;build done\x07\x1b]9;4;1;50\x07\x1b]777;notify;Title;Body\x07\
              \x1b]99;i=a:d=0;Hello\x1b\\\x1b]99;i=a:p=body:e=1:o=unfocused;V29ybGQ=\x1b\\\x1b]99;i=q:p=?;\x1b\\",
        );
        assert_eq!(
            t.take_events(),
            vec![
                TermEvent::Notification { title: "build done".into(), body: String::new(), when: None },
                TermEvent::Notification { title: "Title".into(), body: "Body".into(), when: None },
                TermEvent::Notification {
                    title: "Hello".into(),
                    body: "World".into(),
                    when: Some(NotifyWhen::Unfocused)
                },
            ]
        );
        let reply = String::from_utf8(t.take_responses().unwrap()).unwrap();
        assert!(reply.starts_with("\x1b]99;i=q:p=?;p=title,body"), "{reply:?}");
    }

    #[test]
    fn iterm2_inline_image_is_placed() {
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 30, 40);
            encoder.set_color(png::ColorType::Rgb);
            encoder.write_header().unwrap().write_image_data(&[9; 30 * 40 * 3]).unwrap();
        }
        let data = base64::engine::general_purpose::STANDARD.encode(&png);
        let mut t = Terminal::new(20, 10, 100);
        t.set_cell_pixels(10, 20);
        let (head, tail) = data.split_at(data.len() / 2);
        let input = format!(
            "ab\x1b]1337;File=inline=1:{data}\x07\r\n\x1b]1337;MultipartFile=inline=1;width=1\x07\
             \x1b]1337;FilePart={head}\x07\x1b]1337;FilePart={tail}\x07\x1b]1337;FileEnd\x07"
        );
        Parser::new().advance(&mut t, input.as_bytes());
        let placements = t.graphics().placements();
        assert_eq!(placements.len(), 2);
        assert_eq!((placements[0].line, placements[0].col, placements[0].cols, placements[0].rows), (0, 2, 3, 2));
        assert_eq!(placements[0].pixel_size, Some([30, 40]));
        assert_eq!(placements[1].pixel_size, Some([10, 13]));
        assert_eq!((t.cursor().row, t.cursor().col), (2, 1));
    }

    #[test]
    fn double_width_lines_halve_the_columns() {
        let mut t = term(20, 4, b"\x1b#6abcdefghijkl\x1b[1;20H");
        assert_eq!(t.grid().row(0).line_size, LineSize::DoubleWidth);
        assert_eq!(line(&t, 0), "abcdefghij");
        assert_eq!(line(&t, 1), "kl");
        assert_eq!(t.cursor().col, 9);
        Parser::new().advance(&mut t, b"\x1b[2;1H\x1b#3\x1b[H\x1b[2J");
        assert!((0..4).all(|row| t.grid().row(row).line_size == LineSize::Single));
    }

    #[test]
    fn text_sizing_writes_scaled_blocks() {
        let mut t = term(20, 5, b"\x1b]66;s=2;AB\x07");
        let size = |t: &Terminal, row: usize, col: usize| t.extended(t.grid().row(row).cells[col].extended).size;
        assert_eq!(t.grid().row(0).cells[0].ch, 'A');
        assert_eq!(size(&t, 0, 0).map(|s| (s.scale, s.width, s.dx, s.dy)), Some((2, 1, 0, 0)));
        assert_eq!(size(&t, 1, 1).map(|s| (s.dx, s.dy)), Some((1, 1)));
        assert_eq!(t.grid().row(0).cells[2].ch, 'B');
        assert_eq!((t.cursor().row, t.cursor().col), (0, 4));
        // Writing over part of a block clears the whole block.
        Parser::new().advance(&mut t, b"\x1b[2;2Hx");
        assert_eq!(t.grid().row(0).cells[0].ch, '\0');
        assert!(size(&t, 1, 0).is_none());
        assert_eq!(t.grid().row(1).cells[1].ch, 'x');
        // A two row block on the last row scrolls the screen by one first.
        Parser::new().advance(&mut t, b"\x1b[5;1H\x1b]66;s=2:w=3;hello\x07");
        assert_eq!(t.grid().row(0).cells[1].ch, 'x');
        let mut text = String::new();
        t.grid().row(3).push_cell_text(0, &mut text);
        assert_eq!(text, "hello");
        assert_eq!(size(&t, 4, 5).map(|s| (s.dx, s.dy)), Some((5, 1)));
        assert_eq!((t.cursor().row, t.cursor().col), (3, 6));
    }

    #[test]
    fn grapheme_cluster_mode_can_be_turned_off() {
        let t = term(10, 1, "\x1b[?2027l👨\u{200D}👩❤\u{FE0F}a".as_bytes());
        let row = t.grid().row(0);
        assert!(row.cells[0].flags.contains(Flags::WIDE));
        assert!(row.cells[2].flags.contains(Flags::WIDE));
        assert_eq!(row.cells[4].ch, '❤');
        assert_eq!(row.cells[5].ch, 'a');
    }

    #[test]
    fn pointer_shape_requests_become_events() {
        let mut t = term(10, 2, b"\x1b]22;pointer\x07\x1b]22;\x1b\\");
        assert_eq!(
            t.take_events(),
            vec![TermEvent::PointerShape("pointer".into()), TermEvent::PointerShape(String::new())]
        );
    }

    #[test]
    fn startup_screen_commands_become_events() {
        let mut t = term(10, 2, b"\x1b]7777;abc;shader=on,params=1:0.5:0:0\x07");
        assert_eq!(
            t.take_events(),
            vec![TermEvent::StartupScreen { token: "abc".into(), payload: "shader=on,params=1:0.5:0:0".into() }]
        );
    }

    #[test]
    fn column_mode_switch_resizes_when_allowed() {
        let mut t = term(80, 5, b"\x1b[?3h");
        assert_eq!(t.cols(), 80);
        Parser::new().advance(&mut t, b"\x1b[?40h\x1b[?3h\x1b[?3$p");
        assert_eq!(t.cols(), 132);
        assert!(t.take_events().contains(&TermEvent::ColumnsChanged(132)));
        assert!(t.take_responses().unwrap().ends_with(b"\x1b[?3;1$y"));
    }

    #[test]
    fn selection_follows_text_through_reflow() {
        let mut t = term(5, 4, b"hello world\r\nnext");
        let word = t.viewport_point(1, 2);
        t.set_selection(Some(Selection::new(crate::SelectionKind::Word, word)));
        assert_eq!(t.selection_text().unwrap(), "world");
        t.resize(20, 4);
        assert_eq!(t.selection_text().unwrap(), "world");
        t.resize(3, 4);
        assert_eq!(t.selection_text().unwrap(), "world");
    }

    #[test]
    fn kitty_keyboard_flag_stack() {
        let mut t = term(10, 2, b"\x1b[>1u\x1b[>5u\x1b[?u\x1b[=1;3u\x1b[?u\x1b[<u\x1b[?u\x1b[<5u\x1b[?u");
        assert_eq!(t.take_responses().unwrap(), b"\x1b[?5u\x1b[?4u\x1b[?1u\x1b[?0u");
        let t = term(10, 2, b"\x1b[>3u\x1b[?1049h");
        assert_eq!(t.keyboard_flags(), 0);
    }

    #[test]
    fn capability_and_state_queries() {
        let mut t = term(10, 4, b"\x1b[5 q\x1bP$q q\x1b\\\x1bP+q544e;6e6f7065\x1b\\\x1b[>q\x1b[18t\x1b[?996n");
        let replies = String::from_utf8(t.take_responses().unwrap()).unwrap();
        assert!(replies.starts_with("\x1bP1$r5 q\x1b\\"), "{replies:?}");
        assert!(replies.contains("\x1bP1+r544e=787465726D2D74726F6E\x1b\\"), "{replies:?}");
        assert!(replies.contains("\x1bP0+r6e6f7065\x1b\\"), "{replies:?}");
        assert!(replies.contains("\x1bP>|tron("), "{replies:?}");
        assert!(replies.contains("\x1b[8;4;10t"), "{replies:?}");
        assert!(replies.ends_with("\x1b[?997;1n"), "{replies:?}");
    }

    #[test]
    fn osc8_hyperlinks_survive_sgr_reset() {
        let t = term(20, 2, b"\x1b]8;id=a;https://tron.dev\x1b\\li\x1b[0mnk\x1b]8;;\x1b\\ x");
        let row = t.grid().row(0);
        let link = t.extended(row.cells[0].extended).link;
        assert_ne!(link, 0);
        assert_eq!(t.extended(row.cells[3].extended).link, link);
        assert_eq!(t.extended(row.cells[5].extended).link, 0);
        assert_eq!(t.hyperlink(link).unwrap().uri, "https://tron.dev");
        let found = t.link_at(t.viewport_point(0, 2)).unwrap();
        assert_eq!((found.start.col, found.end.col, found.uri.as_str()), (0, 3, "https://tron.dev"));
        let plain = term(30, 2, b"go to https://a.b/c, now");
        let found = plain.link_at(plain.viewport_point(0, 10)).unwrap();
        assert_eq!(found.uri, "https://a.b/c");
        assert_eq!(found.id, None);
    }

    #[test]
    fn sixel_image_is_placed_and_cursor_moves_below() {
        let mut t = Terminal::new(20, 10, 100);
        t.set_cell_pixels(10, 20);
        // 12 pixels tall: one cell row of 20 px.
        Parser::new().advance(&mut t, b"ab\x1bPq#1;2;100;0;0#1!10~-!10~\x1b\\x");
        let placement = &t.graphics().placements()[0];
        assert_eq!((placement.line, placement.col, placement.rows), (0, 2, 1));
        assert_eq!(t.graphics().images()[&placement.image_id].width, 10);
        assert_eq!((t.cursor().row, t.cursor().col), (1, 3));
    }

    #[test]
    fn column_mode_switch_clears_and_homes() {
        let t = term(10, 3, b"abc\r\ndef\x1b[2;3r\x1b[?3h");
        assert_eq!(line(&t, 0), "");
        assert_eq!(line(&t, 1), "");
        assert_eq!((t.cursor().row, t.cursor().col), (0, 0));
    }

    #[test]
    fn fish_prompt_repaint_after_resizes_leaves_one_prompt() {
        // Bytes fish 4 writes for its prompt and after each SIGWINCH: no cursor up,
        // no new line, just a carriage return, the prompt and a clear below.
        const PROMPT: &[u8] = b"\x1b]133;A;click_events=1\x07\x1b[92mskyline\x1b[m@\x1b[mfedora\x1b[m \
            \x1b[32m~/P/t/t/release\x1b[m (main)\x1b[m> \x1b]133;B\x07";
        let mut t = Terminal::new(100, 8, 100);
        let mut parser = Parser::new();
        parser.advance(&mut t, b"\x1b]133;A;click_events=1\x07skyline@fedora ~/P/t/t/release (main)> ls\r\n");
        parser.advance(&mut t, b"build  deps  examples  incremental  tron  tron.d\r\n");
        parser.advance(&mut t, PROMPT);
        parser.advance(&mut t, b"\x1b[K\r\x1b[39C");
        for cols in [80, 60, 100, 45, 90] {
            t.resize(cols, 8);
            let mut repaint = b"\r\r".to_vec();
            repaint.extend_from_slice(PROMPT);
            repaint.extend_from_slice(b"\x1b[J\r\x1b[39C");
            parser.advance(&mut t, &repaint);
            let lines: Vec<String> = (0..8).map(|row| line(&t, row)).collect();
            assert!(lines.concat().contains("tron  tron.d"), "the output survives, cols {cols}: {lines:?}");
            let prompts: Vec<usize> = (0..8).filter(|&row| lines[row].ends_with("(main)>")).collect();
            assert_eq!(prompts.len(), 1, "one prompt, cols {cols}: {lines:?}");
            assert_eq!((t.cursor().row, t.cursor().col), (prompts[0], 39), "cols {cols}: {lines:?}");
        }
    }

    #[test]
    fn resize_keeps_cursor_line() {
        let mut t = term(10, 4, b"a\r\nb\r\nc\r\nd");
        t.resize(10, 2);
        assert_eq!(line(&t, 1), "d");
        assert_eq!(t.cursor().row, 1);
        t.resize(10, 4);
        assert_eq!(line(&t, 0), "a");
        assert_eq!(t.cursor().row, 3);
    }
}
