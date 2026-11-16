//! The Map module below the UI: Web Mercator maths ([`mercator`]), tile sources ([`source`]), the
//! on-disk tile cache ([`cache`]) and the tile fetch queue ([`service`]), plus the map's state in
//! the session ([`MapState`]), with the saved locations ([`crate::locations`]) and the location
//! filter.
//!
//! Nothing touches the network until a map view asks for tiles: the service is created on first
//! use ([`Session::map_tiles`]) and only fetches the tiles of the views it is shown, following the
//! OpenStreetMap tile usage policy (`docs/map.md`). A session has no cache folder and never goes
//! online until the host asks ([`Session::with_default_map_cache`]), so tests stay hermetic. The offline source
//! starts no service at all. Commands: `map.tileSource`, `map.providers`, `map.cacheInfo`,
//! `map.clearCache`, `map.setEsriKey` (`cmd/map.rs`).
//!
//! The tile core (mercator, source validation, cache, service) is ported from PR #797
//! (Jason Baldwin), moved from its own crate into the engine.

pub mod cache;
pub mod cluster;
mod http_date;
pub mod mercator;
pub mod service;
pub mod source;

#[cfg(test)]
mod tests_cluster;
#[cfg(test)]
mod tests_mercator;
#[cfg(test)]
mod tests_service;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::Session;
pub use cache::{CacheInfo, DEFAULT_CACHE_BYTES, TileCache, TileMeta};
pub use cluster::{CLUSTER_CELL, Cluster, MapPoint};
pub use mercator::{Bounds, MAX_LATITUDE, MAX_ZOOM, TILE_SIZE, TileKey, Viewport, VisibleTile, fit_bounds, from_gps};
pub use service::{HttpResponse, ServiceStats, TileEvent, TileImage, TileService, Transport};
pub use source::{Crop, OSM_ATTRIBUTION, OSM_TILE_URL, TileSource, USER_AGENT};

/// What went wrong in the map core.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MapError {
    /// A tile source setting that can't be used (template, id, attribution, zoom).
    #[error("tile source: {0}")]
    InvalidSource(String),
    /// A cache file or folder problem (disk full, permissions…).
    #[error("map tile cache: {0}")]
    Io(String),
    /// A tile that isn't a usable image.
    #[error("map tile: {0}")]
    BadTile(String),
}

/// Seconds since the Unix epoch, now (0 if the clock is before 1970).
pub(crate) fn now_secs() -> i64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX)).unwrap_or(0)
}

/// Where to get an ArcGIS API key, for messages.
pub const ESRI_KEY_HELP: &str =
    "the Esri layers need your own ArcGIS Location Platform API key (Settings ▸ Map; a free one is at https://location.arcgis.com)";

/// Longest API key accepted (Esri's keys are a few hundred characters).
const MAX_KEY: usize = 2048;
/// Shortest API key accepted.
const MIN_KEY: usize = 20;
/// The file in the config folder that keeps the key (`docs/map.md`).
pub const KEY_FILE: &str = "map-keys.json";

/// The user's ArcGIS API key: a secret. Its `Debug` hides it, and nothing in the engine prints,
/// logs, caches by or reports it; only the tile service puts it in a request to Esri's host.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(hidden)")
    }
}

impl ApiKey {
    /// The key, checked for shape only (no network call): 20–2048 characters of `A–Z a–z 0–9 . _ -`
    /// after trimming. The error never repeats the key.
    pub fn parse(text: &str) -> Result<ApiKey, String> {
        let k = text.trim();
        let ok_chars = k.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !ok_chars || k.len() < MIN_KEY || k.len() > MAX_KEY {
            return Err(format!(
                "that doesn't look like an ArcGIS API key: it should be {MIN_KEY}–{MAX_KEY} characters of letters, digits, '.', '_' or '-'"
            ));
        }
        Ok(ApiKey(k.to_string()))
    }

