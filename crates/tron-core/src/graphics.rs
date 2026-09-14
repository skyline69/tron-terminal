//! Kitty graphics protocol: image storage and placements.
//!
//! Spec: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>
//!
//! Supported: direct, file and temporary file transmission, chunking, zlib
//! compression, PNG and raw RGB/RGBA data, placements with source rectangles,
//! cell offsets, sizes and z-index, deletion and queries. Shared memory
//! transmission and animation are not supported yet.

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;

use base64::Engine;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

const BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);
const MAX_TRANSFER: usize = 400 * 1024 * 1024;
const MAX_DIMENSION: u32 = 10_000;
const DEFAULT_QUOTA: usize = 320 * 1024 * 1024;
/// Placements below this z-index are drawn under cell backgrounds.
pub const BELOW_BACKGROUND_Z: i32 = i32::MIN / 2;

/// Decoded image data, RGBA with straight alpha.
#[derive(Clone)]
pub struct Image {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
    /// Changes whenever the pixels change.
    pub generation: u64,
}

/// An image shown in the grid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    pub image_id: u32,
    pub placement_id: u32,
    /// Absolute line of the top edge.
    pub line: i64,
    pub col: usize,
    /// Size in cells.
    pub cols: u32,
    pub rows: u32,
    /// The application set the size in cells, so the image is scaled to fill
    /// them. Otherwise it is drawn at its natural pixel size.
    pub scaled: bool,
    /// Pixel offset inside the first cell.
    pub offset_x: u32,
    pub offset_y: u32,
    /// Source rectangle in image pixels: x, y, width, height.
    pub source: [u32; 4],
    pub z: i32,
    pub alt_screen: bool,
}

/// Terminal state the protocol needs.
#[derive(Copy, Clone, Debug)]
pub struct Context {
    pub cursor_line: i64,
    pub cursor_col: usize,
    pub screen_top: i64,
    pub rows: usize,
    pub cols: usize,
    pub cell_width: u32,
    pub cell_height: u32,
    pub alt_screen: bool,
}

#[derive(Default, Debug)]
pub struct Outcome {
    pub response: Option<Vec<u8>>,
    /// Cells to move the cursor by, as (columns, rows).
    pub cursor_advance: Option<(u32, u32)>,
}

#[derive(Clone, Debug)]
struct Control {
    action: u8,
    format: u32,
    medium: u8,
    width: u32,
    height: u32,
    size: usize,
    offset: usize,
    id: u32,
    placement: u32,
    compressed: bool,
    more: bool,
    quiet: u32,
    source: [u32; 4],
    offset_x: u32,
    offset_y: u32,
    cols: u32,
    rows: u32,
    no_cursor_move: bool,
    z: i32,
    delete: u8,
}

impl Default for Control {
    fn default() -> Self {
        Self {
            action: b't',
            format: 32,
            medium: b'd',
            width: 0,
            height: 0,
            size: 0,
            offset: 0,
            id: 0,
            placement: 0,
            compressed: false,
            more: false,
            quiet: 0,
            source: [0; 4],
            offset_x: 0,
            offset_y: 0,
            cols: 0,
            rows: 0,
            no_cursor_move: false,
            z: 0,
            delete: b'a',
        }
    }
}

impl Control {
    fn parse(keys: &[u8]) -> Self {
        let mut control = Self::default();
        for pair in keys.split(|&b| b == b',') {
            let [key, b'=', value @ ..] = pair else { continue };
            let char_value = value.first().copied().unwrap_or(0);
            let number = || std::str::from_utf8(value).ok().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
            let unsigned = || number().clamp(0, i64::from(u32::MAX)) as u32;
            match key {
                b'a' => control.action = char_value,
                b't' => control.medium = char_value,
                b'o' => control.compressed = char_value == b'z',
                b'd' => control.delete = char_value,
                b'f' => control.format = unsigned(),
                b's' => control.width = unsigned(),
                b'v' => control.height = unsigned(),
                b'S' => control.size = unsigned() as usize,
                b'O' => control.offset = unsigned() as usize,
                b'i' => control.id = unsigned(),
                b'p' => control.placement = unsigned(),
                b'm' => control.more = unsigned() == 1,
                b'q' => control.quiet = unsigned(),
                b'x' => control.source[0] = unsigned(),
                b'y' => control.source[1] = unsigned(),
                b'w' => control.source[2] = unsigned(),
                b'h' => control.source[3] = unsigned(),
                b'X' => control.offset_x = unsigned(),
                b'Y' => control.offset_y = unsigned(),
                b'c' => control.cols = unsigned(),
                b'r' => control.rows = unsigned(),
                b'C' => control.no_cursor_move = unsigned() == 1,
                b'z' => control.z = number().clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
                _ => {}
            }
        }
        control
    }
}

