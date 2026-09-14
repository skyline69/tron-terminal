//! Embeds every theme in `examples/themes` and shader in `examples/shaders`.

use std::fmt::Write;
use std::path::{Path, PathBuf};

/// Listed first, in this order; everything else follows by name.
const FIRST_THEMES: [&str; 1] = ["tron-light"];
const FIRST_SHADERS: [&str; 5] = ["crt.wgsl", "bloom.wgsl", "cursor-glow.wgsl", "cursor-trail.wgsl", "afterglow.wgsl"];

fn files(dir: &Path, extension: &str, first: &[&str]) -> Vec<(String, PathBuf)> {
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut files: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("reading {}: {error}", dir.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == extension))
        .map(|path| (path.file_name().unwrap().to_string_lossy().into_owned(), path))
        .collect();
    files.sort_by(|(a, _), (b, _)| {
        let rank = |name: &str| first.iter().position(|f| *f == name).unwrap_or(first.len());
        rank(a).cmp(&rank(b)).then_with(|| a.cmp(b))
    });
    files
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut out = String::new();
    out.push_str("/// Themes shipped with tron, as (name, TOML). \"tron\" is the default colors.\n");
    out.push_str("pub const BUILTIN_THEMES: &[(&str, &str)] = &[\n    (\"tron\", \"\"),\n");
    for (name, path) in files(&root.join("themes"), "toml", &FIRST_THEMES) {
        let name = name.trim_end_matches(".toml");
        writeln!(out, "    ({name:?}, include_str!({:?})),", path.canonicalize().unwrap()).unwrap();
    }
    out.push_str("];\n\n");
    out.push_str("/// Shaders shipped with tron, as (file name, WGSL). Files in `shaders/` with the\n");
    out.push_str("/// same name take precedence.\npub const BUILTIN_SHADERS: &[(&str, &str)] = &[\n");
    for (name, path) in files(&root.join("shaders"), "wgsl", &FIRST_SHADERS) {
        writeln!(out, "    ({name:?}, include_str!({:?})),", path.canonicalize().unwrap()).unwrap();
    }
    out.push_str("];\n");
    std::fs::write(Path::new(&std::env::var("OUT_DIR").unwrap()).join("builtin.rs"), out).unwrap();
}
