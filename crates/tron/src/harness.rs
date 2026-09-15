//! Knowing which coding agent harness, such as Claude Code or Codex, runs in a
//! window, also inside tmux, for the glowing line at the top of the window.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Weak};
use std::thread;
use std::time::Duration;

use parking_lot::Mutex;
use tron_config::HarnessConfig;
use winit::event_loop::EventLoopProxy;

/// How often the watcher looks at what runs in the window.
const POLL: Duration = Duration::from_millis(500);

/// Harnesses tron knows, by command name, with the color of their brand.
const KNOWN: &[(&str, [u8; 3])] = &[
    // Claude Code: Anthropic's Claude orange.
    ("claude", [0xD9, 0x77, 0x57]),
    // OpenAI Codex: the white Codex mark.
    ("codex", [0xFA, 0xFA, 0xFA]),
    // opencode: the primary color of its interface.
    ("opencode", [0xFA, 0xB2, 0x83]),
    // pi: the coral of its logo.
    ("pi", [0xF0, 0x90, 0x82]),
    // Gemini CLI: Gemini blue.
    ("gemini", [0x47, 0x96, 0xE3]),
    // Amp: its orange.
    ("amp", [0xF6, 0x83, 0x3B]),
    // Crush: Charm's purple.
    ("crush", [0x6B, 0x50, 0xFF]),
    // Qwen Code: Qwen purple.
    ("qwen", [0x61, 0x5C, 0xED]),
    // Cursor's agent: Cursor's white.
    ("cursor-agent", [0xF5, 0xF5, 0xF5]),
];

/// npm packages whose scripts run through node, named by the harness they are.
const PACKAGES: &[(&str, &str)] = &[
    ("@anthropic-ai/claude-code", "claude"),
    ("@openai/codex", "codex"),
    ("opencode-ai", "opencode"),
    ("pi-coding-agent", "pi"),
    ("@google/gemini-cli", "gemini"),
    ("@sourcegraph/amp", "amp"),
    ("@charmland/crush", "crush"),
    ("@qwen-code/qwen-code", "qwen"),
];

/// Programs that run a script, where the script is what runs.
const RUNNERS: &[&str] = &["node", "nodejs", "bun", "deno", "npx", "bunx", "python", "python3", "uv", "uvx"];

/// Command names of the harnesses to look for: the known ones and those in `[harness.colors]`.
pub fn names(config: &HarnessConfig) -> Vec<String> {
    let mut names: Vec<String> = KNOWN.iter().map(|(name, _)| (*name).to_owned()).collect();
    names.extend(config.colors.keys().filter(|name| !names.contains(name)).cloned().collect::<Vec<_>>());
    names
}

/// The line color of the harness `name`: from `[harness.colors]`, else its brand color.
pub fn color(name: &str, config: &HarnessConfig) -> Option<[u8; 3]> {
    config
        .colors
        .get(name)
        .map(|color| color.to_array())
        .or_else(|| KNOWN.iter().find(|(known, _)| *known == name).map(|(_, color)| *color))
}

/// A program name from an argument: the file name without leading dots and a
/// script extension, and without a process title's suffix such as `tmux: client`.
fn command_name(arg: &str) -> &str {
    let name = arg.rsplit('/').next().unwrap_or(arg).trim_start_matches('.');
    let name = name.split([' ', ':']).next().unwrap_or(name);
    [".js", ".mjs", ".cjs", ".ts", ".py"].iter().find_map(|extension| name.strip_suffix(extension)).unwrap_or(name)
}

/// The harness a command line runs, by its name in `names`.
pub fn identify(args: &[String], names: &[String]) -> Option<String> {
    let known = |name: &str| names.iter().find(|known| *known == name).cloned();
    for (package, name) in PACKAGES {
        if args.iter().take(3).any(|arg| arg.contains(package))
            && let Some(name) = known(name)
        {
            return Some(name);
        }
    }
    let program = command_name(args.first()?);
    if RUNNERS.contains(&program) {
        let script = args.iter().skip(1).find(|arg| !arg.starts_with('-'))?;
        return known(command_name(script));
    }
    known(program)
}

/// What a window's terminal runs: the harness found last, and the names to look for.
#[derive(Default)]
pub struct Watch {
    names: Mutex<Vec<String>>,
    found: Mutex<Option<String>>,
}

impl Watch {
    pub fn set_names(&self, names: Vec<String>) {
        *self.names.lock() = names;
    }

    /// The harness running now, by command name.
    pub fn found(&self) -> Option<String> {
        self.found.lock().clone()
    }
}

/// Watches the terminal of `shell`, whose device is `tty`, for harnesses on a
/// thread, which wakes the event loop when the one running changes and ends once
/// `watch` is dropped.
pub fn spawn(tty: PathBuf, shell: u32, watch: &Arc<Watch>, proxy: EventLoopProxy) {
    let watch: Weak<Watch> = Arc::downgrade(watch);
    let spawned = thread::Builder::new().name("harness-watch".into()).spawn(move || {
        loop {
            thread::sleep(POLL);
            let Some(watch) = watch.upgrade() else { break };
            let names = watch.names.lock().clone();
            let found = find(&tty, shell, &names);
            let mut current = watch.found.lock();
            if *current != found {
                *current = found;
                drop(current);
                proxy.wake_up();
            }
        }
    });
    if let Err(error) = spawned {
        log::warn!("cannot watch for coding agents: {error}");
    }
}

