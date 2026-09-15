//! Pseudo terminal creation and child process management.
//!
//! Linux and macOS. Uses `rustix` for the pty syscalls, no libc bindings.

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

/// Variables set by terminal multiplexers and other terminals, removed from the
/// child's environment.
const INHERITED_TERMINAL_ENV: [&str; 34] = [
    "TMUX",
    "TMUX_PANE",
    "STY",
    "ZELLIJ",
    "ZELLIJ_SESSION_NAME",
    "ZELLIJ_PANE_ID",
    "ZELLIJ_VERSION",
    "KITTY_PID",
    "KITTY_PUBLIC_KEY",
    "KITTY_INSTALLATION_DIR",
    "KITTY_LISTEN_ON",
    "WEZTERM_EXECUTABLE",
    "WEZTERM_EXECUTABLE_DIR",
    "WEZTERM_PANE",
    "WEZTERM_UNIX_SOCKET",
    "WEZTERM_VERSION",
    "GHOSTTY_RESOURCES_DIR",
    "GHOSTTY_BIN_DIR",
    "GHOSTTY_SHELL_FEATURES",
    "ITERM_SESSION_ID",
    "ITERM_PROFILE",
    "TERM_SESSION_ID",
    "VTE_VERSION",
    "KONSOLE_VERSION",
    "KONSOLE_DBUS_SESSION",
    "KONSOLE_DBUS_SERVICE",
    "KONSOLE_DBUS_WINDOW",
    "ALACRITTY_SOCKET",
    "ALACRITTY_LOG",
    "ALACRITTY_WINDOW_ID",
    "WT_SESSION",
    "GNOME_TERMINAL_SCREEN",
    "GNOME_TERMINAL_SERVICE",
    "WINDOWID",
];

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
    /// Variables of this process the child must not see.
    pub remove_env: Vec<OsString>,
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
            .env_remove("DESKTOP_STARTUP_ID")
            .env_remove("XDG_ACTIVATION_TOKEN");
        // tron runs outside whatever tron itself was started from: programs must
        // not think they are inside tmux (Codex turns pets off there) or another terminal.
        for key in INHERITED_TERMINAL_ENV {
            command.env_remove(key);
        }
        for (key, value) in terminal_env(options) {
            command.env(key, value);
        }
        for key in &options.remove_env {
            command.env_remove(key);
        }
        for (key, value) in &options.env {
            command.env(key, value);
        }
        if let Some(dir) = start_directory(options) {
            command.current_dir(dir);
        }
        #[cfg(target_os = "macos")]
        macos_session(&mut command, options, &program);

        // In a Flatpak the child is flatpak-spawn (see `host_command`), and the shell on
        // the host takes the pty as its controlling terminal instead.
        let controlling_terminal = !in_flatpak();
        // SAFETY: the closure only performs async-signal-safe syscalls.
        unsafe {
            command.pre_exec(move || {
                rustix::process::setsid()?;
                if controlling_terminal {
                    // stdin is the pty slave at this point.
                    rustix::process::ioctl_tiocsctty(BorrowedFd::borrow_raw(0))?;
                }
                Ok(())
            });
        }

        let child = command.spawn().map_err(|source| PtyError::Spawn { program, source })?;
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

    /// The child's working directory, read from the process. In a Flatpak the
    /// child is flatpak-spawn, whose directory says nothing about the host shell.
    pub fn cwd(&self) -> Option<PathBuf> {
        if in_flatpak() {
            return None;
        }
        process_cwd(self.child.id())
    }

    /// The terminal device the child uses, such as `/dev/pts/3`.
    pub fn tty_path(&self) -> Option<PathBuf> {
        use std::os::unix::ffi::OsStringExt;
        let name = rustix::pty::ptsname(&self.master, Vec::new()).ok()?;
        Some(PathBuf::from(OsString::from_vec(name.into_bytes())))
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

/// Variables every program in tron sees, before [`SpawnOptions::env`].
fn terminal_env(options: &SpawnOptions) -> Vec<(&'static str, String)> {
    let mut env = vec![
        ("TERM", if options.term.is_empty() { "xterm-256color".to_owned() } else { options.term.clone() }),
        ("COLORTERM", "truecolor".to_owned()),
        ("TERM_PROGRAM", "tron".to_owned()),
        ("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION").to_owned()),
        // Programs that look for kitty before using the kitty graphics protocol,
        // like Codex pets and image viewers, find it. tron speaks the protocol.
        ("KITTY_WINDOW_ID", "1".to_owned()),
    ];
    if let Some(locale) = default_locale() {
        env.push(("LANG", locale));
    }
    env
}

