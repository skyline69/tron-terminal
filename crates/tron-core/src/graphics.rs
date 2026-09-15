//! Kitty graphics protocol and Sixel images: storage, placements, animation.
//!
//! Spec: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>
//!
//! Supported: direct, file, temporary file and shared memory transmission,
//! chunking, zlib compression, PNG and raw RGB/RGBA data, placements with
//! source rectangles, offsets, sizes and z-index, relative placements,
//! Unicode placeholders, animation frames, frame composition and control,
//! deletion and queries. iTerm2 inline images (PNG, JPEG, GIF) are stored here too.

mod diacritics;

use std::collections::HashMap;
use std::io::Read;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

use crate::cell::{Cell, Color, ColorKind};

pub(crate) const BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);
const MAX_TRANSFER: usize = 400 * 1024 * 1024;
const MAX_DIMENSION: u32 = 10_000;
const DEFAULT_QUOTA: usize = 320 * 1024 * 1024;
/// Frame gap used when an application does not give one.
const DEFAULT_GAP_MS: u32 = 40;
/// Most frames kept from an animated GIF.
const MAX_GIF_FRAMES: usize = 1000;
/// Deepest chain of placements positioned relative to each other.
const MAX_RELATIVE_DEPTH: usize = 8;
/// Placements below this z-index are drawn under cell backgrounds.
pub const BELOW_BACKGROUND_Z: i32 = i32::MIN / 2;
/// Character that marks a cell showing part of an image (placeholder mode).
pub const PLACEHOLDER: char = '\u{10EEEE}';

#[derive(Clone)]
pub struct Frame {
    pub rgba: Arc<[u8]>,
    pub gap_ms: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum AnimationState {
    #[default]
    Stopped,
    /// Runs until the last frame, then waits for more frames.
    Loading,
    Running,
}

/// Decoded image data, RGBA with straight alpha.
#[derive(Clone)]
pub struct Image {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    /// Pixels of the frame currently shown.
    pub rgba: Arc<[u8]>,
    /// Changes whenever the shown pixels change.
    pub generation: u64,
    /// Every pixel is fully opaque.
    pub opaque: bool,
    /// Created without an id (for example by Sixel or anonymous transfers).
    anonymous: bool,
    frames: Vec<Frame>,
    current: usize,
    state: AnimationState,
    loops_left: Option<u32>,
    frame_started: Option<Instant>,
}

impl Image {
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    fn memory(&self) -> usize {
        self.frames.iter().map(|f| f.rgba.len()).sum()
    }
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
    /// Not drawn directly: shown through placeholder characters in cells.
    pub virtual_placement: bool,
    /// Image and placement id of the placement this one is positioned relative to.
    pub parent: Option<(u32, u32)>,
    /// Drawn at this size in pixels instead of the natural or cell size.
    pub pixel_size: Option<[u32; 2]>,
    /// Part of the cells it covers, like text: Sixel and iTerm2 inline images.
    /// Writing or erasing any of those cells removes it.
    pub cell_bound: bool,
}

/// A cell that shows part of an image through the placeholder character.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PlaceholderCell {
    pub image_id: u32,
    pub placement_id: u32,
    pub row: u32,
    pub col: u32,
}

/// Decodes a placeholder cell. `previous` is the placeholder cell to the left,
/// used when diacritics are omitted.
pub fn placeholder_cell(
    cell: &Cell,
    underline_color: Color,
    combining: Option<&str>,
    previous: Option<PlaceholderCell>,
) -> Option<PlaceholderCell> {
    if cell.ch != PLACEHOLDER {
        return None;
    }
    let mut marks = combining.unwrap_or("").chars().map(diacritics::index);
    let row = marks.next().flatten();
    let col = marks.next().flatten();
    let msb = marks.next().flatten();
    let color_id = |color: Color| match color.kind() {
        ColorKind::Rgb(r, g, b) => u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b),
        ColorKind::Indexed(i) => u32::from(i),
        ColorKind::Default => 0,
    };
    let low = color_id(cell.fg);
    let continues = previous.filter(|p| p.image_id & 0x00ff_ffff == low);
    let image_id = match (msb, continues) {
        (Some(msb), _) => low | msb << 24,
        (None, Some(p)) => p.image_id,
        (None, None) => low,
    };
    let (row, col) = match (row, col, continues) {
        (Some(r), Some(c), _) => (r, c),
        (Some(r), None, Some(p)) if p.row == r => (r, p.col + 1),
        (None, None, Some(p)) => (p.row, p.col + 1),
        (Some(r), None, _) => (r, 0),
        (None, _, _) => (0, 0),
    };
    Some(PlaceholderCell { image_id, placement_id: color_id(underline_color), row, col })
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

/// Decoded pixels of a transmission.
struct Decoded {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    opaque: bool,
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
    placeholder: bool,
    parent_image: u32,
    parent_placement: u32,
    /// Offset from the parent placement in cells.
    parent_offset_x: i32,
    parent_offset_y: i32,
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
            placeholder: false,
            parent_image: 0,
            parent_placement: 0,
            parent_offset_x: 0,
            parent_offset_y: 0,
        }
    }
}