pub struct Graphics {
    images: HashMap<u32, Image>,
    placements: Vec<Placement>,
    pending: Option<(Control, Vec<u8>)>,
    next_internal_id: u32,
    image_generation: u64,
    generation: u64,
    memory: usize,
    quota: usize,
    allow_files: bool,
}

impl Default for Graphics {
    fn default() -> Self {
        Self::new()
    }
}

impl Graphics {
    pub fn new() -> Self {
        Self {
            images: HashMap::new(),
            placements: Vec::new(),
            pending: None,
            next_internal_id: u32::MAX,
            image_generation: 0,
            generation: 0,
            memory: 0,
            quota: DEFAULT_QUOTA,
            allow_files: true,
        }
    }

    pub fn set_limits(&mut self, quota_bytes: usize, allow_files: bool) {
        self.quota = quota_bytes;
        self.allow_files = allow_files;
        self.enforce_quota(None);
    }

    pub fn images(&self) -> &HashMap<u32, Image> {
        &self.images
    }

    pub fn placements(&self) -> &[Placement] {
        &self.placements
    }

    /// Changes whenever images or placements change.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Handles the payload of an `ESC _ G ... ESC \` sequence.
    pub fn handle(&mut self, payload: &[u8], ctx: &Context) -> Outcome {
        let (keys, data) = match memchr::memchr(b';', payload) {
            Some(i) => (&payload[..i], &payload[i + 1..]),
            None => (payload, &[][..]),
        };
        let control = Control::parse(keys);

        if let Some((first, mut buffer)) = self.pending.take() {
            if buffer.len() + data.len() > MAX_TRANSFER {
                return respond(&first, Err("EFBIG:transfer too large".into()));
            }
            buffer.extend_from_slice(data);
            if control.more {
                self.pending = Some((first, buffer));
                return Outcome::default();
            }
            return self.complete(first, &buffer, ctx);
        }
        if control.more && matches!(control.action, b't' | b'T' | b'q') {
            self.pending = Some((control, data.to_vec()));
            return Outcome::default();
        }
        self.complete(control, data, ctx)
    }

    fn complete(&mut self, control: Control, data: &[u8], ctx: &Context) -> Outcome {
        match control.action {
            b't' | b'T' | b'q' => {
                let (width, height, rgba) = match self.load(&control, data) {
                    Ok(image) => image,
                    Err(error) => return respond(&control, Err(error)),
                };
                if control.action == b'q' {
                    return respond(&control, Ok(()));
                }
                let id = if control.id != 0 { control.id } else { self.internal_id() };
                self.store(id, width, height, rgba);
                let mut outcome = respond(&control, Ok(()));
                if control.action == b'T' {
                    outcome.cursor_advance = self.place(id, &control, ctx);
                }
                outcome
            }
            b'p' => {
                if !self.images.contains_key(&control.id) {
                    return respond(&control, Err("ENOENT:no such image".into()));
                }
                let mut outcome = respond(&control, Ok(()));
                outcome.cursor_advance = self.place(control.id, &control, ctx);
                outcome
            }
            b'd' => {
                self.delete(&control, ctx);
                Outcome::default()
            }
            _ => respond(&control, Err("EINVAL:unsupported action".into())),
        }
    }

    fn internal_id(&mut self) -> u32 {
        while self.images.contains_key(&self.next_internal_id) {
            self.next_internal_id -= 1;
        }
        let id = self.next_internal_id;
        self.next_internal_id -= 1;
        id
    }

