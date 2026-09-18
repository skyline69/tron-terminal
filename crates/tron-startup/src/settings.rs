//! The Settings tab's options, the choices made on the startup screen, the TOML
//! previewed from them and saving them to `config.toml` without losing comments.

use std::collections::BTreeMap;
use std::io;

use tron_config::{Config, Paths};

/// Values a setting can take.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Bool,
    /// One of these strings.
    Choice(Vec<String>),
    Float {
        min: f64,
        max: f64,
        step: f64,
    },
    Integer {
        min: i64,
        max: i64,
        step: i64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Setting {
    /// Dotted path in `config.toml`, such as `font.size`.
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: Kind,
}

/// Stands for an unset `startup` key: show the screen on the first launch only.
const FIRST_LAUNCH: &str = "first launch";
const EVERY_LAUNCH: &str = "every launch";
const NEVER: &str = "never";
/// Stands for `shader.fps = 0`: animations follow the display's refresh rate.
const DISPLAY_RATE: &str = "display rate";

fn choice(values: &[&str]) -> Kind {
    Kind::Choice(values.iter().map(|v| (*v).to_owned()).collect())
}

/// Frame rates offered for `shader.fps`: the display's rate, then common rates up to
/// `max_fps`, the fastest connected monitor's refresh rate, which is offered too.
fn fps_choices(max_fps: Option<u32>) -> Kind {
    const RATES: [u32; 10] = [360, 240, 165, 144, 120, 90, 75, 60, 30, 15];
    let max = max_fps.unwrap_or(120);
    let mut choices = vec![DISPLAY_RATE.to_owned()];
    if !RATES.contains(&max) {
        choices.push(max.to_string());
    }
    choices.extend(RATES.iter().filter(|&&rate| rate <= max).map(u32::to_string));
    Kind::Choice(choices)
}

/// All settings. `fonts` are the families offered for `font.family`, `max_fps` the
/// highest refresh rate of the connected monitors.
pub fn list(fonts: &[String], max_fps: Option<u32>) -> Vec<Setting> {
    let setting = |key, label, help, kind| Setting { key, label, help, kind };
    vec![
        setting(
            "font.family",
            "Font",
            "Font family for all text. Only monospaced fonts are listed.",
            Kind::Choice(fonts.to_vec()),
        ),
        setting("font.size", "Font size", "Size in points.", Kind::Float { min: 6.0, max: 40.0, step: 0.5 }),
        setting(
            "font.ligatures",
            "Ligatures",
            "Programming ligatures such as -> and != drawn as one glyph.",
            Kind::Bool,
        ),
        setting(
            "font.hinting",
            "Hinting",
            "Fit glyph outlines to the pixel grid. Auto hints on low density screens.",
            choice(&["auto", "on", "off"]),
        ),
        setting("font.bidi", "Right-to-left text", "Show Arabic and Hebrew in reading order.", Kind::Bool),
        setting(
            "window.opacity",
            "Opacity",
            "Background opacity. Below 1 the desktop shows through.",
            Kind::Float { min: 0.2, max: 1.0, step: 0.05 },
        ),
        setting(
            "window.blur",
            "Blur",
            "Blur what is behind a translucent window, where the compositor supports it.",
            Kind::Bool,
        ),
        setting(
            "window.vsync",
            "Display sync",
            "Wait for the display's refresh when presenting. Off, tron paces frames itself \
             and input never waits on the display.",
            Kind::Bool,
        ),
        setting(
            "window.padding_x",
            "Padding left and right",
            "Space between the window edge and the text, in pixels.",
            Kind::Integer { min: 0, max: 80, step: 2 },
        ),
        setting(
            "window.padding_y",
            "Padding top and bottom",
            "Space between the window edge and the text, in pixels.",
            Kind::Integer { min: 0, max: 80, step: 2 },
        ),
        setting(
            "shader.animation",
            "Shader animation",
            "Auto animates shaders that read the time. Always and never override it.",
            choice(&["auto", "always", "never"]),
        ),
        setting(
            "shader.fps",
            "Animation frame rate",
            "Cap background animations to save power; capped, they also slow down without focus. \
             Output and cursor effects always use the display's rate.",
            fps_choices(max_fps),
        ),
        setting(
            "shader.pause_after",
            "Pause animations after",
            "Seconds without typing, mouse input or output before background animations pause. \
             0 never pauses them.",
            Kind::Integer { min: 0, max: 3600, step: 30 },
        ),
        setting(
            "cursor.shape",
            "Cursor shape",
            "Shape of the text cursor, unless an application sets one.",
            choice(&["block", "beam", "underline"]),
        ),
        setting(
            "cursor.blinking",
            "Cursor blinking",
            "App blinks when the application asks for it.",
            choice(&["app", "always", "never"]),
        ),
        setting(
            "scrollback.lines",
            "Scrollback",
            "Lines kept in history.",
            Kind::Integer { min: 0, max: 200_000, step: 1000 },
        ),
        setting(
            "scrollback.smooth",
            "Smooth scrolling",
            "Scroll by pixels instead of whole lines, easing the view into place.",
            Kind::Bool,
        ),
        setting(
            "selection.copy_on_select",
            "Copy on select",
            "Selected text goes to the primary selection, pasted with a middle click.",
            Kind::Bool,
        ),
        setting(
            "bell.mode",
            "Bell",
            "What happens on a terminal bell.",
            choice(&["none", "visual", "attention", "both"]),
        ),
        setting(
            "notifications.mode",
            "Notifications",
            "Desktop notifications from applications.",
            choice(&["never", "unfocused", "always"]),
        ),
        setting(
            "startup",
            "Startup screen",
            "When this screen opens. tron --startup always opens it.",
            choice(&[FIRST_LAUNCH, EVERY_LAUNCH, NEVER]),
        ),
        setting(
            "startup_animations",
            "Startup animations",
            "Animate this screen. Turn off for reduced motion.",
            Kind::Bool,
        ),
    ]
}

/// Lower case name of a config enum value, such as `Attention` → `attention`.
fn name(value: impl std::fmt::Debug) -> toml::Value {
    toml::Value::String(format!("{value:?}").to_lowercase())
}

/// Values of every setting in a configuration.
pub fn read(config: &Config) -> BTreeMap<&'static str, toml::Value> {
    use toml::Value::{Boolean, Float, Integer, String};
    let startup = match config.startup {
        None => FIRST_LAUNCH,
        Some(true) => EVERY_LAUNCH,
        Some(false) => NEVER,
    };
    BTreeMap::from([
        ("font.family", String(config.font.family.clone())),
        ("font.size", Float(f64::from(config.font.size))),
        ("font.ligatures", Boolean(config.font.ligatures)),
        ("font.hinting", name(config.font.hinting)),
        ("font.bidi", Boolean(config.font.bidi)),
        ("window.opacity", Float((f64::from(config.window.opacity) * 100.0).round() / 100.0)),
        ("window.blur", Boolean(config.window.blur)),
        ("window.vsync", Boolean(config.window.vsync)),
        ("window.padding_x", Integer(i64::from(config.window.padding_x))),
        ("window.padding_y", Integer(i64::from(config.window.padding_y))),
        ("shader.animation", name(config.shader.animation)),
        (
            "shader.fps",
            String(if config.shader.fps == 0 { DISPLAY_RATE.to_owned() } else { config.shader.fps.to_string() }),
        ),
        ("shader.pause_after", Integer(i64::from(config.shader.pause_after))),
        ("cursor.shape", name(config.cursor.shape)),
        ("cursor.blinking", name(config.cursor.blinking)),
        ("scrollback.lines", Integer(config.scrollback.lines as i64)),
        ("scrollback.smooth", Boolean(config.scrollback.smooth)),
        ("selection.copy_on_select", Boolean(config.selection.copy_on_select)),
        ("bell.mode", name(config.bell.mode)),
        ("notifications.mode", name(config.notifications.mode)),
        ("startup", String(startup.to_owned())),
        ("startup_animations", Boolean(config.startup_animations.unwrap_or(true))),
    ])
}

