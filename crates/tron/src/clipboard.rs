//! System clipboard and primary selection: smithay-clipboard on Wayland, arboard on X11.

use std::cell::RefCell;

use arboard::{GetExtLinux, LinuxClipboardKind, SetExtLinux};
use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use winit::window::Window;

enum Backend {
    Wayland(smithay_clipboard::Clipboard),
    /// arboard serves the selection from its own X connection while alive.
    X11(RefCell<arboard::Clipboard>),
}

pub struct Clipboard {
    inner: Option<Backend>,
}

impl Clipboard {
    pub fn new(window: &dyn Window) -> Self {
        let raw = window.display_handle().ok().map(|handle| handle.as_raw());
        let inner = match raw {
            // SAFETY: the Wayland display outlives the clipboard, because
            // `Session` declares the clipboard before the window and drops it first.
            Some(RawDisplayHandle::Wayland(handle)) => {
                Some(Backend::Wayland(unsafe { smithay_clipboard::Clipboard::new(handle.display.as_ptr()) }))
            }
            Some(RawDisplayHandle::Xlib(_) | RawDisplayHandle::Xcb(_)) => match arboard::Clipboard::new() {
                Ok(clipboard) => Some(Backend::X11(RefCell::new(clipboard))),
                Err(error) => {
                    log::warn!("X11 clipboard unavailable: {error}");
                    None
                }
            },
            _ => {
                log::warn!("clipboard support needs Wayland or X11");
                None
            }
        };
        Self { inner }
    }

    pub fn load(&self, primary: bool) -> Option<String> {
        let result = match self.inner.as_ref()? {
            Backend::Wayland(clipboard) => {
                if primary { clipboard.load_primary() } else { clipboard.load() }.map_err(|error| error.to_string())
            }
            Backend::X11(clipboard) => {
                clipboard.borrow_mut().get().clipboard(kind(primary)).text().map_err(|error| error.to_string())
            }
        };
        result.map_err(|error| log::debug!("clipboard read failed: {error}")).ok()
    }

    pub fn store(&self, primary: bool, text: String) {
        match &self.inner {
            Some(Backend::Wayland(clipboard)) => {
                if primary {
                    clipboard.store_primary(text);
                } else {
                    clipboard.store(text);
                }
            }
            Some(Backend::X11(clipboard)) => {
                if let Err(error) = clipboard.borrow_mut().set().clipboard(kind(primary)).text(text) {
                    log::debug!("clipboard write failed: {error}");
                }
            }
            None => {}
        }
    }
}

fn kind(primary: bool) -> LinuxClipboardKind {
    if primary { LinuxClipboardKind::Primary } else { LinuxClipboardKind::Clipboard }
}
