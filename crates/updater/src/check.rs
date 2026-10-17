//! The update check itself: read the release feed, compare it with this build, pick the file.
//!
//! Nothing is downloaded here ([`crate::download`]) and nothing is installed ([`crate::install`]);
//! a check is one small HTTPS request that returns an [`Update`] describing what is newer, or
//! `None` when this build is current.
//!
//! Where the feed comes from, in order:
//! 1. the body the caller already has ([`check`]'s `feed`, which the engine fetches off the UI
//!    thread and caches);
//! 2. [`Options::manifest_url`] (set through the Rust API only: a test or a mirror; no command,
//!    control request or setting reaches it);
//! 3. the static `latest.json` published on the newest release;
//! 4. GitHub's Releases API for the brand's repository ([`lcc_brand::REPOSITORY`]).
//!
//! The API needs no token for a public repository, but it is rate-limited per IP; a 403 there is
//! reported as such, and the static manifest is tried first because it is a plain file on the
//! release and keeps working when the API is rate-limited.
//!
//! Whatever the feed says, the files it offers must be this repository's release downloads
//! (`https://github.com/<owner>/<repo>/releases/download/…`, [`release_download_prefix`]): a file
//! anywhere else is passed over, and a feed, file or checksum list on plain `http://` is refused.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lcc_net::{Client, ClientConfig};

use crate::error::UpdateError;
use crate::release::{self, Release};
use crate::target::{Asset, Target};
use crate::version::Version;

/// Feed bodies are small; anything larger is not our release list.
const MAX_FEED_BYTES: u64 = 4 * 1024 * 1024;
/// Downloads can be a disk image or an installer: generous, but not unbounded.
pub const MAX_DOWNLOAD_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// How long a finished check answers a repeat question without asking again.
pub const REUSE_AFTER: Duration = Duration::from_secs(20);

/// What a check was told to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// Repository to ask, `owner/name`.
    pub repo: String,
    /// The build to compare against (usually [`Version::current`]).
    pub current: Version,
    /// The machine's platform; `None` when we have no update for it.
    pub target: Option<Target>,
    /// Include prereleases (`-rc.1`) in what counts as newer.
    pub prerelease: bool,
    /// A feed URL that replaces the release's own (a mirror, or a test's local feed); it must be
    /// `https://`. The files it offers still have to be on this repository's releases.
    pub manifest_url: Option<String>,
    /// Tests only: one more origin (`http://127.0.0.1:<port>`) whose feed and files are trusted
    /// like the release's own, plain `http` included. The engine sets it only in its own tests;
    /// no command, control request or MCP tool can.
    pub extra_origin: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            repo: lcc_brand::REPOSITORY.to_string(),
            current: Version::current(),
            target: Target::current(),
            prerelease: false,
            manifest_url: None,
            extra_origin: None,
        }
    }
}

/// A newer release, and the file this machine would install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    /// What is running now.
    pub current: Version,
    /// The newer release.
    pub version: Version,
    /// The release title (or its tag).
    pub title: String,
    pub tag: String,
    /// The asset to download.
    pub asset: Asset,
    /// SHA-256 to check the download against, when the release publishes one.
    pub sha256: Option<String>,
    /// `YYYY-MM-DD`, when the feed says.
    pub published: Option<String>,
    /// The release notes (Markdown).
    pub notes: String,
    /// One line on what installing it will do on this machine.
    pub action: String,
}

impl Update {
    /// The version as a dialog shows it (`1.2.3`).
    pub fn version_text(&self) -> String {
        self.version.to_text()
    }
}

/// Check for an update: `Ok(None)` when this build is the newest the feed knows of.
///
/// `feed` is a release feed body the caller already has (the engine fetches and caches it, so a
/// check costs no request); `None` fetches it from [`Options::manifest_url`], the release's
/// `latest.json`, or the API.
pub fn check(opts: &Options, feed: Option<&str>, cancel: Option<Arc<AtomicBool>>) -> Result<Option<Update>, UpdateError> {
    let Some(target) = opts.target else {
        return Err(UpdateError::Unsupported(format!(
            "there is no update for this build ({} {}); the releases page has the files for the platforms we ship",
            std::env::consts::OS,
            std::env::consts::ARCH
        )));
    };
    let body = match feed {
        Some(text) => text.to_string(),
        None => fetch_feed(opts, cancel.clone())?,
    };
    let release = release::newest(&body, opts.prerelease)?;
    from_release(opts, target, release, cancel)
}

