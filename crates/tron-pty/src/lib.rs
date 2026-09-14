//! Pseudo terminal creation and child process management.
//!
//! Linux first. Uses `rustix` for the pty syscalls, no libc bindings.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};

use rustix::fs::{Mode, OFlags};
use rustix::pty::OpenptFlags;
use rustix::termios::{InputModes, OptionalActions, Winsize};

/// Grid size in cells plus cell size in pixels.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct WindowSize {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl WindowSize {
    fn winsize(self) -> Winsize {
        Winsize {
            ws_row: self.rows,
            ws_col: self.cols,
            ws_xpixel: self.cols.saturating_mul(self.cell_width),
            ws_ypixel: self.rows.saturating_mul(self.cell_height),
        }
    }
}

/// What to run inside the terminal.
#[derive(Clone, Debug, Default)]
pub struct SpawnOptions {
    /// Program to run. Defaults to `$SHELL`, then `/bin/sh`.
    pub program: Option<String>,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Value of `TERM`. Defaults to `xterm-256color`.
    pub term: String,
    /// Extra environment variables, applied after the defaults.
    pub env: Vec<(OsString, OsString)>,
}

#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("failed to open pseudo terminal: {0}")]
    Open(#[source] io::Error),
    #[error("failed to spawn `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: io::Error,
    },
}

/// A running child process attached to a pseudo terminal.
pub struct Pty {
    master: OwnedFd,
    child: Child,
}

impl Pty {
    pub fn spawn(options: &SpawnOptions, size: WindowSize) -> Result<Self, PtyError> {
        let (master, slave) = open_pair(size).map_err(PtyError::Open)?;

        let program = options
            .program
            .clone()
            .or_else(|| std::env::var("SHELL").ok().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| "/bin/sh".to_owned());

        let stdio = |fd: &OwnedFd| fd.try_clone().map(Stdio::from).map_err(PtyError::Open);
        let mut command = Command::new(&program);
        command
            .args(&options.args)
            .stdin(stdio(&slave)?)
            .stdout(stdio(&slave)?)
            .stderr(Stdio::from(slave))
            .env("TERM", if options.term.is_empty() { "xterm-256color" } else { &options.term })
            .env("COLORTERM", "truecolor")
            .env("TERM_PROGRAM", "tron")
            .env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"))
            .env_remove("DESKTOP_STARTUP_ID")
            .env_remove("XDG_ACTIVATION_TOKEN");
        for (key, value) in &options.env {
            command.env(key, value);
        }
        if let Some(cwd) = &options.cwd {
            command.current_dir(cwd);
        }

        // SAFETY: the closure only performs async-signal-safe syscalls.
        unsafe {
            command.pre_exec(|| {
                rustix::process::setsid()?;
                // stdin is the pty slave at this point.
                rustix::process::ioctl_tiocsctty(BorrowedFd::borrow_raw(0))?;
                Ok(())
            });
        }

        let child = command
            .spawn()
            .map_err(|source| PtyError::Spawn { program, source })?;
        Ok(Self { master, child })
    }

    /// A handle for reading child output. Blocking.
    pub fn reader(&self) -> io::Result<File> {
        Ok(File::from(self.master.try_clone()?))
    }

    /// A handle for writing input to the child. Blocking.
    pub fn writer(&self) -> io::Result<File> {
        Ok(File::from(self.master.try_clone()?))
    }

    pub fn resize(&self, size: WindowSize) -> io::Result<()> {
        rustix::termios::tcsetwinsize(&self.master, size.winsize())?;
        Ok(())
    }

    pub fn child_id(&self) -> u32 {
        self.child.id()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Closing the master sends SIGHUP to the session. Reap if it already exited.
        let _ = self.child.try_wait();
    }
}

fn open_pair(size: WindowSize) -> io::Result<(OwnedFd, OwnedFd)> {
    let master = rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
    rustix::pty::grantpt(&master)?;
    rustix::pty::unlockpt(&master)?;
    let name = rustix::pty::ptsname(&master, Vec::new())?;
    let slave = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    rustix::termios::tcsetwinsize(&master, size.winsize())?;
    if let Ok(mut termios) = rustix::termios::tcgetattr(&slave) {
        termios.input_modes.insert(InputModes::IUTF8);
        rustix::termios::tcsetattr(&slave, OptionalActions::Now, &termios)?;
    }
    Ok((master, slave))
}

/// Linux reports `EIO` on the master once the child side is gone.
pub fn is_closed_error(error: &io::Error) -> bool {
    error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn runs_command_and_reads_output() {
        let options = SpawnOptions {
            program: Some("/bin/sh".into()),
            args: vec!["-c".into(), "printf 'cols=%s' \"$(tput cols 2>/dev/null || stty size | cut -d' ' -f2)\"".into()],
            ..Default::default()
        };
        let size = WindowSize { cols: 97, rows: 31, cell_width: 8, cell_height: 16 };
        let pty = Pty::spawn(&options, size).expect("spawn");
        let mut reader = pty.reader().unwrap();
        let mut output = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => output.extend_from_slice(&buf[..n]),
                Err(e) if is_closed_error(&e) => break,
                Err(e) => panic!("{e}"),
            }
        }
        assert_eq!(String::from_utf8_lossy(&output), "cols=97");
    }
}
