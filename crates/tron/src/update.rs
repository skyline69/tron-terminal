//! Finding out whether a newer tron has been released, at most once a day, and
//! remembering the release the user chose not to hear about again. And
//! `tron update`, which installs it.
//!
//! GitHub redirects the latest release page to the release's tag, so `curl`
//! reads the version from the redirect without the API or a JSON parser.

mod download;
mod install;
mod release;

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};

/// The version of this tron.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// How long a check's answer is used before GitHub is asked again.
const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// File in the data directory holding the last check and the skipped release.
const STATE_FILE: &str = "update-check";

/// The page of the release `version` on GitHub.
pub fn release_url(version: &str) -> String {
    format!("{REPOSITORY}/releases/tag/v{version}")
}

/// What the last check found, and the release not to tell about.
#[derive(Debug, Default, PartialEq, Eq)]
struct State {
    /// When GitHub last answered, in seconds since the Unix epoch.
    checked: Option<u64>,
    latest: Option<String>,
    skipped: Option<String>,
}

impl State {
    fn parse(text: &str) -> Self {
        let mut state = Self::default();
        for (key, value) in text.lines().filter_map(|line| line.split_once('=')) {
            let value = value.trim().to_owned();
            match key.trim() {
                "checked" => state.checked = value.parse().ok(),
                "latest" => state.latest = Some(value),
                "skipped" => state.skipped = Some(value),
                _ => {}
            }
        }
        state
    }

    fn format(&self) -> String {
        let mut text = String::new();
        if let Some(checked) = self.checked {
            text.push_str(&format!("checked={checked}\n"));
        }
        for (key, value) in [("latest", &self.latest), ("skipped", &self.skipped)] {
            if let Some(value) = value {
                text.push_str(&format!("{key}={value}\n"));
            }
        }
        text
    }

    fn load(data_dir: &Path) -> Self {
        std::fs::read_to_string(data_dir.join(STATE_FILE)).map(|text| Self::parse(&text)).unwrap_or_default()
    }

    fn save(&self, data_dir: &Path) {
        let path = data_dir.join(STATE_FILE);
        if let Err(error) = std::fs::create_dir_all(data_dir).and_then(|()| std::fs::write(&path, self.format())) {
            log::warn!("cannot write {}: {error}", path.display());
        }
    }

    /// The release to tell about: newer than this tron and not skipped.
    fn announced(&self) -> Option<String> {
        let latest = self.latest.as_deref()?;
        (is_newer(latest, CURRENT) && self.skipped.as_deref() != Some(latest)).then(|| latest.to_owned())
    }
}

/// A newer release to tell about, if any. Asks GitHub when the last answer is
/// older than a day, so it blocks for up to a few seconds.
pub fn newer_release(data_dir: &Path) -> Option<String> {
    let mut state = State::load(data_dir);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs());
    // A check from the future, after the clock was set back, is stale too.
    if state.checked.is_none_or(|checked| checked > now || now - checked >= INTERVAL.as_secs())
        && let Some(latest) = latest_release()
    {
        state.checked = Some(now);
        state.latest = Some(latest);
        state.save(data_dir);
    }
    state.announced()
}

/// Stops telling about the release `version`. Newer releases are told about again.
pub fn skip(data_dir: &Path, version: &str) {
    let mut state = State::load(data_dir);
    state.skipped = Some(version.to_owned());
    state.save(data_dir);
}

/// The version of the latest release, from where GitHub redirects its page.
fn latest_release() -> Option<String> {
    let output = Command::new("curl")
        .args(["--silent", "--show-error", "--max-time", "10", "--output", "/dev/null"])
        .args(["--write-out", "%{redirect_url}"])
        .arg(format!("{REPOSITORY}/releases/latest"))
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let version = version_from_url(String::from_utf8_lossy(&output.stdout).trim());
            if version.is_none() {
                log::debug!("update check: no release found");
            }
            version
        }
        Ok(output) => {
            log::debug!("update check failed: {}", String::from_utf8_lossy(&output.stderr).trim());
            None
        }
        Err(error) => {
            log::debug!("update check: cannot run curl: {error}");
            None
        }
    }
}

/// `1.2.3` from a release URL ending in `/releases/tag/v1.2.3`.
fn version_from_url(url: &str) -> Option<String> {
    let (_, tag) = url.rsplit_once("/releases/tag/")?;
    let version = tag.strip_prefix('v').unwrap_or(tag);
    parse(version).map(|_| version.to_owned())
}

/// A version's numbers. Pre-releases, such as `1.0.0-beta.1`, are not parsed,
/// so they are never announced.
fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let mut numbers = version.split('.').map(|part| part.parse::<u64>().ok());
    let parsed = (numbers.next()??, numbers.next()??, numbers.next()??);
    numbers.next().is_none().then_some(parsed)
}

