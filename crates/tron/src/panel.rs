//! The search box, the command palette and the update notice: small text
//! interfaces drawn with ratatui over the corners of the terminal.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget, Widget,
};
use tron_config::{BindKey, Binding, KeyCombo};
use tron_core::Palette;
use tron_render::Overlay;

use crate::menu::Item;
use crate::update;

/// Widest the search box grows, in cells.
const SEARCH_WIDTH: usize = 44;
/// Widest the command palette grows, in cells.
const PALETTE_WIDTH: usize = 56;
/// Commands listed at once; the list scrolls to the highlighted one.
const PALETTE_ROWS: usize = 10;

/// The command palette's query, highlighted command and list scroll position.
#[derive(Debug, Default)]
pub struct PaletteState {
    pub query: String,
    /// Index into [`PaletteState::entries`].
    pub selected: usize,
    /// Index of the first command shown in the list.
    pub first: usize,
}

impl PaletteState {
    /// The commands matching the query, best match first.
    pub fn entries(&self) -> Vec<Item> {
        let mut scored: Vec<(usize, Item)> = Item::ALL
            .into_iter()
            .filter(|&item| item != Item::CommandPalette)
            .filter_map(|item| Some((score(item.title(), &self.query)?, item)))
            .collect();
        // Stable: equally good matches keep the menu order.
        scored.sort_by_key(|&(score, _)| score);
        scored.into_iter().map(|(_, item)| item).collect()
    }

    pub fn selected_item(&self) -> Option<Item> {
        self.entries().get(self.selected).copied()
    }

    /// Moves the highlight by `delta` entries, wrapping around the ends, and scrolls
    /// the list of a window `rows` tall to keep it in view.
    pub fn move_selection(&mut self, delta: isize, rows: usize) {
        let count = self.entries().len();
        if count == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(count as isize) as usize;
        let shown = shown_rows(count, rows);
        if self.selected < self.first {
            self.first = self.selected;
        } else if self.selected >= self.first + shown {
            self.first = self.selected + 1 - shown;
        }
    }

    /// Scrolls the list of a window `rows` tall by `delta` entries, keeping the highlight.
    pub fn scroll(&mut self, delta: isize, rows: usize) {
        let count = self.entries().len();
        let last = count.saturating_sub(shown_rows(count, rows));
        self.first = self.first.saturating_add_signed(delta).min(last);
    }

    pub fn push_text(&mut self, text: &str) {
        self.query.extend(text.chars().filter(|c| !c.is_control()));
        self.selected = 0;
        self.first = 0;
    }

    pub fn pop_char(&mut self) {
        self.query.pop();
        self.selected = 0;
        self.first = 0;
    }
}

/// What a viewport cell is on the command palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteHit {
    /// The command at this index of [`PaletteState::entries`].
    Entry(usize),
    /// The palette's border, query or hints.
    Inside,
    Outside,
}

/// Commands listed at once for `count` matches in a window `rows` tall.
fn shown_rows(count: usize, rows: usize) -> usize {
    count.clamp(1, PALETTE_ROWS).min(rows.saturating_sub(6).max(1))
}

/// Where the command palette is drawn, in viewport cells.
struct PaletteLayout {
    top: usize,
    left: usize,
    width: usize,
    height: usize,
    /// Commands listed at once, and the first of them.
    shown: usize,
    first: usize,
    count: usize,
}

fn palette_layout(state: &PaletteState, cols: usize, rows: usize) -> Option<PaletteLayout> {
    let count = state.entries().len();
    let width = PALETTE_WIDTH.min(cols.saturating_sub(2));
    let shown = shown_rows(count, rows);
    // Borders, the query and the rule under it.
    let height = shown + 4;
    let (top, left) = place(cols, rows, width, height).filter(|_| width >= 24)?;
    let first = state.first.min(count.saturating_sub(shown));
    Some(PaletteLayout { top, left, width, height, shown, first, count })
}