    fn load(&self, control: &Control, data: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
        let raw = match control.medium {
            b'd' => BASE64.decode(data).map_err(|e| format!("EINVAL:bad base64: {e}"))?,
            b'f' | b't' => {
                if !self.allow_files {
                    return Err("EPERM:file transmission is disabled".into());
                }
                let path = BASE64.decode(data).map_err(|e| format!("EINVAL:bad base64: {e}"))?;
                let path = String::from_utf8(path).map_err(|_| "EINVAL:path is not UTF-8".to_string())?;
                read_file(&path, control)?
            }
            b's' => return Err("EINVAL:shared memory transmission is not supported".into()),
            _ => return Err("EINVAL:unknown transmission medium".into()),
        };
        let raw = if control.compressed {
            miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&raw, MAX_TRANSFER)
                .map_err(|_| "EINVAL:zlib decompression failed".to_string())?
        } else {
            raw
        };
        match control.format {
            100 => decode_png(&raw),
            24 | 32 => {
                let (width, height) = (control.width, control.height);
                if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
                    return Err("EINVAL:invalid image dimensions".into());
                }
                let pixels = width as usize * height as usize;
                let bpp = (control.format / 8) as usize;
                if raw.len() < pixels * bpp {
                    return Err("ENODATA:insufficient image data".into());
                }
                let rgba = if bpp == 4 {
                    raw[..pixels * 4].to_vec()
                } else {
                    raw[..pixels * 3].as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect()
                };
                Ok((width, height, rgba))
            }
            _ => Err("EINVAL:unsupported format".into()),
        }
    }

    fn store(&mut self, id: u32, width: u32, height: u32, rgba: Vec<u8>) {
        self.image_generation += 1;
        let image = Image { id, width, height, rgba: rgba.into(), generation: self.image_generation };
        self.memory += image.rgba.len();
        if let Some(old) = self.images.insert(id, image) {
            self.memory -= old.rgba.len();
        }
        self.enforce_quota(Some(id));
        self.generation += 1;
    }

    /// Evicts the oldest images, preferring ones without placements.
    fn enforce_quota(&mut self, keep: Option<u32>) {
        while self.memory > self.quota {
            let placed = |id: u32| self.placements.iter().any(|p| p.image_id == id);
            let victim = self
                .images
                .values()
                .filter(|img| Some(img.id) != keep)
                .min_by_key(|img| (placed(img.id), img.generation))
                .map(|img| img.id);
            let Some(id) = victim else { break };
            self.remove_image(id);
        }
    }

    fn remove_image(&mut self, id: u32) {
        if let Some(image) = self.images.remove(&id) {
            self.memory -= image.rgba.len();
        }
        self.placements.retain(|p| p.image_id != id);
        self.generation += 1;
    }

    fn place(&mut self, id: u32, control: &Control, ctx: &Context) -> Option<(u32, u32)> {
        let image = self.images.get(&id)?;
        let x = control.source[0].min(image.width);
        let y = control.source[1].min(image.height);
        let width = if control.source[2] == 0 { image.width - x } else { control.source[2].min(image.width - x) };
        let height = if control.source[3] == 0 { image.height - y } else { control.source[3].min(image.height - y) };
        let cell_w = ctx.cell_width.max(1);
        let cell_h = ctx.cell_height.max(1);
        let cols = if control.cols > 0 { control.cols } else { (width + control.offset_x).div_ceil(cell_w).max(1) };
        let rows = if control.rows > 0 { control.rows } else { (height + control.offset_y).div_ceil(cell_h).max(1) };

        if control.placement != 0 {
            self.placements
                .retain(|p| !(p.image_id == id && p.placement_id == control.placement));
        }
        self.placements.push(Placement {
            image_id: id,
            placement_id: control.placement,
            line: ctx.cursor_line,
            col: ctx.cursor_col,
            cols,
            rows,
            scaled: control.cols > 0 || control.rows > 0,
            offset_x: control.offset_x.min(cell_w - 1),
            offset_y: control.offset_y.min(cell_h - 1),
            source: [x, y, width, height],
            z: control.z,
            alt_screen: ctx.alt_screen,
        });
        self.generation += 1;
        (!control.no_cursor_move).then_some((cols, rows))
    }

    fn delete(&mut self, control: &Control, ctx: &Context) {
        let free = control.delete.is_ascii_uppercase();
        let on_screen = |p: &Placement| {
            p.alt_screen == ctx.alt_screen
                && p.line + i64::from(p.rows) > ctx.screen_top
                && p.line < ctx.screen_top + ctx.rows as i64
        };
        let covers = |p: &Placement, line: i64, col: usize| {
            p.alt_screen == ctx.alt_screen
                && (p.line..p.line + i64::from(p.rows)).contains(&line)
                && (p.col..p.col + p.cols as usize).contains(&col)
        };
        let target: Box<dyn Fn(&Placement) -> bool> = match control.delete.to_ascii_lowercase() {
            b'a' => Box::new(on_screen),
            b'i' => Box::new(|p: &Placement| {
                p.image_id == control.id && (control.placement == 0 || p.placement_id == control.placement)
            }),
            b'c' => Box::new(move |p: &Placement| covers(p, ctx.cursor_line, ctx.cursor_col)),
            b'p' => {
                let line = ctx.screen_top + i64::from(control.source[1].saturating_sub(1));
                let col = control.source[0].saturating_sub(1) as usize;
                Box::new(move |p: &Placement| covers(p, line, col))
            }
            b'x' => {
                let col = control.source[0].saturating_sub(1) as usize;
                Box::new(move |p: &Placement| on_screen(p) && (p.col..p.col + p.cols as usize).contains(&col))
            }
            b'y' => {
                let line = ctx.screen_top + i64::from(control.source[1].saturating_sub(1));
                Box::new(move |p: &Placement| on_screen(p) && (p.line..p.line + i64::from(p.rows)).contains(&line))
            }
            b'z' => Box::new(|p: &Placement| on_screen(p) && p.z == control.z),
            _ => return,
        };

        let mut affected = Vec::new();
        self.placements.retain(|p| {
            let remove = target(p);
            if remove {
                affected.push(p.image_id);
            }
            !remove
        });
        if free {
            if control.delete.eq_ignore_ascii_case(&b'i') {
                affected.push(control.id);
            }
            for id in affected {
                if !self.placements.iter().any(|p| p.image_id == id) {
                    self.remove_image(id);
                }
            }
        }
        self.generation += 1;
    }

    /// Drops placements that scrolled out of history.
    pub fn prune(&mut self, oldest_line: i64, alt_screen: bool) {
        if self.placements.is_empty() {
            return;
        }
        let before = self.placements.len();
        self.placements
            .retain(|p| p.alt_screen != alt_screen || p.line + i64::from(p.rows) > oldest_line);
        if self.placements.len() != before {
            self.generation += 1;
        }
    }

    /// Removes placements visible on the given screen (screen clear).
    pub fn clear_screen(&mut self, screen_top: i64, rows: usize, alt_screen: bool) {
        let before = self.placements.len();
        self.placements.retain(|p| {
            p.alt_screen != alt_screen || p.line + i64::from(p.rows) <= screen_top || p.line >= screen_top + rows as i64
        });
        if self.placements.len() != before {
            self.generation += 1;
        }
    }

    pub fn clear_alt_screen(&mut self) {
        let before = self.placements.len();
        self.placements.retain(|p| !p.alt_screen);
        if self.placements.len() != before {
            self.generation += 1;
        }
    }

    pub fn clear(&mut self) {
        self.images.clear();
        self.placements.clear();
        self.pending = None;
        self.memory = 0;
        self.generation += 1;
    }
}

