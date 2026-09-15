//! Cell pipeline: backgrounds, shaped text, decorations, selection and cursor,
//! all drawn as instances of one quad.

use foldhash::HashMap;
use std::mem::size_of;
use std::ops::Range;

use bytemuck::{Pod, Zeroable};
use tron_core::{
    Cell, Color, ColorKind, CursorShape, Extended, Flags, LineSize, Modes, PLACEHOLDER, Palette, Row, SelectionRange,
    Snapshot, TextSize,
};
use tron_font::{CellMetrics, FontSystem, GlyphFormat, GlyphKey, ShapedGlyph, Style};
use unicode_width::UnicodeWidthChar;

use crate::atlas::Atlas;
use crate::{GlowLine, LinkHighlight, Overlay, Scrollbar, Theme};

const INITIAL_ATLAS_SIZE: u32 = 1024;
const SHAPE_CACHE_LIMIT: usize = 16_384;

const KIND_SOLID: u32 = 0;
const KIND_MASK: u32 = 1;
const KIND_COLOR: u32 = 2;
const KIND_CURLY: u32 = 3;
const KIND_ROUNDED: u32 = 4;
const KIND_GLOW: u32 = 5;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, PartialEq)]
struct Instance {
    pos: [f32; 2],
    size: [f32; 2],
    uv: [f32; 4],
    color: [f32; 4],
    kind: u32,
}

const INSTANCE_ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
    0 => Float32x2,
    1 => Float32x2,
    2 => Float32x4,
    3 => Float32x4,
    4 => Uint32,
];

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct Uniforms {
    viewport: [f32; 4],
    cursor_rect: [f32; 4],
    cursor_text: [f32; 4],
}

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
enum GlyphSource {
    Font(GlyphKey, GlyphScale),
    Sprite(char, GlyphScale),
}

/// Horizontal and vertical glyph scale in sixteenths, part of the glyph cache key.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
struct GlyphScale {
    x: u16,
    y: u16,
}

impl GlyphScale {
    const ONE: Self = Self { x: 16, y: 16 };

    fn new(x: f32, y: f32) -> Self {
        let sixteenths = |v: f32| (v * 16.0).round().clamp(1.0, f32::from(u16::MAX)) as u16;
        Self { x: sixteenths(x), y: sixteenths(y) }
    }

    fn factors(self) -> (f32, f32) {
        (f32::from(self.x) / 16.0, f32::from(self.y) / 16.0)
    }
}

/// Where a row's cells are drawn: stretched on double size lines and reordered
/// when the row holds right-to-left text.
struct RowLayout {
    padding: f32,
    /// Width of one column on screen.
    cell_width: f32,
    cell_height: f32,
    scale_x: f32,
    scale_y: f32,
    /// Added to glyph positions. The bottom half of double height text draws the lower half.
    glyph_shift: f32,
    /// Glyphs are cut at the row edges (double height halves).
    clipped: bool,
    /// Columns drawn: half the row on double size lines.
    cols: usize,
    /// Screen column and direction of every column, for rows with right-to-left text.
    bidi: Option<(Vec<usize>, Vec<bool>)>,
}

impl RowLayout {
    fn new(row: &Row, padding: f32, cell_w: f32, cell_h: f32, bidi: bool) -> Self {
        let (scale_x, scale_y, glyph_shift) = match row.line_size {
            LineSize::Single => (1.0, 1.0, 0.0),
            LineSize::DoubleWidth => (2.0, 1.0, 0.0),
            LineSize::DoubleHeightTop => (2.0, 2.0, 0.0),
            LineSize::DoubleHeightBottom => (2.0, 2.0, -cell_h),
        };
        let cols = if row.line_size == LineSize::Single { row.cells.len() } else { (row.cells.len() / 2).max(1) };
        Self {
            padding,
            cell_width: cell_w * scale_x,
            cell_height: cell_h,
            scale_x,
            scale_y,
            glyph_shift,
            clipped: scale_y > 1.0,
            cols,
            bidi: if bidi { bidi_order(row, cols) } else { None },
        }
    }

    fn left(&self, x: usize) -> f32 {
        let column = self.bidi.as_ref().map_or(x, |(visual, _)| visual[x]);
        self.padding + column as f32 * self.cell_width
    }

    fn is_rtl(&self, x: usize) -> bool {
        self.bidi.as_ref().is_some_and(|(_, rtl)| rtl[x])
    }

    fn glyph_scale(&self) -> GlyphScale {
        GlyphScale::new(self.scale_x, self.scale_y)
    }

    fn push_glyph(&self, out: &mut Vec<Instance>, instance: Instance) {
        if !self.clipped {
            out.push(instance);
        } else if let Some(clipped) = clip_vertical(instance, 0.0, self.cell_height) {
            out.push(clipped);
        }
    }
}

/// Screen order of a row with right-to-left characters, by the Unicode
/// bidirectional algorithm with a left-to-right paragraph: the screen column and
/// direction of each column. `None` when the row has no right-to-left text.
fn bidi_order(row: &Row, cols: usize) -> Option<(Vec<usize>, Vec<bool>)> {
    let cells = &row.cells[..cols];
    if !cells.iter().any(|cell| is_rtl_char(cell.ch)) {
        return None;
    }
    let mut text = String::with_capacity(cols);
    let mut units = Vec::with_capacity(cols);
    for (x, cell) in cells.iter().enumerate() {
        if cell.flags.contains(Flags::WIDE_SPACER) && x > 0 && cells[x - 1].flags.contains(Flags::WIDE) {
            continue;
        }
        text.push(if cell.ch == '\0' || cell.ch.is_control() { ' ' } else { cell.ch });
        units.push((x, if cell.flags.contains(Flags::WIDE) { 2 } else { 1 }));
    }
    let info = unicode_bidi::BidiInfo::new(&text, Some(unicode_bidi::Level::ltr()));
    let paragraph = info.paragraphs.first()?;
    let levels = info.reordered_levels_per_char(paragraph, paragraph.range.clone());
    if levels.len() != units.len() {
        return None;
    }
    let mut visual = vec![0; cols];
    let mut rtl = vec![false; cols];
    let mut column = 0;
    for index in unicode_bidi::BidiInfo::reorder_visual(&levels) {
        let (x, width) = units[index];
        for offset in 0..width.min(cols - x) {
            visual[x + offset] = column + offset;
            rtl[x + offset] = levels[index].is_rtl();
        }
        column += width;
    }
    Some((visual, rtl))
}

