//! The map's filter / search bar and its side panel (style, saved locations, tracklog, reverse
//! geocoding, tile cache); the background jobs they start.

use egui::{Rect, RichText};
use serde_json::{Value, json};

use dac_engine::cmd::map::LocationFilter;
use dac_geo::LatLon;

use super::Job;
use crate::DacApp;
use crate::i18n::tr;
use crate::theme::Tokens;
use crate::widgets::register;

/// Parse `48.85, 2.35` (decimal degrees).
fn parse_coords(q: &str) -> Option<LatLon> {
    let mut it = q.split([',', ' ', ';']).filter(|s| !s.is_empty()).map(|s| s.trim().parse::<f64>());
    let (Some(Ok(lat)), Some(Ok(lon)), None) = (it.next(), it.next(), it.next()) else { return None };
    let p = LatLon::new(lat, lon);
    p.is_valid().then_some(p)
}

/// `map.search`: coordinates, then a saved location, then (with consent) the online geocoder.
pub fn search(app: &mut DacApp, q: &str) -> Result<Value, String> {
    let q = q.trim();
    if q.is_empty() {
        app.map.results.clear();
        return Ok(json!({"results": []}));
    }
    if let Some(p) = parse_coords(q) {
        let z = app.map.prefs.zoom.max(12.0);
        app.map.set_view(p, z);
        return Ok(json!({"found": "coordinates", "lat": p.lat, "lon": p.lon}));
    }
    let found = app.session.catalog.saved_locations().find(|l| l.name.to_lowercase().contains(&q.to_lowercase())).cloned();
    if let Some(l) = found {
        let z = (17.0 - (l.radius / 100.0).max(1.0).log2()).clamp(3.0, 17.0);
        app.map.set_view(l.centre(), z);
        return Ok(json!({"found": "savedLocation", "name": l.name}));
    }
    if !app.map.prefs.online_consent {
        return Err(tr(
            "Place search needs online lookups: turn them on under Reverse Geocoding (they send the search text to the geocoding server)",
        )
        .to_string());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let endpoint = app.map.prefs.endpoint.clone();
        let query = q.to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new().name("map-search".into()).spawn(move || {
            let r = dac_geo::geocode::Nominatim::new(&endpoint)
                .and_then(|n| n.search(dac_geo::geocode::Consent::Granted, &query))
                .map_err(|e| e.to_string());
            let _ = tx.send(r);
        });
        spawned.map_err(|e| e.to_string())?;
        app.map.jobs.push(Job::Search(rx));
        Ok(json!({"pending": true}))
    }
    #[cfg(target_arch = "wasm32")]
    Err(tr("Place search isn't available in the web app").to_string())
}

