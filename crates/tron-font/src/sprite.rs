//! Box drawing, block element and Powerline glyphs drawn from geometry.
//!
//! Font glyphs for these characters rarely line up with cell edges, which leaves
//! gaps in TUI borders. Drawing them per cell size makes them seamless.

use crate::{GlyphFormat, RasterizedGlyph};

/// Line weights (up, right, down, left) for U+2500..=U+254B. 1 light, 2 heavy.
const LINES: [&str; 76] = [
    "0101", "0202", "1010", "2020", "0000", "0000", "0000", "0000", "0000", "0000", "0000", "0000", "0110", "0210",
    "0120", "0220", "0011", "0012", "0021", "0022", "1100", "1200", "2100", "2200", "1001", "1002", "2001", "2002",
    "1110", "1210", "2110", "1120", "2120", "2210", "1220", "2220", "1011", "1012", "2011", "1021", "2021", "2012",
    "1022", "2022", "0111", "0112", "0211", "0212", "0121", "0122", "0221", "0222", "1101", "1102", "1201", "1202",
    "2101", "2102", "2201", "2202", "1111", "1112", "1211", "1212", "2111", "1121", "2121", "2112", "2211", "1122",
    "1221", "2212", "1222", "2122", "2221", "2222",
];

/// Weights for U+2550..=U+256C. 3 is a double line.
const DOUBLE_LINES: [&str; 29] = [
    "0303", "3030", "0310", "0130", "0330", "0013", "0031", "0033", "1300", "3100", "3300", "1003", "3001", "3003",
    "1310", "3130", "3330", "1013", "3031", "3033", "0313", "0131", "0333", "1303", "3101", "3303", "1313", "3131",
    "3333",
];

/// Weights for U+2574..=U+257F.
const HALF_LINES: [&str; 12] =
    ["0001", "1000", "0100", "0010", "0002", "2000", "0200", "0020", "0201", "1020", "0102", "2010"];

pub fn is_sprite(c: char) -> bool {
    matches!(u32::from(c), 0x2500..=0x259F | 0xE0B0..=0xE0B4 | 0xE0B6)
}

/// Draws `c` into a `width` x `height` mask. `thickness` is the light stroke width.
pub fn render(c: char, width: u32, height: u32, baseline: u32, thickness: u32) -> Option<RasterizedGlyph> {
    let mut canvas = Canvas::new(width.max(1), height.max(1));
    let light = thickness.max(1) as i32;
    let cp = u32::from(c);
    match cp {
        0x2504..=0x250B => {
            let index = cp - 0x2504;
            let count = if index < 4 { 3 } else { 4 };
            canvas.dashes(count, index % 2 == 1, index % 4 >= 2, light);
        }
        0x254C..=0x254F => {
            let index = cp - 0x254C;
            canvas.dashes(2, index % 2 == 1, index >= 2, light);
        }
        0x2500..=0x254B => canvas.lines(weights(LINES[(cp - 0x2500) as usize]), light),
        0x2550..=0x256C => canvas.lines(weights(DOUBLE_LINES[(cp - 0x2550) as usize]), light),
        0x2574..=0x257F => canvas.lines(weights(HALF_LINES[(cp - 0x2574) as usize]), light),
        0x256D..=0x2570 => canvas.rounded(cp, light),
        0x2571..=0x2573 => canvas.diagonal(cp, light),
        0x2580..=0x259F => canvas.block(cp),
        0xE0B0..=0xE0B4 | 0xE0B6 => canvas.powerline(cp, light),
        _ => return None,
    }
    Some(RasterizedGlyph {
        format: GlyphFormat::Mask,
        width: canvas.width,
        height: canvas.height,
        left: 0,
        top: baseline as i32,
        data: canvas.data,
    })
}

fn weights(spec: &str) -> [u8; 4] {
    let b = spec.as_bytes();
    [b[0] - b'0', b[1] - b'0', b[2] - b'0', b[3] - b'0']
}

struct Canvas {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

impl Canvas {
    fn new(width: u32, height: u32) -> Self {
        Self { width, height, data: vec![0; (width * height) as usize] }
    }

    fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, alpha: u8) {
        let (w, h) = (self.width as i32, self.height as i32);
        for y in y0.clamp(0, h)..y1.clamp(0, h) {
            let row = (y * w) as usize;
            for x in x0.clamp(0, w)..x1.clamp(0, w) {
                let pixel = &mut self.data[row + x as usize];
                *pixel = (*pixel).max(alpha);
            }
        }
    }

