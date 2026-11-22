//! Locating the system programs `tron` runs, without trusting `PATH`.
//!
//! A terminal inherits the environment of whatever launched it, and terminals
//! are often started from a project directory. Anything that puts `.` or a
//! writable directory ahead of `/usr/bin` in `PATH` would otherwise be able to
//! supply `sudo`, `tic` or `codesign`, the first of which runs as root.

use std::path::{Path, PathBuf};

/// Directories that hold system programs, in the order they are preferred.
/// `PATH` is deliberately absent.
const SYSTEM_DIRS: [&str; 6] = ["/usr/bin", "/bin", "/usr/sbin", "/sbin", "/run/wrappers/bin", "/opt/homebrew/bin"];

/// Absolute path of `name` in a system directory, if it is a file that only its
/// owner may write. `None` when the program is not installed there.
#[cfg(unix)]
pub fn system_tool(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    SYSTEM_DIRS.iter().map(|dir| Path::new(dir).join(name)).find(|path| {
        path.metadata().is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o022 == 0)
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
