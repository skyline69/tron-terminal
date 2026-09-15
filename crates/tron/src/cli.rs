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
    /// Open the startup screen's overview
    Overview,
    /// Change settings with a live preview
    Settings,
    /// Preview and pick a color theme
    Themes,
    /// Preview and pick post-processing shaders
    Shaders,
    /// Show the key bindings
    Keys,
    /// Take the tour of tron's features
    Tour,
    /// Show who made the themes, shaders and libraries
    Credits,
    /// Show the version and links
    About,
    /// Run the startup screen, then the given shell (used inside the terminal)
    #[command(hide = true)]
    StartupScreen {
        #[arg(num_args = 1.., required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        shell: Vec<String>,
    },
}

impl Command {
    /// Title of the startup screen tab this command opens.
    pub fn startup_tab(&self) -> Option<&'static str> {
        Some(match self {
            Self::Overview => "Overview",
            Self::Settings => "Settings",
            Self::Themes => "Themes",
            Self::Shaders => "Shaders",
            Self::Keys => "Keys",
            Self::Tour => "Tour",
            Self::Credits => "Credits",
            Self::About => "About",
            Self::Completions { .. } | Self::StartupScreen { .. } => return None,
        })
    }
}

impl Cli {
    /// Startup screen tab requested with a command such as `tron settings`.
    pub fn startup_tab(&self) -> Option<&'static str> {
        self.subcommand.as_ref().and_then(Command::startup_tab)
    }

    /// Whether the startup screen runs before the shell: always with
    /// `--startup` or a tab command, otherwise as configured, and by default
    /// only the first time.
    pub fn show_startup(&self, config: &Config, marker_exists: bool, screenshot: bool) -> bool {
        self.startup
            || self.startup_tab().is_some()
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
         {command}tron settings{command:#}      {dim}opens in this window when run inside tron{dim:#}\n  \
         {command}tron completions fish > ~/.config/fish/completions/tron.fish{command:#}\n\n\
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
    fn tab_commands_open_their_tab() {
        let titles: Vec<&str> = tron_startup::tab_titles().collect();
        for (name, title) in [
            ("overview", "Overview"),
            ("settings", "Settings"),
            ("themes", "Themes"),
            ("shaders", "Shaders"),
            ("keys", "Keys"),
            ("tour", "Tour"),
            ("credits", "Credits"),
            ("about", "About"),
        ] {
            let cli = parse(&[name]);
            assert_eq!(cli.startup_tab(), Some(title));
            assert!(titles.contains(&title), "the startup screen has no {title} tab");
            let never = Config { startup: Some(false), ..Config::default() };
            assert!(cli.show_startup(&never, true, false), "{name} opens the startup screen");
        }
        assert_eq!(titles.len(), 8, "every tab has a command");
        assert_eq!(parse(&[]).startup_tab(), None);
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