/// `map.lookup {ids?}`: reverse-geocode the selected photos with the chosen provider (online in the
/// background, then applied as one undo step).
pub fn lookup(app: &mut DacApp, p: &Value) -> Result<Value, String> {
    let mut params = json!({"provider": app.map.prefs.geocoder, "overwrite": p.get("overwrite").and_then(Value::as_bool).unwrap_or(false)});
    if let (Some(o), Some(ids)) = (params.as_object_mut(), p.get("ids")) {
        o.insert("ids".into(), ids.clone());
    }
    match app.map.prefs.geocoder.as_str() {
        "offline" => app.run("map.reverseGeocode", params),
        "online" => {
            if !app.map.prefs.online_consent {
                return Err(tr("Allow online lookups first (Reverse Geocoding ▸ Online)").to_string());
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let ids: Vec<dac_catalog::PhotoId> = app.session.targets(&params);
                let todo: Vec<(u64, LatLon)> =
                    ids.iter().filter_map(|id| app.session.catalog.photo(*id)?.meta.gps.map(|(a, b)| (id.0, LatLon::new(a, b)))).take(500).collect();
                if todo.is_empty() {
                    return Err(tr("None of the selected photos has a location").to_string());
                }
                let endpoint = app.map.prefs.endpoint.clone();
                let (tx, rx) = std::sync::mpsc::channel();
                let spawned = std::thread::Builder::new().name("map-geocode".into()).spawn(move || {
                    let r = (|| -> Result<Value, String> {
                        let n = dac_geo::geocode::Nominatim::new(&endpoint).map_err(|e| e.to_string())?;
                        let mut places = Vec::new();
                        let mut seen: Vec<(LatLon, Option<dac_geo::geocode::Place>)> = Vec::new();
                        for (id, at) in todo {
                            let pl = match seen.iter().find(|(q, _)| dac_geo::distance_m(*q, at) < 50.0) {
                                Some((_, pl)) => pl.clone(),
                                None => {
                                    let pl = n.reverse(dac_geo::geocode::Consent::Granted, at).map_err(|e| e.to_string())?;
                                    seen.push((at, pl.clone()));
                                    pl
                                }
                            };
                            if let Some(pl) = pl {
                                places.push(
                                    json!({"id": id, "sublocation": pl.sublocation, "city": pl.city, "state": pl.state, "country": pl.country}),
                                );
                            }
                        }
                        Ok(json!({"provider": "given", "places": places}))
                    })();
                    let _ = tx.send(r);
                });
                spawned.map_err(|e| e.to_string())?;
                app.map.jobs.push(Job::Geocode(rx));
                Ok(json!({"pending": true}))
            }
            #[cfg(target_arch = "wasm32")]
            Err(tr("Online lookups aren't available in the web app").to_string())
        }
        _ => Err(tr("Reverse geocoding is off: choose Offline or Online first").to_string()),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn start_download(app: &mut DacApp) {
    let dir = dac_engine::config::config_dir().map(|d| d.join("geonames"));
    let Some(dir) = dir else { return };
    let (tx, rx) = std::sync::mpsc::channel();
    let ok = std::thread::Builder::new()
        .name("geonames".into())
        .spawn(move || {
            let r = dac_geo::geocode::download_geonames(dac_geo::geocode::Consent::Granted, dac_geo::geocode::GEONAMES_BASE, &dir);
            let _ = tx.send(r.map_err(|e| e.to_string()));
        })
        .is_ok();
    if ok {
        app.map.jobs.push(Job::Download(rx));
    }
}

/// Finish background jobs.
pub fn poll_jobs(app: &mut DacApp, ctx: &egui::Context) {
    use std::sync::mpsc::TryRecvError;
    let mut keep = Vec::new();
    for job in std::mem::take(&mut app.map.jobs) {
        match job {
            Job::Search(rx) => match rx.try_recv() {
                Ok(Ok(found)) => {
                    if let Some((_, p)) = found.first() {
                        let z = app.map.prefs.zoom.max(11.0);
                        app.map.set_view(*p, z);
                    }
                    if found.is_empty() {
                        app.toast(ctx, tr("No place found"));
                    }
                    app.map.results = found;
                }
                Ok(Err(e)) => app.toast_error(ctx, e),
                Err(TryRecvError::Empty) => keep.push(Job::Search(rx)),
                Err(TryRecvError::Disconnected) => {}
            },
            Job::Geocode(rx) => match rx.try_recv() {
                Ok(Ok(params)) => match app.run("map.reverseGeocode", params) {
                    Ok(v) => app.toast(ctx, format!("Filled the location of {n} photos", n = v["changed"].as_u64().unwrap_or(0))),
                    Err(e) => app.toast_error(ctx, e),
                },
                Ok(Err(e)) => app.toast_error(ctx, e),
                Err(TryRecvError::Empty) => keep.push(Job::Geocode(rx)),
                Err(TryRecvError::Disconnected) => {}
            },
            Job::Download(rx) => match rx.try_recv() {
                Ok(Ok(n)) => app.toast(ctx, format!("Downloaded {n} place names (GeoNames, CC-BY 4.0)", n = n)),
                Ok(Err(e)) => app.toast_error(ctx, e),
                Err(TryRecvError::Empty) => keep.push(Job::Download(rx)),
                Err(TryRecvError::Disconnected) => {}
            },
        }
    }
    if !keep.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }
    app.map.jobs = keep;
}

/// The bar above the map: location filter and search.
pub fn bar(app: &mut DacApp, ui: &mut egui::Ui, r: Rect) {
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_filled(r, 0.0, t.chrome);
    ui.painter().line_segment([r.left_bottom(), r.right_bottom()], egui::Stroke::new(1.0, t.divider));
    let mut child =
        ui.new_child(egui::UiBuilder::new().max_rect(r.shrink2(egui::vec2(10.0, 5.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    let ui = &mut child;
    ui.label(RichText::new(tr("Location Filter:")).color(t.text_dim));
    let current = app.map.filter.clone();
    for (key, label) in [("all", "Visible on Map"), ("tagged", "Tagged"), ("untagged", "Untagged")] {
        let on = current.key() == key;
        let b = ui.selectable_label(on, tr(label));
        register(ui.ctx(), format!("map:filter:{key}"), b.rect);
        if b.clicked() {
            let _ = app.run("map.filter", json!({"filter": key}));
        }
    }
    let names: Vec<String> = app.session.catalog.saved_locations().map(|l| l.name.clone()).collect();
    let shown = match &current {
        LocationFilter::Location(n) => n.clone(),
        _ => tr("Saved Location").to_string(),
    };
    egui::ComboBox::from_id_salt("map-filter-location").selected_text(shown).width(130.0).show_ui(ui, |ui| {
        for n in &names {
            if ui.selectable_label(current == LocationFilter::Location(n.clone()), n).clicked() {
                let _ = app.run("map.filter", json!({"filter": format!("location:{n}")}));
            }
        }
        if names.is_empty() {
            ui.label(RichText::new(tr("No saved locations")).weak());
        }
    });
    let counts = {
        let all = dac_engine::cmd::map::filtered(&mut app.session, &LocationFilter::All);
        let tagged = all.iter().filter(|(_, g)| g.is_some()).count();
        (tagged, all.len() - tagged)
    };
    ui.label(RichText::new(format!("{a} on map · {b} untagged", a = counts.0, b = counts.1)).color(t.text_dim).size(11.0));
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let edit = egui::TextEdit::singleline(&mut app.map.search).hint_text(tr("Search place or lat, lon")).desired_width(200.0);
        let resp = ui.add(edit);
        register(ui.ctx(), "map:search", resp.rect);
        if resp.has_focus() {
            app.text_focus = true;
        }
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let q = app.map.search.clone();
            if let Err(e) = search(app, &q) {
                app.toast_error(ui.ctx(), e);
            }
        }
    });
}

fn section(ui: &mut egui::Ui, title: &str, open: bool, body: impl FnOnce(&mut egui::Ui)) {
    egui::CollapsingHeader::new(RichText::new(tr(title)).strong()).id_salt(("map-side", title)).default_open(open).show(ui, body);
}

/// The side panel.
pub fn panel(app: &mut DacApp, ui: &mut egui::Ui, r: Rect) {
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_filled(r, 0.0, t.chrome);
    ui.painter().line_segment([r.left_top(), r.left_bottom()], egui::Stroke::new(1.0, t.divider));
    register(ui.ctx(), "map:side", r);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink(10.0)).layout(egui::Layout::top_down(egui::Align::Min)));
    egui::ScrollArea::vertical().id_salt("map-side-scroll").auto_shrink([false, false]).show(&mut child, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        style_section(app, ui);
        locations_section(app, ui);
        track_section(app, ui);
        geocode_section(app, ui);
        let results = app.map.results.clone();
        if !results.is_empty() {
            section(ui, "Search Results", true, |ui| {
                for (name, p) in results {
                    if ui.link(&name).clicked() {
                        let z = app.map.prefs.zoom.max(11.0);
                        app.map.set_view(p, z);
                    }
                }
            });
        }
    });
}

