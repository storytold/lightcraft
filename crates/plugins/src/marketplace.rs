//! The plug-in marketplace: the index the app fetches ([`read_index`]), what it lists
//! ([`Entry`]), and what the app remembers about it between runs ([`Store`]).
//!
//! The index is one JSON file, rebuilt by the marketplace's own repository from each plug-in's
//! newest release, and fetched as it is now: no version is pinned here. It isn't signed, so
//! nothing in it is trusted beyond what it shows. What installs is the downloaded file, which must
//! match the entry's SHA-256, be the plug-in and version the entry names, and pass every check a
//! hand install passes (the sandbox's module checks, `requires`, the pinned author key).
//!
//! ```json
//! {"format": 1, "generated": "2026-11-23T04:17:09Z", "plugins": [
//!   {"id": "org.example.geotag", "name": "Geotag", "version": "1.3.0", "kind": "plugin",
//!    "tier": "vetted", "summary": "Tags photos from a GPX track", "platforms": ["macos", "linux"],
//!    "url": "https://github.com/example/geotag/releases/download/v1.3.0/geotag.wasm",
//!    "sha256": "<64 hex digits>", "size": 231700, "permissions": ["catalog", "network"]}]}
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::about::{hidden, link_field};
use crate::manifest::{MAX_REQUIRES, parse_version, valid_id};
use crate::permissions::Group;

/// The index format this version reads.
pub const FORMAT: u64 = 1;
/// Largest index the host reads.
pub const MAX_INDEX_BYTES: u64 = 8 * 1024 * 1024;
/// Most entries read from an index; the rest are counted, not listed.
pub const MAX_ENTRIES: usize = 4096;
/// How long a fetched index stands before the automatic update pass fetches it again.
pub const REFRESH_INTERVAL_SECS: i64 = 24 * 60 * 60;
/// The platforms an entry may name, in the order they are shown.
pub const PLATFORMS: &[&str] = &["macos", "windows", "linux", "android", "web"];

const MAX_NAME_CHARS: usize = 100;
const MAX_SUMMARY_CHARS: usize = 200;
const MAX_DESCRIPTION_CHARS: usize = 2000;
const MAX_TAGS: usize = 8;
const MAX_VALUES: usize = 64;

/// The platform this build runs on, as an entry's `platforms` names it.
pub fn platform() -> &'static str {
    if cfg!(target_arch = "wasm32") {
        "web"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "android") {
        "android"
    } else {
        // Linux and the BSDs find programs the same way
        "linux"
    }
}

/// How far a listed plug-in has been looked at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// A marketplace maintainer read its source and recorded what it may ask for.
    Vetted,
    /// Automatic checks only: nobody vouches for what it does.
    Community,
}

impl Tier {
    pub fn name(self) -> &'static str {
        match self {
            Tier::Vetted => "vetted",
            Tier::Community => "community",
        }
    }
}

/// Which host runs a listed module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Plugin,
    /// A pixel filter: no imports, no permissions.
    Filter,
}

/// One listed plug-in or pixel filter, every field checked ([`read_index`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: Kind,
    pub tier: Tier,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Where it can be installed, of [`PLATFORMS`]; never empty.
    pub platforms: Vec<String>,
    /// The `.wasm` to download (`https://`).
    pub url: String,
    /// The file's SHA-256, lowercase hex.
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
    /// Host features that version needs, as its manifest names them.
    #[serde(default)]
    pub requires: Vec<String>,
    /// The permission groups it may ask the user for, as the index declares them: shown before
    /// the download, never trusted (the file's own manifest decides).
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Each host, folder and program (by name) behind those groups.
    #[serde(default)]
    pub network: Vec<String>,
    #[serde(default)]
    pub fs: Vec<String>,
    #[serde(default)]
    pub programs: Vec<String>,
    /// The fingerprint of the key the index says signed the file.
    #[serde(default)]
    pub signer: Option<String>,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub changelog: Option<String>,
    /// When that version was released, as the index gives it (an RFC 3339 time).
    #[serde(default)]
    pub released: String,
    /// A vetted entry's reviewer and review date.
    #[serde(default)]
    pub reviewed_by: String,
    #[serde(default)]
    pub reviewed_on: String,
}

