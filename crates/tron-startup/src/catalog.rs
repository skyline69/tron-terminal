//! Themes and shaders the startup screen offers, with what it needs to show them.

use std::path::PathBuf;

use tron_config::{BUILTIN_SHADERS, Colors, Config, Paths};

pub struct Theme {
    pub name: String,
    pub colors: Colors,
    pub builtin: bool,
}

pub struct Shader {
    /// File name, as written in `[shader] files`.
    pub file: String,
    /// First comment line of the source.
    pub description: String,
    pub builtin: bool,
}

pub struct Catalog {
    pub paths: Option<Paths>,
    pub themes: Vec<Theme>,
    pub shaders: Vec<Shader>,
}

impl Catalog {
    /// Reads the catalog for tron's configuration directory, as passed by tron.
    pub fn load() -> Self {
        let paths = match (std::env::var_os(crate::CONFIG_DIR_ENV), Paths::discover()) {
            (Some(dir), Some(found)) => Some(Paths::with_dirs(PathBuf::from(dir), found.data_dir)),
            (Some(dir), None) => Some(Paths::with_dirs(PathBuf::from(&dir), PathBuf::from(dir).join("data"))),
            (None, found) => found,
        };
        Self::for_paths(paths)
    }

    pub fn for_paths(paths: Option<Paths>) -> Self {
        let builtin_themes: Vec<&str> = tron_config::BUILTIN_THEMES.iter().map(|(name, _)| *name).collect();
        let themes = tron_config::theme_names(paths.as_ref())
            .into_iter()
            .filter_map(|name| {
                let config = Config { theme: Some(name.clone()), ..Config::default() };
                let colors = config.colors(paths.as_ref()).ok()?;
                Some(Theme { builtin: builtin_themes.contains(&name.as_str()), name, colors })
            })
            .collect();
        let shaders = tron_config::shader_names(paths.as_ref())
            .into_iter()
            .map(|file| {
                let config = Config {
                    shader: tron_config::ShaderConfig { files: vec![file.clone()], ..Default::default() },
                    ..Config::default()
                };
                let source = config.shader_sources(paths.as_ref()).pop().and_then(Result::ok).map(|s| s.source);
                let description = source
                    .as_deref()
                    .and_then(|s| s.lines().find_map(|line| line.trim().strip_prefix("//")))
                    .map(|line| line.trim().to_owned())
                    .unwrap_or_default();
                let builtin = BUILTIN_SHADERS.iter().any(|(name, _)| *name == file)
                    && paths.as_ref().is_none_or(|p| !p.shaders_dir.join(&file).exists());
                Shader { file, description, builtin }
            })
            .collect();
        Self { paths, themes, shaders }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_builtin_and_user_themes_and_shaders() {
        let dir = std::env::temp_dir().join(format!("tron-catalog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = Paths::with_dirs(dir.clone(), dir.join("data"));
        std::fs::create_dir_all(&paths.themes_dir).unwrap();
        std::fs::create_dir_all(&paths.shaders_dir).unwrap();
        std::fs::write(paths.themes_dir.join("paper.toml"), "background = \"#ffffff\"").unwrap();
        std::fs::write(paths.shaders_dir.join("wobble.wgsl"), "// Wobbles.\nfn shade() {}").unwrap();
        let catalog = Catalog::for_paths(Some(paths));
        let paper = catalog.themes.iter().find(|t| t.name == "paper").unwrap();
        assert!(!paper.builtin && paper.colors.background.to_array() == [255, 255, 255]);
        assert!(catalog.themes.iter().any(|t| t.name == "dracula" && t.builtin));
        let crt = catalog.shaders.iter().find(|s| s.file == "crt.wgsl").unwrap();
        assert!(crt.builtin && crt.description.starts_with("CRT look"));
        assert_eq!(catalog.shaders.last().map(|s| s.description.as_str()), Some("Wobbles."));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
