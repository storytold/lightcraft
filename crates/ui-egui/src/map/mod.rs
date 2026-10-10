//! The Map module (Classic): a slippy map of the photos in view with pins and clusters,
//! drag-to-geotag (from the filmstrip or grid, and by moving pins), track logs with Auto-Tag,
//! saved locations (private ones keep their photos' positions out of exports), a location
//! filter, place search and opt-in reverse geocoding.
//!
//! The library side is `dac_engine::cmd::map` (`map.pins`, `map.geotag`, `map.saveLocation`, …);
//! tiles, clustering, tracks and geocoders are `dac_geo`. This module holds the view state
//! (centre, zoom, style, tracks), persisted in `<settings>/map.json`, and the UI commands that
//! drive it (`map.view`, `map.style`, `map.addServer`, `map.loadTrack`, `map.geotagAt`, …).

mod side;
#[cfg(test)]
mod tests;
pub mod tiles;
mod view;

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use dac_engine::cmd::map::LocationFilter;
use dac_geo::LatLon;
use dac_geo::tiles::{TileKind, TileServer};
use dac_geo::tracks::Track;

use crate::DacApp;
use crate::module::{Module, ModuleId, ModuleKey, PanelId};

/// What `map.json` keeps.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MapPrefs {
    pub lat: f64,
    pub lon: f64,
    pub zoom: f64,
    /// Tile server id.
    pub style: String,
    pub servers: Vec<TileServer>,
    /// Reverse geocoding: "off" | "offline" | "online".
    pub geocoder: String,
    pub endpoint: String,
    /// The user allowed online lookups (search and reverse geocoding) — off by default.
    pub online_consent: bool,
    /// Tile cache limit (MB).
    pub cache_mb: u64,
}

impl Default for MapPrefs {
    fn default() -> Self {
        MapPrefs {
            lat: 30.0,
            lon: 0.0,
            zoom: 2.0,
            style: "osm".into(),
            servers: Vec::new(),
            geocoder: "off".into(),
            endpoint: dac_geo::geocode::NOMINATIM_DEFAULT.into(),
            online_consent: false,
            cache_mb: dac_geo::cache::DEFAULT_MAX_BYTES / (1024 * 1024),
        }
    }
}

/// A loaded track log.
pub struct LoadedTrack {
    pub path: String,
    pub track: Track,
    pub shown: bool,
}

/// Background network work the map started (search, geocoding, dataset download).
pub enum Job {
    Search(Receiver<Result<Vec<(String, LatLon)>, String>>),
    Geocode(Receiver<Result<Value, String>>),
    Download(Receiver<Result<usize, String>>),
}

/// The Map module's state (not in `ui.json`: `map.json` beside it).
pub struct MapUi {
    pub prefs: MapPrefs,
    /// Where `prefs` are saved (`None`: tests and headless hosts, which never touch the user's files).
    pub prefs_path: Option<PathBuf>,
    started: bool,
    pub tiles: tiles::TileLoader,
    pub filter: LocationFilter,
    pub tracks: Vec<LoadedTrack>,
    /// Camera clock's offset for Auto-Tag (`+02:00`).
    pub offset: String,
    pub search: String,
    pub results: Vec<(String, LatLon)>,
    pub jobs: Vec<Job>,
    /// The new-location form.
    pub new_name: String,
    pub new_radius: f64,
    pub new_private: bool,
    /// The add-server form.
    pub new_server: (String, String, String),
    /// A pin being dragged: its photos.
    pub dragging_pin: Option<Vec<u64>>,
    /// Pins as drawn last frame: (screen centre, photo ids) — for `map.inspect`-style queries and tests.
    pub drawn: Vec<(egui::Pos2, Vec<u64>)>,
    /// The map canvas (screen), last frame.
    pub canvas: Option<egui::Rect>,
    pins_cache: Option<(u64, usize, String, i64, std::sync::Arc<Vec<dac_geo::Cluster<u64>>>)>,
}

