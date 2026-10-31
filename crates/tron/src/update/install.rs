//! Replacing an installed tron with a new release, the way `install.sh` lays
//! it out, and an AppImage with the new AppImage.
//!
//! The installation is found through the manifest `install.sh` writes, by the
//! entry that is the running binary. Files are replaced atomically, so running
//! windows keep working, and only when their contents changed: unchanged icons
//! are not rendered again and the desktop caches are refreshed only when
//! something they cache changed.

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, bail};

use super::release::Unpacked;

const APP_ID: &str = "dev.tron.Terminal";
/// Name of the manifest below the share directory.
const MANIFEST: &str = "tron/install-manifest";
/// Fixed icon sizes rendered from the SVG, as `install.sh` does.
const ICON_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 512];

/// How the running tron was installed.
#[derive(Debug)]
pub enum Target {
    /// By `install.sh`.
    Script(Layout),
    /// As an AppImage at this path.
    AppImage(PathBuf),
}

/// Where `install.sh` put tron.
#[derive(Debug, PartialEq, Eq)]
pub struct Layout {
    pub bin_dir: PathBuf,
    pub share_dir: PathBuf,
    /// `tron.app` on macOS.
    pub app: Option<PathBuf>,
    pub manifest: PathBuf,
    /// What the manifest lists.
    pub entries: Vec<PathBuf>,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|home| !home.is_empty()).map(PathBuf::from)
}

/// `$XDG_DATA_HOME`, or `~/.local/share`: the share directory of a user install.
fn user_share_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| home().map(|home| home.join(".local/share")))
}

/// Finds out how the running tron was installed, or explains why it cannot
/// be updated here.
pub fn locate() -> anyhow::Result<Target> {
    if Path::new("/.flatpak-info").exists() {
        bail!(
            "this tron runs as a Flatpak. Install the new .flatpak bundle from {}/releases/latest, \
             or use flatpak update when it came from a repository",
            env!("CARGO_PKG_REPOSITORY")
        );
    }
    if let Some(appimage) = std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|path| path.is_file()) {
        return Ok(Target::AppImage(appimage));
    }
    let exe = std::env::current_exe().context("cannot find the running tron")?;
    let exe = fs::canonicalize(&exe).unwrap_or(exe);
    let mut candidates: Vec<PathBuf> = user_share_dir().into_iter().collect();
    // PREFIX/bin/tron, with the manifest in PREFIX/share.
    if let Some(prefix) = exe.parent().and_then(Path::parent) {
        candidates.push(prefix.join("share"));
    }
    candidates.push(PathBuf::from("/usr/local/share"));
    if let Some(layout) = locate_in(&exe, &candidates) {
        return Ok(Target::Script(layout));
    }
    let hint = if home().is_some_and(|home| exe.starts_with(home.join(".cargo"))) {
        "It was installed with cargo; update it with cargo install.".to_owned()
    } else if exe.components().any(|part| part.as_os_str() == "target") {
        "It is a development build.".to_owned()
    } else if exe.to_string_lossy().contains("Caskroom") || cfg!(target_os = "macos") {
        "When Homebrew installed it, run: brew upgrade --cask tron".to_owned()
    } else {
        "Update it with the package manager that installed it, or install tron again with install.sh.".to_owned()
    };
    bail!("{} was not installed by install.sh, so tron update cannot replace it. {hint}", exe.display())
}