impl Entry {
    /// Whether it can be installed on `platform`.
    pub fn supports(&self, platform: &str) -> bool {
        self.platforms.iter().any(|p| p == platform)
    }

    /// The host features in [`Entry::requires`] that `features` lacks.
    pub fn missing(&self, features: &[String]) -> Vec<String> {
        self.requires.iter().filter(|r| !features.contains(r)).cloned().collect()
    }

    /// Whether every word of `query` is found in its name, id, author, summary, description or
    /// tags, whatever the case. An empty query matches.
    pub fn matches(&self, query: &str) -> bool {
        let hay = [&self.name, &self.id, &self.author, &self.summary, &self.description].map(|s| s.to_lowercase());
        let tags: Vec<String> = self.tags.iter().map(|t| t.to_lowercase()).collect();
        query.split_whitespace().take(16).all(|word| {
            let word = word.to_lowercase();
            hay.iter().chain(&tags).any(|h| h.contains(&word))
        })
    }
}

/// A fetched index.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Index {
    /// When an entry last changed, as the index says.
    #[serde(default)]
    pub generated: String,
    /// The marketplace's own page (`https://`).
    #[serde(default)]
    pub source: Option<String>,
    pub entries: Vec<Entry>,
}

impl Index {
    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }
}

/// Reads an index. An entry the host can't use (a field missing or of the wrong shape, an id two
/// entries share) is left out, with a line pushed onto `log` saying why; an index that isn't a
/// JSON object of this format is an error.
pub fn read_index(bytes: &[u8], log: &mut Vec<String>) -> Result<Index, String> {
    if bytes.len() as u64 > MAX_INDEX_BYTES {
        return Err(format!("the marketplace index is {} bytes (limit {MAX_INDEX_BYTES})", bytes.len()));
    }
    let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("the marketplace index isn't JSON: {e}"))?;
    let Value::Object(doc) = v else { return Err("the marketplace index isn't a JSON object".into()) };
    match doc.get("format").and_then(Value::as_u64) {
        Some(FORMAT) => {}
        Some(f) => return Err(format!("the marketplace index is format {f}; this version of the app reads format {FORMAT}: update the app")),
        None => return Err("the marketplace index has no `format`".into()),
    }
    let Some(Value::Array(list)) = doc.get("plugins") else { return Err("the marketplace index has no `plugins` list".into()) };
    if list.len() > MAX_ENTRIES {
        log.push(format!("marketplace: the index lists {} entries; the first {MAX_ENTRIES} are read", list.len()));
    }
    let mut entries: Vec<Entry> = Vec::new();
    let mut twice = BTreeSet::new();
    for (i, raw) in list.iter().take(MAX_ENTRIES).enumerate() {
        match read_entry(raw) {
            Ok(e) if entries.iter().any(|o| o.id == e.id) => {
                twice.insert(e.id);
            }
            Ok(e) => entries.push(e),
            Err(why) => {
                let id = raw.get("id").and_then(Value::as_str).map(|s| plain(s, 64)).unwrap_or_default();
                log.push(format!("marketplace: entry {i} ({id:?}) is left out: {why}"));
            }
        }
    }
    for id in &twice {
        log.push(format!("marketplace: more than one entry has the id {id:?}; none is listed"));
    }
    entries.retain(|e| !twice.contains(&e.id));
    let generated = doc.get("generated").and_then(Value::as_str).map(|s| plain(s, 40)).unwrap_or_default();
    let source = link_field(&Value::Object(doc.clone()), "source", &mut Vec::new());
    Ok(Index { generated, source, entries })
}

