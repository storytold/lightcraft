//! Maps and places (L2), the non-UI half of the Map module:
//!
//! - [`mercator`]: Web-Mercator projection, tile keys, great-circle distances.
//! - [`tiles`]: tile servers (OpenStreetMap by default, terrain, satellite, user-added XYZ), their
//!   URLs and attribution.
//! - [`cache`]: the on-disk tile cache with a size limit and an expiry.
//! - [`fetch`] (native): tile downloads through `dac-net`, cache first, polite to the servers
//!   (the brand's User-Agent, no bulk prefetch).
//! - [`cluster`]: grid clustering of photo pins, zoom dependent.
//! - [`tracks`]: track display from GPX (parsed by `dac-meta`), KML and GeoJSON.
//! - [`places`]: saved locations (centre, radius, private flag).
//! - [`geocode`]: opt-in reverse geocoding, offline (a GeoNames cities file) or online (a
//!   Nominatim-compatible endpoint, rate-limited); nothing is sent without consent.
//!
//! Sources: Web-Mercator / slippy-map tile numbering from the OpenStreetMap wiki ("Slippy map
//! tilenames", prose spec); haversine from the textbook formula; GeoNames dump format from the
//! readme on download.geonames.org; Nominatim's reverse API from its public documentation;
//! clustering and the rest are own design.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod cache;
pub mod cluster;
#[cfg(not(target_arch = "wasm32"))]
pub mod fetch;
pub mod geocode;
pub mod mercator;
pub mod places;
#[cfg(test)]
mod robust_tests;
pub mod tiles;
pub mod tracks;
mod unzip;

pub use cluster::{Cluster, cluster};
pub use mercator::{LatLon, TileKey, distance_m};
pub use places::SavedLocation;
pub use tiles::TileServer;

/// Errors a user (or agent) can act on.
#[derive(Debug, Clone, PartialEq)]
pub enum GeoError {
    /// A malformed input (track file, dataset, server template, response).
    Invalid(String),
    /// Reading or writing a file.
    Io(String),
    /// A request failed (network, HTTP status).
    Net(String),
    /// A network call was asked for without the user's consent.
    NoConsent,
}

impl std::fmt::Display for GeoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GeoError::Invalid(s) => write!(f, "{s}"),
            GeoError::Io(s) => write!(f, "{s}"),
            GeoError::Net(s) => write!(f, "{s}"),
            GeoError::NoConsent => write!(f, "online lookups are off: turn on online reverse geocoding first"),
        }
    }
}

impl std::error::Error for GeoError {}

impl From<std::io::Error> for GeoError {
    fn from(e: std::io::Error) -> Self {
        GeoError::Io(e.to_string())
    }
}