/// The installation whose manifest, in one of the share directories
/// `candidates`, lists the binary `exe`.
fn locate_in(exe: &Path, candidates: &[PathBuf]) -> Option<Layout> {
    candidates.iter().find_map(|share_dir| {
        let manifest = share_dir.join(MANIFEST);
        let text = fs::read_to_string(&manifest).ok()?;
        let entries: Vec<PathBuf> = text.lines().filter(|line| !line.is_empty()).map(PathBuf::from).collect();
        // The binary, or on macOS the PREFIX/bin/tron link into tron.app.
        let lists_exe =
            entries.iter().any(|entry| entry == exe || fs::canonicalize(entry).is_ok_and(|resolved| resolved == exe));
        if !lists_exe {
            return None;
        }
        let bin = entries.iter().find(|entry| {
            entry.file_name() == Some(OsStr::new("tron"))
                && entry.parent().and_then(Path::file_name) == Some(OsStr::new("bin"))
        })?;
        let app = entries.iter().find(|entry| entry.file_name() == Some(OsStr::new("tron.app"))).cloned();
        Some(Layout { bin_dir: bin.parent()?.to_owned(), share_dir: share_dir.clone(), app, manifest, entries })
    })
}

impl Layout {
    /// The file the binary is written to.
    fn binary(&self) -> PathBuf {
        match &self.app {
            Some(app) => app.join("Contents/MacOS/tron"),
            None => self.bin_dir.join("tron"),
        }
    }

    /// A user install, under `~/.local`, as opposed to a system or prefix one.
    fn is_user(&self) -> bool {
        home().is_some_and(|home| self.bin_dir == home.join(".local/bin"))
    }

    /// Whether this user can write every directory an update writes to.
    pub fn writable(&self) -> bool {
        let binary = self.binary();
        [binary.parent(), Some(self.share_dir.as_path()), Some(self.bin_dir.as_path())]
            .into_iter()
            .flatten()
            .all(dir_writable)
    }
}

/// Whether files can be created in `dir`, or in its nearest existing parent.
fn dir_writable(dir: &Path) -> bool {
    let Some(existing) = dir.ancestors().find(|dir| dir.exists()) else { return false };
    let probe = existing.join(format!(".tron-update-{}", std::process::id()));
    match fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => fs::remove_file(&probe).is_ok(),
        Err(_) => false,
    }
}

/// Writes `data` to `dest` through a temporary file renamed over it, so a
/// running copy keeps its file. Returns false, writing nothing, when `dest`
/// already holds `data` with mode `mode`.
fn write_file(dest: &Path, data: &[u8], mode: u32) -> io::Result<bool> {
    if fs::metadata(dest).is_ok_and(|metadata| metadata.permissions().mode() & 0o777 == mode)
        && same_contents(dest, data)
    {
        return Ok(false);
    }
    let temporary = staged(dest);
    stage(&temporary, data, mode)?;
    fs::rename(&temporary, dest).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })?;
    Ok(true)
}

/// Whether the file at `path` holds `data`. The size is compared first, so a
/// changed binary is not read.
fn same_contents(path: &Path, data: &[u8]) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.len() == data.len() as u64)
        && fs::read(path).is_ok_and(|existing| existing == data)
}

/// `dest.new`, where a file is written before it replaces `dest`.
fn staged(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_owned();
    name.push(".new");
    dest.with_file_name(name)
}

fn stage(path: &Path, data: &[u8], mode: u32) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let _ = fs::remove_file(path);
    let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(path)?;
    file.write_all(data)?;
    file.set_permissions(fs::Permissions::from_mode(mode))
}

/// Whether `program` is on `PATH`.
fn has(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            fs::metadata(dir.join(program))
                .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        })
    })
}

/// Runs a command quietly; failures are only logged, as for `|| true` in install.sh.
fn run_quietly(program: &str, args: &[&OsStr]) {
    match Command::new(program).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status() {
        Ok(status) if !status.success() => log::debug!("{program} failed: {status}"),
        Err(error) => log::debug!("cannot run {program}: {error}"),
        Ok(_) => {}
    }
}