fn read_entry(e: &Value) -> Result<Entry, String> {
    if !e.is_object() {
        return Err("not an object".into());
    }
    let text = |key: &str| -> Result<&str, String> { e.get(key).and_then(Value::as_str).ok_or_else(|| format!("no `{key}` text")) };
    let optional = |key: &str, cap: usize| -> String { e.get(key).and_then(Value::as_str).map(|s| plain(s, cap)).unwrap_or_default() };
    let id = text("id")?.to_string();
    if !valid_id(&id) {
        return Err("`id` isn't 1 to 64 of A-Z a-z 0-9 . _ -".into());
    }
    let name = plain(text("name")?, MAX_NAME_CHARS);
    if name.is_empty() {
        return Err("`name` is empty".into());
    }
    let kind = match text("kind")? {
        "plugin" => Kind::Plugin,
        "filter" => Kind::Filter,
        other => return Err(format!("`kind` is {:?}: plugin or filter", plain(other, 32))),
    };
    let tier = match text("tier")? {
        "vetted" => Tier::Vetted,
        "community" => Tier::Community,
        // a tier this version doesn't know is never shown as vetted
        _ => Tier::Community,
    };
    let version = text("version")?.trim().to_string();
    if version.is_empty() || version.len() > crate::manifest::MAX_VERSION_BYTES || version.chars().any(|c| hidden(c) || c.is_whitespace()) {
        return Err("`version` can't be read".into());
    }
    if kind == Kind::Plugin {
        parse_version(&version)?;
    }
    let mut dropped = Vec::new();
    let url = link_field(e, "url", &mut dropped).ok_or_else(|| dropped.pop().unwrap_or_else(|| "no `url`".into()))?;
    let sha256 = text("sha256")?.trim().to_ascii_lowercase();
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("`sha256` isn't 64 hex digits".into());
    }
    let names = |key: &str, most: usize, ok: &dyn Fn(&str) -> bool| -> Result<Vec<String>, String> {
        match e.get(key) {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(Value::Array(a)) if a.len() <= most => a
                .iter()
                .map(|n| n.as_str().filter(|n| ok(n)).map(str::to_string).ok_or_else(|| format!("`{key}` holds a value that isn't valid")))
                .collect(),
            Some(_) => Err(format!("`{key}` isn't a list of at most {most} values")),
        }
    };
    let texts = |key: &str| -> Vec<String> {
        let list = e.get(key).and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        list.iter().take(MAX_VALUES).filter_map(Value::as_str).map(|s| plain(s, 256)).filter(|s| !s.is_empty()).collect()
    };
    let mut platforms = names("platforms", PLATFORMS.len(), &|p| valid_id(p))?;
    // a platform this version doesn't know isn't this one
    platforms.retain(|p| PLATFORMS.contains(&p.as_str()));
    platforms.dedup();
    if e.get("platforms").is_none() {
        platforms = PLATFORMS.iter().map(|p| p.to_string()).collect();
    }
    if platforms.is_empty() {
        return Err("`platforms` names none this version knows".into());
    }
    let requires = names("requires", MAX_REQUIRES, &|n| valid_id(n))?;
    let mut permissions = names("permissions", MAX_VALUES, &|n| valid_id(n))?;
    permissions.retain(|g| Group::parse(g).is_some());
    permissions.dedup();
    let tags = names("tags", MAX_TAGS, &|t| valid_id(t)).unwrap_or_default();
    let review = e.get("review").filter(|r| r.is_object() && tier == Tier::Vetted);
    let reviewed = |key: &str| review.and_then(|r| r.get(key)).and_then(Value::as_str).map(|s| plain(s, 64)).unwrap_or_default();
    Ok(Entry {
        id,
        name,
        version,
        kind,
        tier,
        summary: optional("summary", MAX_SUMMARY_CHARS),
        description: optional("description", MAX_DESCRIPTION_CHARS),
        author: optional("author", MAX_NAME_CHARS),
        tags,
        platforms,
        url,
        sha256,
        size: e.get("size").and_then(Value::as_u64).unwrap_or(0),
        requires,
        permissions,
        network: texts("network"),
        fs: texts("fs"),
        programs: texts("programs"),
        signer: e.get("signer").and_then(Value::as_str).map(|s| plain(s, 32)).filter(|s| !s.is_empty()),
        license: optional("license", 256),
        // a link the host can't use is left out, as a manifest's is
        homepage: link_field(e, "homepage", &mut Vec::new()),
        repository: link_field(e, "repository", &mut Vec::new()),
        changelog: link_field(e, "changelog", &mut Vec::new()),
        released: optional("released", 40),
        reviewed_by: reviewed("by"),
        reviewed_on: reviewed("date"),
    })
}

