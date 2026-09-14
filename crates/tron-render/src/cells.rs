//! Cell pipeline: backgrounds, shaped text, decorations, selection and cursor,
//! all drawn as instances of one quad.

use foldhash::HashMap;
use std::mem::size_of;
use std::ops::Range;

use bytemuck::{Pod, Zeroable};
use tron_core::{Cell, CursorShape, Flags, Modes, Palette, Row, SelectionRange, Snapshot};
use tron_font::{CellMetrics, FontSystem, GlyphFormat, GlyphKey, ShapedGlyph, Style};

use crate::Theme;
use crate::atlas::Atlas;

const INITIAL_ATLAS_SIZE: u32 = 1024;
const SHAPE_CACHE_LIMIT: usize = 16_384;

const KIND_SOLID: u32 = 0;
const KIND_MASK: u32 = 1;
const KIND_COLOR: u32 = 2;

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
    Font(GlyphKey),
    Sprite(char),
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
}

/// Colors needed to resolve cells.
struct ColorContext<'a> {
    palette: &'a Palette,
    theme: &'a Theme,
    reverse: bool,
    srgb: bool,
    selection: Option<SelectionRange>,
}

impl ColorContext<'_> {
    fn rgba(&self, color: [u8; 3]) -> [f32; 4] {
        to_rgba(color, self.srgb)
    }

    /// Foreground color and, when it differs from the window background, background color.
    fn cell(&self, cell: &Cell, selected: bool) -> ([f32; 4], Option<[f32; 4]>) {
        let palette = self.palette;
        let mut fg = palette.resolve(cell.fg, palette.foreground);
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
    // Scratch buffers reused across rows.
    foreground: Vec<[f32; 4]>,
    run: Run,
    shaped: Vec<ShapedGlyph>,
}