/// The version `binary --version` reports, or why it does not run.
fn binary_version(binary: &Path) -> anyhow::Result<String> {
    let output = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("cannot run {}", binary.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    match stdout.lines().next().and_then(|line| line.split_whitespace().last()) {
        Some(version) if output.status.success() => Ok(version.to_owned()),
        _ => bail!("{}", String::from_utf8_lossy(&output.stderr).trim()),
    }
}

/// A new binary written beside the one it replaces, checked to run here.
struct StagedBinary {
    temporary: PathBuf,
    dest: PathBuf,
}

impl StagedBinary {
    /// Writes `data` beside `dest` and checks it runs: a binary that cannot
    /// start here, for a missing library or an older C library, is never
    /// installed. `None` when `dest` already holds `data`.
    fn new(dest: &Path, data: &[u8], version: &str) -> anyhow::Result<Option<Self>> {
        if same_contents(dest, data) {
            return Ok(None);
        }
        let temporary = staged(dest);
        stage(&temporary, data, 0o755).with_context(|| format!("cannot write {}", temporary.display()))?;
        match binary_version(&temporary) {
            Ok(reported) if reported != version => {
                log::warn!("the new binary reports version {reported}, not {version}")
            }
            Ok(_) => {}
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                bail!("the new tron does not run on this system: {error}");
            }
        }
        Ok(Some(Self { temporary, dest: dest.to_owned() }))
    }

    /// Moves the binary into place, flushed to disk first so a crash cannot
    /// leave an empty one behind.
    fn commit(self) -> anyhow::Result<()> {
        fs::File::open(&self.temporary)
            .and_then(|file| file.sync_all())
            .and_then(|()| fs::rename(&self.temporary, &self.dest))
            .with_context(|| format!("cannot replace {}", self.dest.display()))
    }
}

impl Drop for StagedBinary {
    fn drop(&mut self) {
        // Left over when the update failed before the commit.
        let _ = fs::remove_file(&self.temporary);
    }
}

/// What an update changed.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Files written.
    pub written: usize,
    /// Files that already held the new contents.
    pub unchanged: usize,
}

impl Outcome {
    fn count(&mut self, changed: bool) {
        if changed { self.written += 1 } else { self.unchanged += 1 }
    }
}

/// Installs the unpacked release `files` of tron `version` into `layout`.
pub fn install(layout: &Layout, files: &Unpacked, version: &str) -> anyhow::Result<Outcome> {
    let started = std::time::Instant::now();
    let binary = layout.binary();
    // Before anything changes: a binary that does not run stops the update here.
    let new_binary = StagedBinary::new(&binary, files.get("tron")?, version)?;
    let binary_changed = new_binary.is_some();
    log::debug!("binary checked in {:?}", started.elapsed());
    let mut outcome = Outcome::default();
    outcome.count(binary_changed);
    let mut entries = Vec::new();
    if let Some(app) = &layout.app {
        // The bundle is signed again once the binary is in it.
        if let Some(new_binary) = new_binary {
            new_binary.commit()?;
        }
        update_app(app, version, binary_changed)?;
        entries.push(app.clone());
        let link = layout.bin_dir.join("tron");
        if fs::read_link(&link).ok().as_deref() != Some(binary.as_path()) {
            fs::create_dir_all(&layout.bin_dir)?;
            let temporary = staged(&link);
            let _ = fs::remove_file(&temporary);
            std::os::unix::fs::symlink(&binary, &temporary)?;
            fs::rename(&temporary, &link)?;
        }
        entries.push(link);
        install_completions(layout, &binary, &mut outcome, &mut entries)?;
    } else {
        entries.push(binary.clone());
        // The binary is flushed to disk and asked for its completions while
        // the desktop files and icons are written.
        let (binary_part, files_part) = std::thread::scope(|scope| {
            let job = scope.spawn(|| -> anyhow::Result<(Outcome, Vec<PathBuf>)> {
                if let Some(new_binary) = new_binary {
                    new_binary.commit()?;
                }
                let (mut outcome, mut entries) = (Outcome::default(), Vec::new());
                install_completions(layout, &binary, &mut outcome, &mut entries)?;
                Ok((outcome, entries))
            });
            let mut part = (Outcome::default(), Vec::new());
            let files_part = install_linux_files(layout, files, &mut part.0, &mut part.1).map(|()| part);
            (job.join().unwrap_or_else(|_| Err(anyhow::anyhow!("installing the binary panicked"))), files_part)
        });
        for part in [files_part?, binary_part?] {
            outcome.written += part.0.written;
            outcome.unchanged += part.0.unchanged;
            entries.extend(part.1);
        }
    }
    write_manifest(layout, entries)?;
    log::debug!("installed in {:?}", started.elapsed());
    Ok(outcome)
}