/// What the viewport cell at `row` and `col` is on the command palette.
pub fn palette_hit(state: &PaletteState, cols: usize, rows: usize, row: usize, col: usize) -> PaletteHit {
    let Some(layout) = palette_layout(state, cols, rows) else { return PaletteHit::Outside };
    let (right, bottom) = (layout.left + layout.width, layout.top + layout.height);
    if !(layout.top..bottom).contains(&row) || !(layout.left..right).contains(&col) {
        return PaletteHit::Outside;
    }
    // Commands start below the top border, the query and the rule, and end at the right border.
    let list = layout.top + 3;
    if (list..list + layout.shown).contains(&row) && col + 1 < right && layout.first + row - list < layout.count {
        PaletteHit::Entry(layout.first + row - list)
    } else {
        PaletteHit::Inside
    }
}

/// How well `title` matches `query`, lower being better, or `None`. Titles that
/// contain the query rank by where it starts. Then come titles whose word
/// initials start with it ("cco" for Copy Command Output), and last titles that
/// have its letters in order, the closer together the better.
fn score(title: &str, query: &str) -> Option<usize> {
    let query: String = query.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    if query.is_empty() {
        return Some(0);
    }
    let title = title.to_lowercase();
    if let Some(at) = title.find(&query) {
        return Some(at);
    }
    let initials: String = title.split_whitespace().filter_map(|word| word.chars().next()).collect();
    if initials.starts_with(&query) {
        return Some(500);
    }
    let letters: Vec<char> = title.chars().collect();
    let (mut first, mut next) = (None, 0);
    for wanted in query.chars() {
        let found = next + letters[next..].iter().position(|&c| c == wanted)?;
        first.get_or_insert(found);
        next = found + 1;
    }
    Some(1000 + first.unwrap_or(0) + next)
}

/// The theme colors the boxes are drawn with.
struct Colors {
    fg: [u8; 3],
    bg: [u8; 3],
    accent: [u8; 3],
    dim: [u8; 3],
}

impl Colors {
    fn new(palette: &Palette) -> Self {
        Self { fg: palette.foreground, bg: palette.background, accent: palette.cursor, dim: palette.colors[8] }
    }
}

fn color([r, g, b]: [u8; 3]) -> Color {
    Color::Rgb(r, g, b)
}

/// The search box. `status` is shown in its bottom border.
pub fn search_overlays(query: &str, status: &str, cols: usize, rows: usize, palette: &Palette) -> Vec<Overlay> {
    let colors = Colors::new(palette);
    let width = SEARCH_WIDTH.min(cols.saturating_sub(2));
    let Some((top, left)) = place(cols, rows, width, 3).filter(|_| width >= 16) else { return Vec::new() };
    let mut buffer = Buffer::empty(Rect::new(0, 0, width as u16, 3));
    let block =
        frame(&colors, " Find ").title_bottom(Line::styled(status, Style::new().fg(color(colors.dim))).right_aligned());
    let inner = block.inner(buffer.area);
    block.render(buffer.area, &mut buffer);
    Paragraph::new(input_line(query, &colors)).render(inner, &mut buffer);
    overlays(&buffer, top, left, &colors)
}

