//! Keyboard input encoding: legacy xterm sequences and the kitty keyboard protocol.
//!
//! Kitty protocol: <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>

use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey};

const DISAMBIGUATE: u8 = 1;
const REPORT_EVENT_TYPES: u8 = 2;
const REPORT_ALTERNATE_KEYS: u8 = 4;
const REPORT_ALL_KEYS: u8 = 8;
const REPORT_TEXT: u8 = 16;

/// Bytes to send to the pty for a key event, if any. `kitty_flags` are the
/// progressive enhancement flags the application enabled.
pub fn encode(event: &KeyEvent, mods: ModifiersState, app_cursor: bool, kitty_flags: u8) -> Option<Vec<u8>> {
    if kitty_flags == 0 {
        return (event.state == ElementState::Pressed).then(|| legacy(event, mods, app_cursor)).flatten();
    }
    kitty(event, mods, app_cursor, kitty_flags)
}

/// How a functional key is written in CSI form.
#[derive(Copy, Clone)]
struct Functional {
    number: u32,
    terminator: u8,
}

fn functional(key: NamedKey, location: KeyLocation) -> Option<Functional> {
    let right = location == KeyLocation::Right;
    let (number, terminator) = match key {
        NamedKey::Escape => (27, b'u'),
        NamedKey::Enter => (13, b'u'),
        NamedKey::Tab => (9, b'u'),
        NamedKey::Backspace => (127, b'u'),
        NamedKey::Insert => (2, b'~'),
        NamedKey::Delete => (3, b'~'),
        NamedKey::ArrowLeft => (1, b'D'),
        NamedKey::ArrowRight => (1, b'C'),
        NamedKey::ArrowUp => (1, b'A'),
        NamedKey::ArrowDown => (1, b'B'),
        NamedKey::PageUp => (5, b'~'),
        NamedKey::PageDown => (6, b'~'),
        NamedKey::Home => (1, b'H'),
        NamedKey::End => (1, b'F'),
        NamedKey::CapsLock => (57358, b'u'),
        NamedKey::ScrollLock => (57359, b'u'),
        NamedKey::NumLock => (57360, b'u'),
        NamedKey::PrintScreen => (57361, b'u'),
        NamedKey::Pause => (57362, b'u'),
        NamedKey::ContextMenu => (57363, b'u'),
        NamedKey::F1 => (1, b'P'),
        NamedKey::F2 => (1, b'Q'),
        NamedKey::F3 => (13, b'~'),
        NamedKey::F4 => (1, b'S'),
        NamedKey::F5 => (15, b'~'),
        NamedKey::F6 => (17, b'~'),
        NamedKey::F7 => (18, b'~'),
        NamedKey::F8 => (19, b'~'),
        NamedKey::F9 => (20, b'~'),
        NamedKey::F10 => (21, b'~'),
        NamedKey::F11 => (23, b'~'),
        NamedKey::F12 => (24, b'~'),
        NamedKey::Shift => (if right { 57447 } else { 57441 }, b'u'),
        NamedKey::Control => (if right { 57448 } else { 57442 }, b'u'),
        NamedKey::Alt => (if right { 57449 } else { 57443 }, b'u'),
        NamedKey::Meta => (if right { 57450 } else { 57444 }, b'u'),
        NamedKey::AudioVolumeDown => (57438, b'u'),
        NamedKey::AudioVolumeUp => (57439, b'u'),
        NamedKey::AudioVolumeMute => (57440, b'u'),
        NamedKey::MediaPlayPause => (57430, b'u'),
        NamedKey::MediaStop => (57432, b'u'),
        NamedKey::MediaTrackNext => (57435, b'u'),
        NamedKey::MediaTrackPrevious => (57436, b'u'),
        _ => return None,
    };
    Some(Functional { number, terminator })
}

fn is_modifier_key(key: NamedKey) -> bool {
    matches!(key, NamedKey::Shift | NamedKey::Control | NamedKey::Alt | NamedKey::Meta)
}

fn modifier_bits(mods: ModifiersState) -> u32 {
    u32::from(mods.shift_key())
        | u32::from(mods.alt_key()) << 1
        | u32::from(mods.control_key()) << 2
        | u32::from(mods.meta_key()) << 3
}

