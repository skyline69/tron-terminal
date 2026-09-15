//! Keyboard input encoding: legacy xterm sequences and the kitty keyboard protocol.
//!
//! Kitty protocol: <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>

use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey};

const DISAMBIGUATE: u8 = 1;
const REPORT_EVENT_TYPES: u8 = 2;
const REPORT_ALTERNATE_KEYS: u8 = 4;
const REPORT_ALL_KEYS: u8 = 8;
const REPORT_TEXT: u8 = 16;

/// Whether Option, when it is not configured as Alt, composes a character with
/// `key` on macOS. It composes nothing with Backspace, so Option+Backspace always
/// acts as Alt and sends `ESC DEL`, which deletes the word before the cursor.
#[cfg(any(target_os = "macos", test))]
pub fn option_composes(key: &Key) -> bool {
    !matches!(key, Key::Named(NamedKey::Backspace))
}

/// Modifier keys held during a key event, as the kitty protocol distinguishes them.
#[derive(Copy, Clone, Default, PartialEq, Eq, Debug)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    /// The logo key (winit's `META`).
    pub super_key: bool,
    pub hyper: bool,
    pub meta: bool,
}

impl Mods {
    /// Combines winit's modifier state with modifiers winit does not track.
    pub fn new(state: ModifiersState, hyper: bool, meta: bool) -> Self {
        Self {
            shift: state.shift_key(),
            alt: state.alt_key(),
            ctrl: state.control_key(),
            super_key: state.meta_key(),
            hyper,
            meta,
        }
    }

    /// Kitty modifier bits: shift 1, alt 2, ctrl 4, super 8, hyper 16, meta 32.
    /// Caps Lock and Num Lock are not reported: winit does not expose lock state.
    fn bits(self) -> u32 {
        u32::from(self.shift)
            | u32::from(self.alt) << 1
            | u32::from(self.ctrl) << 2
            | u32::from(self.super_key) << 3
            | u32::from(self.hyper) << 4
            | u32::from(self.meta) << 5
    }

    fn command(self) -> bool {
        self.ctrl || self.alt || self.super_key || self.hyper || self.meta
    }
}

/// Terminal modes that change how keys are encoded.
#[derive(Copy, Clone, Default, Debug)]
pub struct KeyModes {
    pub app_cursor: bool,
    /// DECKPAM: the numeric keypad sends SS3 sequences.
    pub app_keypad: bool,
    /// Kitty progressive enhancement flags.
    pub kitty_flags: u8,
}

