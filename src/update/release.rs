//! The GitHub `releases/latest` document and asset selection.

use serde::{Deserialize, Serialize};

use super::version::Version;
use super::{CRATE, REPO};

/// A published release that may be offered as an update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// Version from the tag.
    pub version: Version,
    /// Release page (always a `https://github.com/<repo>/` URL).
    pub html_url: String,
    /// Downloadable files.
    pub assets: Vec<Asset>,
}

/// One release file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    /// File name.
    pub name: String,
    /// `browser_download_url`.
    pub url: String,
    /// Size in bytes as reported by GitHub (0 if unknown).
    pub size: u64,
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Deserialize)]
struct RawAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// Parses the `releases/latest` body. `Ok(None)` for a draft, a prerelease
/// or a tag that is not a plain `vX.Y.Z` version: those are never offered.
pub fn parse_release(body: &[u8]) -> Result<Option<Release>, String> {
    let raw: RawRelease =
        serde_json::from_slice(body).map_err(|e| format!("unexpected release data: {e}"))?;
    if raw.draft || raw.prerelease {
        return Ok(None);
    }
    let Some(version) = Version::parse(&raw.tag_name) else {
        return Ok(None);
    };
    let html_url = if is_release_page(&raw.html_url) {
        raw.html_url
    } else {
        tag_page(version)
    };
    Ok(Some(Release {
        version,
        html_url,
        assets: raw
            .assets
            .into_iter()
            .map(|a| Asset {
                name: a.name,
                url: a.browser_download_url,
                size: a.size,
            })
            .collect(),
    }))
}

/// Only our own repository's pages are opened from the release data.
fn is_release_page(url: &str) -> bool {
    let prefix = format!("https://github.com/{REPO}/");
    url.starts_with(&prefix)
        && url.len() < 512
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// `https://github.com/<repo>/releases/tag/v<version>`.
pub fn tag_page(version: Version) -> String {
    format!("https://github.com/{REPO}/releases/tag/v{version}")
}

/// `https://github.com/<repo>/releases/latest`.
pub fn latest_page() -> String {
    format!("https://github.com/{REPO}/releases/latest")
}

/// What the release workflow builds for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// `<crate>-v<ver>-linux-x86_64.tar.gz`.
    LinuxX86_64,
    /// No release asset exists for this platform.
    Other,
}

impl Platform {
    /// The platform this binary was built for.
    pub const fn current() -> Self {
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Self::LinuxX86_64
        } else {
            Self::Other
        }
    }
}

/// The archive for a platform and version, as `release.yml` names it.
pub fn archive_name(platform: Platform, version: Version) -> Option<String> {
    match platform {
        Platform::LinuxX86_64 => Some(format!("{CRATE}-v{version}-linux-x86_64.tar.gz")),
        Platform::Other => None,
    }
}

/// The archive and its `.sha256` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// Release version.
    pub version: Version,
    /// The archive.
    pub archive: Asset,
    /// `<archive>.sha256`.
    pub checksum: Asset,
}

/// Picks the archive for `platform` and its checksum file by exact name.
pub fn select_package(release: &Release, platform: Platform) -> Option<Package> {
    let name = archive_name(platform, release.version)?;
    let sum = format!("{name}.sha256");
    let find = |n: &str| release.assets.iter().find(|a| a.name == n).cloned();
    Some(Package {
        version: release.version,
        archive: find(&name)?,
        checksum: find(&sum)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(tag: &str, draft: bool, pre: bool) -> String {
        format!(
            r#"{{"tag_name":"{tag}","draft":{draft},"prerelease":{pre},
            "html_url":"https://github.com/Kartudev/Ferrochat-kartu/releases/tag/{tag}",
            "body":"notes","assets":[
              {{"name":"ferrochat-v9.1.0-linux-x86_64.tar.gz","browser_download_url":"https://github.com/Kartudev/Ferrochat-kartu/releases/download/v9.1.0/ferrochat-v9.1.0-linux-x86_64.tar.gz","size":123}},
              {{"name":"ferrochat-v9.1.0-linux-x86_64.tar.gz.sha256","browser_download_url":"https://github.com/Kartudev/Ferrochat-kartu/releases/download/v9.1.0/ferrochat-v9.1.0-linux-x86_64.tar.gz.sha256","size":99}},
              {{"name":"ferrochat-v9.1.0-linux-x86_64.tar.gz.sig","browser_download_url":"https://example.com/x","size":1}}
            ]}}"#
        )
    }

    #[test]
    fn parses_a_release() {
        let r = parse_release(body("v9.1.0", false, false).as_bytes())
            .unwrap()
            .unwrap();
        assert_eq!(r.version, Version::new(9, 1, 0));
        assert_eq!(
            r.html_url,
            "https://github.com/Kartudev/Ferrochat-kartu/releases/tag/v9.1.0"
        );
        assert_eq!(r.assets.len(), 3);
        assert_eq!(r.assets[0].size, 123);
    }

    #[test]
    fn drafts_prereleases_and_odd_tags_are_ignored() {
        assert_eq!(
            parse_release(body("v9.1.0", true, false).as_bytes()).unwrap(),
            None
        );
        assert_eq!(
            parse_release(body("v9.1.0", false, true).as_bytes()).unwrap(),
            None
        );
        assert_eq!(
            parse_release(body("v9.1.0-rc.1", false, false).as_bytes()).unwrap(),
            None
        );
        assert_eq!(
            parse_release(body("nightly", false, false).as_bytes()).unwrap(),
            None
        );
        assert!(parse_release(b"{ nope").is_err());
        assert!(parse_release(b"{}").is_err());
    }

    #[test]
    fn foreign_release_pages_are_replaced() {
        let j = r#"{"tag_name":"v1.0.0","html_url":"https://evil.example/x","assets":[]}"#;
        let r = parse_release(j.as_bytes()).unwrap().unwrap();
        assert_eq!(r.html_url, tag_page(Version::new(1, 0, 0)));
    }

    #[test]
    fn selects_the_linux_archive_and_checksum() {
        let r = parse_release(body("v9.1.0", false, false).as_bytes())
            .unwrap()
            .unwrap();
        let p = select_package(&r, Platform::LinuxX86_64).unwrap();
        assert_eq!(p.archive.name, "ferrochat-v9.1.0-linux-x86_64.tar.gz");
        assert_eq!(
            p.checksum.name,
            "ferrochat-v9.1.0-linux-x86_64.tar.gz.sha256"
        );
        assert_eq!(select_package(&r, Platform::Other), None);
    }

    #[test]
    fn a_missing_checksum_or_version_mismatch_selects_nothing() {
        let mut r = parse_release(body("v9.1.0", false, false).as_bytes())
            .unwrap()
            .unwrap();
        r.assets.retain(|a| !a.name.ends_with(".sha256"));
        assert_eq!(select_package(&r, Platform::LinuxX86_64), None);
        // Assets named for another version are not this release's archive.
        let r = parse_release(body("v9.2.0", false, false).as_bytes())
            .unwrap()
            .unwrap();
        assert_eq!(select_package(&r, Platform::LinuxX86_64), None);
    }
}
