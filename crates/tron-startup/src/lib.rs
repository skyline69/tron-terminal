//! The startup screen: a terminal UI that tron runs inside its own window
//! before the shell. When it ends, the process replaces itself with the shell,
//! so the session continues in the same pty.

mod app;
pub mod catalog;
mod credits;
pub mod link;
mod motion;
mod pickers;
mod settings;
mod splash;
mod tour;
mod ui;

use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

/// Environment variable naming the file written once the screen was shown.
pub const MARKER_ENV: &str = "TRON_STARTUP_MARKER";
/// Environment variable set to `0` to turn animations off.
pub const ANIMATIONS_ENV: &str = "TRON_STARTUP_ANIMATIONS";
/// Environment variable holding the token that authorizes startup screen commands.
/// Set for everything in a tron window, so `tron --startup` can run in it.
pub const TOKEN_ENV: &str = "TRON_WINDOW_TOKEN";
/// Environment variable naming tron's configuration directory, kept for the shell.
pub const CONFIG_DIR_ENV: &str = "TRON_CONFIG_DIR";
/// Environment variable naming the tab (by title) to open directly, skipping the splash.
pub const TAB_ENV: &str = "TRON_STARTUP_TAB";
/// Debug aid: open this tour page, by number from 1.
pub(crate) const DEBUG_PAGE_ENV: &str = "TRON_STARTUP_DEBUG_PAGE";
/// Variables for the startup screen only, removed before the shell starts.
const ENV: [&str; 4] = [MARKER_ENV, ANIMATIONS_ENV, TAB_ENV, DEBUG_PAGE_ENV];

/// Titles of the startup screen's tabs, in order.
pub fn tab_titles() -> impl Iterator<Item = &'static str> {
    app::Tab::ALL.into_iter().map(app::Tab::title)
}

/// How the splash ended.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// On to the tabs.
    Continue,
    /// Straight to the shell.
    Quit,
}

/// Shows the startup screen, then runs `shell` (program and arguments) in
/// this process. Never returns.
pub fn run(shell: &[String]) -> ! {
    let animations = std::env::var_os(ANIMATIONS_ENV).is_none_or(|value| value != "0");
    if let Err(error) = show(animations, shell, None) {
        eprintln!("tron: startup screen failed: {error}");
    }
    if let Some(marker) = std::env::var_os(MARKER_ENV) {
        mark_shown(Path::new(&marker));
    }
    hand_off(shell)
}

/// Shows the startup screen in the terminal this process runs in, as
/// `tron --startup` does inside a tron window, then returns to the caller.
/// `tab` opens that tab (by title) directly.
pub fn run_here(animations: bool, tab: Option<&str>) -> io::Result<()> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".to_owned());
    show(animations, &[shell], tab)
}

/// Whether this process runs in a tron window that accepts startup screen commands.
pub fn inside_tron() -> bool {
    std::env::var_os(TOKEN_ENV).is_some_and(|token| !token.is_empty())
        && std::env::var_os("TERM_PROGRAM").is_some_and(|program| program == "tron")
}

fn show(animations: bool, shell: &[String], tab: Option<&str>) -> io::Result<()> {
    let mut link = link::Link::new(std::env::var(TOKEN_ENV).ok(), io::stdout());
    let mut terminal = ratatui::try_init()?;
    let tab = tab
        .map(str::to_owned)
        .or_else(|| std::env::var(TAB_ENV).ok())
        .and_then(|title| app::Tab::ALL.into_iter().find(|tab| tab.title().eq_ignore_ascii_case(&title)));
    let splash = match tab {
        Some(_) => {
            link.shader_on(splash::SCENE);
            Ok(Outcome::Continue)
        }
        None => splash::run(&mut terminal, animations, &mut link),
    };
    let result = match splash {
        Ok(Outcome::Continue) => app::run(&mut terminal, animations, &mut link, shell, tab),
        Ok(Outcome::Quit) => Ok(()),
        Err(error) => Err(error),
    };
    // Restores the configuration and turns the shader off, also after a failure.
    drop(link);
    ratatui::try_restore()?;
    result
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
    let command = |program: &str| {
        let mut command = Command::new(program);
        for name in ENV {
            command.env_remove(name);
        }
        command
    };
    let mut shell = command(program);
    shell.args(args);
    // macOS terminals start the shell as a login shell, like tron-pty does without the startup screen.
    #[cfg(target_os = "macos")]
    if args.is_empty()
        && let Some(name) = Path::new(program).file_name()
    {
        shell.arg0(format!("-{}", name.to_string_lossy()));
    }
    let error = shell.exec();
    eprintln!("tron: cannot run {program}: {error}");
    let error = command("/bin/sh").exec();
    eprintln!("tron: cannot run /bin/sh: {error}");
    std::process::exit(127)
}