/// `LANG` for programs in tron when tron itself has no locale, as other terminals
/// do. macOS apps opened from Finder or the Dock get none, and without a UTF-8
/// locale programs such as tmux replace every character outside ASCII with `_`.
fn default_locale() -> Option<String> {
    static LOCALE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    LOCALE
        .get_or_init(|| {
            let set = |name| std::env::var_os(name).is_some_and(|value| !value.is_empty());
            if set("LC_ALL") || set("LC_CTYPE") || set("LANG") {
                return None;
            }
            #[cfg(target_os = "macos")]
            let locale = utf8_locale(apple_locale().as_deref(), |name| {
                std::path::Path::new("/usr/share/locale").join(name).exists()
            });
            #[cfg(not(target_os = "macos"))]
            let locale = "C.UTF-8".to_owned();
            Some(locale)
        })
        .clone()
}

/// The UTF-8 locale for the macOS locale `system`, such as `de_DE.UTF-8` for
/// `de_DE@rg=atzzzz`, when `exists` finds it installed; `en_US.UTF-8` otherwise.
#[cfg(any(target_os = "macos", test))]
fn utf8_locale(system: Option<&str>, exists: impl Fn(&str) -> bool) -> String {
    system
        .map(|id| id.split('@').next().unwrap_or(id).trim())
        .filter(|id| !id.is_empty())
        .map(|id| format!("{id}.UTF-8"))
        .filter(|name| exists(name))
        .unwrap_or_else(|| "en_US.UTF-8".to_owned())
}