fn respond(control: &Control, result: Result<(), String>) -> Outcome {
    let message = match &result {
        Ok(()) if control.quiet >= 1 => return Outcome::default(),
        Err(_) if control.quiet >= 2 => return Outcome::default(),
        Ok(()) => "OK",
        Err(error) => error.as_str(),
    };
    if control.id == 0 {
        return Outcome::default();
    }
    let mut response = format!("\x1b_Gi={}", control.id);
    if control.placement != 0 {
        response.push_str(&format!(",p={}", control.placement));
    }
    response.push(';');
    response.push_str(message);
    response.push_str("\x1b\\");
    Outcome { response: Some(response.into_bytes()), cursor_advance: None }
}

fn read_file(path: &str, control: &Control) -> Result<Vec<u8>, String> {
    let metadata = std::fs::metadata(path).map_err(|e| format!("EBADF:{e}"))?;
    if !metadata.is_file() {
        return Err("EINVAL:not a regular file".into());
    }
    let mut file = std::fs::File::open(path).map_err(|e| format!("EBADF:{e}"))?;
    let mut data = Vec::new();
    if control.offset > 0 {
        std::io::copy(&mut (&mut file).take(control.offset as u64), &mut std::io::sink())
            .map_err(|e| format!("EBADF:{e}"))?;
    }
    let limit = if control.size > 0 { control.size } else { MAX_TRANSFER };
    file.take(limit as u64).read_to_end(&mut data).map_err(|e| format!("EBADF:{e}"))?;
    if control.medium == b't' {
        // Temporary files must be deleted after reading, but only obvious ones.
        let temp = std::env::temp_dir();
        let in_temp = path.starts_with(temp.to_string_lossy().as_ref()) || path.starts_with("/tmp/") || path.starts_with("/dev/shm/");
        if in_temp && path.contains("tty-graphics-protocol") {
            let _ = std::fs::remove_file(path);
        }
    }
    Ok(data)
}

