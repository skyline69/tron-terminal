//! Sixel image decoder (DEC VT340 style), fed byte by byte from a DCS string.

use crate::parser::Params;

/// Largest accepted image side in pixels.
const MAX_SIZE: usize = 4096;

/// VT340 default palette, in percent.
const VT340: [[u8; 3]; 16] = [
    [0, 0, 0],
    [20, 20, 80],
    [80, 13, 13],
    [20, 80, 20],
    [80, 20, 80],
    [20, 80, 80],
    [80, 80, 20],
    [53, 53, 53],
    [26, 26, 26],
    [33, 33, 60],
    [60, 26, 26],
    [33, 60, 33],
    [60, 33, 60],
    [33, 60, 60],
    [60, 60, 33],
    [80, 80, 80],
];

pub struct SixelDecoder {
    palette: [[u8; 3]; 256],
    color: usize,
    x: usize,
    y: usize,
    painted: (usize, usize),
    raster: (usize, usize),
    buffer: Vec<u8>,
    buffer_size: (usize, usize),
    transparent: bool,
    command: Option<u8>,
    params: [u32; 5],
    param_count: usize,
    number: Option<u32>,
    repeat: usize,
}

impl SixelDecoder {
    pub fn new(params: &Params) -> Self {
        let mut palette = [[0u8; 3]; 256];
        for (slot, color) in palette.iter_mut().zip(VT340) {
            *slot = color.map(percent);
        }
        Self {
            palette,
            color: 0,
            x: 0,
            y: 0,
            painted: (0, 0),
            raster: (0, 0),
            buffer: Vec::new(),
            buffer_size: (0, 0),
            transparent: params.raw(1) == Some(1),
            command: None,
            params: [0; 5],
            param_count: 0,
            number: None,
            repeat: 1,
        }
    }

    pub fn put(&mut self, byte: u8) {
        match byte {
            b'0'..=b'9' if self.command.is_some() => {
                let n = self.number.unwrap_or(0);
                self.number = Some((n * 10 + u32::from(byte - b'0')).min(1_000_000));
            }
            b';' if self.command.is_some() => self.push_param(),
            _ => {
                self.finish_command();
                match byte {
                    b'"' | b'#' | b'!' => {
                        self.command = Some(byte);
                        self.param_count = 0;
                        self.number = None;
                    }
                    b'$' => self.x = 0,
                    b'-' => {
                        self.x = 0;
                        self.y += 6;
                    }
                    0x3f..=0x7e => self.paint(byte - 0x3f),
                    _ => {}
                }
            }
        }
    }

    fn push_param(&mut self) {
        if self.param_count < self.params.len() {
            self.params[self.param_count] = self.number.unwrap_or(0);
            self.param_count += 1;
        }
        self.number = None;
    }

    fn finish_command(&mut self) {
        let Some(command) = self.command.take() else { return };
        if self.number.is_some() {
            self.push_param();
        }
        let p = self.params;
        let count = self.param_count;
        match command {
            b'"' if count >= 4 => {
                self.raster = ((p[2] as usize).min(MAX_SIZE), (p[3] as usize).min(MAX_SIZE));
                self.reserve(self.raster.0, self.raster.1);
            }
            b'#' if count >= 1 => {
                let register = (p[0] % 256) as usize;
                if count >= 5 {
                    self.palette[register] = match p[1] {
                        1 => hls_to_rgb(p[2], p[3], p[4]),
                        _ => [p[2], p[3], p[4]].map(|v| percent(v.min(100) as u8)),
                    };
                }
                self.color = register;
            }
            b'!' if count >= 1 => self.repeat = (p[0] as usize).clamp(1, MAX_SIZE),
            _ => {}
        }
    }

    fn paint(&mut self, bits: u8) {
        let count = std::mem::replace(&mut self.repeat, 1);
        if bits != 0 && self.x < MAX_SIZE && self.y < MAX_SIZE {
            let end_x = (self.x + count).min(MAX_SIZE);
            let end_y = (self.y + 6).min(MAX_SIZE);
            self.reserve(end_x, end_y);
            let [r, g, b] = self.palette[self.color];
            let width = self.buffer_size.0;
            for band in 0..6 {
                if bits & (1 << band) == 0 || self.y + band >= MAX_SIZE {
                    continue;
                }
                let row = (self.y + band) * width;
                for x in self.x..end_x {
                    let i = (row + x) * 4;
                    self.buffer[i..i + 4].copy_from_slice(&[r, g, b, 255]);
                }
                self.painted.1 = self.painted.1.max(self.y + band + 1);
            }
            self.painted.0 = self.painted.0.max(end_x);
        }
        self.x += count;
    }