fn kitty(event: &KeyEvent, mods: ModifiersState, app_cursor: bool, flags: u8) -> Option<Vec<u8>> {
    let released = event.state == ElementState::Released;
    let event_type = if released {
        3
    } else if event.repeat {
        2
    } else {
        1
    };
    if released && flags & REPORT_EVENT_TYPES == 0 {
        return None;
    }
    if flags & (DISAMBIGUATE | REPORT_ALL_KEYS) == 0 && !released && !event.repeat {
        // Only event types or alternate keys were requested: presses stay legacy.
        return legacy(event, mods, app_cursor);
    }
    let all_keys = flags & REPORT_ALL_KEYS != 0;
    let bits = modifier_bits(mods);
    let reported_type = if flags & REPORT_EVENT_TYPES != 0 { event_type } else { 1 };

    match &event.logical_key {
        Key::Named(named) => {
            let named = *named;
            if is_modifier_key(named) && !all_keys {
                return None;
            }
            let Some(key) = functional(named, event.location) else {
                // Unmapped named keys that type text (such as Space) behave like text keys.
                return event.text.as_ref().filter(|_| !released).map(|t| t.as_bytes().to_vec());
            };
            let legacy_text = matches!(named, NamedKey::Enter | NamedKey::Tab | NamedKey::Backspace);
            if legacy_text && !all_keys && bits == 0 {
                return (!released).then(|| legacy(event, mods, app_cursor)).flatten();
            }
            if !released && !all_keys && bits == 0 && key.terminator != b'u' && reported_type == 1 {
                return legacy(event, mods, app_cursor);
            }
            if released && legacy_text && !all_keys {
                return None;
            }
            Some(csi(key.number, None, bits, reported_type, None, key.terminator))
        }
        Key::Character(text) => {
            let base = match &event.key_without_modifiers {
                Key::Character(base) => base.chars().next(),
                _ => text.chars().next(),
            }?;
            let code = base.to_lowercase().next().unwrap_or(base) as u32;
            let has_command_modifier = mods.control_key() || mods.alt_key() || mods.meta_key();
            if !all_keys {
                if released {
                    return None;
                }
                if !has_command_modifier {
                    return event.text.as_ref().map(|t| t.as_bytes().to_vec());
                }
            }
            let shifted = (flags & REPORT_ALTERNATE_KEYS != 0 && mods.shift_key())
                .then(|| text.chars().next())
                .flatten()
                .map(|c| c as u32)
                .filter(|&c| c != code);
            let associated = (all_keys && flags & REPORT_TEXT != 0 && !released)
                .then_some(event.text.as_ref())
                .flatten()
                .filter(|t| t.chars().all(|c| !c.is_control()))
                .map(|t| t.chars().map(|c| (c as u32).to_string()).collect::<Vec<_>>().join(":"));
            Some(csi(code, shifted, bits, reported_type, associated, b'u'))
        }
        _ => event.text.as_ref().filter(|_| !released).map(|t| t.as_bytes().to_vec()),
    }
}

fn csi(number: u32, shifted: Option<u32>, bits: u32, event_type: u32, text: Option<String>, terminator: u8) -> Vec<u8> {
    let mut out = format!("\x1b[{number}");
    if number == 1 && terminator != b'u' && bits == 0 && event_type == 1 && text.is_none() {
        // `CSI A` rather than `CSI 1A`.
        out.truncate(2);
    }
    if let Some(shifted) = shifted {
        out.push_str(&format!(":{shifted}"));
    }
    if bits != 0 || event_type != 1 || text.is_some() {
        out.push_str(&format!(";{}", bits + 1));
        if event_type != 1 {
            out.push_str(&format!(":{event_type}"));
        }
    }
    if let Some(text) = text {
        out.push(';');
        out.push_str(&text);
    }
    out.push(terminator as char);
    out.into_bytes()
}