/// Characters of right-to-left scripts: Hebrew, Arabic, Syriac, Thaana, NKo and others.
#[inline]
fn is_rtl_char(ch: char) -> bool {
    ch >= '\u{590}'
        && matches!(u32::from(ch), 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x1_0800..=0x1_0FFF | 0x1_E800..=0x1_EFFF)
}

/// Cuts a quad to the rows between `top` and `bottom`, adjusting its texture coordinates.
fn clip_vertical(instance: Instance, top: f32, bottom: f32) -> Option<Instance> {
    let (start, end) = (instance.pos[1], instance.pos[1] + instance.size[1]);
    let (clip_start, clip_end) = (start.max(top), end.min(bottom));
    if clip_end <= clip_start {
        return None;
    }
    let texels = (instance.uv[3] - instance.uv[1]) / instance.size[1];
    let mut clipped = instance;
    clipped.pos[1] = clip_start;
    clipped.size[1] = clip_end - clip_start;
    if instance.kind != KIND_SOLID {
        clipped.uv[1] = instance.uv[1] + (clip_start - start) * texels;
        clipped.uv[3] = instance.uv[1] + (clip_end - start) * texels;
    }
    Some(clipped)
}

/// Whether the other cells of a scaled text block starting at `x` are still in place on this row.
fn multicell_intact(row: &Row, x: usize, size: &TextSize, colors: &ColorContext<'_>) -> bool {
    (1..size.cells().0).all(|dx| {
        row.cells.get(x + dx).is_some_and(|cell| {
            colors.extended(cell.extended).size.is_some_and(|s| usize::from(s.dx) == dx && s.dy == 0)
        })
    })
}

#[derive(Copy, Clone)]
struct GlyphEntry {
    format: GlyphFormat,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    left: i32,
    top: i32,
}

#[derive(Default)]
struct RowInstances {
    background: Vec<Instance>,
    foreground: Vec<Instance>,
}

/// Text collected for one shaping call: same face and style, no gaps.
#[derive(Default)]
struct Run {
    active: bool,
    face: u32,
    style: Option<Style>,
    text: String,
    /// Byte offset in `text`, column, cell width.
    cells: Vec<(u32, usize, u8)>,
    /// The cursor sits on the last cell. The next cell starts a new run so
    /// ligatures never span the cursor.
    split_after: bool,
    /// Shaped right to left.
    rtl: bool,
}

/// Colors needed to resolve cells.
struct ColorContext<'a> {
    palette: &'a Palette,
    theme: &'a Theme,
    reverse: bool,
    srgb: bool,
    selection: Option<SelectionRange>,
    extended: &'a [Extended],
}

impl ColorContext<'_> {
    #[inline]
    fn extended(&self, id: u16) -> &Extended {
        self.extended.get(usize::from(id)).unwrap_or(&Extended::DEFAULT)
    }

    fn rgba(&self, color: [u8; 3]) -> [f32; 4] {
        to_rgba(color, self.srgb)
    }

    /// Foreground color and, when it differs from the window background, background color.
    fn cell(&self, cell: &Cell, selected: bool) -> ([f32; 4], Option<[f32; 4]>) {
        let palette = self.palette;
        let fg_color = match cell.fg.kind() {
            ColorKind::Indexed(i) if i < 8 && self.theme.bold_is_bright && cell.flags.contains(Flags::BOLD) => {
                Color::indexed(i + 8)
            }
            _ => cell.fg,
        };
        let mut fg = palette.resolve(fg_color, palette.foreground);
        let mut bg = palette.resolve(cell.bg, palette.background);
        let inverse = cell.flags.contains(Flags::INVERSE) != self.reverse;
        if inverse {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.flags.contains(Flags::DIM) {
            fg = fg.map(|c| (u16::from(c) * 2 / 3) as u8);
        }
        let mut show_background = inverse || !cell.bg.is_default();
        if selected {
            bg = self.theme.selection_background;
            show_background = true;
            if let Some(color) = self.theme.selection_foreground {
                fg = color;
            }
        }
        (self.rgba(fg), show_background.then(|| self.rgba(bg)))
    }
}

pub fn to_rgba(color: [u8; 3], srgb: bool) -> [f32; 4] {
    let channel = |c: u8| {
        let v = f32::from(c) / 255.0;
        if !srgb {
            v
        } else if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    [channel(color[0]), channel(color[1]), channel(color[2]), 1.0]
}

pub struct CellPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    mask_atlas: Atlas,
    color_atlas: Atlas,
    max_atlas_size: u32,
    atlas_full: Option<GlyphFormat>,
    glyphs: HashMap<GlyphSource, Option<GlyphEntry>>,
    shape_cache: HashMap<u32, HashMap<String, Vec<ShapedGlyph>>>,
    shape_cache_len: usize,
    rows: Vec<RowInstances>,
    frame: Vec<Instance>,
    /// The frame and uniforms last written to the GPU.
    uploaded: Vec<Instance>,
    uploaded_uniforms: Uniforms,
    split: usize,
    uniforms: Uniforms,
    metrics: CellMetrics,
    padding: [f32; 2],
    full_rebuild: bool,
    last_cursor: Option<(usize, usize)>,
    selection: Option<SelectionRange>,
    palette_generation: u64,
    reverse: bool,
    cursor_rect: [f32; 4],
    cursor_hidden: bool,
    link_highlight: Option<LinkHighlight>,
    /// Instances of recently built rows, keyed by row content, reused when content scrolls.
    row_cache: HashMap<u64, RowInstances>,
    clear_row_cache: bool,
    overlays: Vec<Overlay>,
    scrollbar: Option<Scrollbar>,
    glow_line: Option<GlowLine>,
    overlay_instances: RowInstances,
    bidi: bool,
    flash: f32,
    // Scratch buffers reused across rows.
    foreground: Vec<[f32; 4]>,
    run: Run,
    shaped: Vec<ShapedGlyph>,
}