fn is_newer(version: &str, than: &str) -> bool {
    matches!((parse(version), parse(than)), (Some(version), Some(than)) if version > than)
}

/// What `tron update` was asked to do.
#[derive(Debug, Default)]
pub struct UpdateOptions {
    /// Only tell whether a newer release is out.
    pub check: bool,
    /// Install this release instead of the latest.
    pub to: Option<String>,
    /// Install even when this version is installed.
    pub force: bool,
    /// Install this release archive instead of downloading one.
    pub archive: Option<PathBuf>,
}

/// Where releases are downloaded from: `.../latest/download/NAME` and
/// `.../download/vVERSION/NAME`. `TRON_UPDATE_URL` points it at a mirror or a test server.
fn releases_url() -> String {
    std::env::var("TRON_UPDATE_URL")
        .ok()
        .filter(|url| !url.is_empty())
        .map(|url| url.trim_end_matches('/').to_owned())
        .unwrap_or_else(|| format!("{REPOSITORY}/releases"))
}

fn styled(color: &str, text: &str) -> String {
    if std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        format!("\x1b[{color}m==>\x1b[0m {text}")
    } else {
        format!("==> {text}")
    }
}

fn info(text: &str) {
    eprintln!("{}", styled("36", text));
}

fn success(text: &str) {
    eprintln!("{}", styled("32", text));
}

/// `tron update`: downloads the latest release, or the one asked for, and
/// installs it where the running tron was installed.
///
/// One request for the release's `SHA256SUMS` tells its version and checksums,
/// so there is no API call and no rate limit. The archive then comes down over
/// several connections while the changelog is fetched beside it, is checked
/// and unpacked in memory, and only files whose contents changed are written.
pub fn run(options: UpdateOptions) -> anyhow::Result<()> {
    let started = Instant::now();
    if let Some(archive) = &options.archive {
        return install_archive(archive, started);
    }
    let platform = release::platform()?;
    let client = download::Client::new();
    let base = releases_url();
    let sums_url = match &options.to {
        Some(version) => {
            let version = version.strip_prefix('v').unwrap_or(version);
            if parse(version).is_none() {
                bail!("{version} is not a release version such as 1.2.3");
            }
            format!("{base}/download/v{version}/SHA256SUMS")
        }
        None => format!("{base}/latest/download/SHA256SUMS"),
    };
    let sums = client.text(&sums_url)?.with_context(|| match &options.to {
        Some(version) => format!("there is no release {version}"),
        None => "no release was found".to_owned(),
    })?;
    log::debug!("SHA256SUMS after {:?}", started.elapsed());
    let sums = release::Sums::parse(&sums);
    let version = sums.version(&platform).with_context(|| format!("the release has no archive for {platform}"))?;

    if version == CURRENT && !options.force {
        success(&format!("tron {CURRENT} is the latest release and installed"));
        return Ok(());
    }
    if options.to.is_none() && is_newer(CURRENT, &version) && !options.force {
        info(&format!("This tron, {CURRENT}, is newer than the latest release, {version}. Nothing changed."));
        return Ok(());
    }
    if options.check {
        info(&format!("tron {version} is out; you have {CURRENT}. Run tron update to install it."));
        return Ok(());
    }

    let target = install::locate()?;
    let (name, hash) = match &target {
        install::Target::AppImage(_) => {
            let arch = release::arch().context("there is no AppImage for this machine")?;
            sums.appimage(&version, arch)
        }
        install::Target::Script(_) => sums.archive(&version, &platform),
    }
    .with_context(|| format!("tron {version} has no download for {platform}"))?;
    let (url, changelog_url) =
        (format!("{base}/download/v{version}/{name}"), format!("{base}/download/v{version}/CHANGELOG.md"));

    let verb = if is_newer(&version, CURRENT) {
        "Updating"
    } else if version == CURRENT {
        "Reinstalling"
    } else {
        "Downgrading"
    };
    info(&format!("{verb} tron {CURRENT} to {version}"));
    // The changelog is small, so it comes down beside the archive at no cost.
    let (data, changelog) = std::thread::scope(|scope| {
        let changelog = is_newer(&version, CURRENT).then(|| scope.spawn(|| client.text(&changelog_url)));
        let data = client.file(&url, &name);
        let changelog = changelog.and_then(|job| job.join().ok()).and_then(Result::ok).flatten();
        (data, changelog)
    });
    let data = data?;
    let downloaded = started.elapsed();
    log::debug!("{name} after {downloaded:?}");
    release::verify(&name, &data, hash)?;
    log::debug!("verified after {:?}", started.elapsed());

    match target {
        install::Target::AppImage(path) => {
            install::install_appimage(&path, &data, &version)?;
            success(&format!(
                "tron {version} installed at {} in {:.2}s",
                path.display(),
                started.elapsed().as_secs_f64()
            ));
        }
        install::Target::Script(layout) => {
            let unpacked = release::unpack(&name, &data)?;
            log::debug!("unpacked after {:?}", started.elapsed());
            if layout.writable() {
                let outcome = install::install(&layout, &unpacked, &version)?;
                success(&format!(
                    "tron {version} installed in {:.2}s (download {:.2}s, {} files written, {} unchanged)",
                    started.elapsed().as_secs_f64(),
                    downloaded.as_secs_f64(),
                    outcome.written,
                    outcome.unchanged
                ));
            } else {
                install_as_root(&name, &data)?;
            }
        }
    }
    if let Some(changelog) = changelog {
        let changes = release::changes(&changelog, CURRENT, &version);
        if !changes.is_empty() {
            eprintln!("\n{}", changes.trim_end());
        }
    }
    eprintln!("\nNew windows run tron {version}; open ones keep running {CURRENT} until you restart tron.");
    Ok(())
}