/// [`check`] for a release already parsed (a test, or a caller that cached the parse).
pub fn from_release(opts: &Options, target: Target, release: Release, cancel: Option<Arc<AtomicBool>>) -> Result<Option<Update>, UpdateError> {
    if release.version <= opts.current {
        return Ok(None);
    }
    // a file or checksum list on plain http is refused outright, not quietly passed over
    for offered in [release.best_asset(&target), release.checksum_asset()].into_iter().flatten() {
        release_file(opts, &offered.url)?;
    }
    // only this repository's release downloads are offered, whatever else the feed lists
    let mut release = release;
    release.assets.retain(|a| match release_file(opts, &a.url) {
        Ok(true) => true,
        _ => {
            log::info!("update: {} is not one of the release's own files; passed over", a.name);
            false
        }
    });
    let Some(asset) = release.best_asset(&target).cloned() else {
        return Err(UpdateError::NoAssetFor { version: release.version.to_text(), target: format!("{}-{}", target.os.slug(), target.arch) });
    };
    // the release's own checksum list, when it is not already in the feed entry
    let sha256 = match &asset.sha256 {
        Some(sha) => Some(sha.clone()),
        None => match release.checksum_asset() {
            Some(sums) => fetch_checksum(&sums.url, &asset.name, cancel)?,
            None => None,
        },
    };
    Ok(Some(Update {
        current: opts.current.clone(),
        version: release.version.clone(),
        title: release.title.clone(),
        tag: release.tag.clone(),
        asset,
        sha256,
        published: release.published.clone(),
        notes: release.notes.clone(),
        action: target.describe(),
    }))
}

/// The feed body: the override URL, else the static manifest on the newest release, else the API.
fn fetch_feed(opts: &Options, cancel: Option<Arc<AtomicBool>>) -> Result<String, UpdateError> {
    if let Some(url) = &opts.manifest_url {
        if under_test_origin(opts, url).is_none() {
            require_https(url, "the release feed")?;
        }
        return get_text(url, cancel);
    }
    match get_text(&manifest_url(&opts.repo), cancel.clone()) {
        Ok(body) => Ok(body),
        // no manifest on that release (an older one, or none published yet): ask the API
        Err(UpdateError::Http { code: 404, .. }) => get_text(&api_url(&opts.repo), cancel),
        Err(e) => Err(e),
    }
}

/// `owner/name` from either `owner/name` or a GitHub URL (`https://github.com/owner/name[.git]`),
/// which is how `brand.toml` spells the repository.
fn owner_name(repo: &str) -> &str {
    let r = repo.trim();
    let r = ["https://github.com/", "http://github.com/", "github.com/"].iter().find_map(|p| r.strip_prefix(p)).unwrap_or(r);
    let r = r.trim_matches('/');
    r.strip_suffix(".git").unwrap_or(r)
}

/// `https://github.com/<owner>/<repo>/releases/latest/download/latest.json`.
pub fn manifest_url(repo: &str) -> String {
    format!("https://github.com/{}/releases/latest/download/latest.json", owner_name(repo))
}

/// `https://api.github.com/repos/<owner>/<repo>/releases?per_page=30`.
pub fn api_url(repo: &str) -> String {
    format!("https://api.github.com/repos/{}/releases?per_page=30", owner_name(repo))
}

/// `https://github.com/<owner>/<repo>/releases/download/`: where every file of the repository's
/// releases lives, and the only place an update is taken from.
pub fn release_download_prefix(repo: &str) -> String {
    format!("https://github.com/{}/releases/download/", owner_name(repo))
}

/// Whether `url` is one of the release's own files: `Ok(true)` under
/// [`release_download_prefix`] (or the test origin), `Ok(false)` an https URL anywhere else, and
/// an error for anything that is not https.
fn release_file(opts: &Options, url: &str) -> Result<bool, UpdateError> {
    if let Some(rest) = under_test_origin(opts, url) {
        return Ok(plain_path(rest));
    }
    require_https(url, "the update's file")?;
    Ok(strip_prefix_ignore_case(url.trim(), &release_download_prefix(&opts.repo)).is_some_and(plain_path))
}