/// xterm style encoding.
fn legacy(event: &KeyEvent, mods: ModifiersState, app_cursor: bool) -> Option<Vec<u8>> {
    let (ctrl, alt, shift) = (mods.control_key(), mods.alt_key(), mods.shift_key());
    // xterm modifier parameter: 1 + shift + 2*alt + 4*ctrl.
    let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);

    match &event.logical_key {
        Key::Named(named) => {
            let bytes = match named {
                NamedKey::Enter => with_alt(alt, b"\r"),
                NamedKey::Backspace => with_alt(alt, if ctrl { b"\x08" } else { b"\x7f" }),
                NamedKey::Tab if shift => b"\x1b[Z".to_vec(),
                NamedKey::Tab => with_alt(alt, b"\t"),
                NamedKey::Escape => with_alt(alt, b"\x1b"),
                NamedKey::ArrowUp => cursor_key(b'A', modifier, app_cursor),
                NamedKey::ArrowDown => cursor_key(b'B', modifier, app_cursor),
                NamedKey::ArrowRight => cursor_key(b'C', modifier, app_cursor),
                NamedKey::ArrowLeft => cursor_key(b'D', modifier, app_cursor),
                NamedKey::Home => cursor_key(b'H', modifier, app_cursor),
                NamedKey::End => cursor_key(b'F', modifier, app_cursor),
                NamedKey::Insert => tilde(2, modifier),
                NamedKey::Delete => tilde(3, modifier),
                NamedKey::PageUp => tilde(5, modifier),
                NamedKey::PageDown => tilde(6, modifier),
                NamedKey::F1 => ss3(b'P', modifier),
                NamedKey::F2 => ss3(b'Q', modifier),
                NamedKey::F3 => ss3(b'R', modifier),
                NamedKey::F4 => ss3(b'S', modifier),
                NamedKey::F5 => tilde(15, modifier),
                NamedKey::F6 => tilde(17, modifier),
                NamedKey::F7 => tilde(18, modifier),
                NamedKey::F8 => tilde(19, modifier),
                NamedKey::F9 => tilde(20, modifier),
                NamedKey::F10 => tilde(21, modifier),
                NamedKey::F11 => tilde(23, modifier),
                NamedKey::F12 => tilde(24, modifier),
                _ => return event.text.as_ref().map(|t| with_alt(alt, t.as_bytes())),
            };
            Some(bytes)
        }
        Key::Character(text) => {
            if ctrl {
                let mut chars = text.chars();
                if let (Some(c), None) = (chars.next(), chars.next())
                    && let Some(byte) = control_byte(c)
                {
                    return Some(with_alt(alt, &[byte]));
                }
            }
            let text = event.text.as_deref().unwrap_or(text);
            Some(with_alt(alt, text.as_bytes()))
        }
        _ => event.text.as_ref().map(|t| t.as_bytes().to_vec()),
    }
}

fn with_alt(alt: bool, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 1);
    if alt {
        out.push(0x1b);
    }
    out.extend_from_slice(bytes);
    out
}

fn cursor_key(key: u8, modifier: u8, app_cursor: bool) -> Vec<u8> {
    if modifier > 1 {
        format!("\x1b[1;{modifier}{}", key as char).into_bytes()
    } else if app_cursor {
        vec![0x1b, b'O', key]
    } else {
        vec![0x1b, b'[', key]
    }
}

fn ss3(key: u8, modifier: u8) -> Vec<u8> {
    if modifier > 1 { format!("\x1b[1;{modifier}{}", key as char).into_bytes() } else { vec![0x1b, b'O', key] }
}

fn tilde(number: u8, modifier: u8) -> Vec<u8> {
    if modifier > 1 {
        format!("\x1b[{number};{modifier}~").into_bytes()
    } else {
        format!("\x1b[{number}~").into_bytes()
    }
}

fn control_byte(c: char) -> Option<u8> {
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '/' | '7' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_bytes() {
        assert_eq!(control_byte('c'), Some(3));
        assert_eq!(control_byte('C'), Some(3));
        assert_eq!(control_byte('['), Some(0x1b));
        assert_eq!(control_byte('1'), None);
    }

    #[test]
    fn modified_keys() {
        assert_eq!(cursor_key(b'A', 1, false), b"\x1b[A");
        assert_eq!(cursor_key(b'A', 1, true), b"\x1bOA");
        assert_eq!(cursor_key(b'C', 5, true), b"\x1b[1;5C");
        assert_eq!(tilde(5, 3), b"\x1b[5;3~");
    }

    #[test]
    fn kitty_csi_forms() {
        assert_eq!(csi(97, None, 4, 1, None, b'u'), b"\x1b[97;5u");
        assert_eq!(csi(27, None, 0, 1, None, b'u'), b"\x1b[27u");
        assert_eq!(csi(1, None, 0, 1, None, b'A'), b"\x1b[A");
        assert_eq!(csi(1, None, 1, 3, None, b'A'), b"\x1b[1;2:3A");
        assert_eq!(csi(97, Some(65), 1, 1, Some("65".into()), b'u'), b"\x1b[97:65;2;65u");
        assert_eq!(csi(13, None, 0, 2, None, b'~'), b"\x1b[13;1:2~");
    }
}
