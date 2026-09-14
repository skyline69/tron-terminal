//! Font discovery, fallback and glyph rasterization.
//!
//! Discovery and fallback use `fontique` (reads the system fontconfig setup on
//! Linux). Shaping uses `harfrust`, which gives ligatures and combining marks.
//! Rasterization uses `swash`. Box drawing and block characters are drawn by
//! [`sprite`] so they join seamlessly.

pub mod sprite;

use foldhash::HashMap;
use std::str::FromStr;

use fontique::{
    Attributes, Blob, Collection, CollectionOptions, FontStyle, FontWeight, FontWidth, GenericFamily, QueryFamily,
    QueryStatus, SourceCache, Synthesis,
};
use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{Angle, Format, Transform};
use swash::{CacheKey, FontRef};

#[derive(Debug, thiserror::Error)]
pub enum FontError {
    #[error("no usable font found for family `{0}`")]
    NotFound(String),
}

/// When glyph outlines are hinted to the pixel grid.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum Hinting {
    /// Hint when the scale factor is below 1.5.
    #[default]
    Auto,
    On,
    Off,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Style {
    Regular = 0,
    Bold = 1,
    Italic = 2,
    BoldItalic = 3,
}

impl Style {
    pub fn new(bold: bool, italic: bool) -> Self {
        match (bold, italic) {
            (false, false) => Self::Regular,
            (true, false) => Self::Bold,
            (false, true) => Self::Italic,
            (true, true) => Self::BoldItalic,
        }
    }

    fn attributes(self) -> Attributes {
        let bold = matches!(self, Self::Bold | Self::BoldItalic);
        let italic = matches!(self, Self::Italic | Self::BoldItalic);
        Attributes::new(
            FontWidth::NORMAL,
            if italic { FontStyle::Italic } else { FontStyle::Normal },
            if bold { FontWeight::BOLD } else { FontWeight::NORMAL },
        )
    }
}

/// A glyph in a loaded face.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct GlyphKey {
    pub face: u32,
    pub glyph: u16,
}

/// Cell geometry in physical pixels. Positions are measured from the cell top.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct CellMetrics {
    pub width: u32,
    pub height: u32,
    pub baseline: u32,
    pub underline_position: u32,
    pub underline_thickness: u32,
    pub strikeout_position: u32,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum GlyphFormat {
    /// 8-bit coverage mask.
    Mask,
    /// 32-bit RGBA, straight alpha.
    Color,
}

#[derive(Clone, Debug)]
pub struct RasterizedGlyph {
    pub format: GlyphFormat,
    pub width: u32,
    pub height: u32,
    /// Horizontal offset from the cell origin.
    pub left: i32,
    /// Distance from the baseline to the top edge of the bitmap.
    pub top: i32,
    pub data: Vec<u8>,
}

/// A glyph produced by shaping.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ShapedGlyph {
    pub glyph: GlyphKey,
    /// Byte offset in the shaped text of the first character this glyph belongs to.
    pub cluster: u32,
    pub x_offset: f32,
    pub y_offset: f32,
    pub x_advance: f32,
}

/// Identity of a loaded face, including synthesis and variations.
#[derive(Clone, PartialEq, Eq, Hash)]
struct FaceId {
    blob: u64,
    index: u32,
    embolden: bool,
    skew: u32,
    variations: Vec<([u8; 4], u32)>,
}

struct Face {
    blob: Blob<u8>,
    index: u32,
    offset: u32,
    key: CacheKey,
    embolden: bool,
    skew: Option<f32>,
    /// Variable font axis values: matcher choices merged with user settings.
    variations: Vec<([u8; 4], f32)>,
    shaper: Option<harfrust::ShaperData>,
    instance: Option<harfrust::ShaperInstance>,
}

impl Face {
    fn font_ref(&self) -> FontRef<'_> {
        FontRef { data: self.blob.data(), offset: self.offset, key: self.key }
    }
}

pub struct FontSystem {
    collection: Collection,
    source_cache: SourceCache,
    family: String,
    faces: Vec<Face>,
    face_ids: HashMap<FaceId, u32>,
    primary: [u32; 4],
    glyphs: HashMap<(char, Style), Option<GlyphKey>>,
    emoji_faces: HashMap<char, Option<u32>>,
    fallback: Vec<String>,
    style_families: [Option<String>; 4],
    variations: Vec<([u8; 4], f32)>,
    hinting: Hinting,
    size_pt: f32,
    scale_factor: f64,
    features: Vec<harfrust::Feature>,
    buffer: Option<harfrust::UnicodeBuffer>,
    scaler: ScaleContext,
    size_px: f32,
    hint: bool,
    metrics: CellMetrics,
}