/// `text` as the marketplace shows it: plain text on one line (each run of white space one space,
/// control and invisible characters gone), cut to `cap` characters with an ellipsis.
pub fn plain(text: &str, cap: usize) -> String {
    // a few bytes a character at most: nothing past this could be shown anyway
    let head = text.get(..text.floor_char_boundary(cap.saturating_mul(8))).unwrap_or_default();
    let words: Vec<String> =
        head.split_whitespace().map(|w| w.chars().filter(|c| !hidden(*c)).collect::<String>()).filter(|w| !w.is_empty()).collect();
    let joined = words.join(" ");
    if joined.chars().count() <= cap {
        return joined;
    }
    let keep: String = joined.chars().take(cap.saturating_sub(1)).collect();
    format!("{}…", keep.trim_end())
}

/// Whether version `offered` is above `installed`: by semver precedence when both are semver
/// (a pre-release comes before its release), else as dotted numbers part by part (`1.9` before
/// `1.10`, a missing part counting as 0). Versions that can't be compared offer nothing.
pub fn newer(offered: &str, installed: &str) -> bool {
    if let (Ok(o), Ok(i)) = (parse_version(offered), parse_version(installed)) {
        return o.cmp_precedence(&i) == std::cmp::Ordering::Greater;
    }
    let parts = |v: &str| -> Option<Vec<u64>> {
        let parts: Vec<&str> = v.split('.').collect();
        if parts.len() > 8 {
            return None;
        }
        parts.iter().map(|p| p.parse::<u64>().ok()).collect()
    };
    let (Some(mut o), Some(mut i)) = (parts(offered), parts(installed)) else { return false };
    let len = o.len().max(i.len());
    o.resize(len, 0);
    i.resize(len, 0);
    o > i
}

/// The SHA-256 of `bytes`, lowercase hex, as an entry's `sha256` gives it.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// What the app keeps about the marketplace between runs (`marketplace.json` in the plug-ins
/// folder): the user's choices and the last index fetched, so the window has something to show
/// before the network answers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Saved {
    /// Community entries are listed too (off by default: only vetted ones are).
    pub community: bool,
    /// Installed plug-ins update by themselves when an update asks for nothing new.
    pub auto_update: bool,
    /// The plug-ins whose own setting differs from [`Saved::auto_update`].
    pub auto_update_for: BTreeMap<String, bool>,
    /// When the index was last fetched (seconds since 1970), whether or not it could be read.
    pub fetched_at: Option<i64>,
    /// Why the last fetch failed.
    pub error: Option<String>,
    /// The last index that could be read.
    pub index: Option<Index>,
    /// Updates the automatic pass left for the user, by plug-in id: the version and why.
    pub waiting: BTreeMap<String, Waiting>,
}

/// An update the automatic pass didn't install.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Waiting {
    pub version: String,
    pub why: String,
}

impl Saved {
    /// Whether plug-in `id` updates by itself.
    pub fn auto_updates(&self, id: &str) -> bool {
        self.auto_update_for.get(id).copied().unwrap_or(self.auto_update)
    }