/// Bytes to send to the pty for a key event, if any.
pub fn encode(event: &KeyEvent, mods: Mods, modes: KeyModes) -> Option<Vec<u8>> {
    if modes.kitty_flags == 0 {
        return (event.state == ElementState::Pressed).then(|| legacy(event, mods, modes)).flatten();
    }
    kitty(event, mods, modes)
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
        // winit reports the logo key as Meta.
        NamedKey::Meta => (if right { 57450 } else { 57444 }, b'u'),
        // Deprecated in keyboard-types, but winit's xkb backend still reports Hyper_L/R this way.
        #[allow(deprecated)]
        NamedKey::Hyper => (if right { 57451 } else { 57445 }, b'u'),
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

#[allow(deprecated)]
fn is_modifier_key(key: NamedKey) -> bool {
    matches!(key, NamedKey::Shift | NamedKey::Control | NamedKey::Alt | NamedKey::Meta | NamedKey::Hyper)
}

/// A key on the numeric keypad, as the kitty protocol numbers it.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
struct Keypad {
    number: u32,
    /// The key types text (digits and operators), as opposed to navigation keys.
    text: bool,
}

/// Kitty KP_* code of a numpad key event.
fn keypad(event: &KeyEvent) -> Option<Keypad> {
    if event.location != KeyLocation::Numpad {
        return None;
    }
    let navigation = match &event.logical_key {
        Key::Named(NamedKey::Enter) => Some(57414),
        Key::Named(NamedKey::ArrowLeft) => Some(57417),
        Key::Named(NamedKey::ArrowRight) => Some(57418),
        Key::Named(NamedKey::ArrowUp) => Some(57419),
        Key::Named(NamedKey::ArrowDown) => Some(57420),
        Key::Named(NamedKey::PageUp) => Some(57421),
        Key::Named(NamedKey::PageDown) => Some(57422),
        Key::Named(NamedKey::Home) => Some(57423),
        Key::Named(NamedKey::End) => Some(57424),
        Key::Named(NamedKey::Insert) => Some(57425),
        Key::Named(NamedKey::Delete) => Some(57426),
        Key::Named(NamedKey::Clear) => Some(57427),
        _ => None,
    };
    if let Some(number) = navigation {
        return Some(Keypad { number, text: false });
    }
    let PhysicalKey::Code(code) = event.physical_key else { return None };
    let number = match code {
        KeyCode::Numpad0 => 57399,
        KeyCode::Numpad1 => 57400,
        KeyCode::Numpad2 => 57401,
        KeyCode::Numpad3 => 57402,
        KeyCode::Numpad4 => 57403,
        KeyCode::Numpad5 => 57404,
        KeyCode::Numpad6 => 57405,
        KeyCode::Numpad7 => 57406,
        KeyCode::Numpad8 => 57407,
        KeyCode::Numpad9 => 57408,
        KeyCode::NumpadDecimal => 57409,
        KeyCode::NumpadDivide => 57410,
        KeyCode::NumpadMultiply | KeyCode::NumpadStar => 57411,
        KeyCode::NumpadSubtract => 57412,
        KeyCode::NumpadAdd => 57413,
        KeyCode::NumpadEnter => 57414,
        KeyCode::NumpadEqual => 57415,
        KeyCode::NumpadComma => 57416,
        _ => return None,
    };
    // Numpad 5 without Num Lock types nothing: it is KP_BEGIN.
    let text = matches!(event.logical_key, Key::Character(_));
    Some(Keypad { number: if text || code != KeyCode::Numpad5 { number } else { 57427 }, text })
}

/// The character of a physical key in the US PC-101 layout, for the kitty base layout key.
fn base_layout_key(key: PhysicalKey) -> Option<char> {
    let PhysicalKey::Code(code) = key else { return None };
    Some(match code {
        KeyCode::KeyA => 'a',
        KeyCode::KeyB => 'b',
        KeyCode::KeyC => 'c',
        KeyCode::KeyD => 'd',
        KeyCode::KeyE => 'e',
        KeyCode::KeyF => 'f',
        KeyCode::KeyG => 'g',
        KeyCode::KeyH => 'h',
        KeyCode::KeyI => 'i',
        KeyCode::KeyJ => 'j',
        KeyCode::KeyK => 'k',
        KeyCode::KeyL => 'l',
        KeyCode::KeyM => 'm',
        KeyCode::KeyN => 'n',
        KeyCode::KeyO => 'o',
        KeyCode::KeyP => 'p',
        KeyCode::KeyQ => 'q',
        KeyCode::KeyR => 'r',
        KeyCode::KeyS => 's',
        KeyCode::KeyT => 't',
        KeyCode::KeyU => 'u',
        KeyCode::KeyV => 'v',
        KeyCode::KeyW => 'w',
        KeyCode::KeyX => 'x',
        KeyCode::KeyY => 'y',
        KeyCode::KeyZ => 'z',
        KeyCode::Digit0 => '0',
        KeyCode::Digit1 => '1',
        KeyCode::Digit2 => '2',
        KeyCode::Digit3 => '3',
        KeyCode::Digit4 => '4',
        KeyCode::Digit5 => '5',
        KeyCode::Digit6 => '6',
        KeyCode::Digit7 => '7',
        KeyCode::Digit8 => '8',
        KeyCode::Digit9 => '9',
        KeyCode::Minus => '-',
        KeyCode::Equal => '=',
        KeyCode::BracketLeft => '[',
        KeyCode::BracketRight => ']',
        KeyCode::Backslash | KeyCode::IntlBackslash => '\\',
        KeyCode::Semicolon => ';',
        KeyCode::Quote => '\'',
        KeyCode::Backquote => '`',
        KeyCode::Comma => ',',
        KeyCode::Period => '.',
        KeyCode::Slash => '/',
        KeyCode::Space => ' ',
        _ => return None,
    })
}

fn kitty(event: &KeyEvent, mods: Mods, modes: KeyModes) -> Option<Vec<u8>> {
    let flags = modes.kitty_flags;
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
        return legacy(event, mods, modes);
    }
    let all_keys = flags & REPORT_ALL_KEYS != 0;
    let bits = mods.bits();
    let reported_type = if flags & REPORT_EVENT_TYPES != 0 { event_type } else { 1 };
    let associated_text = || {
        (all_keys && flags & REPORT_TEXT != 0 && !released)
            .then_some(event.text.as_ref())
            .flatten()
            .filter(|t| t.chars().all(|c| !c.is_control()))
            .map(|t| t.chars().map(|c| (c as u32).to_string()).collect::<Vec<_>>().join(":"))
    };

    // Keypad keys have their own numbers. Keys that type text stay text unless
    // every key is reported or a command modifier is held.
    if let Some(key) = keypad(event) {
        if key.text && !all_keys {
            if released {
                return None;
            }
            if !mods.command() {
                return event.text.as_ref().map(|t| t.as_bytes().to_vec());
            }
        }
        let text = if key.text { associated_text() } else { None };
        return Some(csi(key.number, None, None, bits, reported_type, text, b'u'));
    }

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
                return (!released).then(|| legacy(event, mods, modes)).flatten();
            }
            if !released && !all_keys && bits == 0 && key.terminator != b'u' && reported_type == 1 {
                return legacy(event, mods, modes);
            }
            if released && legacy_text && !all_keys {
                return None;
            }
            Some(csi(key.number, None, None, bits, reported_type, None, key.terminator))
        }
        Key::Character(text) => {
            let base = match &event.key_without_modifiers {
                Key::Character(base) => base.chars().next(),
                _ => text.chars().next(),
            }?;
            let code = base.to_lowercase().next().unwrap_or(base) as u32;
            if !all_keys {
                if released {
                    return None;
                }
                if !mods.command() {
                    return event.text.as_ref().map(|t| t.as_bytes().to_vec());
                }
            }
            let alternates = flags & REPORT_ALTERNATE_KEYS != 0;
            let shifted = (alternates && mods.shift)
                .then(|| text.chars().next())
                .flatten()
                .map(|c| c as u32)
                .filter(|&c| c != code);
            let base_layout =
                base_layout_key(event.physical_key).map(|c| c as u32).filter(|&c| alternates && c != code);
            Some(csi(code, shifted, base_layout, bits, reported_type, associated_text(), b'u'))
        }
        _ => event.text.as_ref().filter(|_| !released).map(|t| t.as_bytes().to_vec()),
    }
}