impl FontSystem {
    /// Loads `family` at `size_pt` points for a display with `scale_factor`.
    pub fn new(family: &str, size_pt: f32, scale_factor: f64) -> Result<Self, FontError> {
        let mut system = Self {
            collection: Collection::new(CollectionOptions { shared: false, system_fonts: true }),
            source_cache: SourceCache::default(),
            family: family.to_owned(),
            faces: Vec::new(),
            face_ids: HashMap::default(),
            primary: [0; 4],
            glyphs: HashMap::default(),
            emoji_faces: HashMap::default(),
            fallback: Vec::new(),
            style_families: [None, None, None, None],
            variations: Vec::new(),
            hinting: Hinting::Auto,
            size_pt,
            scale_factor,
            features: Vec::new(),
            buffer: None,
            scaler: ScaleContext::new(),
            size_px: 0.0,
            hint: true,
            metrics: CellMetrics {
                width: 1,
                height: 1,
                baseline: 1,
                underline_position: 1,
                underline_thickness: 1,
                strikeout_position: 1,
            },
        };
        system.reload_faces()?;
        system.set_size(size_pt, scale_factor);
        log::info!("font `{family}` at {size_pt}pt, cell {}x{} px", system.metrics.width, system.metrics.height);
        Ok(system)
    }

