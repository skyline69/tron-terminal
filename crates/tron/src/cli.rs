//! Command line interface.

use std::path::PathBuf;

use clap::builder::styling::{AnsiColor, Effects, Style, Styles};
use clap::{CommandFactory, Parser, Subcommand};
use tron_config::Config;

const STYLES: Styles = Styles::styled()
    .header(AnsiColor::BrightCyan.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::BrightCyan.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::BrightMagenta.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::BrightBlack.on_default())
    .error(AnsiColor::BrightRed.on_default().effects(Effects::BOLD))
    .valid(AnsiColor::BrightGreen.on_default())
    .invalid(AnsiColor::BrightYellow.on_default());

/// File in the data directory written once the startup screen was shown.
pub const STARTUP_MARKER: &str = "startup-done";

#[derive(Parser, Clone, Debug, Default)]
#[command(
    name = "tron",
    version,
    about = "GPU accelerated terminal emulator",
    styles = STYLES,
    before_help = banner(),
    after_help = examples(),
    disable_help_subcommand = true,
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    /// Run a program instead of the shell
    #[arg(short = 'e', long = "command", value_name = "PROGRAM", num_args = 1.., allow_hyphen_values = true)]
    pub command: Option<Vec<String>>,
    /// Start in this directory
    #[arg(short = 'd', long, value_name = "DIR")]
    pub working_directory: Option<PathBuf>,
    /// Use this configuration directory
    #[arg(long, value_name = "DIR")]
    pub config_dir: Option<PathBuf>,
    /// Show the startup screen: welcome, setup and tour
    #[arg(long, conflicts_with = "command")]
    pub startup: bool,
    /// Never show the startup screen (new windows)
    #[arg(long, hide = true)]
    pub no_startup: bool,
    #[command(subcommand)]
    pub subcommand: Option<Command>,
}

#[derive(Subcommand, Clone, Debug)]
pub enum Command {
    /// Print shell completions
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Run the startup screen, then the given shell (used inside the terminal)
    #[command(hide = true)]
    StartupScreen {
        #[arg(num_args = 1.., required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        shell: Vec<String>,
    },
}

impl Cli {
    /// Whether the startup screen runs before the shell: always with
    /// `--startup`, otherwise as configured, and by default only the first time.
    pub fn show_startup(&self, config: &Config, marker_exists: bool, screenshot: bool) -> bool {
        self.startup
            || (!self.no_startup && self.command.is_none() && !screenshot && config.startup.unwrap_or(!marker_exists))
    }
}

pub fn print_completions(shell: clap_complete::Shell) {
    use std::io::Write;
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut Cli::command(), "tron", &mut script);
    // A closed pipe (`tron completions fish | head`) is not an error worth reporting.
    let _ = std::io::stdout().write_all(&script);
}

fn banner() -> String {
    let logo = AnsiColor::BrightCyan.on_default().effects(Effects::BOLD);
    let dim = Style::new().effects(Effects::DIMMED);
    let version = env!("CARGO_PKG_VERSION");
    format!(
        "{logo}▀█▀ █▀█ █▀█ █▄ █{logo:#}\n{logo} █  █▀▄ █▄█ █ ▀█{logo:#}   {dim}GPU accelerated terminal · {version}{dim:#}"
    )
}

fn examples() -> String {
    let header = AnsiColor::BrightCyan.on_default().effects(Effects::BOLD);
    let command = AnsiColor::BrightMagenta.on_default();
    let dim = Style::new().effects(Effects::DIMMED);
    format!(
        "{header}Examples:{header:#}\n  {command}tron -e nvim notes.md{command:#}\n  {command}tron -d ~/projects{command:#}\n  \
         {command}tron --startup{command:#}\n  {command}tron completions fish > ~/.config/fish/completions/tron.fish{command:#}\n\n\
         {dim}Configuration: ~/.config/tron/config.toml{dim:#}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("tron").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn command_takes_everything_after_it() {
        let cli = parse(&["-d", "/tmp", "-e", "nvim", "-u", "NONE", "--startup"]);
        assert_eq!(cli.command.unwrap(), ["nvim", "-u", "NONE", "--startup"]);
        assert_eq!(cli.working_directory.unwrap(), PathBuf::from("/tmp"));
        assert!(!cli.startup);
    }

    #[test]
    fn startup_screen_passes_the_shell_through() {
        let Some(Command::StartupScreen { shell }) = parse(&["startup-screen", "--", "fish", "-l"]).subcommand else {
            panic!("not parsed as startup-screen");
        };
        assert_eq!(shell, ["fish", "-l"]);
    }

    #[test]
    fn startup_screen_rules() {
        let default = Config::default();
        let never = Config { startup: Some(false), ..Config::default() };
        let always = Config { startup: Some(true), ..Config::default() };
        assert!(parse(&[]).show_startup(&default, false, false), "first launch");
        assert!(!parse(&[]).show_startup(&default, true, false), "later launches");
        assert!(parse(&[]).show_startup(&always, true, false));
        assert!(!parse(&[]).show_startup(&never, false, false));
        assert!(parse(&["--startup"]).show_startup(&never, true, true), "--startup always wins");
        assert!(!parse(&["-e", "top"]).show_startup(&default, false, false));
        assert!(!parse(&["--no-startup"]).show_startup(&default, false, false));
        assert!(!parse(&[]).show_startup(&default, false, true), "screenshots skip it");
    }
}