impl Control {
    /// Keys mean different things per action (`r` is rows for placements and a
    /// frame number for animation), so values are stored raw and read per action.
    fn parse(keys: &[u8]) -> Self {
        let mut control = Self::default();
        for pair in keys.split(|&b| b == b',') {
            let [key, b'=', value @ ..] = pair else { continue };
            let char_value = value.first().copied().unwrap_or(0);
            let number = || std::str::from_utf8(value).ok().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
            let unsigned = || number().clamp(0, i64::from(u32::MAX)) as u32;
            let signed = || number().clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
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
                b'U' => control.placeholder = unsigned() == 1,
                b'z' => control.z = signed(),
                b'P' => control.parent_image = unsigned(),
                b'Q' => control.parent_placement = unsigned(),
                b'H' => control.parent_offset_x = signed(),
                b'V' => control.parent_offset_y = signed(),
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

    /// Changes whenever images, frames or placements change.
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

        // Continuation chunks carry only `m` and `q`. Anything else starts a new
        // command and abandons an unfinished transfer.
        let continuation = keys
            .split(|&b| b == b',')
            .all(|pair| pair.is_empty() || pair.starts_with(b"m=") || pair.starts_with(b"q="));
        if !continuation {
            self.pending = None;
        }
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
        // Only direct transmissions are chunked. Players such as mpv set `m=1` on
        // shared memory transfers too, which must not wait for more data.
        if control.more && control.medium == b'd' && matches!(control.action, b't' | b'T' | b'q' | b'f') {
            self.pending = Some((control, data.to_vec()));
            return Outcome::default();
        }
        self.complete(control, data, ctx)
    }

    fn complete(&mut self, control: Control, data: &[u8], ctx: &Context) -> Outcome {
        match control.action {
            b't' | b'T' | b'q' => {
                let decoded = match self.load(&control, data) {
                    Ok(decoded) => decoded,
                    Err(error) => return respond(&control, Err(error)),
                };
                if control.action == b'q' {
                    return respond(&control, Ok(()));
                }
                let anonymous = control.id == 0;
                let id = if anonymous { self.internal_id() } else { control.id };
                self.store(id, decoded, anonymous);
                if control.action != b'T' {
                    return respond(&control, Ok(()));
                }
                match self.place(id, &control, ctx) {
                    Ok(advance) => Outcome { cursor_advance: advance, ..respond(&control, Ok(())) },
                    Err(error) => respond(&control, Err(error)),
                }
            }
            b'p' => match self.place(control.id, &control, ctx) {
                Ok(advance) => Outcome { cursor_advance: advance, ..respond(&control, Ok(())) },
                Err(error) => respond(&control, Err(error)),
            },
            b'c' => {
                let result = self.compose(&control);
                respond(&control, result)
            }
            b'f' => {
                let result = self.add_frame(&control, data);
                respond(&control, result)
            }
            b'a' => {
                let result = self.control_animation(&control);
                respond(&control, result)
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
            self.next_internal_id = self.next_internal_id.wrapping_sub(1).max(1 << 31);
        }
        let id = self.next_internal_id;
        self.next_internal_id = self.next_internal_id.wrapping_sub(1).max(1 << 31);
        id
    }

    fn next_image_generation(&mut self) -> u64 {
        self.image_generation += 1;
        self.image_generation
    }

    fn load(&self, control: &Control, data: &[u8]) -> Result<Decoded, String> {
        let raw = match control.medium {
            b'd' => BASE64.decode(data).map_err(|e| format!("EINVAL:bad base64: {e}"))?,
            b'f' | b't' | b's' => {
                let name = BASE64.decode(data).map_err(|e| format!("EINVAL:bad base64: {e}"))?;
                let name = String::from_utf8(name).map_err(|_| "EINVAL:path is not UTF-8".to_string())?;
                if control.medium == b's' {
                    read_shared_memory(&name, control)?
                } else if self.allow_files {
                    read_file(&name, control)?
                } else {
                    return Err("EPERM:file transmission is disabled".into());
                }
            }
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
                let (rgba, opaque) = if bpp == 4 {
                    let rgba = raw[..pixels * 4].to_vec();
                    let opaque = rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255);
                    (rgba, opaque)
                } else {
                    (raw[..pixels * 3].as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect(), true)
                };
                Ok(Decoded { width, height, rgba, opaque })
            }
            _ => Err("EINVAL:unsupported format".into()),
        }
    }

    fn store(&mut self, id: u32, decoded: Decoded, anonymous: bool) {
        let generation = self.next_image_generation();
        let rgba: Arc<[u8]> = decoded.rgba.into();
        let image = Image {
            id,
            width: decoded.width,
            height: decoded.height,
            rgba: rgba.clone(),
            generation,
            opaque: decoded.opaque,
            anonymous,
            frames: vec![Frame { rgba, gap_ms: DEFAULT_GAP_MS }],
            current: 0,
            state: AnimationState::Stopped,
            loops_left: None,
            frame_started: None,
        };
        self.memory += image.memory();
        if let Some(old) = self.images.insert(id, image) {
            self.memory -= old.memory();
        }
        self.enforce_quota(Some(id));
        self.generation += 1;
    }

    /// Adds a decoded image (Sixel) at the cursor. Returns the cursor advance.
    pub fn add_image(&mut self, width: u32, height: u32, rgba: Vec<u8>, ctx: &Context) -> (u32, u32) {
        let id = self.internal_id();
        self.store(id, Decoded { width, height, rgba, opaque: false }, true);
        let advance = self.place(id, &Control::default(), ctx).ok().flatten();
        if let Some(placement) = self.placements.last_mut().filter(|p| p.image_id == id) {
            placement.cell_bound = true;
        }
        advance.unwrap_or((1, 1))
    }

