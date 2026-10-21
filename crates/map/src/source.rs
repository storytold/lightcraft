//! Tile sources: where map tiles come from.
//!
//! The default is OpenStreetMap's standard tile layer, used under the OSMF tile usage policy
//! (<https://operations.osmfoundation.org/policies/tiles/>): the exact URL [`OSM_TILE_URL`], the
//! attribution [`OSM_ATTRIBUTION`] shown on the map, and a User-Agent naming LightCraft
//! ([`USER_AGENT`]). Users may point the map at another raster tile server (an `https://` URL
//! template with `{z}`, `{x}` and `{y}`) or turn tiles off ([`TileSource::offline`]).

use serde::{Deserialize, Serialize};

use crate::MapError;
use crate::mercator::{MAX_ZOOM, TileKey};

/// OpenStreetMap's standard raster tiles.
pub const OSM_TILE_URL: &str = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";
/// The attribution OpenStreetMap's tiles require, shown on the map.
pub const OSM_ATTRIBUTION: &str = "© OpenStreetMap contributors";
/// OpenStreetMap's deepest zoom.
pub const OSM_MAX_ZOOM: u8 = 19;
/// The id of the default source (and its cache folder).
pub const OSM_ID: &str = "osm";
/// The id of the no-network source.
pub const OFFLINE_ID: &str = "offline";
/// The User-Agent sent with every tile request: distinct and stable, naming the app and where to
/// find it, as the OSMF policy asks.
pub const USER_AGENT: &str = concat!("LightCraft/", env!("CARGO_PKG_VERSION"), " (+https://github.com/storytold/lightcraft)");

/// Longest URL template accepted.
const MAX_TEMPLATE: usize = 512;
/// Longest attribution accepted.
const MAX_ATTRIBUTION: usize = 200;
/// Longest source id accepted.
const MAX_ID: usize = 40;

/// Where tiles come from. Build one with [`TileSource::openstreetmap`], [`TileSource::offline`] or
/// [`TileSource::custom`] (which checks everything); the engine stores the user's choice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TileSource {
    /// Short id, also the name of the source's cache folder (`[a-z0-9._-]`).
    pub id: String,
    /// `https://…/{z}/{x}/{y}.png`; empty for the offline source.
    pub url_template: String,
    /// The credit shown on the map (always visible, never behind a toggle).
    pub attribution: String,
    /// Deepest zoom the server has tiles for (views zoomed further scale these up).
    pub max_zoom: u8,
    /// Sent with every request ([`USER_AGENT`]).
    #[serde(skip, default = "user_agent")]
    pub user_agent: String,
}

fn user_agent() -> String {
    USER_AGENT.to_string()
}

impl Default for TileSource {
    fn default() -> Self {
        TileSource::openstreetmap()
    }
}

impl TileSource {
    /// OpenStreetMap's standard tiles (the default).
    pub fn openstreetmap() -> TileSource {
        TileSource {
            id: OSM_ID.into(),
            url_template: OSM_TILE_URL.into(),
            attribution: OSM_ATTRIBUTION.into(),
            max_zoom: OSM_MAX_ZOOM,
            user_agent: user_agent(),
        }
    }

    /// No tiles and no network: the map shows only what is drawn on top of it (pins, tracks).
    pub fn offline() -> TileSource {
        TileSource {
            id: OFFLINE_ID.into(),
            url_template: String::new(),
            attribution: String::new(),
            max_zoom: OSM_MAX_ZOOM,
            user_agent: user_agent(),
        }
    }

    /// A user-configured raster source, checked: the template must be an `https://` URL with
    /// `{z}`, `{x}` and `{y}` (and no other placeholder), the attribution non-empty (tile licences
    /// require credit; OpenStreetMap's template gets [`OSM_ATTRIBUTION`] when none is given), the
    /// max zoom at most [`MAX_ZOOM`]. `id` names the cache folder; when absent it is derived from
    /// the template (`osm` for OpenStreetMap's, `custom-<hash>` otherwise).
    pub fn custom(id: Option<&str>, url_template: &str, attribution: Option<&str>, max_zoom: Option<u8>) -> Result<TileSource, MapError> {
        let url_template = url_template.trim();
        validate_template(url_template)?;
        let is_osm = url_template == OSM_TILE_URL;
        let attribution = match attribution.map(str::trim).filter(|a| !a.is_empty()) {
            Some(a) => a.to_string(),
            None if is_osm => OSM_ATTRIBUTION.to_string(),
            None => return Err(bad("an attribution is required (the tile provider's credit, shown on the map)")),
        };
        if attribution.chars().count() > MAX_ATTRIBUTION || attribution.chars().any(char::is_control) {
            return Err(bad(format!("the attribution must be one line of at most {MAX_ATTRIBUTION} characters")));
        }
        let max_zoom = max_zoom.unwrap_or(if is_osm { OSM_MAX_ZOOM } else { 18 });
        if max_zoom > MAX_ZOOM {
            return Err(bad(format!("maxZoom must be 0..={MAX_ZOOM}")));
        }
        if is_osm && max_zoom > OSM_MAX_ZOOM {
            return Err(bad(format!("OpenStreetMap has tiles up to zoom {OSM_MAX_ZOOM}")));
        }
        let id = match id.map(str::trim).filter(|i| !i.is_empty()) {
            Some(id) => {
                validate_id(id)?;
                if id == OSM_ID && !is_osm {
                    return Err(bad("the id `osm` is OpenStreetMap's; give this source another id"));
                }
                id.to_string()
            }
            None if is_osm => OSM_ID.to_string(),
            None => format!("custom-{:08x}", fnv1a(url_template.as_bytes()) as u32),
        };
        Ok(TileSource { id, url_template: url_template.to_string(), attribution, max_zoom, user_agent: user_agent() })
    }

