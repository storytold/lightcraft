//! The release feed: GitHub's Releases API and the static `latest.json` the pipeline publishes.
//!
//! Both are read into the same [`Release`], so the update check does not care which one answered.
//! The API path needs no authentication for a public repository (GitHub rate-limits it per IP,
//! which the caller sees as an [`UpdateError::Http`] with `403`); `latest.json` is a plain file on
//! the release itself and works when the API is rate-limited.

use serde_json::Value;

use crate::error::UpdateError;
use crate::target::{Asset, Kind, Target};
use crate::version::Version;

/// One release, as much of it as an update needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// The git tag (`v1.2.3`).
    pub tag: String,
    /// The release's title.
    pub title: String,
    /// The release notes (Markdown).
    pub notes: String,
    /// `YYYY-MM-DD` from the feed, when it says.
    pub published: Option<String>,
    /// Whether the feed marks it a prerelease.
    pub prerelease: bool,
    /// Every asset we can classify.
    pub assets: Vec<Asset>,
}

impl Release {
    /// Read one release out of a feed entry (GitHub API object or one `releases` array element of
    /// `latest.json`). `None` when it is not a release of ours: a draft, or a tag that does not
    /// name `<binary> <version>`.
    pub fn parse(v: &Value, allow_prerelease: bool) -> Option<Release> {
        if v.get("draft").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        let tag = v.get("tag_name").or_else(|| v.get("tag")).and_then(Value::as_str).unwrap_or("").trim();
        let version = version_of(tag)?;
        let prerelease = v.get("prerelease").and_then(Value::as_bool).unwrap_or(false) || !version.is_release();
        if prerelease && !allow_prerelease {
            return None;
        }
        let notes = v
            .get("body")
            .or_else(|| v.get("notes"))
            .and_then(Value::as_str)
            .unwrap_or("")
            // a release body is Markdown a maintainer wrote; it is shown, never executed
            .chars()
            .take(200_000)
            .collect::<String>();
        let assets = v.get("assets").and_then(Value::as_array).map(|list| list.iter().filter_map(asset_of).collect()).unwrap_or_default();
        Some(Release {
            version,
            tag: tag.to_string(),
            title: v.get("name").and_then(Value::as_str).unwrap_or(tag).trim().to_string(),
            notes,
            published: v.get("published_at").or_else(|| v.get("date")).and_then(Value::as_str).map(|s| s.chars().take(10).collect()),
            prerelease,
            assets,
        })
    }

    /// The file to install on `target`: an installer first, then a plain archive; `None` when the
    /// release has nothing this platform can use.
    pub fn best_asset(&self, target: &Target) -> Option<&Asset> {
        self.assets.iter().filter(|a| a.kind != Kind::Checksums && target.matches_name(&a.name)).min_by_key(|a| (a.kind.rank(), a.size))
    }

    /// The release's checksum list, when it publishes one.
    pub fn checksum_asset(&self) -> Option<&Asset> {
        self.assets.iter().find(|a| a.kind == Kind::Checksums)
    }
}

/// The version a tag names: `v1.2.3` and `1.2.3` both count, anything else (`nightly`) is not a
/// release of ours.
pub fn version_of(tag: &str) -> Option<Version> {
    Version::parse(tag).ok()
}

fn asset_of(v: &Value) -> Option<Asset> {
    let name = v.get("name").and_then(Value::as_str)?.trim();
    let url = v.get("browser_download_url").or_else(|| v.get("url")).and_then(Value::as_str)?.trim();
    if url.is_empty() {
        return None;
    }
    let size = v.get("size").and_then(Value::as_u64).unwrap_or(0);
    // the API leaves `digest` null for older uploads; `sha256:<hex>` when GitHub computed it
    let sha256 = v
        .get("sha256")
        .and_then(Value::as_str)
        .or_else(|| v.get("digest").and_then(Value::as_str).and_then(|d| d.strip_prefix("sha256:")))
        .map(str::trim)
        .filter(|d| d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase);
    Asset::new(name, url, size, sha256)
}