/// The desktop entry, AppStream metadata and icons, as install.sh installs them.
fn install_linux_files(
    layout: &Layout,
    files: &Unpacked,
    outcome: &mut Outcome,
    entries: &mut Vec<PathBuf>,
) -> anyhow::Result<()> {
    let share = &layout.share_dir;
    let mut cached_changed = false;
    let desktop = desktop_entry(
        &String::from_utf8_lossy(files.get(&format!("dist/{APP_ID}.desktop"))?),
        &layout.bin_dir.join("tron"),
    );
    let mut put = |dest: PathBuf, data: &[u8]| -> anyhow::Result<bool> {
        let changed = write_file(&dest, data, 0o644).with_context(|| format!("cannot write {}", dest.display()))?;
        outcome.count(changed);
        entries.push(dest);
        Ok(changed)
    };
    cached_changed |= put(share.join(format!("applications/{APP_ID}.desktop")), desktop.as_bytes())?;
    put(share.join(format!("metainfo/{APP_ID}.metainfo.xml")), files.get(&format!("dist/{APP_ID}.metainfo.xml"))?)?;
    let icons = share.join("icons/hicolor");
    let svg = icons.join(format!("scalable/apps/{APP_ID}.svg"));
    let svg_changed = put(svg.clone(), files.get(&format!("dist/{APP_ID}.svg"))?)?;
    cached_changed |= svg_changed;
    cached_changed |= put(icons.join(format!("256x256/apps/{APP_ID}.png")), files.get(&format!("dist/{APP_ID}.png"))?)?;

    let sized = |size: u32| icons.join(format!("{size}x{size}/apps/{APP_ID}.png"));
    let missing = ICON_SIZES.iter().any(|&size| !sized(size).exists());
    if svg_changed || missing {
        // Rendering takes a moment per size, so all sizes render at once.
        let (svg, sized) = (&svg, &sized);
        let rendered: Vec<bool> = std::thread::scope(|scope| {
            let jobs: Vec<_> =
                ICON_SIZES.iter().map(|&size| scope.spawn(move || render_icon(svg, size, &sized(size)))).collect();
            jobs.into_iter().map(|job| job.join().unwrap_or(false)).collect()
        });
        for (&size, rendered) in ICON_SIZES.iter().zip(rendered) {
            if rendered {
                outcome.written += 1;
                cached_changed = true;
            }
            if sized(size).exists() {
                entries.push(sized(size));
            }
        }
    } else {
        outcome.unchanged += ICON_SIZES.len();
        entries.extend(ICON_SIZES.iter().map(|&size| sized(size)));
    }
    if cached_changed {
        let started = std::time::Instant::now();
        refresh_linux_caches(share);
        log::debug!("desktop caches refreshed in {:?}", started.elapsed());
    }
    Ok(())
}

/// The desktop entry with the binary's full path, since launchers do not
/// always have `~/.local/bin` on `PATH`.
fn desktop_entry(template: &str, binary: &Path) -> String {
    let path = binary.to_string_lossy();
    let exec = if path.contains(char::is_whitespace) { format!("\"{path}\"") } else { path.clone().into_owned() };
    let mut text = String::with_capacity(template.len() + 64);
    for line in template.lines() {
        if let Some(rest) = line.strip_prefix("Exec=tron") {
            text.push_str(&format!("Exec={exec}{rest}"));
        } else if let Some(rest) = line.strip_prefix("TryExec=tron") {
            text.push_str(&format!("TryExec={path}{rest}"));
        } else {
            text.push_str(line);
        }
        text.push('\n');
    }
    text
}