    /// Whether the automatic pass should fetch the index again at `now`.
    pub fn refresh_due(&self, now: i64) -> bool {
        self.fetched_at.is_none_or(|at| now.saturating_sub(at) >= REFRESH_INTERVAL_SECS || at > now)
    }
}

/// [`Saved`] and where it is kept: a file in the plug-ins folder, or memory only.
#[derive(Debug, Default)]
pub struct Store {
    path: Option<PathBuf>,
    pub saved: Saved,
}

/// The file's name in the plug-ins folder.
pub const STORE_FILE: &str = "marketplace.json";

impl Store {
    /// Kept in memory only (tests, a session without a plug-ins folder).
    pub fn in_memory() -> Store {
        Store::default()
    }

    /// Reads `<dir>/marketplace.json`. A missing file is empty; one that can't be read is empty
    /// too (the index is fetched again, the choices go back to their defaults), with why.
    pub fn open(dir: &Path) -> (Store, Option<String>) {
        let path = dir.join(STORE_FILE);
        let (saved, why) = match crate::safe_io::read_capped(&path, MAX_INDEX_BYTES.saturating_mul(2)) {
            Ok(bytes) => match serde_json::from_slice::<Saved>(&bytes) {
                Ok(s) => (s, None),
                Err(e) => (Saved::default(), Some(format!("{}: {e}", path.display()))),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Saved::default(), None),
            Err(e) => (Saved::default(), Some(format!("{}: {e}", path.display()))),
        };
        (Store { path: Some(path), saved }, why)
    }