/// The locale chosen in System Settings, such as `en_US` or `de_DE@rg=atzzzz`.
#[cfg(target_os = "macos")]
fn apple_locale() -> Option<String> {
    let output = Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLocale"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The foreground process group of the controlling terminal of process `pid`,
/// such as a shell: the job it runs, or the shell itself. Asking the terminal
/// device instead only works from its own controlling process.
#[cfg(target_os = "linux")]
pub fn terminal_foreground(pid: u32) -> Option<u32> {
    stat_terminal_foreground(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// `tpgid` from `/proc/<pid>/stat`: the sixth field after the command name, which
/// is in parentheses and may itself contain spaces and parentheses.
#[cfg(any(target_os = "linux", test))]
fn stat_terminal_foreground(stat: &str) -> Option<u32> {
    let fields = &stat[stat.rfind(')')? + 1..];
    fields
        .split_whitespace()
        .nth(5)?
        .parse::<i64>()
        .ok()
        .and_then(|group| u32::try_from(group).ok())
        .filter(|&group| group > 0)
}

/// The foreground process group of the controlling terminal of process `pid`,
/// from `proc_bsdinfo.e_tpgid`.
#[cfg(target_os = "macos")]
pub fn terminal_foreground(pid: u32) -> Option<u32> {
    use std::ffi::{c_int, c_void};

    unsafe extern "C" {
        /// libproc, part of libSystem.
        fn proc_pidinfo(pid: c_int, flavor: c_int, arg: u64, buffer: *mut c_void, size: c_int) -> c_int;
    }
    const PROC_PIDTBSDINFO: c_int = 3;
    /// `struct proc_bsdinfo`: twelve 32-bit fields, 16 and 32 byte names, five more
    /// 32-bit fields ending with `e_tpgid`, then the nice value and two 64-bit times.
    const SIZE: usize = 136;
    const E_TPGID: usize = 112;
    let mut info = [0u8; SIZE];
    // SAFETY: the buffer is writable and as large as the size passed.
    let written = unsafe {
        proc_pidinfo(c_int::try_from(pid).ok()?, PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast(), SIZE as c_int)
    };
    if written != SIZE as c_int {
        return None;
    }
    let group = u32::from_ne_bytes(info[E_TPGID..E_TPGID + 4].try_into().ok()?);
    (group > 0).then_some(group)
}

/// A process's command line.
#[cfg(target_os = "linux")]
pub fn process_args(pid: u32) -> Option<Vec<String>> {
    let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    Some(
        bytes
            .split(|&b| b == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect(),
    )
}

/// A variable from a process's environment.
#[cfg(target_os = "linux")]
pub fn process_env_var(pid: u32, name: &str) -> Option<String> {
    let bytes = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    env_var(bytes.split(|&b| b == 0).map(|entry| String::from_utf8_lossy(entry).into_owned()), name)
}

/// A process's command line, from `KERN_PROCARGS2`.
#[cfg(target_os = "macos")]
pub fn process_args(pid: u32) -> Option<Vec<String>> {
    procargs2(pid).and_then(|buffer| parse_procargs2(&buffer)).map(|(args, _)| args)
}

/// A variable from a process's environment, from `KERN_PROCARGS2`.
#[cfg(target_os = "macos")]
pub fn process_env_var(pid: u32, name: &str) -> Option<String> {
    let (_, env) = parse_procargs2(&procargs2(pid)?)?;
    env_var(env.into_iter(), name)
}

/// The value of `name` among `NAME=value` entries.
fn env_var(entries: impl Iterator<Item = String>, name: &str) -> Option<String> {
    entries.into_iter().find_map(|entry| entry.strip_prefix(name)?.strip_prefix('=').map(str::to_owned))
}

/// The raw `KERN_PROCARGS2` data of a process.
#[cfg(target_os = "macos")]
fn procargs2(pid: u32) -> Option<Vec<u8>> {
    use std::ffi::{c_int, c_uint, c_void};

    unsafe extern "C" {
        fn sysctl(
            name: *const c_int,
            namelen: c_uint,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *const c_void,
            newlen: usize,
        ) -> c_int;
    }
    const CTL_KERN: c_int = 1;
    const KERN_PROCARGS2: c_int = 49;
    let mib = [CTL_KERN, KERN_PROCARGS2, c_int::try_from(pid).ok()?];
    let mut size = 0usize;
    // SAFETY: a null buffer asks for the size the arguments need.
    if unsafe { sysctl(mib.as_ptr(), 3, std::ptr::null_mut(), &mut size, std::ptr::null(), 0) } != 0 {
        return None;
    }
    let mut buffer = vec![0u8; size];
    // SAFETY: the buffer is writable and `size` bytes long.
    if unsafe { sysctl(mib.as_ptr(), 3, buffer.as_mut_ptr().cast(), &mut size, std::ptr::null(), 0) } != 0 {
        return None;
    }
    buffer.truncate(size);
    Some(buffer)
}

/// `KERN_PROCARGS2` data: the argument count, the executable path, padding, the
/// arguments, then the environment up to an empty string.
#[cfg(any(target_os = "macos", test))]
fn parse_procargs2(buffer: &[u8]) -> Option<(Vec<String>, Vec<String>)> {
    let argc = usize::try_from(i32::from_ne_bytes(buffer.get(..4)?.try_into().ok()?)).ok()?;
    let rest = &buffer[4..];
    let rest = &rest[rest.iter().position(|&b| b == 0)?..];
    let rest = &rest[rest.iter().position(|&b| b != 0)?..];
    let mut strings = rest.split(|&b| b == 0).map(|string| String::from_utf8_lossy(string).into_owned());
    let args = strings.by_ref().take(argc).collect();
    let env = strings.take_while(|entry| !entry.is_empty()).collect();
    Some((args, env))
}

/// The executable a process runs.
#[cfg(target_os = "linux")]
pub fn process_path(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

/// The executable a process runs, from `proc_pidpath`.
#[cfg(target_os = "macos")]
pub fn process_path(pid: u32) -> Option<PathBuf> {
    use std::ffi::{c_int, c_void};
    use std::os::unix::ffi::OsStrExt;

    unsafe extern "C" {
        /// libproc, part of libSystem.
        fn proc_pidpath(pid: c_int, buffer: *mut c_void, size: u32) -> c_int;
    }
    /// `PROC_PIDPATHINFO_MAXSIZE`, four times MAXPATHLEN.
    const SIZE: usize = 4 * 1024;
    let mut buffer = [0u8; SIZE];
    // SAFETY: the buffer is writable and as large as the size passed.
    let length = unsafe { proc_pidpath(c_int::try_from(pid).ok()?, buffer.as_mut_ptr().cast(), SIZE as u32) };
    let length = usize::try_from(length).ok().filter(|&length| length > 0)?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buffer[..length])))
}

#[cfg(target_os = "linux")]
fn process_cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(target_os = "macos")]
fn process_cwd(pid: u32) -> Option<PathBuf> {
    use std::ffi::{CStr, c_int, c_void};
    use std::os::unix::ffi::OsStrExt;

    unsafe extern "C" {
        /// libproc, part of libSystem.
        fn proc_pidinfo(pid: c_int, flavor: c_int, arg: u64, buffer: *mut c_void, size: c_int) -> c_int;
    }
    const PROC_PIDVNODEPATHINFO: c_int = 9;
    // `struct proc_vnodepathinfo`: the current and root directories, each a
    // 152 byte `vnode_info` followed by a MAXPATHLEN (1024) path.
    const SIZE: usize = 2 * (152 + 1024);
    const CDIR_PATH: std::ops::Range<usize> = 152..152 + 1024;

    let mut info = [0u8; SIZE];
    // SAFETY: the buffer is writable and as large as the size passed.
    let written =
        unsafe { proc_pidinfo(pid as c_int, PROC_PIDVNODEPATHINFO, 0, info.as_mut_ptr().cast(), SIZE as c_int) };
    if written != SIZE as c_int {
        return None;
    }
    let path = CStr::from_bytes_until_nul(&info[CDIR_PATH]).ok()?.to_bytes();
    (!path.is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(path)))
}