/// Renders the SVG icon at `size` pixels into `dest`. False when no renderer
/// is installed or it failed.
fn render_icon(svg: &Path, size: u32, dest: &Path) -> bool {
    let temporary = staged(dest);
    if let Some(dir) = dest.parent()
        && fs::create_dir_all(dir).is_err()
    {
        return false;
    }
    let size_text = size.to_string();
    let status = if has("rsvg-convert") {
        Command::new("rsvg-convert")
            .args(["-w", &size_text, "-h", &size_text])
            .arg(svg)
            .arg("-o")
            .arg(&temporary)
            .stderr(Stdio::null())
            .status()
    } else if has("magick") {
        Command::new("magick")
            .args(["-background", "none", "-density", "384"])
            .arg(svg)
            .args(["-resize", &format!("{size}x{size}")])
            .arg(&temporary)
            .stderr(Stdio::null())
            .status()
    } else {
        return false;
    };
    if status.is_ok_and(|status| status.success()) && fs::rename(&temporary, dest).is_ok() {
        return true;
    }
    let _ = fs::remove_file(&temporary);
    false
}

/// Lets menus, KRunner, GNOME search and software centers pick up new files.
fn refresh_linux_caches(share: &Path) {
    let applications = share.join("applications");
    let icons = share.join("icons/hicolor");
    std::thread::scope(|scope| {
        if has("update-desktop-database") {
            scope.spawn(|| run_quietly("update-desktop-database", &[OsStr::new("-q"), applications.as_os_str()]));
        }
        if has("gtk-update-icon-cache") {
            scope.spawn(|| {
                run_quietly(
                    "gtk-update-icon-cache",
                    &[OsStr::new("-q"), OsStr::new("-t"), OsStr::new("-f"), icons.as_os_str()],
                )
            });
        }
        if let Some(kde) = ["kbuildsycoca6", "kbuildsycoca5"].into_iter().find(|program| has(program)) {
            scope.spawn(move || run_quietly(kde, &[]));
        }
    });
}

/// Brings tron.app's Info.plist to `version` and signs the bundle again, as
/// a changed binary breaks its ad-hoc signature.
fn update_app(app: &Path, version: &str, binary_changed: bool) -> anyhow::Result<()> {
    let plist_path = app.join("Contents/Info.plist");
    let mut plist_changed = false;
    if let Ok(plist) = fs::read_to_string(&plist_path) {
        let updated = ["CFBundleShortVersionString", "CFBundleVersion"]
            .iter()
            .fold(plist.clone(), |plist, key| set_plist_string(&plist, key, version));
        if updated != plist {
            write_file(&plist_path, updated.as_bytes(), 0o644)
                .with_context(|| format!("cannot write {}", plist_path.display()))?;
            plist_changed = true;
        }
    }
    if !binary_changed && !plist_changed {
        return Ok(());
    }
    if has("codesign") {
        let status = Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(app)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !status.is_ok_and(|status| status.success()) {
            log::warn!("ad-hoc signing {} failed", app.display());
        }
    }
    let lsregister =
        "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";
    if Path::new(lsregister).exists() {
        run_quietly(lsregister, &[OsStr::new("-f"), app.as_os_str()]);
    }
    Ok(())
}

/// `plist` with the string value of `key` set to `value`.
fn set_plist_string(plist: &str, key: &str, value: &str) -> String {
    let marker = format!("<key>{key}</key>");
    let Some(key_at) = plist.find(&marker) else { return plist.to_owned() };
    let after_key = key_at + marker.len();
    let Some(open) = plist[after_key..].find("<string>").map(|at| after_key + at + "<string>".len()) else {
        return plist.to_owned();
    };
    // Only the element right after the key, with nothing but space between.
    if !plist[after_key..open - "<string>".len()].trim().is_empty() {
        return plist.to_owned();
    }
    let Some(close) = plist[open..].find("</string>").map(|at| open + at) else { return plist.to_owned() };
    format!("{}{value}{}", &plist[..open], &plist[close..])
}

