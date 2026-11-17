use super::widgets::{field, heading, hint, row};
use super::{LightcraftApp, RichText, Tokens, json, register};

// ----------------------------------------------------------------------------------------- Map

/// Where users get an ArcGIS Location Platform API key (free tier).
const ARCGIS_SIGN_UP: &str = "https://location.arcgis.com";

/// The project's map documentation (sources, the OSM policy, Esri, privacy).
const MAP_DOCS: &str = "https://github.com/storytold/lightcraft/blob/main/docs/map.md";

/// A measurement of the tile cache (bytes, tiles) running on a worker thread: it lands here.
type CacheJob = std::sync::Arc<std::sync::Mutex<Option<(u64, u64)>>>;

/// What Settings ▸ Map remembers between frames (egui temporary memory: never saved): the custom
/// server's fields as typed, the last refusal, and the cache size with when it was measured.
#[derive(Clone, Default)]
struct MapDraft {
    template: String,
    attribution: String,
    max_zoom: String,
    /// The fields were filled from the current custom source (once, when the tab opened).
    seeded: bool,
    error: Option<String>,
    /// The last measurement that finished: bytes, tiles, when.
    cache: Option<(u64, u64, f64)>,
    /// A measurement under way (the last one stays shown meanwhile).
    measuring: Option<CacheJob>,
}

/// Measure the tile cache under `root` on a worker thread (it walks the cache folder, up to a
/// million files): the result lands in the job. Where no thread can be started (the web) it is
/// measured at once.
fn measure_cache(root: Option<std::path::PathBuf>) -> CacheJob {
    let job = CacheJob::default();
    let Some(root) = root else {
        *job.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((0, 0));
        return job;
    };
    let measure = |root: &std::path::Path| {
        let info = std::panic::catch_unwind(|| lightcraft_engine::map::cache::info(root)).unwrap_or_default();
        (info.bytes, info.tiles)
    };
    let out = job.clone();
    let worker_root = root.clone();
    let started = std::thread::Builder::new().name("map-cache-size".into()).spawn(move || {
        let r = measure(&worker_root);
        *out.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(r);
    });
    if started.is_err() {
        *job.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(measure(&root));
    }
    job
}