    fn families(&mut self) -> Vec<QueryFamily<'static>> {
        let generic = match self.family.to_ascii_lowercase().as_str() {
            "monospace" | "mono" => Some(GenericFamily::Monospace),
            "sans-serif" | "sans" => Some(GenericFamily::SansSerif),
            "serif" => Some(GenericFamily::Serif),
            _ => None,
        };
        let mut families = Vec::with_capacity(2);
        match generic {
            Some(generic) => families.push(QueryFamily::Generic(generic)),
            None => {
                let id = self.collection.family_id(&self.family);
                if let Some(id) = id {
                    families.push(QueryFamily::Id(id));
                } else {
                    log::warn!("font family `{}` not found, using monospace", self.family);
                }
            }
        }
        families.push(QueryFamily::Generic(GenericFamily::Monospace));
        families
    }

    /// Resolves the primary face of each style from the configured families.
    fn reload_faces(&mut self) -> Result<(), FontError> {
        for style in [Style::Regular, Style::Bold, Style::Italic, Style::BoldItalic] {
            let mut families = Vec::new();
            if let Some(name) = self.style_families[style as usize].clone() {
                match self.collection.family_id(&name) {
                    Some(id) => families.push(QueryFamily::Id(id)),
                    None => log::warn!("font family `{name}` not found"),
                }
            }
            families.extend(self.families());
            let face = self.query(&families, style, None).ok_or_else(|| FontError::NotFound(self.family.clone()))?;
            self.primary[style as usize] = face;
        }
        self.glyphs.clear();
        self.emoji_faces.clear();
        Ok(())
    }

    /// Families for bold, italic and bold italic. `None` uses the main family.
    pub fn set_style_families(&mut self, bold: Option<&str>, italic: Option<&str>, bold_italic: Option<&str>) {
        let families = [None, bold.map(String::from), italic.map(String::from), bold_italic.map(String::from)];
        if families != self.style_families {
            self.style_families = families;
            if let Err(error) = self.reload_faces() {
                log::error!("{error}");
            }
        }
    }

    /// Variable font axis values such as `("wght", 450.0)`. Invalid tags are skipped.
    pub fn set_variations(&mut self, variations: &[(String, f32)]) {
        let parsed: Vec<([u8; 4], f32)> = variations
            .iter()
            .filter_map(|(tag, value)| match <[u8; 4]>::try_from(tag.as_bytes()) {
                Ok(tag) => Some((tag, *value)),
                Err(_) => {
                    log::warn!("invalid font variation axis `{tag}`");
                    None
                }
            })
            .collect();
        if parsed != self.variations {
            self.variations = parsed;
            if let Err(error) = self.reload_faces() {
                log::error!("{error}");
            }
            self.set_size(self.size_pt, self.scale_factor);
        }
    }

    pub fn set_hinting(&mut self, hinting: Hinting) {
        if hinting != self.hinting {
            self.hinting = hinting;
            self.set_size(self.size_pt, self.scale_factor);
        }
    }

    /// Families tried before system fallback for characters the main font lacks.
    pub fn set_fallback(&mut self, families: &[String]) {
        self.fallback = families.to_vec();
        self.glyphs.clear();
    }

    /// OpenType features such as `"-calt"` or `"ss01"`. Invalid entries are logged and skipped.
    pub fn set_features(&mut self, features: &[String]) {
        self.features = features
            .iter()
            .filter_map(|f| match harfrust::Feature::from_str(f) {
                Ok(feature) => Some(feature),
                Err(_) => {
                    log::warn!("invalid font feature `{f}`");
                    None
                }
            })
            .collect();
    }

    pub fn set_size(&mut self, size_pt: f32, scale_factor: f64) {
        // Points at 96 DPI, scaled for the output.
        self.size_pt = size_pt;
        self.scale_factor = scale_factor;
        self.size_px = size_pt * scale_factor as f32 * 96.0 / 72.0;
        self.hint = match self.hinting {
            Hinting::Auto => scale_factor < 1.5,
            Hinting::On => true,
            Hinting::Off => false,
        };
        self.metrics = compute_metrics(&self.faces[self.primary[0] as usize], self.size_px);
    }

    pub fn size_px(&self) -> f32 {
        self.size_px
    }

    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    /// Finds a glyph for `ch`, falling back to other fonts when needed.
    pub fn glyph(&mut self, ch: char, style: Style) -> Option<GlyphKey> {
        if let Some(&cached) = self.glyphs.get(&(ch, style)) {
            return cached;
        }
        let primary = self.primary[style as usize];
        let found = self.map(primary, ch).or_else(|| {
            let mut families = Vec::new();
            for name in self.fallback.clone() {
                if let Some(id) = self.collection.family_id(&name) {
                    families.push(QueryFamily::Id(id));
                }
            }
            families.extend(self.families());
            families.extend([
                QueryFamily::Generic(GenericFamily::SansSerif),
                QueryFamily::Generic(GenericFamily::Emoji),
                QueryFamily::Generic(GenericFamily::Serif),
            ]);
            let face = self.query(&families, style, Some(ch))?;
            self.map(face, ch)
        });
        self.glyphs.insert((ch, style), found);
        found
    }

    /// Face used to render `ch` in `style`, after fallback.
    pub fn face_for(&mut self, ch: char, style: Style) -> u32 {
        self.glyph(ch, style).map_or(self.primary[style as usize], |key| key.face)
    }

    /// Face for a grapheme cluster. An emoji variation selector (U+FE0F)
    /// selects a color emoji font even when the text font has the character.
    pub fn face_for_cluster(&mut self, base: char, combining: Option<&str>, style: Style) -> u32 {
        if combining.is_some_and(|c| c.contains('\u{FE0F}'))
            && let Some(face) = self.emoji_face(base)
        {
            return face;
        }
        self.face_for(base, style)
    }

    fn emoji_face(&mut self, ch: char) -> Option<u32> {
        if let Some(&face) = self.emoji_faces.get(&ch) {
            return face;
        }
        let face = self.query(&[QueryFamily::Generic(GenericFamily::Emoji)], Style::Regular, Some(ch));
        self.emoji_faces.insert(ch, face);
        face
    }

    /// Shapes `text` with `face`. Clusters are byte offsets into `text`.
    pub fn shape(&mut self, face: u32, text: &str, out: &mut Vec<ShapedGlyph>) {
        out.clear();
        let size_px = self.size_px;
        let Face { blob, index, shaper, instance, variations, .. } = &mut self.faces[face as usize];
        let Ok(font) = harfrust::FontRef::from_index(blob.data(), *index) else { return };
        let data = shaper.get_or_insert_with(|| harfrust::ShaperData::new(&font));
        let instance = instance.get_or_insert_with(|| {
            harfrust::ShaperInstance::from_variations(
                &font,
                variations
                    .iter()
                    .map(|(tag, value)| harfrust::Variation { tag: harfrust::Tag::new(tag), value: *value }),
            )
        });
        let shaper = data.shaper(&font).instance(Some(instance)).build();
        let mut buffer = self.buffer.take().unwrap_or_default();
        buffer.push_str(text);
        buffer.set_direction(harfrust::Direction::LeftToRight);
        buffer.guess_segment_properties();
        let glyphs = shaper.shape(buffer, harfrust::ShapeOptions::new().features(&self.features));
        let scale = size_px / shaper.units_per_em().max(1) as f32;
        out.extend(glyphs.glyph_infos().iter().zip(glyphs.glyph_positions()).map(|(info, pos)| ShapedGlyph {
            glyph: GlyphKey { face, glyph: info.glyph_id as u16 },
            cluster: info.cluster,
            x_offset: pos.x_offset as f32 * scale,
            y_offset: pos.y_offset as f32 * scale,
            x_advance: pos.x_advance as f32 * scale,
        }));
        self.buffer = Some(glyphs.clear());
    }

    /// Draws a box drawing, block or Powerline character to fit the cell.
    pub fn sprite(&self, ch: char) -> Option<RasterizedGlyph> {
        let m = self.metrics;
        sprite::render(ch, m.width, m.height, m.baseline, m.underline_thickness)
    }

    fn map(&self, face: u32, ch: char) -> Option<GlyphKey> {
        let glyph = self.faces[face as usize].font_ref().charmap().map(ch);
        (glyph != 0).then_some(GlyphKey { face, glyph })
    }

    fn query(&mut self, families: &[QueryFamily<'_>], style: Style, ch: Option<char>) -> Option<u32> {
        let mut found: Option<(Blob<u8>, u32, Synthesis)> = None;
        let mut query = self.collection.query(&mut self.source_cache);
        query.set_families(families.iter().copied());
        query.set_attributes(style.attributes());
        query.matches_with(|font| {
            if let Some(ch) = ch {
                let covered = FontRef::from_index(font.blob.data(), font.index as usize)
                    .is_some_and(|f| f.charmap().map(ch) != 0);
                if !covered {
                    return QueryStatus::Continue;
                }
            }
            found = Some((font.blob.clone(), font.index, font.synthesis));
            QueryStatus::Stop
        });
        drop(query);
        let (blob, index, synthesis) = found?;
        self.intern_face(blob, index, synthesis)
    }

    fn intern_face(&mut self, blob: Blob<u8>, index: u32, synthesis: Synthesis) -> Option<u32> {
        let embolden = synthesis.embolden();
        let skew = synthesis.skew();
        // Axes chosen by the font matcher (for example wght for bold) win over user settings.
        let mut variations: Vec<([u8; 4], f32)> =
            synthesis.variation_settings().iter().map(|(tag, value)| (tag.to_be_bytes(), *value)).collect();
        for (tag, value) in &self.variations {
            if !variations.iter().any(|(t, _)| t == tag) {
                variations.push((*tag, *value));
            }
        }
        let id = FaceId {
            blob: blob.id(),
            index,
            embolden,
            skew: skew.unwrap_or(0.0).to_bits(),
            variations: variations.iter().map(|(tag, value)| (*tag, value.to_bits())).collect(),
        };
        if let Some(&face) = self.face_ids.get(&id) {
            return Some(face);
        }
        let font = FontRef::from_index(blob.data(), index as usize)?;
        let (offset, key) = (font.offset, font.key);
        let face = self.faces.len() as u32;
        self.faces.push(Face { blob, index, offset, key, embolden, skew, variations, shaper: None, instance: None });
        self.face_ids.insert(id, face);
        Some(face)
    }

    pub fn rasterize(&mut self, key: GlyphKey) -> Option<RasterizedGlyph> {
        let face = &self.faces[key.face as usize];
        let mut scaler = self
            .scaler
            .builder(face.font_ref())
            .size(self.size_px)
            .hint(self.hint)
            .variations(face.variations.iter().map(|(tag, value)| (swash::tag_from_bytes(tag), *value)))
            .build();
        let mut render = Render::new(&[
            Source::ColorOutline(0),
            Source::ColorBitmap(StrikeWith::BestFit),
            Source::Outline,
            Source::Bitmap(StrikeWith::BestFit),
        ]);
        render.format(Format::Alpha);
        if face.embolden {
            render.embolden(self.size_px / 32.0);
        }
        if let Some(degrees) = face.skew {
            render.transform(Some(Transform::skew(Angle::from_degrees(degrees), Angle::ZERO)));
        }
        let image = render.render(&mut scaler, key.glyph)?;
        let format = match image.content {
            Content::Color => GlyphFormat::Color,
            _ => GlyphFormat::Mask,
        };
        let glyph = RasterizedGlyph {
            format,
            width: image.placement.width,
            height: image.placement.height,
            left: image.placement.left,
            top: image.placement.top,
            data: image.data,
        };
        // Bitmap emoji come in fixed large strikes. Fit them to the cell height.
        let target = self.metrics.height;
        if format == GlyphFormat::Color && glyph.height > target && glyph.height > 0 {
            return Some(downscale(&glyph, target as f32 / glyph.height as f32));
        }
        Some(glyph)
    }
}

