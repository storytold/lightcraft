//! The map's state in the session: the tile source (a library preference, saved in prefs.json),
//! where tiles are cached, and the tile service that loads them (`lightcraft-map`).
//!
//! Nothing touches the network until a map view asks for tiles: the service is created on first
//! use ([`Session::map_tiles`]) and only fetches the tiles of the view it is shown. A session has no
//! cache folder until the host asks for one ([`Session::with_default_map_cache`]), so tests stay
//! hermetic. Commands: `map.tileSource`, `map.cacheInfo`, `map.clearCache` (`cmd/map.rs`).

use std::path::PathBuf;
use std::sync::Arc;

use lightcraft_map::{DEFAULT_CACHE_BYTES, TileCache, TileService, TileSource, Transport};

use crate::Session;

/// See the module docs.
pub struct MapState {
    source: TileSource,
    /// The tile cache folder (`<cache>/map-tiles`, one folder per source inside); `None` = tiles
    /// are kept in memory only (tests, the web).
    pub cache_root: Option<PathBuf>,
    /// Size cap of the tile cache, all sources together.
    pub cache_bytes: u64,
    service: Option<TileService>,
    /// The library's saved locations (`locations.json`, see [`crate::locations`]).
    pub locations: crate::locations::SavedLocations,
    /// The track log loaded into the Map view (session only, see [`crate::tracklog`]).
    pub track: Option<crate::tracklog::LoadedTrack>,
    /// Bumped by every change of [`MapState::track`] (load, active track, offset, clear).
    pub track_generation: u64,
    /// How the services this state starts fetch tiles: `None` is HTTPS (`lightcraft-fetch`); a
    /// host with its own transport, or tests with a fake server, set it so that every source the
    /// user picks (Settings ▸ Map) goes through it.
    pub transport: Option<Arc<dyn Transport>>,
}

impl std::fmt::Debug for MapState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MapState")
            .field("source", &self.source)
            .field("cache_root", &self.cache_root)
            .field("cache_bytes", &self.cache_bytes)
            .field("service", &self.service.is_some())
            .field("locations", &self.locations)
            .field("track", &self.track.is_some())
            .field("track_generation", &self.track_generation)
            .field("transport", &self.transport.is_some())
            .finish()
    }
}

impl Default for MapState {
    fn default() -> Self {
        MapState {
            source: TileSource::openstreetmap(),
            cache_root: None,
            cache_bytes: DEFAULT_CACHE_BYTES,
            service: None,
            locations: Default::default(),
            track: None,
            track_generation: 0,
            transport: None,
        }
    }
}

impl MapState {
    /// The tile source in use.
    pub fn source(&self) -> &TileSource {
        &self.source
    }

    /// Use `source` from now on (the next [`Session::map_tiles`] starts a service for it; tiles in
    /// flight for the old one are dropped).
    pub fn set_source(&mut self, source: TileSource) {
        if source != self.source {
            self.source = source;
            self.reset_service();
        }
    }

    /// Take the source a settings file stored, re-checked (it is hostile input): an unusable one
    /// falls back to OpenStreetMap, with a log line.
    pub(crate) fn load_source(&mut self, stored: Option<TileSource>) {
        let source = match stored.map(|s| s.checked()) {
            None => TileSource::openstreetmap(),
            Some(Ok(s)) => s,
            Some(Err(e)) => {
                log::warn!("map: the saved tile source can't be used ({e}); using OpenStreetMap");
                TileSource::openstreetmap()
            }
        };
        self.set_source(source);
    }

    /// Stop the tile service (its threads end; the next use starts a fresh one).
    pub fn reset_service(&mut self) {
        self.service = None;
    }

    /// Load tiles with `service` from now on, its source becoming the current one (not saved): a
    /// host with its own transport, or tests with a fake one. Changing the source replaces it.
    pub fn set_service(&mut self, service: TileService) {
        self.source = service.source().clone();
        self.service = Some(service);
    }

    /// The service if one is running (no side effects).
    pub fn service(&self) -> Option<&TileService> {
        self.service.as_ref()
    }
}

impl Session {
    /// The tile service for the current source, started on first use. The map view calls
    /// [`TileService::show`] with its viewport each frame, draws [`TileService::get`] and polls
    /// [`TileService::poll`] for arrivals; nothing here blocks on disk or network.
    pub fn map_tiles(&mut self) -> &TileService {
        let m = &mut self.map;
        m.service.get_or_insert_with(|| match &m.transport {
            Some(t) => {
                let cache = if m.source.is_offline() {
                    None
                } else {
                    m.cache_root
                        .clone()
                        .and_then(|root| TileCache::new(root, &m.source.id, m.cache_bytes).map_err(|e| log::warn!("map: no tile cache: {e}")).ok())
                };
                TileService::with_transport(m.source.clone(), cache, Some(Arc::clone(t)))
            }
            None => TileService::new(m.source.clone(), m.cache_root.clone(), m.cache_bytes),
        })
    }
}
