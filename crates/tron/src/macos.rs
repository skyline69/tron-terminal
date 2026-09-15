//! macOS integration: Option as Alt and notifications.

use std::process::Command;

use tron_config::OptionAsAlt as Setting;
use winit::event::Modifiers;
use winit::keyboard::ModifiersKeyState;
use winit::platform::macos::{OptionAsAlt, WindowAttributesMacOS, WindowExtMacOS};
use winit::window::{Window, WindowAttributes};

fn option_as_alt(setting: Setting) -> OptionAsAlt {
    match setting {
        Setting::None => OptionAsAlt::None,
        Setting::Left => OptionAsAlt::OnlyLeft,
        Setting::Right => OptionAsAlt::OnlyRight,
        Setting::Both => OptionAsAlt::Both,
    }
}

pub fn window_attributes(attributes: WindowAttributes, setting: Setting) -> WindowAttributes {
    let platform = WindowAttributesMacOS::default().with_option_as_alt(option_as_alt(setting));
    attributes.with_platform_attributes(Box::new(platform))
}

pub fn set_option_as_alt(window: &dyn Window, setting: Setting) {
    let value = option_as_alt(setting);
    if window.option_as_alt() != value {
        window.set_option_as_alt(value);
    }
}

/// Whether the left and right Option keys are held.
pub fn option_keys(modifiers: &Modifiers) -> (bool, bool) {
    (modifiers.lalt_state() == ModifiersKeyState::Pressed, modifiers.ralt_state() == ModifiersKeyState::Pressed)
}

/// Whether the held Option keys act as Alt rather than composing characters.
pub fn option_is_alt(setting: Setting, (left, right): (bool, bool)) -> bool {
    match setting {
        Setting::None => false,
        Setting::Left => left,
        Setting::Right => right,
        Setting::Both => true,
    }
}

/// `program --app-name tron -- title body`, or a Notification Center banner when
/// `program` is `osascript`. The text is passed as arguments, never as script source.
pub fn notification(program: &str, title: &str, body: &str) -> Command {
    let mut command = Command::new(program);
    if program == "osascript" {
        command.args([
            "-e",
            "on run argv",
            "-e",
            "display notification (item 2 of argv) with title (item 1 of argv)",
            "-e",
            "end run",
            title,
            body,
        ]);
    } else {
        command.args(["--app-name", "tron", "--", title, body]);
    }
    command
}