    /// Fills pixels by 4x4 supersampling an inside test in pixel coordinates.
    fn sample(&mut self, inside: impl Fn(f32, f32) -> bool) {
        for y in 0..self.height {
            for x in 0..self.width {
                let mut hits = 0u32;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let px = x as f32 + (sx as f32 + 0.5) / 4.0;
                        let py = y as f32 + (sy as f32 + 0.5) / 4.0;
                        hits += u32::from(inside(px, py));
                    }
                }
                if hits > 0 {
                    let pixel = &mut self.data[(y * self.width + x) as usize];
                    *pixel = (*pixel).max((hits * 255 / 16) as u8);
                }
            }
        }
    }

    fn lines(&mut self, [up, right, down, left]: [u8; 4], light: i32) {
        let size = |weight: u8| match weight {
            1 => light,
            2 => light * 2,
            3 => light * 3,
            _ => 0,
        };
        let (w, h) = (self.width as i32, self.height as i32);
        let (cx, cy) = (w / 2, h / 2);
        let vertical = size(up).max(size(down));
        let horizontal = size(left).max(size(right));
        // Offsets of the strokes that make up one line, relative to the center.
        let bands = |weight: u8| -> Vec<(i32, i32)> {
            let s = size(weight);
            let start = -(s / 2);
            if weight == 3 {
                vec![(start, start + light), (start + 2 * light, start + s)]
            } else {
                vec![(start, start + s)]
            }
        };
        if up > 0 {
            for (a, b) in bands(up) {
                self.rect(cx + a, 0, cx + b, cy - horizontal / 2 + horizontal, 255);
            }
        }
        if down > 0 {
            for (a, b) in bands(down) {
                self.rect(cx + a, cy - horizontal / 2, cx + b, h, 255);
            }
        }
        if left > 0 {
            for (a, b) in bands(left) {
                self.rect(0, cy + a, cx - vertical / 2 + vertical, cy + b, 255);
            }
        }
        if right > 0 {
            for (a, b) in bands(right) {
                self.rect(cx - vertical / 2, cy + a, w, cy + b, 255);
            }
        }
    }

    fn dashes(&mut self, count: i32, heavy: bool, vertical: bool, light: i32) {
        let stroke = if heavy { light * 2 } else { light };
        let (w, h) = (self.width as i32, self.height as i32);
        let length = if vertical { h } else { w };
        let segment = (length / count).max(1);
        let gap = (segment / 3).max(1);
        for i in 0..count {
            let start = i * segment + gap / 2;
            let end = start + segment - gap;
            if vertical {
                self.rect(w / 2 - stroke / 2, start, w / 2 - stroke / 2 + stroke, end, 255);
            } else {
                self.rect(start, h / 2 - stroke / 2, end, h / 2 - stroke / 2 + stroke, 255);
            }
        }
    }

    fn rounded(&mut self, cp: u32, light: i32) {
        let (w, h) = (self.width as f32, self.height as f32);
        let t = light as f32;
        // Center line of the strokes.
        let lx = (self.width as i32 / 2 - light / 2) as f32 + t / 2.0;
        let ly = (self.height as i32 / 2 - light / 2) as f32 + t / 2.0;
        let (sx, sy) = match cp {
            0x256D => (1.0, 1.0),
            0x256E => (-1.0, 1.0),
            0x256F => (-1.0, -1.0),
            _ => (1.0, -1.0),
        };
        let radius = (w.min(h) / 2.0).max(t);
        let (ox, oy) = (lx + sx * radius, ly + sy * radius);
        self.sample(|x, y| {
            let on_arc_side = (x - ox) * sx <= 0.0 && (y - oy) * sy <= 0.0;
            on_arc_side && ((x - ox).hypot(y - oy) - radius).abs() <= t / 2.0
        });
        let half = light / 2;
        let (lxi, lyi) = (self.width as i32 / 2 - half, self.height as i32 / 2 - half);
        if sy > 0.0 {
            self.rect(lxi, oy as i32, lxi + light, self.height as i32, 255);
        } else {
            self.rect(lxi, 0, lxi + light, oy.ceil() as i32, 255);
        }
        if sx > 0.0 {
            self.rect(ox as i32, lyi, self.width as i32, lyi + light, 255);
        } else {
            self.rect(0, lyi, ox.ceil() as i32, lyi + light, 255);
        }
    }

    fn diagonal(&mut self, cp: u32, light: i32) {
        let (w, h) = (self.width as f32, self.height as f32);
        let half = light as f32 / 2.0;
        let length = w.hypot(h);
        let rising = move |x: f32, y: f32| (h * x + w * y - w * h).abs() / length <= half;
        let falling = move |x: f32, y: f32| (h * x - w * y).abs() / length <= half;
        match cp {
            0x2571 => self.sample(rising),
            0x2572 => self.sample(falling),
            _ => self.sample(|x, y| rising(x, y) || falling(x, y)),
        }
    }

    fn block(&mut self, cp: u32) {
        let (w, h) = (self.width as i32, self.height as i32);
        let eighth_h = |n: u32| (h * n as i32 + 4) / 8;
        let eighth_w = |n: u32| (w * n as i32 + 4) / 8;
        match cp {
            0x2580 => self.rect(0, 0, w, h / 2, 255),
            0x2581..=0x2588 => self.rect(0, h - eighth_h(cp - 0x2580), w, h, 255),
            0x2589..=0x258F => self.rect(0, 0, eighth_w(8 - (cp - 0x2588)), h, 255),
            0x2590 => self.rect(w / 2, 0, w, h, 255),
            0x2591 => self.rect(0, 0, w, h, 64),
            0x2592 => self.rect(0, 0, w, h, 128),
            0x2593 => self.rect(0, 0, w, h, 191),
            0x2594 => self.rect(0, 0, w, eighth_h(1), 255),
            0x2595 => self.rect(w - eighth_w(1), 0, w, h, 255),
            _ => {
                // Quadrants: upper left 1, upper right 2, lower left 4, lower right 8.
                const QUADRANTS: [u8; 10] = [4, 8, 1, 13, 9, 7, 11, 2, 6, 14];
                let mask = QUADRANTS[(cp - 0x2596) as usize];
                let (mx, my) = (w / 2, h / 2);
                if mask & 1 != 0 {
                    self.rect(0, 0, mx, my, 255);
                }
                if mask & 2 != 0 {
                    self.rect(mx, 0, w, my, 255);
                }
                if mask & 4 != 0 {
                    self.rect(0, my, mx, h, 255);
                }
                if mask & 8 != 0 {
                    self.rect(mx, my, w, h, 255);
                }
            }
        }
    }

    fn powerline(&mut self, cp: u32, light: i32) {
        let (w, h) = (self.width as f32, self.height as f32);
        let half = light as f32 / 2.0;
        let mid = h / 2.0;
        // Distance from a point to the segment a-b.
        let segment = |px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32| {
            let (dx, dy) = (bx - ax, by - ay);
            let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
            (px - (ax + t * dx)).hypot(py - (ay + t * dy))
        };
        match cp {
            0xE0B0 => self.sample(|x, y| x <= w * (1.0 - (2.0 * y / h - 1.0).abs())),
            0xE0B2 => self.sample(|x, y| x >= w * (2.0 * y / h - 1.0).abs()),
            0xE0B1 => {
                self.sample(|x, y| segment(x, y, 0.0, 0.0, w, mid) <= half || segment(x, y, w, mid, 0.0, h) <= half)
            }
            0xE0B3 => {
                self.sample(|x, y| segment(x, y, w, 0.0, 0.0, mid) <= half || segment(x, y, 0.0, mid, w, h) <= half)
            }
            0xE0B4 => self.sample(|x, y| (x / w).powi(2) + ((y - mid) / mid).powi(2) <= 1.0),
            _ => self.sample(|x, y| ((w - x) / w).powi(2) + ((y - mid) / mid).powi(2) <= 1.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coverage(c: char) -> Vec<u8> {
        render(c, 10, 20, 15, 1).unwrap().data
    }

    #[test]
    fn cross_reaches_every_edge() {
        let data = coverage('┼');
        let at = |x: usize, y: usize| data[y * 10 + x];
        assert_eq!(at(5, 0), 255);
        assert_eq!(at(5, 19), 255);
        assert_eq!(at(0, 10), 255);
        assert_eq!(at(9, 10), 255);
        assert_eq!(at(0, 0), 0);
    }

    #[test]
    fn full_block_and_shades() {
        assert!(coverage('█').iter().all(|&a| a == 255));
        assert!(coverage('▒').iter().all(|&a| a == 128));
        let lower = coverage('▄');
        assert_eq!(lower[0], 0);
        assert_eq!(lower[19 * 10], 255);
    }

    #[test]
    fn every_sprite_renders() {
        for cp in (0x2500..=0x259F).chain(0xE0B0..=0xE0B4).chain([0xE0B6]) {
            let c = char::from_u32(cp).unwrap();
            assert!(is_sprite(c));
            let glyph = render(c, 9, 19, 14, 1).unwrap();
            assert!(glyph.data.iter().any(|&a| a > 0), "U+{cp:04X} is empty");
        }
    }
}