impl Default for MapUi {
    fn default() -> Self {
        MapUi {
            prefs: MapPrefs::default(),
            prefs_path: None,
            started: false,
            tiles: tiles::TileLoader::default(),
            filter: LocationFilter::All,
            tracks: Vec::new(),
            offset: String::new(),
            search: String::new(),
            results: Vec::new(),
            jobs: Vec::new(),
            new_name: String::new(),
            new_radius: 200.0,
            new_private: false,
            new_server: Default::default(),
            dragging_pin: None,
            drawn: Vec::new(),
            canvas: None,
            pins_cache: None,
        }
    }
}

/// `<settings>/` (never in tests).
fn settings_dir() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        dac_engine::config::config_dir()
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

impl MapUi {
    /// First use: read `map.json`, start the tile fetcher with its disk cache.
    pub fn start(&mut self, headless: bool) {
        if self.started {
            return;
        }
        self.started = true;
        let dir = settings_dir();
        if !headless {
            self.prefs_path = dir.as_ref().map(|d| d.join("map.json"));
            if let Some(p) = &self.prefs_path
                && let Ok(text) = std::fs::read_to_string(p)
            {
                match serde_json::from_str::<MapPrefs>(&text) {
                    Ok(prefs) => self.prefs = prefs,
                    Err(e) => log::warn!("map.json: {e}; using defaults"),
                }
            }
        }
        self.sanitize();
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.tiles.start(dir.map(|d| d.join("tile-cache")));
        }
    }

    /// Keep hostile or stale prefs in range.
    fn sanitize(&mut self) {
        let p = &mut self.prefs;
        if !LatLon::new(p.lat, p.lon).is_valid() {
            (p.lat, p.lon) = (30.0, 0.0);
        }
        p.lat = p.lat.clamp(-dac_geo::mercator::MAX_LAT, dac_geo::mercator::MAX_LAT);
        if !p.zoom.is_finite() {
            p.zoom = 2.0;
        }
        p.zoom = p.zoom.clamp(1.0, f64::from(dac_geo::mercator::MAX_ZOOM));
        p.servers.retain(|s| s.validate().is_ok());
        if !self.servers().iter().any(|s| s.id == self.prefs.style) {
            self.prefs.style = "osm".into();
        }
    }

    pub fn save(&self) {
        let Some(p) = &self.prefs_path else { return };
        let Ok(text) = serde_json::to_string_pretty(&self.prefs) else { return };
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = p.with_extension("json.part");
        if std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, p)).is_err() {
            log::warn!("couldn't save {}", p.display());
        }
    }

    pub fn servers(&self) -> Vec<TileServer> {
        dac_geo::tiles::all(&self.prefs.servers)
    }

    pub fn server(&self) -> TileServer {
        let all = self.servers();
        let pick = all.iter().find(|s| s.id == self.prefs.style).cloned();
        pick.or_else(|| all.into_iter().next()).unwrap_or_else(|| TileServer {
            id: "osm".into(),
            name: "OpenStreetMap".into(),
            url: "https://tile.openstreetmap.org/{z}/{x}/{y}.png".into(),
            attribution: "© OpenStreetMap contributors".into(),
            max_zoom: 19,
            kind: TileKind::Road,
            subdomains: Vec::new(),
        })
    }

    pub fn centre(&self) -> LatLon {
        LatLon::new(self.prefs.lat, self.prefs.lon)
    }

    pub fn set_view(&mut self, c: LatLon, zoom: f64) {
        if c.is_valid() {
            self.prefs.lat = c.lat.clamp(-dac_geo::mercator::MAX_LAT, dac_geo::mercator::MAX_LAT);
            self.prefs.lon = (c.lon + 180.0).rem_euclid(360.0) - 180.0;
        }
        if zoom.is_finite() {
            self.prefs.zoom = zoom.clamp(1.0, f64::from(self.server().max_zoom.max(1)));
        }
    }

    /// The pins for the photos in view at the current zoom (cached per catalog revision, filter and zoom step;
    /// shared, so a frame doesn't copy 50k clusters — P6.1).
    pub fn pins(&mut self, app_session: &mut dac_engine::Session) -> std::sync::Arc<Vec<dac_geo::Cluster<u64>>> {
        let rev = app_session.catalog.revision;
        let n = app_session.visible().len();
        let zkey = (self.prefs.zoom * 4.0).round() as i64;
        let fkey = self.filter.key();
        if let Some((r, vn, f, z, pins)) = &self.pins_cache
            && *r == rev
            && *vn == n
            && *f == fkey
            && *z == zkey
        {
            return std::sync::Arc::clone(pins);
        }
        let pts: Vec<(u64, LatLon)> =
            dac_engine::cmd::map::filtered(app_session, &self.filter).into_iter().filter_map(|(id, g)| g.map(|g| (id.0, g))).collect();
        let pins = std::sync::Arc::new(dac_geo::cluster(&pts, zkey as f64 / 4.0, 44.0));
        self.pins_cache = Some((rev, n, fkey, zkey, std::sync::Arc::clone(&pins)));
        pins
    }
}