fn style_section(app: &mut DacApp, ui: &mut egui::Ui) {
    section(ui, "Map Style", true, |ui| {
        let servers = app.map.servers();
        let cur = app.map.server();
        egui::ComboBox::from_id_salt("map-style").selected_text(cur.name.clone()).width(240.0).show_ui(ui, |ui| {
            for s in &servers {
                if ui.selectable_label(s.id == cur.id, &s.name).clicked() {
                    let _ = app.run("map.style", json!({"id": s.id}));
                }
            }
        });
        egui::CollapsingHeader::new(tr("Add Tile Server")).id_salt("map-add-server").show(ui, |ui| {
            let (n, u, a) = &mut app.map.new_server;
            ui.add(egui::TextEdit::singleline(n).hint_text(tr("Name")));
            ui.add(egui::TextEdit::singleline(u).hint_text("https://…/{z}/{x}/{y}.png"));
            ui.add(egui::TextEdit::singleline(a).hint_text(tr("Attribution (shown on the map)")));
            if ui.button(tr("Add")).clicked() {
                let (n, u, a) = app.map.new_server.clone();
                match app.run("map.addServer", json!({"name": n, "url": u, "attribution": a})) {
                    Ok(_) => app.map.new_server = Default::default(),
                    Err(e) => app.toast_error(ui.ctx(), e),
                }
            }
        });
        if let Some((bytes, files)) = app.map.tiles.cache_usage() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Tile cache: {mb} MB, {n} tiles", mb = bytes / (1024 * 1024), n = files)).weak().size(11.0));
                if ui.small_button(tr("Clear")).clicked() {
                    let _ = app.run("map.tileCache", json!({"clear": true}));
                }
            });
        }
    });
}

