//! What a release offers: its files and their checksums from `SHA256SUMS`,
//! the archive for this system, its contents and the changelog.

use std::collections::HashMap;
use std::io::Read;

use anyhow::{Context, bail};
use sha2::{Digest, Sha256};

/// Most bytes unpacked from an archive, against a corrupt or hostile one.
const UNPACKED_LIMIT: u64 = 512 << 20;

/// The files of a release with their SHA-256 checksums, from its `SHA256SUMS`.
#[derive(Debug, Default)]
pub struct Sums {
    files: Vec<(String, String)>,
}

impl Sums {
    pub fn parse(text: &str) -> Self {
        let files = text
            .lines()
            .filter_map(|line| {
                let (hash, name) = line.split_once(char::is_whitespace)?;
                // `sha256sum` marks binary mode with a `*` before the name.
                let name = name.trim_start().trim_start_matches('*');
                (hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) && !name.is_empty())
                    .then(|| (name.to_owned(), hash.to_ascii_lowercase()))
            })
            .collect();
        Self { files }
    }

    pub fn hash(&self, name: &str) -> Option<&str> {
        self.files.iter().find(|(file, _)| file == name).map(|(_, hash)| hash.as_str())
    }

    /// The release's version, read from the name of its archive for `platform`.
    pub fn version(&self, platform: &str) -> Option<String> {
        self.files.iter().find_map(|(name, _)| {
            let rest = name.strip_prefix("tron-")?;
            let version = [".tar.zst", ".tar.gz"]
                .iter()
                .find_map(|extension| rest.strip_suffix(&format!("-{platform}{extension}")))?;
            super::parse(version).map(|_| version.to_owned())
        })
    }

    /// The archive for `platform`: zstd when the release has one, being
    /// smaller and quicker to unpack, otherwise gzip. Name and checksum.
    pub fn archive(&self, version: &str, platform: &str) -> Option<(String, &str)> {
        ["tar.zst", "tar.gz"].iter().find_map(|extension| {
            let name = format!("tron-{version}-{platform}.{extension}");
            self.hash(&name).map(|hash| (name, hash))
        })
    }

    /// The AppImage for `arch` (x86_64 or aarch64). Name and checksum.
    pub fn appimage(&self, version: &str, arch: &str) -> Option<(String, &str)> {
        let name = format!("tron-{version}-{arch}.AppImage");
        self.hash(&name).map(|hash| (name, hash))
    }
}

/// The machine's architecture as release files name it.
pub fn arch() -> Option<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Some("x86_64"),
        "aarch64" => Some("aarch64"),
        _ => None,
    }
}

/// Suffix of the release archives for this machine, such as `x86_64-linux`.
pub fn platform() -> anyhow::Result<String> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "macos",
        other => bail!("there is no prebuilt tron for {other}"),
    };
    let arch = arch().with_context(|| format!("there is no prebuilt tron for {}", std::env::consts::ARCH))?;
    Ok(format!("{arch}-{os}"))
}

/// Fails unless `data` has the SHA-256 checksum `expected` (lowercase hex).
pub fn verify(name: &str, data: &[u8], expected: &str) -> anyhow::Result<()> {
    let actual = hex(&Sha256::digest(data));
    if actual != expected {
        bail!("the checksum of {name} does not match; the download is damaged or was altered");
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The files of a release archive by their path below its top directory,
/// such as `tron` and `dist/dev.tron.Terminal.desktop`, and the name of that
/// directory (`tron-0.4.7-x86_64-linux`).
pub struct Unpacked {
    pub top: String,
    pub files: HashMap<String, Vec<u8>>,
}

impl Unpacked {
    pub fn get(&self, path: &str) -> anyhow::Result<&[u8]> {
        self.files.get(path).map(Vec::as_slice).with_context(|| format!("the archive has no {path}"))
    }

    /// The version in the top directory's name.
    pub fn version(&self) -> Option<String> {
        let rest = self.top.strip_prefix("tron-")?;
        // The platform is the last two dash separated parts.
        let (rest, _os) = rest.rsplit_once('-')?;
        let (version, _arch) = rest.rsplit_once('-')?;
        super::parse(version).map(|_| version.to_owned())
    }
}

/// Unpacks a `.tar.zst` or `.tar.gz` archive named `name` in memory.
pub fn unpack(name: &str, data: &[u8]) -> anyhow::Result<Unpacked> {
    let reader: Box<dyn Read + '_> = if name.ends_with(".tar.zst") {
        Box::new(zstd::stream::read::Decoder::with_buffer(data).context("cannot read the zstd archive")?)
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        Box::new(flate2::bufread::GzDecoder::new(data))
    } else {
        bail!("{name} is not a .tar.zst or .tar.gz archive");
    };
    let mut archive = tar::Archive::new(reader.take(UNPACKED_LIMIT));
    let mut top = None;
    let mut files = HashMap::new();
    for entry in archive.entries().with_context(|| format!("cannot read {name}"))? {
        let mut entry = entry.with_context(|| format!("cannot read {name}"))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path().with_context(|| format!("cannot read {name}"))?.to_string_lossy().into_owned();
        let Some((dir, rest)) = path.trim_start_matches("./").split_once('/') else { continue };
        if top.get_or_insert_with(|| dir.to_owned()) != dir {
            bail!("{name} has more than one top directory");
        }
        let mut data = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut data).with_context(|| format!("cannot unpack {path} from {name}"))?;
        files.insert(rest.to_owned(), data);
    }
    let top = top.with_context(|| format!("{name} is empty"))?;
    Ok(Unpacked { top, files })
}

