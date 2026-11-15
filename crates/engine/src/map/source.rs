//! Tile sources: where map tiles come from.
//!
//! The default is OpenStreetMap's standard tile layer, used under the OSMF tile usage policy
//! (<https://operations.osmfoundation.org/policies/tiles/>): the exact URL [`OSM_TILE_URL`], the
//! attribution [`OSM_ATTRIBUTION`] always shown on the map, and a User-Agent naming LightCraft
//! ([`USER_AGENT`]). OpenTopoMap is a second built-in choice; users may also point the map at
//! another raster tile server (an `https://` URL template with `{z}`, `{x}` and `{y}`) or turn
//! tiles off ([`TileSource::Offline`]).
//!
//! The Esri layers need an explicit opt-in ([`TileSource::requires_opt_in`]): until the user has
//! turned them on they are not offered and never fetched. See `docs/map.md` (Esri's terms).
//!
//! Validation of custom sources is ported from the map core of PR #797 (Jason Baldwin).

use serde::{Deserialize, Serialize};

use super::MapError;
use super::mercator::{MAX_ZOOM, TileKey};

/// OpenStreetMap's standard raster tiles.
pub const OSM_TILE_URL: &str = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";
/// The attribution OpenStreetMap's tiles require, shown on the map.
pub const OSM_ATTRIBUTION: &str = "© OpenStreetMap contributors";
/// OpenStreetMap's deepest zoom.
pub const OSM_MAX_ZOOM: u8 = 19;
/// OpenTopoMap's raster tiles (CC-BY-SA; <https://opentopomap.org/about>).
pub const OPENTOPOMAP_TILE_URL: &str = "https://tile.opentopomap.org/{z}/{x}/{y}.png";
/// The credit OpenTopoMap asks for.
pub const OPENTOPOMAP_ATTRIBUTION: &str = "Map data: © OpenStreetMap contributors, SRTM | Map style: © OpenTopoMap (CC-BY-SA)";
const ESRI_STREET_URL: &str = "https://server.arcgisonline.com/ArcGIS/rest/services/World_Street_Map/MapServer/tile/{z}/{y}/{x}";
const ESRI_TOPO_URL: &str = "https://server.arcgisonline.com/ArcGIS/rest/services/World_Topo_Map/MapServer/tile/{z}/{y}/{x}";
const ESRI_IMAGERY_URL: &str = "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}";
/// The credits of each Esri layer, as its ArcGIS Online item gives them (`accessInformation`).
const ESRI_STREET_ATTRIBUTION: &str = "Powered by Esri | Sources: Esri, HERE, Garmin, USGS, Intermap, INCREMENT P, NRCAN, Esri Japan, METI, Esri China (Hong Kong), NOSTRA, © OpenStreetMap contributors, and the GIS User Community";
const ESRI_TOPO_ATTRIBUTION: &str = "Powered by Esri | Sources: Esri, HERE, Garmin, Intermap, INCREMENT P, GEBCO, USGS, FAO, NPS, NRCAN, GeoBase, IGN, Kadaster NL, Ordnance Survey, Esri Japan, METI, Esri China (Hong Kong), © OpenStreetMap contributors, GIS User Community";
const ESRI_IMAGERY_ATTRIBUTION: &str = "Powered by Esri | Esri, Vantor, Earthstar Geographics, and the GIS User Community";
/// The User-Agent sent with every tile request: distinct and stable, naming the app and where to
/// find it, as the OSMF policy asks. No personal details.
pub const USER_AGENT: &str = concat!("LightCraft/", env!("CARGO_PKG_VERSION"), " (+https://github.com/storytold/lightcraft)");

/// Longest URL template accepted.
const MAX_TEMPLATE: usize = 512;
/// Longest attribution accepted.
const MAX_ATTRIBUTION: usize = 200;
/// Longest source id accepted.
const MAX_ID: usize = 40;

