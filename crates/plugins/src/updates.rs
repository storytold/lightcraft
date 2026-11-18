//! Plug-in updates (host feature `updates`): the feed a manifest's `updateUrl` names, and the
//! update it offers ([`read_feed`]). The feed isn't signed: it only says what to show before
//! anything is downloaded. What installs is the downloaded file, checked against the feed's
//! SHA-256, its own stamped manifest and the author key pinned for the plug-in
//! ([`crate::Manager::install_update`]).
//!
//! A feed is one JSON object holding an entry per plug-in, under any key (a folder name), matched
//! to the plug-in by the entry's `id`:
//!
//! ```json
//! {"geotag": {"id": "org.example.geotag", "version": "1.3.0",
//!             "url": "https://example.org/geotag-1.3.0.wasm", "sha256": "<64 hex digits>",
//!             "requires": ["settings"], "permissions": ["catalog", "network"],
//!             "changelog": "https://example.org/geotag/CHANGELOG.md", "notes": "Faster lookups."}}
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::about::{hidden, link_field};
use crate::manifest::{MAX_REQUIRES, parse_version, valid_id};
use crate::permissions::{Group, Permissions, Request};

/// Largest feed the host reads.
pub const MAX_FEED_BYTES: u64 = 1024 * 1024;
/// Most characters of an entry's `notes` shown, the ellipsis included.
pub const MAX_NOTES_CHARS: usize = 280;
/// How long a check's result stands before opening the Plug-in Manager checks again.
pub const CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;
/// The longest one fetch (a feed or an update's file) may take, start to end: a host that keeps
/// sending a byte now and then can't hold the update worker longer.
pub const FETCH_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5 * 60);
/// Longest `notes` text read before it is cut (a feed is capped anyway).
const MAX_NOTES_BYTES: usize = 64 * 1024;

/// A newer version a plug-in's feed offers, as the Plug-in Manager shows it before downloading.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Offer {
    pub version: String,
    /// The `.wasm` to download (`https://`).
    pub url: String,
    /// The file's SHA-256, lowercase hex.
    pub sha256: String,
    /// Host features that version needs, as its manifest names them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    /// The permission groups that version asks for, as the feed declares them: shown before the
    /// download, never trusted (the file's own manifest decides).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<String>,
    /// Its changelog (`https://`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changelog: Option<String>,
    /// What's new, as plain text of at most [`MAX_NOTES_CHARS`] characters ([`plain_notes`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl Offer {
    /// The host features in [`Offer::requires`] that `features` lacks: such an update is shown but
    /// can't be installed.
    pub fn missing(&self, features: &[String]) -> Vec<String> {
        self.requires.iter().filter(|r| !features.contains(r)).cloned().collect()
    }

    /// The declared permission groups that `installed` (the installed version's request) has none
    /// of.
    pub fn adds(&self, installed: &Permissions) -> Vec<String> {
        self.permissions.iter().filter(|g| Group::parse(g).is_some_and(|g| !installed.holds(&Request::group(g)))).cloned().collect()
    }
}

/// The update feed `feed` offers plug-in `id` over its `installed` version: `Ok(None)` when it
/// offers nothing newer. An entry that can't be used (an unparsable version, a field it can't
/// use, or two entries with the id) offers nothing, with a line pushed onto `log` saying why; a
/// feed that isn't a JSON object of entries is an error.
pub fn read_feed(feed: &[u8], id: &str, installed: &str, log: &mut Vec<String>) -> Result<Option<Offer>, String> {
    if feed.len() as u64 > MAX_FEED_BYTES {
        return Err(format!("the update feed is {} bytes (limit {MAX_FEED_BYTES})", feed.len()));
    }
    let v: Value = serde_json::from_slice(feed).map_err(|e| format!("the update feed isn't JSON: {e}"))?;
    let Value::Object(entries) = v else { return Err("the update feed isn't a JSON object of entries".into()) };
    let mut mine = entries.values().filter(|e| e.get("id").and_then(Value::as_str) == Some(id));
    let Some(entry) = mine.next() else { return Ok(None) };
    if mine.next().is_some() {
        log.push(format!("update feed: more than one entry has the id {id:?}; none is offered"));
        return Ok(None);
    }
    let installed = parse_version(installed).map_err(|e| format!("the installed version: {e}"))?;
    match read_entry(entry) {
        Ok(offer) => match parse_version(&offer.version) {
            Ok(v) if v > installed => Ok(Some(offer)),
            Ok(_) => Ok(None),
            Err(e) => {
                log.push(format!("update feed: the entry for {id:?} is ignored: {e}"));
                Ok(None)
            }
        },
        Err(e) => {
            log.push(format!("update feed: the entry for {id:?} is ignored: {e}"));
            Ok(None)
        }
    }
}

