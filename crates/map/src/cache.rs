//! The on-disk tile cache.
//!
//! Layout: `<root>/<source id>/<z>/<x>/<y>.tile`, one file per tile holding a small header (magic,
//! JSON [`TileMeta`]: when it was fetched, when it expires, its ETag and Last-Modified) and the
//! image bytes as the server sent them. `<root>` is LightCraft's cache folder's `map-tiles` (the
//! engine picks it); every source has its own folder, so switching sources never mixes tiles.
//!
//! - **Freshness** follows the server: `Cache-Control: max-age`, else `Expires`, else 7 days
//!   ([`DEFAULT_FRESH_SECS`]); `no-cache` means "revalidate every time" and `no-store` means "don't
//!   write it". An expired tile is still shown while a conditional request (If-None-Match /
//!   If-Modified-Since) refreshes it; a 304 answer only updates its metadata. Tiles are never
//!   deleted for being old, only to stay under the size cap.
//! - **Size cap** ([`DEFAULT_CACHE_BYTES`], for all sources together): when a write takes the
//!   cache past it, the least recently used tiles (file modification time, bumped when a tile is
//!   read) are deleted until it is back under 90 % of the cap.
//! - **Atomic writes**: a tile is written to a unique `.part` file and renamed into place, so a
//!   crash never leaves a half-written tile under its real name (stale `.part` files are swept by
//!   eviction and [`clear`]).
//! - **Damage**: an entry that can't be parsed (truncated, wrong magic, bad metadata, oversized) is
//!   deleted when read and fetched again; nothing here panics on a hostile file.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::MapError;
use crate::mercator::TileKey;
use crate::source::validate_id;

/// Default size cap of the tile cache (all sources together).
pub const DEFAULT_CACHE_BYTES: u64 = 500 << 20;
/// How long a tile is fresh when the server gives no Cache-Control max-age or Expires (the OSMF
/// policy asks for at least 7 days).
pub const DEFAULT_FRESH_SECS: i64 = 7 * 86_400;
/// Longest freshness believed (a server saying "100 years" gets this).
const MAX_FRESH_SECS: i64 = 365 * 86_400;
/// Largest cache file read (a tile is tens of kB; anything this big is damage).
pub const MAX_ENTRY_BYTES: u64 = 4 << 20;
/// Largest metadata header.
const MAX_META: usize = 8 * 1024;
/// A read bumps the access time when it is older than this (keeps reads from writing every time).
const TOUCH_AFTER_SECS: i64 = 600;
/// `.part` files older than this belong to a crashed writer and are swept.
const STALE_PART_SECS: i64 = 3600;
/// Most files looked at by one walk of the cache (bounds a hostile or huge folder).
const MAX_WALK: usize = 1_000_000;
const MAGIC: &[u8; 8] = b"LCTILE1\n";

/// What the cache knows about a tile besides its bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TileMeta {
    /// When it was fetched or last revalidated (Unix seconds).
    pub fetched: i64,
    /// Fresh until (Unix seconds); after that it is shown but revalidated.
    pub expires: i64,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

impl TileMeta {
    /// Metadata for a fresh `200` response with headers `header` (case-insensitive lookup), and
    /// whether it may be stored (`no-store` says no).
    pub fn from_response<'a>(now: i64, header: impl Fn(&str) -> Option<&'a str>) -> (TileMeta, bool) {
        let (expires, store) = freshness(now, header("cache-control"), header("expires"), header("date"));
        let keep =
            |v: Option<&str>| v.map(str::trim).filter(|v| !v.is_empty() && v.len() <= 256 && !v.chars().any(char::is_control)).map(str::to_string);
        (TileMeta { fetched: now, expires, etag: keep(header("etag")), last_modified: keep(header("last-modified")) }, store)
    }

    /// The metadata after a `304 Not Modified` with headers `header`: new freshness, and the
    /// validators the server sent (else the old ones).
    pub fn revalidated<'a>(&self, now: i64, header: impl Fn(&str) -> Option<&'a str>) -> TileMeta {
        let (fresh, _) = TileMeta::from_response(now, header);
        TileMeta {
            fetched: now,
            expires: fresh.expires,
            etag: fresh.etag.or_else(|| self.etag.clone()),
            last_modified: fresh.last_modified.or_else(|| self.last_modified.clone()),
        }
    }

    /// Fresh at `now` (else shown, but revalidated).
    pub fn is_fresh(&self, now: i64) -> bool {
        now < self.expires
    }

    /// The headers of a conditional request for this tile (If-None-Match, If-Modified-Since).
    pub fn conditional_headers(&self) -> Vec<(&'static str, String)> {
        let mut h = Vec::new();
        if let Some(e) = &self.etag {
            h.push(("If-None-Match", e.clone()));
        }
        if let Some(m) = &self.last_modified {
            h.push(("If-Modified-Since", m.clone()));
        }
        h
    }
}

