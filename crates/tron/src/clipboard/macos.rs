//! The general pasteboard through arboard. macOS has no primary selection, so
//! tron keeps its own for middle click paste.

use std::cell::RefCell;

use winit::window::Window;

pub struct Clipboard {
    pasteboard: Option<RefCell<arboard::Clipboard>>,
    selection: RefCell<String>,
}

impl Clipboard {
    pub fn new(_window: &dyn Window) -> Self {
        let pasteboard = arboard::Clipboard::new()
            .map_err(|error| log::warn!("pasteboard unavailable: {error}"))
            .ok()
            .map(RefCell::new);
        Self { pasteboard, selection: RefCell::default() }
    }

    pub fn load(&self, primary: bool) -> Option<String> {
        if primary {
            return Some(self.selection.borrow().clone());
        }
        let result = self.pasteboard.as_ref()?.borrow_mut().get_text();
        result.map_err(|error| log::debug!("clipboard read failed: {error}")).ok()
    }

    pub fn store(&self, primary: bool, text: String) {
        if primary {
            *self.selection.borrow_mut() = text;
        } else if let Some(pasteboard) = &self.pasteboard
            && let Err(error) = pasteboard.borrow_mut().set_text(text)
        {
            log::debug!("clipboard write failed: {error}");
        }
    }
}