    /// Re-check a stored source (a settings file is hostile input): the offline and OpenStreetMap
    /// sources as they are, anything else through [`TileSource::custom`].
    pub fn checked(&self) -> Result<TileSource, MapError> {
        if self.id == OFFLINE_ID && self.url_template.is_empty() {
            return Ok(TileSource::offline());
        }
        TileSource::custom(Some(&self.id), &self.url_template, Some(&self.attribution), Some(self.max_zoom))
    }

    /// No network: tiles are never fetched.
    pub fn is_offline(&self) -> bool {
        self.url_template.is_empty()
    }

    /// The URL of `key`, or `None` for the offline source (or a key past the source's max zoom).
    pub fn url(&self, key: TileKey) -> Option<String> {
        if self.is_offline() || key.z > self.max_zoom {
            return None;
        }
        Some(self.url_template.replace("{z}", &key.z.to_string()).replace("{x}", &key.x.to_string()).replace("{y}", &key.y.to_string()))
    }

    /// The server's host name (lower-case), for messages and backoff.
    pub fn host(&self) -> Option<String> {
        let rest = self.url_template.split_once("://")?.1;
        let host = rest.split(['/', '?', '#']).next()?;
        (!host.is_empty()).then(|| host.to_ascii_lowercase())
    }
}

fn bad(msg: impl Into<String>) -> MapError {
    MapError::InvalidSource(msg.into())
}

/// FNV-1a, 64 bit: a stable hash for derived cache-folder names.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

/// A source id: 1–40 characters of `[a-z0-9._-]`, not starting with `.`, not `offline`.
pub fn validate_id(id: &str) -> Result<(), MapError> {
    let ok_chars = id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    if id.is_empty() || id.len() > MAX_ID || !ok_chars || id.starts_with('.') {
        return Err(bad(format!("the id must be 1–{MAX_ID} characters of a–z, 0–9, '.', '_' or '-' (not starting with '.')")));
    }
    if id == OFFLINE_ID {
        return Err(bad("the id `offline` is reserved for the no-network choice"));
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
        assert_eq!(s.url_template, "https://tile.openstreetmap.org/{z}/{x}/{y}.png");
        assert_eq!(s.attribution, "© OpenStreetMap contributors");
        assert_eq!((s.id.as_str(), s.max_zoom), ("osm", 19));
        assert_eq!(s.user_agent, format!("LightCraft/{} (+https://github.com/storytold/lightcraft)", env!("CARGO_PKG_VERSION")));
        assert_eq!(s.url(TileKey::new(10, 511, 340).unwrap()).as_deref(), Some("https://tile.openstreetmap.org/10/511/340.png"));
        assert_eq!(s.url(TileKey::new(20, 0, 0).unwrap()), None, "past the max zoom");
        assert_eq!(s.host().as_deref(), Some("tile.openstreetmap.org"));
        assert_eq!(s.checked().unwrap(), s);
    }

    #[test]
    fn offline_never_has_urls() {
        let s = TileSource::offline();
        assert!(s.is_offline());
        assert_eq!(s.url(TileKey::new(0, 0, 0).unwrap()), None);
        assert_eq!(s.checked().unwrap(), s);
    }

    #[test]
    fn custom_sources_are_checked() {
        let s = TileSource::custom(None, "https://tiles.example.org/base/{z}/{x}/{y}.png?key=1", Some("© Example"), Some(17)).unwrap();
        assert!(s.id.starts_with("custom-") && s.id.len() == 15, "{}", s.id);
        assert_eq!(s.url(TileKey::new(3, 1, 2).unwrap()).as_deref(), Some("https://tiles.example.org/base/3/1/2.png?key=1"));
        assert_eq!(
            TileSource::custom(None, "https://tiles.example.org/base/{z}/{x}/{y}.png?key=1", Some("© Example"), None).unwrap().id,
            s.id,
            "stable id"
        );
        let named = TileSource::custom(Some("my-tiles"), "https://a.example/{z}/{y}/{x}", Some("© A"), None).unwrap();
        assert_eq!((named.id.as_str(), named.max_zoom), ("my-tiles", 18));
        // OpenStreetMap's own template gets its attribution and id
        let osm = TileSource::custom(None, OSM_TILE_URL, None, None).unwrap();
        assert_eq!(osm, TileSource::openstreetmap());
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
            ("https://tiles.example.org/{z}/{x}/{y}.png", Some("x"), None, Some("../etc")),
            ("https://tiles.example.org/{z}/{x}/{y}.png", Some("x"), None, Some("Upper")),
            (OSM_TILE_URL, None, Some(20), None),
        ] {
            assert!(TileSource::custom(id, template, attribution, zoom).is_err(), "{template} {attribution:?} {zoom:?} {id:?}");
        }
        assert!(TileSource::custom(None, &format!("https://a.example/{}{{z}}/{{x}}/{{y}}", "p".repeat(600)), Some("x"), None).is_err());
    }

    #[test]
    fn stored_sources_are_rechecked() {
        let mut s = TileSource::openstreetmap();
        s.url_template = "http://evil.example/{z}/{x}/{y}".into();
        assert!(s.checked().is_err());
        let json = serde_json::to_string(&TileSource::openstreetmap()).unwrap();
        assert!(!json.contains("userAgent"), "the User-Agent is not a setting: {json}");
        let back: TileSource = serde_json::from_str(&json).unwrap();
        assert_eq!(back, TileSource::openstreetmap());
    }
}