/// When a response fetched at `now` stops being fresh, from its `Cache-Control`, `Expires` and
/// `Date` headers, and whether it may be stored. `max-age` wins over `Expires` (RFC 9111); an
/// `Expires` is taken relative to the server's `Date` when there is one (clock skew); an
/// unparsable `Expires` means already expired. Neither: [`DEFAULT_FRESH_SECS`].
pub fn freshness(now: i64, cache_control: Option<&str>, expires: Option<&str>, date: Option<&str>) -> (i64, bool) {
    let mut store = true;
    let mut max_age: Option<i64> = None;
    let mut no_cache = false;
    for d in cache_control.unwrap_or_default().split(',') {
        let d = d.trim().to_ascii_lowercase();
        let (name, value) = match d.split_once('=') {
            Some((n, v)) => (n.trim().to_string(), Some(v.trim().trim_matches('"').to_string())),
            None => (d.clone(), None),
        };
        match name.as_str() {
            "no-store" => store = false,
            "no-cache" => no_cache = true,
            // a bad max-age counts as 0 (RFC 9111 §4.2.1)
            "max-age" => max_age = Some(value.and_then(|v| v.parse::<u64>().ok()).map_or(0, |v| i64::try_from(v).unwrap_or(i64::MAX))),
            _ => {}
        }
    }
    let lifetime = if no_cache {
        0
    } else if let Some(age) = max_age {
        age
    } else if let Some(exp) = expires {
        match crate::http_date::parse(exp) {
            Some(e) => match date.and_then(crate::http_date::parse) {
                Some(d) => e.saturating_sub(d),
                None => e.saturating_sub(now),
            },
            None => 0,
        }
    } else {
        DEFAULT_FRESH_SECS
    };
    (now.saturating_add(lifetime.clamp(0, MAX_FRESH_SECS)), store)
}

/// A tile read from the cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedTile {
    pub meta: TileMeta,
    /// The image file as the server sent it.
    pub bytes: Vec<u8>,
}

/// How much the cache holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CacheInfo {
    pub bytes: u64,
    pub tiles: u64,
}

/// One source's tiles in the cache folder `root`.
#[derive(Debug)]
pub struct TileCache {
    root: PathBuf,
    dir: PathBuf,
    cap: u64,
    /// Bytes in the whole cache as last counted plus what was written since (counted on the first
    /// write; `None` until then).
    tally: Mutex<Option<u64>>,
}

static PART_SEQ: AtomicU64 = AtomicU64::new(0);

impl TileCache {
    /// The cache of source `source_id` under `root`, capped at `cap` bytes for all sources.
    /// Nothing is touched on disk until a tile is written.
    pub fn new(root: impl Into<PathBuf>, source_id: &str, cap: u64) -> Result<TileCache, MapError> {
        validate_id(source_id)?;
        let root = root.into();
        let dir = root.join(source_id);
        Ok(TileCache { root, dir, cap: cap.max(1 << 20), tally: Mutex::new(None) })
    }