/// The harness in the foreground of the terminal of `shell`, whose device is
/// `tty`, or in tmux's active pane when the foreground is a tmux client.
fn find(tty: &Path, shell: u32, names: &[String]) -> Option<String> {
    let group = tron_pty::terminal_foreground(shell)?;
    let args = tron_pty::process_args(group)?;
    if command_name(args.first()?) == "tmux" {
        let found = in_tmux(group, &args, tty, names);
        log::debug!("harness watch: tmux client {group} {args:?}: {found:?}");
        return found;
    }
    identify(&args, names)
}

/// The harness in the active pane of the tmux client `client`, attached to `tty`.
fn in_tmux(client: u32, args: &[String], tty: &Path, names: &[String]) -> Option<String> {
    let program = tron_pty::process_path(client).unwrap_or_else(|| PathBuf::from("tmux"));
    let mut command = Command::new(program);
    // The client's server socket, when its command line still shows it.
    let mut rest = args.iter().skip(1);
    while let Some(arg) = rest.next() {
        if arg == "-L" || arg == "-S" {
            command.arg(arg).args(rest.next());
        } else if arg.len() > 2 && (arg.starts_with("-L") || arg.starts_with("-S")) {
            command.arg(arg);
        }
    }
    // Otherwise the client's environment picks the socket, not tron's: tron started
    // inside tmux has a TMUX of its own, and the shell may set TMUX_TMPDIR.
    command.env_remove("TMUX").env_remove("TMUX_PANE");
    match tron_pty::process_env_var(client, "TMUX_TMPDIR") {
        Some(dir) => command.env("TMUX_TMPDIR", dir),
        None => command.env_remove("TMUX_TMPDIR"),
    };
    let output = command
        .args(["display-message", "-p", "-c"])
        .arg(tty)
        .arg("#{pane_pid}")
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    if !output.status.success() {
        log::debug!("harness watch: tmux display-message failed: {}", String::from_utf8_lossy(&output.stderr).trim());
        return None;
    }
    let pane_shell = String::from_utf8_lossy(&output.stdout).trim().parse().ok()?;
    let group = tron_pty::terminal_foreground(pane_shell)?;
    let pane_args = tron_pty::process_args(group)?;
    log::debug!("harness watch: tmux pane shell {pane_shell}, foreground {group} {pane_args:?}");
    identify(&pane_args, names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_owned).collect()
    }

    fn known() -> Vec<String> {
        names(&HarnessConfig::default())
    }

    #[test]
    fn identifies_harnesses_from_their_command_lines() {
        let names = known();
        assert_eq!(identify(&args("claude"), &names).as_deref(), Some("claude"));
        assert_eq!(identify(&args("/home/me/.local/bin/claude --resume"), &names).as_deref(), Some("claude"));
        assert_eq!(
            identify(&args("node /usr/lib/node_modules/@openai/codex/bin/codex.js"), &names).as_deref(),
            Some("codex")
        );
        assert_eq!(
            identify(&args("node /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js"), &names).as_deref(),
            Some("claude")
        );
        assert_eq!(identify(&args("/opt/opencode/.opencode"), &names).as_deref(), Some("opencode"));
        assert_eq!(identify(&args("bun --smol /usr/bin/pi"), &names).as_deref(), Some("pi"));
        assert_eq!(identify(&args("npx @google/gemini-cli"), &names).as_deref(), Some("gemini"));
        assert_eq!(identify(&args("fish"), &names), None);
        assert_eq!(identify(&args("node server.js"), &names), None);
        assert_eq!(identify(&args("vim claude.md"), &names), None);
    }

    #[test]
    fn config_adds_harnesses_and_overrides_colors() {
        let config = tron_config::Config::parse("[harness.colors]\nclaude = \"#112233\"\nmyagent = \"#445566\"")
            .expect("valid harness config")
            .harness;
        let names = names(&config);
        assert_eq!(identify(&args("myagent --fast"), &names).as_deref(), Some("myagent"));
        assert_eq!(color("claude", &config), Some([0x11, 0x22, 0x33]));
        assert_eq!(color("codex", &config), Some([0xFA, 0xFA, 0xFA]));
        assert_eq!(color("myagent", &config), Some([0x44, 0x55, 0x66]));
        assert_eq!(color("unknown", &config), None);
    }

    #[test]
    fn program_names_drop_paths_titles_and_extensions() {
        assert_eq!(command_name("/usr/bin/tmux"), "tmux");
        assert_eq!(command_name("tmux: client"), "tmux");
        assert_eq!(command_name("cli.mjs"), "cli");
        assert_eq!(command_name(".opencode"), "opencode");
    }
}
