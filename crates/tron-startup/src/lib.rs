//! The startup screen: a terminal UI that tron runs inside its own window
//! before the shell. When it ends, the process replaces itself with the shell,
//! so the session continues in the same pty.

mod link;
mod splash;

use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

/// Environment variable naming the file written once the screen was shown.
pub const MARKER_ENV: &str = "TRON_STARTUP_MARKER";
/// Environment variable set to `0` to turn animations off.
pub const ANIMATIONS_ENV: &str = "TRON_STARTUP_ANIMATIONS";
/// Environment variable holding the token that authorizes startup screen commands.
pub const TOKEN_ENV: &str = "TRON_STARTUP_TOKEN";

/// What the user chose on the startup screen.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Skip,
    Setup,
    Tour,
}

/// Shows the startup screen, then runs `shell` (program and arguments) in
/// this process. Never returns.
pub fn run(shell: &[String]) -> ! {
    let animations = std::env::var_os(ANIMATIONS_ENV).is_none_or(|value| value != "0");
    match show(animations) {
        Ok(outcome) => log::debug!("startup screen finished: {outcome:?}"),
        Err(error) => eprintln!("tron: startup screen failed: {error}"),
    }
    if let Some(marker) = std::env::var_os(MARKER_ENV) {
        mark_shown(Path::new(&marker));
    }
    hand_off(shell)
}

fn show(animations: bool) -> io::Result<Outcome> {
    let mut link = link::Link::new(std::env::var(TOKEN_ENV).ok(), io::stdout());
    let mut terminal = ratatui::try_init()?;
    let outcome = splash::run(&mut terminal, animations, &mut link);
    // Turns the shader off even when the screen failed.
    drop(link);
    ratatui::try_restore()?;
    outcome
}

/// Remembers that the screen was shown, so it does not start automatically again.
fn mark_shown(marker: &Path) {
    let written = marker
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(marker, env!("CARGO_PKG_VERSION")));
    if let Err(error) = written {
        eprintln!("tron: cannot write {}: {error}", marker.display());
    }
}

/// Replaces this process with the shell, falling back to `/bin/sh`.
fn hand_off(shell: &[String]) -> ! {
    let (program, args) = shell.split_first().map_or(("/bin/sh", &[][..]), |(p, a)| (p.as_str(), a));
    let error =
        Command::new(program).args(args).env_remove(MARKER_ENV).env_remove(ANIMATIONS_ENV).env_remove(TOKEN_ENV).exec();
    eprintln!("tron: cannot run {program}: {error}");
    let error = Command::new("/bin/sh").env_remove(MARKER_ENV).env_remove(ANIMATIONS_ENV).exec();
    eprintln!("tron: cannot run /bin/sh: {error}");
    std::process::exit(127)
}