    /// The cache folder (all sources).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// This source's folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, key: TileKey) -> PathBuf {
        self.dir.join(key.z.to_string()).join(key.x.to_string()).join(format!("{}.tile", key.y))
    }

    /// The cached tile, if there is a readable one. A damaged entry is deleted (and `None`
    /// returned, so the tile is fetched again). Marks the tile as recently used.
    pub fn read(&self, key: TileKey) -> Option<CachedTile> {
        let path = self.path(key);
        let md = fs::symlink_metadata(&path).ok()?;
        if !md.is_file() {
            return None;
        }
        let parsed = if md.len() > MAX_ENTRY_BYTES { None } else { fs::read(&path).ok().and_then(|b| parse_entry(&b)) };
        let Some(tile) = parsed else {
            log::warn!("map cache: deleting damaged tile {}", path.display());
            let _ = fs::remove_file(&path);
            return None;
        };
        let now = crate::now_secs();
        if md.modified().ok().map(secs_of).is_none_or(|m| now.saturating_sub(m) > TOUCH_AFTER_SECS) {
            touch(&path, now);
        }
        Some(tile)
    }

    /// Store `bytes` with `meta` (atomically), then evict least recently used tiles if the cache
    /// went past its cap.
    pub fn write(&self, key: TileKey, meta: &TileMeta, bytes: &[u8]) -> Result<(), MapError> {
        let path = self.path(key);
        let old = fs::symlink_metadata(&path).ok().filter(|m| m.is_file()).map_or(0, |m| m.len());
        let size = write_entry(&path, meta, bytes)?;
        let mut tally = self.tally.lock().unwrap_or_else(PoisonError::into_inner);
        let total = match *tally {
            Some(t) => t.saturating_sub(old).saturating_add(size),
            None => info(&self.root).bytes,
        };
        *tally = Some(if total > self.cap { evict(&self.root, self.cap / 10 * 9).bytes } else { total });
        Ok(())
    }

    /// Replace a cached tile's metadata, keeping its bytes (after a 304). `false` when the tile is
    /// no longer cached.
    pub fn refresh(&self, key: TileKey, meta: &TileMeta) -> Result<bool, MapError> {
        let Some(tile) = self.read(key) else { return Ok(false) };
        write_entry(&self.path(key), meta, &tile.bytes)?;
        Ok(true)
    }

    /// Forget a tile (e.g. its bytes turned out not to decode).
    pub fn remove(&self, key: TileKey) {
        let _ = fs::remove_file(self.path(key));
    }
}

fn secs_of(t: std::time::SystemTime) -> i64 {
    t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn touch(path: &Path, now: i64) {
    let t = std::time::UNIX_EPOCH + Duration::from_secs(u64::try_from(now).unwrap_or(0));
    if let Ok(f) = File::options().write(true).open(path) {
        let _ = f.set_modified(t);
    }
}

fn parse_entry(b: &[u8]) -> Option<CachedTile> {
    let rest = b.strip_prefix(MAGIC.as_slice())?;
    let len = u32::from_le_bytes(rest.get(..4)?.try_into().ok()?) as usize;
    if len > MAX_META {
        return None;
    }
    let meta: TileMeta = serde_json::from_slice(rest.get(4..4usize.checked_add(len)?)?).ok()?;
    let bytes = rest.get(4 + len..)?;
    (!bytes.is_empty()).then(|| CachedTile { meta, bytes: bytes.to_vec() })
}

fn write_entry(path: &Path, meta: &TileMeta, bytes: &[u8]) -> Result<u64, MapError> {
    let io = |what: &str, e: std::io::Error| MapError::Io(format!("{what} {}: {e}", path.display()));
    let meta = serde_json::to_vec(meta).map_err(|e| MapError::Io(e.to_string()))?;
    if meta.len() > MAX_META || bytes.is_empty() {
        return Err(MapError::BadTile("nothing to cache".into()));
    }
    let mut data = Vec::with_capacity(MAGIC.len() + 4 + meta.len() + bytes.len());
    data.extend_from_slice(MAGIC);
    data.extend_from_slice(&(meta.len() as u32).to_le_bytes());
    data.extend_from_slice(&meta);
    data.extend_from_slice(bytes);
    if data.len() as u64 > MAX_ENTRY_BYTES {
        return Err(MapError::BadTile("tile too large to cache".into()));
    }
    let parent = path.parent().ok_or_else(|| MapError::Io("tile path has no folder".into()))?;
    fs::create_dir_all(parent).map_err(|e| io("creating", e))?;
    let seq = PART_SEQ.fetch_add(1, Ordering::Relaxed);
    let part = path.with_extension(format!("tile.{}-{seq}.part", std::process::id()));
    let written = File::create(&part).and_then(|mut f| f.write_all(&data)).and_then(|()| fs::rename(&part, path));
    if let Err(e) = written {
        let _ = fs::remove_file(&part);
        return Err(io("writing", e));
    }
    Ok(data.len() as u64)
}

/// A file of the cache: path, size, last use.
struct Entry {
    path: PathBuf,
    len: u64,
    used: i64,
}

/// Every `.tile` under `root` at `<source>/<z>/<x>/<file>` (sources with valid ids only; no
/// symlinks followed). Stale `.part` files met on the way are deleted when `sweep`.
fn walk(root: &Path, sweep: bool) -> Vec<Entry> {
    let now = crate::now_secs();
    let mut out = Vec::new();
    let mut seen = 0usize;
    let dirs = |p: &Path| -> Vec<PathBuf> {
        fs::read_dir(p)
            .map(|rd| rd.filter_map(Result::ok).filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).collect())
            .unwrap_or_default()
    };
    let numeric =
        |p: &Path| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| !n.is_empty() && n.len() <= 10 && n.bytes().all(|b| b.is_ascii_digit()));
    for source in dirs(root) {
        if source.file_name().and_then(|n| n.to_str()).is_none_or(|n| validate_id(n).is_err()) {
            continue;
        }
        for z in dirs(&source).into_iter().filter(|p| numeric(p)) {
            for x in dirs(&z).into_iter().filter(|p| numeric(p)) {
                let Ok(rd) = fs::read_dir(&x) else { continue };
                for e in rd.filter_map(Result::ok) {
                    seen += 1;
                    if seen > MAX_WALK {
                        return out;
                    }
                    let Ok(md) = e.metadata() else { continue };
                    if !md.is_file() || e.file_type().is_ok_and(|t| t.is_symlink()) {
                        continue;
                    }
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    let used = md.modified().ok().map_or(0, secs_of);
                    if name.ends_with(".part") {
                        if sweep && now.saturating_sub(used) > STALE_PART_SECS {
                            let _ = fs::remove_file(e.path());
                        }
                    } else if name.ends_with(".tile") {
                        out.push(Entry { path: e.path(), len: md.len(), used });
                    }
                }
            }
        }
    }
    out
}