impl CellPipeline {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        metrics: CellMetrics,
        padding: [f32; 2],
        cache: Option<&wgpu::PipelineCache>,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("cells.wgsl"));
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cells"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_entry(1),
                texture_entry(2),
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cells"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cells"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &INSTANCE_ATTRIBUTES,
                })],
            },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache,
        });
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cell uniforms"),
            size: size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let instance_capacity = 4096;
        let mask_atlas = Atlas::new(device, "mask atlas", wgpu::TextureFormat::R8Unorm, INITIAL_ATLAS_SIZE);
        let color_atlas = Atlas::new(device, "color atlas", wgpu::TextureFormat::Rgba8Unorm, INITIAL_ATLAS_SIZE);
        let bind_group = create_bind_group(device, &bind_group_layout, &uniform_buffer, &mask_atlas, &color_atlas);
        Self {
            pipeline,
            bind_group_layout,
            bind_group,
            uniform_buffer,
            instance_buffer: create_instance_buffer(device, instance_capacity),
            instance_capacity,
            mask_atlas,
            color_atlas,
            max_atlas_size: device.limits().max_texture_dimension_2d.min(8192),
            atlas_full: None,
            glyphs: HashMap::default(),
            shape_cache: HashMap::default(),
            shape_cache_len: 0,
            rows: Vec::new(),
            frame: Vec::new(),
            uploaded: Vec::new(),
            uploaded_uniforms: Uniforms::zeroed(),
            split: 0,
            uniforms: Uniforms::zeroed(),
            metrics,
            padding,
            full_rebuild: true,
            last_cursor: None,
            selection: None,
            palette_generation: u64::MAX,
            reverse: false,
            cursor_rect: [0.0; 4],
            cursor_hidden: false,
            link_highlight: None,
            row_cache: HashMap::default(),
            clear_row_cache: false,
            overlays: Vec::new(),
            scrollbar: None,
            glow_line: None,
            overlay_instances: RowInstances::default(),
            bidi: true,
            flash: 0.0,
            foreground: Vec::new(),
            run: Run::default(),
            shaped: Vec::new(),
        }
    }

    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    pub fn padding(&self) -> [f32; 2] {
        self.padding
    }

    /// Moves the grid without touching the glyph caches.
    pub fn set_padding(&mut self, padding: [f32; 2]) {
        if padding != self.padding {
            self.padding = padding;
            self.full_rebuild = true;
        }
    }

    /// Cursor rectangle of the last frame in pixels: x, y, width, height.
    pub fn cursor_rect(&self) -> [f32; 4] {
        self.cursor_rect
    }

    pub fn set_metrics(&mut self, device: &wgpu::Device, metrics: CellMetrics, padding: [f32; 2]) {
        self.metrics = metrics;
        self.padding = padding;
        self.glyphs.clear();
        self.shape_cache.clear();
        self.shape_cache_len = 0;
        let (mask, color) = (self.mask_atlas.size(), self.color_atlas.size());
        self.mask_atlas.reset(device, mask);
        self.color_atlas.reset(device, color);
        self.rebind(device);
        self.full_rebuild = true;
        self.clear_row_cache = true;
    }

    pub fn invalidate(&mut self) {
        self.full_rebuild = true;
        self.clear_row_cache = true;
    }

    pub fn set_cursor_hidden(&mut self, hidden: bool) {
        self.cursor_hidden = hidden;
    }

    pub fn set_link_highlight(&mut self, highlight: Option<LinkHighlight>) {
        if highlight != self.link_highlight {
            self.link_highlight = highlight;
            self.full_rebuild = true;
        }
    }

    pub fn set_overlays(&mut self, overlays: Vec<Overlay>) {
        self.overlays = overlays;
    }

    /// Reorders rows with right-to-left text for display (Unicode bidirectional algorithm).
    pub fn set_bidi(&mut self, enabled: bool) {
        if enabled != self.bidi {
            self.bidi = enabled;
            self.invalidate();
        }
    }

    pub fn set_flash(&mut self, strength: f32) {
        self.flash = strength.clamp(0.0, 1.0);
    }

    pub fn set_scrollbar(&mut self, scrollbar: Option<Scrollbar>) {
        self.scrollbar = scrollbar;
    }

    pub fn set_glow_line(&mut self, line: Option<GlowLine>) {
        self.glow_line = line;
    }

    fn rebind(&mut self, device: &wgpu::Device) {
        self.bind_group = create_bind_group(
            device,
            &self.bind_group_layout,
            &self.uniform_buffer,
            &self.mask_atlas,
            &self.color_atlas,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        snapshot: &Snapshot,
        fonts: &mut FontSystem,
        theme: &Theme,
        focused: bool,
        srgb: bool,
        viewport: [f32; 2],
    ) {
        for _ in 0..4 {
            self.atlas_full = None;
            self.build(queue, snapshot, fonts, theme, focused, srgb, viewport);
            let Some(format) = self.atlas_full else { break };
            self.grow_atlas(device, format);
        }
    }

    fn grow_atlas(&mut self, device: &wgpu::Device, format: GlyphFormat) {
        let max = self.max_atlas_size;
        let atlas = match format {
            GlyphFormat::Mask => &mut self.mask_atlas,
            GlyphFormat::Color => &mut self.color_atlas,
        };
        let size = (atlas.size() * 2).min(max);
        log::debug!("{format:?} atlas full, resetting at {size}px");
        atlas.reset(device, size);
        self.glyphs.retain(|_, entry| entry.is_none_or(|e| e.format != format));
        self.rebind(device);
        self.full_rebuild = true;
        self.clear_row_cache = true;
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        &mut self,
        queue: &wgpu::Queue,
        snapshot: &Snapshot,
        fonts: &mut FontSystem,
        theme: &Theme,
        focused: bool,
        srgb: bool,
        viewport: [f32; 2],
    ) {
        let rows = snapshot.rows();
        if self.rows.len() != rows {
            self.rows.resize_with(rows, RowInstances::default);
            self.full_rebuild = true;
        }
        if snapshot.palette_generation != self.palette_generation {
            self.palette_generation = snapshot.palette_generation;
            self.full_rebuild = true;
            self.clear_row_cache = true;
        }
        if snapshot.selection != self.selection {
            self.selection = snapshot.selection;
            self.full_rebuild = true;
        }
        let reverse = snapshot.modes().contains(Modes::REVERSE_VIDEO);
        if reverse != self.reverse {
            self.reverse = reverse;
            self.full_rebuild = true;
            self.clear_row_cache = true;
        }
        let cursor = snapshot.cursor();
        let cursor_cell = (cursor.visible && snapshot.display_offset == 0).then_some((cursor.row, cursor.col));
        let moved = cursor_cell != self.last_cursor;
        let (old_row, new_row) = (self.last_cursor.map(|c| c.0), cursor_cell.map(|c| c.0));

        let colors = ColorContext {
            palette: &snapshot.palette,
            theme,
            reverse,
            srgb,
            selection: snapshot.selection,
            extended: &snapshot.extended,
        };
        if self.clear_row_cache || self.row_cache.len() > rows * 4 + 64 {
            self.row_cache.clear();
            self.clear_row_cache = false;
        }
        for y in 0..rows {
            let cursor_row = moved && (Some(y) == old_row || Some(y) == new_row);
            if !(self.full_rebuild || snapshot.damaged[y] || cursor_row) {
                continue;
            }
            let row = &snapshot.rows[y];
            let line = snapshot.line(y);
            let cursor_col = cursor_cell.filter(|c| c.0 == y).map(|c| c.1);
            // Rows touched by a selection or link highlight depend on more than their cells.
            let highlighted = snapshot.selection.is_some_and(|s| (s.start.line..=s.end.line).contains(&line))
                || self
                    .link_highlight
                    .as_ref()
                    .is_some_and(|h| h.id.is_some() || (h.start.line..=h.end.line).contains(&line));
            let key = (!highlighted).then(|| row_key(row, cursor_col));
            let mut instances = std::mem::take(&mut self.rows[y]);
            if let Some(cached) = key.and_then(|k| self.row_cache.get(&k)) {
                // Scrolled content: same cells as a row built earlier, reuse its instances.
                instances.background.clone_from(&cached.background);
                instances.foreground.clone_from(&cached.foreground);
            } else {
                instances.background.clear();
                instances.foreground.clear();
                self.build_row(row, line, cursor_col, &colors, fonts, queue, &mut instances);
                if let Some(key) = key {
                    let copy = RowInstances {
                        background: instances.background.clone(),
                        foreground: instances.foreground.clone(),
                    };
                    self.row_cache.insert(key, copy);
                }
            }
            self.rows[y] = instances;
        }
        self.full_rebuild = false;
        self.last_cursor = cursor_cell;

        // Row instances are relative to the row top; place them now.
        let cell_h = self.metrics.height as f32;
        let padding_top = self.padding[1];
        let shifted = |instance: &Instance, offset: f32| Instance {
            pos: [instance.pos[0], instance.pos[1] + offset],
            ..*instance
        };
        self.frame.clear();
        for (y, row) in self.rows.iter().enumerate() {
            let offset = padding_top + y as f32 * cell_h;
            self.frame.extend(row.background.iter().map(|i| shifted(i, offset)));
        }
        let cursor_rect = self.push_cursor(snapshot, &colors, focused);
        self.split = self.frame.len();
        for (y, row) in self.rows.iter().enumerate() {
            let offset = padding_top + y as f32 * cell_h;
            self.frame.extend(row.foreground.iter().map(|i| shifted(i, offset)));
        }

        // Overlays (search bar, IME preedit) are drawn above the terminal text.
        if !self.overlays.is_empty() {
            let overlays = std::mem::take(&mut self.overlays);
            let mut instances = std::mem::take(&mut self.overlay_instances);
            for overlay in &overlays {
                if overlay.row >= rows {
                    continue;
                }
                instances.background.clear();
                instances.foreground.clear();
                let row = overlay_row(overlay, snapshot.cols);
                self.build_row(&row, i64::MIN, None, &colors, fonts, queue, &mut instances);
                let offset = padding_top + overlay.row as f32 * cell_h;
                self.frame.extend(instances.background.iter().chain(&instances.foreground).map(|i| shifted(i, offset)));
            }
            self.overlay_instances = instances;
            self.overlays = overlays;
        }
        if let Some(bar) = self.scrollbar {
            let [r, g, b, _] = colors.rgba(bar.color);
            self.frame.push(Instance {
                pos: [bar.x, bar.y],
                size: [bar.width, bar.height],
                // Corner radius, then the size the shader measures the rounded box in.
                uv: [bar.width / 2.0, 0.0, bar.width, bar.height],
                color: [r, g, b, bar.alpha],
                kind: KIND_ROUNDED,
            });
        }
        if let Some(line) = self.glow_line {
            let [r, g, b, _] = colors.rgba(line.color);
            let tail = line.tail.max(1.0);
            // Brightness at a distance from the center: 0 where the tail ends, 1 at the head.
            let brightness = |distance: f32| (distance - (line.head - tail)) / tail;
            let height = line.thickness + line.glow;
            let streak = |left: f32, right: f32, left_brightness: f32, right_brightness: f32| Instance {
                pos: [left, line.top],
                size: [right - left, height],
                // The shader runs from one brightness to the other across the streak.
                uv: [left_brightness, line.thickness, right_brightness, line.glow],
                color: [r, g, b, 1.0],
                kind: KIND_GLOW,
            };
            let near = (line.head - tail).max(0.0);
            let right_far = line.head.min(viewport[0] - line.center);
            if right_far > near {
                self.frame.push(streak(
                    line.center + near,
                    line.center + right_far,
                    brightness(near),
                    brightness(right_far),
                ));
            }
            let left_far = line.head.min(line.center);
            if left_far > near {
                self.frame.push(streak(
                    line.center - left_far,
                    line.center - near,
                    brightness(left_far),
                    brightness(near),
                ));
            }
        }
        if self.flash > 0.0 {
            self.frame.push(solid(0.0, 0.0, viewport[0], viewport[1], [1.0, 1.0, 1.0, self.flash * 0.18]));
        }

        let cursor_text = theme.cursor_text.unwrap_or(colors.palette.background);
        self.uniforms = Uniforms {
            viewport: [viewport[0], viewport[1], 0.0, 0.0],
            cursor_rect,
            cursor_text: colors.rgba(cursor_text),
        };
    }

    #[allow(clippy::too_many_arguments)]
    fn build_row(
        &mut self,
        row: &Row,
        line: i64,
        cursor_col: Option<usize>,
        colors: &ColorContext<'_>,
        fonts: &mut FontSystem,
        queue: &wgpu::Queue,
        out: &mut RowInstances,
    ) {
        let m = self.metrics;
        let (cell_w, cell_h) = (m.width as f32, m.height as f32);
        // Positions are relative to the row top.
        let top = 0.0;
        let layout = RowLayout::new(row, self.padding[0], cell_w, cell_h, self.bidi);
        let cols = layout.cols;

        // Backgrounds, decorations and per-cell text colors.
        let mut foreground = std::mem::take(&mut self.foreground);
        foreground.clear();
        foreground.resize(row.cells.len(), [0.0; 4]);
        for (x, cell) in row.cells[..cols].iter().enumerate() {
            let selected = colors.selection.is_some_and(|s| s.contains(line, x));
            let (fg, bg) = colors.cell(cell, selected);
            foreground[x] = fg;
            let trailing_half =
                cell.flags.contains(Flags::WIDE_SPACER) && x > 0 && row.cells[x - 1].flags.contains(Flags::WIDE);
            if trailing_half {
                continue;
            }
            let left = layout.left(x);
            let width = if cell.flags.contains(Flags::WIDE) { 2.0 } else { 1.0 } * layout.cell_width;
            if let Some(bg) = bg {
                match out.background.last_mut() {
                    Some(last) if last.color == bg && (last.pos[0] + last.size[0] - left).abs() < 0.5 => {
                        last.size[0] += width;
                    }
                    _ => out.background.push(solid(left, top, width, cell_h, bg)),
                }
            }
            if !cell.flags.contains(Flags::HIDDEN) {
                let linked = self
                    .link_highlight
                    .as_ref()
                    .is_some_and(|h| h.contains(line, x, colors.extended(cell.extended).link));
                let start = out.foreground.len();
                self.push_decorations(cell, colors, fg, left, top, width, linked, &layout, out);
                if layout.clipped {
                    let clipped: Vec<Instance> =
                        out.foreground.drain(start..).filter_map(|i| clip_vertical(i, top, top + cell_h)).collect();
                    out.foreground.extend(clipped);
                }
            }
        }

        // Text, shaped in runs.
        let mut run = std::mem::take(&mut self.run);
        run.active = false;
        run.text.clear();
        run.cells.clear();
        let mut x = 0;
        while x < cols {
            let cell = &row.cells[x];
            let width = if cell.flags.contains(Flags::WIDE) { 2 } else { 1 };
            let has_extra = cell.flags.contains(Flags::GRAPHEME);
            if cell.flags.intersects(Flags::WIDE_SPACER | Flags::HIDDEN)
                || (cell.is_empty() && !has_extra)
                || cell.ch == PLACEHOLDER
            {
                self.flush_run(&mut run, top, &foreground, &layout, fonts, queue, out);
                x += 1;
                continue;
            }
            let style = Style::new(cell.flags.contains(Flags::BOLD), cell.flags.contains(Flags::ITALIC));
            if let Some(size) = colors.extended(cell.extended).size
                && (size.dx, size.dy) == (0, 0)
                && multicell_intact(row, x, &size, colors)
            {
                self.flush_run(&mut run, top, &foreground, &layout, fonts, queue, out);
                self.draw_multicell(row, x, size, style, foreground[x], &layout, fonts, queue, out);
                x += size.cells().0.max(1);
                continue;
            }
            if !has_extra && tron_font::sprite::is_sprite(cell.ch) {
                self.flush_run(&mut run, top, &foreground, &layout, fonts, queue, out);
                if let Some(glyph) = self.glyph(GlyphSource::Sprite(cell.ch, layout.glyph_scale()), fonts, queue) {
                    let y = top + layout.glyph_shift + m.baseline as f32 * layout.scale_y - glyph.top as f32;
                    layout.push_glyph(&mut out.foreground, glyph_instance(&glyph, layout.left(x), y, foreground[x]));
                }
                x += width;
                continue;
            }
            let face = if has_extra {
                fonts.face_for_cluster(cell.ch, row.combining(x), style)
            } else {
                fonts.face_for(cell.ch, style)
            };
            let rtl = layout.is_rtl(x);
            let at_cursor = cursor_col == Some(x);
            if run.active
                && (run.face != face || run.style != Some(style) || run.rtl != rtl || at_cursor || run.split_after)
            {
                self.flush_run(&mut run, top, &foreground, &layout, fonts, queue, out);
            }
            if !run.active {
                run.active = true;
                run.face = face;
                run.style = Some(style);
                run.rtl = rtl;
            }
            run.cells.push((run.text.len() as u32, x, width as u8));
            row.push_cell_text(x, &mut run.text);
            run.split_after = at_cursor;
            x += width;
        }
        self.flush_run(&mut run, top, &foreground, &layout, fonts, queue, out);
        self.run = run;
        self.foreground = foreground;
    }

    #[allow(clippy::too_many_arguments)]
    fn flush_run(
        &mut self,
        run: &mut Run,
        top: f32,
        foreground: &[[f32; 4]],
        layout: &RowLayout,
        fonts: &mut FontSystem,
        queue: &wgpu::Queue,
        out: &mut RowInstances,
    ) {
        if !run.active {
            return;
        }
        run.active = false;
        run.split_after = false;
        let shaped = self.shape(fonts, run.face, &run.text, run.rtl);
        let baseline = top + layout.glyph_shift + self.metrics.baseline as f32 * layout.scale_y;
        let scale = layout.glyph_scale();
        let mut cluster = u32::MAX;
        let mut pen = 0.0;
        for glyph in &shaped {
            if glyph.cluster != cluster {
                cluster = glyph.cluster;
                pen = 0.0;
            }
            let index = match run.cells.binary_search_by_key(&glyph.cluster, |c| c.0) {
                Ok(i) => i,
                Err(i) => i.saturating_sub(1),
            };
            let (_, col, width) = run.cells[index];
            if let Some(entry) = self.glyph(GlyphSource::Font(glyph.glyph, scale), fonts, queue) {
                let cell_left = layout.left(col);
                let left = match entry.format {
                    GlyphFormat::Mask => cell_left + pen + glyph.x_offset * layout.scale_x + entry.left as f32,
                    // Center color glyphs (emoji) in their cells.
                    GlyphFormat::Color => {
                        cell_left + ((f32::from(width) * layout.cell_width - entry.width as f32) / 2.0).floor()
                    }
                };
                let y = baseline - glyph.y_offset * layout.scale_y - entry.top as f32;
                layout.push_glyph(&mut out.foreground, glyph_instance(&entry, left, y, foreground[col]));
            }
            pen += glyph.x_advance * layout.scale_x;
        }
        self.shaped = shaped;
        run.text.clear();
        run.cells.clear();
    }

    /// Draws a block of scaled text (kitty text sizing) whose top left cell is `x`.
    #[allow(clippy::too_many_arguments)]
    fn draw_multicell(
        &mut self,
        row: &Row,
        x: usize,
        size: TextSize,
        style: Style,
        color: [f32; 4],
        layout: &RowLayout,
        fonts: &mut FontSystem,
        queue: &wgpu::Queue,
        out: &mut RowInstances,
    ) {
        let m = self.metrics;
        let scale = size.font_scale();
        let (block_cols, block_rows) = size.cells();
        let block_w = block_cols as f32 * layout.cell_width;
        let block_h = block_rows as f32 * m.height as f32;
        let mut text = String::new();
        row.push_cell_text(x, &mut text);
        let face = fonts.face_for_cluster(row.cells[x].ch, row.combining(x), style);
        let shaped = self.shape(fonts, face, &text, false);
        let advance = shaped.iter().map(|g| g.x_advance).sum::<f32>() * scale;
        let (spare_w, spare_h) = ((block_w - advance).max(0.0), (block_h - scale * m.height as f32).max(0.0));
        let offset_x = match size.horizontal {
            1 => spare_w,
            2 => spare_w / 2.0,
            _ => 0.0,
        };
        let offset_y = match size.vertical {
            1 => spare_h,
            2 => spare_h / 2.0,
            _ => 0.0,
        };
        let origin = layout.left(x) + offset_x;
        let baseline = offset_y + m.baseline as f32 * scale;
        let glyph_scale = GlyphScale::new(scale, scale);
        let mut pen = 0.0;
        for glyph in &shaped {
            if let Some(entry) = self.glyph(GlyphSource::Font(glyph.glyph, glyph_scale), fonts, queue) {
                let left = match entry.format {
                    GlyphFormat::Mask => origin + pen + glyph.x_offset * scale + entry.left as f32,
                    GlyphFormat::Color => origin + ((block_w - entry.width as f32) / 2.0).floor(),
                };
                let y = baseline - glyph.y_offset * scale - entry.top as f32;
                out.foreground.push(glyph_instance(&entry, left, y, color));
            }
            pen += glyph.x_advance * scale;
        }
        self.shaped = shaped;
    }

    fn shape(&mut self, fonts: &mut FontSystem, face: u32, text: &str, rtl: bool) -> Vec<ShapedGlyph> {
        let mut shaped = std::mem::take(&mut self.shaped);
        shaped.clear();
        // Faces are numbered from zero; the top bit marks right-to-left shaping.
        let key = face | u32::from(rtl) << 31;
        if let Some(cached) = self.shape_cache.get(&key).and_then(|cache| cache.get(text)) {
            shaped.extend_from_slice(cached);
            return shaped;
        }
        fonts.shape_directional(face, text, rtl, &mut shaped);
        if self.shape_cache_len >= SHAPE_CACHE_LIMIT {
            self.shape_cache.clear();
            self.shape_cache_len = 0;
        }
        self.shape_cache.entry(key).or_default().insert(text.to_owned(), shaped.clone());
        self.shape_cache_len += 1;
        shaped
    }

    #[allow(clippy::too_many_arguments)]
    fn push_decorations(
        &self,
        cell: &Cell,
        colors: &ColorContext<'_>,
        fg: [f32; 4],
        left: f32,
        top: f32,
        width: f32,
        linked: bool,
        layout: &RowLayout,
        out: &mut RowInstances,
    ) {
        let m = self.metrics;
        let scale = layout.scale_y;
        // Double height halves draw the part of a two row tall line that falls in this row.
        let origin = top + layout.glyph_shift;
        let at = |position: u32| origin + position as f32 * scale;
        let thickness = (m.underline_thickness as f32 * scale).max(1.0);
        let flags = cell.flags;
        if flags.intersects(Flags::ANY_UNDERLINE) {
            let underline_color = colors.extended(cell.extended).underline_color;
            let color = if underline_color.is_default() {
                fg
            } else {
                colors.rgba(colors.palette.resolve(underline_color, colors.palette.foreground))
            };
            let y = at(m.underline_position);
            if flags.contains(Flags::DOUBLE_UNDERLINE) {
                let second =
                    if y + 3.0 * thickness <= at(m.height) { y + 2.0 * thickness } else { y - 2.0 * thickness };
                out.foreground.push(solid(left, y, width, thickness, color));
                out.foreground.push(solid(left, second, width, thickness, color));
            } else if flags.intersects(Flags::DOTTED_UNDERLINE | Flags::DASHED_UNDERLINE) {
                let dash =
                    if flags.contains(Flags::DOTTED_UNDERLINE) { thickness } else { (layout.cell_width / 3.0).ceil() };
                let mut x = left;
                while x < left + width {
                    out.foreground.push(solid(x, y, dash.min(left + width - x), thickness, color));
                    x += dash * 2.0;
                }
            } else if flags.contains(Flags::CURLY_UNDERLINE) {
                let amplitude = (thickness * 1.25).max(1.5);
                let band = 2.0 * (amplitude + thickness) + 2.0;
                out.foreground.push(Instance {
                    pos: [left, y + thickness / 2.0 - band / 2.0],
                    size: [width, band],
                    uv: [layout.cell_width, amplitude, thickness.max(1.0), band],
                    color,
                    kind: KIND_CURLY,
                });
            } else {
                out.foreground.push(solid(left, y, width, thickness, color));
            }
        } else if linked {
            out.foreground.push(solid(left, at(m.underline_position), width, thickness, fg));
        }
        if flags.contains(Flags::STRIKETHROUGH) {
            out.foreground.push(solid(left, at(m.strikeout_position), width, thickness, fg));
        }
        if flags.contains(Flags::OVERLINE) {
            out.foreground.push(solid(left, origin, width, thickness, fg));
        }
    }

    /// Adds cursor quads to the frame. Returns the block cursor rectangle for the shader.
    fn push_cursor(&mut self, snapshot: &Snapshot, colors: &ColorContext<'_>, focused: bool) -> [f32; 4] {
        const NONE: [f32; 4] = [-1.0, -1.0, -1.0, -1.0];
        self.cursor_rect = [0.0; 4];
        let cursor = snapshot.cursor();
        if !cursor.visible
            || snapshot.display_offset != 0
            || cursor.row >= snapshot.rows()
            || cursor.col >= snapshot.cols
        {
            return NONE;
        }
        let m = self.metrics;
        let (cell_w, cell_h) = (m.width as f32, m.height as f32);
        let row = &snapshot.rows[cursor.row];
        let layout = RowLayout::new(row, self.padding[0], cell_w, cell_h, self.bidi);
        let col = cursor.col.min(layout.cols - 1);
        let wide = row.cells.get(col).is_some_and(|c| c.flags.contains(Flags::WIDE));
        let width = if wide { 2.0 } else { 1.0 } * layout.cell_width;
        let x = layout.left(col);
        let y = self.padding[1] + cursor.row as f32 * cell_h;
        self.cursor_rect = [x, y, width, cell_h];
        // An overlay such as the command palette covers the cursor's cell: the cursor
        // would paint over it and recolor its text.
        let covered = self.overlays.iter().any(|overlay| {
            let span: usize = overlay.text.chars().map(|c| c.width().unwrap_or(0)).sum();
            overlay.row == cursor.row && (overlay.col..overlay.col + span).contains(&col)
        });
        if self.cursor_hidden || covered {
            return NONE;
        }
        let color = colors.rgba(colors.palette.cursor);
        let stroke = (m.underline_thickness as f32).max((cell_h / 16.0).round()).max(1.0);

        if !focused {
            self.frame.extend_from_slice(&[
                solid(x, y, width, stroke, color),
                solid(x, y + cell_h - stroke, width, stroke, color),
                solid(x, y, stroke, cell_h, color),
                solid(x + width - stroke, y, stroke, cell_h, color),
            ]);
            return NONE;
        }
        match cursor.shape {
            CursorShape::Block => {
                self.frame.push(solid(x, y, width, cell_h, color));
                [x, y, x + width, y + cell_h]
            }
            CursorShape::Beam => {
                self.frame.push(solid(x, y, stroke.max(2.0), cell_h, color));
                NONE
            }
            CursorShape::Underline => {
                self.frame.push(solid(x, y + cell_h - stroke * 2.0, width, stroke * 2.0, color));
                NONE
            }
        }
    }

    fn glyph(&mut self, source: GlyphSource, fonts: &mut FontSystem, queue: &wgpu::Queue) -> Option<GlyphEntry> {
        if let Some(entry) = self.glyphs.get(&source) {
            return *entry;
        }
        let raster = match source {
            GlyphSource::Font(key, GlyphScale::ONE) => fonts.rasterize(key),
            GlyphSource::Font(key, scale) => {
                let (x, y) = scale.factors();
                fonts.rasterize_scaled(key, x, y)
            }
            GlyphSource::Sprite(ch, scale) => {
                let (x, y) = scale.factors();
                fonts.sprite_scaled(ch, x, y)
            }
        };
        let entry = match raster {
            Some(glyph) if glyph.width > 0 && glyph.height > 0 => {
                let atlas = match glyph.format {
                    GlyphFormat::Mask => &mut self.mask_atlas,
                    GlyphFormat::Color => &mut self.color_atlas,
                };
                let Some((x, y)) = atlas.allocate(glyph.width, glyph.height) else {
                    self.atlas_full = Some(glyph.format);
                    return None;
                };
                atlas.upload(queue, x, y, glyph.width, glyph.height, &glyph.data);
                Some(GlyphEntry {
                    format: glyph.format,
                    x,
                    y,
                    width: glyph.width,
                    height: glyph.height,
                    left: glyph.left,
                    top: glyph.top,
                })
            }
            _ => None,
        };
        self.glyphs.insert(source, entry);
        entry
    }

    /// Writes the frame to the GPU. Returns whether it differs from the last frame
    /// written; an unchanged frame is not written again.
    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> bool {
        if self.frame == self.uploaded
            && bytemuck::bytes_of(&self.uniforms) == bytemuck::bytes_of(&self.uploaded_uniforms)
        {
            return false;
        }
        if self.frame.len() > self.instance_capacity {
            self.instance_capacity = self.frame.len().next_power_of_two();
            self.instance_buffer = create_instance_buffer(device, self.instance_capacity);
        }
        if !self.frame.is_empty() {
            queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(&self.frame));
        }
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&self.uniforms));
        self.uploaded.clone_from(&self.frame);
        self.uploaded_uniforms = self.uniforms;
        true
    }

    /// Instance range of backgrounds and cursor, then of text and decorations.
    pub fn ranges(&self) -> (Range<u32>, Range<u32>) {
        (0..self.split as u32, self.split as u32..self.frame.len() as u32)
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, range: Range<u32>) {
        if range.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        pass.draw(0..4, range);
    }
}

