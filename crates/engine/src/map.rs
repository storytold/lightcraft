//! Map providers, tile URLs, saved locations, and geographic filtering for the Map module.

use lightcraft_geom::{LatLon, TileCoord};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Available slippy map tile providers for LightCraft.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MapProvider {
    /// OpenStreetMap standard cartographic style.
    #[default]
    OpenStreetMap,
    /// Esri World Imagery (satellite/aerial orthophotos).
    EsriImagery,
    /// Esri World Street Map (roads, labels, buildings).
    EsriStreets,
    /// Esri World Topographic Map (terrain, contours).
    EsriTopo,
    /// Esri Hybrid: World Imagery base with Boundaries and Places label overlay.
    EsriHybrid,
}

impl MapProvider {
    pub const ALL: [MapProvider; 5] =
        [MapProvider::OpenStreetMap, MapProvider::EsriImagery, MapProvider::EsriStreets, MapProvider::EsriTopo, MapProvider::EsriHybrid];

    /// The string identifier used in commands and settings.
    pub fn id(self) -> &'static str {
        match self {
            MapProvider::OpenStreetMap => "osm",
            MapProvider::EsriImagery => "esri-imagery",
            MapProvider::EsriStreets => "esri-streets",
            MapProvider::EsriTopo => "esri-topo",
            MapProvider::EsriHybrid => "esri-hybrid",
        }
    }

    /// User-facing label in the Map Style dropdown.
    pub fn label(self) -> &'static str {
        match self {
            MapProvider::OpenStreetMap => "OpenStreetMap",
            MapProvider::EsriImagery => "Esri World Imagery",
            MapProvider::EsriStreets => "Esri World Street",
            MapProvider::EsriTopo => "Esri World Topographic",
            MapProvider::EsriHybrid => "Esri Hybrid",
        }
    }

    /// Parse provider by id or case-insensitive string.
    pub fn parse(s: &str) -> Option<Self> {
        let trimmed = s.trim();
        Self::ALL.into_iter().find(|p| p.id().eq_ignore_ascii_case(trimmed) || p.label().eq_ignore_ascii_case(trimmed))
    }

    /// Maximum supported zoom level for this provider.
    pub fn max_zoom(self) -> u8 {
        match self {
            MapProvider::OpenStreetMap => 19,
            MapProvider::EsriImagery | MapProvider::EsriHybrid => 19,
            MapProvider::EsriStreets | MapProvider::EsriTopo => 18,
        }
    }

    /// Required copyright / attribution text.
    pub fn attribution(self) -> &'static str {
        match self {
            MapProvider::OpenStreetMap => "© OpenStreetMap contributors",
            MapProvider::EsriImagery | MapProvider::EsriStreets | MapProvider::EsriTopo | MapProvider::EsriHybrid => {
                "Tiles © Esri — Source: Esri, i-cubed, USDA, USGS, AEX, GeoEye, Getmapping, Aerogrid, IGN, IGP, UPR-EGP, GIS Community"
            }
        }
    }

    /// URLs for the tiles at coordinate `coord`.
    /// Providers with multi-layer overlays (such as `EsriHybrid`) return base tile first, followed by overlay tile(s).
    pub fn tile_urls(self, coord: TileCoord) -> Vec<String> {
        let (z, x, y) = (coord.z, coord.x, coord.y);
        match self {
            MapProvider::OpenStreetMap => {
                vec![format!("https://tile.openstreetmap.org/{z}/{x}/{y}.png")]
            }
            MapProvider::EsriImagery => {
                vec![format!("https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}")]
            }
            MapProvider::EsriStreets => {
                vec![format!("https://server.arcgisonline.com/ArcGIS/rest/services/World_Street_Map/MapServer/tile/{z}/{y}/{x}")]
            }
            MapProvider::EsriTopo => {
                vec![format!("https://server.arcgisonline.com/ArcGIS/rest/services/World_Topo_Map/MapServer/tile/{z}/{y}/{x}")]
            }
            MapProvider::EsriHybrid => {
                vec![
                    format!("https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}"),
                    format!("https://server.arcgisonline.com/ArcGIS/rest/services/Reference/World_Boundaries_and_Places/MapServer/tile/{z}/{y}/{x}"),
                ]
            }
        }
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

/// Base directory for caching downloaded map tiles locally.
pub fn tile_cache_dir() -> PathBuf {
    crate::config::config_dir().map(|d| d.join("map-tiles")).unwrap_or_else(|| std::env::temp_dir().join("lightcraft-tiles"))
}

/// Construct the disk cache file path for a map tile.
pub fn tile_cache_path(cache_root: &Path, provider: MapProvider, coord: TileCoord, layer_index: usize) -> PathBuf {
    let ext = match provider {
        MapProvider::EsriImagery => "jpg",
        MapProvider::EsriHybrid if layer_index == 0 => "jpg",
        _ => "png",
    };
    cache_root
        .join("tiles")
        .join(provider.id())
        .join(layer_index.to_string())
        .join(coord.z.to_string())
        .join(coord.x.to_string())
        .join(format!("{}.{ext}", coord.y))
}

/// Pure-Rust network fetch for a map tile.
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch_tile_bytes(url_str: &str) -> std::result::Result<Vec<u8>, String> {
    use lightcraft_fetch::http::{Limits, Url};
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    let url = Url::parse(url_str).map_err(|e| format!("invalid tile url: {e}"))?;
    let headers = [
        ("User-Agent", "LightCraft/0.9 (https://github.com/storytold/lightcraft; photo-editor)".to_string()),
        ("Accept", "image/png,image/jpeg,image/*,*/*".to_string()),
    ];
    let cancel = AtomicBool::new(false);
    let limits = Limits { connect: Duration::from_secs(6), stall: Duration::from_secs(12), cancel: &cancel };
    let mut resp = lightcraft_fetch::http::get(&url, &headers, &limits).map_err(|e| format!("fetch tile failed: {e}"))?;
    if resp.status != 200 {
        return Err(format!("fetch tile HTTP {}", resp.status));
    }
    let mut bytes = Vec::with_capacity(resp.content_length().unwrap_or(32 * 1024) as usize);
    let mut buf = [0u8; 8192];
    loop {
        let n = resp.read(&mut buf, &limits).map_err(|e| format!("read tile stream: {e}"))?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(buf.get(..n).unwrap_or_default());
        if bytes.len() > 2 * 1024 * 1024 {
            return Err("tile size exceeds 2 MiB".into());
        }
    }
    Ok(bytes)
}

#[cfg(target_arch = "wasm32")]
pub fn fetch_tile_bytes(_url_str: &str) -> std::result::Result<Vec<u8>, String> {
    Err("tile fetching on web is not implemented via sync engine".into())
}

/// Decodes tile PNG or JPEG bytes into 8-bit RGBA raster pixels.
pub fn decode_tile(bytes: &[u8]) -> std::result::Result<lightcraft_raster::Rgba8, String> {
    let opts = lightcraft_codecs::DecodeOptions::default();
    let decoded = lightcraft_codecs::decode(bytes, opts).map_err(|e| format!("decode tile: {e}"))?;
    Ok(decoded.to_srgb8())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_provider_urls_and_attributes() {
        let coord = TileCoord::new(10, 512, 340);
        let osm = MapProvider::OpenStreetMap;
        assert_eq!(osm.id(), "osm");
        assert_eq!(osm.tile_urls(coord), vec!["https://tile.openstreetmap.org/10/512/340.png"]);

        let hybrid = MapProvider::EsriHybrid;
        let urls = hybrid.tile_urls(coord);
        assert_eq!(urls.len(), 2);
        assert!(urls[0].contains("World_Imagery"));
        assert!(urls[1].contains("World_Boundaries_and_Places"));
    }

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
}
