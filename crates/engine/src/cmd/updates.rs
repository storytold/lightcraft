//! Check for Updates (issue #1553): ask GitHub for the latest published LightCraft release and say
//! whether it is newer than the running version. Only when the user asks (Help ▸ Check for
//! Updates…, `app.checkForUpdates`): nothing is checked at startup and nothing is downloaded or
//! installed; a newer release is offered as a link to its page. The request carries no photo or
//! library data.
//!
//! Sources: GitHub REST API "Get the latest release" (prose); own design.

use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};

use super::{CommandSpec, always, cmd};
use crate::{Result, Session};

/// GitHub's latest published release (drafts and pre-releases excluded).
pub const RELEASES_API: &str = "https://api.github.com/repos/storytold/lightcraft/releases/latest";
/// The page offered when the release names no page of its own.
pub const RELEASES_PAGE: &str = "https://github.com/storytold/lightcraft/releases/latest";
/// Only release pages of this repository are offered for opening.
const RELEASE_PAGE_PREFIX: &str = "https://github.com/storytold/lightcraft/releases/";

/// How a check ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// `latest` is newer than the running version; `url` is its release page.
    Available { latest: String, url: String },
    /// The running version is the latest (or newer: a development build).
    UpToDate { latest: String },
    /// Failed, in words the user can act on.
    Failed(String),
}

impl UpdateOutcome {
    fn to_json(&self) -> Value {
        match self {
            UpdateOutcome::Available { latest, url } => json!({"newer": true, "latest": latest, "url": url}),
            UpdateOutcome::UpToDate { latest } => json!({"newer": false, "latest": latest}),
            UpdateOutcome::Failed(e) => json!({"error": e}),
        }
    }
}

#[derive(Default)]
struct Shared {
    checking: bool,
    last: Option<UpdateOutcome>,
    /// The last outcome, until the UI has shown it ([`Updates::take_outcome`]).
    unreported: Option<UpdateOutcome>,
}

/// The session's update check.
#[derive(Default)]
pub struct Updates {
    shared: Arc<Mutex<Shared>>,
}

impl Updates {
    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A check that ended since the last call (the UI shows it once).
    pub fn take_outcome(&self) -> Option<UpdateOutcome> {
        self.lock().unreported.take()
    }

    pub fn checking(&self) -> bool {
        self.lock().checking
    }
}

/// `major.minor.patch` of a version or tag (`v0.7.1`, `0.7.1`); a pre-release or build suffix is
/// ignored, a missing minor or patch is 0.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim();
    let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
    let core = s.split(['-', '+']).next().unwrap_or_default();
    let mut parts = core.split('.');
    let mut next = || -> Option<u64> {
        match parts.next() {
            None => Some(0),
            Some(p) => p.parse().ok(),
        }
    };
    let v = (next()?, next()?, next()?);
    parts.next().is_none().then_some(v)
}

/// What a release (the GitHub API's JSON) means for `current`.
pub fn outcome_for(release: &str, current: &str) -> UpdateOutcome {
    let Ok(r) = serde_json::from_str::<Value>(release) else {
        return UpdateOutcome::Failed("GitHub's answer could not be read. Try again later.".into());
    };
    let Some(tag) = r.get("tag_name").and_then(Value::as_str) else {
        return UpdateOutcome::Failed("GitHub's answer names no release. Try again later.".into());
    };
    let (Some(latest), Some(running)) = (parse_version(tag), parse_version(current)) else {
        return UpdateOutcome::Failed(format!("The latest release's version ({tag}) could not be read."));
    };
    let shown = format!("{}.{}.{}", latest.0, latest.1, latest.2);
    if latest <= running {
        return UpdateOutcome::UpToDate { latest: shown };
    }
    let url = r
        .get("html_url")
        .and_then(Value::as_str)
        .filter(|u| u.starts_with(RELEASE_PAGE_PREFIX) && !u.contains(char::is_whitespace))
        .unwrap_or(RELEASES_PAGE)
        .to_string();
    UpdateOutcome::Available { latest: shown, url }
}

fn status(s: &mut Session, _p: &Value) -> Result<Value> {
    let sh = s.updates.lock();
    Ok(json!({
        "current": env!("CARGO_PKG_VERSION"),
        "checking": sh.checking,
        "last": sh.last.as_ref().map(UpdateOutcome::to_json),
    }))
}

#[cfg(not(target_arch = "wasm32"))]
mod net {
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use lightcraft_fetch::http::{self, Limits, Url};

    use super::*;
    use crate::activity::Cancel;
    use crate::cmd::{bad, str_param};

    /// A release's JSON is a few KB plus its notes; anything this big is not one.
    const MAX_BODY: usize = 2 * 1024 * 1024;

    /// An endpoint: https, or plain http to this machine's loopback address only (a test server).
    pub(super) fn endpoint_allowed(url: &str) -> bool {
        url.starts_with("https://")
            || url.strip_prefix("http://127.0.0.1").is_some_and(|rest| rest.is_empty() || rest.starts_with(':') || rest.starts_with('/'))
    }