/// The next value of `setting` after `value`, `delta` steps away. Choices wrap around.
pub fn step(setting: &Setting, value: &toml::Value, delta: isize) -> toml::Value {
    match (&setting.kind, value) {
        (Kind::Bool, toml::Value::Boolean(on)) => toml::Value::Boolean(if delta == 0 { *on } else { !on }),
        (Kind::Choice(values), toml::Value::String(current)) if !values.is_empty() => {
            let index = values.iter().position(|v| v == current).map_or(0, |i| i as isize + delta);
            toml::Value::String(values[index.rem_euclid(values.len() as isize) as usize].clone())
        }
        (Kind::Float { min, max, step }, toml::Value::Float(current)) => {
            let next = ((current / step).round() + delta as f64) * step;
            toml::Value::Float((next.clamp(*min, *max) * 1000.0).round() / 1000.0)
        }
        (Kind::Integer { min, max, step }, toml::Value::Integer(current)) => {
            let next = (current / step + delta as i64) * step;
            toml::Value::Integer(next.clamp(*min, *max))
        }
        _ => value.clone(),
    }
}

/// How a value reads on screen.
pub fn display(value: &toml::Value) -> String {
    match value {
        toml::Value::Boolean(true) => "on".to_owned(),
        toml::Value::Boolean(false) => "off".to_owned(),
        toml::Value::String(text) => text.clone(),
        toml::Value::Float(number) => {
            let text = format!("{number:.2}");
            text.trim_end_matches('0').trim_end_matches('.').to_owned()
        }
        toml::Value::Integer(number) => number.to_string(),
        toml::Value::Array(items) => {
            if items.is_empty() {
                "none".to_owned()
            } else {
                items.iter().map(display).collect::<Vec<_>>().join(", ")
            }
        }
        other => other.to_string(),
    }
}

