//! The Map module below the UI: Web Mercator maths ([`mercator`]), tile sources ([`source`]), the
//! on-disk tile cache ([`cache`]) and the tile fetch queue ([`service`]), plus the map's state in
//! the session ([`MapState`]), saved locations and the location filter.
//!
//! Nothing touches the network until a map view asks for tiles: the service is created on first
//! use ([`Session::map_tiles`]) and only fetches the tiles of the views it is shown, following the
//! OpenStreetMap tile usage policy (`docs/map.md`). A session has no cache folder and never goes
//! online until the host asks ([`Session::with_default_map_cache`]), so tests stay hermetic. The offline source
//! starts no service at all. Commands: `map.tileSource`, `map.providers`, `map.cacheInfo`,
//! `map.clearCache` (`cmd/map.rs`).
//!
//! The tile core (mercator, source validation, cache, service) is ported from PR #797
//! (Jason Baldwin), moved from its own crate into the engine.

pub mod cache;
mod http_date;
pub mod mercator;
pub mod service;
pub mod source;

#[cfg(test)]
mod tests_mercator;
#[cfg(test)]
mod tests_service;

use std::path::PathBuf;

use lightcraft_geom::LatLon;
use serde::{Deserialize, Serialize};

use crate::Session;
pub use cache::{CacheInfo, DEFAULT_CACHE_BYTES, TileCache, TileMeta};
pub use mercator::{Bounds, MAX_LATITUDE, MAX_ZOOM, TILE_SIZE, TileKey, Viewport, VisibleTile, fit_bounds, from_gps};
pub use service::{HttpResponse, ServiceStats, TileEvent, TileImage, TileService, Transport};
pub use source::{OSM_ATTRIBUTION, OSM_TILE_URL, TileSource, USER_AGENT};

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

/// Why the Esri layers can't be turned on: their terms (read 2026-11-14, `docs/map.md`) allow
/// them only with Esri software or an ArcGIS subscription, which LightCraft isn't and doesn't have.
pub const ESRI_UNAVAILABLE: &str =
    "the Esri map layers are off: Esri's terms allow them only with Esri software or an ArcGIS subscription (see docs/map.md)";

/// The map's state in the session (see the module docs).
#[derive(Debug)]
pub struct MapState {
    /// The tile source the user chose, saved in prefs.json as it is. An Esri source while Esri is
    /// off is kept here (never overwritten) but not used: see [`MapState::effective_source`].
    source: TileSource,
    /// The Esri layers may be offered and fetched. Off, and no command turns it on: Esri's terms
    /// don't allow LightCraft to use them (`docs/map.md`); only tests set it, to check the gate.
    esri_enabled: bool,
    /// The tile cache folder (`<cache>/maptiles`, one folder per source inside); `None` = tiles
    /// are kept in memory only (tests, the web).
    pub cache_root: Option<PathBuf>,
    /// Size cap of the tile cache, all sources together.
    pub cache_bytes: u64,
    /// Tiles may be fetched from the network. Off until the host asks
    /// ([`Session::with_default_map_cache`]), so tests and sessions without one never go online.
    pub online: bool,
    service: Option<TileService>,
}

impl Default for MapState {
    fn default() -> Self {
        MapState { source: TileSource::Osm, esri_enabled: false, cache_root: None, cache_bytes: DEFAULT_CACHE_BYTES, online: false, service: None }
    }
}

impl MapState {
    /// The tile source the user chose (what prefs.json stores).
    pub fn source(&self) -> &TileSource {
        &self.source
    }

    /// The source the map draws and fetches: the chosen one, or OpenStreetMap while the chosen
    /// one needs an opt-in that isn't given.
    pub fn effective_source(&self) -> TileSource {
        if self.usable(&self.source) { self.source.clone() } else { TileSource::Osm }
    }

    /// Whether the Esri layers may be offered and fetched.
    pub fn esri_enabled(&self) -> bool {
        self.esri_enabled
    }

    /// `source` may be used now (an Esri layer only with the opt-in).
    pub fn usable(&self, source: &TileSource) -> bool {
        !source.requires_opt_in() || self.esri_enabled
    }