fn locations_section(app: &mut DacApp, ui: &mut egui::Ui) {
    section(ui, "Saved Locations", true, |ui| {
        let list = app.run("map.locations", json!({})).ok().and_then(|v| v["locations"].as_array().cloned()).unwrap_or_default();
        for l in &list {
            let name = l["name"].as_str().unwrap_or("").to_string();
            let private = l["private"].as_bool().unwrap_or(false);
            ui.horizontal(|ui| {
                let mut p = private;
                if ui.checkbox(&mut p, "").on_hover_text(tr("Private: photos here are exported without their location")).changed() {
                    let _ = app.run("map.saveLocation", json!({"name": name, "private": p}));
                }
                let label = format!("{}{}  ({})", if private { "🔒 " } else { "" }, name, l["photos"].as_u64().unwrap_or(0));
                if ui.link(label).clicked() {
                    let _ = app.run("map.search", json!({"query": name}));
                }
                if ui.small_button("×").on_hover_text(tr("Delete this saved location")).clicked() {
                    let _ = app.run("map.deleteLocation", json!({"name": name}));
                }
            });
        }
        if list.is_empty() {
            ui.label(RichText::new(tr("Name a place, set its radius and save it at the map's centre (or right-click the map).")).weak().size(11.0));
        }
        ui.separator();
        let resp = ui.add(egui::TextEdit::singleline(&mut app.map.new_name).hint_text(tr("New location name")));
        if resp.has_focus() {
            app.text_focus = true;
        }
        ui.horizontal(|ui| {
            ui.label(tr("Radius"));
            ui.add(egui::DragValue::new(&mut app.map.new_radius).range(10.0..=100_000.0).speed(10.0).suffix(" m"));
            ui.checkbox(&mut app.map.new_private, tr("Private"));
        });
        if ui.button(tr("Save at Map Centre")).clicked() {
            let c = app.map.centre();
            let name = app.map.new_name.clone();
            match app.run(
                "map.saveLocation",
                json!({"name": name, "lat": c.lat, "lon": c.lon, "radius": app.map.new_radius, "private": app.map.new_private}),
            ) {
                Ok(_) => app.map.new_name.clear(),
                Err(e) => app.toast_error(ui.ctx(), e),
            }
        }
    });
}

fn track_section(app: &mut DacApp, ui: &mut egui::Ui) {
    section(ui, "Tracklog", true, |ui| {
        if ui.button(tr("Load Tracklog…")).clicked()
            && let Err(e) = app.run("map.loadTrack", json!({}))
        {
            app.toast_error(ui.ctx(), e);
        }
        let mut remove = None;
        for (i, tr_) in app.map.tracks.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.checkbox(&mut tr_.shown, "");
                ui.label(format!("{} ({})", tr_.track.name, tr_.track.points()));
                if ui.small_button("×").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove
            && i < app.map.tracks.len()
        {
            app.map.tracks.remove(i);
        }
        if app.map.tracks.iter().any(|t| t.track.timed) {
            ui.horizontal(|ui| {
                ui.label(tr("Camera time zone"));
                let r = ui.add(egui::TextEdit::singleline(&mut app.map.offset).hint_text("+02:00").desired_width(70.0));
                if r.has_focus() {
                    app.text_focus = true;
                }
            });
            if ui
                .button(tr("Set Offset from Active Photo"))
                .on_hover_text(tr("The active photo was taken where it is geotagged (or at the map's centre): match it to the track"))
                .clicked()
            {
                let path = app.map.tracks.iter().rev().find(|t| t.track.timed).map(|t| t.path.clone()).unwrap_or_default();
                let mut p = json!({"path": path});
                if let Some(id) = app.session.active() {
                    let has_gps = app.session.catalog.photo(id).is_some_and(|ph| ph.meta.gps.is_some());
                    p["id"] = json!(id.0);
                    if !has_gps {
                        let c = app.map.centre();
                        p["lat"] = json!(c.lat);
                        p["lon"] = json!(c.lon);
                    }
                }
                match app.run("map.trackOffset", p) {
                    Ok(v) => app.map.offset = v["offset"].as_str().unwrap_or("").to_string(),
                    Err(e) => app.toast_error(ui.ctx(), e),
                }
            }
            let has_sel = !app.session.selection.ids.is_empty();
            if ui.add_enabled(has_sel, egui::Button::new(tr("Auto-Tag Selected Photos"))).clicked() {
                match app.run("map.autoTag", json!({})) {
                    Ok(v) => app.toast(ui.ctx(), format!("Tagged {n} photos from the tracklog", n = v["tagged"].as_u64().unwrap_or(0))),
                    Err(e) => app.toast_error(ui.ctx(), e),
                }
            }
        }
    });
}

