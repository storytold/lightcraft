//! The map core: everything a map view needs below the UI.
//!
//! - [`mercator`]: Web Mercator (EPSG:3857) maths with 256 px tiles: latitude/longitude ↔ world
//!   pixels and tiles at any (fractional) zoom, the tiles covering a [`Viewport`], screen positions
//!   of photo pins, the [`Bounds`] of a set of points (antimeridian aware) and the view that fits
//!   them ([`fit_bounds`]). Input points are checked ([`LatLon::new`]): NaN, infinities and
//!   out-of-range latitudes/longitudes are dropped, never propagated.
//! - [`cluster`]: photo pins grouped per zoom ([`cluster()`]): points projected once
//!   ([`MapPoint`]), grouped every frame into world-anchored screen cells.
//! - [`track`]: GPS track logs drawn on the map ([`TrackShape`]): lines projected once, cut at the
//!   antimeridian, simplified per zoom (Douglas–Peucker ranks computed once) and clipped to the view.
//! - [`source`]: where tiles come from ([`TileSource`]): OpenStreetMap's standard tiles by default,
//!   a user-configured `https://…/{z}/{x}/{y}.png` template, or offline (no network at all).
//! - [`cache`]: the on-disk tile cache ([`TileCache`]): PNG bytes plus HTTP freshness metadata,
//!   Cache-Control / Expires / ETag honoured (7 days when the server says nothing), atomic writes,
//!   a size cap with least-recently-used eviction; damaged entries are deleted and fetched again.
//! - [`service`]: the fetch queue ([`TileService`]): background threads that read the cache and
//!   fetch only the tiles of the current viewport (plus at most one tile around it), newest view
//!   first, at most two requests at a time, with backoff after errors. The UI thread never waits:
//!   it calls [`TileService::show`] with its viewport, draws what [`TileService::get`] has, and
//!   [`TileService::poll`]s for arrivals.
//!
//! What is fetched, from where, and the OpenStreetMap tile usage policy this follows are described
//! in `docs/map.md`. On wasm32 the network side is absent: the maths and the cache code compile,
//! and tiles simply never arrive.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod cache;
pub mod cluster;
pub mod http_date;
pub mod mercator;
pub mod service;
pub mod source;
pub mod track;

#[cfg(test)]
mod tests_cluster;
#[cfg(test)]
mod tests_mercator;
#[cfg(test)]
mod tests_service;
#[cfg(test)]
mod tests_track;

pub use cache::{CacheInfo, CachedTile, DEFAULT_CACHE_BYTES, TileCache, TileMeta};
pub use cluster::{CLUSTER_CELL, Cluster, MapPoint, cluster};
pub use mercator::{Area, Bounds, LatLon, MAX_LATITUDE, MAX_ZOOM, TILE_SIZE, TileKey, Viewport, VisibleTile, fit_bounds};
pub use service::{HttpResponse, ServiceStats, TileEvent, TileImage, TileService, Transport};
pub use source::{OSM_ATTRIBUTION, OSM_TILE_URL, TileSource, USER_AGENT};
pub use track::{Level, TrackShape};

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