fn read_entry(e: &Value) -> Result<Offer, String> {
    let text = |key: &str| -> Result<&str, String> { e.get(key).and_then(Value::as_str).ok_or_else(|| format!("no `{key}` text")) };
    let version = text("version")?.trim().to_string();
    let mut dropped = Vec::new();
    let url = link_field(e, "url", &mut dropped).ok_or_else(|| dropped.pop().unwrap_or_else(|| "no `url`".into()))?;
    let sha256 = text("sha256")?.trim().to_ascii_lowercase();
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("`sha256` isn't 64 hex digits".into());
    }
    let names = |key: &str| -> Result<Vec<String>, String> {
        match e.get(key) {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(Value::Array(a)) if a.len() <= MAX_REQUIRES => a
                .iter()
                .map(|n| n.as_str().filter(|n| valid_id(n)).map(str::to_string).ok_or_else(|| format!("`{key}` holds a name that isn't valid")))
                .collect(),
            Some(_) => Err(format!("`{key}` isn't a list of at most {MAX_REQUIRES} names")),
        }
    };
    let requires = names("requires")?;
    let mut permissions = names("permissions")?;
    permissions.retain(|g| Group::parse(g).is_some());
    permissions.dedup();
    // a changelog link the host can't use is left out, as a manifest's is
    let changelog = link_field(e, "changelog", &mut Vec::new());
    let notes = match e.get("notes") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => plain_notes(s),
        Some(_) => return Err("`notes` isn't text".into()),
    };
    Ok(Offer { version, url, sha256, requires, permissions, changelog, notes })
}

/// `notes` as the Plug-in Manager shows it: plain text on one line (each run of white space one
/// space, control and invisible characters gone), cut to [`MAX_NOTES_CHARS`] characters on a word
/// boundary with an ellipsis. Markup and links stay as the text they are. `None` when nothing is
/// left.
pub fn plain_notes(notes: &str) -> Option<String> {
    let head = notes.get(..notes.floor_char_boundary(MAX_NOTES_BYTES)).unwrap_or_default();
    let words: Vec<String> =
        head.split_whitespace().map(|w| w.chars().filter(|c| !hidden(*c)).collect::<String>()).filter(|w| !w.is_empty()).collect();
    let text = words.join(" ");
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= MAX_NOTES_CHARS && head.len() == notes.len() {
        return Some(text);
    }
    // room for the ellipsis; back to the last space when there is one in the second half
    let keep: String = text.chars().take(MAX_NOTES_CHARS - 1).collect();
    let cut = match keep.rfind(' ') {
        Some(i) if i >= keep.len() / 2 => keep.get(..i).unwrap_or(&keep),
        _ => &keep,
    };
    Some(format!("{}…", cut.trim_end()))
}

/// Gets a URL's body: the feed, then the update's file. The app's is [`Net`]; tests serve their
/// own.
pub trait Fetch: Send + Sync + std::fmt::Debug {
    /// The body of `url` (an `https://` link), refused past `max` bytes.
    fn get(&self, url: &str, max: u64) -> Result<Vec<u8>, String>;
}

/// [`Fetch`] over the network: `https://` only (a redirect to `http://` is refused), a response
/// other than 2xx is an error, and the whole request ends within [`FETCH_DEADLINE`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Net;

#[cfg(not(target_arch = "wasm32"))]
impl Fetch for Net {
    fn get(&self, url: &str, max: u64) -> Result<Vec<u8>, String> {
        if crate::about::link_host(url).is_none() {
            return Err(format!("{}: not an https:// link", url.chars().take(200).collect::<String>()));
        }
        fetch(url, max, FETCH_DEADLINE)
    }
}

