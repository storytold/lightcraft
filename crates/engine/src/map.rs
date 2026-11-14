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
/// Largest map tile accepted from a server; real 256 px tiles are tens of KiB.
#[cfg(not(target_arch = "wasm32"))]
const MAX_TILE_BYTES: usize = 2 * 1024 * 1024;

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
    // The announced length comes from the server: refuse one past the cap instead of reserving it.
    let announced = resp.content_length();
    if announced.is_some_and(|n| n > MAX_TILE_BYTES as u64) {
        return Err("tile size exceeds 2 MiB".into());
    }
    let capacity = announced.and_then(|n| usize::try_from(n).ok()).unwrap_or(32 * 1024).min(MAX_TILE_BYTES);
    let mut bytes = Vec::with_capacity(capacity);
    let mut buf = [0u8; 8192];
    loop {
        let n = resp.read(&mut buf, &limits).map_err(|e| format!("read tile stream: {e}"))?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(buf.get(..n).unwrap_or_default());
        if bytes.len() > MAX_TILE_BYTES {
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

    /// Serves one response with `head` (status line and headers) and `body` from a local server
    /// (never the internet) and returns the tile URL that reaches it.
    #[cfg(not(target_arch = "wasm32"))]
    fn serve_once(head: &'static str, body: &'static [u8]) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else { return };
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body);
        });
        (format!("http://127.0.0.1:{port}/6/25/31.png"), server)
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_huge_announced_tile_length_is_an_error_not_an_allocation() {
        // A server announcing u64::MAX bytes used to size the buffer from that number and panic
        // with a capacity overflow before reading a byte.
        let (url, server) = serve_once("HTTP/1.1 200 OK\r\nContent-Length: 18446744073709551615\r\n\r\n", b"tile");
        let err = fetch_tile_bytes(&url).unwrap_err();
        assert!(err.contains("2 MiB"), "{err}");
        server.join().unwrap();
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_tile_with_an_honest_length_is_read_whole() {
        let (url, server) = serve_once("HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\n", b"tile");
        assert_eq!(fetch_tile_bytes(&url).unwrap(), b"tile");
        server.join().unwrap();
    }
}
