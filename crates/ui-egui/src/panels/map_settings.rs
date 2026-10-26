//! Settings ▸ Map: where map tiles come from (OpenStreetMap, offline, or a custom tile server),
//! the tile cache (its size and Clear Cache) and what viewing a map tells the tile server.
//!
//! Every change goes through the commands an agent uses (`map.tileSource`, `map.cacheInfo`,
//! `map.clearCache`), so the rules are the same: a custom template is checked by
//! `map.tileSource` and a refused one changes nothing. A custom server's fields are a draft
//! ([`SourceDraft`]) until **Apply** (or Return in a field): the map keeps its current source while
//! the URL is being typed, so no half-typed address is ever asked for tiles.

use egui::RichText;
use lightcraft_map::source::{OSM_ID, OSM_TILE_URL};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::panels::settings::{choices, heading, hint, row};
use crate::text_field::TextField;
use crate::theme::Tokens;
use crate::widgets::register;

/// Where docs/map.md lives (the privacy line's link).
pub const DOCS: &str = "https://github.com/storytold/lightcraft/blob/main/docs/map.md";

/// The tile source kinds Settings ▸ Map offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    OpenStreetMap,
    Offline,
    Custom,
}

/// The custom server being typed (applied only on confirm), and the last refusal.
#[derive(Clone, Debug, Default)]
pub struct SourceDraft {
    /// Custom chosen but not applied yet (the map keeps the source it has).
    pub custom: bool,
    pub url: String,
    pub attribution: String,
    pub max_zoom: String,
    /// Why the last Apply was refused.
    pub error: Option<String>,
}

/// The kind of source in use.
pub fn kind_of(app: &LightcraftApp) -> SourceKind {
    let s = app.session.map.source();
    if s.is_offline() {
        SourceKind::Offline
    } else if s.id == OSM_ID && s.url_template == OSM_TILE_URL {
        SourceKind::OpenStreetMap
    } else {
        SourceKind::Custom
    }
}

/// Fill the draft from the source in use when it is a custom one (else leave it empty).
fn start_draft(app: &mut LightcraftApp) {
    let s = app.session.map.source().clone();
    let custom = kind_of(app) == SourceKind::Custom;
    app.map.source_draft = SourceDraft {
        custom: true,
        url: if custom { s.url_template.clone() } else { String::new() },
        attribution: if custom { s.attribution.clone() } else { String::new() },
        max_zoom: if custom { s.max_zoom.to_string() } else { String::new() },
        error: None,
    };
}

/// Apply the draft through `map.tileSource` (the only way it reaches the map). A refusal is kept
/// in the draft and changes nothing.
pub fn apply_draft(app: &mut LightcraftApp) -> Result<Value, String> {
    let d = app.map.source_draft.clone();
    let mut p = json!({"urlTemplate": d.url.trim(), "attribution": d.attribution.trim()});
    let zoom = d.max_zoom.trim();
    if !zoom.is_empty() {
        match zoom.parse::<u8>() {
            Ok(z) => p["maxZoom"] = json!(z),
            Err(_) => {
                let e = "Max zoom is a whole number from 0 to 24".to_string();
                app.map.source_draft.error = Some(e.clone());
                return Err(e);
            }
        }
    }
    match app.run("map.tileSource", p) {
        Ok(v) => {
            app.map.source_draft = SourceDraft::default();
            Ok(v)
        }
        Err(e) => {
            app.map.source_draft.error = Some(e.clone());
            Err(e)
        }
    }
}

/// The tab.
pub fn tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    heading(ui, t, "Tile source");
    let in_use = kind_of(app);
    let mut choice = if app.map.source_draft.custom { SourceKind::Custom } else { in_use };
    let picked = row(ui, t, "Map tiles", |ui| {
        choices(
            ui,
            "settingsMapSource",
            &[(SourceKind::OpenStreetMap, "OpenStreetMap"), (SourceKind::Offline, "Offline (no tiles)"), (SourceKind::Custom, "Custom server")],
            &mut choice,
        )
    });
    if picked {
        let r = match choice {
            SourceKind::OpenStreetMap => app.run("map.tileSource", json!({"id": OSM_ID})).map(|_| ()),
            SourceKind::Offline => app.run("map.tileSource", json!({"offline": true})).map(|_| ()),
            SourceKind::Custom => {
                start_draft(app);
                Ok(())
            }
        };
        if choice != SourceKind::Custom {
            app.map.source_draft = SourceDraft::default();
        }
        if let Err(e) = r {
            app.toast(ui.ctx(), e);
        }
    }
    let src = app.session.map.source().clone();
    let status = if src.is_offline() {
        crate::i18n::tr("In use: none. The map is grey and nothing is fetched.").to_string()
    } else {
        crate::i18n::tr_format!("In use: {url} (up to zoom {zoom})", url = src.url_template, zoom = src.max_zoom)
    };
    hint(ui, t, &status);
    if choice == SourceKind::Custom {
        custom_fields(app, ui, t);
    }

    heading(ui, t, "Tile cache");
    cache_rows(app, ui, t);

    heading(ui, t, "Privacy");
    hint(
        ui,
        t,
        "Showing a map asks the tile server for the tiles of the area on screen, so the server sees which places you look at, with your IP address and LightCraft's name and version. Nothing is sent while no map is on screen, or when tiles are off.",
    );
    let r = ui.link(crate::i18n::tr("About the map and its tiles (docs/map.md)"));
    register(ui, "link:settingsMapDocs", r.rect);
    if r.clicked()
        && let Err(e) = crate::links::open(app, DOCS)
    {
        app.toast(ui.ctx(), e);
    }
}

