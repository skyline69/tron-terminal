//! Finding out whether a newer tron has been released, at most once a day, and
//! remembering the release the user chose not to hear about again.
//!
//! GitHub redirects the latest release page to the release's tag, so `curl`
//! reads the version from the redirect without the API or a JSON parser.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