    /// Shows an image file (PNG, JPEG or animated GIF) at the cursor, sized as in
    /// iTerm2's inline image protocol. Returns the cursor advance in cells.
    pub fn add_inline_image(
        &mut self,
        data: &[u8],
        args: &InlineImageArgs,
        ctx: &Context,
    ) -> Result<(u32, u32), String> {
        let file = decode_file(data)?;
        let (cell_w, cell_h) = (ctx.cell_width.max(1), ctx.cell_height.max(1));
        let screen_w = (ctx.cols as u32).saturating_mul(cell_w).max(1);
        let screen_h = (ctx.rows as u32).saturating_mul(cell_h).max(1);
        let (natural_w, natural_h) = (f64::from(file.width), f64::from(file.height));
        let wanted_w = args.width.pixels(cell_w, screen_w).map(f64::from);
        let wanted_h = args.height.pixels(cell_h, screen_h).map(f64::from);
        let (mut draw_w, mut draw_h) = match (wanted_w, wanted_h) {
            (None, None) => (natural_w, natural_h),
            (Some(w), None) => (w, w * natural_h / natural_w),
            (None, Some(h)) => (h * natural_w / natural_h, h),
            (Some(w), Some(h)) if args.preserve_aspect_ratio => {
                let scale = (w / natural_w).min(h / natural_h);
                (natural_w * scale, natural_h * scale)
            }
            (Some(w), Some(h)) => (w, h),
        };
        // Images wider than the screen shrink to fit, keeping their shape.
        if draw_w > f64::from(screen_w) {
            draw_h *= f64::from(screen_w) / draw_w;
            draw_w = f64::from(screen_w);
        }
        let draw = [(draw_w.round() as u32).max(1), (draw_h.round() as u32).max(1)];

        let id = self.internal_id();
        let mut frames = file.frames.into_iter();
        let (first, first_gap) = frames.next().ok_or("EINVAL:image has no frames")?;
        self.store(id, Decoded { width: file.width, height: file.height, rgba: first, opaque: file.opaque }, true);
        if let Some(image) = self.images.get_mut(&id) {
            image.frames[0].gap_ms = first_gap;
            for (rgba, gap_ms) in frames {
                self.memory += rgba.len();
                image.frames.push(Frame { rgba: rgba.into(), gap_ms });
            }
            if image.frames.len() > 1 {
                image.state = AnimationState::Running;
            }
        }
        self.enforce_quota(Some(id));
        let control = Control { cols: draw[0].div_ceil(cell_w), rows: draw[1].div_ceil(cell_h), ..Control::default() };
        self.place(id, &control, ctx)?;
        if let Some(placement) = self.placements.last_mut() {
            placement.scaled = false;
            placement.pixel_size = Some(draw);
            placement.cell_bound = true;
        }
        Ok((control.cols, control.rows))
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
            self.memory -= image.memory();
        }
        self.placements.retain(|p| p.image_id != id);
        self.remove_orphans();
        self.generation += 1;
    }

    /// Places image `id` at the cursor, or relative to a parent placement.
    /// Returns the cursor advance in cells.
    fn place(&mut self, id: u32, control: &Control, ctx: &Context) -> Result<Option<(u32, u32)>, String> {
        let image = self.images.get(&id).ok_or("ENOENT:no such image")?;
        let anonymous = image.anonymous;
        let x = control.source[0].min(image.width);
        let y = control.source[1].min(image.height);
        let width = if control.source[2] == 0 { image.width - x } else { control.source[2].min(image.width - x) };
        let height = if control.source[3] == 0 { image.height - y } else { control.source[3].min(image.height - y) };
        let cell_w = ctx.cell_width.max(1);
        let cell_h = ctx.cell_height.max(1);
        let cols = if control.cols > 0 { control.cols } else { (width + control.offset_x).div_ceil(cell_w).max(1) };
        let rows = if control.rows > 0 { control.rows } else { (height + control.offset_y).div_ceil(cell_h).max(1) };

        let key = (id, control.placement);
        let parent = (control.parent_image != 0).then_some((control.parent_image, control.parent_placement));
        let (line, col, alt_screen) = match parent {
            None => (ctx.cursor_line, ctx.cursor_col, ctx.alt_screen),
            Some(parent_key) => {
                let anchor = self
                    .placement(parent_key)
                    .filter(|p| !p.virtual_placement)
                    .ok_or("ENOPARENT:no such parent placement")?;
                if control.placement != 0 && (parent_key == key || self.has_ancestor(anchor, key)) {
                    return Err("ECYCLE:placement would be its own parent".into());
                }
                if self.depth(anchor) + 1 >= MAX_RELATIVE_DEPTH {
                    return Err("ETOODEEP:too many nested relative placements".into());
                }
                let col = (anchor.col as i64 + i64::from(control.parent_offset_x)).max(0) as usize;
                (anchor.line + i64::from(control.parent_offset_y), col, anchor.alt_screen)
            }
        };
        let mut moved_from = None;
        if control.placement != 0
            && let Some(index) =
                self.placements.iter().position(|p| p.image_id == id && p.placement_id == control.placement)
        {
            let old = self.placements.remove(index);
            moved_from = Some((old.line, old.col));
        }
        if anonymous && !control.placeholder {
            // Players redraw frames as new anonymous images at the same spot.
            // Replace the previous one instead of stacking them.
            let replaced: Vec<u32> = self
                .placements
                .iter()
                .filter(|p| {
                    p.line == line
                        && p.col == col
                        && p.alt_screen == alt_screen
                        && p.image_id != id
                        && self.images.get(&p.image_id).is_some_and(|i| i.anonymous)
                })
                .map(|p| p.image_id)
                .collect();
            for old in replaced {
                self.remove_image(old);
            }
        }
        self.placements.push(Placement {
            image_id: id,
            placement_id: control.placement,
            line,
            col,
            cols,
            rows,
            scaled: control.cols > 0 || control.rows > 0,
            offset_x: control.offset_x.min(cell_w - 1),
            offset_y: control.offset_y.min(cell_h - 1),
            source: [x, y, width, height],
            z: control.z,
            alt_screen,
            virtual_placement: control.placeholder,
            parent,
            pixel_size: None,
            cell_bound: false,
        });
        // Placements relative to a moved one move with it.
        if let Some((old_line, old_col)) = moved_from {
            self.shift_descendants(key, line - old_line, col as i64 - old_col as i64);
        }
        self.generation += 1;
        Ok((!control.no_cursor_move && !control.placeholder && parent.is_none()).then_some((cols, rows)))
    }

    fn placement(&self, (image, placement): (u32, u32)) -> Option<&Placement> {
        self.placements.iter().find(|p| p.image_id == image && p.placement_id == placement)
    }

    /// Number of parents above `placement`.
    fn depth(&self, placement: &Placement) -> usize {
        let mut depth = 0;
        let mut current = placement.parent;
        while let Some(parent) = current
            && depth <= MAX_RELATIVE_DEPTH
        {
            depth += 1;
            current = self.placement(parent).and_then(|p| p.parent);
        }
        depth
    }

    fn has_ancestor(&self, placement: &Placement, key: (u32, u32)) -> bool {
        let mut current = placement.parent;
        for _ in 0..=MAX_RELATIVE_DEPTH {
            match current {
                Some(parent) if parent == key => return true,
                Some(parent) => current = self.placement(parent).and_then(|p| p.parent),
                None => return false,
            }
        }
        false
    }

    fn shift_descendants(&mut self, key: (u32, u32), lines: i64, cols: i64) {
        let mut pending = vec![key];
        let mut visited = 0;
        while let Some(parent) = pending.pop()
            && visited < self.placements.len()
        {
            visited += 1;
            for child in self.placements.iter_mut().filter(|p| p.parent == Some(parent)) {
                child.line += lines;
                child.col = (child.col as i64 + cols).max(0) as usize;
                pending.push((child.image_id, child.placement_id));
            }
        }
    }

    /// Removes relative placements whose parent is gone. Returns their image ids.
    fn remove_orphans(&mut self) -> Vec<u32> {
        let mut removed = Vec::new();
        while self.placements.iter().any(|p| p.parent.is_some()) {
            let keys: std::collections::HashSet<(u32, u32)> =
                self.placements.iter().map(|p| (p.image_id, p.placement_id)).collect();
            let before = removed.len();
            self.placements.retain(|p| {
                let orphan = p.parent.is_some_and(|parent| !keys.contains(&parent));
                if orphan {
                    removed.push(p.image_id);
                }
                !orphan
            });
            if removed.len() == before {
                break;
            }
        }
        removed
    }

    /// Copies a rectangle from one animation frame onto another (`a=c`).
    fn compose(&mut self, control: &Control) -> Result<(), String> {
        let generation = self.next_image_generation();
        let image = self.images.get_mut(&control.id).ok_or("ENOENT:no such image")?;
        let count = image.frames.len();
        let (source, dest) = (control.rows as usize, control.cols as usize);
        if source == 0 || dest == 0 || source > count || dest > count {
            return Err("ENOENT:no such frame".into());
        }
        let (width, height) = (image.width as usize, image.height as usize);
        let w = if control.source[2] == 0 { width } else { control.source[2] as usize };
        let h = if control.source[3] == 0 { height } else { control.source[3] as usize };
        let (dest_x, dest_y) = (control.source[0] as usize, control.source[1] as usize);
        let (src_x, src_y) = (control.offset_x as usize, control.offset_y as usize);
        if dest_x + w > width || dest_y + h > height || src_x + w > width || src_y + h > height {
            return Err("EINVAL:rectangle outside the frame".into());
        }
        let src = image.frames[source - 1].rgba.clone();
        let mut canvas = image.frames[dest - 1].rgba.to_vec();
        let replace = control.no_cursor_move;
        for row in 0..h {
            for col in 0..w {
                let from = &src[((src_y + row) * width + src_x + col) * 4..][..4];
                let to = &mut canvas[((dest_y + row) * width + dest_x + col) * 4..][..4];
                composite(to, from, replace);
            }
        }
        image.frames[dest - 1].rgba = canvas.into();
        if dest - 1 == image.current {
            image.rgba = image.frames[dest - 1].rgba.clone();
            image.generation = generation;
        }
        self.generation += 1;
        Ok(())
    }

    /// Adds or edits an animation frame (`a=f`).
    fn add_frame(&mut self, control: &Control, data: &[u8]) -> Result<(), String> {
        let decoded = self.load(control, data)?;
        let generation = self.next_image_generation();
        let image = self.images.get_mut(&control.id).ok_or("ENOENT:no such image")?;
        let before = image.memory();
        let (width, height) = (image.width as usize, image.height as usize);
        let edit = control.rows as usize;
        let base = control.cols as usize;
        let count = image.frames.len();
        let mut canvas: Vec<u8> = if edit > 0 && edit <= count {
            image.frames[edit - 1].rgba.to_vec()
        } else if base > 0 && base <= count {
            image.frames[base - 1].rgba.to_vec()
        } else {
            control.offset_y.to_be_bytes().repeat(width * height)
        };
        let replace = control.offset_x == 1;
        let (fx, fy) = (control.source[0] as usize, control.source[1] as usize);
        for y in 0..decoded.height as usize {
            if fy + y >= height {
                break;
            }
            for x in 0..decoded.width as usize {
                if fx + x >= width {
                    break;
                }
                let src = &decoded.rgba[(y * decoded.width as usize + x) * 4..][..4];
                let dst = &mut canvas[((fy + y) * width + fx + x) * 4..][..4];
                composite(dst, src, replace);
            }
        }
        let gap_ms = if control.z > 0 {
            control.z as u32
        } else if control.z < 0 {
            0
        } else {
            DEFAULT_GAP_MS
        };
        let frame = Frame { rgba: canvas.into(), gap_ms };
        if edit > 0 && edit <= count {
            let keep_gap = image.frames[edit - 1].gap_ms;
            image.frames[edit - 1] = Frame { gap_ms: if control.z == 0 { keep_gap } else { gap_ms }, ..frame };
            if edit - 1 == image.current {
                image.rgba = image.frames[edit - 1].rgba.clone();
                image.generation = generation;
            }
        } else {
            image.frames.push(frame);
        }
        image.opaque = image.opaque && decoded.opaque;
        let after = image.memory();
        self.memory = self.memory + after - before;
        self.enforce_quota(Some(control.id));
        self.generation += 1;
        Ok(())
    }

    /// Starts, stops or adjusts an animation (`a=a`).
    fn control_animation(&mut self, control: &Control) -> Result<(), String> {
        let generation = self.next_image_generation();
        let image = self.images.get_mut(&control.id).ok_or("ENOENT:no such image")?;
        let now = Instant::now();
        let state = match control.width {
            1 => Some(AnimationState::Stopped),
            2 => Some(AnimationState::Loading),
            3 => Some(AnimationState::Running),
            _ => None,
        };
        if let Some(state) = state {
            if state != AnimationState::Stopped && image.state == AnimationState::Stopped {
                image.frame_started = Some(now);
            }
            image.state = state;
        }
        if control.height > 0 {
            image.loops_left = if control.height == 1 { None } else { Some(control.height - 1) };
        }
        if control.cols > 0 && !image.frames.is_empty() {
            image.current = (control.cols as usize - 1).min(image.frames.len() - 1);
            image.rgba = image.frames[image.current].rgba.clone();
            image.generation = generation;
            image.frame_started = Some(now);
        }
        if control.rows > 0
            && control.z != 0
            && let Some(frame) = image.frames.get_mut(control.rows as usize - 1)
        {
            frame.gap_ms = control.z.max(0) as u32;
        }
        self.generation += 1;
        Ok(())
    }

    /// Advances running animations. Returns when the next frame is due.
    pub fn tick(&mut self, now: Instant) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        let mut generation = self.image_generation;
        let mut changed = false;
        for image in self.images.values_mut() {
            if image.frames.len() < 2 || image.state == AnimationState::Stopped {
                continue;
            }
            let started = *image.frame_started.get_or_insert(now);
            let gap = Duration::from_millis(u64::from(image.frames[image.current].gap_ms));
            if now >= started + gap {
                let last = image.current + 1 == image.frames.len();
                if last {
                    if image.state == AnimationState::Loading {
                        continue;
                    }
                    match image.loops_left {
                        Some(0) => {
                            image.state = AnimationState::Stopped;
                            continue;
                        }
                        Some(n) => image.loops_left = Some(n - 1),
                        None => {}
                    }
                }
                image.current = (image.current + 1) % image.frames.len();
                image.rgba = image.frames[image.current].rgba.clone();
                generation += 1;
                image.generation = generation;
                image.frame_started = Some(now);
                changed = true;
            }
            let due = image.frame_started.unwrap_or(now)
                + Duration::from_millis(u64::from(image.frames[image.current].gap_ms.max(1)));
            next = Some(next.map_or(due, |n| n.min(due)));
        }
        self.image_generation = generation;
        if changed {
            self.generation += 1;
        }
        next
    }

    fn delete(&mut self, control: &Control, ctx: &Context) {
        let free = control.delete.is_ascii_uppercase();
        let on_screen = |p: &Placement| {
            !p.virtual_placement
                && p.alt_screen == ctx.alt_screen
                && p.line + i64::from(p.rows) > ctx.screen_top
                && p.line < ctx.screen_top + ctx.rows as i64
        };
        let covers = |p: &Placement, line: i64, col: usize| {
            !p.virtual_placement
                && p.alt_screen == ctx.alt_screen
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
        affected.extend(self.remove_orphans());
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

    /// Moves primary screen placements after a reflow renumbered lines.
    /// Placements whose rows no longer exist are removed.
    pub fn remap_lines(&mut self, map: impl Fn(i64) -> Option<i64>) {
        if self.placements.is_empty() {
            return;
        }
        self.placements.retain_mut(|p| {
            if p.alt_screen || p.virtual_placement {
                return true;
            }
            // The top row may already be gone from history; use the first row that remains.
            match (0..i64::from(p.rows)).find_map(|k| map(p.line + k).map(|new| new - k)) {
                Some(line) => {
                    p.line = line;
                    true
                }
                None => false,
            }
        });
        self.remove_orphans();
        self.generation += 1;
    }

    /// Drops placements that scrolled out of history.
    pub fn prune(&mut self, oldest_line: i64, alt_screen: bool) {
        if self.placements.is_empty() {
            return;
        }
        let before = self.placements.len();
        self.placements
            .retain(|p| p.virtual_placement || p.alt_screen != alt_screen || p.line + i64::from(p.rows) > oldest_line);
        if self.placements.len() != before {
            self.remove_orphans();
            self.generation += 1;
        }
    }

    /// Removes cell bound placements that overlap the given lines and columns,
    /// freeing their images.
    pub fn erase_cells(&mut self, lines: std::ops::Range<i64>, cols: std::ops::Range<usize>, alt_screen: bool) {
        let mut erased = Vec::new();
        self.placements.retain(|p| {
            let hit = p.cell_bound
                && p.alt_screen == alt_screen
                && p.line < lines.end
                && p.line + i64::from(p.rows) > lines.start
                && p.col < cols.end
                && p.col + p.cols as usize > cols.start;
            if hit {
                erased.push(p.image_id);
            }
            !hit
        });
        if erased.is_empty() {
            return;
        }
        for id in erased {
            if !self.placements.iter().any(|p| p.image_id == id) {
                self.remove_image(id);
            }
        }
        self.remove_orphans();
        self.generation += 1;
    }

    /// Removes placements visible on the given screen (screen clear).
    pub fn clear_screen(&mut self, screen_top: i64, rows: usize, alt_screen: bool) {
        let before = self.placements.len();
        self.placements.retain(|p| {
            p.virtual_placement
                || p.alt_screen != alt_screen
                || p.line + i64::from(p.rows) <= screen_top
                || p.line >= screen_top + rows as i64
        });
        if self.placements.len() != before {
            self.remove_orphans();
            self.generation += 1;
        }
    }

    pub fn clear_alt_screen(&mut self) {
        let before = self.placements.len();
        self.placements.retain(|p| !p.alt_screen || p.virtual_placement);
        if self.placements.len() != before {
            self.remove_orphans();
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

/// A width or height in an iTerm2 inline image request.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum InlineSize {
    /// The image's own size.
    #[default]
    Auto,
    Cells(u32),
    Pixels(u32),
    /// Percent of the terminal's width or height.
    Percent(u32),
}

impl InlineSize {
    fn parse(value: &[u8]) -> Self {
        let text = std::str::from_utf8(value).unwrap_or("").trim();
        let number = |digits: &str| digits.parse::<u32>().ok();
        if let Some(pixels) = text.strip_suffix("px") {
            number(pixels).map_or(Self::Auto, Self::Pixels)
        } else if let Some(percent) = text.strip_suffix('%') {
            number(percent).map_or(Self::Auto, Self::Percent)
        } else {
            number(text).map_or(Self::Auto, Self::Cells)
        }
    }

    /// Size in pixels, `None` for automatic.
    fn pixels(self, cell: u32, screen: u32) -> Option<u32> {
        match self {
            Self::Auto => None,
            Self::Cells(n) => Some(n.saturating_mul(cell)),
            Self::Pixels(n) => Some(n),
            Self::Percent(percent) => Some(screen / 100 * percent.min(100)),
        }
        .filter(|&pixels| pixels > 0)
    }
}

/// Arguments of an iTerm2 `File=` or `MultipartFile=` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineImageArgs {
    pub width: InlineSize,
    pub height: InlineSize,
    pub preserve_aspect_ratio: bool,
    /// Only inline files are shown; others would be downloads.
    pub inline: bool,
}

impl InlineImageArgs {
    /// Parses `key=value` pairs separated by `;`.
    pub fn parse(args: &[u8]) -> Self {
        let mut parsed =
            Self { width: InlineSize::Auto, height: InlineSize::Auto, preserve_aspect_ratio: true, inline: false };
        for pair in args.split(|&b| b == b';') {
            let Some(equals) = memchr::memchr(b'=', pair) else { continue };
            let (key, value) = (&pair[..equals], &pair[equals + 1..]);
            match key {
                b"width" => parsed.width = InlineSize::parse(value),
                b"height" => parsed.height = InlineSize::parse(value),
                b"preserveAspectRatio" => parsed.preserve_aspect_ratio = value != b"0",
                b"inline" => parsed.inline = value == b"1",
                _ => {}
            }
        }
        parsed
    }
}

/// A decoded image file: frames of straight alpha RGBA with display times in milliseconds.
struct DecodedFile {
    width: u32,
    height: u32,
    frames: Vec<(Vec<u8>, u32)>,
    opaque: bool,
}

/// Decodes PNG, JPEG or GIF data, recognized by its signature.
fn decode_file(data: &[u8]) -> Result<DecodedFile, String> {
    if data.starts_with(b"\x89PNG") {
        let png = decode_png(data)?;
        return Ok(DecodedFile {
            width: png.width,
            height: png.height,
            opaque: png.opaque,
            frames: vec![(png.rgba, DEFAULT_GAP_MS)],
        });
    }
    if data.starts_with(&[0xff, 0xd8]) {
        return decode_jpeg(data);
    }
    if data.starts_with(b"GIF8") {
        return decode_gif(data);
    }
    Err("EINVAL:unsupported image format".into())
}

fn decode_jpeg(data: &[u8]) -> Result<DecodedFile, String> {
    use zune_jpeg::zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(MAX_DIMENSION as usize)
        .set_max_height(MAX_DIMENSION as usize);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(data), options);
    let rgba = decoder.decode().map_err(|e| format!("EINVAL:bad JPEG: {e:?}"))?;
    let info = decoder.info().ok_or("EINVAL:bad JPEG")?;
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    if rgba.len() != width as usize * height as usize * 4 {
        return Err("EINVAL:bad JPEG".into());
    }
    Ok(DecodedFile { width, height, frames: vec![(rgba, DEFAULT_GAP_MS)], opaque: true })
}

/// Decodes every frame of a GIF onto a full size canvas, applying disposal.
fn decode_gif(data: &[u8]) -> Result<DecodedFile, String> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    options.set_memory_limit(gif::MemoryLimit::Bytes(NonZeroU64::new(MAX_TRANSFER as u64).expect("limit is not zero")));
    let mut decoder = options.read_info(std::io::Cursor::new(data)).map_err(|e| format!("EINVAL:bad GIF: {e}"))?;
    let (width, height) = (u32::from(decoder.width()), u32::from(decoder.height()));
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err("EINVAL:invalid image dimensions".into());
    }
    let (w, h) = (width as usize, height as usize);
    let mut canvas = vec![0u8; w * h * 4];
    let mut frames = Vec::new();
    let mut total = 0;
    while let Some(frame) = decoder.read_next_frame().map_err(|e| format!("EINVAL:bad GIF: {e}"))? {
        let previous = (frame.dispose == gif::DisposalMethod::Previous).then(|| canvas.clone());
        let (left, top) = (usize::from(frame.left), usize::from(frame.top));
        let (fw, fh) = (usize::from(frame.width), usize::from(frame.height));
        for y in 0..fh.min(h.saturating_sub(top)) {
            for x in 0..fw.min(w.saturating_sub(left)) {
                let Some(pixel) = frame.buffer.get((y * fw + x) * 4..(y * fw + x) * 4 + 4) else { continue };
                if pixel[3] != 0 {
                    canvas[((top + y) * w + left + x) * 4..][..4].copy_from_slice(pixel);
                }
            }
        }
        // Browsers treat delays of 10 ms or less as 100 ms; so do most terminals.
        let gap_ms = match u32::from(frame.delay) * 10 {
            0..=10 => 100,
            ms => ms,
        };
        total += canvas.len();
        frames.push((canvas.clone(), gap_ms));
        match frame.dispose {
            gif::DisposalMethod::Background => {
                for y in top..(top + fh).min(h) {
                    canvas[(y * w + left.min(w)) * 4..(y * w + (left + fw).min(w)) * 4].fill(0);
                }
            }
            gif::DisposalMethod::Previous => {
                if let Some(previous) = previous {
                    canvas = previous;
                }
            }
            _ => {}
        }
        if frames.len() == MAX_GIF_FRAMES || total > MAX_TRANSFER {
            break;
        }
    }
    let opaque = frames.iter().all(|(rgba, _)| rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
    Ok(DecodedFile { width, height, frames, opaque })
}