    /// The built-in sources a picker offers: those usable now.
    pub fn offered(&self) -> Vec<TileSource> {
        TileSource::BUILT_IN.into_iter().filter(|s| self.usable(s)).collect()
    }

    /// A one-line notice while the chosen source can't be used (the map shows OpenStreetMap).
    pub fn fallback_notice(&self) -> Option<String> {
        (!self.usable(&self.source)).then(|| format!("The map shows OpenStreetMap instead of {}: {ESRI_UNAVAILABLE}.", self.source.label()))
    }

    /// Use `source` from now on (the next [`Session::map_tiles`] starts a service for it). An Esri
    /// source while Esri is off is refused and nothing changes.
    pub fn choose(&mut self, source: TileSource) -> Result<(), String> {
        if !self.usable(&source) {
            return Err(format!("{}: {ESRI_UNAVAILABLE}", source.label()));
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
    /// falls back to OpenStreetMap, with a log line. An Esri source while Esri is off is kept as
    /// the choice (prefs.json keeps it) but the map shows OpenStreetMap; the returned notice says so.
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

    /// Let tests turn the Esri gate on (there is no command for it; see [`ESRI_UNAVAILABLE`]).
    #[cfg(test)]
    pub(crate) fn set_esri_enabled(&mut self, on: bool) {
        if on != self.esri_enabled {
            self.esri_enabled = on;
            self.reset_service();
        }
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
        Some(m.service.get_or_insert_with(|| TileService::new(source, m.cache_root.clone(), m.cache_bytes, Some(activity), m.online)))
    }
}

/// User-saved geographical bookmark with circular radius and privacy control.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedLocation {
    pub id: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    /// Radius in meters.
    pub radius: f64,
    /// When true, GPS and location IPTC metadata are stripped upon export.
    pub is_private: bool,
}

impl SavedLocation {
    pub fn new(id: impl Into<String>, name: impl Into<String>, lat: f64, lon: f64, radius: f64, is_private: bool) -> Self {
        Self { id: id.into(), name: name.into(), lat, lon, radius: radius.max(1.0), is_private }
    }

    /// Center coordinate as `LatLon`.
    pub fn center(&self) -> LatLon {
        LatLon { lat: self.lat, lon: self.lon }
    }

    /// Whether `coord` falls inside this saved location's circular perimeter.
    pub fn contains(&self, coord: LatLon) -> bool {
        self.center().distance_meters(coord) <= self.radius
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

    #[test]
    fn test_saved_location_contains() {
        let loc = SavedLocation::new("loc1", "Home", 51.5074, -0.1278, 1000.0, true);
        assert!(loc.is_private);
        // Point right at center
        assert!(loc.contains(LatLon { lat: 51.5074, lon: -0.1278 }));
        // Point ~100 meters away
        assert!(loc.contains(LatLon { lat: 51.5080, lon: -0.1278 }));
        // Point 10 km away
        assert!(!loc.contains(LatLon { lat: 51.6000, lon: -0.1278 }));
    }

    #[test]
    fn esri_is_neither_offered_nor_used_until_enabled() {
        let mut m = MapState::default();
        assert!(!m.esri_enabled());
        assert!(m.offered().iter().all(|s| !s.requires_opt_in()), "{:?}", m.offered());
        assert!(m.choose(TileSource::EsriImagery).unwrap_err().contains("Esri"));
        assert_eq!(*m.source(), TileSource::Osm, "a refusal changes nothing");
        // a saved Esri choice is kept but not used, with a notice
        let notice = m.load_source(Some(TileSource::EsriTopo)).unwrap();
        assert!(notice.contains("OpenStreetMap") && notice.contains("Esri"), "{notice}");
        assert_eq!(*m.source(), TileSource::EsriTopo);
        assert_eq!(m.effective_source(), TileSource::Osm);
        // the gate itself: with the opt-in the same choices are accepted and used
        m.set_esri_enabled(true);
        assert_eq!(m.effective_source(), TileSource::EsriTopo);
        assert!(m.offered().contains(&TileSource::EsriImagery));
        m.choose(TileSource::EsriImagery).unwrap();
        assert_eq!(m.fallback_notice(), None);
        m.set_esri_enabled(false);
        assert_eq!(m.effective_source(), TileSource::Osm);
    }
}
