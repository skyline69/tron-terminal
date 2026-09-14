//! User configuration.
//!
//! The config directory is located with `etcetera`: `$XDG_CONFIG_HOME/tron` on
//! Linux, usually `~/.config/tron`. `TRON_CONFIG_DIR` overrides it.
//!
//! ```text
//! ~/.config/tron/
//!   config.toml           main configuration
//!   themes/<name>.toml    color themes, selected with `theme = "<name>"`
//!   shaders/<name>.wgsl   post-processing shaders, listed in `[shader] files`
//! ```
//!
//! Every field has a default, so an empty or missing file is valid.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use etcetera::{AppStrategy, AppStrategyArgs};
use serde::{Deserialize, Deserializer};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid config {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },
    #[error("theme `{0}` not found")]
    ThemeNotFound(String),
}

/// Locations of configuration and data files.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub themes_dir: PathBuf,
    pub shaders_dir: PathBuf,
    /// Generated data, such as the compiled terminfo entry.
    pub data_dir: PathBuf,
}

impl Paths {
    pub fn discover() -> Option<Self> {
        let strategy = etcetera::choose_app_strategy(AppStrategyArgs {
            top_level_domain: "dev".into(),
            author: "tron".into(),
            app_name: "tron".into(),
        })
        .ok()?;
        let config_dir = std::env::var_os("TRON_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| strategy.config_dir());
        Some(Self::with_dirs(config_dir, strategy.data_dir()))
    }

    pub fn with_dirs(config_dir: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            config_file: config_dir.join("config.toml"),
            themes_dir: config_dir.join("themes"),
            shaders_dir: config_dir.join("shaders"),
            config_dir,
            data_dir,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Name of a theme in `themes/`, or a built-in theme.
    pub theme: Option<String>,
    pub font: FontConfig,
    pub window: WindowConfig,
    pub cursor: CursorConfig,
    /// Overrides applied on top of the theme.
    pub colors: ColorOverrides,
    pub scrollback: ScrollbackConfig,
    pub shell: ShellConfig,
    pub selection: SelectionConfig,
    pub clipboard: ClipboardConfig,
    pub shader: ShaderConfig,
    pub images: ImageConfig,
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Loads `config.toml`. A missing file yields the defaults.
    pub fn load(paths: &Paths) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(&paths.config_file) {
            Ok(text) => Self::parse(&text).map_err(|source| ConfigError::Parse {
                path: paths.config_file.clone(),
                source: Box::new(source),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Read { path: paths.config_file.clone(), source }),
        }
    }

    /// Final colors: built-in defaults, then the theme, then `[colors]`.
    pub fn colors(&self, paths: Option<&Paths>) -> Result<Colors, ConfigError> {
        let mut colors = Colors::default();
        if let Some(name) = &self.theme
            && name != "tron"
        {
            let path = paths
                .map(|p| p.themes_dir.join(format!("{name}.toml")))
                .filter(|p| p.exists())
                .ok_or_else(|| ConfigError::ThemeNotFound(name.clone()))?;
            let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Read { path: path.clone(), source })?;
            let theme: ColorOverrides =
                toml::from_str(&text).map_err(|source| ConfigError::Parse { path, source: Box::new(source) })?;
            theme.apply(&mut colors);
        }
        self.colors.apply(&mut colors);
        Ok(colors)
    }

    /// Reads the shader files listed in `[shader] files`, in order.
    pub fn shader_sources(&self, paths: Option<&Paths>) -> Vec<Result<ShaderSource, ConfigError>> {
        self.shader
            .files
            .iter()
            .map(|file| {
                let path = PathBuf::from(file);
                let path = match paths {
                    Some(paths) if path.is_relative() => paths.shaders_dir.join(path),
                    _ => path,
                };
                let source = std::fs::read_to_string(&path).map_err(|source| ConfigError::Read { path: path.clone(), source })?;
                Ok(ShaderSource { name: file.clone(), path, source })
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct ShaderSource {
    pub name: String,
    pub path: PathBuf,
    pub source: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FontConfig {
    /// Family name, or a generic family such as `monospace`.
    pub family: String,
    /// Size in points.
    pub size: f32,
    /// Families tried, in order, before system fallback.
    pub fallback: Vec<String>,
    /// OpenType features, for example `["ss01", "-calt"]`.
    pub features: Vec<String>,
    /// Programming ligatures. `false` disables `calt`, `liga` and `dlig`.
    pub ligatures: bool,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: "monospace".into(),
            size: 12.0,
            fallback: Vec::new(),
            features: Vec::new(),
            ligatures: true,
        }
    }
}

impl FontConfig {
    /// Features to pass to the shaper.
    pub fn shaping_features(&self) -> Vec<String> {
        let mut features = self.features.clone();
        if !self.ligatures {
            features.extend(["-calt", "-liga", "-dlig"].map(String::from));
        }
        features
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WindowConfig {
    pub title: String,
    /// Horizontal padding in logical pixels.
    pub padding_x: u16,
    /// Vertical padding in logical pixels.
    pub padding_y: u16,
    /// Background opacity from 0.0 to 1.0. Changing it needs a restart.
    pub opacity: f32,
    pub decorations: bool,
    pub columns: u16,
    pub rows: u16,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: "tron".into(),
            padding_x: 10,
            padding_y: 8,
            opacity: 1.0,
            decorations: true,
            columns: 100,
            rows: 30,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorShape {
    #[default]
    Block,
    Beam,
    Underline,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CursorConfig {
    pub shape: CursorShape,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScrollbackConfig {
    pub lines: usize,
    /// Lines scrolled per mouse wheel step.
    pub multiplier: f32,
}

impl Default for ScrollbackConfig {
    fn default() -> Self {
        Self { lines: 10_000, multiplier: 3.0 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShellConfig {
    /// Defaults to `$SHELL`.
    pub program: Option<String>,
    pub args: Vec<String>,
    /// Value of `TERM`. `xterm-tron` uses the bundled terminfo entry.
    pub term: String,
    pub env: BTreeMap<String, String>,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self { program: None, args: Vec::new(), term: "xterm-tron".into(), env: BTreeMap::new() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SelectionConfig {
    /// Characters that end a word on double click, besides whitespace.
    pub word_separators: String,
    /// Copy selections to the primary selection when released.
    pub copy_on_select: bool,
}

impl Default for SelectionConfig {
    fn default() -> Self {
        Self { word_separators: ",│`|:\"'()[]{}<>".into(), copy_on_select: true }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Osc52 {
    Disabled,
    /// Applications may set the clipboard.
    #[default]
    Copy,
    /// Applications may also read the clipboard.
    CopyPaste,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClipboardConfig {
    pub osc52: Osc52,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Animation {
    /// Animate when a shader reads `tron.time` or `tron.frame`.
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShaderConfig {
    /// WGSL files, relative to `shaders/` or absolute. Applied in order.
    pub files: Vec<String>,
    pub animation: Animation,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImageConfig {
    /// Memory limit for decoded images, in MiB.
    pub memory_limit: u32,
    /// Allow applications to send images as file paths.
    pub file_transfer: bool,
}

impl Default for ImageConfig {
    fn default() -> Self {
        Self { memory_limit: 320, file_transfer: true }
    }
}

/// A color written as `"#rrggbb"`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    const fn hex(value: u32) -> Self {
        Self::new((value >> 16) as u8, (value >> 8) as u8, value as u8)
    }

    pub fn parse(text: &str) -> Option<Self> {
        let hex = text.strip_prefix('#').unwrap_or(text);
        if hex.len() != 6 {
            return None;
        }
        u32::from_str_radix(hex, 16).ok().map(Self::hex)
    }

    pub const fn to_array(self) -> [u8; 3] {
        [self.r, self.g, self.b]
    }
}

impl fmt::Debug for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid color `{text}`, expected #rrggbb")))
    }
}

/// Resolved colors.
#[derive(Debug, Clone, PartialEq)]
pub struct Colors {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    /// Text color under a block cursor. Defaults to the background.
    pub cursor_text: Option<Rgb>,
    pub selection_background: Rgb,
    /// Text color inside selections. Defaults to the cell's own color.
    pub selection_foreground: Option<Rgb>,
    /// ANSI colors 0-7.
    pub normal: [Rgb; 8],
    /// ANSI colors 8-15.
    pub bright: [Rgb; 8],
}

impl Default for Colors {
    /// The built-in "tron" theme.
    fn default() -> Self {
        Self {
            foreground: Rgb::hex(0xc7d5e0),
            background: Rgb::hex(0x0a0e14),
            cursor: Rgb::hex(0x4fd6ff),
            cursor_text: None,
            selection_background: Rgb::hex(0x1f4a6b),
            selection_foreground: None,
            normal: [
                Rgb::hex(0x1b2230),
                Rgb::hex(0xff5c75),
                Rgb::hex(0x5ce6a6),
                Rgb::hex(0xffc86b),
                Rgb::hex(0x4f9dff),
                Rgb::hex(0xc38bff),
                Rgb::hex(0x4fd6ff),
                Rgb::hex(0xc7d5e0),
            ],
            bright: [
                Rgb::hex(0x4a5568),
                Rgb::hex(0xff8095),
                Rgb::hex(0x86f0bf),
                Rgb::hex(0xffd99a),
                Rgb::hex(0x7fb8ff),
                Rgb::hex(0xd6adff),
                Rgb::hex(0x8ae6ff),
                Rgb::hex(0xeef4f8),
            ],
        }
    }
}

impl Colors {
    /// The 16 ANSI colors as byte triples.
    pub fn ansi(&self) -> [[u8; 3]; 16] {
        let mut ansi = [[0u8; 3]; 16];
        for i in 0..8 {
            ansi[i] = self.normal[i].to_array();
            ansi[i + 8] = self.bright[i].to_array();
        }
        ansi
    }
}

/// Color settings where every field is optional. Used for themes and `[colors]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ColorOverrides {
    pub foreground: Option<Rgb>,
    pub background: Option<Rgb>,
    pub cursor: Option<Rgb>,
    pub cursor_text: Option<Rgb>,
    pub selection_background: Option<Rgb>,
    pub selection_foreground: Option<Rgb>,
    pub normal: Option<[Rgb; 8]>,
    pub bright: Option<[Rgb; 8]>,
}

impl ColorOverrides {
    pub fn apply(&self, colors: &mut Colors) {
        macro_rules! set {
            ($($field:ident),*) => { $( if let Some(value) = self.$field { colors.$field = value; } )* };
        }
        set!(foreground, background, cursor, selection_background, normal, bright);
        if self.cursor_text.is_some() {
            colors.cursor_text = self.cursor_text;
        }
        if self.selection_foreground.is_some() {
            colors.selection_foreground = self.selection_foreground;
        }
    }
}

/// Watches the config directory and calls back on changes.
pub struct Watcher {
    _watcher: notify::RecommendedWatcher,
}

impl Watcher {
    /// Creates the config directory if needed and starts watching it.
    pub fn new(paths: &Paths, on_change: impl Fn() + Send + 'static) -> Result<Self, notify::Error> {
        std::fs::create_dir_all(&paths.config_dir).map_err(notify::Error::io)?;
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event
                && (event.kind.is_modify() || event.kind.is_create() || event.kind.is_remove())
            {
                on_change();
            }
        })?;
        notify::Watcher::watch(&mut watcher, &paths.config_dir, notify::RecursiveMode::Recursive)?;
        Ok(Self { _watcher: watcher })
    }
}

/// Resolves a path relative to the config directory, for display.
pub fn display_path(path: &Path) -> String {
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) if path.starts_with(&home) => format!("~/{}", path.strip_prefix(&home).unwrap_or(path).display()),
        _ => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_paths(name: &str) -> Paths {
        let dir = std::env::temp_dir().join(format!("tron-config-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("themes")).unwrap();
        std::fs::create_dir_all(dir.join("shaders")).unwrap();
        Paths::with_dirs(dir.clone(), dir.join("data"))
    }

    #[test]
    fn empty_config_is_default() {
        let config = Config::parse("").unwrap();
        assert_eq!(config.font.family, "monospace");
        assert!(config.font.ligatures);
    }

    #[test]
    fn partial_config() {
        let config = Config::parse(
            r##"
            [font]
            size = 14.5
            ligatures = false

            [colors]
            background = "#101010"
            "##,
        )
        .unwrap();
        assert_eq!(config.font.size, 14.5);
        assert_eq!(config.font.shaping_features(), ["-calt", "-liga", "-dlig"]);
        let colors = config.colors(None).unwrap();
        assert_eq!(colors.background, Rgb::new(16, 16, 16));
        assert_eq!(colors.foreground, Colors::default().foreground);
    }

    #[test]
    fn rejects_unknown_keys_and_bad_colors() {
        assert!(Config::parse("[font]\nfamliy = \"x\"").is_err());
        assert!(Config::parse("[colors]\nbackground = \"red\"").is_err());
    }

    #[test]
    fn theme_then_overrides_and_shaders() {
        let paths = temp_paths("theme");
        std::fs::write(paths.themes_dir.join("paper.toml"), "background = \"#ffffff\"\nforeground = \"#000000\"").unwrap();
        std::fs::write(paths.shaders_dir.join("crt.wgsl"), "fn shade() {}").unwrap();
        std::fs::write(
            &paths.config_file,
            "theme = \"paper\"\n[colors]\nforeground = \"#333333\"\n[shader]\nfiles = [\"crt.wgsl\"]",
        )
        .unwrap();
        let config = Config::load(&paths).unwrap();
        let colors = config.colors(Some(&paths)).unwrap();
        assert_eq!(colors.background, Rgb::new(255, 255, 255));
        assert_eq!(colors.foreground, Rgb::new(51, 51, 51));
        let shaders = config.shader_sources(Some(&paths));
        assert_eq!(shaders[0].as_ref().unwrap().source, "fn shade() {}");
        assert!(matches!(
            Config::parse("theme = \"missing\"").unwrap().colors(Some(&paths)),
            Err(ConfigError::ThemeNotFound(_))
        ));
        let _ = std::fs::remove_dir_all(&paths.config_dir);
    }
}
