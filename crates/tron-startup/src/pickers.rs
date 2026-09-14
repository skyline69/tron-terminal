//! The Themes and Shaders tabs: scrolling lists whose highlighted entry tron
//! previews live, next to a panel describing it.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use tron_config::Rgb;

use crate::catalog::{Motion, Shader, Theme};
use crate::ui::{CYAN, DIM, MAGENTA, TEXT};

/// Selection and scroll position of a list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Picker {
    pub selected: usize,
    offset: usize,
}

impl Picker {
    pub fn new(selected: usize) -> Self {
        Self { selected, offset: 0 }
    }

    /// Moves the selection, clamped to the list. Returns whether it changed.
    pub fn move_by(&mut self, delta: isize, len: usize) -> bool {
        let target = (self.selected as isize + delta).clamp(0, len.saturating_sub(1) as isize) as usize;
        self.set(target, len)
    }

    pub fn set(&mut self, index: usize, len: usize) -> bool {
        let index = index.min(len.saturating_sub(1));
        let changed = index != self.selected;
        self.selected = index;
        changed
    }

    /// Scrolls so the selection is visible in `height` rows. Returns the first visible row.
    pub fn scroll(&mut self, height: usize) -> usize {
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if height > 0 && self.selected >= self.offset + height {
            self.offset = self.selected + 1 - height;
        }
        self.offset
    }
}

/// One list row: name, a short tag after it, and whether it is chosen.
pub struct Item<'a> {
    pub name: &'a str,
    pub tag: &'a str,
    pub chosen: bool,
}