    /// `GET endpoint` → the response body.
    fn fetch(endpoint: &str, cancel: &AtomicBool) -> std::result::Result<String, String> {
        let url = Url::parse(endpoint).map_err(|e| e.to_string())?;
        let limits = Limits { connect: Duration::from_secs(15), stall: Duration::from_secs(20), cancel };
        let headers = [("Accept", "application/vnd.github+json".to_string())];
        let mut res = http::get(&url, &headers, &limits).map_err(|e| e.to_string())?;
        match res.status {
            200 => {}
            404 => return Err("no published release was found".into()),
            403 | 429 => return Err("GitHub is limiting requests from this network for now".into()),
            s => return Err(format!("GitHub answered with status {s}")),
        }
        let mut body = Vec::new();
        let mut buf = [0u8; 16 * 1024];
        loop {
            let n = res.read(&mut buf, &limits).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(buf.get(..n).unwrap_or_default());
            if body.len() > MAX_BODY {
                return Err("GitHub's answer is unexpectedly large".into());
            }
        }
        String::from_utf8(body).map_err(|_| "GitHub's answer is not text".to_string())
    }

    /// `app.checkForUpdates`: starts the check in the background (shown in the activity stack).
    pub(super) fn start(s: &mut Session, p: &Value) -> Result<Value> {
        const C: &str = "app.checkForUpdates";
        let endpoint = str_param(p, "endpoint").unwrap_or(RELEASES_API).to_string();
        if !endpoint_allowed(&endpoint) {
            return Err(bad(C, "`endpoint` must be an https:// address"));
        }
        let shared = s.updates.shared.clone();
        {
            let mut sh = s.updates.lock();
            if sh.checking {
                return Ok(json!({"started": false, "checking": true}));
            }
            sh.checking = true;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let task = s.activity.start("updates", "Checking for updates", Cancel::Flag(cancel.clone()));
        let thread_shared = shared.clone();
        let spawned = std::thread::Builder::new().name("update-check".into()).spawn(move || {
            // the activity row goes when the thread ends, however it ends
            let _task = task;
            let fetched = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fetch(&endpoint, &cancel)));
            let cancelled = cancel.load(std::sync::atomic::Ordering::Relaxed);
            let outcome = match fetched {
                _ if cancelled => None,
                Ok(Ok(body)) => Some(outcome_for(&body, env!("CARGO_PKG_VERSION"))),
                Ok(Err(e)) => Some(UpdateOutcome::Failed(format!("Could not check for updates: {e}. Check the connection and try again."))),
                Err(_) => Some(UpdateOutcome::Failed("The update check stopped unexpectedly. Try again.".into())),
            };
            if let Some(UpdateOutcome::Failed(e)) = &outcome {
                log::warn!("{e}");
            }
            let mut sh = thread_shared.lock().unwrap_or_else(PoisonError::into_inner);
            sh.checking = false;
            if let Some(o) = outcome {
                sh.last = Some(o.clone());
                sh.unreported = Some(o);
            }
        });
        if let Err(e) = spawned {
            shared.lock().unwrap_or_else(PoisonError::into_inner).checking = false;
            return Err(bad(C, format!("could not start the update check: {e}")));
        }
        Ok(json!({"started": true, "checking": true}))
    }
}

