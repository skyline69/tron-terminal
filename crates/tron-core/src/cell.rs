//! Cells, colors and text attributes.

use bitflags::bitflags;
use foldhash::HashMap;

/// A terminal color as set by SGR. Resolved to RGB by the renderer.
///
/// Packed into 32 bits: the top byte is a tag, the low 24 bits carry the payload.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Default)]
#[repr(transparent)]
pub struct Color(u32);

/// Unpacked view of a [`Color`].
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ColorKind {
    /// Foreground or background default, depending on use.
    Default,
    /// Index into the 256 color palette.
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl Color {
    const TAG_INDEXED: u32 = 1 << 24;
    const TAG_RGB: u32 = 2 << 24;

    pub const DEFAULT: Self = Self(0);

    pub const fn indexed(index: u8) -> Self {
        Self(Self::TAG_INDEXED | index as u32)
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self(Self::TAG_RGB | (r as u32) << 16 | (g as u32) << 8 | b as u32)
    }

    pub const fn kind(self) -> ColorKind {
        match self.0 >> 24 {
            1 => ColorKind::Indexed(self.0 as u8),
            2 => ColorKind::Rgb((self.0 >> 16) as u8, (self.0 >> 8) as u8, self.0 as u8),
            _ => ColorKind::Default,
        }
    }

    pub const fn is_default(self) -> bool {
        self.0 == 0
    }
}

impl std::fmt::Debug for Color {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.kind().fmt(f)
    }
}

bitflags! {
    /// Per-cell attributes.
    #[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
    pub struct Flags: u16 {
        const BOLD             = 1 << 0;
        const DIM              = 1 << 1;
        const ITALIC           = 1 << 2;
        const UNDERLINE        = 1 << 3;
        const DOUBLE_UNDERLINE = 1 << 4;
        const CURLY_UNDERLINE  = 1 << 5;
        const DOTTED_UNDERLINE = 1 << 6;
        const DASHED_UNDERLINE = 1 << 7;
        const BLINK            = 1 << 8;
        const INVERSE          = 1 << 9;
        const HIDDEN           = 1 << 10;
        const STRIKETHROUGH    = 1 << 11;
        const OVERLINE         = 1 << 12;
        /// First column of a double width character.
        const WIDE             = 1 << 13;
        /// Second column of a double width character. Holds no glyph.
        /// Also marks the empty last column of a row when a wide character wrapped.
        const WIDE_SPACER      = 1 << 14;
        /// The row stores combining characters for this cell.
        const GRAPHEME         = 1 << 15;

        const ANY_UNDERLINE = Self::UNDERLINE.bits()
            | Self::DOUBLE_UNDERLINE.bits()
            | Self::CURLY_UNDERLINE.bits()
            | Self::DOTTED_UNDERLINE.bits()
            | Self::DASHED_UNDERLINE.bits();
        /// Attributes that belong to the character, not the pen.
        const WIDTH_MASK = Self::WIDE.bits() | Self::WIDE_SPACER.bits();
        /// Flags that describe cell content rather than text style.
        const CONTENT_MASK = Self::WIDTH_MASK.bits() | Self::GRAPHEME.bits();
    }
}

/// One grid cell. 16 bytes, laid out in declaration order with no padding.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
#[repr(C)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: Flags,
    /// Index of the cell's rarely used attributes in the [`ExtendedTable`], 0 for none.
    pub extended: u16,
}

impl Default for Cell {
    fn default() -> Self {
        Self::BLANK
    }
}

impl Cell {
    /// An empty cell. The NUL character keeps the bit pattern all zero, so
    /// clearing rows compiles to `memset`. Empty cells read as spaces.
    pub const BLANK: Self =
        Self { ch: '\0', fg: Color::DEFAULT, bg: Color::DEFAULT, flags: Flags::empty(), extended: 0 };

    /// Whether the cell holds no character.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ch == '\0' || self.ch == ' '
    }

    /// A blank cell that keeps the background of `pen` (background color erase).
    #[inline]
    pub fn erased(pen: &Cell) -> Self {
        Self { bg: pen.bg, ..Self::BLANK }
    }
}

