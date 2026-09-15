//! AT-SPI adapter.

use accesskit::TreeUpdate;
use winit::window::Window;

use super::{Activation, Deactivation, IgnoreActions};

pub struct Adapter(accesskit_unix::Adapter);

impl Adapter {
    /// Registers with the accessibility bus in the background. Never blocks.
    pub fn new(activation: Activation, deactivation: Deactivation, _window: &dyn Window) -> Option<Self> {
        Some(Self(accesskit_unix::Adapter::new(activation, IgnoreActions, deactivation)))
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.0.update_window_focus_state(focused);
    }

    pub fn update_if_active(&mut self, tree: impl FnOnce() -> TreeUpdate) {
        self.0.update_if_active(tree);
    }
}
