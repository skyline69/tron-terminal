//! Commands tron puts on the shell's `PATH`.
//!
//! The data directory's `bin` holds a `tron` launcher, so `tron settings` and
//! other commands work in every tron window, however tron was installed: from a
//! disk image, as an AppImage, as a Flatpak or as a plain binary. It also holds
//! the `ssh` wrapper from [`crate::terminfo`].

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Flatpak id used when the sandbox does not name one.
const FLATPAK_ID: &str = "dev.tron.Terminal";

/// Writes the launchers and returns their directory. The `ssh` and `tmux`
/// wrappers are only kept while tron uses its own terminfo entry.
pub fn install(data_dir: &Path, wrappers: bool) -> Option<PathBuf> {
    let dir = data_dir.join("bin");
    if !write_executable(&dir.join("tron"), &tron_script()?) {
        return None;
    }
    if wrappers {
        crate::terminfo::install_wrappers(data_dir);
    } else {
        for name in crate::terminfo::WRAPPED {
            let _ = std::fs::remove_file(dir.join(name));
        }
    }
    Some(dir)
}

/// A script that runs the tron this process belongs to. tron rewrites it on
/// every start, so it follows the installation that ran last.
fn tron_script() -> Option<String> {
    let target = if tron_pty::in_flatpak() {
        // The launcher runs on the host, outside the sandbox.
        let id = std::env::var("FLATPAK_ID").unwrap_or_else(|_| FLATPAK_ID.to_owned());
        format!("flatpak run --command=tron {}", quote(&id))
    } else if let Some(appimage) = std::env::var_os("APPIMAGE") {
        // An AppImage's binary lives in a mount that disappears when tron exits.
        quote(&appimage.to_string_lossy())
    } else {
        quote(&std::env::current_exe().ok()?.to_string_lossy())
    };
    Some(format!(
        "#!/bin/sh\n# Runs the tron that opened this window. tron writes this file when it starts.\nexec {target} \"$@\"\n"
    ))
}

/// Quotes `text` for a POSIX shell.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Writes an executable script unless it already has `content`. Returns whether
/// the file holds `content` afterwards.
pub fn write_executable(path: &Path, content: &str) -> bool {
    if std::fs::read_to_string(path).ok().as_deref() == Some(content) {
        return true;
    }
    let Some(dir) = path.parent() else { return false };
    if let Err(error) = std::fs::create_dir_all(dir) {
        log::warn!("cannot create {}: {error}", dir.display());
        return false;
    }
    // Written beside the target and renamed, so a running command never sees a partial file.
    let name = path.file_name().map_or_else(Default::default, |name| name.to_string_lossy());
    let temp = dir.join(format!(".{name}.{}", std::process::id()));
    let written = std::fs::write(&temp, content)
        .and_then(|()| std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755)))
        .and_then(|()| std::fs::rename(&temp, path));
    if let Err(error) = written {
        log::warn!("cannot write {}: {error}", path.display());
        let _ = std::fs::remove_file(&temp);
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_paths_for_the_shell() {
        assert_eq!(quote("/Applications/tron.app/Contents/MacOS/tron"), "'/Applications/tron.app/Contents/MacOS/tron'");
        assert_eq!(quote("/home/o'neil/tron"), r"'/home/o'\''neil/tron'");
    }

    #[test]
    fn launcher_runs_this_tron_with_its_arguments() {
        let dir = std::env::temp_dir().join(format!("tron-launcher-test-{}", std::process::id()));
        let bin = install(&dir, false).expect("launcher written");
        let script = std::fs::read_to_string(bin.join("tron")).unwrap();
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.trim_end().ends_with("\"$@\""), "{script}");
        assert!(!bin.join("ssh").exists());
        let mode = std::fs::metadata(bin.join("tron")).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "executable");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