/// Choices made on the startup screen.
#[derive(Clone, Debug, PartialEq)]
pub struct Choices {
    pub theme: String,
    pub shaders: Vec<String>,
    pub settings: BTreeMap<&'static str, toml::Value>,
}

impl Choices {
    pub fn from_config(config: &Config) -> Self {
        Self {
            theme: config.theme.clone().unwrap_or_else(|| "tron".to_owned()),
            shaders: config.shader.files.clone(),
            settings: read(config),
        }
    }

    /// Every configuration key with its value, as it would be written.
    fn entries(&self) -> Vec<(&str, toml::Value)> {
        let files = self.shaders.iter().map(|file| toml::Value::String(file.clone())).collect();
        let mut entries =
            vec![("theme", toml::Value::String(self.theme.clone())), ("shader.files", toml::Value::Array(files))];
        entries.extend(self.settings.iter().map(|(key, value)| (*key, value.clone())));
        entries
    }

    /// The TOML tron previews. `theme` and `shaders` replace the chosen ones.
    pub fn overlay(&self, theme: &str, shaders: &[String]) -> String {
        let mut table = toml::Table::new();
        let preview = Choices { theme: theme.to_owned(), shaders: shaders.to_vec(), settings: self.settings.clone() };
        for (key, value) in preview.entries() {
            // The startup keys do nothing while the screen is open.
            if let (false, Some(value)) = (key.starts_with("startup"), config_value(key, &value)) {
                insert_path(&mut table, key, value);
            }
        }
        toml::to_string(&table).unwrap_or_default()
    }

    /// What differs from `saved`, as (label, old, new) for display.
    pub fn changes(&self, saved: &Choices, settings: &[Setting]) -> Vec<(String, String, String)> {
        let label = |key: &str| match key {
            "theme" => "Theme".to_owned(),
            "shader.files" => "Shaders".to_owned(),
            key => settings.iter().find(|s| s.key == key).map_or(key.to_owned(), |s| s.label.to_owned()),
        };
        saved
            .entries()
            .into_iter()
            .zip(self.entries())
            .filter(|((_, old), (_, new))| old != new)
            .map(|((key, old), (_, new))| (label(key), display(&old), display(&new)))
            .collect()
    }

    /// Writes the values that differ from `saved` to `config.toml`, keeping
    /// everything else in the file, comments included.
    pub fn save(&self, saved: &Choices, paths: &Paths) -> io::Result<()> {
        let text = match std::fs::read_to_string(&paths.config_file) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error),
        };
        let mut document: toml_edit::DocumentMut =
            text.parse().map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        for ((key, old), (_, new)) in saved.entries().into_iter().zip(self.entries()) {
            if old == new {
                continue;
            }
            let item = config_value(key, &new)
                .and_then(|value| value.to_string().parse::<toml_edit::Value>().ok())
                .map(toml_edit::Item::Value);
            set_path(&mut document, key, item);
        }
        std::fs::create_dir_all(&paths.config_dir)?;
        let temporary = paths.config_file.with_extension("toml.tmp");
        std::fs::write(&temporary, document.to_string())?;
        std::fs::rename(&temporary, &paths.config_file)
    }
}