    /// The key itself (for the request URL only).
    pub(crate) fn secret(&self) -> &str {
        &self.0
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    esri_api_key: Option<String>,
}

/// The map's state in the session (see the module docs).
#[derive(Debug)]
pub struct MapState {
    /// The tile source the user chose, saved in prefs.json as it is. An Esri source without a key is
    /// kept here (never overwritten) but not used: see [`MapState::effective_source`].
    source: TileSource,
    /// The user's ArcGIS API key: the Esri layers are offered and fetched only with one.
    esri_key: Option<ApiKey>,
    /// Where the key is kept (`<config>/map-keys.json`); `None`: memory only (tests, the web).
    pub key_file: Option<PathBuf>,
    /// The tile cache folder (`<cache>/maptiles`, one folder per source inside); `None` = tiles
    /// are kept in memory only (tests, the web).
    pub cache_root: Option<PathBuf>,
    /// Size cap of the tile cache, all sources together.
    pub cache_bytes: u64,
    /// Tiles may be fetched from the network. Off until the host asks
    /// ([`Session::with_default_map_cache`]), so tests and sessions without one never go online.
    pub online: bool,
    service: Option<TileService>,
    /// The library's saved locations (`locations.json`, see [`crate::locations`]).
    pub locations: crate::locations::SavedLocations,
}

impl Default for MapState {
    fn default() -> Self {
        MapState {
            source: TileSource::Osm,
            esri_key: None,
            key_file: None,
            cache_root: None,
            cache_bytes: DEFAULT_CACHE_BYTES,
            online: false,
            service: None,
            locations: Default::default(),
        }
    }
}

impl MapState {
    /// The tile source the user chose (what prefs.json stores).
    pub fn source(&self) -> &TileSource {
        &self.source
    }

    /// The source the map draws and fetches: the chosen one, or OpenStreetMap while the chosen
    /// one needs a key that isn't set.
    pub fn effective_source(&self) -> TileSource {
        if self.usable(&self.source) { self.source.clone() } else { TileSource::Osm }
    }

    /// An ArcGIS API key is set (the key itself is never reported).
    pub fn esri_key_set(&self) -> bool {
        self.esri_key.is_some()
    }

    /// `source` may be used now (an Esri layer only with a key).
    pub fn usable(&self, source: &TileSource) -> bool {
        !source.requires_key() || self.esri_key.is_some()
    }

    /// The built-in sources a picker offers: those usable now.
    pub fn offered(&self) -> Vec<TileSource> {
        TileSource::BUILT_IN.into_iter().filter(|s| self.usable(s)).collect()
    }

    /// A one-line notice while the chosen source can't be used (the map shows OpenStreetMap).
    pub fn fallback_notice(&self) -> Option<String> {
        (!self.usable(&self.source)).then(|| format!("The map shows OpenStreetMap instead of {}: {ESRI_KEY_HELP}.", self.source.label()))
    }

    /// Use `source` from now on (the next [`Session::map_tiles`] starts a service for it). An Esri
    /// source without a key is refused and nothing changes.
    pub fn choose(&mut self, source: TileSource) -> Result<(), String> {
        if !self.usable(&source) {
            return Err(format!("{}: {ESRI_KEY_HELP}", source.label()));
        }
        if source != self.source {
            self.source = source;
            self.reset_service();
        }
        Ok(())
    }

    /// Put back a choice that couldn't be saved (no checks: it was the choice before).
    pub(crate) fn restore(&mut self, source: TileSource) {
        if source != self.source {
            self.source = source;
            self.reset_service();
        }
    }

    /// Take the source a settings file stored, re-checked (it is hostile input): an unusable one
    /// falls back to OpenStreetMap, with a log line. An Esri source without a key is kept as the
    /// choice (prefs.json keeps it) but the map shows OpenStreetMap; the returned notice says so.
    pub(crate) fn load_source(&mut self, stored: Option<TileSource>) -> Option<String> {
        let source = match stored.map(|s| s.checked()) {
            None => TileSource::Osm,
            Some(Ok(s)) => s,
            Some(Err(e)) => {
                log::warn!("map: the saved tile source can't be used ({e}); using OpenStreetMap");
                TileSource::Osm
            }
        };
        self.restore(source);
        let notice = self.fallback_notice();
        if let Some(n) = &notice {
            log::warn!("map: {n}");
        }
        notice
    }