fn decode_png(data: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| format!("EINVAL:bad PNG: {e}"))?;
    let size = reader.output_buffer_size().ok_or("EINVAL:PNG too large")?;
    let mut buffer = vec![0; size];
    let info = reader.next_frame(&mut buffer).map_err(|e| format!("EINVAL:bad PNG: {e}"))?;
    let (width, height) = (info.width, info.height);
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err("EINVAL:image too large".into());
    }
    buffer.truncate(info.line_size * height as usize);
    let rgba = match info.color_type {
        png::ColorType::Rgba => buffer,
        png::ColorType::Rgb => buffer.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => buffer.as_chunks::<2>().0.iter().flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => buffer.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("EINVAL:unexpanded palette PNG".into()),
    };
    Ok((width, height, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context {
            cursor_line: 5,
            cursor_col: 2,
            screen_top: 0,
            rows: 24,
            cols: 80,
            cell_width: 10,
            cell_height: 20,
            alt_screen: false,
        }
    }

    #[test]
    fn transmit_and_display_rgba_in_chunks() {
        let mut g = Graphics::new();
        let pixels = BASE64.encode([255u8; 4 * 25 * 30]);
        let (first, rest) = pixels.split_at(8);
        let out = g.handle(format!("a=T,f=32,s=25,v=30,i=7,m=1;{first}").as_bytes(), &ctx());
        assert!(out.response.is_none());
        let out = g.handle(format!("m=0;{rest}").as_bytes(), &ctx());
        assert_eq!(out.response.as_deref(), Some(&b"\x1b_Gi=7;OK\x1b\\"[..]));
        assert_eq!(out.cursor_advance, Some((3, 2)));
        let p = &g.placements()[0];
        assert_eq!((p.line, p.col, p.cols, p.rows), (5, 2, 3, 2));
        assert_eq!(g.images()[&7].width, 25);
    }

    #[test]
    fn errors_and_quiet_mode() {
        let mut g = Graphics::new();
        let out = g.handle(b"a=p,i=9", &ctx());
        assert!(String::from_utf8(out.response.unwrap()).unwrap().contains("ENOENT"));
        let out = g.handle(b"a=p,i=9,q=2", &ctx());
        assert!(out.response.is_none());
    }

    #[test]
    fn delete_by_id_frees_image() {
        let mut g = Graphics::new();
        let data = BASE64.encode([0u8; 3 * 4]);
        g.handle(format!("a=T,f=24,s=2,v=2,i=1,q=1;{data}").as_bytes(), &ctx());
        g.handle(b"a=d,d=I,i=1", &ctx());
        assert!(g.placements().is_empty());
        assert!(g.images().is_empty());
    }

    #[test]
    fn zlib_compressed_payload() {
        let mut g = Graphics::new();
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&[7u8; 4 * 4], 6);
        let data = BASE64.encode(compressed);
        let out = g.handle(format!("a=t,f=32,o=z,s=2,v=2,i=3;{data}").as_bytes(), &ctx());
        assert_eq!(out.response.as_deref(), Some(&b"\x1b_Gi=3;OK\x1b\\"[..]));
        assert_eq!(&g.images()[&3].rgba[..4], &[7, 7, 7, 7]);
    }
}