/// Whether tron runs inside a Flatpak sandbox.
pub fn in_flatpak() -> bool {
    static IN_FLATPAK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IN_FLATPAK.get_or_init(|| cfg!(target_os = "linux") && std::path::Path::new("/.flatpak-info").exists())
}

/// Runs on the host for [`host_command`]: puts the directories tron added to PATH in
/// front of the host's PATH and finds the login shell when no program is given.
/// flatpak-spawn makes the pty the controlling terminal, which job control needs;
/// `setsid --ctty` does it where that did not happen. Forcing it when a controlling
/// terminal exists fails, so the script checks for one with `/dev/tty` first.
const HOST_SCRIPT: &str = r#"if [ -n "$TRON_PATH_PREFIX" ]; then PATH="$TRON_PATH_PREFIX:$PATH"; export PATH; fi
unset TRON_PATH_PREFIX
if [ "$#" -eq 0 ]; then
    shell=$(getent passwd "$(id -un)" 2>/dev/null | cut -d: -f7)
    set -- "${shell:-/bin/sh}"
fi
if ! (: </dev/tty) 2>/dev/null && setsid --help 2>&1 | grep -q -- --ctty; then
    exec setsid --ctty --wait "$@"
fi
exec "$@""#;

/// The program and arguments that run `options` on the host from inside a Flatpak,
/// where the user's shell and tools are, through `flatpak-spawn --host`. tron's
/// environment is passed along; without a program, the user's login shell runs.
pub fn host_command(options: &SpawnOptions) -> (String, Vec<String>) {
    let mut args = vec!["--host".to_owned(), "--watch-bus".to_owned()];
    if let Some(cwd) = &options.cwd {
        args.push(format!("--directory={}", cwd.display()));
    }
    let base: Vec<(OsString, OsString)> =
        terminal_env(options).into_iter().map(|(key, value)| (OsString::from(key), OsString::from(value))).collect();
    for (key, value) in base.iter().chain(&options.env) {
        if key == "PATH" {
            // The sandbox PATH is not the host's; pass only the directory tron put in front.
            if let Some(first) = std::env::split_paths(value).next() {
                args.push(format!("--env=TRON_PATH_PREFIX={}", first.display()));
            }
            continue;
        }
        args.push(format!("--env={}={}", key.to_string_lossy(), value.to_string_lossy()));
    }
    args.extend(["sh", "-c", HOST_SCRIPT, "sh"].map(str::to_owned));
    if let Some(program) = &options.program {
        args.push(program.clone());
        args.extend(options.args.iter().cloned());
    }
    ("flatpak-spawn".to_owned(), args)
}

fn open_pair(size: WindowSize) -> io::Result<(OwnedFd, OwnedFd)> {
    let master = open_master()?;
    rustix::pty::grantpt(&master)?;
    rustix::pty::unlockpt(&master)?;
    let name = rustix::pty::ptsname(&master, Vec::new())?;
    let slave = rustix::fs::open(name.as_c_str(), OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC, Mode::empty())?;
    rustix::termios::tcsetwinsize(&master, size.winsize())?;
    if let Ok(mut termios) = rustix::termios::tcgetattr(&slave) {
        termios.input_modes.insert(InputModes::IUTF8);
        rustix::termios::tcsetattr(&slave, OptionalActions::Now, &termios)?;
    }
    Ok((master, slave))
}