/// The rest of `url` after the test origin, when it is under it.
fn under_test_origin<'a>(opts: &Options, url: &'a str) -> Option<&'a str> {
    let origin = opts.extra_origin.as_deref().map(str::trim).filter(|o| !o.is_empty())?;
    let origin = origin.trim_end_matches('/');
    let rest = strip_prefix_ignore_case(url.trim(), origin)?;
    // `http://127.0.0.1:1` must not cover `http://127.0.0.1:12/…` or `…:1.evil/…`
    (rest.is_empty() || rest.starts_with('/')).then_some(rest)
}

/// `text` without `prefix`, compared ignoring ASCII case (scheme and host are case-insensitive,
/// and GitHub's owner and repository names are too).
fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) { text.get(prefix.len()..) } else { None }
}

/// A path that stays where it starts: no `.`/`..` segment, no backslash and no encoded dot or
/// slash, so `…/releases/download/../../../other/repo/…` cannot reach another repository.
fn plain_path(rest: &str) -> bool {
    let path = rest.split(['?', '#']).next().unwrap_or("");
    let lower = path.to_ascii_lowercase();
    !lower.contains('\\')
        && !lower.contains("%2e")
        && !lower.contains("%2f")
        && !lower.contains("%5c")
        && !path.split('/').any(|seg| seg == "." || seg == "..")
}

/// `Err` unless `url` is `https://`.
fn require_https(url: &str, what: &str) -> Result<(), UpdateError> {
    if strip_prefix_ignore_case(url.trim(), "https://").is_some() {
        return Ok(());
    }
    let shown: String = url.trim().chars().take(200).collect();
    Err(UpdateError::Insecure(format!("{what} at {shown}")))
}

fn client() -> Result<Client, UpdateError> {
    Client::new(ClientConfig {
        max_body: MAX_FEED_BYTES,
        connect_timeout: Duration::from_secs(8),
        stall_timeout: Duration::from_secs(20),
        ..ClientConfig::default()
    })
    .map_err(|e| UpdateError::Net(e.to_string()))
}

/// GET a small text body.
fn get_text(url: &str, cancel: Option<Arc<AtomicBool>>) -> Result<String, UpdateError> {
    let client = client()?;
    let mut req = client.get(url).header("Accept", "application/vnd.github+json, application/json");
    if let Some(flag) = cancel {
        req = req.cancel(flag);
    }
    let resp = req.send()?;
    let status = resp.status;
    let body = resp.text()?;
    if status == 403 || status == 429 {
        // never the body: it can be a whole HTML page
        let what = if body.contains("rate limit") { "GitHub's rate limit was reached; try again later" } else { "the feed refused the request" };
        return Err(UpdateError::Http { code: status, what: what.to_string() });
    }
    if !(200..300).contains(&status) {
        return Err(UpdateError::Http { code: status, what: "the release feed was not found".into() });
    }
    Ok(body)
}

/// The release's checksum list: the SHA-256 for `file`, when it lists it.
fn fetch_checksum(url: &str, file: &str, cancel: Option<Arc<AtomicBool>>) -> Result<Option<String>, UpdateError> {
    // a checksum that cannot be fetched is not a failure: the download is still TLS-verified and
    // size-checked, and saying "no update" because of a missing sums file would be worse
    match get_text(url, cancel) {
        Ok(body) => Ok(crate::checksum::lookup(&body, file)),
        Err(e) => {
            log::debug!("update: checksum list: {e}");
            Ok(None)
        }
    }
}

/// The result of one check, for the UI and `update.status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Checked {
    /// A newer release is available.
    Found(Box<Update>),
    /// This build is the newest the feed knows of.
    Current(String),
    /// The check failed.
    Failed(String),
}

/// A memo of the last check, so a dialog opened twice does not ask GitHub twice, and only one
/// check runs at a time.
#[derive(Default)]
pub struct Cache {
    last: Mutex<Option<(std::time::Instant, Options, Checked)>>,
    running: AtomicBool,
}

impl Cache {
    /// The last result, when it is recent and was asked with the same options.
    pub fn get(&self, opts: &Options) -> Option<Checked> {
        let guard = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        match guard.as_ref() {
            Some((when, had, result)) if had == opts && when.elapsed() < REUSE_AFTER => Some(result.clone()),
            _ => None,
        }
    }

    /// Record a result.
    pub fn put(&self, opts: &Options, result: Checked) {
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = Some((std::time::Instant::now(), opts.clone(), result));
    }

    /// Claim the single check slot (`false`: another one is already running).
    pub fn begin(&self) -> bool {
        !self.running.swap(true, Ordering::SeqCst)
    }

