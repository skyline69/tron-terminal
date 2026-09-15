//! The search box and the command palette: small text interfaces drawn with
//! ratatui over the top right corner of the terminal.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph, Widget};
use tron_config::{BindKey, Binding, KeyCombo};
use tron_core::Palette;
use tron_render::Overlay;

use crate::menu::Item;

/// Widest the search box grows, in cells.
const SEARCH_WIDTH: usize = 44;
/// Widest the command palette grows, in cells.
const PALETTE_WIDTH: usize = 56;
/// Commands listed at once; the list scrolls to the highlighted one.
const PALETTE_ROWS: usize = 10;

/// The command palette's query and highlighted command.
#[derive(Debug, Default)]
pub struct PaletteState {
    pub query: String,
    /// Index into [`PaletteState::entries`].
    pub selected: usize,
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

    /// Moves the highlight by `delta` entries, wrapping around the ends.
    pub fn move_selection(&mut self, delta: isize) {
        let count = self.entries().len();
        if count > 0 {
            self.selected = (self.selected as isize + delta).rem_euclid(count as isize) as usize;
        }
    }

    pub fn push_text(&mut self, text: &str) {
        self.query.extend(text.chars().filter(|c| !c.is_control()));
        self.selected = 0;
    }

    pub fn pop_char(&mut self) {
        self.query.pop();
        self.selected = 0;
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
    let width = PALETTE_WIDTH.min(cols.saturating_sub(2));
    let shown = entries.len().clamp(1, PALETTE_ROWS).min(rows.saturating_sub(6).max(1));
    // Borders, the query and the rule under it.
    let height = shown + 4;
    let Some((top, left)) = place(cols, rows, width, height).filter(|_| width >= 24) else { return Vec::new() };
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
    let first = state.selected.saturating_sub(shown - 1);
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
        PaletteState { query: query.to_owned(), selected: 0 }
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
        state.move_selection(-1);
        assert_eq!(state.selected, count - 1);
        state.move_selection(1);
        assert_eq!(state.selected, 0);
        state.push_text("p");
        assert_eq!(state.selected, 0);
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