/// The value written for a setting, `None` to remove the key.
fn config_value(key: &str, value: &toml::Value) -> Option<toml::Value> {
    match (key, value) {
        ("startup", toml::Value::String(text)) => match text.as_str() {
            EVERY_LAUNCH => Some(toml::Value::Boolean(true)),
            NEVER => Some(toml::Value::Boolean(false)),
            _ => None,
        },
        // The display rate is the default: no key.
        ("shader.fps", toml::Value::String(text)) => text.parse::<i64>().ok().map(toml::Value::Integer),
        _ => Some(value.clone()),
    }
}

fn insert_path(table: &mut toml::Table, key: &str, value: toml::Value) {
    match key.split_once('.') {
        Some((head, rest)) => {
            let entry = table.entry(head).or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let toml::Value::Table(nested) = entry {
                insert_path(nested, rest, value);
            }
        }
        None => {
            table.insert(key.to_owned(), value);
        }
    }
}

/// Sets or, with `None`, removes a dotted key, creating tables on the way.
fn set_path(document: &mut toml_edit::DocumentMut, key: &str, item: Option<toml_edit::Item>) {
    if !key.contains('.') {
        match item {
            Some(item) => insert_top_level(document, key, item),
            None => {
                document.as_table_mut().remove(key);
            }
        }
        return;
    }
    let mut parts: Vec<&str> = key.split('.').collect();
    let Some(last) = parts.pop() else { return };
    let mut table: &mut dyn toml_edit::TableLike = document.as_table_mut();
    for part in parts {
        if !table.contains_key(part) {
            if item.is_none() {
                return;
            }
            table.insert(part, toml_edit::table());
        }
        table = match table.get_mut(part).and_then(toml_edit::Item::as_table_like_mut) {
            Some(nested) => nested,
            None => return,
        };
    }
    match item {
        Some(item) => {
            table.insert(last, item);
        }
        None => {
            table.remove(last);
        }
    }
}