    /// Release the single check slot.
    pub fn end(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::Os;

    fn opts() -> Options {
        Options {
            // the real repository, so the URL helpers are checked against what we publish to
            repo: lcc_brand::REPOSITORY.to_string(),
            current: Version::parse("1.0.0").expect("a version"),
            target: Some(Target { os: Os::Windows, arch: "x64" }),
            prerelease: false,
            manifest_url: None,
            extra_origin: None,
        }
    }

    /// The feed URLs point at the repository the app is built from, whether brand.toml spells it
    /// as a URL (as it does) or as `owner/name` (the bug: a URL gave `github.com/https://…`, a 404).
    #[test]
    fn feed_urls_name_the_repository_once() {
        for repo in [lcc_brand::REPOSITORY, "emmertex/x", "https://github.com/emmertex/x/", "https://github.com/emmertex/x.git"] {
            for url in [manifest_url(repo), api_url(repo)] {
                assert_eq!(url.matches("github.com").count(), 1, "{url}");
                assert!(!url.contains("//github.com/https") && !url.contains("repos/https"), "{url}");
            }
        }
        let own = lcc_brand::REPOSITORY.trim_end_matches('/');
        assert_eq!(manifest_url(lcc_brand::REPOSITORY), format!("{own}/releases/latest/download/latest.json"));
    }

    fn feed(version: &str) -> String {
        serde_json::json!([{
            "tag_name": format!("v{version}"),
            "name": format!("App v{version}"),
            "body": "notes",
            "published_at": "2026-10-12T00:00:00Z",
            "assets": [
                {"name": format!("app-{version}-windows-x64.msi"), "browser_download_url": on_release("a.msi"), "size": 100},
            ],
        }])
        .to_string()
    }

    fn run(opts: &Options, body: &str) -> Result<Option<Update>, UpdateError> {
        check(opts, Some(body), None)
    }

    #[test]
    fn finds_a_newer_release_and_picks_the_file() {
        let u = run(&opts(), &feed("1.2.0")).expect("a check").expect("an update");
        assert_eq!((u.current.to_text().as_str(), u.version_text().as_str()), ("1.0.0", "1.2.0"));
        assert_eq!(u.asset.name, "app-1.2.0-windows-x64.msi");
        assert_eq!(u.asset.size, 100);
        assert_eq!(u.action, "run the 64-bit installer and then start the app again");
        assert_eq!(u.title, "App v1.2.0");
        assert_eq!(u.published.as_deref(), Some("2026-10-12"));
        assert_eq!(u.notes, "notes");
        assert_eq!(u.tag, "v1.2.0");
        assert_eq!(u.sha256, None, "this release publishes no sums file");
    }

    #[test]
    fn the_same_version_or_an_older_one_is_not_an_update() {
        assert!(run(&opts(), &feed("1.0.0")).expect("a check").is_none());
        assert!(run(&opts(), &feed("0.9.9")).expect("a check").is_none());
        // a development build ahead of the newest release stays quiet
        let ahead = Options { current: Version::parse("1.3.0").expect("v"), ..opts() };
        assert!(run(&ahead, &feed("1.2.0")).expect("a check").is_none());
    }

    #[test]
    fn a_release_without_our_file_is_reported() {
        let mac = Options { target: Some(Target { os: Os::Macos, arch: "universal" }), ..opts() };
        let err = run(&mac, &feed("1.2.0")).expect_err("no dmg");
        assert!(matches!(err, UpdateError::NoAssetFor { .. }), "{err}");
        assert!(err.to_string().contains("macos-universal"), "{err}");
    }

    #[test]
    fn a_platform_we_do_not_build_for_says_so() {
        let none = Options { target: None, ..opts() };
        let err = run(&none, &feed("1.2.0")).expect_err("no platform");
        assert!(matches!(err, UpdateError::Unsupported(_)), "{err}");
    }

    #[test]
    fn urls_are_what_github_serves() {
        assert_eq!(manifest_url("a/b"), "https://github.com/a/b/releases/latest/download/latest.json");
        assert_eq!(manifest_url("/a/b/"), "https://github.com/a/b/releases/latest/download/latest.json");
        assert_eq!(api_url("a/b"), "https://api.github.com/repos/a/b/releases?per_page=30");
    }

    #[test]
    fn the_cache_answers_again_and_claims_one_check() {
        let c = Cache::default();
        let o = opts();
        assert!(c.get(&o).is_none());
        c.put(&o, Checked::Current("1.0.0".into()));
        assert_eq!(c.get(&o), Some(Checked::Current("1.0.0".into())));
        let other = Options { prerelease: true, ..opts() };
        assert!(c.get(&other).is_none(), "different options: a different question");
        assert!(c.begin(), "the first check takes the slot");
        assert!(!c.begin(), "a second one waits");
        c.end();
        assert!(c.begin());
    }

    #[test]
    fn defaults_come_from_the_brand() {
        let o = Options::default();
        assert_eq!(o.repo, lcc_brand::REPOSITORY);
        assert_eq!(o.current, Version::current());
        assert_eq!(o.target, Target::current());
        assert!(!o.prerelease);
    }

    #[test]
    fn a_checksums_asset_pins_the_download() {
        let body = serde_json::json!([{
            "tag_name": "v1.2.0",
            "assets": [
                {"name": "app-1.2.0-windows-x64.msi", "browser_download_url": on_release("app-1.2.0-windows-x64.msi"), "size": 100},
                {"name": "SHA256SUMS.txt", "browser_download_url": "http://127.0.0.1:1/sums", "size": 40},
            ],
        }])
        .to_string();
        // the sums file is unreachable here (a test origin, so http is allowed): the update still
        // comes back, without a hash
        let o = Options { extra_origin: Some("http://127.0.0.1:1".into()), ..opts() };
        let u = run(&o, &body).expect("a check").expect("an update");
        assert_eq!(u.sha256, None);
        assert_eq!(u.asset.kind, crate::target::Kind::Msi);
    }

    /// Where this repository's release files live: the only place an update may come from.
    fn on_release(file: &str) -> String {
        format!("{}/releases/download/v1.2.0/{file}", lcc_brand::REPOSITORY.trim_end_matches('/'))
    }

    fn feed_with(assets: serde_json::Value) -> String {
        serde_json::json!([{"tag_name": "v1.2.0", "assets": assets}]).to_string()
    }

    #[test]
    fn a_file_that_is_not_on_this_repositorys_releases_is_not_offered() {
        for url in [
            "https://example.invalid/app-1.2.0-windows-x64.msi".to_string(),
            "https://github.com/someone-else/fork/releases/download/v1.2.0/app-1.2.0-windows-x64.msi".to_string(),
            // the repository's prefix, but a sibling of it
            format!("{}-evil/releases/download/v1.2.0/app-1.2.0-windows-x64.msi", lcc_brand::REPOSITORY.trim_end_matches('/')),
            // climbing out of the release folder into another repository
            on_release("../../../../other/repo/releases/download/v1/app-1.2.0-windows-x64.msi"),
            on_release("%2e%2e/%2e%2e/app-1.2.0-windows-x64.msi"),
        ] {
            let body = feed_with(serde_json::json!([{"name": "app-1.2.0-windows-x64.msi", "browser_download_url": url, "size": 100}]));
            let err = run(&opts(), &body).expect_err("a file from elsewhere is no update");
            assert!(matches!(err, UpdateError::NoAssetFor { .. }), "{url}: {err}");
        }
        // the same file on the release is offered
        let body = feed_with(
            serde_json::json!([{"name": "app-1.2.0-windows-x64.msi", "browser_download_url": on_release("app-1.2.0-windows-x64.msi"), "size": 100}]),
        );
        assert!(run(&opts(), &body).expect("a check").is_some());
    }

    #[test]
    fn a_feed_on_plain_http_is_refused_before_any_request() {
        // nothing listens on port 1: without the https rule this would be a connection error
        let o = Options { manifest_url: Some("http://127.0.0.1:1/latest.json".into()), ..opts() };
        let err = check(&o, None, None).expect_err("http feed");
        assert!(err.to_string().contains("https is required"), "{err}");
    }

    #[test]
    fn a_file_or_checksum_list_on_plain_http_is_refused() {
        let http_file = feed_with(serde_json::json!([
            {"name": "app-1.2.0-windows-x64.msi", "browser_download_url": on_release("app-1.2.0-windows-x64.msi").replacen("https://", "http://", 1), "size": 100},
        ]));
        let err = run(&opts(), &http_file).expect_err("http asset");
        assert!(err.to_string().contains("https is required"), "{err}");
        let http_sums = feed_with(serde_json::json!([
            {"name": "app-1.2.0-windows-x64.msi", "browser_download_url": on_release("app-1.2.0-windows-x64.msi"), "size": 100},
            {"name": "SHA256SUMS.txt", "browser_download_url": "http://127.0.0.1:1/SHA256SUMS.txt", "size": 40},
        ]));
        let err = run(&opts(), &http_sums).expect_err("http checksum list");
        assert!(err.to_string().contains("https is required"), "{err}");
    }

    #[test]
    fn the_default_options_name_no_feed_of_their_own() {
        // the feed comes from the brand's repository; nothing in the environment replaces it
        assert_eq!(Options::default().manifest_url, None);
    }

    /// The `latest.json` the v0.8.1 release published (notes shortened), with the brand's names.
    fn release_0_8_1() -> String {
        let bin = lcc_brand::BINARY;
        let base = format!("{}/releases/download/v0.8.1", lcc_brand::REPOSITORY.trim_end_matches('/'));
        let files = [
            format!("{bin}-0.8.1-linux-aarch64.AppImage"),
            format!("{bin}-0.8.1-linux-aarch64.AppImage.zsync"),
            format!("{bin}-0.8.1-linux-aarch64.deb"),
            format!("{bin}-0.8.1-linux-aarch64.flatpak"),
            format!("{bin}-0.8.1-linux-aarch64.rpm"),
            format!("{bin}-0.8.1-linux-aarch64.tar.gz"),
            format!("{bin}-0.8.1-linux-x86_64.AppImage"),
            format!("{bin}-0.8.1-linux-x86_64.AppImage.zsync"),
            format!("{bin}-0.8.1-linux-x86_64.deb"),
            format!("{bin}-0.8.1-linux-x86_64.flatpak"),
            format!("{bin}-0.8.1-linux-x86_64.rpm"),
            format!("{bin}-0.8.1-linux-x86_64.tar.gz"),
            format!("{bin}-0.8.1-macos-universal.dmg"),
            format!("{bin}-0.8.1-windows-arm64-portable.zip"),
            format!("{bin}-0.8.1-windows-arm64.msi"),
            format!("{bin}-0.8.1-windows-x64-portable.zip"),
            format!("{bin}-0.8.1-windows-x64.msi"),
            format!("{bin}-cli-0.8.1-macos-universal.zip"),
            format!("{bin}-web-0.8.1.zip"),
        ];
        let assets: Vec<serde_json::Value> = files
            .iter()
            .enumerate()
            .map(|(i, f)| serde_json::json!({"name": f, "url": format!("{base}/{f}"), "size": 100_000_000 + i, "sha256": format!("{:064x}", i + 1)}))
            .collect();
        serde_json::json!({
            "version": "0.8.1",
            "name": "App v0.8.1",
            "pub_date": "2026-10-14T05:03:58Z",
            "notes": "## What's Changed",
            "releases": [{"tag": "v0.8.1", "name": "App v0.8.1", "notes": "## What's Changed", "date": "2026-10-14", "prerelease": false, "assets": assets}],
        })
        .to_string()
    }

    #[test]
    fn the_genuine_release_feed_offers_each_platform_its_file() {
        let bin = lcc_brand::BINARY;
        let older = Options { current: Version::parse("0.8.0").expect("v"), ..opts() };
        for (os, arch, file) in [
            (Os::Windows, "x64", format!("{bin}-0.8.1-windows-x64.msi")),
            (Os::Windows, "arm64", format!("{bin}-0.8.1-windows-arm64.msi")),
            (Os::Macos, "universal", format!("{bin}-0.8.1-macos-universal.dmg")),
            (Os::Linux, "x86_64", format!("{bin}-0.8.1-linux-x86_64.AppImage")),
            (Os::Linux, "aarch64", format!("{bin}-0.8.1-linux-aarch64.AppImage")),
        ] {
            let o = Options { target: Some(Target { os, arch }), ..older.clone() };
            let u = run(&o, &release_0_8_1()).expect("a check").expect("an update");
            assert_eq!(u.asset.name, file, "{os:?} {arch}");
            assert!(u.asset.url.ends_with(&format!("/releases/download/v0.8.1/{file}")), "{}", u.asset.url);
            assert_eq!(u.sha256.as_deref().map(str::len), Some(64), "the feed's own hash pins the download");
        }
        let bsd = Options { target: Some(Target { os: Os::Freebsd, arch: "x86_64" }), ..older };
        assert!(matches!(run(&bsd, &release_0_8_1()), Err(UpdateError::NoAssetFor { .. })), "0.8.1 shipped no FreeBSD file");
    }
}