/// The command palette: the query, then the matching commands with their shortcuts.
pub fn palette_overlays(
    state: &PaletteState,
    bindings: &[Binding],
    cols: usize,
    rows: usize,
    palette: &Palette,
) -> Vec<Overlay> {
    let colors = Colors::new(palette);
    let entries = state.entries();
    let Some(layout) = palette_layout(state, cols, rows) else { return Vec::new() };
    let PaletteLayout { top, left, width, height, shown, first, count } = layout;
    let mut buffer = Buffer::empty(Rect::new(0, 0, width as u16, height as u16));
    let hints = Line::styled(" ↑↓ select  ⏎ run  esc close ", Style::new().fg(color(colors.dim)));
    let block = frame(&colors, " Commands ").title_bottom(hints.right_aligned());
    let inner = block.inner(buffer.area);
    block.render(buffer.area, &mut buffer);

    let inner_width = usize::from(inner.width);
    let mut lines = vec![
        input_line(&state.query, &colors),
        Line::styled("─".repeat(inner_width), Style::new().fg(color(colors.dim))),
    ];
    if entries.is_empty() {
        lines.push(Line::styled(" no matching commands", Style::new().fg(color(colors.dim))));
    }
    for (index, item) in entries.iter().enumerate().skip(first).take(shown) {
        let title = item.title();
        let shortcut = item.shortcut(bindings).map(|(_, combo)| combo_label(combo)).unwrap_or_default();
        let gap = inner_width.saturating_sub(Span::raw(title).width() + Span::raw(&shortcut).width() + 2);
        let (fg, bg, key) = if index == state.selected {
            (colors.bg, colors.accent, colors.bg)
        } else {
            (colors.fg, colors.bg, colors.dim)
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {title}{}", " ".repeat(gap)), Style::new().fg(color(fg)).bg(color(bg))),
            Span::styled(format!("{shortcut} "), Style::new().fg(color(key)).bg(color(bg))),
        ]));
    }
    Paragraph::new(lines).render(inner, &mut buffer);
    // A scrollbar on the right border when the list is longer than the box.
    if count > shown {
        // Scaled so the thumb reaches the end when the last commands are shown.
        let position = first * (count - 1) / (count - shown);
        let mut state = ScrollbarState::new(count).position(position).viewport_content_length(shown);
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .track_style(Style::new().fg(color(colors.dim)))
            .thumb_symbol("┃")
            .thumb_style(Style::new().fg(color(colors.accent)))
            .render(Rect::new(0, 3, width as u16, shown as u16), &mut buffer, &mut state);
    }
    overlays(&buffer, top, left, &colors)
}

/// A button on the update notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeButton {
    /// Opens the release page.
    Open,
    Close,
    /// Stops telling about this release.
    Skip,
}

impl NoticeButton {
    const ALL: [Self; 3] = [Self::Open, Self::Close, Self::Skip];

    fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Close => "Close",
            Self::Skip => "Don't show again",
        }
    }

    /// Width of the button: its label with a space on each side.
    fn width(self) -> usize {
        self.label().len() + 2
    }
}

/// What a viewport cell is on the update notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeHit {
    Button(NoticeButton),
    /// The notice's border or message.
    Inside,
    Outside,
}

/// Where the update notice is drawn, in viewport cells: a cell in from the
/// bottom right corner, with the message above a row of buttons.
struct NoticeLayout {
    top: usize,
    left: usize,
    width: usize,
}

/// Rows of the update notice: borders, the message and the buttons.
const NOTICE_HEIGHT: usize = 4;

fn notice_message(version: &str) -> String {
    format!("tron {version} is out. You have {}.", update::CURRENT)
}

fn notice_layout(version: &str, cols: usize, rows: usize) -> Option<NoticeLayout> {
    let buttons: usize = NoticeButton::ALL.iter().map(|button| button.width() + 1).sum::<usize>() - 1;
    // Borders and a space inside each.
    let width = Span::raw(notice_message(version)).width().max(buttons) + 4;
    if cols < width || rows < NOTICE_HEIGHT {
        return None;
    }
    let top = rows - NOTICE_HEIGHT - usize::from(rows > NOTICE_HEIGHT);
    Some(NoticeLayout { top, left: cols - width - usize::from(cols > width), width })
}