/// How much the cache under `root` holds (all sources).
pub fn info(root: &Path) -> CacheInfo {
    let e = walk(root, false);
    CacheInfo { bytes: e.iter().map(|e| e.len).sum(), tiles: e.len() as u64 }
}

/// Delete the least recently used tiles under `root` until at most `target` bytes remain; returns
/// what remains.
pub fn evict(root: &Path, target: u64) -> CacheInfo {
    let mut e = walk(root, true);
    e.sort_by_key(|e| e.used);
    let mut total: u64 = e.iter().map(|e| e.len).sum();
    let mut tiles = e.len() as u64;
    for entry in &e {
        if total <= target {
            break;
        }
        if fs::remove_file(&entry.path).is_ok() {
            total = total.saturating_sub(entry.len);
            tiles -= 1;
        }
    }
    CacheInfo { bytes: total, tiles }
}

/// Delete every cached tile under `root` (and leftover `.part` files), then the emptied folders;
/// returns what was deleted. Only tile files are removed: a cache folder pointed somewhere odd
/// loses nothing else.
pub fn clear(root: &Path) -> Result<CacheInfo, MapError> {
    let entries = walk(root, false);
    let mut gone = CacheInfo::default();
    let mut failed = None;
    for e in &entries {
        match fs::remove_file(&e.path) {
            Ok(()) => {
                gone.bytes += e.len;
                gone.tiles += 1;
            }
            Err(err) => failed = Some(format!("{}: {err}", e.path.display())),
        }
    }
    // parts of any age, then the empty folders (remove_dir only removes empty ones)
    let _ = evict_parts(root);
    for e in &entries {
        let mut dir = e.path.parent();
        for _ in 0..3 {
            let Some(d) = dir else { break };
            if fs::remove_dir(d).is_err() {
                break;
            }
            dir = d.parent();
        }
    }
    match failed {
        Some(f) => Err(MapError::Io(format!("couldn't delete {f}"))),
        None => Ok(gone),
    }
}

