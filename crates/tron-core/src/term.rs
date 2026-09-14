//! Terminal state: applies parsed escape sequences to the screen.

use std::time::{Duration, Instant};

use bitflags::bitflags;
use unicode_width::UnicodeWidthChar;

use base64::Engine;

use crate::cell::{Cell, Color, Flags};
use crate::graphics::{self, Graphics};
use crate::grid::Grid;
use crate::palette::{Palette, format_color_spec, parse_color_spec};
use crate::parser::{MAX_PARAMS, Params, Perform};
use crate::selection::{Point, Selection, SelectionRange};

/// How long synchronized output (mode 2026) may hold back rendering.
const SYNC_TIMEOUT: Duration = Duration::from_millis(150);

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

#[derive(Debug)]
enum DcsRequest {
    Decrqss(Vec<u8>),
    Xtgettcap(Vec<u8>),
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
    /// Kitty keyboard protocol flag stacks for the primary and alternate screen.
    keyboard: [Vec<u8>; 2],
    dcs: Option<DcsRequest>,
    links: Vec<Hyperlink>,
    link_ids: std::collections::HashMap<(Option<String>, String), u16>,
    title_stack: Vec<String>,
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
            modes: Modes::AUTOWRAP | Modes::CURSOR_VISIBLE | Modes::ALTERNATE_SCROLL,
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
            keyboard: [Vec::new(), Vec::new()],
            dcs: None,
            links: Vec::new(),
            link_ids: std::collections::HashMap::new(),
            title_stack: Vec::new(),
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
        snapshot.selection = selection;
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

    /// Target of hyperlink `id` from a cell's `link` field.
    pub fn hyperlink(&self, id: u16) -> Option<&Hyperlink> {
        id.checked_sub(1).and_then(|i| self.links.get(usize::from(i)))
    }