/// Settings ▸ Map: where tiles come from (the built-in sources, Offline, or a custom server applied
/// with Apply or Return, never while typing; a refused one shows why and changes nothing), the tile
/// cache's size with Clear Cache, what the tile server sees, then the Esri key. (After PR #921's
/// map settings, Jason Baldwin.)
pub(super) fn map_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let id = egui::Id::unique("settings:mapDraft");
    let mut d: MapDraft = ui.data_mut(|m| m.get_temp::<MapDraft>(id)).unwrap_or_default();
    if !d.seeded {
        if let lightcraft_engine::map::TileSource::Custom { url_template, attribution, max_zoom, .. } = app.session.map.source() {
            d.template = url_template.clone();
            d.attribution = attribution.clone();
            d.max_zoom = max_zoom.to_string();
        }
        d.seeded = true;
    }
    heading(ui, t, crate::i18n::tr("Map tiles"));
    let current = app.session.map.effective_source();
    let custom_now = matches!(current, lightcraft_engine::map::TileSource::Custom { .. });
    let mut chosen: Option<String> = None;
    row(ui, t, crate::i18n::tr("Tile source"), |ui| {
        ui.horizontal_wrapped(|ui| {
            for s in app.session.map.offered() {
                let r = ui.radio(s == current, crate::i18n::tr(s.label()));
                register(ui, format!("settings:mapSource:{}", s.id()), r.rect);
                if r.clicked() && s != current {
                    chosen = Some(s.id().to_string());
                }
            }
            let r = ui.radio(custom_now, crate::i18n::tr("Custom tile server"));
            register(ui, "settings:mapSource:custom", r.rect);
        });
    });
    if let Some(s) = chosen {
        match app.run("map.tileSource", json!({"id": s})) {
            Ok(_) => d.error = None,
            Err(e) => d.error = Some(e),
        }
    }
    // the custom server: applied on Apply or Return only
    let mut apply = false;
    row(ui, t, crate::i18n::tr("URL template"), |ui| {
        let r =
            field(ui, crate::text_field::TextField::singleline("field:mapTemplate", &mut d.template).hint("https://…/{z}/{x}/{y}.png").width(320.0));
        apply |= matches!(r.ending, Some(crate::text_field::Ending::Return));
    });
    row(ui, t, crate::i18n::tr("Attribution"), |ui| {
        let r = field(
            ui,
            crate::text_field::TextField::singleline("field:mapAttribution", &mut d.attribution).hint(crate::i18n::tr("© the provider")).width(320.0),
        );
        apply |= matches!(r.ending, Some(crate::text_field::Ending::Return));
    });
    row(ui, t, crate::i18n::tr("Max zoom"), |ui| {
        let r = field(ui, crate::text_field::TextField::singleline("field:mapMaxZoom", &mut d.max_zoom).hint("18").width(60.0));
        apply |= matches!(r.ending, Some(crate::text_field::Ending::Return));
        let b = ui.add_enabled(!d.template.trim().is_empty(), egui::Button::new(crate::i18n::tr("Apply")));
        register(ui, "button:mapCustomApply", b.rect);
        apply |= b.clicked();
    });
    if apply && !d.template.trim().is_empty() {
        let zoom = d.max_zoom.trim();
        let mut p = json!({"urlTemplate": d.template.trim(), "attribution": d.attribution.trim()});
        let bad_zoom = !zoom.is_empty() && zoom.parse::<u8>().is_err();
        if let Ok(z) = zoom.parse::<u8>() {
            p["maxZoom"] = json!(z);
        }
        d.error =
            if bad_zoom { Some(crate::i18n::tr("Max zoom is a whole number from 0 to 24").to_string()) } else { app.run("map.tileSource", p).err() };
    }
    if let Some(e) = &d.error {
        let r = ui.label(RichText::new(e).size(11.5).color(t.caution));
        register(ui, "settings:mapError", r.rect);
    }
    hint(
        ui,
        t,
        crate::i18n::tr(
            "A tile server sees which tiles you look at (so roughly which places), your IP address and LightCraft's name and version; nothing else leaves the machine. Offline fetches nothing. A custom server's address may carry your own key, on your own responsibility.",
        ),
    );
    // the cache, measured at most every 5 seconds on a worker thread (it walks the cache folder);
    // the last measurement stays shown until the next one lands
    let now = app.tick_now();
    if let Some(job) = &d.measuring {
        let landed = job.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        match landed {
            Some((bytes, tiles)) => {
                d.cache = Some((bytes, tiles, now));
                d.measuring = None;
            }
            None => ui.ctx().request_repaint_after(std::time::Duration::from_millis(100)),
        }
    }
    if d.measuring.is_none() && d.cache.is_none_or(|(_, _, at)| now - at > 5.0) {
        d.measuring = Some(measure_cache(app.session.map.cache_root.clone()));
        ui.ctx().request_repaint();
    }
    let (bytes, tiles, _) = d.cache.unwrap_or_default();
    row(ui, t, crate::i18n::tr("Tile cache"), |ui| {
        let text = if d.cache.is_some() {
            crate::i18n::tr_format!("{size} MB, {n} tiles", size = format!("{:.1}", bytes as f64 / 1_048_576.0), n = tiles)
        } else {
            crate::i18n::tr("Measuring…").to_string()
        };
        let r = ui.label(text);
        register(ui, "settings:mapCacheSize", r.rect);
        let b = ui.add_enabled(tiles > 0, egui::Button::new(crate::i18n::tr("Clear Cache")));
        register(ui, "button:mapClearCache", b.rect);
        if b.clicked() {
            if let Err(e) = app.run("map.clearCache", json!({})) {
                app.toast(ui.ctx(), e);
            }
            // measured again (a measurement from before the clear is dropped)
            d.cache = None;
            d.measuring = None;
        }
    });
    row(ui, t, "", |ui| {
        let r = ui.link(crate::i18n::tr("About map tiles and privacy (docs/map.md)"));
        register(ui, "link:mapDocs", r.rect);
        if r.clicked()
            && let Err(e) = crate::links::open(app, MAP_DOCS)
        {
            app.toast(ui.ctx(), e);
        }
    });
    ui.data_mut(|m| m.insert_temp(id, d));
    ui.add_space(8.0);
    esri_key(app, ui, t);
}