/// What the viewport cell at `row` and `col` is on the notice about `version`.
pub fn notice_hit(version: &str, cols: usize, rows: usize, row: usize, col: usize) -> NoticeHit {
    let Some(NoticeLayout { top, left, width }) = notice_layout(version, cols, rows) else {
        return NoticeHit::Outside;
    };
    if !(top..top + NOTICE_HEIGHT).contains(&row) || !(left..left + width).contains(&col) {
        return NoticeHit::Outside;
    }
    if row == top + 2 {
        let mut start = left + 2;
        for button in NoticeButton::ALL {
            if (start..start + button.width()).contains(&col) {
                return NoticeHit::Button(button);
            }
            start += button.width() + 1;
        }
    }
    NoticeHit::Inside
}

/// The notice that tron `version` is out, with `hovered` highlighted.
pub fn notice_overlays(
    version: &str,
    hovered: Option<NoticeButton>,
    cols: usize,
    rows: usize,
    palette: &Palette,
) -> Vec<Overlay> {
    let colors = Colors::new(palette);
    let Some(NoticeLayout { top, left, width }) = notice_layout(version, cols, rows) else { return Vec::new() };
    let mut buffer = Buffer::empty(Rect::new(0, 0, width as u16, NOTICE_HEIGHT as u16));
    let block = frame(&colors, " Update ");
    let inner = block.inner(buffer.area);
    block.render(buffer.area, &mut buffer);
    let mut buttons = vec![Span::raw(" ")];
    for button in NoticeButton::ALL {
        let style = if hovered == Some(button) {
            Style::new().fg(color(colors.bg)).bg(color(colors.accent))
        } else if button == NoticeButton::Skip {
            Style::new().fg(color(colors.dim))
        } else {
            Style::new().fg(color(colors.accent))
        };
        buttons.push(Span::styled(format!(" {} ", button.label()), style));
        buttons.push(Span::raw(" "));
    }
    let lines = vec![Line::raw(format!(" {}", notice_message(version))), Line::from(buttons)];
    Paragraph::new(lines).render(inner, &mut buffer);
    overlays(&buffer, top, left, &colors)
}

/// A rounded box in the theme's colors.
fn frame<'a>(colors: &Colors, title: &'a str) -> Block<'a> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(color(colors.accent)))
        .title(Span::styled(title, Style::new().fg(color(colors.accent))))
        .style(Style::new().fg(color(colors.fg)).bg(color(colors.bg)))
}

/// The line being typed, with a bar for the cursor.
fn input_line<'a>(query: &'a str, colors: &Colors) -> Line<'a> {
    let accent = Style::new().fg(color(colors.accent));
    Line::from(vec![Span::styled("› ", accent), Span::raw(query), Span::styled("▏", accent)])
}

/// Where a box of `width` by `height` cells sits: a cell in from the top right
/// corner when there is room. `None` when it does not fit.
fn place(cols: usize, rows: usize, width: usize, height: usize) -> Option<(usize, usize)> {
    if width == 0 || cols < width || rows < height {
        return None;
    }
    Some((usize::from(rows > height), cols - width - usize::from(cols > width)))
}