/// Hash of everything a row's instances depend on besides colors and fonts.
fn row_key(row: &Row, cursor_col: Option<usize>) -> u64 {
    use std::hash::{BuildHasher, Hash, Hasher};
    let mut hasher = foldhash::fast::FixedState::with_seed(0x7472_6f6e).build_hasher();
    // Hash the cells as one byte slice. Their derived `Hash` feeds every field
    // separately, several hasher rounds per cell instead of one per 16 bytes.
    // SAFETY: `Cell` is `repr(C)`, 16 bytes without padding (`cell_is_compact`), so every byte is initialized.
    let bytes = unsafe { std::slice::from_raw_parts(row.cells.as_ptr().cast::<u8>(), size_of_val(&row.cells[..])) };
    hasher.write(bytes);
    row.line_size.hash(&mut hasher);
    for (col, cell) in row.cells.iter().enumerate() {
        if cell.flags.contains(Flags::GRAPHEME) {
            col.hash(&mut hasher);
            row.combining(col).hash(&mut hasher);
        }
    }
    cursor_col.hash(&mut hasher);
    hasher.finish()
}

/// Lays out overlay text as a row of cells with explicit colors.
fn overlay_row(overlay: &Overlay, cols: usize) -> Row {
    let mut row = Row::new(cols);
    let [r, g, b] = overlay.fg;
    let fg = Color::rgb(r, g, b);
    let [r, g, b] = overlay.bg;
    let bg = Color::rgb(r, g, b);
    let base = if overlay.underline { Flags::UNDERLINE } else { Flags::empty() };
    let mut col = overlay.col;
    for c in overlay.text.chars() {
        let width = c.width().unwrap_or(0);
        if width == 0 {
            continue;
        }
        if col + width > cols {
            break;
        }
        let flags = if width == 2 { base | Flags::WIDE } else { base };
        row.cells[col] = Cell { ch: c, fg, bg, flags, ..Cell::BLANK };
        if width == 2 {
            row.cells[col + 1] = Cell { fg, bg, flags: base | Flags::WIDE_SPACER, ..Cell::BLANK };
        }
        col += width;
    }
    row
}