impl CellPipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, metrics: CellMetrics, padding: [f32; 2]) -> Self {
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
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
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
            cache: None,
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
    }

    pub fn invalidate(&mut self) {
        self.full_rebuild = true;
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
        }
        if snapshot.selection != self.selection {
            self.selection = snapshot.selection;
            self.full_rebuild = true;
        }
        let reverse = snapshot.modes().contains(Modes::REVERSE_VIDEO);
        if reverse != self.reverse {
            self.reverse = reverse;
            self.full_rebuild = true;
        }
        let cursor = snapshot.cursor();
        let cursor_cell = (cursor.visible && snapshot.display_offset == 0).then_some((cursor.row, cursor.col));
        let moved = cursor_cell != self.last_cursor;
        let (old_row, new_row) = (self.last_cursor.map(|c| c.0), cursor_cell.map(|c| c.0));

        let colors = ColorContext { palette: &snapshot.palette, theme, reverse, srgb, selection: snapshot.selection };
        for y in 0..rows {
            let cursor_row = moved && (Some(y) == old_row || Some(y) == new_row);
            if self.full_rebuild || snapshot.damaged[y] || cursor_row {
                let mut instances = std::mem::take(&mut self.rows[y]);
                instances.background.clear();
                instances.foreground.clear();
                let cursor_col = cursor_cell.filter(|c| c.0 == y).map(|c| c.1);
                self.build_row(&snapshot.rows[y], y, snapshot.line(y), cursor_col, &colors, fonts, queue, &mut instances);
                self.rows[y] = instances;
            }
        }
        self.full_rebuild = false;
        self.last_cursor = cursor_cell;

        self.frame.clear();
        for row in &self.rows {
            self.frame.extend_from_slice(&row.background);
        }
        let cursor_rect = self.push_cursor(snapshot, &colors, focused);
        self.split = self.frame.len();
        for row in &self.rows {
            self.frame.extend_from_slice(&row.foreground);
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
        y: usize,
        line: i64,
        cursor_col: Option<usize>,
        colors: &ColorContext<'_>,
        fonts: &mut FontSystem,
        queue: &wgpu::Queue,
        out: &mut RowInstances,
    ) {
        let m = self.metrics;
        let (cell_w, cell_h) = (m.width as f32, m.height as f32);
        let top = self.padding[1] + y as f32 * cell_h;
        let cols = row.cells.len();

        // Backgrounds, decorations and per-cell text colors.
        let mut foreground = std::mem::take(&mut self.foreground);
        foreground.clear();
        foreground.resize(cols, [0.0; 4]);
        for (x, cell) in row.cells.iter().enumerate() {
            let selected = colors.selection.is_some_and(|s| s.contains(line, x));
            let (fg, bg) = colors.cell(cell, selected);
            foreground[x] = fg;
            let trailing_half = cell.flags.contains(Flags::WIDE_SPACER) && x > 0 && row.cells[x - 1].flags.contains(Flags::WIDE);
            if trailing_half {
                continue;
            }
            let left = self.padding[0] + x as f32 * cell_w;
            let width = if cell.flags.contains(Flags::WIDE) { 2.0 * cell_w } else { cell_w };
            if let Some(bg) = bg {
                match out.background.last_mut() {
                    Some(last) if last.color == bg && (last.pos[0] + last.size[0] - left).abs() < 0.5 => {
                        last.size[0] += width;
                    }
                    _ => out.background.push(solid(left, top, width, cell_h, bg)),
                }
            }
            if !cell.flags.contains(Flags::HIDDEN) {
                self.push_decorations(cell, colors, fg, left, top, width, out);
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
            if cell.flags.intersects(Flags::WIDE_SPACER | Flags::HIDDEN) || (cell.is_empty() && !has_extra) {
                self.flush_run(&mut run, top, &foreground, fonts, queue, out);
                x += 1;
                continue;
            }
            if !has_extra && tron_font::sprite::is_sprite(cell.ch) {
                self.flush_run(&mut run, top, &foreground, fonts, queue, out);
                if let Some(glyph) = self.glyph(GlyphSource::Sprite(cell.ch), fonts, queue) {
                    let left = self.padding[0] + x as f32 * cell_w;
                    out.foreground.push(glyph_instance(&glyph, left, top + m.baseline as f32 - glyph.top as f32, foreground[x]));
                }
                x += width;
                continue;
            }
            let style = Style::new(cell.flags.contains(Flags::BOLD), cell.flags.contains(Flags::ITALIC));
            let face = if has_extra {
                fonts.face_for_cluster(cell.ch, row.combining(x), style)
            } else {
                fonts.face_for(cell.ch, style)
            };
            let at_cursor = cursor_col == Some(x);
            if run.active && (run.face != face || run.style != Some(style) || at_cursor || run.split_after) {
                self.flush_run(&mut run, top, &foreground, fonts, queue, out);
            }
            if !run.active {
                run.active = true;
                run.face = face;
                run.style = Some(style);
            }
            run.cells.push((run.text.len() as u32, x, width as u8));
            row.push_cell_text(x, &mut run.text);
            run.split_after = at_cursor;
            x += width;
        }
        self.flush_run(&mut run, top, &foreground, fonts, queue, out);
        self.run = run;
        self.foreground = foreground;
    }

    #[allow(clippy::too_many_arguments)]
    fn flush_run(
        &mut self,
        run: &mut Run,
        top: f32,
        foreground: &[[f32; 4]],
        fonts: &mut FontSystem,
        queue: &wgpu::Queue,
        out: &mut RowInstances,
    ) {
        if !run.active {
            return;
        }
        run.active = false;
        run.split_after = false;
        let shaped = self.shape(fonts, run.face, &run.text);
        let m = self.metrics;
        let cell_w = m.width as f32;
        let baseline = top + m.baseline as f32;
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
            if let Some(entry) = self.glyph(GlyphSource::Font(glyph.glyph), fonts, queue) {
                let cell_left = self.padding[0] + col as f32 * cell_w;
                let left = match entry.format {
                    GlyphFormat::Mask => cell_left + pen + glyph.x_offset + entry.left as f32,
                    // Center color glyphs (emoji) in their cells.
                    GlyphFormat::Color => cell_left + ((f32::from(width) * cell_w - entry.width as f32) / 2.0).floor(),
                };
                let y = baseline - glyph.y_offset - entry.top as f32;
                out.foreground.push(glyph_instance(&entry, left, y, foreground[col]));
            }
            pen += glyph.x_advance;
        }
        self.shaped = shaped;
        run.text.clear();
        run.cells.clear();
    }

    fn shape(&mut self, fonts: &mut FontSystem, face: u32, text: &str) -> Vec<ShapedGlyph> {
        let mut shaped = std::mem::take(&mut self.shaped);
        shaped.clear();
        if let Some(cached) = self.shape_cache.get(&face).and_then(|cache| cache.get(text)) {
            shaped.extend_from_slice(cached);
            return shaped;
        }
        fonts.shape(face, text, &mut shaped);
        if self.shape_cache_len >= SHAPE_CACHE_LIMIT {
            self.shape_cache.clear();
            self.shape_cache_len = 0;
        }
        self.shape_cache.entry(face).or_default().insert(text.to_owned(), shaped.clone());
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
        out: &mut RowInstances,
    ) {
        let m = self.metrics;
        let thickness = m.underline_thickness as f32;
        let flags = cell.flags;
        if flags.intersects(Flags::ANY_UNDERLINE) {
            let color = if cell.underline_color.is_default() {
                fg
            } else {
                colors.rgba(colors.palette.resolve(cell.underline_color, colors.palette.foreground))
            };
            let y = top + m.underline_position as f32;
            if flags.contains(Flags::DOUBLE_UNDERLINE) {
                let second = if y + 3.0 * thickness <= top + m.height as f32 { y + 2.0 * thickness } else { y - 2.0 * thickness };
                out.foreground.push(solid(left, y, width, thickness, color));
                out.foreground.push(solid(left, second, width, thickness, color));
            } else if flags.intersects(Flags::DOTTED_UNDERLINE | Flags::DASHED_UNDERLINE) {
                let dash = if flags.contains(Flags::DOTTED_UNDERLINE) { thickness } else { (m.width as f32 / 3.0).ceil() };
                let mut x = left;
                while x < left + width {
                    out.foreground.push(solid(x, y, dash.min(left + width - x), thickness, color));
                    x += dash * 2.0;
                }
            } else if flags.contains(Flags::CURLY_UNDERLINE) {
                let segments = 4;
                let segment = width / segments as f32;
                for i in 0..segments {
                    let offset = if i % 2 == 0 { -thickness / 2.0 } else { thickness / 2.0 };
                    out.foreground.push(solid(left + i as f32 * segment, y + offset, segment, thickness, color));
                }
            } else {
                out.foreground.push(solid(left, y, width, thickness, color));
            }
        }
        if flags.contains(Flags::STRIKETHROUGH) {
            out.foreground.push(solid(left, top + m.strikeout_position as f32, width, thickness, fg));
        }
        if flags.contains(Flags::OVERLINE) {
            out.foreground.push(solid(left, top, width, thickness, fg));
        }
    }

    /// Adds cursor quads to the frame. Returns the block cursor rectangle for the shader.
    fn push_cursor(&mut self, snapshot: &Snapshot, colors: &ColorContext<'_>, focused: bool) -> [f32; 4] {
        const NONE: [f32; 4] = [-1.0, -1.0, -1.0, -1.0];
        self.cursor_rect = [0.0; 4];
        let cursor = snapshot.cursor();
        if !cursor.visible || snapshot.display_offset != 0 || cursor.row >= snapshot.rows() || cursor.col >= snapshot.cols {
            return NONE;
        }
        let m = self.metrics;
        let (cell_w, cell_h) = (m.width as f32, m.height as f32);
        let wide = snapshot.rows[cursor.row].cells.get(cursor.col).is_some_and(|c| c.flags.contains(Flags::WIDE));
        let width = if wide { 2.0 * cell_w } else { cell_w };
        let x = self.padding[0] + cursor.col as f32 * cell_w;
        let y = self.padding[1] + cursor.row as f32 * cell_h;
        self.cursor_rect = [x, y, width, cell_h];
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
            GlyphSource::Font(key) => fonts.rasterize(key),
            GlyphSource::Sprite(ch) => fonts.sprite(ch),
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

    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.frame.len() > self.instance_capacity {
            self.instance_capacity = self.frame.len().next_power_of_two();
            self.instance_buffer = create_instance_buffer(device, self.instance_capacity);
        }
        if !self.frame.is_empty() {
            queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(&self.frame));
        }
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&self.uniforms));
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

fn glyph_instance(entry: &GlyphEntry, left: f32, top: f32, color: [f32; 4]) -> Instance {
    Instance {
        pos: [left.round(), top.round()],
        size: [entry.width as f32, entry.height as f32],
        uv: [
            entry.x as f32,
            entry.y as f32,
            (entry.x + entry.width) as f32,
            (entry.y + entry.height) as f32,
        ],
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