/// Blends `src` over `dst`, both straight alpha RGBA. `replace` copies instead.
fn composite(dst: &mut [u8], src: &[u8], replace: bool) {
    if replace || src[3] == 255 {
        dst.copy_from_slice(src);
        return;
    }
    let sa = u32::from(src[3]);
    let da = u32::from(dst[3]) * (255 - sa) / 255;
    let out_a = sa + da;
    if out_a == 0 {
        dst.copy_from_slice(&[0, 0, 0, 0]);
        return;
    }
    for c in 0..3 {
        dst[c] = ((u32::from(src[c]) * sa + u32::from(dst[c]) * da) / out_a) as u8;
    }
    dst[3] = out_a as u8;
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

fn read_limited(mut file: std::fs::File, control: &Control) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    if control.offset > 0 {
        std::io::copy(&mut (&mut file).take(control.offset as u64), &mut std::io::sink())
            .map_err(|e| format!("EBADF:{e}"))?;
    }
    let limit = if control.size > 0 { control.size } else { MAX_TRANSFER };
    file.take(limit as u64).read_to_end(&mut data).map_err(|e| format!("EBADF:{e}"))?;
    Ok(data)
}

fn read_file(path: &str, control: &Control) -> Result<Vec<u8>, String> {
    // Never read device or kernel files, whatever an application asks for.
    let canonical = std::fs::canonicalize(path).map_err(|e| format!("EBADF:{e}"))?;
    let denied = ["/proc", "/sys", "/dev"].iter().any(|prefix| canonical.starts_with(prefix))
        && !canonical.starts_with("/dev/shm");
    if denied {
        return Err("EPERM:refusing to read system files".into());
    }
    let metadata = std::fs::metadata(&canonical).map_err(|e| format!("EBADF:{e}"))?;
    if !metadata.is_file() {
        return Err("EINVAL:not a regular file".into());
    }
    let file = std::fs::File::open(&canonical).map_err(|e| format!("EBADF:{e}"))?;
    let data = read_limited(file, control)?;
    if control.medium == b't' {
        // Temporary files must be deleted after reading, but only obvious ones.
        // Directories are canonicalized too: on macOS they are links into /private.
        let in_temp = [std::env::temp_dir(), "/tmp".into(), "/dev/shm".into()]
            .into_iter()
            .any(|dir| canonical.starts_with(std::fs::canonicalize(&dir).unwrap_or(dir)));
        if in_temp && path.contains("tty-graphics-protocol") {
            let _ = std::fs::remove_file(&canonical);
        }
    }
    Ok(data)
}

