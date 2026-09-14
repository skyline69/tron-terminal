//! The Credits tab: who made the themes, shaders and libraries tron ships with.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::ui::{CYAN, DIM, MAGENTA, TEXT};

const CREDITS: &str = include_str!("credits.toml");

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Credit {
    pub title: String,
    /// What it is: Themes, Shader, Cursor shader, Library.
    pub kind: String,
    pub author: Option<String>,
    pub author_url: Option<String>,
    pub source_url: String,
    /// Where tron's WGSL version was ported from, when that differs from the source.
    pub port_url: Option<String>,
    /// The license as stated by the author. `None` when none is stated.
    pub license: Option<String>,
    pub note: Option<String>,
}

/// Every credit, in file order.
pub fn all() -> Vec<Credit> {
    let table: toml::Table = toml::from_str(CREDITS).unwrap_or_default();
    table
        .get("credit")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let entry = entry.as_table()?;
            let text = |key: &str| entry.get(key).and_then(toml::Value::as_str).map(str::to_owned);
            Some(Credit {
                title: text("title")?,
                kind: text("kind").unwrap_or_default(),
                author: text("author"),
                author_url: text("author_url"),
                source_url: text("source_url")?,
                port_url: text("port_url"),
                license: text("license"),
                note: text("note"),
            })
        })
        .collect()
}

/// Everything known about `credit`, next to the list.
pub fn draw_details(frame: &mut Frame, area: Rect, credit: &Credit) {
    let label = Style::new().fg(DIM);
    let value = Style::new().fg(TEXT);
    let field = |name: &'static str, text: &str| {
        Line::from(vec![Span::styled(format!("{name:<9}"), label), Span::styled(text.to_owned(), value)])
    };
    let mut lines = vec![
        Line::styled(credit.title.clone(), Style::new().fg(CYAN).add_modifier(Modifier::BOLD)),
        Line::styled(credit.kind.clone(), label),
        Line::raw(""),
    ];
    if let Some(note) = &credit.note {
        lines.push(Line::styled(note.clone(), value));
        lines.push(Line::raw(""));
    }
    if let Some(author) = &credit.author {
        lines.push(field("By", author));
    }
    if let Some(url) = &credit.author_url {
        lines.push(field("Profile", url));
    }
    lines.push(field("Source", &credit.source_url));
    if let Some(url) = &credit.port_url {
        lines.push(field("Ported", url));
    }
    lines.push(field("License", credit.license.as_deref().unwrap_or("none stated")));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("Enter", Style::new().fg(MAGENTA).add_modifier(Modifier::BOLD)),
        Span::styled(" opens the source", label),
    ]));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_credit_has_a_title_kind_and_source() {
        let credits = all();
        assert!(credits.len() >= 7);
        for credit in &credits {
            assert!(!credit.kind.is_empty() && credit.source_url.starts_with("https://"), "{credit:?}");
        }
    }
}
