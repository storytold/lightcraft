//! Tile servers: an XYZ URL template, attribution and zoom range. OpenStreetMap is the default
//! (its tile usage policy: a real User-Agent, attribution on screen, no bulk downloading — the
//! app only fetches what is on screen and caches it). Users add their own servers.

use serde::{Deserialize, Serialize};

use crate::GeoError;
use crate::mercator::{MAX_ZOOM, TileKey};

/// What a server draws (the style picker groups by it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TileKind {
    #[default]
    Road,
    Terrain,
    Satellite,
}

/// One XYZ tile server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TileServer {
    /// Stable id (the cache folder name): `[a-z0-9-]`.
    pub id: String,
    pub name: String,
    /// `https://…/{z}/{x}/{y}.png`; `{s}` picks one of `subdomains`.
    pub url: String,
    /// Shown on the map, always.
    pub attribution: String,
    #[serde(default = "default_max_zoom")]
    pub max_zoom: u8,
    #[serde(default)]
    pub kind: TileKind,
    #[serde(default)]
    pub subdomains: Vec<String>,
}

fn default_max_zoom() -> u8 {
    19
}

impl TileServer {
    /// A user-added server; checks the template.
    pub fn custom(id: &str, name: &str, url: &str, attribution: &str, max_zoom: u8, kind: TileKind) -> Result<TileServer, GeoError> {
        let s = TileServer {
            id: id.to_string(),
            name: name.to_string(),
            url: url.to_string(),
            attribution: attribution.to_string(),
            max_zoom: max_zoom.min(MAX_ZOOM),
            kind,
            subdomains: Vec::new(),
        };
        s.validate()?;
        Ok(s)
    }

    pub fn validate(&self) -> Result<(), GeoError> {
        if self.id.is_empty() || self.id.len() > 64 || !self.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return Err(GeoError::Invalid(format!("tile server id `{}`: use a-z, 0-9 and -", self.id)));
        }
        if !(self.url.starts_with("https://") || self.url.starts_with("http://")) {
            return Err(GeoError::Invalid("a tile server URL starts with https:// or http://".into()));
        }
        for t in ["{z}", "{x}", "{y}"] {
            if !self.url.contains(t) {
                return Err(GeoError::Invalid(format!("the tile URL needs {t} (e.g. https://tile.example.org/{{z}}/{{x}}/{{y}}.png)")));
            }
        }
        if self.url.contains("{s}") && self.subdomains.is_empty() {
            return Err(GeoError::Invalid("the tile URL uses {s} but no subdomains are given".into()));
        }
        if self.attribution.trim().is_empty() {
            return Err(GeoError::Invalid("a tile server needs its attribution (shown on the map)".into()));
        }
        Ok(())
    }

    /// The URL of one tile.
    pub fn tile_url(&self, k: TileKey) -> String {
        let s = match self.subdomains.len() {
            0 => "",
            n => self.subdomains.get(((k.x as usize) + (k.y as usize)) % n).map(String::as_str).unwrap_or(""),
        };
        self.url.replace("{z}", &k.z.to_string()).replace("{x}", &k.x.to_string()).replace("{y}", &k.y.to_string()).replace("{s}", s)
    }
}

/// The built-in servers; the first is the default.
pub fn builtin() -> Vec<TileServer> {
    vec![
        TileServer {
            id: "osm".into(),
            name: "OpenStreetMap".into(),
            url: "https://tile.openstreetmap.org/{z}/{x}/{y}.png".into(),
            attribution: "© OpenStreetMap contributors".into(),
            max_zoom: 19,
            kind: TileKind::Road,
            subdomains: Vec::new(),
        },
        TileServer {
            id: "opentopomap".into(),
            name: "OpenTopoMap (terrain)".into(),
            url: "https://{s}.tile.opentopomap.org/{z}/{x}/{y}.png".into(),
            attribution: "© OpenStreetMap contributors, SRTM | style © OpenTopoMap (CC-BY-SA)".into(),
            max_zoom: 17,
            kind: TileKind::Terrain,
            subdomains: vec!["a".into(), "b".into(), "c".into()],
        },
        TileServer {
            id: "esri-imagery".into(),
            name: "Esri World Imagery (satellite)".into(),
            url: "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}".into(),
            attribution: "Tiles © Esri — Source: Esri, Maxar, Earthstar Geographics, and the GIS User Community".into(),
            max_zoom: 19,
            kind: TileKind::Satellite,
            subdomains: Vec::new(),
        },
    ]
}