fn compute_metrics(face: &Face, px: f32) -> CellMetrics {
    let font = face.font_ref();
    let metrics = font.metrics(&[]).scale(px);
    let glyph_metrics = font.glyph_metrics(&[]).scale(px);
    let charmap = font.charmap();
    let advance = ['M', '0', ' ']
        .into_iter()
        .map(|c| charmap.map(c))
        .find(|&g| g != 0)
        .map(|g| glyph_metrics.advance_width(g))
        .unwrap_or(metrics.average_width);

    let ascent = metrics.ascent.round();
    let descent = metrics.descent.abs().round();
    let leading = metrics.leading.max(0.0).round();
    let height = (ascent + descent + leading).max(1.0);
    let baseline = ascent + (leading / 2.0).floor();
    let thickness = metrics.stroke_size.round().max(1.0);
    let underline = (baseline - metrics.underline_offset).round().clamp(0.0, height - thickness);
    let strikeout_offset =
        if metrics.strikeout_offset > 0.0 { metrics.strikeout_offset } else { metrics.x_height / 2.0 };
    let strikeout = (baseline - strikeout_offset).round().clamp(0.0, height - thickness);

    CellMetrics {
        width: advance.round().max(1.0) as u32,
        height: height as u32,
        baseline: baseline as u32,
        underline_position: underline as u32,
        underline_thickness: thickness as u32,
        strikeout_position: strikeout as u32,
    }
}

