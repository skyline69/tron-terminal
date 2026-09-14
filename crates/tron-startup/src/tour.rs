//! The Tour tab: pages that show what tron can draw. Most demos need escape
//! sequences ratatui cannot produce (styled underlines, scaled text, images,
//! hyperlinks), so they are written to the terminal directly into an area the
//! view leaves blank, and cleared by a full redraw when the page changes.

use std::io::Write;

use base64::Engine;
use ratatui::layout::Rect;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Page {
    Text,
    Unicode,
    RightToLeft,
    Images,
    ScaledText,
    Links,
}

impl Page {
    pub const ALL: [Page; 6] =
        [Page::Text, Page::Unicode, Page::RightToLeft, Page::Images, Page::ScaledText, Page::Links];

    pub fn title(self) -> &'static str {
        match self {
            Page::Text => "Text and styles",
            Page::Unicode => "Unicode and emoji",
            Page::RightToLeft => "Right-to-left",
            Page::Images => "Images",
            Page::ScaledText => "Scaled text",
            Page::Links => "Links and prompts",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Page::Text => "Ligatures from your font, every underline style in any color, and true color.",
            Page::Unicode => "Emoji sequences, flags, CJK and combining marks take the width they should.",
            Page::RightToLeft => {
                "Arabic and Hebrew show in reading order, and mixed lines keep their numbers in order."
            }
            Page::Images => {
                "Kitty graphics, Sixel and iTerm2 images, animated. Try kitten icat, chafa, yazi or mpv --vo=kitty."
            }
            Page::ScaledText => "Applications can draw text larger than a cell with the kitty text sizing protocol.",
            Page::Links => "Hyperlinks from programs and URLs in output open with Ctrl+click.",
        }
    }
}

/// Image number used for the animated demo image.
const IMAGE_ID: u32 = 7771;
const IMAGE_SIZE: (u32, u32) = (240, 120);
const IMAGE_FRAMES: u32 = 10;

/// Writes `text` at `row` of `area`, cut to the area's width, if that row exists.
fn at(out: &mut impl Write, area: Rect, row: u16, text: &str) {
    if row < area.height {
        let text = clip(text, usize::from(area.width));
        // Without auto-wrap, nothing spills onto the next line even if a width is misjudged.
        let _ = write!(out, "\x1b[?7l\x1b[{};{}H{text}\x1b[0m\x1b[?7h", area.y + row + 1, area.x + 1);
    }
}

/// `text` cut to `width` cells. Escape sequences pass through without taking
/// space. Scaled text (OSC 66) counts as its width.
fn clip(text: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut out = String::with_capacity(text.len());
    let mut used = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        if c == '\x1b' {
            // Copy the whole sequence: CSI up to its final byte, OSC and APC up to BEL or ST.
            let mut end = start + 1;
            let kind = chars.peek().map(|&(_, next)| next);
            let is_string = matches!(kind, Some(']' | '_' | 'P'));
            let mut previous = c;
            let mut osc_text = String::new();
            for (index, next) in chars.by_ref() {
                end = index + next.len_utf8();
                if is_string {
                    if next == '\x07' || (previous == '\x1b' && next == '\\') {
                        break;
                    }
                    osc_text.push(next);
                } else if index > start + 1 && ('@'..='~').contains(&next) {
                    break;
                }
                previous = next;
            }
            // Scaled text takes `scale * width` cells, or its text width times the scale.
            if let Some(rest) = osc_text.strip_prefix("]66;")
                && let Some((metadata, body)) = rest.split_once(';')
            {
                let get = |key: &str| {
                    metadata
                        .split(':')
                        .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('=')?.parse::<usize>().ok())
                };
                let scale = get("s").unwrap_or(1);
                let body = body.trim_end_matches(['\x07', '\x1b']);
                let cells = scale * get("w").unwrap_or_else(|| unicode_width::UnicodeWidthStr::width(body));
                if used + cells > width {
                    break;
                }
                used += cells;
            }
            out.push_str(&text[start..end]);
            continue;
        }
        let cells = c.width().unwrap_or(0);
        if used + cells > width {
            break;
        }
        used += cells;
        out.push(c);
    }
    out
}