    /// Set the ArcGIS API key (`None` clears it), keeping it in [`MapState::key_file`] when there
    /// is one. Shape-checked only; nothing is sent anywhere. A key that can't be saved is not set.
    pub fn set_esri_key(&mut self, key: Option<ApiKey>) -> Result<(), String> {
        if let Some(path) = &self.key_file {
            let file = KeyFile { esri_api_key: key.as_ref().map(|k| k.secret().to_string()) };
            write_key_file(path, &file).map_err(|e| format!("can't save the API key in {}: {e}", path.display()))?;
        }
        if key != self.esri_key {
            self.esri_key = key;
            self.reset_service();
        }
        Ok(())
    }

    /// Read the key kept in `path` (a missing file: no key). A damaged file or a key of the wrong
    /// shape is ignored with a log line that never shows it.
    pub(crate) fn load_esri_key(&mut self, path: Option<PathBuf>) {
        self.key_file = path;
        let Some(path) = &self.key_file else { return };
        let bytes = match crate::read_file::read_regular(path, 64 * 1024) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                log::warn!("map: can't read {}: {e}", path.display());
                return;
            }
        };
        let key = match serde_json::from_slice::<KeyFile>(&bytes) {
            Ok(f) => {
                f.esri_api_key.and_then(|k| ApiKey::parse(&k).map_err(|e| log::warn!("map: {}: the Esri key is ignored: {e}", path.display())).ok())
            }
            Err(_) => {
                log::warn!("map: {} isn't a key file; ignored", path.display());
                None
            }
        };
        self.esri_key = key;
        self.reset_service();
    }

    /// Stop the tile service (its threads end; the next use starts a fresh one).
    pub fn reset_service(&mut self) {
        self.service = None;
    }

    /// Load tiles with `service` from now on (tests with a fake transport, hosts with their own).
    /// It is used while its source is [`MapState::effective_source`]; a change of source replaces it.
    pub fn set_service(&mut self, service: TileService) {
        self.service = Some(service);
    }

    /// The service if one is running (no side effects).
    pub fn service(&self) -> Option<&TileService> {
        self.service.as_ref()
    }

    /// The credit to paint for the effective source: the running service's (an Esri layer's own
    /// metadata once fetched), else the source's fixed one.
    pub fn attribution(&self) -> String {
        let source = self.effective_source();
        match self.service.as_ref().filter(|s| *s.source() == source) {
            Some(s) => s.attribution(),
            None => source.attribution().to_string(),
        }
    }
}

/// Write the key file atomically, readable by its owner only where the system has such modes.
fn write_key_file(path: &std::path::Path, file: &KeyFile) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let data = serde_json::to_vec_pretty(file).map_err(std::io::Error::other)?;
    lightcraft_catalog::safe_file::write_atomic(path, &data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

impl Session {
    /// The tile service for the effective source, started on first use; `None` for the offline
    /// source (no tiles, no network, no threads). The map view hands it the tiles it draws
    /// ([`TileService::want`]), draws [`TileService::get`] and polls [`TileService::poll`] for
    /// arrivals; nothing here blocks on disk or network.
    pub fn map_tiles(&mut self) -> Option<&TileService> {
        let source = self.map.effective_source();
        if source.is_offline() {
            self.map.service = None;
            return None;
        }
        let activity = self.activity.clone();
        let m = &mut self.map;
        if m.service.as_ref().is_some_and(|s| *s.source() != source) {
            m.service = None;
        }
        let token = if source.requires_key() { m.esri_key.clone() } else { None };
        Some(
            m.service
                .get_or_insert_with(|| TileService::new(source, m.cache_root.clone(), m.cache_bytes, Some(activity), m.online).with_token(token)),
        )
    }
}

/// The filter state for the Map Location Filter bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LocationFilter {
    #[default]
    None,
    VisibleOnMap,
    Tagged,
    Untagged,
}