/// Where tiles come from. The built-in sources carry their URL, attribution and max zoom; a
/// [`TileSource::Custom`] one is built with [`TileSource::custom`], which checks everything.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TileSource {
    /// OpenStreetMap's standard tiles (the default).
    #[default]
    Osm,
    /// OpenTopoMap (topographic, from OpenStreetMap data and SRTM).
    OpenTopoMap,
    /// Esri World Street Map (opt-in).
    EsriStreet,
    /// Esri World Topographic Map (opt-in).
    EsriTopo,
    /// Esri World Imagery (opt-in).
    EsriImagery,
    /// No tiles and no network: the map shows only what is drawn on top of it (pins, tracks).
    Offline,
    /// A user-configured raster tile server.
    Custom {
        /// Short id, also the name of the source's cache folder (`[a-z0-9._-]`).
        id: String,
        /// `https://…/{z}/{x}/{y}.png`.
        url_template: String,
        /// The credit shown on the map (always visible, never behind a toggle).
        attribution: String,
        /// Deepest zoom the server has tiles for (views zoomed further scale these up).
        max_zoom: u8,
    },
}

impl TileSource {
    /// The built-in sources, in the order a picker lists them.
    pub const BUILT_IN: [TileSource; 6] =
        [TileSource::Osm, TileSource::OpenTopoMap, TileSource::EsriStreet, TileSource::EsriTopo, TileSource::EsriImagery, TileSource::Offline];

    /// The built-in source with id `id` (also accepts a few spellings: `openstreetmap`,
    /// `esri-street`, `esriImagery`…), case-insensitive.
    pub fn built_in(id: &str) -> Option<TileSource> {
        let id = id.trim().to_ascii_lowercase().replace(['_', ' '], "-");
        Some(match id.as_str() {
            "osm" | "openstreetmap" => TileSource::Osm,
            "opentopomap" | "otm" => TileSource::OpenTopoMap,
            "esri-streets" | "esri-street" | "esristreet" | "esristreets" => TileSource::EsriStreet,
            "esri-topo" | "esritopo" => TileSource::EsriTopo,
            "esri-imagery" | "esriimagery" => TileSource::EsriImagery,
            "offline" | "none" => TileSource::Offline,
            _ => return None,
        })
    }

    /// Short id: the built-in name, or the custom source's own id. Also the cache folder name.
    pub fn id(&self) -> &str {
        match self {
            TileSource::Osm => "osm",
            TileSource::OpenTopoMap => "opentopomap",
            TileSource::EsriStreet => "esri-streets",
            TileSource::EsriTopo => "esri-topo",
            TileSource::EsriImagery => "esri-imagery",
            TileSource::Offline => "offline",
            TileSource::Custom { id, .. } => id,
        }
    }

    /// English name for a picker (the UI translates it).
    pub fn label(&self) -> &str {
        match self {
            TileSource::Osm => "OpenStreetMap",
            TileSource::OpenTopoMap => "OpenTopoMap",
            TileSource::EsriStreet => "Esri World Street Map",
            TileSource::EsriTopo => "Esri World Topographic Map",
            TileSource::EsriImagery => "Esri World Imagery",
            TileSource::Offline => "Offline (no tiles)",
            TileSource::Custom { .. } => "Custom tile server",
        }
    }

    /// `https://…/{z}/{x}/{y}…`; `None` for the offline source.
    pub fn url_template(&self) -> Option<&str> {
        Some(match self {
            TileSource::Osm => OSM_TILE_URL,
            TileSource::OpenTopoMap => OPENTOPOMAP_TILE_URL,
            TileSource::EsriStreet => ESRI_STREET_URL,
            TileSource::EsriTopo => ESRI_TOPO_URL,
            TileSource::EsriImagery => ESRI_IMAGERY_URL,
            TileSource::Offline => return None,
            TileSource::Custom { url_template, .. } => url_template,
        })
    }

    /// The credit painted on the map whenever its tiles are (empty for the offline source).
    pub fn attribution(&self) -> &str {
        match self {
            TileSource::Osm => OSM_ATTRIBUTION,
            TileSource::OpenTopoMap => OPENTOPOMAP_ATTRIBUTION,
            TileSource::EsriStreet => ESRI_STREET_ATTRIBUTION,
            TileSource::EsriTopo => ESRI_TOPO_ATTRIBUTION,
            TileSource::EsriImagery => ESRI_IMAGERY_ATTRIBUTION,
            TileSource::Offline => "",
            TileSource::Custom { attribution, .. } => attribution,
        }
    }

    /// Deepest zoom the server has tiles for.
    pub fn max_zoom(&self) -> u8 {
        match self {
            TileSource::Osm | TileSource::Offline => OSM_MAX_ZOOM,
            TileSource::OpenTopoMap => 17,
            TileSource::EsriStreet | TileSource::EsriTopo => 18,
            TileSource::EsriImagery => 19,
            TileSource::Custom { max_zoom, .. } => *max_zoom,
        }
    }