    /// The link at an absolute cell position: OSC 8 hyperlinks first, then URLs in the text.
    pub fn link_at(&self, point: Point) -> Option<LinkMatch> {
        let grid = self.grid();
        let logical = crate::text::LogicalLine::at(grid, point.line)?;
        let row = grid.line(point.line)?;
        let link = row.cells.get(point.col)?.link;
        if let Some(target) = self.hyperlink(link) {
            let same = |p: &Point| grid.line(p.line).and_then(|r| r.cells.get(p.col)).is_some_and(|c| c.link == link);
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
        // Shells that mark their prompt redraw it after SIGWINCH, assuming the
        // prompt was not rewrapped. Clear it instead of reflowing, and keep the
        // cursor where the shell expects it relative to the prompt start.
        let prompt = self.prompt_redraw_start();
        if let Some((prompt_row, _)) = prompt {
            let grid = &mut self.grids[PRIMARY];
            let old_cols = grid.cols();
            for row in prompt_row..grid.rows() {
                grid.erase(row, 0..old_cols, Cell::BLANK);
            }
        }
        for index in [PRIMARY, ALTERNATE] {
            let reflow = index == PRIMARY;
            if index == self.active {
                if let (Some((prompt_row, below)), PRIMARY) = (prompt, index) {
                    let (mut row, _) = self.grids[index].resize(cols, rows, (prompt_row, 0), reflow);
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
        for saved in self.saved.iter_mut().flatten() {
            saved.cursor.col = saved.cursor.col.min(cols - 1);
            saved.cursor.row = saved.cursor.row.min(rows - 1);
        }
        self.selection = None;
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
        self.cursor.col = col.min(self.cols() - 1);
        self.cursor.pending_wrap = false;
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
        self.cursor.col = (self.cursor.col + n).min(self.cols() - 1);
        self.cursor.pending_wrap = false;
    }

    fn move_left(&mut self, n: usize) {
        self.cursor.col = self.cursor.col.saturating_sub(n);
        self.cursor.pending_wrap = false;
    }

    fn tab_forward(&mut self, n: usize) {
        let cols = self.cols();
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
        self.last_cluster.is_some()
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
        let cols = self.cols();
        if width > cols {
            return;
        }
        let autowrap = self.modes.contains(Modes::AUTOWRAP);
        if self.cursor.pending_wrap && autowrap {
            self.wrap();
        }
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
                    grid.erase(r, 0..cols, blank);
                }
            }
            1 => {
                for r in 0..row {
                    grid.erase(r, 0..cols, blank);
                }
                grid.erase(row, 0..col + 1, blank);
            }
            2 => {
                for r in 0..rows {
                    grid.erase(r, 0..cols, blank);
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
        *self = Self::new(self.cols(), self.rows(), self.max_scrollback);
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
            1007 => Modes::ALTERNATE_SCROLL,
            47 | 1047 | 1049 => Modes::ALT_SCREEN,
            2004 => Modes::BRACKETED_PASTE,
            2026 => Modes::SYNC_OUTPUT,
            2031 => Modes::COLOR_SCHEME_UPDATES,
            _ => return None,
        };
        Some(self.modes.contains(flag))
    }

    fn sgr(&mut self, params: &Params) {
        let pen = &mut self.cursor.pen;
        let link = pen.link;
        if params.is_empty() {
            *pen = Cell { link, ..Cell::BLANK };
            return;
        }
        let mut groups: [&[u16]; MAX_PARAMS] = [&[]; MAX_PARAMS];
        let mut count = 0;
        for group in params.groups() {
            groups[count] = group;
            count += 1;
        }
        let groups = &groups[..count];

        let mut i = 0;
        while i < groups.len() {
            let group = groups[i];
            match group[0] {
                0 => *pen = Cell { link, ..Cell::BLANK },
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
                    if let Some((color, used)) = extended_color(group, &groups[i + 1..]) {
                        match group[0] {
                            38 => pen.fg = color,
                            48 => pen.bg = color,
                            _ => pen.underline_color = color,
                        }
                        i += used;
                    }
                }
                39 => pen.fg = Color::DEFAULT,
                n @ 40..=47 => pen.bg = Color::indexed((n - 40) as u8),
                49 => pen.bg = Color::DEFAULT,
                53 => pen.flags.insert(Flags::OVERLINE),
                55 => pen.flags.remove(Flags::OVERLINE),
                59 => pen.underline_color = Color::DEFAULT,
                n @ 90..=97 => pen.fg = Color::indexed((n - 90 + 8) as u8),
                n @ 100..=107 => pen.bg = Color::indexed((n - 100 + 8) as u8),
                n => log::debug!("unhandled SGR {n}"),
            }
            i += 1;
        }
    }

    fn respond(&mut self, bytes: &[u8]) {
        self.responses.extend_from_slice(bytes);
    }
}

/// Parses `38;5;n`, `38;2;r;g;b` and their colon forms.
/// Returns the color and how many following groups were consumed.
fn extended_color(group: &[u16], rest: &[&[u16]]) -> Option<(Color, usize)> {
    let byte = |v: u16| v.min(255) as u8;
    if group.len() > 1 {
        return match group[1] {
            5 => group.get(2).map(|&n| (Color::indexed(byte(n)), 0)),
            2 => {
                let values = &group[2..];
                let rgb = match values.len() {
                    3 => values,
                    n if n >= 4 => &values[1..4],
                    _ => return None,
                };
                Some((Color::rgb(byte(rgb[0]), byte(rgb[1]), byte(rgb[2])), 0))
            }
            _ => None,
        };
    }
    match rest.first().map(|g| g[0]) {
        Some(5) => rest.get(1).map(|g| (Color::indexed(byte(g[0])), 2)),
        Some(2) if rest.len() >= 4 => Some((Color::rgb(byte(rest[1][0]), byte(rest[2][0]), byte(rest[3][0])), 4)),
        _ => None,
    }
}

fn default_tabs(cols: usize) -> Vec<bool> {
    (0..cols).map(|c| c % 8 == 0).collect()
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
#[inline]
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
        let Some(width) = c.width() else { return };
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
            let pen = self.cursor.pen;
            let line = self.grids[self.active].row_mut(row);
            repair_wide(line, col, n);
            line.touch(col, end);
            for (cell, &b) in line.cells[col..col + n].iter_mut().zip(chunk) {
                *cell = Cell { ch: b as char, ..pen };
            }
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
            if autowrap && !self.cursor.pending_wrap && !(self.last_was_zwj && self.last_cluster.is_some()) {
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
                self.cursor.col = (n(0) - 1).min(self.cols() - 1);
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
            ([], b'c') if mode(0) == 0 => self.respond(b"\x1b[?62;22c"),
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
                let (rows, cols) = (self.rows(), self.cols());
                let fill = Cell { ch: 'E', ..Cell::BLANK };
                for r in 0..rows {
                    self.grids[self.active].erase(r, 0..cols, fill);
                }
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
                    self.cursor.pen.link = 0;
                } else {
                    let id = link_params
                        .split(|&b| b == b':')
                        .find_map(|p| p.strip_prefix(b"id="))
                        .map(|id| String::from_utf8_lossy(id).into_owned());
                    let uri = String::from_utf8_lossy(&uri).into_owned();
                    self.cursor.pen.link = self.intern_link(id, uri);
                }
            }
            [b"133", kind, ..] => match kind.first() {
                Some(b'A') if self.active == PRIMARY => {
                    self.prompt_line = Some(self.grids[PRIMARY].screen_line(self.cursor.row));
                }
                Some(b'C' | b'D') => self.prompt_line = None,
                _ => {}
            },
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

    fn hook(&mut self, _params: &Params, intermediates: &[u8], ignore: bool, action: u8) {
        self.dcs = match (intermediates, action, ignore) {
            ([b'$'], b'q', false) => Some(DcsRequest::Decrqss(Vec::new())),
            ([b'+'], b'q', false) => Some(DcsRequest::Xtgettcap(Vec::new())),
            _ => None,
        };
    }

    fn put(&mut self, byte: u8) {
        if let Some(DcsRequest::Decrqss(buffer) | DcsRequest::Xtgettcap(buffer)) = &mut self.dcs
            && buffer.len() < 4096
        {
            buffer.push(byte);
        }
    }

    fn unhook(&mut self) {
        match self.dcs.take() {
            Some(DcsRequest::Decrqss(request)) => self.decrqss(&request),
            Some(DcsRequest::Xtgettcap(request)) => self.xtgettcap(&request),
            None => {}
        }
    }

    fn apc_dispatch(&mut self, data: &[u8]) {
        let Some(payload) = data.strip_prefix(b"G") else { return };
        let grid = &self.grids[self.active];
        let ctx = graphics::Context {
            cursor_line: grid.screen_line(self.cursor.row),
            cursor_col: self.cursor.col,
            screen_top: grid.screen_line(0),
            rows: grid.rows(),
            cols: grid.cols(),
            cell_width: self.cell_pixels.0,
            cell_height: self.cell_pixels.1,
            alt_screen: self.active == ALTERNATE,
        };
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
        assert_eq!(t.take_responses().unwrap(), b"\x1b[3;4R\x1b[?62;22c");
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
    fn resize_during_command_output_still_reflows() {
        let mut t = term(10, 4, b"\x1b]133;A\x07$ \x1b]133;B\x07\x1b]133;C\x07\r\n0123456789abc");
        t.resize(20, 4);
        assert_eq!(line(&t, 0), "$");
        assert_eq!(line(&t, 1), "0123456789abc");
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
        let link = row.cells[0].link;
        assert_ne!(link, 0);
        assert_eq!(row.cells[3].link, link);
        assert_eq!(row.cells[5].link, 0);
        assert_eq!(t.hyperlink(link).unwrap().uri, "https://tron.dev");
        let found = t.link_at(t.viewport_point(0, 2)).unwrap();
        assert_eq!((found.start.col, found.end.col, found.uri.as_str()), (0, 3, "https://tron.dev"));
        let plain = term(30, 2, b"go to https://a.b/c, now");
        let found = plain.link_at(plain.viewport_point(0, 10)).unwrap();
        assert_eq!(found.uri, "https://a.b/c");
        assert_eq!(found.id, None);
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