/// Reads a POSIX shared memory object by name and unlinks it, as the spec requires.
fn read_shared_memory(name: &str, control: &Control) -> Result<Vec<u8>, String> {
    let name = name.trim_start_matches('/');
    if name.is_empty() || name.contains('/') || name.contains("..") {
        return Err("EINVAL:invalid shared memory name".into());
    }
    let name = format!("/{name}");
    let data = map_shared_memory(&name, control);
    let _ = rustix::shm::unlink(name.as_str());
    data
}

/// Copies the requested range of a shared memory object. It is mapped rather
/// than read, because macOS cannot `read` shared memory.
fn map_shared_memory(name: &str, control: &Control) -> Result<Vec<u8>, String> {
    use rustix::mm::{MapFlags, ProtFlags};
    let fd = rustix::shm::open(name, rustix::shm::OFlags::RDONLY, rustix::fs::Mode::empty())
        .map_err(|e| format!("EBADF:{e}"))?;
    // macOS rounds the object up to whole pages; the `S` key gives the exact size.
    let len = usize::try_from(rustix::fs::fstat(&fd).map_err(|e| format!("EBADF:{e}"))?.st_size).unwrap_or(0);
    let start = control.offset.min(len);
    let limit = if control.size > 0 { control.size } else { MAX_TRANSFER };
    let count = (len - start).min(limit);
    if count == 0 {
        return Ok(Vec::new());
    }
    // SAFETY: a read-only mapping of the whole object, unmapped before returning.
    // Shared, because macOS refuses private mappings of shared memory.
    let data = unsafe {
        let ptr = rustix::mm::mmap(std::ptr::null_mut(), len, ProtFlags::READ, MapFlags::SHARED, &fd, 0)
            .map_err(|e| format!("EBADF:{e}"))?;
        let data = std::slice::from_raw_parts(ptr.cast::<u8>().add(start), count).to_vec();
        let _ = rustix::mm::munmap(ptr, len);
        data
    };
    Ok(data)
}