    /// Grows the pixel buffer to at least `width` x `height`, keeping content.
    fn reserve(&mut self, width: usize, height: usize) {
        let (old_w, old_h) = self.buffer_size;
        if width <= old_w && height <= old_h {
            return;
        }
        let new_w = width.max(old_w).max(old_w.saturating_mul(2).min(MAX_SIZE)).max(64);
        let new_h = height.max(old_h).max(old_h.saturating_mul(2).min(MAX_SIZE)).max(64);
        let mut buffer = vec![0u8; new_w * new_h * 4];
        for y in 0..old_h {
            let src = y * old_w * 4;
            let dst = y * new_w * 4;
            buffer[dst..dst + old_w * 4].copy_from_slice(&self.buffer[src..src + old_w * 4]);
        }
        self.buffer = buffer;
        self.buffer_size = (new_w, new_h);
    }

    /// The decoded image as RGBA. Unpainted pixels use `background` unless the
    /// image asked for a transparent background.
    pub fn finish(mut self, background: [u8; 3]) -> Option<(u32, u32, Vec<u8>)> {
        self.finish_command();
        let width = self.painted.0.max(self.raster.0).min(MAX_SIZE);
        let height = self.painted.1.max(self.raster.1).min(MAX_SIZE);
        if width == 0 || height == 0 {
            return None;
        }
        self.reserve(width, height);
        let stride = self.buffer_size.0;
        let mut out = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            for x in 0..width {
                let i = (y * stride + x) * 4;
                let pixel = &self.buffer[i..i + 4];
                if pixel[3] == 0 && !self.transparent {
                    out.extend_from_slice(&[background[0], background[1], background[2], 255]);
                } else {
                    out.extend_from_slice(pixel);
                }
            }
        }
        Some((width as u32, height as u32, out))
    }
}

fn percent(value: u8) -> u8 {
    (u16::from(value.min(100)) * 255 / 100) as u8
}

/// DEC HLS: hue 0 is blue, 120 red, 240 green. Lightness and saturation in percent.
fn hls_to_rgb(hue: u32, lightness: u32, saturation: u32) -> [u8; 3] {
    let h = ((hue % 360) as f32 + 240.0) % 360.0 / 360.0;
    let l = lightness.min(100) as f32 / 100.0;
    let s = saturation.min(100) as f32 / 100.0;
    if s == 0.0 {
        let v = (l * 255.0).round() as u8;
        return [v, v, v];
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let channel = |t: f32| {
        let t = t.rem_euclid(1.0);
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round() as u8
    };
    [channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(data: &[u8], transparent: bool) -> (u32, u32, Vec<u8>) {
        let mut decoder = SixelDecoder::new(&Params::default());
        decoder.transparent = transparent;
        for &b in data {
            decoder.put(b);
        }
        decoder.finish([1, 2, 3]).unwrap()
    }

    #[test]
    fn decodes_colors_repeats_and_bands() {
        // Red register, 3 full columns, next band, 1 green column of the top pixel only.
        let (w, h, rgba) = decode(b"#1;2;100;0;0#1!3~-#2;2;0;100;0@", false);
        assert_eq!((w, h), (3, 7));
        assert_eq!(&rgba[..4], &[255, 0, 0, 255]);
        let pixel = |x: usize, y: usize| &rgba[(y * 3 + x) * 4..(y * 3 + x) * 4 + 4];
        assert_eq!(pixel(0, 6), &[0, 255, 0, 255]);
        assert_eq!(pixel(1, 6), &[1, 2, 3, 255]);
    }

    #[test]
    fn raster_attributes_and_transparency() {
        let (w, h, rgba) = decode(b"\"1;1;8;12#0~", true);
        assert_eq!((w, h), (8, 12));
        assert_eq!(rgba[3], 255);
        assert_eq!(rgba[(8 * 11 + 7) * 4 + 3], 0);
    }

    #[test]
    fn hls_primaries() {
        assert_eq!(hls_to_rgb(120, 50, 100), [255, 0, 0]);
        assert_eq!(hls_to_rgb(0, 50, 100), [0, 0, 255]);
        assert_eq!(hls_to_rgb(240, 50, 100), [0, 255, 0]);
    }
}
