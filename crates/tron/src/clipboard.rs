//! System clipboard and primary selection, one backend per platform.

#[cfg(not(target_os = "macos"))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(not(target_os = "macos"))]
pub use linux::Clipboard;
#[cfg(target_os = "macos")]
pub use macos::Clipboard;
