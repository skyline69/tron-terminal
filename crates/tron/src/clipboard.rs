//! System clipboard and primary selection, one backend per platform.

#[cfg(not(target_os = "macos"))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(not(target_os = "macos"))]
pub use linux::Clipboard;
#[cfg(target_os = "macos")]
pub use macos::Clipboard;

use winit::data_transfer::TypeHint;
use winit::event_loop::{ActiveEventLoop, AsyncRequestSerial};

/// Read text through Winit's clipboard API, which uses the same data device as
/// drag-and-drop rather than a competing one.
///
/// The result arrives as `WindowEvent::DataTransferReceived`, identified by the
/// returned request serial. An empty or non-text selection errors.
pub fn request_text(event_loop: &dyn ActiveEventLoop) -> Result<AsyncRequestSerial, String> {
    let id = event_loop.clipboard().map_err(|error| error.to_string())?;
    let Some(id) = id else { return Err("selection is empty".into()) };
    event_loop.fetch_data_transfer(id, &TypeHint::Plaintext).map_err(|error| error.to_string())
}