fn evict_parts(root: &Path) -> std::io::Result<()> {
    for source in fs::read_dir(root)?.filter_map(Result::ok) {
        if source.file_name().to_str().is_none_or(|n| validate_id(n).is_err()) || !source.file_type()?.is_dir() {
            continue;
        }
        for z in fs::read_dir(source.path())?.filter_map(Result::ok).filter(|e| e.file_type().is_ok_and(|t| t.is_dir())) {
            for x in fs::read_dir(z.path())?.filter_map(Result::ok).filter(|e| e.file_type().is_ok_and(|t| t.is_dir())) {
                for f in fs::read_dir(x.path())?.filter_map(Result::ok) {
                    if f.file_name().to_string_lossy().ends_with(".part") && f.file_type().is_ok_and(|t| t.is_file()) {
                        let _ = fs::remove_file(f.path());
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lightcraft-map-cache-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn key(z: u8, x: u32, y: u32) -> TileKey {
        TileKey::new(z, x, y).unwrap()
    }

    fn headers<'a>(h: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<&'a str> {
        move |n| h.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| *v)
    }

    #[test]
    fn freshness_follows_the_headers() {
        let now = 1_000_000;
        assert_eq!(freshness(now, Some("max-age=3600"), None, None), (now + 3600, true));
        assert_eq!(
            freshness(now, Some("public, max-age=86400, stale-while-revalidate=1"), Some("Thu, 01 Jan 1970 00:00:00 GMT"), None),
            (now + 86_400, true),
            "max-age wins over Expires"
        );
        assert_eq!(freshness(now, Some("max-age=abc"), None, None), (now, true), "a bad max-age is 0");
        assert_eq!(freshness(now, Some("no-cache"), None, None), (now, true));
        assert!(!freshness(now, Some("no-store"), None, None).1);
        assert_eq!(freshness(now, Some("max-age=99999999999"), None, None).0, now + MAX_FRESH_SECS);
        assert_eq!(freshness(now, None, None, None), (now + 7 * 86_400, true), "7 days when the server says nothing");
        // Expires relative to the server's Date
        assert_eq!(freshness(now, None, Some("Sun, 06 Nov 1994 09:49:37 GMT"), Some("Sun, 06 Nov 1994 08:49:37 GMT")), (now + 3600, true));
        // Expires without Date: absolute
        let at = crate::http_date::parse("Sun, 06 Nov 1994 08:49:37 GMT").unwrap();
        assert_eq!(freshness(at - 60, None, Some("Sun, 06 Nov 1994 08:49:37 GMT"), None), (at, true));
        // already expired, or junk: expired now (shown, revalidated)
        assert_eq!(freshness(now, None, Some("0"), None), (now, true));
        assert_eq!(freshness(now, None, Some("Thu, 01 Jan 1970 00:00:00 GMT"), None), (now, true));
    }

    #[test]
    fn meta_from_headers_and_304() {
        let h = [("Cache-Control", "max-age=60"), ("ETag", "\"abc\""), ("Last-Modified", "Sun, 06 Nov 1994 08:49:37 GMT")];
        let (m, store) = TileMeta::from_response(100, headers(&h));
        assert!(store);
        assert_eq!(
            m,
            TileMeta { fetched: 100, expires: 160, etag: Some("\"abc\"".into()), last_modified: Some("Sun, 06 Nov 1994 08:49:37 GMT".into()) }
        );
        assert!(m.is_fresh(159) && !m.is_fresh(160));
        assert_eq!(
            m.conditional_headers(),
            vec![("If-None-Match", "\"abc\"".to_string()), ("If-Modified-Since", "Sun, 06 Nov 1994 08:49:37 GMT".to_string())]
        );
        // a 304 without headers: 7 days, same validators
        let r = m.revalidated(500, headers(&[]));
        assert_eq!((r.fetched, r.expires, r.etag.as_deref()), (500, 500 + DEFAULT_FRESH_SECS, Some("\"abc\"")));
        let r = m.revalidated(500, headers(&[("ETag", "\"new\""), ("Cache-Control", "max-age=10")]));
        assert_eq!((r.expires, r.etag.as_deref()), (510, Some("\"new\"")));
        // a header with a line break is not kept (it would be sent back)
        let (m, _) = TileMeta::from_response(0, headers(&[("ETag", "a\r\nX: 1")]));
        assert_eq!(m.etag, None);
    }

    #[test]
    fn writes_reads_and_refreshes() {
        let root = tmp("rw");
        let c = TileCache::new(&root, "osm", DEFAULT_CACHE_BYTES).unwrap();
        let k = key(3, 2, 1);
        assert_eq!(c.read(k), None);
        let meta = TileMeta { fetched: 1, expires: 2, etag: Some("e".into()), last_modified: None };
        c.write(k, &meta, b"png bytes").unwrap();
        assert!(root.join("osm/3/2/1.tile").is_file());
        assert_eq!(c.read(k), Some(CachedTile { meta: meta.clone(), bytes: b"png bytes".to_vec() }));
        let newer = TileMeta { fetched: 10, expires: 20, ..meta };
        assert!(c.refresh(k, &newer).unwrap());
        assert_eq!(c.read(k).unwrap(), CachedTile { meta: newer.clone(), bytes: b"png bytes".to_vec() });
        assert!(!c.refresh(key(3, 0, 0), &newer).unwrap(), "nothing to refresh");
        // no .part left behind
        let leftovers: Vec<_> = fs::read_dir(root.join("osm/3/2"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty());
        assert_eq!(info(&root), CacheInfo { bytes: fs::metadata(root.join("osm/3/2/1.tile")).unwrap().len(), tiles: 1 });
        // an unusable source id never becomes a path
        assert!(TileCache::new(&root, "../x", DEFAULT_CACHE_BYTES).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn damaged_entries_are_deleted_not_fatal() {
        let root = tmp("damaged");
        let c = TileCache::new(&root, "osm", DEFAULT_CACHE_BYTES).unwrap();
        let dir = root.join("osm/1/0");
        fs::create_dir_all(&dir).unwrap();
        let mut too_long_meta = MAGIC.to_vec();
        too_long_meta.extend_from_slice(&u32::MAX.to_le_bytes());
        let mut bad_json = MAGIC.to_vec();
        bad_json.extend_from_slice(&3u32.to_le_bytes());
        bad_json.extend_from_slice(b"{{{png");
        let mut no_body = MAGIC.to_vec();
        no_body.extend_from_slice(&2u32.to_le_bytes());
        no_body.extend_from_slice(b"{}");
        for (i, bytes) in [b"".to_vec(), b"LCTILE".to_vec(), b"garbage garbage".to_vec(), too_long_meta, bad_json, no_body].into_iter().enumerate() {
            let y = i as u32 % 2;
            let k = key(1, 0, y);
            fs::write(dir.join(format!("{y}.tile")), &bytes).unwrap();
            assert_eq!(c.read(k), None, "case {i}");
            assert!(!dir.join(format!("{y}.tile")).exists(), "case {i}: deleted");
        }
        // oversized
        fs::write(dir.join("0.tile"), vec![0u8; MAX_ENTRY_BYTES as usize + 1]).unwrap();
        assert_eq!(c.read(key(1, 0, 0)), None);
        assert!(!dir.join("0.tile").exists());
        // a folder where a tile should be is left alone
        fs::create_dir_all(dir.join("1.tile")).unwrap();
        assert_eq!(c.read(key(1, 0, 1)), None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn evicts_least_recently_used_past_the_cap() {
        let root = tmp("lru");
        let cap = 1 << 20; // the smallest cap; tiles of ~100 kB
        let c = TileCache::new(&root, "osm", cap).unwrap();
        let body = vec![7u8; 100_000];
        let meta = TileMeta::default();
        let now = crate::now_secs();
        for x in 0..9u32 {
            c.write(key(5, x, 0), &meta, &body).unwrap();
            // make the order of use unambiguous: tile x was used x hours ago… reversed
            touch(&c.path(key(5, x, 0)), now - 100_000 + i64::from(x) * 1000);
        }
        // tile 0 is the oldest; reading it makes it the newest
        assert!(c.read(key(5, 0, 0)).is_some());
        assert!(info(&root).bytes < cap);
        // two more tiles push past 1 MiB: eviction brings it under 90 %, oldest first
        c.write(key(5, 9, 0), &meta, &body).unwrap();
        c.write(key(5, 10, 0), &meta, &body).unwrap();
        let i = info(&root);
        assert!(i.bytes <= cap / 10 * 9, "{i:?}");
        assert!(c.read(key(5, 0, 0)).is_some(), "recently read: kept");
        assert!(c.read(key(5, 10, 0)).is_some(), "just written: kept");
        assert!(c.read(key(5, 1, 0)).is_none(), "least recently used: evicted");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn clear_deletes_only_tiles_and_sweeps_parts() {
        let root = tmp("clear");
        let c = TileCache::new(&root, "osm", DEFAULT_CACHE_BYTES).unwrap();
        let other = TileCache::new(&root, "custom-1", DEFAULT_CACHE_BYTES).unwrap();
        c.write(key(2, 1, 1), &TileMeta::default(), b"a").unwrap();
        other.write(key(2, 1, 1), &TileMeta::default(), b"b").unwrap();
        fs::write(root.join("osm/2/1/1.tile.1-1.part"), b"half").unwrap();
        fs::write(root.join("notes.txt"), b"not ours").unwrap();
        fs::create_dir_all(root.join("Not An Id/1/1")).unwrap();
        fs::write(root.join("Not An Id/1/1/1.tile"), b"not ours either").unwrap();
        let gone = clear(&root).unwrap();
        assert_eq!(gone.tiles, 2);
        assert_eq!(info(&root), CacheInfo::default());
        assert!(!root.join("osm").exists() && !root.join("custom-1").exists());
        assert!(root.join("notes.txt").exists() && root.join("Not An Id/1/1/1.tile").exists());
        assert_eq!(clear(&root.join("missing")).unwrap(), CacheInfo::default());
        let _ = fs::remove_dir_all(&root);
    }
}