pub fn specs() -> Vec<CommandSpec> {
    #[allow(unused_mut)]
    let mut v = vec![cmd!(
        query "app.updateStatus",
        "Update Status",
        [],
        None,
        "{} → {current, checking, last: {newer, latest, url} | {error} | null} — the running version and how the last Check for Updates ended",
        always,
        status
    )];
    #[cfg(not(target_arch = "wasm32"))]
    v.push(cmd!(
        query "app.checkForUpdates",
        "Check for Updates…",
        ["Help"],
        None,
        "{} → {started, checking} — asks GitHub for the latest LightCraft release in the background (no photo or library data is sent; nothing is downloaded); `app.updateStatus` reports the result",
        always,
        net::start
    ));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_with_or_without_v_and_suffixes() {
        assert_eq!(parse_version("v0.7.1"), Some((0, 7, 1)));
        assert_eq!(parse_version("0.7.1"), Some((0, 7, 1)));
        assert_eq!(parse_version(" V1.2 "), Some((1, 2, 0)));
        assert_eq!(parse_version("1.0.0-beta.2+build.5"), Some((1, 0, 0)));
        assert_eq!(parse_version("0.10.0"), Some((0, 10, 0)));
        for bad in ["", "v", "latest", "1.x.0", "1.2.3.4", "1..2"] {
            assert_eq!(parse_version(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn newer_releases_are_offered_with_their_page() {
        let r = r#"{"tag_name": "v0.10.0", "html_url": "https://github.com/storytold/lightcraft/releases/tag/v0.10.0"}"#;
        // numeric, not string, comparison: 0.10 is newer than 0.9
        assert_eq!(
            outcome_for(r, "0.9.3"),
            UpdateOutcome::Available { latest: "0.10.0".into(), url: "https://github.com/storytold/lightcraft/releases/tag/v0.10.0".into() }
        );
        assert_eq!(outcome_for(r, "0.10.0"), UpdateOutcome::UpToDate { latest: "0.10.0".into() });
        // a development build ahead of the release is up to date
        assert_eq!(outcome_for(r, "0.11.0"), UpdateOutcome::UpToDate { latest: "0.10.0".into() });
    }

    #[test]
    fn only_this_repositorys_release_pages_are_offered() {
        for url in [
            "https://evil.example/releases/v9",
            "https://github.com/someone/else/releases/tag/v9",
            "https://github.com/storytold/lightcraft/releases/ x",
        ] {
            let r = json!({"tag_name": "v9.0.0", "html_url": url}).to_string();
            assert_eq!(outcome_for(&r, "0.7.1"), UpdateOutcome::Available { latest: "9.0.0".into(), url: RELEASES_PAGE.into() }, "{url}");
        }
        let r = json!({"tag_name": "v9.0.0"}).to_string();
        assert!(matches!(outcome_for(&r, "0.7.1"), UpdateOutcome::Available { url, .. } if url == RELEASES_PAGE));
    }

    #[test]
    fn unreadable_answers_are_errors_not_panics() {
        for body in ["", "<html>rate limited</html>", "{}", r#"{"tag_name": 7}"#, r#"{"tag_name": "nightly"}"#, "[1,2]"] {
            assert!(matches!(outcome_for(body, "0.7.1"), UpdateOutcome::Failed(_)), "{body:?}");
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn endpoint_is_https_or_loopback_only() {
        assert!(net::endpoint_allowed(RELEASES_API));
        assert!(net::endpoint_allowed("http://127.0.0.1:8080/latest"));
        assert!(!net::endpoint_allowed("http://127.0.0.1.example.com/"));
        assert!(!net::endpoint_allowed("http://api.github.com/"));
        let mut s = Session::with_demo();
        assert!(s.execute("app.checkForUpdates", &json!({"endpoint": "http://example.com/"})).is_err());
        assert!(!s.updates.checking());
    }

    /// A local server answering every request with `status` and `body`.
    #[cfg(not(target_arch = "wasm32"))]
    fn serve(status: &'static str, body: String) -> String {
        use std::io::{BufRead, BufReader, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut c) = conn else { continue };
                let mut r = BufReader::new(c.try_clone().unwrap());
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                }
                let _ = c.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
                let _ = c.write_all(body.as_bytes());
            }
        });
        format!("http://127.0.0.1:{port}/latest")
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn check(s: &mut Session, endpoint: &str) -> UpdateOutcome {
        let r = s.execute("app.checkForUpdates", &json!({"endpoint": endpoint})).unwrap();
        assert_eq!(r["started"], true);
        let started = std::time::Instant::now();
        loop {
            if let Some(o) = s.updates.take_outcome() {
                // the activity row ends with the thread
                while !s.activity.list().is_empty() && started.elapsed().as_secs() < 30 {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                assert!(s.activity.list().is_empty());
                assert!(s.updates.take_outcome().is_none(), "reported once");
                return o;
            }
            assert!(started.elapsed().as_secs() < 60, "the check never ended");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn check_end_to_end_and_its_failures() {
        let mut s = Session::with_demo();
        let st = s.execute("app.updateStatus", &json!({})).unwrap();
        assert_eq!((st["current"].as_str(), st["checking"].as_bool(), st["last"].is_null()), (Some(env!("CARGO_PKG_VERSION")), Some(false), true));

        let newer = json!({"tag_name": "v999.0.0", "html_url": "https://github.com/storytold/lightcraft/releases/tag/v999.0.0"}).to_string();
        let o = check(&mut s, &serve("200 OK", newer));
        assert_eq!(
            o,
            UpdateOutcome::Available { latest: "999.0.0".into(), url: "https://github.com/storytold/lightcraft/releases/tag/v999.0.0".into() }
        );
        let st = s.execute("app.updateStatus", &json!({})).unwrap();
        assert_eq!(
            (st["checking"].as_bool(), st["last"]["newer"].as_bool(), st["last"]["latest"].as_str()),
            (Some(false), Some(true), Some("999.0.0"))
        );

        let o = check(&mut s, &serve("200 OK", json!({"tag_name": "v0.0.1"}).to_string()));
        assert_eq!(o, UpdateOutcome::UpToDate { latest: "0.0.1".into() });

        for (status, why) in
            [("404 Not Found", "no published release"), ("403 Forbidden", "limiting requests"), ("500 Internal Server Error", "status 500")]
        {
            let UpdateOutcome::Failed(e) = check(&mut s, &serve(status, "{}".into())) else { panic!("{status}") };
            assert!(e.contains(why) && e.contains("try again"), "{status}: {e}");
            let st = s.execute("app.updateStatus", &json!({})).unwrap();
            assert_eq!(st["last"]["error"].as_str(), Some(e.as_str()));
        }
        let UpdateOutcome::Failed(e) = check(&mut s, &serve("200 OK", "<html>not json</html>".into())) else { panic!("garbage") };
        assert!(e.contains("could not be read"), "{e}");
    }
}