// ---------- the Module

pub struct MapModule;
pub static MAP: MapModule = MapModule;

impl Module for MapModule {
    fn id(&self) -> ModuleId {
        ModuleId::Map
    }
    fn left_panels(&self) -> &'static [PanelId] {
        &[]
    }
    fn right_panels(&self) -> &'static [PanelId] {
        &[]
    }
    fn toolbar(&self, _ui: &mut egui::Ui, _app: &mut DacApp) {}
    fn center(&self, ui: &mut egui::Ui, app: &mut DacApp) {
        view::center(ui, app);
    }
    fn keymap(&self) -> &'static [ModuleKey] {
        &[crate::help_overlay::KEY]
    }
    fn command_prefixes(&self) -> &'static [&'static str] {
        &["map."]
    }
}

// ---------- UI commands

/// The Map module's UI commands: `(id, label, shortcut, menu)`; dispatched from `module::run`.
pub const COMMANDS: &[crate::menus::UiCommand] = &[
    ("map.view", "Map View", None, ""),
    ("map.style", "Map Style", None, ""),
    ("map.addServer", "Add Tile Server", None, ""),
    ("map.removeServer", "Remove Tile Server", None, ""),
    ("map.filter", "Location Filter", None, ""),
    ("map.loadTrack", "Load Tracklog", None, ""),
    ("map.clearTracks", "Clear Tracklogs", None, ""),
    ("map.autoTag", "Auto-Tag Selected Photos", None, ""),
    ("map.geotagAt", "Geotag at Map Point", None, ""),
    ("map.fit", "Show All Pins", None, ""),
    ("map.search", "Find Place", None, ""),
    ("map.geocoder", "Reverse Geocoding Settings", None, ""),
    ("map.lookup", "Look Up Locations", None, ""),
    ("map.tileCache", "Tile Cache", None, ""),
];

fn f(p: &Value, k: &str) -> Option<f64> {
    p.get(k).and_then(Value::as_f64)
}

fn s<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Value::as_str)
}

fn view_json(app: &DacApp) -> Value {
    let m = &app.map;
    json!({
        "lat": m.prefs.lat, "lon": m.prefs.lon, "zoom": m.prefs.zoom, "style": m.prefs.style,
        "filter": m.filter.key(), "tracks": m.tracks.iter().map(|t| json!({"name": t.track.name, "path": t.path, "points": t.track.points(), "shown": t.shown})).collect::<Vec<_>>(),
        "pins": m.drawn.iter().map(|(p, ids)| json!({"x": p.x, "y": p.y, "ids": ids})).collect::<Vec<_>>(),
        "tiles": {"loaded": m.tiles.loaded, "failed": m.tiles.failed, "error": m.tiles.last_error, "fetcher": m.tiles.has_fetcher()},
        "attribution": m.server().attribution,
    })
}

/// Run a map UI command; `None`: not one.
pub fn run(app: &mut DacApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if !COMMANDS.iter().any(|c| c.0 == id) {
        return None;
    }
    app.map.start(app.headless_host);
    Some(run_inner(app, id, p))
}

