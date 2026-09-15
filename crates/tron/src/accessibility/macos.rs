//! NSAccessibility adapter, installed on the window's content view.

use accesskit::TreeUpdate;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use super::{Activation, Deactivation, IgnoreActions};

pub struct Adapter(accesskit_macos::SubclassingAdapter);

impl Adapter {
    /// AppKit never turns accessibility off again, so `_deactivation` is unused.
    pub fn new(activation: Activation, _deactivation: Deactivation, window: &dyn Window) -> Option<Self> {
        let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else { return None };
        // SAFETY: runs on the main thread with the window's content view, and
        // `Session` drops the adapter before the window.
        let adapter = unsafe {
            accesskit_macos::SubclassingAdapter::new(handle.ns_view.as_ptr().cast(), activation, IgnoreActions)
        };
        Some(Self(adapter))
    }

    pub fn set_focused(&mut self, focused: bool) {
        if let Some(events) = self.0.update_view_focus_state(focused) {
            events.raise();
        }
    }

    pub fn update_if_active(&mut self, tree: impl FnOnce() -> TreeUpdate) {
        if let Some(events) = self.0.update_if_active(tree) {
            events.raise();
        }
    }
}