/// Shell completions from the new binary, where install.sh puts them.
fn install_completions(
    layout: &Layout,
    binary: &Path,
    outcome: &mut Outcome,
    entries: &mut Vec<PathBuf>,
) -> anyhow::Result<()> {
    let share = &layout.share_dir;
    let mut targets = vec![("bash", share.join("bash-completion/completions/tron"))];
    if has("zsh") {
        targets.push(("zsh", share.join("zsh/site-functions/_tron")));
    }
    if has("fish") {
        let dir = match (layout.is_user(), std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from), home()) {
            (true, Some(config), _) if config.is_absolute() => config.join("fish/completions"),
            (true, _, Some(home)) => home.join(".config/fish/completions"),
            _ => share.join("fish/vendor_completions.d"),
        };
        targets.push(("fish", dir.join("tron.fish")));
    }
    let scripts: Vec<io::Result<Vec<u8>>> = std::thread::scope(|scope| {
        let jobs: Vec<_> = targets
            .iter()
            .map(|(shell, _)| {
                scope.spawn(move || {
                    let output = Command::new(binary).args(["completions", shell]).stdin(Stdio::null()).output()?;
                    if output.status.success() {
                        Ok(output.stdout)
                    } else {
                        Err(io::Error::other(String::from_utf8_lossy(&output.stderr).trim().to_owned()))
                    }
                })
            })
            .collect();
        jobs.into_iter().map(|job| job.join().unwrap_or_else(|_| Err(io::Error::other("panicked")))).collect()
    });
    for ((shell, dest), script) in targets.into_iter().zip(scripts) {
        let script = script.with_context(|| format!("cannot generate the {shell} completions"))?;
        outcome.count(write_file(&dest, &script, 0o644).with_context(|| format!("cannot write {}", dest.display()))?);
        entries.push(dest);
    }
    Ok(())
}

/// Records the installed files for `install.sh --uninstall`: the files of this
/// update, earlier entries that still exist, and the manifest itself.
fn write_manifest(layout: &Layout, mut entries: Vec<PathBuf>) -> anyhow::Result<()> {
    for old in &layout.entries {
        if old != &layout.manifest && !entries.contains(old) && fs::symlink_metadata(old).is_ok() {
            entries.push(old.clone());
        }
    }
    entries.push(layout.manifest.clone());
    let mut text = String::new();
    for entry in &entries {
        text.push_str(&entry.to_string_lossy());
        text.push('\n');
    }
    write_file(&layout.manifest, text.as_bytes(), 0o644)
        .with_context(|| format!("cannot write {}", layout.manifest.display()))?;
    Ok(())
}