/// The newest release in a feed: GitHub's `GET /repos/{owner}/{repo}/releases` array, or the
/// `latest.json` object (`{"releases": [...]}` or one release on its own).
pub fn newest(body: &str, allow_prerelease: bool) -> Result<Release, UpdateError> {
    let json: Value = serde_json::from_str(body).map_err(|e| UpdateError::Parse(e.to_string()))?;
    let list: Vec<&Value> = match &json {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => match json.get("releases").and_then(Value::as_array) {
            Some(items) => items.iter().collect(),
            None => vec![&json],
        },
        _ => return Err(UpdateError::Parse("not a release list".into())),
    };
    list.iter()
        .filter_map(|v| Release::parse(v, allow_prerelease))
        .max_by(|a, b| a.version.cmp(&b.version))
        .ok_or_else(|| UpdateError::Parse("the release list holds no release of this app".into()))
    // (the newest by version, not by feed order: the API lists newest first, `latest.json` need not)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api_release(tag: &str, prerelease: bool, assets: &[(&str, &str, u64)]) -> Value {
        serde_json::json!({
            "tag_name": tag,
            "name": format!("App {tag}"),
            "body": "### Fixed\n- a thing",
            "published_at": "2026-10-12T09:31:00Z",
            "prerelease": prerelease,
            "draft": false,
            "assets": assets.iter().map(|(n, u, s)| serde_json::json!({"name": n, "browser_download_url": u, "size": s})).collect::<Vec<_>>(),
        })
    }

    fn feed() -> String {
        serde_json::json!([
            api_release("v0.6.9", false, &[("app-0.6.9-windows-x64.msi", "https://x/6", 10)]),
            api_release(
                "v0.7.1",
                false,
                &[
                    ("app-0.7.1-windows-x64.msi", "https://x/71", 40),
                    ("app-0.7.1-windows-x64-portable.zip", "https://x/71z", 30),
                    ("SHA256SUMS.txt", "https://x/71s", 2),
                    ("app-0.7.1-linux-x86_64.AppImage", "https://x/71l", 50),
                ]
            ),
            api_release("v0.7.0-rc.2", true, &[("app-0.7.0-rc.2-windows-x64.msi", "https://x/70", 35)]),
            api_release("nightly-2026-10-12", false, &[("app-nightly.msi", "https://x/n", 1)]),
        ])
        .to_string()
    }

    #[test]
    fn picks_the_newest_published_release() {
        let r = newest(&feed(), false).expect("a release");
        assert_eq!(r.version.to_text(), "0.7.1");
        assert_eq!(r.tag, "v0.7.1");
        assert_eq!(r.title, "App v0.7.1");
        assert_eq!(r.published.as_deref(), Some("2026-10-12"));
        assert!(r.notes.contains("a thing"));
        assert!(!r.prerelease);
    }

    #[test]
    fn prereleases_are_off_unless_asked_for() {
        assert_eq!(newest(&feed(), false).expect("release").version.to_text(), "0.7.1");
        let with_pre = newest(&feed(), true).expect("release");
        assert_eq!(with_pre.version.to_text(), "0.7.1", "a release still outranks its prereleases");
        let only_pre = serde_json::json!([api_release("v0.8.0-rc.1", true, &[("app.msi", "https://x/a", 1)])]).to_string();
        assert!(newest(&only_pre, false).is_err());
        assert_eq!(newest(&only_pre, true).expect("rc").version.to_text(), "0.8.0-rc.1");
    }

    #[test]
    fn a_tag_that_is_not_a_version_is_not_ours() {
        let v = serde_json::json!([api_release("docs-2026", false, &[("app.msi", "https://x/a", 1)])]).to_string();
        assert!(newest(&v, false).is_err(), "an unversioned tag is not a release");
        let draft = serde_json::json!([{"tag_name": "v9.9.9", "draft": true, "assets": []}]).to_string();
        assert!(newest(&draft, false).is_err(), "a draft is not published yet");
    }

    #[test]
    fn choices_the_installer_over_the_archive() {
        let r = newest(&feed(), false).expect("a release");
        let win = Target { os: crate::target::Os::Windows, arch: "x64" };
        let picked = r.best_asset(&win).expect("an msi");
        assert_eq!((picked.name.as_str(), picked.kind), ("app-0.7.1-windows-x64.msi", Kind::Msi));
        assert_eq!(r.checksum_asset().map(|a| a.name.as_str()), Some("SHA256SUMS.txt"));
        let lin = Target { os: crate::target::Os::Linux, arch: "x86_64" };
        assert_eq!(r.best_asset(&lin).map(|a| a.name.as_str()), Some("app-0.7.1-linux-x86_64.AppImage"));
        let arm = Target { os: crate::target::Os::Linux, arch: "aarch64" };
        assert!(r.best_asset(&arm).is_none(), "no aarch64 file in this release");
    }

    #[test]
    fn reads_the_static_manifest() {
        let sha = "ab".repeat(32);
        let manifest = serde_json::json!({
            "version": "1.0.0",
            "notes": "the notes",
            "pub_date": "2026-10-12T00:00:00Z",
            "releases": [
                {"tag": "v1.0.0", "name": "App 1.0.0", "notes": "the notes", "date": "2026-10-12",
                 "assets": [{"name": "app-1.0.0-windows-x64.msi", "url": "https://x/1", "size": 7, "sha256": sha}]}
            ]
        })
        .to_string();
        let r = newest(&manifest, false).expect("a release");
        assert_eq!(r.version.to_text(), "1.0.0");
        assert_eq!(r.notes, "the notes");
        let a = &r.assets[0];
        assert_eq!(a.size, 7);
        assert_eq!(a.sha256.as_deref().map(str::len), Some(64));
        assert_eq!(a.kind, Kind::Msi);
    }

    #[test]
    fn a_single_release_object_works_too() {
        let one = serde_json::json!({"tag_name": "v2.0.0", "assets": [{"name": "app-2.0.0-macos-universal.dmg", "browser_download_url": "https://x/d", "size": 3}]}).to_string();
        assert_eq!(newest(&one, false).expect("a release").version.to_text(), "2.0.0");
    }

    #[test]
    fn broken_feeds_are_errors_not_panics() {
        assert!(matches!(newest("not json", false), Err(UpdateError::Parse(_))));
        assert!(matches!(newest("42", false), Err(UpdateError::Parse(_))));
        assert!(matches!(newest("[]", false), Err(UpdateError::Parse(_))));
        assert!(Version::parse("v1.2.3").is_ok());
        assert!(version_of("v1.2.3-rc.1").is_some());
        assert!(version_of("v1.2").is_none());
    }

    #[test]
    fn assets_without_a_url_are_skipped() {
        let v = serde_json::json!([{"tag_name": "v1.0.1", "assets": [{"name": "app-1.0.1-windows-x64.msi", "size": 1}]}]).to_string();
        let r = newest(&v, false).expect("a release");
        assert!(r.assets.is_empty());
        assert!(r.best_asset(&Target { os: crate::target::Os::Windows, arch: "x64" }).is_none());
    }
}