/// Draws `page` into `area`. `image_sent` records that the demo image was
/// transmitted, so later visits only place it again.
pub fn paint(page: Page, area: Rect, out: &mut impl Write, image_sent: &mut bool) {
    match page {
        Page::Text => {
            at(out, area, 0, "fn render(cells: &[Cell]) -> Result<(), Error> {");
            at(out, area, 1, "    if a != b && c >= d || e <= f { return x => y; }");
            at(out, area, 2, "    let value = input |> parse |> eval; // :: ... === !== <=> ++");
            at(out, area, 3, "}");
            at(
                out,
                area,
                5,
                "\x1b[1mbold\x1b[0m   \x1b[3mitalic\x1b[0m   \x1b[1;3mbold italic\x1b[0m   \x1b[2mdim\x1b[0m   \
                 \x1b[9mstrikethrough\x1b[0m   \x1b[53moverline\x1b[0m",
            );
            at(
                out,
                area,
                7,
                "\x1b[4:3;58:2::255:92:117mcurly\x1b[0m   \x1b[4:2;58:2::79:214:255mdouble\x1b[0m   \
                 \x1b[4:4;58:2::195:139:255mdotted\x1b[0m   \x1b[4:5;58:2::92:230:166mdashed\x1b[0m   \
                 \x1b[4;58:2::255:200:107mstraight\x1b[0m",
            );
            let width = usize::from(area.width).min(64);
            let gradient: String = (0..width)
                .map(|i| {
                    let t = i as f32 / width.max(2) as f32;
                    let (r, g, b) = neon(t, 0.0);
                    format!("\x1b[38;2;{r};{g};{b}m█")
                })
                .collect();
            at(out, area, 9, &gradient);
        }
        Page::Unicode => {
            at(out, area, 0, "Emoji      👨‍👩‍👧‍👦  🧑🏽‍💻  ❤️‍🔥  🏳️‍🌈  🇩🇪 🇯🇵 🇧🇷");
            at(out, area, 2, "CJK        中文字符  日本語のテキスト  한국어");
            at(out, area, 4, "Math       ∀x ∈ ℝ: x² ≥ 0   ∑ᵢ aᵢ   λ → ∞   √2 ≈ 1.414");
            at(out, area, 6, "Combining  é ñ ü å  ạ̄  Z̤͔ȧ̤̈l̤g̈o");
            at(out, area, 8, "Drawing    ╭──────╮ ┏━━━━━━┓ ░▒▓█  ▁▂▃▄▅▆▇█  ⣿⡇ ");
            at(out, area, 9, "           ╰──────╯ ┗━━━━━━┛ ← ↑ → ↓ ▲ ▶ ▼ ◀");
        }
        Page::RightToLeft => {
            at(out, area, 0, "English first, then Arabic: مرحبا بالعالم");
            at(out, area, 2, "Hebrew in the middle: שלום עולם, and English again.");
            at(out, area, 4, "Numbers keep their order: السعر 1234 دولار");
            at(out, area, 6, "\x1b[2mTurn it off in Settings, Right-to-left text.");
        }
        Page::Images => {
            if !*image_sent {
                send_image(out);
                *image_sent = true;
            }
            at(out, area, 0, "\x1b_Ga=p,i=7771,p=1,C=1,q=2\x1b\\");
            at(out, area, 7, "\x1b[2mTen generated frames, sent with the kitty graphics protocol.");
        }
        Page::ScaledText => {
            at(out, area, 0, "\x1b]66;s=2;Twice as tall\x07");
            at(out, area, 2, "\x1b]66;s=3;HUGE\x07");
            at(out, area, 5, "Normal text, and \x1b]66;n=1:d=2:w=9;half sized text\x07 next to it.");
            at(out, area, 7, "\x1b[2mprintf '\\e]66;s=2;Hello\\a'");
        }
        Page::Links => {
            at(
                out,
                area,
                0,
                "\x1b]8;;https://github.com/skyline69/tron-terminal\x1b\\\x1b[38;2;79;214;255mtron on GitHub\x1b]8;;\x1b\\\x1b[0m  \x1b[2ma hyperlink from a program",
            );
            at(out, area, 1, "https://sw.kovidgoyal.net/kitty/  \x1b[2ma URL in plain output");
            at(out, area, 3, "\x1b[2mHold Ctrl and hover to underline, Ctrl+click to open.");
            at(out, area, 5, "Ctrl+Shift+Z / X  \x1b[2mprevious or next prompt");
            at(out, area, 6, "Ctrl+Shift+G      \x1b[2mselect the last output");
            at(out, area, 8, "\x1b[2mDesktop notification:");
            at(out, area, 9, "printf '\\e]777;notify;Done;Tests passed\\e\\\\'");
        }
    }
    let _ = out.flush();
}

/// Removes the demo image from the screen, keeping its data for later visits.
pub fn clear(out: &mut impl Write) {
    let _ = write!(out, "\x1b_Ga=d,d=i,i={IMAGE_ID},q=2\x1b\\");
    let _ = out.flush();
}