/// Box filter downscale for RGBA glyph bitmaps.
fn downscale(glyph: &RasterizedGlyph, scale: f32) -> RasterizedGlyph {
    let (sw, sh) = (glyph.width as usize, glyph.height as usize);
    let dw = ((sw as f32 * scale).round() as usize).max(1);
    let dh = ((sh as f32 * scale).round() as usize).max(1);
    let mut data = vec![0u8; dw * dh * 4];
    for dy in 0..dh {
        let y0 = dy * sh / dh;
        let y1 = ((dy + 1) * sh / dh).max(y0 + 1).min(sh);
        for dx in 0..dw {
            let x0 = dx * sw / dw;
            let x1 = ((dx + 1) * sw / dw).max(x0 + 1).min(sw);
            let mut sum = [0u32; 4];
            let mut count = 0;
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * sw + x) * 4;
                    let a = u32::from(glyph.data[i + 3]);
                    // Weight color by alpha so transparent pixels do not darken edges.
                    for (c, s) in sum.iter_mut().take(3).enumerate() {
                        *s += u32::from(glyph.data[i + c]) * a;
                    }
                    sum[3] += a;
                    count += 1;
                }
            }
            let o = (dy * dw + dx) * 4;
            for c in 0..3 {
                data[o + c] = sum[c].checked_div(sum[3]).unwrap_or(0) as u8;
            }
            data[o + 3] = (sum[3] / count) as u8;
        }
    }
    RasterizedGlyph {
        format: glyph.format,
        width: dw as u32,
        height: dh as u32,
        left: (glyph.left as f32 * scale).round() as i32,
        top: (glyph.top as f32 * scale).round() as i32,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_system_monospace_and_rasterizes() {
        let mut fonts = FontSystem::new("monospace", 12.0, 1.0).expect("a monospace font is installed");
        let m = fonts.metrics();
        assert!(m.width > 4 && m.height > m.width && m.baseline < m.height);
        let key = fonts.glyph('A', Style::Regular).expect("glyph for A");
        let glyph = fonts.rasterize(key).expect("rasterized A");
        assert_eq!(glyph.format, GlyphFormat::Mask);
        assert_eq!(glyph.data.len(), (glyph.width * glyph.height) as usize);
        assert!(fonts.glyph('中', Style::Regular).is_some() || fonts.glyph('λ', Style::Regular).is_some());
    }

    #[test]
    fn shapes_text_with_byte_clusters() {
        let mut fonts = FontSystem::new("monospace", 12.0, 1.0).unwrap();
        let face = fonts.face_for('a', Style::Regular);
        let mut out = Vec::new();
        fonts.shape(face, "aé=", &mut out);
        assert_eq!(out.first().map(|g| g.cluster), Some(0));
        assert_eq!(out.last().map(|g| g.cluster), Some(3));
        assert!(out.iter().all(|g| g.x_advance > 0.0));
        assert!(fonts.sprite('─').is_some());
    }
}