/// The custom server's fields, with Apply: nothing reaches the map until it is confirmed.
fn custom_fields(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let mut d = std::mem::take(&mut app.map.source_draft);
    d.custom = true;
    let mut confirmed = false;
    for (widget, label, hint_text, text, width) in [
        ("field:settingsMapUrl", "URL template", "https://tiles.example.org/{z}/{x}/{y}.png", &mut d.url, 340.0),
        ("field:settingsMapAttribution", "Attribution", "© Tile provider", &mut d.attribution, 340.0),
        ("field:settingsMapMaxZoom", "Max zoom", "19", &mut d.max_zoom, 60.0),
    ] {
        row(ui, t, label, |ui| {
            let r = TextField::singleline(widget, text).hint(hint_text).width(width).show(ui);
            confirmed |= r.ending == Some(crate::text_field::Ending::Return);
        });
    }
    app.map.source_draft = d;
    let mut apply = false;
    row(ui, t, "", |ui| {
        let r = ui.button(crate::i18n::tr("Apply"));
        register(ui, "button:settingsMapApply", r.rect);
        apply = r.clicked();
    });
    if (apply || confirmed)
        && let Ok(v) = apply_draft(app)
    {
        let id = v["id"].as_str().unwrap_or_default().to_string();
        app.toast(ui.ctx(), crate::i18n::tr_format!("Map tiles from {id}", id = id));
    }
    if let Some(e) = &app.map.source_draft.error {
        ui.label(RichText::new(e).size(11.0).color(t.reject));
    }
    hint(
        ui,
        t,
        "An https:// address with {z}, {x} and {y}; the attribution the server asks for is shown on the map. Use a server whose terms allow it.",
    );
}

/// The cache's size (refreshed every 2 s: counting a large folder each frame would be slow) and
/// Clear Cache.
fn cache_rows(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let key = egui::Id::unique("settings-map-cache");
    let now = ui.input(|i| i.time);
    let info: Value = match ui.data(|d| d.get_temp::<(f64, Value)>(key)) {
        Some((at, v)) if now - at < 2.0 => v,
        _ => {
            let v = app.run("map.cacheInfo", json!({})).unwrap_or(Value::Null);
            ui.data_mut(|d| d.insert_temp(key, (now, v.clone())));
            v
        }
    };
    let mb = |b: u64| b as f64 / 1_048_576.0;
    let text = if info["dir"].is_null() {
        crate::i18n::tr("Memory only (no cache folder)").to_string()
    } else {
        crate::i18n::tr_format!(
            "{size} MB in {n} tiles (limit {cap} MB)",
            size = format!("{:.1}", mb(info["bytes"].as_u64().unwrap_or(0))),
            n = info["tiles"].as_u64().unwrap_or(0),
            cap = format!("{:.0}", mb(info["capBytes"].as_u64().unwrap_or(0)))
        )
    };
    let mut clear = false;
    row(ui, t, "In use", |ui| {
        ui.label(RichText::new(text).color(t.text));
        let r = ui.button(crate::i18n::tr("Clear Cache"));
        register(ui, "button:settingsMapClearCache", r.rect);
        clear = r.clicked();
    });
    if clear {
        ui.data_mut(|d| d.remove::<(f64, Value)>(key));
        match app.run("map.clearCache", json!({})) {
            Ok(v) => app.toast(ui.ctx(), crate::i18n::tr_format!("Deleted {n} map tiles", n = v["tiles"].as_u64().unwrap_or(0))),
            Err(e) => app.toast(ui.ctx(), e),
        }
    }
    hint(ui, t, "Tiles are kept for a week at least (as the tile server allows) so a place you looked at before opens without asking again.");
}