/// The ArcGIS API key the Esri layers need. The key typed here is applied with Apply or Return
/// (never while typing), then the field empties: the key is never shown again, only whether one is
/// set. It is kept outside `ui.json` and the library (`map.setEsriKey`).
fn esri_key(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    heading(ui, t, crate::i18n::tr("Esri map layers"));
    hint(
        ui,
        t,
        crate::i18n::tr(
            "The Esri Streets and Esri Outdoor map styles need your own ArcGIS Location Platform API key. A free account gives a monthly tile allowance; tiles count against your key. Esri's server then sees which places you look at.",
        ),
    );
    let set = app.session.map.esri_key_set();
    row(ui, t, crate::i18n::tr("Status"), |ui| {
        let text = if set { crate::i18n::tr("A key is set") } else { crate::i18n::tr("No key: the Esri styles are not offered") };
        let r = ui.label(RichText::new(text).color(if set { t.text } else { t.text_dim }));
        register(ui, "settings:esriKeyStatus", r.rect);
    });
    // the draft lives in egui's temporary memory only (never saved with the window state)
    let draft_id = egui::Id::unique("settings:esriKeyDraft");
    let mut draft: String = ui.data_mut(|d| d.get_temp::<String>(draft_id)).unwrap_or_default();
    let mut apply = false;
    row(ui, t, crate::i18n::tr("ArcGIS API key"), |ui| {
        let shown = field(
            ui,
            crate::text_field::TextField::singleline("field:esriKey", &mut draft)
                .hint(crate::i18n::tr(if set { "Paste a new key to replace it" } else { "Paste your key" }))
                .width(260.0),
        );
        apply |= matches!(shown.ending, Some(crate::text_field::Ending::Return));
        let b = ui.add_enabled(!draft.trim().is_empty(), egui::Button::new(crate::i18n::tr("Apply")));
        register(ui, "button:esriKeyApply", b.rect);
        apply |= b.clicked();
        let c = ui.add_enabled(set, egui::Button::new(crate::i18n::tr("Remove Key")));
        register(ui, "button:esriKeyClear", c.rect);
        if c.clicked() {
            if let Err(e) = app.run("map.setEsriKey", json!({"key": ""})) {
                app.toast(ui.ctx(), e);
            }
            draft.clear();
        }
    });
    if apply && !draft.trim().is_empty() {
        match app.run("map.setEsriKey", json!({"key": draft.trim()})) {
            Ok(_) => draft.clear(),
            // a key of the wrong shape stays in the field to be corrected; nothing changed
            Err(e) => app.toast(ui.ctx(), e),
        }
    }
    ui.data_mut(|d| d.insert_temp(draft_id, draft));
    row(ui, t, "", |ui| {
        let r = ui.link(crate::i18n::tr("Get a free key at location.arcgis.com"));
        register(ui, "link:arcgisSignUp", r.rect);
        if r.clicked()
            && let Err(e) = crate::links::open(app, ARCGIS_SIGN_UP)
        {
            app.toast(ui.ctx(), e);
        }
    });
    hint(
        ui,
        t,
        crate::i18n::tr(
            "Give the key only the static basemap tiles privilege. It is kept in LightCraft's settings folder (map-keys.json), never in your library, and sent only to Esri's tile server. Choose the style in the Map view's Map Style list.",
        ),
    );
}