fn shortcut_key(key: &BindKey) -> String {
    match key {
        BindKey::Char(' ') => "Space".to_owned(),
        BindKey::Char(c) => c.to_uppercase().collect(),
        BindKey::Named(name) => name
            .split('_')
            .map(|word| {
                let mut chars = word.chars();
                chars.next().map(|first| first.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// A shortcut as macOS menus write it, such as ⇧⌘P.
#[cfg(target_os = "macos")]
fn combo_label(combo: &KeyCombo) -> String {
    let mut label = String::new();
    for (held, symbol) in [(combo.ctrl, '⌃'), (combo.alt, '⌥'), (combo.shift, '⇧'), (combo.super_key, '⌘')] {
        if held {
            label.push(symbol);
        }
    }
    label + &shortcut_key(&combo.key)
}

/// A shortcut such as Ctrl+Shift+P.
#[cfg(not(target_os = "macos"))]
fn combo_label(combo: &KeyCombo) -> String {
    let mut parts = Vec::new();
    for (held, name) in [(combo.ctrl, "Ctrl"), (combo.alt, "Alt"), (combo.shift, "Shift"), (combo.super_key, "Super")] {
        if held {
            parts.push(name.to_owned());
        }
    }
    parts.push(shortcut_key(&combo.key));
    parts.join("+")
}

/// The drawn cells as overlays, one per run of cells with the same colors.
fn overlays(buffer: &Buffer, top: usize, left: usize, colors: &Colors) -> Vec<Overlay> {
    let rgb = |c: Color, default: [u8; 3]| match c {
        Color::Rgb(r, g, b) => [r, g, b],
        _ => default,
    };
    let mut runs = Vec::new();
    let area = buffer.area;
    for y in 0..area.height {
        let mut run: Option<Overlay> = None;
        let mut x = 0;
        while x < area.width {
            let cell = &buffer[(x, y)];
            let symbol = cell.symbol();
            let (fg, bg) = (rgb(cell.fg, colors.fg), rgb(cell.bg, colors.bg));
            match &mut run {
                Some(current) if current.fg == fg && current.bg == bg => current.text.push_str(symbol),
                _ => {
                    runs.extend(run.take());
                    run = Some(Overlay {
                        row: top + usize::from(y),
                        col: left + usize::from(x),
                        text: symbol.to_owned(),
                        fg,
                        bg,
                        underline: false,
                    });
                }
            }
            // A wide character covers the next cell, which ratatui leaves blank.
            x += Span::raw(symbol).width().max(1) as u16;
        }
        runs.extend(run);
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette_with(query: &str) -> PaletteState {
        PaletteState { query: query.to_owned(), ..PaletteState::default() }
    }

    #[test]
    fn palette_ranks_contained_queries_first() {
        let entries = palette_with("copy").entries();
        assert_eq!(entries[..2], [Item::Copy, Item::CopyCommandOutput]);
        let initials = palette_with("cco").entries();
        assert_eq!(initials.first(), Some(&Item::CopyCommandOutput));
        assert!(initials.contains(&Item::SelectCommandOutput), "letters in order match too");
        assert!(palette_with("zzz").entries().is_empty());
        let all = palette_with("").entries();
        assert!(!all.contains(&Item::CommandPalette));
        assert_eq!(all.len(), Item::ALL.len() - 1);
    }

    #[test]
    fn palette_selection_wraps() {
        let mut state = palette_with("scroll to");
        let count = state.entries().len();
        state.move_selection(-1, 24);
        assert_eq!(state.selected, count - 1);
        state.move_selection(1, 24);
        assert_eq!(state.selected, 0);
        state.push_text("p");
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn keys_keep_the_highlight_in_view_and_the_wheel_scrolls() {
        let mut state = palette_with("");
        let count = state.entries().len();
        assert!(count > PALETTE_ROWS);
        state.move_selection(-1, 24);
        assert_eq!(state.first, count - PALETTE_ROWS, "wrapping to the last command shows it");
        state.move_selection(1, 24);
        assert_eq!(state.first, 0);
        state.scroll(3, 24);
        assert_eq!((state.first, state.selected), (3, 0), "the wheel keeps the highlight");
        state.scroll(100, 24);
        assert_eq!(state.first, count - PALETTE_ROWS);
        state.scroll(-100, 24);
        assert_eq!(state.first, 0);
    }

    #[test]
    fn pointer_hits_commands_in_the_list() {
        let mut state = palette_with("");
        let (top, left) = (1, 80 - PALETTE_WIDTH - 1);
        let list = top + 3;
        assert_eq!(palette_hit(&state, 80, 24, list, left + 5), PaletteHit::Entry(0));
        assert_eq!(palette_hit(&state, 80, 24, list + 2, left + 5), PaletteHit::Entry(2));
        assert_eq!(palette_hit(&state, 80, 24, top + 1, left + 5), PaletteHit::Inside, "the query line");
        assert_eq!(palette_hit(&state, 80, 24, list, left + PALETTE_WIDTH - 1), PaletteHit::Inside, "the border");
        assert_eq!(palette_hit(&state, 80, 24, list, left - 1), PaletteHit::Outside);
        assert_eq!(palette_hit(&state, 80, 24, 20, left + 5), PaletteHit::Outside);
        state.scroll(4, 24);
        assert_eq!(palette_hit(&state, 80, 24, list, left + 5), PaletteHit::Entry(4));
    }

    #[test]
    fn long_lists_get_a_scrollbar() {
        let palette = Palette::default();
        let right = 80 - 2;
        let thumb_rows = |state: &PaletteState| -> Vec<usize> {
            palette_overlays(state, &[], 80, 24, &palette)
                .iter()
                .filter(|o| o.text.contains('┃') && o.col + Span::raw(&o.text).width() - 1 == right)
                .map(|o| o.row)
                .collect()
        };
        let mut state = palette_with("");
        let at_top = thumb_rows(&state);
        assert!(at_top.contains(&4), "thumb at the top: {at_top:?}");
        state.scroll(100, 24);
        let at_end = thumb_rows(&state);
        assert!(at_end.contains(&(4 + PALETTE_ROWS - 1)), "thumb at the end: {at_end:?}");
        assert!(thumb_rows(&palette_with("copy")).is_empty(), "short lists have none");
    }

    #[test]
    fn update_notice_sits_at_the_bottom_right_with_buttons() {
        let palette = Palette::default();
        let version = "9.9.9";
        let notice = notice_overlays(version, Some(NoticeButton::Close), 80, 24, &palette);
        let bottom = notice.iter().map(|o| o.row).max().unwrap();
        assert_eq!(bottom, 22);
        let right = notice.iter().map(|o| o.col + Span::raw(&o.text).width()).max().unwrap();
        assert_eq!(right, 79);
        assert!(notice.iter().any(|o| o.text.contains("tron 9.9.9 is out")));
        let close = notice.iter().find(|o| o.text == " Close ").expect("Close is highlighted on its own");
        assert_eq!(close.bg, palette.cursor);

        let hit = |row, col| notice_hit(version, 80, 24, row, col);
        let row = close.row;
        assert_eq!(hit(row, close.col), NoticeHit::Button(NoticeButton::Close));
        assert_eq!(hit(row, close.col + 6), NoticeHit::Button(NoticeButton::Close));
        assert_eq!(hit(row, close.col + 7), NoticeHit::Inside, "the gap between buttons");
        assert_eq!(hit(row, close.col - 2), NoticeHit::Button(NoticeButton::Open));
        assert_eq!(hit(row, close.col + 8), NoticeHit::Button(NoticeButton::Skip));
        assert_eq!(hit(row - 1, close.col), NoticeHit::Inside, "the message");
        assert_eq!(hit(row, 10), NoticeHit::Outside);
        assert!(notice_overlays(version, None, 20, 24, &palette).is_empty(), "too narrow");
    }

    #[test]
    fn boxes_sit_at_the_top_right() {
        let palette = Palette::default();
        let search = search_overlays("tron", " no matches ", 80, 24, &palette);
        assert_eq!((search[0].row, search[0].col), (1, 80 - SEARCH_WIDTH - 1));
        assert!(search[0].text.starts_with('╭'));
        for row in 1..=3 {
            let width: usize = search.iter().filter(|o| o.row == row).map(|o| Span::raw(&o.text).width()).sum();
            assert_eq!(width, SEARCH_WIDTH, "row {row}");
        }
        assert!(search.iter().any(|o| o.text.contains("tron")));

        let commands = palette_overlays(&palette_with(""), &[], 80, 24, &palette);
        let bottom = commands.iter().map(|o| o.row).max().unwrap();
        assert_eq!(bottom, 1 + PALETTE_ROWS + 3);
        assert!(search_overlays("", "", 10, 24, &palette).is_empty());
    }
}