#[cfg(not(target_os = "macos"))]
fn open_master() -> io::Result<OwnedFd> {
    Ok(rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?)
}

/// macOS `posix_openpt` takes no `O_CLOEXEC`, so the flag is set afterwards.
#[cfg(target_os = "macos")]
fn open_master() -> io::Result<OwnedFd> {
    let master = rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
    rustix::io::fcntl_setfd(&master, rustix::io::FdFlags::CLOEXEC)?;
    Ok(master)
}

/// Starts the default shell the way macOS terminals do: as a login shell, which
/// sets up `PATH` through `path_helper`.
#[cfg(target_os = "macos")]
fn macos_session(command: &mut Command, options: &SpawnOptions, program: &str) {
    if options.program.is_none()
        && options.args.is_empty()
        && let Some(name) = std::path::Path::new(program).file_name()
    {
        let mut arg0 = OsString::from("-");
        arg0.push(name);
        command.arg0(arg0);
    }
}

/// The directory the child starts in: `options.cwd` while it exists, else tron's
/// own directory (by returning `None`), except where a terminal should not start.
/// Then it is the home directory.
fn start_directory(options: &SpawnOptions) -> Option<PathBuf> {
    if let Some(dir) = options.cwd.as_ref().filter(|dir| dir.is_dir()) {
        return Some(dir.clone());
    }
    let current = std::env::current_dir().ok().filter(|dir| dir.is_dir());
    let bundle = std::env::current_exe().ok().and_then(|exe| app_bundle(&exe));
    if !starts_at_home(current.as_deref(), bundle.as_deref()) {
        return None;
    }
    std::env::var_os("HOME").map(PathBuf::from).filter(|home| home.is_dir())
}

/// Whether a terminal started from tron's directory `current` belongs in the home
/// directory instead: when that directory is gone, when it is `/` because Finder
/// or the Dock launched tron on macOS, or when it is inside tron's own app bundle,
/// which disappears with the disk image it was opened from.
fn starts_at_home(current: Option<&std::path::Path>, bundle: Option<&std::path::Path>) -> bool {
    let Some(current) = current else { return true };
    (cfg!(target_os = "macos") && current == std::path::Path::new("/"))
        || bundle.is_some_and(|bundle| current.starts_with(bundle))
}

/// The `.app` bundle containing the executable `exe`, if any.
fn app_bundle(exe: &std::path::Path) -> Option<PathBuf> {
    exe.ancestors().find(|dir| dir.extension().is_some_and(|extension| extension == "app")).map(PathBuf::from)
}

