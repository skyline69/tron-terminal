//! Colors and small layout helpers shared by the startup screen's views.

use ratatui::layout::Rect;
use ratatui::style::Color;

pub const CYAN: Color = Color::Rgb(0x4f, 0xd6, 0xff);
pub const MAGENTA: Color = Color::Rgb(0xc3, 0x8b, 0xff);
pub const TEXT: Color = Color::Rgb(0xc7, 0xd5, 0xe0);
pub const DIM: Color = Color::Rgb(0x6a, 0x75, 0x88);
/// Color text fades from and to. Close to tron's default background.
pub const DARK: Color = Color::Rgb(0x0a, 0x0e, 0x14);

/// `area` narrowed to `width` columns in the middle.
pub fn centered(area: Rect, width: usize) -> Rect {
    let width = (width as u16).min(area.width);
    Rect { x: area.x + (area.width - width) / 2, width, ..area }
}