    /// Esri's layers: offered and fetched only after the user turns them on (`docs/map.md`).
    pub fn requires_opt_in(&self) -> bool {
        matches!(self, TileSource::EsriStreet | TileSource::EsriTopo | TileSource::EsriImagery)
    }

    /// No network: tiles are never fetched.
    pub fn is_offline(&self) -> bool {
        matches!(self, TileSource::Offline)
    }

    /// The User-Agent sent with every request ([`USER_AGENT`]).
    pub fn user_agent(&self) -> &'static str {
        USER_AGENT
    }

    /// The URL of `key`, or `None` for the offline source (or a key past the source's max zoom).
    pub fn url(&self, key: TileKey) -> Option<String> {
        let template = self.url_template()?;
        if key.z > self.max_zoom() {
            return None;
        }
        Some(template.replace("{z}", &key.z.to_string()).replace("{x}", &key.x.to_string()).replace("{y}", &key.y.to_string()))
    }

    /// The server's host name (lower-case), for messages.
    pub fn host(&self) -> Option<String> {
        let rest = self.url_template()?.split_once("://")?.1;
        let host = rest.split(['/', '?', '#']).next()?;
        (!host.is_empty()).then(|| host.to_ascii_lowercase())
    }

    /// A user-configured raster source, checked: the template must be an `https://` URL with
    /// `{z}`, `{x}` and `{y}` (and no other placeholder), the attribution non-empty (tile licences
    /// require credit; OpenStreetMap's template gives [`TileSource::Osm`]), the max zoom at most
    /// [`MAX_ZOOM`]. `id` names the cache folder; when absent it is derived from the template
    /// (`custom-<hash>`). It may not be a built-in source's id.
    pub fn custom(id: Option<&str>, url_template: &str, attribution: Option<&str>, max_zoom: Option<u8>) -> Result<TileSource, MapError> {
        let url_template = url_template.trim();
        validate_template(url_template)?;
        if url_template == OSM_TILE_URL && id.is_none_or(|i| i.trim().is_empty() || i.trim() == "osm") {
            if max_zoom.is_some_and(|z| z != OSM_MAX_ZOOM) {
                return Err(bad(format!("OpenStreetMap has tiles up to zoom {OSM_MAX_ZOOM}")));
            }
            return Ok(TileSource::Osm);
        }
        let attribution = match attribution.map(str::trim).filter(|a| !a.is_empty()) {
            Some(a) => a.to_string(),
            None => return Err(bad("an attribution is required (the tile provider's credit, shown on the map)")),
        };
        if attribution.chars().count() > MAX_ATTRIBUTION || attribution.chars().any(char::is_control) {
            return Err(bad(format!("the attribution must be one line of at most {MAX_ATTRIBUTION} characters")));
        }
        let max_zoom = max_zoom.unwrap_or(18);
        if max_zoom > MAX_ZOOM {
            return Err(bad(format!("maxZoom must be 0..={MAX_ZOOM}")));
        }
        let id = match id.map(str::trim).filter(|i| !i.is_empty()) {
            Some(id) => {
                validate_id(id)?;
                if TileSource::built_in(id).is_some() {
                    return Err(bad(format!("the id `{id}` is a built-in source's; give this source another id")));
                }
                id.to_string()
            }
            None => format!("custom-{:08x}", fnv1a(url_template.as_bytes()) as u32),
        };
        Ok(TileSource::Custom { id, url_template: url_template.to_string(), attribution, max_zoom })
    }

    /// Re-check a stored source (a settings file is hostile input): built-in sources as they are,
    /// a custom one through [`TileSource::custom`].
    pub fn checked(&self) -> Result<TileSource, MapError> {
        match self {
            TileSource::Custom { id, url_template, attribution, max_zoom } => {
                TileSource::custom(Some(id), url_template, Some(attribution), Some(*max_zoom))
            }
            other => Ok(other.clone()),
        }
    }
}

fn bad(msg: impl Into<String>) -> MapError {
    MapError::InvalidSource(msg.into())
}

/// FNV-1a, 64 bit: a stable hash for derived cache-folder names.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

