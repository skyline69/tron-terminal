//! macOS integration: Option as Alt, notifications and the title bar folder icon.

use std::path::Path;
use std::process::Command;

use objc2_app_kit::NSView;
use objc2_foundation::{NSString, NSURL};
use tron_config::OptionAsAlt as Setting;
use winit::event::Modifiers;
use winit::keyboard::ModifiersKeyState;
use winit::platform::macos::{OptionAsAlt, WindowAttributesMacOS, WindowExtMacOS};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowAttributes};

/// Shows `directory` as the window's proxy icon, the folder icon beside the
/// title. It can be dragged, and Command-clicking it lists the parent folders.
pub fn set_represented_directory(window: &dyn Window, directory: Option<&Path>) {
    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return };
    // SAFETY: winit's handle is the window's content view, used on the main thread
    // while the window is alive.
    let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
    let Some(ns_window) = view.window() else { return };
    let url = directory
        .and_then(Path::to_str)
        .map(|path| NSURL::fileURLWithPath_isDirectory(&NSString::from_str(path), true));
    ns_window.setRepresentedURL(url.as_deref());
}

fn option_as_alt(setting: Setting) -> OptionAsAlt {
    match setting {
        Setting::None => OptionAsAlt::None,
        Setting::Left => OptionAsAlt::OnlyLeft,
        Setting::Right => OptionAsAlt::OnlyRight,
        Setting::Both => OptionAsAlt::Both,
    }
}

pub fn window_attributes(attributes: WindowAttributes, setting: Setting) -> WindowAttributes {
    // The terminal background, with its opacity and shaders, extends under a
    // transparent title bar. A translucent window otherwise leaves the title bar
    // without any background. Text starts below it, see `top_inset` in main.rs.
    let platform = WindowAttributesMacOS::default()
        .with_option_as_alt(option_as_alt(setting))
        .with_titlebar_transparent(true)
        .with_fullsize_content_view(true);
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
