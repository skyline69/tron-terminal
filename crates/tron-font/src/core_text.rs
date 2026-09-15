//! System font fallback through Core Text on macOS.
//!
//! fontique falls back by script, and symbols such as `⏵` have the Common
//! script, so it finds nothing for them. Core Text knows which system font
//! covers each character, the way native macOS apps draw them.

use std::path::PathBuf;
use std::ptr;

use objc2_core_foundation::{CFRange, CFString, CFURL};
use objc2_core_text::{CTFont, kCTFontURLAttribute};

/// A font Core Text draws a character with.
pub(crate) struct SystemFont {
    pub family: String,
    /// The font file, when the font lives in one.
    pub path: Option<PathBuf>,
}

/// The font Core Text would draw `ch` with when `base` lacks it. `None` when
/// nothing better than `base` or the LastResort font covers it: LastResort
/// glyphs are boxes too.
pub(crate) fn font_for(base: &str, ch: char) -> Option<SystemFont> {
    let name = CFString::from_str(base);
    // SAFETY: a null matrix means the identity transform.
    let base = unsafe { CTFont::with_name(&name, 12.0, ptr::null()) };
    let text = CFString::from_str(ch.encode_utf8(&mut [0; 4]));
    let range = CFRange { location: 0, length: text.length() };
    // SAFETY: the range covers exactly the string.
    let font = unsafe { base.for_string(&text, range) };
    // SAFETY: both fonts are valid Core Text fonts.
    let (post_script, family, base_family) = unsafe {
        (font.post_script_name().to_string(), font.family_name().to_string(), base.family_name().to_string())
    };
    if post_script == "LastResort" || family == base_family {
        return None;
    }
    // SAFETY: the font is valid and the attribute key is a Core Text constant.
    let url = unsafe { font.attribute(kCTFontURLAttribute) };
    let path = url.and_then(|url| url.downcast::<CFURL>().ok()).and_then(|url| url.to_file_path());
    Some(SystemFont { family, path })
}