fn glyph_instance(entry: &GlyphEntry, left: f32, top: f32, color: [f32; 4]) -> Instance {
    Instance {
        pos: [left.round(), top.round()],
        size: [entry.width as f32, entry.height as f32],
        uv: [entry.x as f32, entry.y as f32, (entry.x + entry.width) as f32, (entry.y + entry.height) as f32],
        color,
        kind: match entry.format {
            GlyphFormat::Mask => KIND_MASK,
            GlyphFormat::Color => KIND_COLOR,
        },
    }
}

fn solid(x: f32, y: f32, width: f32, height: f32, color: [f32; 4]) -> Instance {
    Instance { pos: [x, y], size: [width, height], uv: [0.0; 4], color, kind: KIND_SOLID }
}

fn create_instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cell instances"),
        size: (capacity * size_of::<Instance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniforms: &wgpu::Buffer,
    mask: &Atlas,
    color: &Atlas,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("cells"),
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&mask.view) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&color.view) },
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str) -> Row {
        let mut row = Row::new(text.chars().count());
        for (cell, ch) in row.cells.iter_mut().zip(text.chars()) {
            cell.ch = ch;
        }
        row
    }

    #[test]
    fn right_to_left_text_is_reversed_on_screen() {
        assert!(bidi_order(&row("plain text"), 10).is_none());
        // "ab אבג cd": the Hebrew word is drawn reversed, the rest keeps its place.
        let (visual, rtl) = bidi_order(&row("ab \u{5D0}\u{5D1}\u{5D2} cd"), 9).unwrap();
        assert_eq!(visual, [0, 1, 2, 5, 4, 3, 6, 7, 8]);
        assert_eq!(rtl, [false, false, false, true, true, true, false, false, false]);
        // Numbers after right-to-left text join its run: "ab 12 גבא" on screen.
        let (visual, _) = bidi_order(&row("ab \u{5D0}\u{5D1}\u{5D2} 12"), 9).unwrap();
        assert_eq!(visual, [0, 1, 2, 8, 7, 6, 5, 3, 4]);
    }

    #[test]
    fn double_size_lines_stretch_and_clip() {
        let mut double = row("abcdef");
        double.line_size = LineSize::DoubleHeightBottom;
        let layout = RowLayout::new(&double, 10.0, 8.0, 16.0, true);
        assert_eq!((layout.cols, layout.left(2), layout.glyph_shift), (3, 42.0, -16.0));
        let glyph = Instance {
            pos: [0.0, -10.0],
            size: [4.0, 20.0],
            uv: [0.0, 100.0, 4.0, 120.0],
            color: [1.0; 4],
            kind: KIND_MASK,
        };
        let clipped = clip_vertical(glyph, 0.0, 16.0).unwrap();
        assert_eq!((clipped.pos[1], clipped.size[1], clipped.uv[1], clipped.uv[3]), (0.0, 10.0, 110.0, 120.0));
        assert!(clip_vertical(glyph, 20.0, 36.0).is_none());
    }
}
