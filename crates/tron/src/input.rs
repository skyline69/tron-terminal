//! Keyboard input encoding (xterm style).
//!
//! The kitty keyboard protocol will replace this for applications that ask for it.

use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Bytes to send to the pty for a key press, if any.
pub fn encode(event: &KeyEvent, mods: ModifiersState, app_cursor: bool) -> Option<Vec<u8>> {
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
    if modifier > 1 {
        format!("\x1b[1;{modifier}{}", key as char).into_bytes()
    } else {
        vec![0x1b, b'O', key]
    }
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
}