/// The built-ins followed by the user's servers (a user server with a built-in id replaces it).
pub fn all(user: &[TileServer]) -> Vec<TileServer> {
    let mut v = builtin();
    for s in user.iter().filter(|s| s.validate().is_ok()) {
        match v.iter_mut().find(|b| b.id == s.id) {
            Some(b) => *b = s.clone(),
            None => v.push(s.clone()),
        }
    }
    v
}

/// Decode a tile (PNG / JPEG / WebP) to RGBA8: `(width, height, pixels)`. Tiles are at most
/// 1024 px square; anything bigger or broken is an error.
pub fn decode_tile(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), GeoError> {
    let img = image::load_from_memory(bytes).map_err(|e| GeoError::Invalid(format!("not a tile image: {e}")))?;
    if img.width() == 0 || img.height() == 0 || img.width() > 1024 || img.height() > 1024 {
        return Err(GeoError::Invalid(format!("unexpected tile size {}×{}", img.width(), img.height())));
    }
    let rgba = img.to_rgba8();
    Ok((rgba.width(), rgba.height(), rgba.into_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_fill_the_template() {
        let osm = &builtin()[0];
        assert_eq!(osm.tile_url(TileKey { z: 3, x: 4, y: 5 }), "https://tile.openstreetmap.org/3/4/5.png");
        let topo = &builtin()[1];
        assert_eq!(topo.tile_url(TileKey { z: 1, x: 1, y: 0 }), "https://b.tile.opentopomap.org/1/1/0.png");
        let esri = &builtin()[2];
        assert!(esri.tile_url(TileKey { z: 2, x: 1, y: 3 }).ends_with("/tile/2/3/1"));
        for s in builtin() {
            s.validate().unwrap();
        }
    }

    #[test]
    fn decodes_png_tiles_and_rejects_junk() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(256, 256, image::Rgba([1, 2, 3, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let (w, h, px) = decode_tile(&png).unwrap();
        assert_eq!((w, h, &px[..4]), (256, 256, &[1u8, 2, 3, 255][..]));
        assert!(decode_tile(b"not an image").is_err());
        assert!(decode_tile(&png[..40]).is_err());
    }

    #[test]
    fn custom_servers_are_checked() {
        assert!(TileServer::custom("mine", "Mine", "https://t.example/{z}/{x}/{y}.png", "© me", 18, TileKind::Road).is_ok());
        assert!(TileServer::custom("Mine!", "x", "https://t/{z}/{x}/{y}", "a", 1, TileKind::Road).is_err());
        assert!(TileServer::custom("m", "x", "ftp://t/{z}/{x}/{y}", "a", 1, TileKind::Road).is_err());
        assert!(TileServer::custom("m", "x", "https://t/{z}/{x}", "a", 1, TileKind::Road).is_err());
        assert!(TileServer::custom("m", "x", "https://t/{z}/{x}/{y}", " ", 1, TileKind::Road).is_err());
        let s = TileServer::custom("osm", "Own", "http://localhost/{z}/{x}/{y}.png", "local", 25, TileKind::Road).unwrap();
        assert_eq!(s.max_zoom, MAX_ZOOM);
        let a = all(&[s]);
        assert_eq!(a.len(), 3);
        assert_eq!(a[0].name, "Own");
    }
}
