//! System clipboard and primary selection (Wayland).

use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use winit::window::Window;

pub struct Clipboard {
    inner: Option<smithay_clipboard::Clipboard>,
}

impl Clipboard {
    pub fn new(window: &dyn Window) -> Self {
        let raw = window.display_handle().ok().map(|handle| handle.as_raw());
        let inner = match raw {
            // SAFETY: the Wayland display outlives the clipboard, because
            // `Session` declares the clipboard before the window and drops it first.
            Some(RawDisplayHandle::Wayland(handle)) => Some(unsafe { smithay_clipboard::Clipboard::new(handle.display.as_ptr()) }),
            _ => {
                log::warn!("clipboard support needs Wayland");
                None
            }
        };
        Self { inner }
    }

    pub fn load(&self, primary: bool) -> Option<String> {
        let clipboard = self.inner.as_ref()?;
        let result = if primary { clipboard.load_primary() } else { clipboard.load() };
        result.map_err(|error| log::debug!("clipboard read failed: {error}")).ok()
    }

    pub fn store(&self, primary: bool, text: String) {
        if let Some(clipboard) = &self.inner {
            if primary {
                clipboard.store_primary(text);
            } else {
                clipboard.store(text);
            }
        }
    }
}
