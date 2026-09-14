//! Installs the bundled `xterm-tron` terminfo entry.
//!
//! The source is compiled with `tic` into the data directory once per version
//! of the source. The child process finds it through `TERMINFO_DIRS`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SOURCE: &str = include_str!("../terminfo/xterm-tron.terminfo");

/// Returns the directory holding the compiled entry, compiling it when needed.
pub fn install(data_dir: &Path) -> Option<PathBuf> {
    let dir = data_dir.join("terminfo");
    let stamp = dir.join(".source-hash");
    let fingerprint = fingerprint(SOURCE);
    let compiled = || ["x", "78"].iter().any(|sub| dir.join(sub).join("xterm-tron").exists());
    if std::fs::read_to_string(&stamp).ok().as_deref() == Some(fingerprint.as_str()) && compiled() {
        return Some(dir);
    }
    if let Err(error) = std::fs::create_dir_all(&dir) {
        log::warn!("cannot create {}: {error}", dir.display());
        return None;
    }
    let source = dir.join("xterm-tron.terminfo");
    std::fs::write(&source, SOURCE).ok()?;
    let output = Command::new("tic")
        .arg("-x")
        .arg("-o")
        .arg(&dir)
        .arg(&source)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    match output {
        Ok(output) if output.status.success() && compiled() => {
            let _ = std::fs::write(&stamp, fingerprint);
            Some(dir)
        }
        Ok(output) => {
            log::warn!("tic failed: {}", String::from_utf8_lossy(&output.stderr).trim());
            None
        }
        Err(error) => {
            log::warn!("cannot run tic: {error}");
            None
        }
    }
}

/// `TERMINFO_DIRS` value with `dir` first. The trailing empty entry keeps the
/// system default directory in the search path.
pub fn search_path(dir: &Path) -> OsString {
    let mut value = OsString::from(dir);
    value.push(":");
    if let Some(existing) = std::env::var_os("TERMINFO_DIRS") {
        value.push(existing);
    }
    value
}

fn fingerprint(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}
