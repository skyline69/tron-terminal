//! The macOS menu bar: the items tron handles, what they do and the shortcuts they show.
//!
//! This part does not touch AppKit, so it is tested on every platform. `macos`
//! builds the menus from it.

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::{install, take_commands, update};

use tron_config::{Action, BindKey, Binding, KeyCombo};

/// Opened by Help > tron Documentation.
const DOCUMENTATION_URL: &str = "https://github.com/skyline69/tron-terminal/wiki";
/// Opened by Help > Report an Issue.
const ISSUES_URL: &str = "https://github.com/skyline69/tron-terminal/issues";

/// What choosing a menu item asks the application to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuCommand {
    /// Runs a key binding action in the session.
    Action(Action),
    /// Opens the startup screen's Settings tab in a new window.
    OpenSettings,
    /// Opens a web page.
    OpenUrl(&'static str),
}

/// A menu item tron carries out itself. AppKit's standard items, such as Quit
/// or Minimize, go through the responder chain instead and are not listed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Settings,
    NewWindow,
    Copy,
    Paste,
    PasteSelection,
    Find,
    SelectCommandOutput,
    CopyCommandOutput,
    ClearScrollback,
    IncreaseFontSize,
    DecreaseFontSize,
    ResetFontSize,
    PreviousPrompt,
    NextPrompt,
    ScrollToTop,
    ScrollToBottom,
    ReloadConfig,
    Documentation,
    ReportIssue,
}

impl Item {
    /// Every item, in declaration order, so that `ALL[i]` has the tag `i + 1`.
    pub const ALL: [Item; 19] = [
        Item::Settings,
        Item::NewWindow,
        Item::Copy,
        Item::Paste,
        Item::PasteSelection,
        Item::Find,
        Item::SelectCommandOutput,
        Item::CopyCommandOutput,
        Item::ClearScrollback,
        Item::IncreaseFontSize,
        Item::DecreaseFontSize,
        Item::ResetFontSize,
        Item::PreviousPrompt,
        Item::NextPrompt,
        Item::ScrollToTop,
        Item::ScrollToBottom,
        Item::ReloadConfig,
        Item::Documentation,
        Item::ReportIssue,
    ];

    /// The `NSMenuItem` tag identifying this item. Tags start at 1 because 0 is
    /// the tag of every menu item that was never given one.
    pub fn tag(self) -> isize {
        self as isize + 1
    }

    pub fn from_tag(tag: isize) -> Option<Self> {
        let index = usize::try_from(tag.checked_sub(1)?).ok()?;
        Self::ALL.get(index).copied()
    }

    pub fn title(self) -> &'static str {
        match self {
            Item::Settings => "Settings…",
            Item::NewWindow => "New Window",
            Item::Copy => "Copy",
            Item::Paste => "Paste",
            Item::PasteSelection => "Paste Selection",
            Item::Find => "Find…",
            Item::SelectCommandOutput => "Select Command Output",
            Item::CopyCommandOutput => "Copy Command Output",
            Item::ClearScrollback => "Clear Scrollback",
            Item::IncreaseFontSize => "Increase Font Size",
            Item::DecreaseFontSize => "Decrease Font Size",
            Item::ResetFontSize => "Reset Font Size",
            Item::PreviousPrompt => "Scroll to Previous Prompt",
            Item::NextPrompt => "Scroll to Next Prompt",
            Item::ScrollToTop => "Scroll to Top",
            Item::ScrollToBottom => "Scroll to Bottom",
            Item::ReloadConfig => "Reload Configuration",
            Item::Documentation => "tron Documentation",
            Item::ReportIssue => "Report an Issue",
        }
    }

    pub fn command(self) -> MenuCommand {
        let action = match self {
            Item::Settings => return MenuCommand::OpenSettings,
            Item::Documentation => return MenuCommand::OpenUrl(DOCUMENTATION_URL),
            Item::ReportIssue => return MenuCommand::OpenUrl(ISSUES_URL),
            Item::NewWindow => Action::NewWindow,
            Item::Copy => Action::Copy,
            Item::Paste => Action::Paste,
            Item::PasteSelection => Action::PasteSelection,
            Item::Find => Action::Search,
            Item::SelectCommandOutput => Action::SelectCommandOutput,
            Item::CopyCommandOutput => Action::CopyCommandOutput,
            Item::ClearScrollback => Action::ClearScrollback,
            Item::IncreaseFontSize => Action::IncreaseFontSize,
            Item::DecreaseFontSize => Action::DecreaseFontSize,
            Item::ResetFontSize => Action::ResetFontSize,
            Item::PreviousPrompt => Action::ScrollToPreviousPrompt,
            Item::NextPrompt => Action::ScrollToNextPrompt,
            Item::ScrollToTop => Action::ScrollToTop,
            Item::ScrollToBottom => Action::ScrollToBottom,
            Item::ReloadConfig => Action::ReloadConfig,
        };
        MenuCommand::Action(action)
    }

    /// The key binding whose shortcut the item shows, if any binding runs its action.
    pub fn shortcut(self, bindings: &[Binding]) -> Option<(String, &KeyCombo)> {
        match self.command() {
            MenuCommand::Action(action) => shortcut(bindings, &action),
            MenuCommand::OpenSettings | MenuCommand::OpenUrl(_) => None,
        }
    }
}