/// Gets `url`'s body, at most `max` bytes, the whole request ending within `deadline`.
#[cfg(not(target_arch = "wasm32"))]
fn fetch(url: &str, max: u64, deadline: std::time::Duration) -> Result<Vec<u8>, String> {
    use lcc_net::{Client, ClientConfig};
    let config = ClientConfig {
        max_body: max,
        connect_timeout: std::time::Duration::from_secs(10),
        stall_timeout: std::time::Duration::from_secs(30),
        ..ClientConfig::default()
    };
    let client = Client::new(config).map_err(|e| e.to_string())?;
    within_deadline(deadline, |cancel| {
        let resp = client.get(url).cancel(cancel.clone()).send().map_err(|e| e.to_string())?;
        if !(200..300).contains(&resp.status) {
            return Err(format!("{url}: HTTP {}", resp.status));
        }
        resp.bytes().map_err(|e| e.to_string())
    })
}

/// Runs `work`, raising the cancel flag it is given once `deadline` has passed (a watchdog thread
/// that leaves as soon as `work` returns); work stopped that way is an error saying it took too
/// long.
#[cfg(not(target_arch = "wasm32"))]
fn within_deadline<T>(
    deadline: std::time::Duration,
    work: impl FnOnce(&std::sync::Arc<std::sync::atomic::AtomicBool>) -> Result<T, String>,
) -> Result<T, String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    let cancel = Arc::new(AtomicBool::new(false));
    let (done, finished) = mpsc::channel::<()>();
    let flag = cancel.clone();
    let watchdog = std::thread::Builder::new()
        .name("plugin-update-deadline".into())
        .spawn(move || {
            if finished.recv_timeout(deadline) == Err(mpsc::RecvTimeoutError::Timeout) {
                flag.store(true, Ordering::Relaxed);
            }
        })
        .map_err(|e| e.to_string())?;
    let result = work(&cancel);
    drop(done);
    // it has nothing left to wait for: a join that fails only means it ended oddly
    let _ = watchdog.join();
    if cancel.load(Ordering::Relaxed) {
        return Err(format!("it took longer than {} minutes", deadline.as_secs() / 60));
    }
    result
}