/// A source id: 1–40 characters of `[a-z0-9._-]`, not starting with `.`.
pub fn validate_id(id: &str) -> Result<(), MapError> {
    let ok_chars = id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    if id.is_empty() || id.len() > MAX_ID || !ok_chars || id.starts_with('.') {
        return Err(bad(format!("the id must be 1–{MAX_ID} characters of a–z, 0–9, '.', '_' or '-' (not starting with '.')")));
    }
    Ok(())
}

/// A tile URL template: `https://host/…` with `{z}`, `{x}` and `{y}` each once, no other `{…}`, no
/// spaces or control characters, at most 512 characters.
pub fn validate_template(t: &str) -> Result<(), MapError> {
    if t.len() > MAX_TEMPLATE {
        return Err(bad(format!("the URL template is longer than {MAX_TEMPLATE} characters")));
    }
    if t.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(bad("the URL template has spaces or control characters"));
    }
    let Some(rest) = t.strip_prefix("https://") else {
        return Err(bad("the URL template must start with https:// (tiles are only fetched over a secure connection)"));
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_ok = !host.is_empty()
        && !host.contains('@')
        && !host.contains('{')
        && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'));
    if !host_ok {
        return Err(bad("the URL template needs a plain host name after https:// (no placeholders or user names there)"));
    }
    for p in ["{z}", "{x}", "{y}"] {
        if t.matches(p).count() != 1 {
            return Err(bad(format!("the URL template must contain {p} exactly once")));
        }
    }
    let stripped = t.replace("{z}", "").replace("{x}", "").replace("{y}", "");
    if stripped.contains(['{', '}']) {
        return Err(bad("the URL template may only use the {z}, {x} and {y} placeholders"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_openstreetmap_under_its_policy() {
        let s = TileSource::default();
        assert_eq!(s, TileSource::Osm);
        assert_eq!(s.url_template(), Some("https://tile.openstreetmap.org/{z}/{x}/{y}.png"));
        assert_eq!(s.attribution(), "© OpenStreetMap contributors");
        assert_eq!((s.id(), s.max_zoom()), ("osm", 19));
        assert_eq!(s.user_agent(), format!("LightCraft/{} (+https://github.com/storytold/lightcraft)", env!("CARGO_PKG_VERSION")));
        assert_eq!(s.url(TileKey::new(10, 511, 340).unwrap()).as_deref(), Some("https://tile.openstreetmap.org/10/511/340.png"));
        assert_eq!(s.url(TileKey::new(20, 0, 0).unwrap()), None, "past the max zoom");
        assert_eq!(s.host().as_deref(), Some("tile.openstreetmap.org"));
        assert_eq!(s.checked().unwrap(), s);
        assert!(!s.requires_opt_in());
    }

    #[test]
    fn built_in_sources_have_urls_credits_and_opt_in() {
        for s in TileSource::BUILT_IN {
            assert_eq!(TileSource::built_in(s.id()), Some(s.clone()), "{s:?}");
            if !s.is_offline() {
                assert!(!s.attribution().is_empty(), "{s:?} always has a credit");
                assert!(s.url_template().is_some_and(|t| validate_template(t).is_ok()), "{s:?}");
            }
        }
        let esri: Vec<_> = TileSource::BUILT_IN.into_iter().filter(TileSource::requires_opt_in).collect();
        assert_eq!(esri, vec![TileSource::EsriStreet, TileSource::EsriTopo, TileSource::EsriImagery]);
        // Esri's tile URLs are row before column
        assert!(TileSource::EsriImagery.url(TileKey::new(3, 1, 2).unwrap()).unwrap().ends_with("/tile/3/2/1"));
        assert_eq!(TileSource::built_in("OpenStreetMap"), Some(TileSource::Osm));
        assert_eq!(TileSource::built_in("esri-street"), Some(TileSource::EsriStreet));
        assert_eq!(TileSource::built_in("bing"), None);
    }

    #[test]
    fn offline_never_has_urls() {
        let s = TileSource::Offline;
        assert!(s.is_offline());
        assert_eq!(s.url(TileKey::new(0, 0, 0).unwrap()), None);
        assert_eq!(s.checked().unwrap(), s);
    }

    #[test]
    fn custom_sources_are_checked() {
        let s = TileSource::custom(None, "https://tiles.example.org/base/{z}/{x}/{y}.png?key=1", Some("© Example"), Some(17)).unwrap();
        assert!(s.id().starts_with("custom-") && s.id().len() == 15, "{}", s.id());
        assert_eq!(s.url(TileKey::new(3, 1, 2).unwrap()).as_deref(), Some("https://tiles.example.org/base/3/1/2.png?key=1"));
        assert_eq!(
            TileSource::custom(None, "https://tiles.example.org/base/{z}/{x}/{y}.png?key=1", Some("© Example"), None).unwrap().id(),
            s.id(),
            "stable id"
        );
        let named = TileSource::custom(Some("my-tiles"), "https://a.example/{z}/{y}/{x}", Some("© A"), None).unwrap();
        assert_eq!((named.id(), named.max_zoom()), ("my-tiles", 18));
        // OpenStreetMap's own template is OpenStreetMap
        assert_eq!(TileSource::custom(None, OSM_TILE_URL, None, None).unwrap(), TileSource::Osm);
        for (template, attribution, zoom, id) in [
            ("http://tiles.example.org/{z}/{x}/{y}.png", Some("x"), None, None),
            ("ftp://tiles.example.org/{z}/{x}/{y}.png", Some("x"), None, None),
            ("file:///tmp/{z}/{x}/{y}.png", Some("x"), None, None),
            ("https://tiles.example.org/{z}/{x}.png", Some("x"), None, None),
            ("https://tiles.example.org/{z}/{x}/{y}/{y}.png", Some("x"), None, None),
            ("https://tiles.example.org/{s}/{z}/{x}/{y}.png", Some("x"), None, None),
            ("https://{z}.example.org/{x}/{y}.png", Some("x"), None, None),
            ("https://user@tiles.example.org/{z}/{x}/{y}.png", Some("x"), None, None),
            ("https:///{z}/{x}/{y}.png", Some("x"), None, None),
            ("https://tiles.example.org/{z}/{x}/{y}.png\r\nX: 1", Some("x"), None, None),
            ("https://tiles.example.org/{z}/{x}/{y}.png", None, None, None),
            ("https://tiles.example.org/{z}/{x}/{y}.png", Some("  "), None, None),
            ("https://tiles.example.org/{z}/{x}/{y}.png", Some("a\nb"), None, None),
            ("https://tiles.example.org/{z}/{x}/{y}.png", Some("x"), Some(25), None),
            ("https://tiles.example.org/{z}/{x}/{y}.png", Some("x"), None, Some("osm")),
            ("https://tiles.example.org/{z}/{x}/{y}.png", Some("x"), None, Some("offline")),
            ("https://tiles.example.org/{z}/{x}/{y}.png", Some("x"), None, Some("esri-imagery")),
            ("https://tiles.example.org/{z}/{x}/{y}.png", Some("x"), None, Some("../etc")),
            ("https://tiles.example.org/{z}/{x}/{y}.png", Some("x"), None, Some("Upper")),
            (OSM_TILE_URL, None, Some(20), None),
        ] {
            assert!(TileSource::custom(id, template, attribution, zoom).is_err(), "{template} {attribution:?} {zoom:?} {id:?}");
        }
        assert!(TileSource::custom(None, &format!("https://a.example/{}{{z}}/{{x}}/{{y}}", "p".repeat(600)), Some("x"), None).is_err());
    }

    #[test]
    fn stored_sources_are_rechecked_and_round_trip() {
        let evil =
            TileSource::Custom { id: "evil".into(), url_template: "http://evil.example/{z}/{x}/{y}".into(), attribution: "x".into(), max_zoom: 19 };
        assert!(evil.checked().is_err());
        for s in
            TileSource::BUILT_IN
                .into_iter()
                .chain([TileSource::custom(Some("mine"), "https://t.example/{z}/{x}/{y}.png", Some("© T"), Some(12)).unwrap()])
        {
            let json = serde_json::to_string(&s).unwrap();
            assert!(!json.contains("userAgent"), "the User-Agent is not a setting: {json}");
            let back: TileSource = serde_json::from_str(&json).unwrap();
            assert_eq!(back, s);
        }
        assert_eq!(serde_json::to_value(TileSource::Osm).unwrap(), serde_json::json!({"kind": "osm"}));
        assert!(serde_json::from_str::<TileSource>(r#"{"kind": "bing"}"#).is_err());
    }
}
