//! Locating the system programs `tron` runs, without trusting `PATH`.
//!
//! A terminal inherits the environment of whatever launched it, and terminals
//! are often started from a project directory. Anything that puts `.` or a
//! writable directory ahead of `/usr/bin` in `PATH` would otherwise be able to
//! supply `sudo`, `tic` or `codesign`, the first of which runs as root.

use std::path::{Path, PathBuf};

/// Directories that hold system programs, in the order they are preferred.
/// `PATH` is deliberately absent.
///
/// `/usr/local/bin` and `/opt/local/bin` are on the list because Homebrew and
/// MacPorts put `rsvg-convert` and `magick` there; they are only accepted while
/// neither they nor the program itself are writable by anyone but their owner.
const SYSTEM_DIRS: [&str; 9] = [
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
    "/usr/local/bin",
    "/opt/local/bin",
    "/run/wrappers/bin",
    "/run/current-system/sw/bin",
    "/opt/homebrew/bin",
];

/// Absolute path of `name` in a system directory, if it is a file that only its
/// owner may write. `None` when the program is not installed there.
#[cfg(unix)]
pub fn system_tool(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let closed = |path: &Path| path.metadata().is_ok_and(|meta| meta.permissions().mode() & 0o022 == 0);
    SYSTEM_DIRS.iter().map(|dir| Path::new(dir).join(name)).find(|path| {
        path.metadata().is_ok_and(|metadata| metadata.is_file()) && closed(path) && path.parent().is_some_and(closed)
    })
}

#[cfg(not(unix))]
pub fn system_tool(_name: &str) -> Option<PathBuf> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn finds_a_system_program_and_not_a_relative_one() {
        let sh = system_tool("sh").or_else(|| system_tool("ls")).expect("a system directory has sh or ls");
        assert!(sh.is_absolute(), "{}", sh.display());
        assert!(system_tool("definitely-not-a-program-xyz").is_none());
    }
}