/// The changelog entries of the releases after `from` up to `to`, shortened
/// for a terminal: commit links are dropped and section titles kept.
pub fn changes(changelog: &str, from: &str, to: &str) -> String {
    let mut text = String::new();
    let mut include = false;
    for line in changelog.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            let version = heading.strip_prefix('[').and_then(|rest| rest.split_once(']')).map(|(version, _)| version);
            include = version.is_some_and(|version| super::is_newer(version, from) && !super::is_newer(version, to));
            if include {
                let date = heading.rsplit_once(" - ").map(|(_, date)| date.trim()).unwrap_or_default();
                let version = version.unwrap_or_default();
                text.push_str(&if date.is_empty() {
                    format!("tron {version}\n")
                } else {
                    format!("tron {version} ({date})\n")
                });
            }
            continue;
        }
        if !include {
            continue;
        }
        if let Some(section) = line.strip_prefix("### ") {
            text.push_str(&format!("  {section}\n"));
        } else if let Some(item) = line.strip_prefix("- ") {
            // "- Fix it ([abc1234](https://...))" without the link.
            let item = match item.rfind(" ([") {
                Some(at) if item.ends_with("))") => &item[..at],
                _ => item,
            };
            text.push_str(&format!("    - {item}\n"));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUMS: &str = "\
c28ff3380d61c3831b2f070d862fa1272f962b59896e6602b80a41f4af8b45fb  tron-0.4.7-aarch64-linux.tar.gz
cfaa987e87726d01488b4c6c141e69399ed93dfed67e2a5b9aae0697bdda6cd6  tron-0.4.7-aarch64.AppImage
8ae202d3d26f572a3efa81c3fd570b134cfc50b699fbf0bc2ff601e25a38a5bb  tron-0.4.7-x86_64-linux.tar.gz
d3b2616cc55a85603a07f79f6e1cf9ff18360e80c75cbec9059579cf05b1f589 *tron-0.4.7-x86_64-macos.tar.gz
0000000000000000000000000000000000000000000000000000000000000000  tron-0.4.7-x86_64-macos.tar.zst
not a line
";

    #[test]
    fn reads_the_release_from_its_checksums() {
        let sums = Sums::parse(SUMS);
        assert_eq!(sums.version("x86_64-linux").as_deref(), Some("0.4.7"));
        assert_eq!(sums.version("aarch64-macos"), None);
        let (name, hash) = sums.archive("0.4.7", "x86_64-linux").unwrap();
        assert_eq!(name, "tron-0.4.7-x86_64-linux.tar.gz");
        assert!(hash.starts_with("8ae202d3"));
        assert_eq!(sums.archive("0.4.7", "x86_64-macos").unwrap().0, "tron-0.4.7-x86_64-macos.tar.zst", "zstd first");
        assert_eq!(sums.appimage("0.4.7", "aarch64").unwrap().0, "tron-0.4.7-aarch64.AppImage");
        assert_eq!(sums.appimage("0.4.7", "x86_64"), None);
    }

    #[test]
    fn verifies_checksums() {
        let hash = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(verify("abc", b"abc", hash).is_ok());
        assert!(verify("abc", b"abd", hash).is_err());
    }

    fn tar(top: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, format!("{top}/{path}"), *data).unwrap();
        }
        builder.into_inner().unwrap()
    }

    #[test]
    fn unpacks_zstd_and_gzip_archives() {
        let top = "tron-1.2.3-x86_64-linux";
        let tar = tar(top, &[("tron", b"binary"), ("dist/a.desktop", b"desktop")]);
        let zst = zstd::encode_all(tar.as_slice(), 3).unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar).unwrap();
        let gz = gz.finish().unwrap();
        for (name, data) in [("x.tar.zst", zst), ("x.tar.gz", gz)] {
            let unpacked = unpack(name, &data).unwrap();
            assert_eq!(unpacked.top, top);
            assert_eq!(unpacked.version().as_deref(), Some("1.2.3"));
            assert_eq!(unpacked.get("tron").unwrap(), b"binary");
            assert_eq!(unpacked.get("dist/a.desktop").unwrap(), b"desktop");
            assert!(unpacked.get("missing").is_err());
        }
        assert!(unpack("x.zip", b"").is_err());
    }

    #[test]
    fn shows_the_changes_since_the_installed_version() {
        let changelog = "\
# Changelog

## [0.5.0](https://example/v0.5.0) - 2026-11-02

### Fixes

- Fix a thing ([abc1234](https://example/commit/abc1234))

## [0.4.8](https://example/v0.4.8) - 2026-11-01

### Features

- Add a thing ([def5678](https://example/commit/def5678))
- Keep (parentheses) in text

## [0.4.7](https://example/v0.4.7) - 2026-10-30

### Changes

- Old change
";
        let text = changes(changelog, "0.4.7", "0.5.0");
        assert_eq!(
            text,
            "tron 0.5.0 (2026-11-02)\n  Fixes\n    - Fix a thing\n\
             tron 0.4.8 (2026-11-01)\n  Features\n    - Add a thing\n    - Keep (parentheses) in text\n"
        );
        assert_eq!(changes(changelog, "0.4.8", "0.4.8"), "");
        assert!(changes(changelog, "0.4.7", "0.4.8").starts_with("tron 0.4.8"));
    }
}
