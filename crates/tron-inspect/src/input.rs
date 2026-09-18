//! winit events as egui input.
//!
//! `egui-winit` binds to winit 0.30, tron runs winit 0.31, and the two crates'
//! types do not meet, so the events the inspector needs are translated here.

use egui::{Modifiers, Pos2, RawInput, pos2, vec2};
use winit::event::{ButtonSource, ElementState, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, ModifiersState};

/// Collects winit events until the next frame is built.
#[derive(Default)]
pub struct Input {
    events: Vec<egui::Event>,
    modifiers: Modifiers,
    pointer: Pos2,
    focused: bool,
}

impl Input {
    /// Adds a window event. Returns false for events the inspector ignores.
    pub fn push(&mut self, event: &WindowEvent, scale: f32) -> bool {
        match event {
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = translate_modifiers(modifiers.state());
                self.events.push(egui::Event::ModifiersChanged(self.modifiers));
            }
            WindowEvent::Focused(focused) => {
                self.focused = *focused;
                self.events.push(egui::Event::WindowFocused(*focused));
            }
            WindowEvent::PointerMoved { position, .. } => {
                self.pointer = pos2(position.x as f32 / scale, position.y as f32 / scale);
                self.events.push(egui::Event::PointerMoved(self.pointer));
            }
            WindowEvent::PointerLeft { .. } => self.events.push(egui::Event::PointerGone),
            WindowEvent::PointerButton { state, position, button: ButtonSource::Mouse(button), .. } => {
                let Some(button) = translate_button(*button) else { return false };
                self.pointer = pos2(position.x as f32 / scale, position.y as f32 / scale);
                self.events.push(egui::Event::PointerButton {
                    pos: self.pointer,
                    button,
                    pressed: *state == ElementState::Pressed,
                    modifiers: self.modifiers,
                });
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (unit, delta) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (egui::MouseWheelUnit::Line, vec2(*x, *y)),
                    MouseScrollDelta::PixelDelta(position) => {
                        (egui::MouseWheelUnit::Point, vec2(position.x as f32 / scale, position.y as f32 / scale))
                    }
                    _ => return false,
                };
                self.events.push(egui::Event::MouseWheel {
                    unit,
                    delta,
                    phase: egui::TouchPhase::Move,
                    modifiers: self.modifiers,
                });
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                if let Some(key) = translate_key(&event.logical_key) {
                    // The shortcuts egui knows, so text fields copy and paste.
                    if pressed && self.modifiers.command {
                        match key {
                            egui::Key::C => self.events.push(egui::Event::Copy),
                            egui::Key::X => self.events.push(egui::Event::Cut),
                            _ => {}
                        }
                    }
                    self.events.push(egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers: self.modifiers,
                    });
                }
                // Text only when no command modifier is held, so Ctrl+C is not a "c".
                if pressed
                    && !self.modifiers.command
                    && !self.modifiers.alt
                    && let Some(text) = event.text.as_ref().filter(|text| !text.chars().any(char::is_control))
                {
                    self.events.push(egui::Event::Text(text.to_string()));
                }
            }
            _ => return false,
        }
        true
    }

    /// Text pasted into the inspector, such as into a search field.
    pub fn paste(&mut self, text: String) {
        self.events.push(egui::Event::Paste(text));
    }

    /// The input of the next frame, of a window `size` physical pixels large.
    pub fn take(&mut self, size: (u32, u32), scale: f32, time: f64) -> RawInput {
        let screen = egui::Rect::from_min_size(
            Pos2::ZERO,
            vec2(size.0 as f32 / scale, size.1 as f32 / scale).max(vec2(1.0, 1.0)),
        );
        RawInput {
            screen_rect: Some(screen),
            time: Some(time),
            events: std::mem::take(&mut self.events),
            focused: self.focused,
            ..Default::default()
        }
    }

    /// Whether a key that closes the window was pressed.
    pub fn wants_close(&self) -> bool {
        self.events.iter().any(|event| match event {
            egui::Event::Key { key: egui::Key::Escape, pressed: true, .. } => true,
            egui::Event::Key { key: egui::Key::W, pressed: true, modifiers, .. } => modifiers.command,
            _ => false,
        })
    }
}

fn translate_modifiers(state: ModifiersState) -> Modifiers {
    let command = if cfg!(target_os = "macos") { state.meta_key() } else { state.control_key() };
    Modifiers {
        alt: state.alt_key(),
        ctrl: state.control_key(),
        shift: state.shift_key(),
        mac_cmd: cfg!(target_os = "macos") && state.meta_key(),
        command,
    }
}

fn translate_button(button: winit::event::MouseButton) -> Option<egui::PointerButton> {
    match button {
        winit::event::MouseButton::Left => Some(egui::PointerButton::Primary),
        winit::event::MouseButton::Right => Some(egui::PointerButton::Secondary),
        winit::event::MouseButton::Middle => Some(egui::PointerButton::Middle),
        winit::event::MouseButton::Back => Some(egui::PointerButton::Extra1),
        winit::event::MouseButton::Forward => Some(egui::PointerButton::Extra2),
        _ => None,
    }
}

fn translate_key(key: &Key) -> Option<egui::Key> {
    match key {
        Key::Named(named) => egui::Key::from_name(&format!("{named:?}")),
        Key::Character(text) => egui::Key::from_name(text),
        _ => None,
    }
}

/// The winit cursor for what egui asks to show.
pub fn cursor_icon(icon: egui::CursorIcon) -> Option<winit::cursor::CursorIcon> {
    use egui::CursorIcon as Egui;
    use winit::cursor::CursorIcon as Winit;
    Some(match icon {
        Egui::None => return None,
        Egui::Default => Winit::Default,
        Egui::Text => Winit::Text,
        Egui::PointingHand => Winit::Pointer,
        Egui::Grab => Winit::Grab,
        Egui::Grabbing => Winit::Grabbing,
        Egui::Progress => Winit::Progress,
        Egui::Wait => Winit::Wait,
        Egui::NotAllowed => Winit::NotAllowed,
        Egui::NoDrop => Winit::NoDrop,
        Egui::Crosshair => Winit::Crosshair,
        Egui::Move => Winit::Move,
        Egui::ResizeHorizontal => Winit::EwResize,
        Egui::ResizeVertical => Winit::NsResize,
        Egui::ResizeColumn => Winit::ColResize,
        Egui::ResizeRow => Winit::RowResize,
        Egui::ResizeEast => Winit::EResize,
        Egui::ResizeWest => Winit::WResize,
        Egui::ResizeNorth => Winit::NResize,
        Egui::ResizeSouth => Winit::SResize,
        Egui::ResizeNeSw => Winit::NeswResize,
        Egui::ResizeNwSe => Winit::NwseResize,
        Egui::ResizeNorthEast => Winit::NeResize,
        Egui::ResizeNorthWest => Winit::NwResize,
        Egui::ResizeSouthEast => Winit::SeResize,
        Egui::ResizeSouthWest => Winit::SwResize,
        _ => Winit::Default,
    })
}