    /// Writes the file (nothing to do in memory).
    pub fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else { return Ok(()) };
        let bytes = serde_json::to_vec(&self.saved).map_err(|e| e.to_string())?;
        crate::safe_io::write_atomic(path, &bytes).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Records a fetch at `at`: the index read from `fetched`, or why not (the last good index
    /// stays). Entries left out are pushed onto `log`.
    pub fn record_fetch(&mut self, at: i64, fetched: Result<Vec<u8>, String>, log: &mut Vec<String>) {
        self.saved.fetched_at = Some(at);
        match fetched.and_then(|bytes| read_index(&bytes, log)) {
            Ok(index) => {
                // an update that is no longer the one offered waits for nothing
                self.saved.waiting.retain(|id, w| index.get(id).is_some_and(|e| e.version == w.version));
                self.saved.index = Some(index);
                self.saved.error = None;
            }
            Err(e) => self.saved.error = Some(plain(&e, 400)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn entry(id: &str) -> Value {
        json!({"id": id, "name": "Geotag", "version": "1.3.0", "kind": "plugin", "tier": "vetted", "platforms": ["macos", "linux"],
            "url": "https://example.org/geo.wasm", "sha256": SHA, "summary": "Tags photos", "tags": ["gps"], "author": "Ann"})
    }

    fn index(entries: Value) -> Vec<u8> {
        json!({"format": 1, "generated": "2026-11-23T04:17:09Z", "source": "https://example.org/market", "plugins": entries}).to_string().into_bytes()
    }

    fn read(entries: Value) -> (Index, Vec<String>) {
        let mut log = Vec::new();
        (read_index(&index(entries), &mut log).unwrap(), log)
    }

    #[test]
    fn an_entry_is_read_field_by_field() {
        let mut e = entry("org.x.geo");
        e["permissions"] = json!(["catalog", "network", "teleport"]);
        e["network"] = json!(["api.example.org"]);
        e["review"] = json!({"by": "bob", "date": "2026-11-01"});
        e["homepage"] = json!("http://insecure.example");
        e["signer"] = json!("3f2a 9c41 07be d5e8");
        let (i, log) = read(json!([e]));
        assert!(log.is_empty(), "{log:?}");
        let e = &i.entries[0];
        assert_eq!((e.tier, e.kind, e.version.as_str()), (Tier::Vetted, Kind::Plugin, "1.3.0"));
        assert_eq!(e.permissions, ["catalog", "network"], "a group this version doesn't know isn't shown");
        assert_eq!(e.homepage, None, "a link that isn't https is left out");
        assert_eq!((e.reviewed_by.as_str(), e.reviewed_on.as_str()), ("bob", "2026-11-01"));
        assert!(e.supports("macos") && !e.supports("windows"));
        assert_eq!(i.source.as_deref(), Some("https://example.org/market"));
    }

    #[test]
    fn search_matches_every_word_in_any_field() {
        let (i, _) = read(json!([entry("org.x.geo")]));
        let e = &i.entries[0];
        for q in ["", "geo", "GEOTAG ann", "gps tags", "org.x"] {
            assert!(e.matches(q), "{q:?}");
        }
        assert!(!e.matches("geotag film"));
    }

    #[test]
    fn no_platforms_means_every_platform_and_an_unknown_one_is_not_this_one() {
        let mut all = entry("a");
        all.as_object_mut().unwrap().remove("platforms");
        let mut future = entry("b");
        future["platforms"] = json!(["visionos", "linux"]);
        let mut none = entry("c");
        none["platforms"] = json!(["visionos"]);
        let (i, log) = read(json!([all, future, none]));
        assert_eq!(i.get("a").unwrap().platforms.len(), PLATFORMS.len());
        assert_eq!(i.get("b").unwrap().platforms, ["linux"]);
        assert!(i.get("c").is_none() && log.len() == 1, "{log:?}");
    }

    #[test]
    fn an_unknown_tier_is_never_vetted() {
        let mut e = entry("a");
        e["tier"] = json!("official");
        e["review"] = json!({"by": "bob"});
        let (i, _) = read(json!([e]));
        assert_eq!((i.entries[0].tier, i.entries[0].reviewed_by.as_str()), (Tier::Community, ""));
    }

    #[test]
    fn two_entries_with_one_id_list_neither() {
        let (i, log) = read(json!([entry("a"), entry("b"), entry("a")]));
        assert_eq!(i.entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["b"]);
        assert!(log.iter().any(|l| l.contains("more than one entry")), "{log:?}");
    }

    /// Hostile and malformed indexes and entries: an error or a skipped entry, never a panic.
    #[test]
    fn hostile_indexes_never_panic() {
        let whole: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"not json".to_vec(),
            b"[]".to_vec(),
            b"{}".to_vec(),
            br#"{"format": 2, "plugins": []}"#.to_vec(),
            br#"{"format": 1, "plugins": {}}"#.to_vec(),
            br#"{"format": "1", "plugins": []}"#.to_vec(),
            vec![0xff; 64],
            "[".repeat(100_000).into_bytes(),
        ];
        for bytes in &whole {
            assert!(read_index(bytes, &mut Vec::new()).is_err(), "{:?}", String::from_utf8_lossy(bytes).get(..40));
        }
        assert!(read_index(&vec![b' '; MAX_INDEX_BYTES as usize + 1], &mut Vec::new()).is_err());
        let mut bad = Vec::new();
        for (key, value) in [
            ("id", json!("has space")),
            ("id", json!(7)),
            ("id", json!("x".repeat(65))),
            ("name", json!("\u{202e}\u{200b}")),
            ("name", Value::Null),
            ("kind", json!("driver")),
            ("version", json!("1.2")),
            ("version", json!("1.0.0\u{202e}")),
            ("version", json!("")),
            ("url", json!("http://example.org/a.wasm")),
            ("url", json!("https://a.com@evil.example/a.wasm")),
            ("url", json!("file:///etc/passwd")),
            ("sha256", json!("abc")),
            ("sha256", json!("z".repeat(64))),
            ("platforms", json!("linux")),
            ("platforms", json!([1, 2])),
            ("requires", json!(["a b"])),
            ("permissions", json!({"catalog": true})),
        ] {
            let mut e = entry("org.x.bad");
            e[key] = value;
            bad.push(e);
        }
        bad.extend([json!(null), json!(3), json!("entry"), json!([]), json!({})]);
        let n = bad.len();
        let (i, log) = read(Value::Array(bad));
        assert!(i.entries.is_empty(), "{:?}", i.entries);
        assert_eq!(log.len(), n, "each one says why: {log:?}");
        // text is shown plain and capped, numbers of the wrong shape read as absent
        let mut odd = entry("org.x.odd");
        odd["summary"] = json!(format!("a\u{202e}b\n\tc {}", "é".repeat(5000)));
        odd["size"] = json!(-4);
        odd["network"] = json!([3, "host.example", {"a": 1}]);
        odd["tags"] = json!("not a list");
        let (i, _) = read(json!([odd]));
        let e = &i.entries[0];
        assert!(e.summary.starts_with("ab c ") && e.summary.chars().count() == MAX_SUMMARY_CHARS && e.summary.ends_with('…'));
        assert_eq!((e.size, e.network.as_slice(), e.tags.len()), (0, ["host.example".to_string()].as_slice(), 0));
    }

    #[test]
    fn more_entries_than_the_cap_are_counted_not_read() {
        let many: Vec<Value> = (0..MAX_ENTRIES + 3).map(|n| entry(&format!("p{n}"))).collect();
        let (i, log) = read(Value::Array(many));
        assert_eq!(i.entries.len(), MAX_ENTRIES);
        assert!(log.iter().any(|l| l.contains("the first")), "{log:?}");
    }

    #[test]
    fn versions_compare_by_semver_then_as_dotted_numbers() {
        for (offered, installed, is) in [
            ("1.3.0", "1.2.9", true),
            ("1.3.0", "1.3.0", false),
            ("1.3.0", "1.3.0-beta.1", true),
            ("1.3.0-beta.1", "1.3.0", false),
            ("1.3.0+build.2", "1.3.0+build.1", false),
            ("0.10", "0.9", true),
            ("1.2", "1.2.0", false),
            ("1.2.1", "1.2", true),
            ("abc", "1.0.0", false),
            ("1.0.0", "", false),
            ("9.9.9.9.9.9.9.9.9", "1", false),
        ] {
            assert_eq!(newer(offered, installed), is, "{offered} over {installed}");
        }
    }

    #[test]
    fn the_store_keeps_the_last_good_index_through_a_failed_fetch() {
        let dir = lcc_testkit::TempDir::new("marketplace-store");
        let (mut s, why) = Store::open(dir.path());
        assert!(why.is_none() && s.saved.index.is_none() && s.saved.refresh_due(100));
        s.record_fetch(100, Ok(index(json!([entry("a")]))), &mut Vec::new());
        s.saved.community = true;
        s.saved.auto_update_for.insert("a".into(), true);
        s.saved.waiting.insert("a".into(), Waiting { version: "1.3.0".into(), why: "asks for more".into() });
        s.save().unwrap();
        let (mut s, _) = Store::open(dir.path());
        assert!(s.saved.community && s.saved.auto_updates("a") && !s.saved.auto_updates("b") && !s.saved.refresh_due(200));
        s.record_fetch(300, Err("HTTP 503".into()), &mut Vec::new());
        assert_eq!((s.saved.error.as_deref(), s.saved.index.as_ref().map(|i| i.entries.len())), (Some("HTTP 503"), Some(1)));
        // a newer version is offered: the wait for the old one is over
        let mut next = entry("a");
        next["version"] = json!("1.4.0");
        s.record_fetch(400, Ok(index(json!([next]))), &mut Vec::new());
        assert!(s.saved.waiting.is_empty() && s.saved.error.is_none());
        // a damaged file reads as empty and says why
        std::fs::write(dir.path().join(STORE_FILE), b"{ nope").unwrap();
        let (s, why) = Store::open(dir.path());
        assert!(why.is_some() && s.saved == Saved::default());
    }
}