fn geocode_section(app: &mut DacApp, ui: &mut egui::Ui) {
    section(ui, "Reverse Geocoding", false, |ui| {
        ui.label(RichText::new(tr("Fills Sublocation, City, State and Country from GPS. Off until you choose a provider.")).weak().size(11.0));
        let cur = app.map.prefs.geocoder.clone();
        for (key, label) in [("off", "Off"), ("offline", "Offline (GeoNames place list)"), ("online", "Online (Nominatim-compatible server)")] {
            if ui.radio(cur == key, tr(label)).clicked() {
                let _ = app.run("map.geocoder", json!({"provider": key}));
            }
        }
        match cur.as_str() {
            "offline" => {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let st = app.run("map.geocodeStatus", json!({})).unwrap_or_default();
                    let have = st["offline"]["downloaded"].as_bool().unwrap_or(false);
                    ui.label(RichText::new(dac_geo::geocode::GEONAMES_ATTRIBUTION).weak().size(11.0));
                    let busy = app.map.jobs.iter().any(|j| matches!(j, Job::Download(_)));
                    let label =
                        if have { tr("Update Place Names (from geonames.org)") } else { tr("Download Place Names (from geonames.org, ~3 MB)") };
                    if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
                        start_download(app);
                    }
                }
            }
            "online" => {
                let r = ui.add(egui::TextEdit::singleline(&mut app.map.prefs.endpoint).desired_width(240.0));
                if r.has_focus() {
                    app.text_focus = true;
                }
                if r.lost_focus() {
                    app.map.save();
                }
                let mut consent = app.map.prefs.online_consent;
                if ui.checkbox(&mut consent, tr("Allow sending photo positions and searches to this server")).changed() {
                    let _ = app.run("map.geocoder", json!({"consent": consent}));
                }
                ui.label(
                    RichText::new(format!("{} {}", tr("At most one request per second."), dac_geo::geocode::NOMINATIM_ATTRIBUTION)).weak().size(11.0),
                );
            }
            _ => {}
        }
        let busy = app.map.jobs.iter().any(|j| matches!(j, Job::Geocode(_)));
        let can = cur != "off" && !app.session.selection.ids.is_empty() && !busy;
        if ui.add_enabled(can, egui::Button::new(tr("Look Up Selected Photos"))).clicked() {
            match app.run("map.lookup", json!({})) {
                Ok(v) if v.get("pending").is_some() => app.toast(ui.ctx(), tr("Looking up locations…")),
                Ok(v) => app.toast(ui.ctx(), format!("Filled the location of {n} photos", n = v["found"].as_u64().unwrap_or(0))),
                Err(e) => app.toast_error(ui.ctx(), e),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_parse() {
        assert_eq!(parse_coords("48.85, 2.35"), Some(LatLon::new(48.85, 2.35)));
        assert_eq!(parse_coords("-33.9 151.2"), Some(LatLon::new(-33.9, 151.2)));
        assert_eq!(parse_coords("Paris"), None);
        assert_eq!(parse_coords("91, 0"), None);
        assert_eq!(parse_coords("1, 2, 3"), None);
    }
}