impl LocationFilter {
    pub fn id(self) -> &'static str {
        match self {
            LocationFilter::None => "none",
            LocationFilter::VisibleOnMap => "visible",
            LocationFilter::Tagged => "tagged",
            LocationFilter::Untagged => "untagged",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" | "off" => Some(LocationFilter::None),
            "visible" | "visibleonmap" => Some(LocationFilter::VisibleOnMap),
            "tagged" => Some(LocationFilter::Tagged),
            "untagged" => Some(LocationFilter::Untagged),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> ApiKey {
        ApiKey::parse("AAPK0123456789abcdef_TEST-key.x").unwrap()
    }

    #[test]
    fn esri_is_neither_offered_nor_used_without_a_key() {
        let mut m = MapState::default();
        assert!(!m.esri_key_set());
        assert!(m.offered().iter().all(|s| !s.requires_key()), "{:?}", m.offered());
        let e = m.choose(TileSource::EsriStreet).unwrap_err();
        assert!(e.contains("API key") && e.contains("location.arcgis.com"), "{e}");
        assert_eq!(*m.source(), TileSource::Osm, "a refusal changes nothing");
        // a saved Esri choice is kept but not used, with a notice
        let notice = m.load_source(Some(TileSource::EsriTopo)).unwrap();
        assert!(notice.contains("OpenStreetMap") && notice.contains("API key"), "{notice}");
        assert_eq!(*m.source(), TileSource::EsriTopo);
        assert_eq!(m.effective_source(), TileSource::Osm);
        // with a key the same choices are offered, accepted and used
        m.set_esri_key(Some(key())).unwrap();
        assert_eq!(m.effective_source(), TileSource::EsriTopo);
        assert!(m.offered().contains(&TileSource::EsriStreet));
        m.choose(TileSource::EsriStreet).unwrap();
        assert_eq!(m.fallback_notice(), None);
        m.set_esri_key(None).unwrap();
        assert_eq!(m.effective_source(), TileSource::Osm);
    }

    #[test]
    fn keys_are_shape_checked_and_never_printed() {
        let k = key();
        assert_eq!(format!("{k:?}"), "ApiKey(hidden)");
        let mut m = MapState::default();
        m.set_esri_key(Some(k.clone())).unwrap();
        assert!(!format!("{m:?}").contains(k.secret()), "Debug of the state hides the key");
        for bad in ["", "short", "has spaces in it 0123456789", "AAPK0123456789abcdef\nX", "ключ0123456789abcdefghij", &"a".repeat(3000)] {
            let e = ApiKey::parse(bad).unwrap_err();
            assert!(bad.len() < 5 || !e.contains(bad), "the error doesn't repeat the key");
        }
        assert_eq!(ApiKey::parse("  AAPK0123456789abcdef_TEST-key.x \n").unwrap(), k, "trimmed");
    }

    #[test]
    fn the_key_file_round_trips_and_a_damaged_one_is_ignored() {
        let dir = std::env::temp_dir().join(format!("lightcraft-map-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(KEY_FILE);
        let mut m = MapState::default();
        m.load_esri_key(Some(path.clone()));
        assert!(!m.esri_key_set(), "no file: no key");
        m.set_esri_key(Some(key())).unwrap();
        let mut back = MapState::default();
        back.load_esri_key(Some(path.clone()));
        assert!(back.esri_key_set());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        m.set_esri_key(None).unwrap();
        back.load_esri_key(Some(path.clone()));
        assert!(!back.esri_key_set(), "cleared in the file too");
        for junk in [&b"not json"[..], br#"{"esriApiKey": "bad key"}"#, br#"{"esriApiKey": 5}"#] {
            std::fs::write(&path, junk).unwrap();
            back.load_esri_key(Some(path.clone()));
            assert!(!back.esri_key_set());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