fn decode_png(data: &[u8]) -> Result<Decoded, String> {
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
    let (rgba, opaque) = match info.color_type {
        png::ColorType::Rgba => {
            let opaque = buffer.as_chunks::<4>().0.iter().all(|p| p[3] == 255);
            (buffer, opaque)
        }
        png::ColorType::Rgb => (buffer.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect(), true),
        png::ColorType::GrayscaleAlpha => {
            (buffer.as_chunks::<2>().0.iter().flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(), false)
        }
        png::ColorType::Grayscale => (buffer.iter().flat_map(|&g| [g, g, g, 255]).collect(), true),
        png::ColorType::Indexed => return Err("EINVAL:unexpanded palette PNG".into()),
    };
    Ok(Decoded { width, height, rgba, opaque })
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
        assert!(g.images()[&7].opaque);
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

    /// Creates a POSIX shared memory object holding `data`, as clients do.
    fn create_shared_memory(name: &str, data: &[u8]) {
        use rustix::mm::{MapFlags, ProtFlags};
        use rustix::shm::OFlags;
        let fd = rustix::shm::open(
            format!("/{name}").as_str(),
            OFlags::CREATE | OFlags::EXCL | OFlags::RDWR,
            rustix::fs::Mode::from_raw_mode(0o600),
        )
        .unwrap();
        rustix::fs::ftruncate(&fd, data.len() as u64).unwrap();
        // SAFETY: a fresh shared mapping of exactly `data.len()` bytes.
        unsafe {
            let ptr =
                rustix::mm::mmap(std::ptr::null_mut(), data.len(), ProtFlags::WRITE, MapFlags::SHARED, &fd, 0).unwrap();
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr.cast::<u8>(), data.len());
            rustix::mm::munmap(ptr, data.len()).unwrap();
        }
    }

    fn shared_memory_exists(name: &str) -> bool {
        rustix::shm::open(format!("/{name}").as_str(), rustix::shm::OFlags::RDONLY, rustix::fs::Mode::empty()).is_ok()
    }

    #[test]
    fn shared_memory_transfer_is_read_and_unlinked() {
        let name = format!("tron-test-{}", std::process::id());
        create_shared_memory(&name, &[9u8; 3]);
        let mut g = Graphics::new();
        let encoded = BASE64.encode(&name);
        let out = g.handle(format!("a=t,t=s,f=24,s=1,v=1,i=4;{encoded}").as_bytes(), &ctx());
        assert_eq!(out.response.as_deref(), Some(&b"\x1b_Gi=4;OK\x1b\\"[..]));
        assert_eq!(&g.images()[&4].rgba[..], &[9, 9, 9, 255]);
        assert!(!shared_memory_exists(&name));
    }

    #[test]
    fn shared_memory_with_more_flag_is_not_chunked() {
        let name = format!("tron-test-more-{}", std::process::id());
        create_shared_memory(&name, &[5u8; 3]);
        let mut g = Graphics::new();
        let encoded = BASE64.encode(&name);
        g.handle(format!("a=T,t=s,f=24,s=1,v=1,C=1,q=2,m=1;{encoded}").as_bytes(), &ctx());
        assert_eq!(g.images().len(), 1);
        // A new command after an unfinished direct transfer is not taken as a chunk.
        g.handle(b"a=T,f=24,s=1,v=1,i=9,m=1;AAAA", &ctx());
        let out = g.handle(b"a=p,i=77", &ctx());
        assert!(String::from_utf8(out.response.unwrap()).unwrap().contains("ENOENT"));
    }

    #[test]
    fn animation_frames_advance() {
        let mut g = Graphics::new();
        let red = BASE64.encode([255u8, 0, 0]);
        let blue = BASE64.encode([0u8, 0, 255]);
        g.handle(format!("a=t,f=24,s=1,v=1,i=2,q=1;{red}").as_bytes(), &ctx());
        g.handle(format!("a=f,f=24,s=1,v=1,i=2,z=10,q=1;{blue}").as_bytes(), &ctx());
        assert_eq!(g.images()[&2].frame_count(), 2);
        let start = Instant::now();
        assert!(g.tick(start).is_none(), "stopped animations do not tick");
        g.handle(b"a=a,i=2,s=3,v=1,q=1", &ctx());
        let due = g.tick(start).expect("running animation schedules a frame");
        g.tick(due + Duration::from_millis(1));
        assert_eq!(&g.images()[&2].rgba[..], &[0, 0, 255, 255]);
    }

    #[test]
    fn anonymous_images_at_same_spot_replace_each_other() {
        let mut g = Graphics::new();
        let data = BASE64.encode([1u8, 2, 3]);
        for _ in 0..5 {
            g.handle(format!("a=T,f=24,s=1,v=1,C=1,q=2;{data}").as_bytes(), &ctx());
        }
        assert_eq!(g.images().len(), 1);
        assert_eq!(g.placements().len(), 1);
    }

    #[test]
    fn frames_compose_rectangles() {
        let mut g = Graphics::new();
        let red = BASE64.encode([255u8, 0, 0, 255, 0, 0]);
        let blue = BASE64.encode([0u8, 0, 255, 0, 0, 255]);
        g.handle(format!("a=t,f=24,s=2,v=1,i=1,q=1;{red}").as_bytes(), &ctx());
        g.handle(format!("a=f,f=24,s=2,v=1,i=1,q=1;{blue}").as_bytes(), &ctx());
        let out = g.handle(b"a=c,i=1,r=2,c=1,x=1,w=1,h=1,C=1", &ctx());
        assert_eq!(out.response.as_deref(), Some(&b"\x1b_Gi=1;OK\x1b\\"[..]));
        assert_eq!(&g.images()[&1].rgba[..], &[255, 0, 0, 255, 0, 0, 255, 255]);
        let out = g.handle(b"a=c,i=1,r=2,c=1,x=1,w=2,h=1", &ctx());
        assert!(String::from_utf8(out.response.unwrap()).unwrap().contains("EINVAL"));
    }

    #[test]
    fn relative_placements_follow_and_die_with_their_parent() {
        let mut g = Graphics::new();
        let data = BASE64.encode([0u8; 3]);
        g.handle(format!("a=t,f=24,s=1,v=1,i=1,q=2;{data}").as_bytes(), &ctx());
        g.handle(format!("a=t,f=24,s=1,v=1,i=2,q=2;{data}").as_bytes(), &ctx());
        let out = g.handle(b"a=p,i=2,p=1,P=1,Q=1", &ctx());
        assert!(String::from_utf8(out.response.unwrap()).unwrap().contains("ENOPARENT"));
        g.handle(b"a=p,i=1,p=1,q=2", &ctx());
        let out = g.handle(b"a=p,i=2,p=1,P=1,Q=1,H=3,V=-1,q=2", &ctx());
        assert_eq!(out.cursor_advance, None);
        let child = |g: &Graphics| g.placements().iter().find(|p| p.image_id == 2).map(|p| (p.line, p.col));
        assert_eq!(child(&g), Some((4, 5)));
        let moved = Context { cursor_line: 10, cursor_col: 0, ..ctx() };
        g.handle(b"a=p,i=1,p=1,q=2", &moved);
        assert_eq!(child(&g), Some((9, 3)));
        g.handle(b"a=d,d=i,i=1", &ctx());
        assert!(g.placements().is_empty());
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&vec![200; (width * height * 4) as usize]).unwrap();
        writer.finish().unwrap();
        out
    }

    #[test]
    fn inline_images_are_sized_like_iterm2() {
        let args = InlineImageArgs::parse(b"name=eA==;size=10;width=4;height=50%;inline=1");
        assert_eq!(args.width, InlineSize::Cells(4));
        assert_eq!(args.height, InlineSize::Percent(50));
        assert!(args.inline && args.preserve_aspect_ratio);

        let mut g = Graphics::new();
        let args = InlineImageArgs::parse(b"width=4;inline=1");
        let advance = g.add_inline_image(&png(20, 10), &args, &ctx()).unwrap();
        assert_eq!(advance, (4, 1));
        let p = &g.placements()[0];
        assert_eq!((p.pixel_size, p.line, p.col), (Some([40, 20]), 5, 2));

        // Natural size wider than the 800 pixel screen shrinks to fit.
        let args = InlineImageArgs::parse(b"inline=1");
        assert_eq!(g.add_inline_image(&png(1600, 400), &args, &ctx()).unwrap(), (80, 10));
        assert!(g.add_inline_image(b"not an image", &args, &ctx()).is_err());
    }

    #[test]
    fn animated_gifs_decode_every_frame() {
        let mut data = Vec::new();
        {
            let mut encoder = gif::Encoder::new(&mut data, 2, 1, &[255, 0, 0, 0, 0, 255]).unwrap();
            for index in [0u8, 1] {
                let mut frame = gif::Frame::from_indexed_pixels(2, 1, vec![index, index], None);
                frame.delay = 5;
                encoder.write_frame(&frame).unwrap();
            }
        }
        let file = decode_file(&data).unwrap();
        assert_eq!((file.width, file.height, file.frames.len()), (2, 1, 2));
        assert_eq!(&file.frames[1].0[..4], &[0, 0, 255, 255]);
        assert_eq!(file.frames[0].1, 50);
    }

    #[test]
    fn placeholder_cells_decode_ids_rows_and_columns() {
        let cell = Cell { ch: PLACEHOLDER, fg: Color::rgb(0, 0, 42), ..Cell::BLANK };
        let first = placeholder_cell(&cell, Color::DEFAULT, Some("\u{305}\u{30D}"), None).unwrap();
        assert_eq!((first.image_id, first.row, first.col), (42, 0, 1));
        let next = placeholder_cell(&cell, Color::DEFAULT, None, Some(first)).unwrap();
        assert_eq!((next.row, next.col), (0, 2));
        assert!(placeholder_cell(&Cell::BLANK, Color::DEFAULT, None, None).is_none());
    }
}