/// Inserts a top-level value. The first one in a file that has only tables would be
/// written above the file's leading comments, which belong to its first table. It
/// takes those comments over instead, so it sits below them, next to its commented example.
fn insert_top_level(document: &mut toml_edit::DocumentMut, key: &str, item: toml_edit::Item) {
    let root = document.as_table_mut();
    let first_value = !root.iter().any(|(_, item)| item.is_value());
    root.insert(key, item);
    if !first_value {
        return;
    }
    let first_table =
        root.iter().filter_map(|(name, item)| Some((item.as_table()?.position()?, name.to_owned()))).min();
    let Some((_, name)) = first_table else { return };
    let Some(table) = root.get_mut(&name).and_then(toml_edit::Item::as_table_mut) else { return };
    let Some(prefix) = table.decor().prefix().and_then(|prefix| prefix.as_str()).map(|prefix| {
        // No blank line between the comments and the value.
        format!("{}\n", prefix.trim_end_matches('\n'))
    }) else {
        return;
    };
    table.decor_mut().set_prefix("\n");
    if let Some(mut key) = root.key_mut(key) {
        key.leaf_decor_mut().set_prefix(prefix);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_paths(name: &str) -> Paths {
        let dir = std::env::temp_dir().join(format!("tron-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Paths::with_dirs(dir.clone(), dir.join("data"))
    }

    #[test]
    fn values_step_within_their_kind() {
        let settings = list(&["monospace".into(), "Iosevka".into()], Some(60));
        let find = |key| settings.iter().find(|s| s.key == key).unwrap();
        let size = find("font.size");
        assert_eq!(step(size, &toml::Value::Float(12.0), 1), toml::Value::Float(12.5));
        assert_eq!(step(size, &toml::Value::Float(40.0), 3), toml::Value::Float(40.0));
        let opacity = find("window.opacity");
        assert_eq!(step(opacity, &toml::Value::Float(0.95), 1), toml::Value::Float(1.0));
        assert_eq!(step(find("font.ligatures"), &toml::Value::Boolean(true), 1), toml::Value::Boolean(false));
        let shape = find("cursor.shape");
        assert_eq!(step(shape, &toml::Value::String("block".into()), -1), toml::Value::String("underline".into()));
        assert_eq!(
            step(find("font.family"), &toml::Value::String("Iosevka".into()), 1),
            toml::Value::String("monospace".into())
        );
        assert_eq!(step(find("scrollback.lines"), &toml::Value::Integer(10_000), -1), toml::Value::Integer(9_000));
        let fps = find("shader.fps");
        assert_eq!(fps.kind, choice(&[DISPLAY_RATE, "60", "30", "15"]), "up to the fastest monitor");
        assert_eq!(step(fps, &toml::Value::String(DISPLAY_RATE.into()), 2), toml::Value::String("30".into()));
        assert_eq!(fps_choices(Some(100)), choice(&[DISPLAY_RATE, "100", "90", "75", "60", "30", "15"]));
        assert_eq!(display(&toml::Value::Float(0.85)), "0.85");
        assert_eq!(display(&toml::Value::Float(12.0)), "12");
    }

    #[test]
    fn overlay_holds_every_previewable_value() {
        let mut choices = Choices::from_config(&Config::default());
        choices.settings.insert("font.size", toml::Value::Float(16.5));
        choices.settings.insert("startup", toml::Value::String(NEVER.into()));
        choices.settings.insert("shader.fps", toml::Value::String("30".into()));
        choices.settings.insert("shader.animation", toml::Value::String("always".into()));
        let text = choices.overlay("nord", &["crt.wgsl".into()]);
        let config: Config = toml::from_str(&text).unwrap();
        assert_eq!((config.theme.as_deref(), config.font.size), (Some("nord"), 16.5));
        assert_eq!(config.shader.files, ["crt.wgsl"]);
        assert_eq!(config.shader.fps, 30);
        assert_eq!(config.shader.animation, tron_config::Animation::Always);
        choices.settings.insert("shader.fps", toml::Value::String(DISPLAY_RATE.into()));
        let config: Config = toml::from_str(&choices.overlay("nord", &[])).unwrap();
        assert_eq!(config.shader.fps, 0, "the display rate writes no key");
        assert_eq!(config.startup, None, "startup is not previewed");
    }

    #[test]
    fn saving_into_the_example_config_keeps_its_header_and_comments() {
        let paths = temp_paths("example");
        assert!(paths.create_config().unwrap());
        assert!(!paths.create_config().unwrap(), "an existing file stays");
        let config = Config::load(&paths).unwrap();
        let saved = Choices::from_config(&config);
        let mut choices = saved.clone();
        choices.theme = "nord".into();
        choices.shaders = vec!["crt.wgsl".into(), "bloom.wgsl".into()];
        choices.settings.insert("font.size", toml::Value::Float(14.0));
        choices.save(&saved, &paths).unwrap();
        let text = std::fs::read_to_string(&paths.config_file).unwrap();
        assert!(text.starts_with("# tron configuration."), "{text}");
        assert!(text.contains("# theme = \"tron-light\"\ntheme = \"nord\"\n\n[font]"), "{text}");
        assert!(text.contains("# Theme from themes/<name>.toml"), "{text}");
        let written = Config::load(&paths).unwrap();
        assert_eq!(Choices::from_config(&written), choices);
    }

    #[test]
    fn saving_changes_keeps_the_rest_of_the_file() {
        let paths = temp_paths("save");
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(&paths.config_file, "# my settings\n[font]\nfamily = \"Iosevka\" # favorite\nsize = 12.0\n")
            .unwrap();
        let config = Config::load(&paths).unwrap();
        let saved = Choices::from_config(&config);
        let mut choices = saved.clone();
        choices.theme = "dracula".into();
        choices.shaders = vec!["bloom.wgsl".into()];
        choices.settings.insert("font.size", toml::Value::Float(14.0));
        choices.settings.insert("window.opacity", toml::Value::Float(0.9));
        choices.settings.insert("startup", toml::Value::String(NEVER.into()));
        let changes = choices.changes(&saved, &list(&[], None));
        assert!(changes.contains(&("Font size".into(), "12".into(), "14".into())), "{changes:?}");
        assert_eq!(changes.len(), 5, "{changes:?}");

        choices.save(&saved, &paths).unwrap();
        let text = std::fs::read_to_string(&paths.config_file).unwrap();
        assert!(text.contains("# my settings") && text.contains("# favorite"), "{text}");
        let written = Config::load(&paths).unwrap();
        assert_eq!(Choices::from_config(&written), choices);
        assert_eq!(written.startup, Some(false));

        // Back to the first launch default removes the key again.
        let mut again = choices.clone();
        again.settings.insert("startup", toml::Value::String(FIRST_LAUNCH.into()));
        again.save(&choices, &paths).unwrap();
        assert_eq!(Config::load(&paths).unwrap().startup, None);
        let _ = std::fs::remove_dir_all(&paths.config_dir);
    }
}
