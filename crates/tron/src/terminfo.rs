//! Installs the bundled `xterm-tron` terminfo entry.
//!
//! The source is compiled with `tic` into the data directory once per version
//! of the source. The child process finds it through `TERMINFO_DIRS`.
//!
//! Remote hosts do not have the entry. An `ssh` wrapper placed first in `PATH`
//! copies it to each host before the first interactive session, and remembers
//! the hosts that have it. Hosts that cannot take it get `TERM=xterm-256color`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SOURCE: &str = include_str!("../terminfo/xterm-tron.terminfo");
const SSH_WRAPPER: &str = include_str!("../terminfo/ssh");

/// Writes the `ssh` wrapper and returns its directory.
pub fn install_ssh_wrapper(data_dir: &Path) -> Option<PathBuf> {
    let dir = data_dir.join("bin");
    crate::launcher::write_executable(&dir.join("ssh"), SSH_WRAPPER).then_some(dir)
}

/// `PATH` value with `dir` first, unless it is already in the path.
pub fn path_with(dir: &Path) -> OsString {
    let existing = std::env::var_os("PATH").unwrap_or_default();
    if std::env::split_paths(&existing).any(|entry| entry == dir) {
        return existing;
    }
    let mut value = OsString::from(dir);
    if !existing.is_empty() {
        value.push(":");
        value.push(existing);
    }
    value
}

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

#[cfg(test)]
mod tests {
    use super::*;

    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};
    use tron_pty::{Pty, SpawnOptions, WindowSize};

    /// Stands in for OpenSSH: logs each call, answers `-G`, and runs the remote
    /// command locally against an empty home directory.
    const FAKE_SSH: &str = r#"#!/bin/sh
printf 'TERM=%s %s\n' "$TERM" "$*" >>"$FAKE_LOG"
if [ "$1" = -G ]; then
    printf 'user u\nhostname h\nport 22\n'
    exit 0
fi
for last; do :; done
case $last in
    "sh -c "*)
        unset TERMINFO TERMINFO_DIRS
        HOME=$FAKE_HOME
        export HOME
        eval "$last" ;;
esac
"#;

    struct Fixture {
        root: PathBuf,
        wrapper: PathBuf,
        env: Vec<(OsString, OsString)>,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("tron-ssh-wrapper-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let terminfo = install(&root).expect("tic compiles the entry");
            let wrapper_dir = install_ssh_wrapper(&root).unwrap();
            assert_eq!(install_ssh_wrapper(&root), Some(wrapper_dir.clone()));

            let fake_dir = root.join("fake");
            std::fs::create_dir_all(root.join("home")).unwrap();
            std::fs::create_dir_all(&fake_dir).unwrap();
            let fake = fake_dir.join("ssh");
            std::fs::write(&fake, FAKE_SSH).unwrap();
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

            let mut path = OsString::from(&wrapper_dir);
            path.push(":");
            path.push(&fake_dir);
            path.push(":");
            path.push(std::env::var_os("PATH").unwrap_or_default());
            let env = vec![
                ("PATH".into(), path),
                ("TERMINFO_DIRS".into(), search_path(&terminfo)),
                ("XDG_RUNTIME_DIR".into(), root.clone().into()),
                ("FAKE_LOG".into(), root.join("log").into()),
                ("FAKE_HOME".into(), root.join("home").into()),
            ];
            Self { wrapper: wrapper_dir.join("ssh"), root, env }
        }

        /// Runs the wrapper and returns the ssh calls it made.
        fn run(&self, args: &[&str], tty: bool) -> Vec<String> {
            let status = if tty {
                let options = SpawnOptions {
                    program: Some(self.wrapper.to_string_lossy().into_owned()),
                    args: args.iter().map(|&a| a.to_owned()).collect(),
                    term: "xterm-tron".into(),
                    env: self.env.clone(),
                    ..SpawnOptions::default()
                };
                let size = WindowSize { cols: 80, rows: 24, cell_width: 8, cell_height: 16 };
                let mut pty = Pty::spawn(&options, size).unwrap();
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    if let Some(status) = pty.try_wait().unwrap() {
                        break status;
                    }
                    assert!(Instant::now() < deadline, "wrapper did not exit");
                    std::thread::sleep(Duration::from_millis(10));
                }
            } else {
                Command::new(&self.wrapper)
                    .args(args)
                    .env("TERM", "xterm-tron")
                    .envs(self.env.iter().map(|(k, v)| (k, v)))
                    .stdin(Stdio::null())
                    .status()
                    .unwrap()
            };
            assert!(status.success());
            let log = self.root.join("log");
            let calls = std::fs::read_to_string(&log).unwrap_or_default().lines().map(str::to_owned).collect();
            let _ = std::fs::remove_file(&log);
            calls
        }
    }

    #[test]
    fn ssh_wrapper_installs_terminfo_once_per_host() {
        let fixture = Fixture::new();
        let cache = fixture.root.join("ssh-terminfo-hosts");

        let calls = fixture.run(&["-p", "2222", "user@host"], true);
        assert_eq!(calls.len(), 3, "{calls:?}");
        assert!(calls[0].starts_with("TERM=xterm-tron -G "), "{calls:?}");
        assert!(calls[1].contains("-o ControlMaster=yes"), "{calls:?}");
        assert!(calls[2].starts_with("TERM=xterm-tron -o ControlPath="), "{calls:?}");
        assert!(calls[2].ends_with(" -p 2222 user@host"), "{calls:?}");
        let home = fixture.root.join("home/.terminfo");
        assert!(home.join("x/xterm-tron").exists() || home.join("78/xterm-tron").exists());
        assert!(std::fs::read_to_string(&cache).unwrap().ends_with(" u@h:22\n"));

        // Known host: no install connection.
        let calls = fixture.run(&["-p", "2222", "user@host"], true);
        assert_eq!(calls.last().unwrap(), "TERM=xterm-tron -p 2222 user@host");
        assert_eq!(calls.len(), 2, "{calls:?}");
        let calls = fixture.run(&["-t", "user@host", "htop"], true);
        assert_eq!(calls.last().unwrap(), "TERM=xterm-tron -t user@host htop");

        // No terminal, like scp or git.
        let calls = fixture.run(&["user@host"], false);
        assert_eq!(calls, ["TERM=xterm-256color user@host"]);

        // Unknown host with a remote command: no install.
        std::fs::remove_file(&cache).unwrap();
        let calls = fixture.run(&["user@host", "-p", "22", "ls", "-l"], true);
        assert_eq!(calls.last().unwrap(), "TERM=xterm-256color user@host -p 22 ls -l");
        assert_eq!(calls.len(), 2, "{calls:?}");

        std::fs::remove_dir_all(&fixture.root).unwrap();
    }
}