/// A cyan to magenta color at `t` (0..1), shifted by `phase`.
fn neon(t: f32, phase: f32) -> (u8, u8, u8) {
    let wave = 0.5 + 0.5 * ((t + phase) * std::f32::consts::TAU).sin();
    let mix = |a: f32, b: f32| ((a + (b - a) * wave) * 255.0) as u8;
    (mix(0.31, 0.76), mix(0.84, 0.35), mix(1.0, 1.0))
}

/// One animation frame: a neon gradient with a grid scrolling toward the bottom.
fn frame(index: u32) -> Vec<u8> {
    let (width, height) = IMAGE_SIZE;
    let phase = index as f32 / IMAGE_FRAMES as f32;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let (r, g, b) = neon(x as f32 / width as f32, phase);
            let line = x.is_multiple_of(20) || (y + (phase * 20.0) as u32).is_multiple_of(20);
            let (dx, dy) = (x as f32 / width as f32 - 0.5, y as f32 / height as f32 - 0.5);
            let shade = (1.0 - (dx * dx + dy * dy) * 1.6).clamp(0.25, 1.0) * if line { 1.0 } else { 0.45 };
            let scale = |c: u8| (f32::from(c) * shade) as u8;
            rgba.extend_from_slice(&[scale(r), scale(g), scale(b), 255]);
        }
    }
    rgba
}

/// Sends `keys` with `data` base64 encoded in chunks, as the protocol requires.
fn send_chunked(out: &mut impl Write, keys: &str, data: &[u8]) {
    let encoded = base64::engine::general_purpose::STANDARD.encode(data);
    let chunks: Vec<&[u8]> = encoded.as_bytes().chunks(4096).collect();
    for (index, chunk) in chunks.iter().enumerate() {
        let more = u8::from(index + 1 < chunks.len());
        let chunk = std::str::from_utf8(chunk).unwrap_or_default();
        if index == 0 {
            let _ = write!(out, "\x1b_G{keys},q=2,m={more};{chunk}\x1b\\");
        } else {
            let _ = write!(out, "\x1b_Gm={more};{chunk}\x1b\\");
        }
    }
}

fn send_image(out: &mut impl Write) {
    let (width, height) = IMAGE_SIZE;
    send_chunked(out, &format!("a=t,f=32,s={width},v={height},i={IMAGE_ID}"), &frame(0));
    for index in 1..IMAGE_FRAMES {
        send_chunked(out, &format!("a=f,f=32,s={width},v={height},i={IMAGE_ID},z=90"), &frame(index));
    }
    // Loop forever.
    let _ = write!(out, "\x1b_Ga=a,i={IMAGE_ID},s=3,v=1,q=2\x1b\\");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_page_writes_inside_its_area() {
        for page in Page::ALL {
            let mut out = Vec::new();
            let mut sent = false;
            paint(page, Rect::new(10, 5, 70, 12), &mut out, &mut sent);
            let text = String::from_utf8_lossy(&out);
            assert!(text.starts_with("\x1b[6;11H") || text.contains("\x1b[6;11H"), "{page:?}");
            assert!(!text.contains("\x1b[18;"), "{page:?} writes below the area");
        }
    }

    #[test]
    fn lines_are_clipped_to_the_area_without_breaking_escapes() {
        assert_eq!(clip("\x1b[1mbold\x1b[0m text", 6), "\x1b[1mbold\x1b[0m t");
        assert_eq!(clip("中文字符", 5), "中文");
        assert_eq!(
            clip("\x1b]8;;https://a.b\x1b\\link\x1b]8;;\x1b\\ after", 4),
            "\x1b]8;;https://a.b\x1b\\link\x1b]8;;\x1b\\"
        );
        assert_eq!(clip("ab\x1b]66;s=3;HUGE\x07cd", 13), "ab", "HUGE needs 12 cells");
        assert_eq!(clip("ab\x1b]66;s=3;HUGE\x07cd", 14), "ab\x1b]66;s=3;HUGE\x07");
    }

    #[test]
    fn the_image_is_sent_once_and_animates() {
        let mut out = Vec::new();
        let mut sent = false;
        paint(Page::Images, Rect::new(0, 0, 80, 10), &mut out, &mut sent);
        let first = String::from_utf8_lossy(&out).into_owned();
        assert!(sent && first.contains("a=t,f=32,s=240,v=120,i=7771"));
        assert_eq!(first.matches("a=f,").count(), IMAGE_FRAMES as usize - 1);
        assert!(first.contains("a=a,i=7771,s=3,v=1") && first.contains("a=p,i=7771"));
        let mut again = Vec::new();
        paint(Page::Images, Rect::new(0, 0, 80, 10), &mut again, &mut sent);
        let again = String::from_utf8_lossy(&again);
        assert!(!again.contains("a=t,") && again.contains("a=p,i=7771"));
        assert_ne!(frame(0), frame(3), "frames differ");
    }
}
