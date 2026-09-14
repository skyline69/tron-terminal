//! Mouse reporting (X10 and SGR encodings).

use winit::event::MouseButton;
use winit::keyboard::ModifiersState;

pub const WHEEL_UP: u8 = 64;
pub const WHEEL_DOWN: u8 = 65;
/// Motion without a pressed button.
pub const NO_BUTTON: u8 = 3;

pub fn button_code(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        _ => None,
    }
}

/// Encodes a mouse event at a zero based cell position.
pub fn encode(
    code: u8,
    pressed: bool,
    motion: bool,
    row: usize,
    col: usize,
    mods: ModifiersState,
    sgr: bool,
) -> Option<Vec<u8>> {
    let mut value = u32::from(code);
    if mods.shift_key() {
        value |= 4;
    }
    if mods.alt_key() {
        value |= 8;
    }
    if mods.control_key() {
        value |= 16;
    }
    if motion {
        value |= 32;
    }
    if sgr {
        let suffix = if pressed { 'M' } else { 'm' };
        return Some(format!("\x1b[<{value};{};{}{suffix}", col + 1, row + 1).into_bytes());
    }
    if !pressed {
        value = (value & !3) | 3;
    }
    let (x, y) = (col + 33, row + 33);
    if x > 255 || y > 255 || value + 32 > 255 {
        return None;
    }
    Some(vec![0x1b, b'[', b'M', (value + 32) as u8, x as u8, y as u8])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sgr_and_x10() {
        let none = ModifiersState::empty();
        assert_eq!(encode(0, true, false, 4, 9, none, true).unwrap(), b"\x1b[<0;10;5M");
        assert_eq!(encode(0, false, false, 4, 9, ModifiersState::CONTROL, true).unwrap(), b"\x1b[<16;10;5m");
        assert_eq!(encode(WHEEL_UP, true, false, 0, 0, none, false).unwrap(), b"\x1b[M`!!");
        assert_eq!(encode(2, false, false, 0, 0, none, false).unwrap(), b"\x1b[M#!!");
        assert!(encode(0, true, false, 0, 300, none, false).is_none());
    }
}