/// Replaces the AppImage at `path` with `data`, after checking it runs.
pub fn install_appimage(path: &Path, data: &[u8], version: &str) -> anyhow::Result<bool> {
    let Some(new_binary) = StagedBinary::new(path, data, version)? else { return Ok(false) };
    new_binary.commit()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tron-update-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // On macOS the temporary directory is reached through a link
        // (/var to /private/var), and the running binary's path is resolved.
        fs::canonicalize(&dir).unwrap()
    }

    #[test]
    fn finds_the_installation_listing_the_binary() {
        let dir = temp_dir("locate");
        let share = dir.join("share");
        let bin = dir.join("bin/tron");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fs::write(&bin, "").unwrap();
        fs::create_dir_all(share.join("tron")).unwrap();
        let manifest = share.join(MANIFEST);
        fs::write(
            &manifest,
            format!("{}\n{}/applications/x.desktop\n{}\n", bin.display(), share.display(), manifest.display()),
        )
        .unwrap();
        let other = dir.join("elsewhere");
        let layout = locate_in(&bin, &[other.clone(), share.clone()]).unwrap();
        assert_eq!(layout.bin_dir, dir.join("bin"));
        assert_eq!(layout.share_dir, share);
        assert_eq!(layout.app, None);
        assert_eq!(layout.entries.len(), 3);
        assert!(
            locate_in(&dir.join("bin/other"), std::slice::from_ref(&share)).is_none(),
            "a binary the manifest does not list"
        );
        assert!(layout.writable());

        // macOS: the manifest lists tron.app and a link to its binary.
        let app = dir.join("Applications/tron.app");
        let app_binary = app.join("Contents/MacOS/tron");
        fs::create_dir_all(app_binary.parent().unwrap()).unwrap();
        fs::write(&app_binary, "").unwrap();
        fs::remove_file(&bin).unwrap();
        std::os::unix::fs::symlink(&app_binary, &bin).unwrap();
        fs::write(&manifest, format!("{}\n{}\n{}\n", app.display(), bin.display(), manifest.display())).unwrap();
        let layout = locate_in(&app_binary, &[share]).unwrap();
        assert_eq!(layout.app.as_deref(), Some(app.as_path()));
        assert_eq!(layout.binary(), app_binary);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writes_only_changed_files() {
        let dir = temp_dir("write");
        let file = dir.join("a/b.txt");
        assert!(write_file(&file, b"one", 0o644).unwrap());
        assert!(!write_file(&file, b"one", 0o644).unwrap(), "same contents");
        assert!(write_file(&file, b"one", 0o755).unwrap(), "another mode");
        assert!(write_file(&file, b"two", 0o755).unwrap());
        assert_eq!(fs::read(&file).unwrap(), b"two");
        assert!(!staged(&file).exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_a_binary_that_does_not_run() {
        let dir = temp_dir("binary");
        let dest = dir.join("tron");
        fs::write(&dest, "old").unwrap();
        assert!(StagedBinary::new(&dest, b"not a program", "1.0.0").is_err());
        assert_eq!(fs::read(&dest).unwrap(), b"old", "the old binary stays");
        assert!(!staged(&dest).exists());
        let script = b"#!/bin/sh\necho tron 1.0.0\n";
        drop(StagedBinary::new(&dest, script, "1.0.0").unwrap());
        assert!(!staged(&dest).exists(), "an update that failed later leaves nothing behind");
        assert_eq!(fs::read(&dest).unwrap(), b"old");
        StagedBinary::new(&dest, script, "1.0.0").unwrap().unwrap().commit().unwrap();
        assert!(StagedBinary::new(&dest, script, "1.0.0").unwrap().is_none(), "unchanged");
        assert_eq!(binary_version(&dest).unwrap(), "1.0.0");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn desktop_entry_names_the_binary() {
        let template =
            "[Desktop Entry]\nTryExec=tron\nExec=tron\nName=tron\n[Desktop Action New]\nExec=tron --startup\n";
        let entry = desktop_entry(template, Path::new("/home/me/.local/bin/tron"));
        assert_eq!(
            entry,
            "[Desktop Entry]\nTryExec=/home/me/.local/bin/tron\nExec=/home/me/.local/bin/tron\nName=tron\n\
             [Desktop Action New]\nExec=/home/me/.local/bin/tron --startup\n"
        );
        let spaced = desktop_entry("Exec=tron\n", Path::new("/my apps/bin/tron"));
        assert_eq!(spaced, "Exec=\"/my apps/bin/tron\"\n");
    }

    #[test]
    fn sets_plist_versions() {
        let plist = "<dict>\n\t<key>CFBundleShortVersionString</key><string>0.4.7</string>\n\
                     \t<key>CFBundleVersion</key>\n\t<string>0.4.7</string>\n\t<key>Other</key><string>x</string>\n</dict>";
        let updated = ["CFBundleShortVersionString", "CFBundleVersion"]
            .iter()
            .fold(plist.to_owned(), |plist, key| set_plist_string(&plist, key, "0.5.0"));
        assert_eq!(updated, plist.replace("0.4.7", "0.5.0"));
        assert_eq!(set_plist_string(plist, "Missing", "1"), plist);
        assert_eq!(
            set_plist_string("<key>A</key><true/><string>x</string>", "A", "1"),
            "<key>A</key><true/><string>x</string>"
        );
    }
}