/// Installs a release archive from disk: run by sudo for a system install,
/// or by hand for an archive downloaded elsewhere.
fn install_archive(path: &Path, started: Instant) -> anyhow::Result<()> {
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let data = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let unpacked = release::unpack(&name, &data)?;
    let version = unpacked.version().with_context(|| format!("{name} does not name a tron version"))?;
    let install::Target::Script(layout) = install::locate()? else {
        bail!("an AppImage is updated with the new AppImage, not with an archive");
    };
    if !layout.writable() {
        bail!("cannot write to {}; run this as a user who can", layout.bin_dir.display());
    }
    let outcome = install::install(&layout, &unpacked, &version)?;
    success(&format!(
        "tron {version} installed in {:.2}s ({} files written, {} unchanged)",
        started.elapsed().as_secs_f64(),
        outcome.written,
        outcome.unchanged
    ));
    Ok(())
}

/// Hands the verified archive to `sudo tron update --archive`, for an
/// installation this user cannot write to, such as one in /usr/local.
fn install_as_root(name: &str, data: &[u8]) -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("cannot find the running tron")?;
    let dir = std::env::temp_dir().join(format!("tron-update-{}", std::process::id()));
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let archive = dir.join(name);
    let result = (|| {
        std::fs::File::create(&archive)
            .and_then(|mut file| file.write_all(data))
            .with_context(|| format!("cannot write {}", archive.display()))?;
        info("Installing needs administrator rights; sudo will ask for your password.");
        let status = Command::new("sudo")
            .arg("--")
            .arg(&exe)
            .args(["update", "--archive"])
            .arg(&archive)
            .status()
            .context("cannot run sudo")?;
        if !status.success() {
            bail!("the installation as administrator failed");
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_version_from_the_release_redirect() {
        let url = "https://github.com/skyline69/tron-terminal/releases/tag/v0.2.1";
        assert_eq!(version_from_url(url).as_deref(), Some("0.2.1"));
        assert_eq!(version_from_url("https://github.com/skyline69/tron-terminal/releases"), None);
        assert_eq!(version_from_url("https://github.com/x/y/releases/tag/v1.0.0-beta.1"), None);
    }

    #[test]
    fn compares_versions_by_number() {
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("1.0", "0.1.0"));
    }

    #[test]
    fn announces_newer_releases_until_skipped() {
        let newer = "999.0.0";
        let mut state = State { checked: Some(1), latest: Some(newer.to_owned()), skipped: None };
        assert_eq!(state.announced().as_deref(), Some(newer));
        assert_eq!(State::parse(&state.format()), state);
        state.skipped = Some(newer.to_owned());
        assert_eq!(state.announced(), None);
        state.latest = Some("999.0.1".to_owned());
        assert_eq!(state.announced().as_deref(), Some("999.0.1"), "a later release is told about again");
        state.latest = Some(CURRENT.to_owned());
        assert_eq!(state.announced(), None);
    }

    #[test]
    fn skipping_is_remembered() {
        let dir = std::env::temp_dir().join(format!("tron-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        State { checked: Some(now), latest: Some("999.0.0".to_owned()), skipped: None }.save(&dir);
        // Checked just now: no request to GitHub.
        assert_eq!(newer_release(&dir).as_deref(), Some("999.0.0"));
        skip(&dir, "999.0.0");
        assert_eq!(newer_release(&dir), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