/// The binding shown for `action` with its key equivalent. Several bindings can
/// run one action; menus conventionally show a Command shortcut, then the one
/// with the fewest modifiers. Ties keep the first binding.
fn shortcut<'a>(bindings: &'a [Binding], action: &Action) -> Option<(String, &'a KeyCombo)> {
    bindings
        .iter()
        .filter(|binding| &binding.action == action)
        .filter_map(|binding| Some((key_equivalent(&binding.combo.key)?, &binding.combo)))
        .min_by_key(|(_, combo)| {
            let modifiers = [combo.ctrl, combo.shift, combo.alt, combo.super_key].into_iter().filter(|&held| held);
            (!combo.super_key, modifiers.count())
        })
}

/// The `NSMenuItem` key equivalent string for a key: characters as themselves
/// and named keys as the characters AppKit reports for them.
pub fn key_equivalent(key: &BindKey) -> Option<String> {
    let code = match key {
        BindKey::Char(c) => return Some(c.to_lowercase().collect()),
        BindKey::Named(name) => named_key_character(name)?,
    };
    char::from_u32(code).map(String::from)
}

/// AppKit's character for a named key, from `NSText.h` and `NSEvent.h`.
fn named_key_character(name: &str) -> Option<u32> {
    /// `NSF1FunctionKey`; F2 to F35 follow it.
    const F1: u32 = 0xF704;
    if let Some(number) = name.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=35).contains(&number).then(|| F1 + number - 1);
    }
    Some(match name {
        "enter" => 0x0D,     // NSCarriageReturnCharacter
        "tab" => 0x09,       // NSTabCharacter
        "backspace" => 0x08, // NSBackspaceCharacter
        "escape" => 0x1B,
        "up" => 0xF700,        // NSUpArrowFunctionKey
        "down" => 0xF701,      // NSDownArrowFunctionKey
        "left" => 0xF702,      // NSLeftArrowFunctionKey
        "right" => 0xF703,     // NSRightArrowFunctionKey
        "insert" => 0xF727,    // NSInsertFunctionKey
        "delete" => 0xF728,    // NSDeleteFunctionKey, the forward delete key
        "home" => 0xF729,      // NSHomeFunctionKey
        "end" => 0xF72B,       // NSEndFunctionKey
        "page_up" => 0xF72C,   // NSPageUpFunctionKey
        "page_down" => 0xF72D, // NSPageDownFunctionKey
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(combo: &str, action: Action) -> Binding {
        Binding { combo: KeyCombo::parse(combo).unwrap(), action }
    }

    #[test]
    fn tags_identify_items() {
        for item in Item::ALL {
            assert_eq!(Item::from_tag(item.tag()), Some(item), "{item:?}");
        }
        assert_eq!(Item::from_tag(0), None);
        assert_eq!(Item::from_tag(-1), None);
        assert_eq!(Item::from_tag(Item::ALL.len() as isize + 1), None);
    }

    #[test]
    fn items_map_to_commands() {
        assert_eq!(Item::Find.command(), MenuCommand::Action(Action::Search));
        assert_eq!(Item::ReloadConfig.command(), MenuCommand::Action(Action::ReloadConfig));
        assert_eq!(Item::Settings.command(), MenuCommand::OpenSettings);
        assert_eq!(
            Item::Documentation.command(),
            MenuCommand::OpenUrl("https://github.com/skyline69/tron-terminal/wiki")
        );
        assert_eq!(
            Item::ReportIssue.command(),
            MenuCommand::OpenUrl("https://github.com/skyline69/tron-terminal/issues")
        );
        assert!(Item::ALL.iter().all(|item| !item.title().is_empty()));
    }

    #[test]
    fn key_equivalents() {
        let named = |name: &str| key_equivalent(&BindKey::Named(name.into()));
        assert_eq!(key_equivalent(&BindKey::Char('c')).as_deref(), Some("c"));
        assert_eq!(key_equivalent(&BindKey::Char('A')).as_deref(), Some("a"));
        assert_eq!(key_equivalent(&BindKey::Char('+')).as_deref(), Some("+"));
        assert_eq!(named("enter").as_deref(), Some("\r"));
        assert_eq!(named("up").as_deref(), Some("\u{F700}"));
        assert_eq!(named("page_down").as_deref(), Some("\u{F72D}"));
        assert_eq!(named("f1").as_deref(), Some("\u{F704}"));
        assert_eq!(named("f12").as_deref(), Some("\u{F70F}"));
        assert_eq!(named("f35").as_deref(), Some("\u{F726}"));
        assert_eq!(named("f36"), None);
        assert_eq!(named("f0"), None);
        assert_eq!(named("menu"), None);
    }

    #[test]
    fn shortcut_prefers_command_bindings() {
        let bindings = [
            binding("shift+home", Action::ScrollToTop),
            binding("super+home", Action::ScrollToTop),
            binding("super+shift+up", Action::SelectCommandOutput),
            binding("super+alt+g", Action::SelectCommandOutput),
            binding("super+g", Action::SelectCommandOutput),
        ];
        let (key, combo) = Item::ScrollToTop.shortcut(&bindings).unwrap();
        assert_eq!(key, "\u{F729}");
        assert!(combo.super_key && !combo.shift);
        let (key, combo) = Item::SelectCommandOutput.shortcut(&bindings).unwrap();
        assert_eq!(key, "g");
        assert!(combo.super_key && !combo.alt && !combo.ctrl);
        assert_eq!(Item::Copy.shortcut(&bindings), None);
        assert_eq!(Item::Settings.shortcut(&bindings), None);
    }
}