/// Draws a list and returns the screen area of every visible row, by index.
pub fn draw_list(
    frame: &mut Frame,
    area: Rect,
    items: &[Item<'_>],
    picker: &mut Picker,
    hover: Option<usize>,
    focused: bool,
) -> Vec<(Rect, usize)> {
    let height = usize::from(area.height);
    picker.scroll(height);
    let mut rows = Vec::new();
    for (row, index) in (picker.offset..items.len()).take(height).enumerate() {
        let item = &items[index];
        let selected = index == picker.selected;
        let marker = if item.chosen { Span::styled("● ", Style::new().fg(MAGENTA)) } else { Span::raw("  ") };
        let pointer = if selected { Span::styled("› ", Style::new().fg(CYAN)) } else { Span::raw("  ") };
        let name_style = match (selected, hover == Some(index)) {
            (true, _) if focused => Style::new().fg(CYAN).add_modifier(Modifier::BOLD),
            (true, _) => Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
            (false, true) => Style::new().fg(TEXT).add_modifier(Modifier::UNDERLINED),
            (false, false) => Style::new().fg(TEXT),
        };
        let line = Line::from(vec![
            pointer,
            marker,
            Span::styled(item.name, name_style),
            Span::styled(format!("  {}", item.tag), Style::new().fg(DIM)),
        ]);
        let row_area = Rect::new(area.x, area.y + row as u16, area.width, 1);
        frame.render_widget(Paragraph::new(line), row_area);
        rows.push((row_area, index));
    }
    // Arrows show that the list continues.
    let arrow = Style::new().fg(DIM);
    if picker.offset > 0 {
        frame.render_widget(Paragraph::new(Line::styled("↑", arrow)).right_aligned(), Rect { height: 1, ..area });
    }
    if picker.offset + height < items.len() {
        let bottom = Rect { y: area.bottom().saturating_sub(1), height: 1, ..area };
        frame.render_widget(Paragraph::new(Line::styled("↓", arrow)).right_aligned(), bottom);
    }
    rows
}

fn rgb(color: Rgb) -> Color {
    Color::Rgb(color.r, color.g, color.b)
}

/// Palette swatches and a sample terminal session in the theme's colors.
pub fn draw_theme_preview(frame: &mut Frame, area: Rect, theme: &Theme, chosen: bool) {
    let colors = &theme.colors;
    let [title, _, normal, bright, _, sample] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(area);
    let status = if chosen {
        Span::styled("  ● chosen", Style::new().fg(MAGENTA))
    } else {
        Span::styled("  Enter to choose", Style::new().fg(DIM))
    };
    let title_line = Line::from(vec![
        Span::styled(theme.name.as_str(), Style::new().fg(CYAN).add_modifier(Modifier::BOLD)),
        Span::styled(if theme.builtin { "  built in" } else { "  from themes/" }, Style::new().fg(DIM)),
        status,
    ]);
    frame.render_widget(Paragraph::new(title_line), title);
    for (row, palette) in [(normal, &colors.normal), (bright, &colors.bright)] {
        let swatches: Vec<Span> = palette
            .iter()
            .flat_map(|&color| [Span::styled("    ", Style::new().bg(rgb(color))), Span::raw(" ")])
            .collect();
        frame.render_widget(Paragraph::new(Line::from(swatches)), row);
    }

    let [fg, bg] = [rgb(colors.foreground), rgb(colors.background)];
    let ansi = |index: usize| Style::new().fg(rgb(colors.normal[index]));
    let plain = Style::new().fg(fg);
    let lines = vec![
        Line::raw(""),
        Line::from(vec![
            Span::styled(" skyline", ansi(2)),
            Span::styled("@", plain),
            Span::styled("fedora ", ansi(2)),
            Span::styled("~/projects/tron", ansi(4)),
            Span::styled(" (main)", ansi(5)),
            Span::styled("> ", plain),
            Span::styled("cargo build", plain.add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled("    Compiling", ansi(2).add_modifier(Modifier::BOLD)),
            Span::styled(" tron v0.1.0", plain),
        ]),
        Line::from(vec![
            Span::styled(" warning", ansi(3).add_modifier(Modifier::BOLD)),
            Span::styled(": unused variable `frame`", plain),
        ]),
        Line::from(vec![
            Span::styled(" error", ansi(1).add_modifier(Modifier::BOLD)),
            Span::styled("[E0425]: cannot find value `grid`", plain),
        ]),
        Line::from(vec![
            Span::styled(" src/", ansi(4).add_modifier(Modifier::BOLD)),
            Span::styled("  Cargo.toml  ", plain),
            Span::styled("build.sh", ansi(2)),
            Span::styled("  README.md", plain),
        ]),
        Line::from(vec![
            Span::styled(" ", plain),
            Span::styled(
                "selected text",
                Style::new().bg(rgb(colors.selection_background)).fg(colors.selection_foreground.map_or(fg, rgb)),
            ),
            Span::styled("  ", plain),
            Span::styled(" ", Style::new().bg(rgb(colors.cursor))),
        ]),
    ];
    let block = Block::new().style(Style::new().bg(bg).fg(fg));
    frame.render_widget(Paragraph::new(lines).block(block), sample);
}

/// What a shader does and where it runs in the chain.
pub fn draw_shader_details(
    frame: &mut Frame,
    area: Rect,
    shader: &Shader,
    position: Option<usize>,
    enabled: &[String],
) {
    let label = Style::new().fg(DIM);
    let value = Style::new().fg(TEXT);
    let field = |name: &'static str, text: String| {
        Line::from(vec![Span::styled(format!("{name:<11}"), label), Span::styled(text, value)])
    };
    let state = match position {
        Some(index) => Span::styled(format!("  ● on, runs {}", ordinal(index + 1)), Style::new().fg(MAGENTA)),
        None => Span::styled("  Space to turn on", label),
    };
    let motion = match shader.motion {
        Motion::EveryFrame => "every frame",
        Motion::AfterCursorMoves => "for a moment after the cursor moves",
        Motion::OnChanges => "only when the screen changes",
    };
    let chain = if enabled.is_empty() { "none".to_owned() } else { enabled.join(" → ") };
    let lines = vec![
        Line::from(vec![Span::styled(shader.file.as_str(), Style::new().fg(CYAN).add_modifier(Modifier::BOLD)), state]),
        Line::raw(""),
        Line::styled(if shader.description.is_empty() { "No description." } else { &shader.description }, value),
        Line::raw(""),
        field("Source", if shader.builtin { "built in".to_owned() } else { "shaders/".to_owned() + &shader.file }),
        field("Redraws", motion.to_owned()),
        field("Chain", chain),
        Line::raw(""),
        Line::styled("The window shows the highlighted shader on top of the ones already on.", label),
        Line::styled("The startup effects pause on this tab so you see shaders on their own.", label),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn ordinal(n: usize) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_clamps_and_scrolls() {
        let mut picker = Picker::new(0);
        assert!(!picker.move_by(-1, 5));
        assert!(picker.move_by(10, 5));
        assert_eq!(picker.selected, 4);
        picker.scroll(2);
        assert_eq!(picker.offset, 3);
        picker.set(0, 5);
        picker.scroll(2);
        assert_eq!(picker.offset, 0);
    }

    #[test]
    fn ordinals() {
        assert_eq!(ordinal(2), "2nd");
        assert_eq!(ordinal(12), "12th");
    }
}