fn csi(
    number: u32,
    shifted: Option<u32>,
    base_layout: Option<u32>,
    bits: u32,
    event_type: u32,
    text: Option<String>,
    terminator: u8,
) -> Vec<u8> {
    let mut out = format!("\x1b[{number}");
    if number == 1 && terminator != b'u' && bits == 0 && event_type == 1 && text.is_none() {
        // `CSI A` rather than `CSI 1A`.
        out.truncate(2);
    }
    if shifted.is_some() || base_layout.is_some() {
        out.push(':');
        if let Some(shifted) = shifted {
            out.push_str(&shifted.to_string());
        }
    }
    if let Some(base) = base_layout {
        out.push_str(&format!(":{base}"));
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

/// SS3 sequence of a numpad key in application keypad mode (xterm).
fn app_keypad_key(event: &KeyEvent) -> Option<Vec<u8>> {
    if event.location != KeyLocation::Numpad || !matches!(event.logical_key, Key::Character(_)) {
        return None;
    }
    let PhysicalKey::Code(code) = event.physical_key else { return None };
    let final_byte = match code {
        KeyCode::Numpad0 => b'p',
        KeyCode::Numpad1 => b'q',
        KeyCode::Numpad2 => b'r',
        KeyCode::Numpad3 => b's',
        KeyCode::Numpad4 => b't',
        KeyCode::Numpad5 => b'u',
        KeyCode::Numpad6 => b'v',
        KeyCode::Numpad7 => b'w',
        KeyCode::Numpad8 => b'x',
        KeyCode::Numpad9 => b'y',
        KeyCode::NumpadMultiply | KeyCode::NumpadStar => b'j',
        KeyCode::NumpadAdd => b'k',
        KeyCode::NumpadComma => b'l',
        KeyCode::NumpadSubtract => b'm',
        KeyCode::NumpadDecimal => b'n',
        KeyCode::NumpadDivide => b'o',
        KeyCode::NumpadEqual => b'X',
        _ => return None,
    };
    Some(vec![0x1b, b'O', final_byte])
}

/// xterm style encoding.
fn legacy(event: &KeyEvent, mods: Mods, modes: KeyModes) -> Option<Vec<u8>> {
    // Command combinations are shortcuts on macOS. Unbound ones type nothing
    // rather than the plain key.
    #[cfg(target_os = "macos")]
    if mods.super_key {
        return None;
    }
    let (ctrl, alt, shift) = (mods.ctrl, mods.alt, mods.shift);
    let app_cursor = modes.app_cursor;
    if modes.app_keypad
        && !mods.command()
        && !shift
        && let Some(bytes) = app_keypad_key(event)
    {
        return Some(bytes);
    }
    if modes.app_keypad
        && !mods.command()
        && event.location == KeyLocation::Numpad
        && event.logical_key == Key::Named(NamedKey::Enter)
    {
        return Some(b"\x1bOM".to_vec());
    }
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
    fn option_backspace_acts_as_alt() {
        assert!(!option_composes(&Key::Named(NamedKey::Backspace)));
        assert!(option_composes(&Key::Character("e".into())));
        assert!(option_composes(&Key::Named(NamedKey::ArrowLeft)));
        // With Alt kept, shells, readline and tmux see a delete-word key.
        let backspace = key(Key::Named(NamedKey::Backspace), KeyCode::Backspace, KeyLocation::Standard, None);
        let alt = Mods { alt: true, ..Mods::default() };
        assert_eq!(encode(&backspace, alt, KeyModes::default()).as_deref(), Some(&b"\x1b\x7f"[..]));
        assert_eq!(encode(&backspace, alt, kitty_modes(DISAMBIGUATE)).as_deref(), Some(&b"\x1b[127;3u"[..]));
    }

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
        assert_eq!(csi(97, None, None, 4, 1, None, b'u'), b"\x1b[97;5u");
        assert_eq!(csi(27, None, None, 0, 1, None, b'u'), b"\x1b[27u");
        assert_eq!(csi(1, None, None, 0, 1, None, b'A'), b"\x1b[A");
        assert_eq!(csi(1, None, None, 1, 3, None, b'A'), b"\x1b[1;2:3A");
        assert_eq!(csi(97, Some(65), None, 1, 1, Some("65".into()), b'u'), b"\x1b[97:65;2;65u");
        assert_eq!(csi(13, None, None, 0, 2, None, b'~'), b"\x1b[13;1:2~");
    }

    #[test]
    fn base_layout_key_forms() {
        // ctrl+С on a Cyrillic layout: code 1089, base layout c.
        assert_eq!(csi(1089, None, Some(99), 4, 1, None, b'u'), b"\x1b[1089::99;5u");
        assert_eq!(csi(1089, Some(1057), Some(99), 5, 1, None, b'u'), b"\x1b[1089:1057:99;6u");
        assert_eq!(base_layout_key(PhysicalKey::Code(KeyCode::KeyC)), Some('c'));
        assert_eq!(base_layout_key(PhysicalKey::Code(KeyCode::Slash)), Some('/'));
        assert_eq!(base_layout_key(PhysicalKey::Code(KeyCode::F1)), None);
    }

    fn key(logical: Key, physical: KeyCode, location: KeyLocation, text: Option<&str>) -> KeyEvent {
        KeyEvent {
            physical_key: PhysicalKey::Code(physical),
            logical_key: logical.clone(),
            text: text.map(Into::into),
            location,
            state: ElementState::Pressed,
            repeat: false,
            text_with_all_modifiers: text.map(Into::into),
            key_without_modifiers: logical,
        }
    }

    fn kitty_modes(flags: u8) -> KeyModes {
        KeyModes { kitty_flags: flags, ..KeyModes::default() }
    }

    #[test]
    fn keypad_keys_under_kitty_flags() {
        let one = key(Key::Character("1".into()), KeyCode::Numpad1, KeyLocation::Numpad, Some("1"));
        let ctrl = Mods { ctrl: true, ..Mods::default() };
        assert_eq!(encode(&one, Mods::default(), kitty_modes(DISAMBIGUATE)).unwrap(), b"1");
        assert_eq!(encode(&one, ctrl, kitty_modes(DISAMBIGUATE)).unwrap(), b"\x1b[57400;5u");
        assert_eq!(encode(&one, Mods::default(), kitty_modes(REPORT_ALL_KEYS)).unwrap(), b"\x1b[57400u");
        let left = key(Key::Named(NamedKey::ArrowLeft), KeyCode::Numpad4, KeyLocation::Numpad, None);
        assert_eq!(encode(&left, Mods::default(), kitty_modes(DISAMBIGUATE)).unwrap(), b"\x1b[57417u");
        let enter = key(Key::Named(NamedKey::Enter), KeyCode::NumpadEnter, KeyLocation::Numpad, Some("\r"));
        assert_eq!(encode(&enter, Mods::default(), kitty_modes(DISAMBIGUATE)).unwrap(), b"\x1b[57414u");
    }

    #[test]
    fn application_keypad_sends_ss3() {
        let modes = KeyModes { app_keypad: true, ..KeyModes::default() };
        let one = key(Key::Character("1".into()), KeyCode::Numpad1, KeyLocation::Numpad, Some("1"));
        assert_eq!(encode(&one, Mods::default(), modes).unwrap(), b"\x1bOq");
        let enter = key(Key::Named(NamedKey::Enter), KeyCode::NumpadEnter, KeyLocation::Numpad, Some("\r"));
        assert_eq!(encode(&enter, Mods::default(), modes).unwrap(), b"\x1bOM");
        assert_eq!(encode(&one, Mods::default(), KeyModes::default()).unwrap(), b"1");
    }

    #[test]
    fn alternate_keys_include_base_layout() {
        let cyrillic = key(Key::Character("с".into()), KeyCode::KeyC, KeyLocation::Standard, Some("с"));
        let ctrl = Mods { ctrl: true, ..Mods::default() };
        let bytes = encode(&cyrillic, ctrl, kitty_modes(DISAMBIGUATE | REPORT_ALTERNATE_KEYS)).unwrap();
        assert_eq!(bytes, b"\x1b[1089::99;5u");
        let latin = key(Key::Character("c".into()), KeyCode::KeyC, KeyLocation::Standard, Some("c"));
        assert_eq!(encode(&latin, ctrl, kitty_modes(DISAMBIGUATE | REPORT_ALTERNATE_KEYS)).unwrap(), b"\x1b[99;5u");
    }

    #[test]
    fn modifier_bits_keep_hyper_and_meta_apart() {
        let mods = Mods { ctrl: true, super_key: true, hyper: true, meta: true, ..Mods::default() };
        assert_eq!(mods.bits(), 4 | 8 | 16 | 32);
        assert!(Mods { hyper: true, ..Mods::default() }.command());
        assert_eq!(Mods::default().bits(), 0);
    }
}