/// Linux reports `EIO` on the master once the child side is gone; macOS reports
/// end of file or `EIO`.
pub fn is_closed_error(error: &io::Error) -> bool {
    error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn host_command_passes_environment_and_program() {
        let options = SpawnOptions {
            program: Some("htop".into()),
            args: vec!["-d".into(), "5".into()],
            cwd: Some("/home/user/project".into()),
            term: "xterm-tron".into(),
            env: vec![("PATH".into(), "/data/tron/bin:/app/bin:/usr/bin".into()), ("EDITOR".into(), "nvim".into())],
            ..Default::default()
        };
        let (program, args) = host_command(&options);
        assert_eq!(program, "flatpak-spawn");
        assert_eq!(args[..3], ["--host", "--watch-bus", "--directory=/home/user/project"]);
        assert!(args.contains(&"--env=TERM=xterm-tron".to_owned()));
        assert!(args.contains(&"--env=EDITOR=nvim".to_owned()));
        assert!(args.contains(&"--env=TRON_PATH_PREFIX=/data/tron/bin".to_owned()));
        assert!(!args.iter().any(|arg| arg.starts_with("--env=PATH=")));
        assert_eq!(args[args.len() - 3..], ["htop", "-d", "5"]);

        let shell = host_command(&SpawnOptions::default()).1;
        assert_eq!(shell.last().map(String::as_str), Some("sh"), "no program: the script finds the login shell");
    }

    #[test]
    fn finds_the_foreground_process_and_its_arguments() {
        let options = SpawnOptions { program: Some("sleep".into()), args: vec!["5".into()], ..Default::default() };
        let pty = Pty::spawn(&options, WindowSize { cols: 80, rows: 24, ..Default::default() }).unwrap();
        assert!(pty.tty_path().unwrap().to_string_lossy().starts_with("/dev/"));
        // The child makes the terminal its controlling terminal after it starts.
        let mut group = None;
        for _ in 0..100 {
            group = terminal_foreground(pty.child_id());
            if group.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let group = group.unwrap();
        assert_eq!(group, pty.child_id());
        assert_eq!(process_args(group).unwrap(), ["sleep", "5"]);
        assert!(process_path(group).unwrap().ends_with("sleep"));
    }

    #[test]
    fn reads_the_terminal_foreground_group_from_proc_stat() {
        let stat = "4242 (my (odd) prog) S 4200 4242 4242 34817 4300 4194560 120 0 0 0";
        assert_eq!(stat_terminal_foreground(stat), Some(4300));
        assert_eq!(stat_terminal_foreground("1 (init) S 0 1 1 0 -1 4194560"), None, "no terminal");
    }

    #[test]
    fn picks_a_utf8_locale_for_the_system_locale() {
        let installed = |name: &str| ["de_DE.UTF-8", "en_US.UTF-8"].contains(&name);
        assert_eq!(utf8_locale(Some("de_DE@rg=atzzzz"), installed), "de_DE.UTF-8");
        assert_eq!(utf8_locale(Some("de_DE\n"), installed), "de_DE.UTF-8");
        assert_eq!(utf8_locale(Some("xx_YY"), installed), "en_US.UTF-8", "not installed");
        assert_eq!(utf8_locale(Some(""), installed), "en_US.UTF-8");
        assert_eq!(utf8_locale(None, installed), "en_US.UTF-8");
    }

    #[test]
    fn parses_kern_procargs2() {
        let mut data = 2i32.to_ne_bytes().to_vec();
        data.extend_from_slice(
            b"/usr/local/bin/node\0\0\0\0node\0/usr/local/bin/codex\0HOME=/Users/me\0TMUX_TMPDIR=/tmp/a=b\0\0junk\0",
        );
        let (args, env) = parse_procargs2(&data).unwrap();
        assert_eq!(args, ["node", "/usr/local/bin/codex"]);
        assert_eq!(env, ["HOME=/Users/me", "TMUX_TMPDIR=/tmp/a=b"]);
        assert_eq!(env_var(env.into_iter(), "TMUX_TMPDIR").as_deref(), Some("/tmp/a=b"));
        assert_eq!(parse_procargs2(&data[..3]), None);
    }

    #[test]
    fn terminals_start_at_home_outside_usable_directories() {
        use std::path::Path;
        let bundle = app_bundle(Path::new("/Volumes/tron/tron.app/Contents/MacOS/tron")).unwrap();
        assert_eq!(bundle, Path::new("/Volumes/tron/tron.app"));
        assert_eq!(app_bundle(Path::new("/usr/local/bin/tron")), None);
        assert!(starts_at_home(Some(Path::new("/Volumes/tron/tron.app/Contents/MacOS")), Some(&bundle)));
        assert!(starts_at_home(None, None), "tron's own directory is gone");
        assert!(!starts_at_home(Some(Path::new("/home/me/project")), Some(&bundle)));
        assert!(!starts_at_home(Some(Path::new("/home/me/project")), None));
        assert_eq!(starts_at_home(Some(Path::new("/")), None), cfg!(target_os = "macos"));

        let gone = SpawnOptions { cwd: Some("/nonexistent/tron/test".into()), ..Default::default() };
        let start = start_directory(&gone);
        assert!(start.as_deref().is_none_or(Path::is_dir), "never a missing directory: {start:?}");
    }

    #[test]
    fn reads_the_child_working_directory() {
        let dir = std::env::temp_dir().canonicalize().unwrap();
        let options = SpawnOptions {
            program: Some("sleep".into()),
            args: vec!["5".into()],
            cwd: Some(dir.clone()),
            ..Default::default()
        };
        let pty = Pty::spawn(&options, WindowSize { cols: 80, rows: 24, ..Default::default() }).unwrap();
        assert_eq!(pty.cwd(), Some(dir));
    }

    #[test]
    fn runs_command_and_reads_output() {
        let options = SpawnOptions {
            remove_env: Vec::new(),
            program: Some("/bin/sh".into()),
            args: vec![
                "-c".into(),
                // stty reads stdin, the pty. macOS tput reads stdout, a pipe inside $(...).
                "printf 'cols=%s' \"$(stty size | cut -d' ' -f2)\"".into(),
            ],
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
