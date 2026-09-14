//! Color table used to resolve SGR colors. Applications can change it with OSC 4/10/11/12.

use crate::cell::{Color, ColorKind};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Palette {
    pub colors: [[u8; 3]; 256],
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub cursor: [u8; 3],
}

impl Default for Palette {
    fn default() -> Self {
        const ANSI: [[u8; 3]; 16] = [
            [0, 0, 0],
            [205, 0, 0],
            [0, 205, 0],
            [205, 205, 0],
            [0, 0, 238],
            [205, 0, 205],
            [0, 205, 205],
            [229, 229, 229],
            [127, 127, 127],
            [255, 0, 0],
            [0, 255, 0],
            [255, 255, 0],
            [92, 92, 255],
            [255, 0, 255],
            [0, 255, 255],
            [255, 255, 255],
        ];
        Self::from_ansi([229, 229, 229], [0, 0, 0], [229, 229, 229], ANSI)
    }
}

impl Palette {
    /// Builds the xterm 256 color table from the 16 ANSI colors.
    pub fn from_ansi(foreground: [u8; 3], background: [u8; 3], cursor: [u8; 3], ansi: [[u8; 3]; 16]) -> Self {
        let mut colors = [[0u8; 3]; 256];
        colors[..16].copy_from_slice(&ansi);
        const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        for i in 0..216 {
            colors[16 + i] = [STEPS[i / 36], STEPS[(i / 6) % 6], STEPS[i % 6]];
        }
        for i in 0..24 {
            let v = 8 + 10 * i as u8;
            colors[232 + i] = [v, v, v];
        }
        Self { colors, foreground, background, cursor }
    }

    #[inline]
    pub fn resolve(&self, color: Color, default: [u8; 3]) -> [u8; 3] {
        match color.kind() {
            ColorKind::Default => default,
            ColorKind::Indexed(i) => self.colors[usize::from(i)],
            ColorKind::Rgb(r, g, b) => [r, g, b],
        }
    }
}

/// Parses X11 color specs: `rgb:r/g/b` (1 to 4 hex digits per channel) and `#rgb` forms.
pub fn parse_color_spec(spec: &[u8]) -> Option<[u8; 3]> {
    let spec = std::str::from_utf8(spec).ok()?;
    if let Some(rest) = spec.strip_prefix("rgb:") {
        let mut channels = rest.split('/');
        let mut out = [0u8; 3];
        for slot in &mut out {
            let part = channels.next()?;
            if part.is_empty() || part.len() > 4 {
                return None;
            }
            let value = u32::from_str_radix(part, 16).ok()?;
            let max = (1u32 << (4 * part.len())) - 1;
            *slot = (value * 255 / max) as u8;
        }
        return channels.next().is_none().then_some(out);
    }
    let hex = spec.strip_prefix('#')?;
    if hex.is_empty() || hex.len() % 3 != 0 || hex.len() > 12 {
        return None;
    }
    let digits = hex.len() / 3;
    let mut out = [0u8; 3];
    for (i, slot) in out.iter_mut().enumerate() {
        let value = u32::from_str_radix(&hex[i * digits..(i + 1) * digits], 16).ok()?;
        *slot = if digits == 1 { (value * 17) as u8 } else { (value >> (4 * digits - 8)) as u8 };
    }
    Some(out)
}

/// Formats a color as an X11 `rgb:rrrr/gggg/bbbb` spec.
pub fn format_color_spec(color: [u8; 3]) -> String {
    let [r, g, b] = color.map(|c| u16::from(c) * 257);
    format!("rgb:{r:04x}/{g:04x}/{b:04x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_color_specs() {
        assert_eq!(parse_color_spec(b"rgb:ff/80/00"), Some([255, 128, 0]));
        assert_eq!(parse_color_spec(b"rgb:ffff/0000/8080"), Some([255, 0, 128]));
        assert_eq!(parse_color_spec(b"rgb:f/0/8"), Some([255, 0, 136]));
        assert_eq!(parse_color_spec(b"#ff8000"), Some([255, 128, 0]));
        assert_eq!(parse_color_spec(b"#f80"), Some([255, 136, 0]));
        assert_eq!(parse_color_spec(b"red"), None);
        assert_eq!(format_color_spec([255, 0, 128]), "rgb:ffff/0000/8080");
    }
}