/// Cell attributes that few cells use: underline color, hyperlink and text size.
/// Cells refer to them by index, which keeps [`Cell`] small.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct Extended {
    pub underline_color: Color,
    /// Hyperlink id (OSC 8), 0 for none. See [`crate::Terminal::hyperlink`].
    pub link: u16,
    /// Text drawn larger than one cell (kitty text sizing, OSC 66).
    pub size: Option<TextSize>,
}

impl Extended {
    pub const DEFAULT: Self = Self { underline_color: Color::DEFAULT, link: 0, size: None };
}

impl Default for Extended {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A block of cells showing one piece of scaled text (kitty text sizing protocol).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct TextSize {
    /// Rows the block spans, 1 to 7. Columns are `scale * width`.
    pub scale: u8,
    /// Width in cells before scaling.
    pub width: u8,
    /// Fractional font scale inside the block, `numerator / denominator` (0 for none).
    pub numerator: u8,
    pub denominator: u8,
    /// Vertical alignment for fractional scales: 0 top, 1 bottom, 2 center.
    pub vertical: u8,
    /// Horizontal alignment for fractional scales: 0 left, 1 right, 2 center.
    pub horizontal: u8,
    /// Position of this cell inside the block. The text lives at (0, 0).
    pub dx: u8,
    pub dy: u8,
}

impl TextSize {
    /// Size of the block in cells: columns, rows.
    pub fn cells(&self) -> (usize, usize) {
        (usize::from(self.scale) * usize::from(self.width), usize::from(self.scale))
    }

    /// Font scale relative to normal text.
    pub fn font_scale(&self) -> f32 {
        let fraction = if self.numerator > 0 && self.denominator > self.numerator {
            f32::from(self.numerator) / f32::from(self.denominator)
        } else {
            1.0
        };
        f32::from(self.scale) * fraction
    }
}

/// Interned [`Extended`] values. Index 0 is the default. Entries are never
/// removed, so an index stays valid for the life of the terminal.
pub struct ExtendedTable {
    entries: Vec<Extended>,
    index: HashMap<Extended, u16>,
}

impl Default for ExtendedTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ExtendedTable {
    pub fn new() -> Self {
        let mut index = HashMap::default();
        index.insert(Extended::DEFAULT, 0);
        Self { entries: vec![Extended::DEFAULT], index }
    }

    #[inline]
    pub fn get(&self, id: u16) -> &Extended {
        self.entries.get(usize::from(id)).unwrap_or(&Extended::DEFAULT)
    }

    pub fn entries(&self) -> &[Extended] {
        &self.entries
    }

    /// Index of `value`, adding it when new. When the table is full, the
    /// closest existing entry is used: the same link without other attributes.
    pub fn intern(&mut self, value: Extended) -> u16 {
        if value == Extended::DEFAULT {
            return 0;
        }
        if let Some(&id) = self.index.get(&value) {
            return id;
        }
        if self.entries.len() > usize::from(u16::MAX) {
            let plain = Extended { link: value.link, ..Extended::DEFAULT };
            return self.index.get(&plain).copied().unwrap_or(0);
        }
        let id = self.entries.len() as u16;
        self.entries.push(value);
        self.index.insert(value, id);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_roundtrip() {
        assert_eq!(Color::DEFAULT.kind(), ColorKind::Default);
        assert_eq!(Color::indexed(200).kind(), ColorKind::Indexed(200));
        assert_eq!(Color::rgb(1, 2, 3).kind(), ColorKind::Rgb(1, 2, 3));
    }

    #[test]
    fn extended_table_interns_and_saturates() {
        let mut table = ExtendedTable::new();
        let red = Extended { underline_color: Color::indexed(1), ..Extended::DEFAULT };
        assert_eq!(table.intern(Extended::DEFAULT), 0);
        let id = table.intern(red);
        assert_eq!(table.intern(red), id);
        assert_eq!(*table.get(id), red);
        assert_eq!(*table.get(u16::MAX), Extended::DEFAULT);
    }

    #[test]
    fn cell_is_compact() {
        assert_eq!(std::mem::size_of::<Cell>(), 16);
        assert_eq!(std::mem::offset_of!(Cell, ch), 0);
    }
}