#[cfg(target_arch = "wasm32")]
impl Fetch for Net {
    fn get(&self, _: &str, _: u64) -> Result<Vec<u8>, String> {
        Err("plug-in updates are not available in the web build".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn entry(version: &str) -> Value {
        json!({"id": "org.x.geo", "version": version, "url": "https://example.org/geo.wasm", "sha256": SHA})
    }

    fn read(feed: &Value, installed: &str) -> (Result<Option<Offer>, String>, Vec<String>) {
        let mut log = Vec::new();
        (read_feed(feed.to_string().as_bytes(), "org.x.geo", installed, &mut log), log)
    }

    #[test]
    fn a_newer_entry_matched_by_its_id_is_offered_whatever_its_key() {
        let feed = json!({"geo-folder": entry("1.2.0"), "other": {"id": "org.x.other", "version": "9.0.0"}, "schema": 2});
        let (offer, log) = read(&feed, "1.1.9");
        let offer = offer.unwrap().unwrap();
        assert_eq!((offer.version.as_str(), offer.url.as_str(), log.len()), ("1.2.0", "https://example.org/geo.wasm", 0));
        // the key alone is no match
        let (offer, _) = read(&json!({"org.x.geo": {"version": "2.0.0"}}), "1.0.0");
        assert_eq!(offer, Ok(None));
    }

    /// AC-7: only a version above the installed one is offered; a stale or replayed feed offers
    /// nothing; an unparsable version and a repeated id offer nothing and say so in the log.
    #[test]
    fn only_a_newer_parsable_single_entry_is_offered() {
        for installed in ["1.2.0", "1.3.0"] {
            assert_eq!(read(&json!({"a": entry("1.2.0")}), installed), (Ok(None), vec![]), "{installed}");
        }
        let (offer, log) = read(&json!({"a": entry("2.x")}), "1.0.0");
        assert_eq!(offer, Ok(None));
        assert!(log.len() == 1 && log[0].contains("not semver"), "{log:?}");
        let (offer, log) = read(&json!({"a": entry("2.0.0"), "b": entry("3.0.0")}), "1.0.0");
        assert_eq!(offer, Ok(None));
        assert!(log.len() == 1 && log[0].contains("more than one entry"), "{log:?}");
    }

    #[test]
    fn notes_are_plain_text_cut_on_a_word_boundary() {
        assert_eq!(plain_notes("  Faster\n\n lookups.\u{202E} "), Some("Faster lookups.".into()));
        assert_eq!(plain_notes(" \u{200B} "), None);
        let long = "word ".repeat(100);
        let cut = plain_notes(&long).unwrap();
        assert!(cut.chars().count() <= MAX_NOTES_CHARS && cut.ends_with("word…"), "{cut}");
        // one long word is cut where it has to be
        let cut = plain_notes(&"x".repeat(1000)).unwrap();
        assert_eq!(cut.chars().count(), MAX_NOTES_CHARS);
        // markup and links are kept as text
        assert_eq!(plain_notes("<b>See</b> [this](https://x.org)").as_deref(), Some("<b>See</b> [this](https://x.org)"));
    }

    #[test]
    fn declared_groups_and_missing_features() {
        let mut e = entry("2.0.0");
        e["permissions"] = json!(["catalog", "network", "futureGroup"]);
        e["requires"] = json!(["settings", "teleport"]);
        let offer = read(&json!({"a": e}), "1.0.0").0.unwrap().unwrap();
        assert_eq!(offer.permissions, ["catalog", "network"], "unknown groups are left out");
        let installed = Permissions { catalog: true, ..Permissions::default() };
        assert_eq!(offer.adds(&installed), ["network"]);
        assert_eq!(offer.missing(&["settings".to_string()]), ["teleport"]);
    }

    /// A fetch that runs past its deadline (a host sending a byte now and then) is stopped.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_fetch_past_its_deadline_is_stopped_and_a_quick_one_is_not() {
        use std::sync::atomic::Ordering;
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        // a host trickling bytes: it goes on until the flag is raised
        let slow = within_deadline(Duration::from_millis(200), |cancel| {
            while !cancel.load(Ordering::Relaxed) {
                if t0.elapsed() > Duration::from_secs(20) {
                    return Ok("never stopped");
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err("cancelled".to_string())
        });
        assert!(slow.unwrap_err().contains("took longer than"), "stopped at the deadline");
        assert!(t0.elapsed() < Duration::from_secs(10));
        let t1 = Instant::now();
        assert_eq!(within_deadline(Duration::from_secs(60), |_| Ok(7)), Ok(7));
        assert!(t1.elapsed() < Duration::from_secs(5), "the watchdog leaves when the work is done");
    }

    /// A host that sends its body a byte at a time (each well inside the stall timeout) is cut off at
    /// the fetch's deadline.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_trickling_host_is_cut_off_at_the_deadline() {
        use std::io::{Read, Write};
        use std::time::{Duration, Instant};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n");
            // 100 s of body unless the client goes
            for _ in 0..1000 {
                if s.write_all(b"x").is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        let t0 = Instant::now();
        let r = fetch(&format!("http://127.0.0.1:{port}/feed.json"), MAX_FEED_BYTES, Duration::from_millis(800));
        assert!(r.unwrap_err().contains("took longer than"), "stopped at the deadline");
        assert!(t0.elapsed() < Duration::from_secs(20), "well before the host was done: {:?}", t0.elapsed());
        let _ = server.join();
    }

    /// AC-7: a malformed feed or entry offers nothing and never panics (beside the hostile-input
    /// tests: `robust.rs` runs every truncation of a feed).
    #[test]
    fn hostile_feeds_offer_nothing() {
        let bad_feeds: [&[u8]; 6] = [b"", b"[]", b"\"x\"", b"{", b"\xff\xfe", b"{\"a\": 1}"];
        for f in bad_feeds {
            let r = read_feed(f, "org.x.geo", "1.0.0", &mut Vec::new());
            assert!(matches!(r, Err(_) | Ok(None)), "{f:?}: {r:?}");
        }
        let huge = vec![b' '; MAX_FEED_BYTES as usize + 1];
        assert!(read_feed(&huge, "org.x.geo", "1.0.0", &mut Vec::new()).is_err());
        let fields = [
            ("url", json!("http://example.org/geo.wasm")),
            ("url", json!(7)),
            ("sha256", json!("abc")),
            ("sha256", json!(null)),
            ("version", json!(2)),
            ("requires", json!("settings")),
            ("requires", json!(["bad name"])),
            ("permissions", json!(vec!["catalog"; 100])),
            ("notes", json!({"text": "x"})),
        ];
        for (key, value) in fields {
            let mut e = entry("2.0.0");
            e[key] = value.clone();
            let (offer, log) = read(&json!({"a": e}), "1.0.0");
            assert_eq!(offer, Ok(None), "{key}: {value}");
            assert_eq!(log.len(), 1, "{key}: {value}: {log:?}");
        }
    }
}