fn run_inner(app: &mut DacApp, id: &str, p: &Value) -> Result<Value, String> {
    match id {
        "map.view" => {
            let c = app.map.centre();
            let lat = f(p, "lat").unwrap_or(c.lat);
            let lon = f(p, "lon").unwrap_or(c.lon);
            if !LatLon::new(lat, lon).is_valid() {
                return Err(format!("{lat}, {lon} is not a position"));
            }
            let zoom = f(p, "zoom").unwrap_or(app.map.prefs.zoom);
            app.map.set_view(LatLon::new(lat, lon), zoom);
            if p.as_object().is_some_and(|o| !o.is_empty()) {
                app.map.save();
            }
            Ok(view_json(app))
        }
        "map.style" => {
            let Some(want) = s(p, "id") else {
                return Ok(json!({"style": app.map.prefs.style, "servers": app.map.servers()}));
            };
            if !app.map.servers().iter().any(|x| x.id == want) {
                return Err(format!("no tile server `{want}`"));
            }
            app.map.prefs.style = want.to_string();
            app.map.tiles.clear();
            let z = app.map.prefs.zoom;
            app.map.set_view(app.map.centre(), z);
            app.map.save();
            Ok(json!({"style": want}))
        }
        "map.addServer" => {
            let name = s(p, "name").ok_or("missing `name`")?.trim();
            let url = s(p, "url").ok_or("missing `url` (https://…/{z}/{x}/{y}.png)")?.trim();
            let attribution = s(p, "attribution").ok_or("missing `attribution` (shown on the map)")?;
            let id: String = s(p, "id").map(str::to_string).unwrap_or_else(|| {
                let slug: String = name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
                let slug = slug.trim_matches('-').chars().take(40).collect::<String>();
                if slug.is_empty() { "custom".into() } else { slug }
            });
            let kind = match s(p, "kind").unwrap_or("road") {
                "terrain" => TileKind::Terrain,
                "satellite" => TileKind::Satellite,
                _ => TileKind::Road,
            };
            let max = f(p, "maxZoom").unwrap_or(19.0).clamp(0.0, 20.0) as u8;
            let mut server = TileServer::custom(&id, name, url, attribution, max, kind).map_err(|e| e.to_string())?;
            if let Some(subs) = p.get("subdomains").and_then(Value::as_array) {
                server.subdomains = subs.iter().filter_map(Value::as_str).map(str::to_string).take(16).collect();
                server.validate().map_err(|e| e.to_string())?;
            }
            app.map.prefs.servers.retain(|x| x.id != server.id);
            app.map.prefs.servers.push(server.clone());
            if p.get("use").and_then(Value::as_bool).unwrap_or(true) {
                app.map.prefs.style = server.id.clone();
                app.map.tiles.clear();
            }
            app.map.save();
            Ok(json!({"server": server}))
        }
        "map.removeServer" => {
            let want = s(p, "id").ok_or("missing `id`")?;
            let before = app.map.prefs.servers.len();
            app.map.prefs.servers.retain(|x| x.id != want);
            if app.map.prefs.servers.len() == before {
                return Err(format!("`{want}` is not a server you added"));
            }
            if app.map.prefs.style == want {
                app.map.prefs.style = "osm".into();
                app.map.tiles.clear();
            }
            app.map.save();
            Ok(json!({"removed": want}))
        }
        "map.filter" => {
            let want = s(p, "filter").unwrap_or("all");
            let filter = LocationFilter::parse(want).ok_or("filter is all|tagged|untagged|location:<name>")?;
            if let LocationFilter::Location(n) = &filter
                && app.session.catalog.saved_location(n).is_none()
            {
                return Err(format!("no saved location `{n}`"));
            }
            app.map.filter = filter;
            Ok(json!({"filter": app.map.filter.key()}))
        }
        "map.loadTrack" => {
            let path = match s(p, "path") {
                Some(x) => x.to_string(),
                None => {
                    let req = crate::pick::PickRequest::file(
                        crate::i18n::tr("Load Tracklog"),
                        crate::i18n::tr("Track Log"),
                        &["gpx", "kml", "geojson", "json"],
                    );
                    match crate::pick::ask(app, id, p, "path", req, |s| s.pick_tracklog.as_mut().map(|f| f())) {
                        crate::pick::Picked::Now(v) => match v.into_iter().next() {
                            Some(x) => x,
                            None => return Ok(Value::Null),
                        },
                        crate::pick::Picked::Later => return Ok(json!({"pending": true})),
                        crate::pick::Picked::Unavailable => return Err("no file dialog here: pass `path`".into()),
                    }
                }
            };
            let meta = std::fs::metadata(&path).map_err(|e| format!("can't read {path}: {e}"))?;
            if meta.len() > 512 * 1024 * 1024 {
                return Err("the track file is larger than 512 MB".into());
            }
            let text = std::fs::read_to_string(&path).map_err(|e| format!("can't read {path}: {e}"))?;
            let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let track = dac_geo::tracks::parse(&name, &text).map_err(|e| e.to_string())?;
            let pts: Vec<LatLon> = track.all_points().collect();
            let fit = app.map.canvas.map(|r| (f64::from(r.width()), f64::from(r.height()))).unwrap_or((1000.0, 700.0));
            if let Some((c, z)) = dac_geo::mercator::fit(&pts, fit.0, fit.1) {
                app.map.set_view(c, z);
            }
            let out = json!({"name": track.name, "points": track.points(), "segments": track.segments.len(), "timed": track.timed});
            app.map.tracks.retain(|t| t.path != path);
            app.map.tracks.push(LoadedTrack { path, track, shown: true });
            Ok(out)
        }
        "map.clearTracks" => {
            let n = app.map.tracks.len();
            app.map.tracks.clear();
            Ok(json!({"cleared": n}))
        }
        "map.autoTag" => {
            let track = app.map.tracks.iter().rev().find(|t| t.track.timed && t.shown).ok_or("load a GPX track log with times first")?;
            let path = track.path.clone();
            let offset = s(p, "offset").map(str::to_string).unwrap_or_else(|| app.map.offset.clone());
            let mut params = json!({"path": path, "offset": offset});
            if let (Some(o), Some(ids)) = (params.as_object_mut(), p.get("ids")) {
                o.insert("ids".into(), ids.clone());
            }
            app.run("photo.autoTagTracklog", params)
        }
        "map.geotagAt" => {
            // a screen point of the map (control channel / tests): what a drop there does
            let (x, y) = (f(p, "x").ok_or("missing `x`")?, f(p, "y").ok_or("missing `y`")?);
            let r = app.map.canvas.ok_or("the map isn't on screen")?;
            let at = view::screen_to_geo(&app.map, r, egui::pos2(x as f32, y as f32));
            let mut params = json!({"lat": at.lat, "lon": at.lon});
            if let (Some(o), Some(ids)) = (params.as_object_mut(), p.get("ids")) {
                o.insert("ids".into(), ids.clone());
            }
            app.run("map.geotag", params)
        }
        "map.fit" => {
            let pins = app.map.pins(&mut app.session);
            let pts: Vec<LatLon> = pins.iter().map(|c| c.centre).collect();
            let size = app.map.canvas.map(|r| (f64::from(r.width()), f64::from(r.height()))).unwrap_or((1000.0, 700.0));
            let (c, z) = dac_geo::mercator::fit(&pts, size.0, size.1).ok_or("no photos with a location in view")?;
            app.map.set_view(c, z);
            Ok(view_json(app))
        }
        "map.search" => {
            let q = s(p, "query").ok_or("missing `query`")?.to_string();
            app.map.search = q.clone();
            side::search(app, &q)
        }
        "map.geocoder" => {
            if let Some(g) = s(p, "provider") {
                if !matches!(g, "off" | "offline" | "online") {
                    return Err("provider is off|offline|online".into());
                }
                app.map.prefs.geocoder = g.to_string();
            }
            if let Some(e) = s(p, "endpoint") {
                if !(e.starts_with("https://") || e.starts_with("http://")) {
                    return Err("the endpoint starts with https://".into());
                }
                app.map.prefs.endpoint = e.to_string();
            }
            if let Some(c) = p.get("consent").and_then(Value::as_bool) {
                app.map.prefs.online_consent = c;
            }
            app.map.save();
            Ok(json!({"provider": app.map.prefs.geocoder, "endpoint": app.map.prefs.endpoint, "consent": app.map.prefs.online_consent}))
        }
        "map.lookup" => side::lookup(app, p),
        "map.tileCache" => {
            if p.get("clear").and_then(Value::as_bool).unwrap_or(false) {
                app.map.tiles.clear_cache()?;
            }
            let (bytes, files) = app.map.tiles.cache_usage().unwrap_or((0, 0));
            Ok(json!({"bytes": bytes, "files": files, "limitMb": app.map.prefs.cache_mb}))
        }
        _ => Err(format!("unknown map command {id}")),
    }
}
