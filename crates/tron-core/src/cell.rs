//! Cells, colors and text attributes.

use bitflags::bitflags;

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

/// One grid cell. 20 bytes.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub underline_color: Color,
    pub flags: Flags,
}

impl Default for Cell {
    fn default() -> Self {
        Self::BLANK
    }
}

impl Cell {
    /// An empty cell. The NUL character keeps the bit pattern all zero, so
    /// clearing rows compiles to `memset`. Empty cells read as spaces.
    pub const BLANK: Self = Self {
        ch: '\0',
        fg: Color::DEFAULT,
        bg: Color::DEFAULT,
        underline_color: Color::DEFAULT,
        flags: Flags::empty(),
    };

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
    fn cell_is_compact() {
        assert_eq!(std::mem::size_of::<Cell>(), 20);
    }
}
