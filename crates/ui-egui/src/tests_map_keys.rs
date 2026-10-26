//! Headless tests of the Map view's keys, its View ▸ Map menu, Hide Pins and Settings ▸ Map
//! (tile source switching, the custom server applied only on confirm, Clear Cache). Tiles come
//! from in-process fake servers (never the network).

use std::sync::{Arc, Mutex};

use lightcraft_map::{HttpResponse, Transport};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::menubar::MenuNode;
use crate::state::{RightPanel, ViewMode};
use crate::tests_map::{T, clusters, demo, fake, frames, run, state, widget};

fn key(h: &mut Headless, k: &str, cmd: bool) {
    let r = h.request("ui.key", json!({"key": k, "cmd": cmd}), T);
    assert_eq!(r["ok"], true, "{k}: {r}");
    frames(h, 2);
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    frames(h, 3);
}

/// The map open on Paris and London, settled.
fn map() -> Headless {
    let (mut h, _) = demo(Some(fake(false)));
    run(&mut h, "view.map", json!({}));
    frames(&mut h, 3);
    h
}

#[test]
fn map_keys_run_their_commands_in_the_map_view() {
    let mut h = map();
    run(&mut h, "map.location.add", json!({"name": "Paris", "lat": 48.8566, "lon": 2.3522, "radius": 5000}));
    frames(&mut h, 2);
    let before = (h.app.ui.right, h.app.ui.before_after, h.app.ui.mask_overlay);
    assert!(!clusters(&state(&mut h)).is_empty());

    // H hides the photo pins (saved locations stay), and brings them back
    key(&mut h, "H", false);
    let s = state(&mut h);
    assert_eq!(s["showPins"], false, "{s}");
    assert!(clusters(&s).is_empty(), "no pins: {s}");
    assert_eq!(s["locations"].as_array().map(Vec::len), Some(1), "saved locations stay: {s}");
    assert!(h.app.widgets.iter().all(|(w, _)| !w.starts_with("map:pin:")));
    key(&mut h, "H", false);
    assert!(h.app.ui.map_show_pins && !clusters(&state(&mut h)).is_empty());

    // ⌘K locks the markers; \ hides the location filter bar; O folds the Saved Locations list
    key(&mut h, "K", true);
    assert!(h.app.ui.map_lock_markers);
    key(&mut h, "K", true);
    assert!(!h.app.ui.map_lock_markers);
    assert!(widget(&h, "map:filterBar").is_some());
    key(&mut h, "Backslash", false);
    assert!(!h.app.ui.map_filter_bar && widget(&h, "map:filterBar").is_none());
    key(&mut h, "Backslash", false);
    assert!(h.app.ui.map_filter_bar && widget(&h, "map:filterBar").is_some());
    key(&mut h, "O", false);
    assert_eq!(state(&mut h)["locationList"], false);
    key(&mut h, "O", false);
    assert_eq!(state(&mut h)["locationList"], true);

    // = / − zoom the map one step (and leave the grid thumbnail size alone)
    let thumbs = h.app.ui.thumb_size;
    let z = state(&mut h)["zoom"].as_f64().unwrap();
    key(&mut h, "Equals", false);
    assert_eq!(state(&mut h)["zoom"].as_f64().unwrap(), (z + 1.0).floor());
    key(&mut h, "Minus", false);
    key(&mut h, "Minus", false);
    assert_eq!(state(&mut h)["zoom"].as_f64().unwrap(), (z + 1.0).floor() - 2.0);
    // + (the keypad's, or Shift+= reported as Plus) zooms in like =
    key(&mut h, "Plus", false);
    assert_eq!(state(&mut h)["zoom"].as_f64().unwrap(), (z + 1.0).floor() - 1.0);
    assert_eq!(h.app.ui.thumb_size, thumbs, "= / − / + in the Map view don't size the grid thumbnails");

    // the keys they share stayed the map's: no Remove panel, no Show Original, no mask overlay change
    assert_eq!((h.app.ui.right, h.app.ui.before_after, h.app.ui.mask_overlay), before);
    assert_eq!(h.app.ui.view, ViewMode::Map);
}

#[test]
fn map_keys_do_nothing_outside_the_map_view() {
    let mut h = map();
    run(&mut h, "view.detail", json!({}));
    frames(&mut h, 3);
    let cam = h.app.map.camera;
    let (lock, overlay) = (h.app.ui.map_lock_markers, h.app.ui.mask_overlay);
    // H is still Remove, O the mask overlay; ⌘K, = and − do nothing to the map
    key(&mut h, "H", false);
    assert_eq!(h.app.ui.right, RightPanel::Remove);
    assert!(h.app.ui.map_show_pins, "H outside the map doesn't hide pins");
    key(&mut h, "O", false);
    assert_ne!(h.app.ui.mask_overlay, overlay, "O is the mask overlay outside the map");
    assert_eq!(state(&mut h)["locationList"], true);
    key(&mut h, "Equals", false);
    key(&mut h, "Minus", false);
    key(&mut h, "Backslash", false);
    assert!(h.app.ui.map_filter_bar);
    assert_eq!(h.app.map.camera, cam);
    // ⌘K is Add Keywords, whose field takes the keys typed next
    key(&mut h, "K", true);
    assert_eq!(h.app.ui.right, RightPanel::Keywords);
    assert_eq!(h.app.ui.map_lock_markers, lock);
    h.view.ctx.memory_mut(|m| m.stop_text_input());
    // and in the grid, where = sizes the thumbnails instead
    run(&mut h, "view.photoGrid", json!({}));
    frames(&mut h, 2);
    let thumbs = h.app.ui.thumb_size;
    key(&mut h, "Equals", false);
    assert!(h.app.ui.thumb_size > thumbs, "= in the grid grows the thumbnails");
    key(&mut h, "K", true);
    assert_eq!((h.app.map.camera, h.app.ui.map_lock_markers), (cam, lock));
}

/// View ▸ Map's items (flattened, with the submenus they sit in).
fn map_menu(h: &Headless) -> Vec<(String, MenuNode)> {
    fn walk(nodes: &[MenuNode], path: &str, out: &mut Vec<(String, MenuNode)>) {
        for n in nodes {
            match n {
                MenuNode::Submenu { label, children } => walk(children, &format!("{path}>{label}"), out),
                MenuNode::Item { .. } => out.push((path.to_string(), n.clone())),
                MenuNode::Separator => {}
            }
        }
    }
    let bar = crate::menubar::menu_bar(&h.app);
    let view = &bar.iter().find(|(t, _)| t == "View").expect("View menu").1;
    let Some(MenuNode::Submenu { children, .. }) = view.iter().find(|n| matches!(n, MenuNode::Submenu { label, .. } if label == "Map")) else {
        panic!("no View ▸ Map: {view:?}")
    };
    let mut out = Vec::new();
    walk(children, "Map", &mut out);
    out
}

fn item<'a>(menu: &'a [(String, MenuNode)], id: &str, params: Value) -> &'a MenuNode {
    &menu
        .iter()
        .find(|(_, n)| matches!(n, MenuNode::Item { id: i, params: p, .. } if i == id && *p == params))
        .unwrap_or_else(|| panic!("{id} {params} not in View ▸ Map: {menu:?}"))
        .1
}

fn enabled(n: &MenuNode) -> bool {
    matches!(n, MenuNode::Item { enabled: true, .. })
}

#[test]
fn the_view_map_menu_runs_each_map_command() {
    let mut h = map();
    let menu = map_menu(&h);
    let want = [
        ("Map", "map.fit"),
        ("Map", "map.zoomIn"),
        ("Map", "map.zoomOut"),
        ("Map", "map.showPins"),
        ("Map", "map.lockMarkers"),
        ("Map", "map.filterBar"),
        ("Map>Saved Locations", "map.location.new"),
        ("Map>Saved Locations", "map.showLocations"),
        ("Map>Tracklog", "map.loadTrackLog"),
        ("Map>Tracklog", "map.trackLog.autoTag"),
        ("Map>Tracklog", "map.trackLog"),
        ("Map", "map.settings"),
    ];
    for (path, id) in want {
        assert!(menu.iter().any(|(p, n)| p == path && matches!(n, MenuNode::Item { id: i, .. } if i == id)), "{path} ▸ {id}: {menu:?}");
    }
    // the keys show next to their items; toggles are checked
    let MenuNode::Item { shortcut, checked, .. } = item(&menu, "map.showPins", Value::Null) else { panic!("not an item") };
    assert_eq!((shortcut.as_deref(), *checked), (Some("H"), Some(true)));
    let MenuNode::Item { shortcut, .. } = item(&menu, "map.lockMarkers", Value::Null) else { panic!("not an item") };
    assert_eq!(shortcut.as_deref(), Some("Cmd+K"));
    let MenuNode::Item { shortcut, .. } = item(&menu, "map.fit", Value::Null) else { panic!("not an item") };
    assert_eq!(shortcut.as_deref(), Some("Cmd+0"));
    // Clear Tracklog waits for a track
    assert!(!enabled(item(&menu, "map.trackLog", json!({"clear": true}))));

    // each item runs its command
    crate::menubar::run_item(&mut h.app, "map.showPins", Value::Null).unwrap();
    assert!(!h.app.ui.map_show_pins);
    crate::menubar::run_item(&mut h.app, "map.showPins", Value::Null).unwrap();
    crate::menubar::run_item(&mut h.app, "map.lockMarkers", Value::Null).unwrap();
    assert!(h.app.ui.map_lock_markers);
    crate::menubar::run_item(&mut h.app, "map.filterBar", Value::Null).unwrap();
    assert!(!h.app.ui.map_filter_bar);
    for mode in ["visible", "tagged", "untagged", "none"] {
        let n = item(&map_menu(&h), "map.locationFilter", json!({"mode": mode})).clone();
        let MenuNode::Item { params, .. } = n else { panic!("not an item") };
        crate::menubar::run_item(&mut h.app, "map.locationFilter", params).unwrap();
        frames(&mut h, 1);
        let MenuNode::Item { checked, .. } = item(&map_menu(&h), "map.locationFilter", json!({"mode": mode})).clone() else { panic!("not an item") };
        assert_eq!(checked, Some(true), "{mode}");
        assert_eq!(state(&mut h)["locationFilter"]["mode"], mode);
    }
    let z = state(&mut h)["zoom"].as_f64().unwrap();
    crate::menubar::run_item(&mut h.app, "map.zoomIn", Value::Null).unwrap();
    assert_eq!(state(&mut h)["zoom"].as_f64().unwrap(), (z + 1.0).floor());
    crate::menubar::run_item(&mut h.app, "map.location.new", Value::Null).unwrap();
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::TextPrompt { command, .. }) if command == "map.location.add"));
    h.app.ui.dialog = None;
    crate::menubar::run_item(&mut h.app, "map.showLocations", Value::Null).unwrap();
    assert!(h.app.map.loc.list_folded);
    crate::menubar::run_item(&mut h.app, "map.settings", Value::Null).unwrap();
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { tab }) if tab == "map"));

    // outside the Map view its items are greyed (Map Tiles… and Load Tracklog… excepted)
    h.app.ui.dialog = None;
    run(&mut h, "view.photoGrid", json!({}));
    let menu = map_menu(&h);
    for id in ["map.fit", "map.zoomIn", "map.showPins", "map.lockMarkers", "map.filterBar", "map.location.new", "map.showLocations"] {
        assert!(!enabled(item(&menu, id, Value::Null)), "{id} enabled outside the map");
    }
    assert!(enabled(item(&menu, "map.settings", Value::Null)));
}

#[test]
fn hidden_pins_cant_be_clicked_dragged_or_hovered() {
    let mut h = map();
    let s = state(&mut h);
    let c = clusters(&s).into_iter().next().unwrap();
    let at = egui::pos2(c["x"].as_f64().unwrap() as f32, c["y"].as_f64().unwrap() as f32);
    run(&mut h, "library.select", json!({"ids": []}));
    run(&mut h, "map.showPins", json!({"on": false}));
    frames(&mut h, 2);
    let r = h.request(
        "ui.pointer",
        json!({"screen": true, "events": [{"x": at.x, "y": at.y, "kind": "move"}, {"x": at.x, "y": at.y, "kind": "down"}, {"x": at.x, "y": at.y, "kind": "up"}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    frames(&mut h, 3);
    assert!(h.app.session.selection.ids.is_empty(), "a hidden pin doesn't select");
    assert!(widget(&h, "map:hoverThumb").is_none());
    // a bad value changes nothing
    let r = h.request("engine.execute", json!({"command": "map.showPins", "params": {"on": "yes"}}), T);
    assert_eq!(r["ok"], false);
    assert!(!h.app.ui.map_show_pins);
    // the toolbar toggle brings them back, and the same click selects the pin's photos
    click(&mut h, "map:hidePins");
    assert!(h.app.ui.map_show_pins && !clusters(&state(&mut h)).is_empty());
    let r = h.request(
        "ui.pointer",
        json!({"screen": true, "events": [{"x": at.x, "y": at.y, "kind": "move"}, {"x": at.x, "y": at.y, "kind": "down"}, {"x": at.x, "y": at.y, "kind": "up"}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    frames(&mut h, 3);
    assert!(!h.app.session.selection.ids.is_empty(), "a shown pin selects");
}

/// A tile server that records every URL asked for.
#[derive(Default)]
struct Recorder {
    urls: Mutex<Vec<String>>,
}

impl Recorder {
    fn asked(&self, host: &str) -> usize {
        self.urls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().filter(|u| u.contains(host)).count()
    }
}

impl Transport for Recorder {
    fn get(&self, url: &str, _: &[(&'static str, String)], _: usize, _: &std::sync::atomic::AtomicBool) -> Result<HttpResponse, String> {
        self.urls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(url.to_string());
        Ok(HttpResponse { status: 200, headers: vec![("cache-control".into(), "max-age=604800".into())], body: crate::tests_map::tile_png() })
    }
}

fn settle_tiles(h: &mut Headless) {
    h.step_until(T, |h| h.app.session.map.service().is_none_or(|s| s.is_idle()));
    frames(h, 3);
}

/// The map with its tiles from `rec` for every source Settings ▸ Map picks.
fn recorded(rec: &Arc<Recorder>) -> Headless {
    let (mut h, _) = demo(None);
    h.app.session.map.transport = Some(Arc::clone(rec) as Arc<dyn Transport>);
    run(&mut h, "view.map", json!({}));
    run(&mut h, "app.settings", json!({"tab": "map"}));
    frames(&mut h, 3);
    h
}

#[test]
fn settings_map_switches_the_tile_source() {
    let rec = Arc::new(Recorder::default());
    let mut h = recorded(&rec);
    assert!(h.app.session.map.source().is_offline());
    assert!(h.painted_text().iter().any(|t| t.contains("Map tiles are off (Settings ▸ Map)")), "the note points at Settings ▸ Map");
    // OpenStreetMap
    click(&mut h, "button:settingsMapSource-0");
    assert_eq!(h.app.session.map.source().id, "osm");
    settle_tiles(&mut h);
    assert!(rec.asked("tile.openstreetmap.org") > 0);
    // back offline: nothing more is asked
    click(&mut h, "button:settingsMapSource-1");
    assert!(h.app.session.map.source().is_offline());
    let n = rec.urls.lock().unwrap().len();
    frames(&mut h, 10);
    assert_eq!(rec.urls.lock().unwrap().len(), n);
    // the choice is saved where map.tileSource keeps it
    assert_eq!(run(&mut h, "map.tileSource", json!({}))["offline"], true);
}

#[test]
fn a_custom_server_is_used_only_once_confirmed_and_a_bad_one_changes_nothing() {
    let rec = Arc::new(Recorder::default());
    let mut h = recorded(&rec);
    click(&mut h, "button:settingsMapSource-0");
    settle_tiles(&mut h);
    // Custom: the fields open, the map keeps OpenStreetMap
    click(&mut h, "button:settingsMapSource-2");
    assert_eq!(h.app.session.map.source().id, "osm");
    click(&mut h, "field:settingsMapUrl");
    // typed a piece at a time, frames in between: nothing reaches the map or the server
    for part in ["https://", "tiles.example", ".org/{z}", "/{x}/{y}", ".png"] {
        let r = h.request("ui.text", json!({"text": part}), T);
        assert_eq!(r["ok"], true);
        frames(&mut h, 4);
        assert_eq!(h.app.session.map.source().id, "osm", "applied while typing");
    }
    assert_eq!(h.app.map.source_draft.url, "https://tiles.example.org/{z}/{x}/{y}.png");
    click(&mut h, "field:settingsMapAttribution");
    h.request("ui.text", json!({"text": "© Example tiles"}), T);
    frames(&mut h, 4);
    settle_tiles(&mut h);
    assert_eq!(rec.asked("example"), 0, "no tile asked from a half-typed or unconfirmed server");
    click(&mut h, "button:settingsMapApply");
    let src = h.app.session.map.source().clone();
    assert_eq!((src.url_template.as_str(), src.attribution.as_str()), ("https://tiles.example.org/{z}/{x}/{y}.png", "© Example tiles"));
    settle_tiles(&mut h);
    assert!(rec.asked("tiles.example.org") > 0, "the confirmed server serves the map");
    assert!(h.painted_text().iter().any(|t| t.contains("© Example tiles")), "its credit is on the map");

    // a bad template (no https, no {x}/{y}) and a bad max zoom are refused, the source stays
    click(&mut h, "button:settingsMapSource-2");
    h.app.map.source_draft.url = "http://tiles.bad.example/{z}.png".into();
    click(&mut h, "button:settingsMapApply");
    assert!(h.app.map.source_draft.error.is_some(), "refused");
    assert_eq!(h.app.session.map.source().url_template, src.url_template);
    h.app.map.source_draft.url = src.url_template.clone();
    h.app.map.source_draft.max_zoom = "99".into();
    click(&mut h, "button:settingsMapApply");
    assert!(h.app.map.source_draft.error.is_some());
    assert_eq!(h.app.session.map.source(), &src);
    settle_tiles(&mut h);
    assert_eq!(rec.asked("bad.example"), 0);
}

#[test]
fn clear_cache_in_settings_empties_the_tile_cache() {
    let dir = std::env::temp_dir().join(format!("lightcraft-map-settings-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let rec = Arc::new(Recorder::default());
    let (mut h, _) = demo(None);
    h.app.session.map.transport = Some(Arc::clone(&rec) as Arc<dyn Transport>);
    h.app.session.map.cache_root = Some(dir.clone());
    run(&mut h, "map.tileSource", json!({"id": "osm"}));
    run(&mut h, "view.map", json!({}));
    frames(&mut h, 3);
    settle_tiles(&mut h);
    assert!(h.step_until(T, |h| h.app.session.map.cache_root.as_deref().is_some_and(|d| lightcraft_map::cache::info(d).tiles > 0)));
    run(&mut h, "app.settings", json!({"tab": "map"}));
    frames(&mut h, 3);
    assert!(h.painted_text().iter().any(|t| t.contains("tiles (limit")), "the cache's size is shown");
    // the map off screen while it clears: nothing refills it
    run(&mut h, "view.photoGrid", json!({}));
    frames(&mut h, 2);
    click(&mut h, "button:settingsMapClearCache");
    let info = run(&mut h, "map.cacheInfo", json!({}));
    assert_eq!(info["tiles"], 0, "{info}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The PR's pictures (`LIGHTCRAFT_TEST_SHOTS=<dir>`): View ▸ Map open, Settings ▸ Map, and the map
/// with its pins hidden (saved locations stay).
#[test]
fn screenshots_of_map_keys_menus_and_settings() {
    let Some(dir) = std::env::var_os("LIGHTCRAFT_TEST_SHOTS") else { return };
    let png = |img: &lightcraft_raster::Rgba8| {
        lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default()).unwrap_or_default()
    };
    let rec = Arc::new(Recorder::default());
    let services = crate::Services {
        png: Some(Box::new(png)),
        write: Some(Box::new(|p: &str, b: &[u8]| std::fs::write(p, b).map_err(|e| e.to_string()))),
        ..Default::default()
    };
    let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    app.session.map.transport = Some(Arc::clone(&rec) as Arc<dyn Transport>);
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    h.settle(crate::tests_map::SETTLE);
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(6).map(|p| p.0).collect();
    for (id, (lat, lon)) in ids.iter().zip(crate::tests_map::PLACES) {
        h.app.run("photo.setMeta", json!({"ids": [id], "gps": [lat, lon]})).unwrap();
    }
    run(&mut h, "view.map", json!({}));
    run(&mut h, "map.location.add", json!({"name": "Paris", "lat": 48.8566, "lon": 2.3522, "radius": 60000}));
    run(&mut h, "map.location.add", json!({"name": "London", "lat": 51.5072, "lon": -0.1276, "radius": 40000}));
    frames(&mut h, 3);
    settle_tiles(&mut h);
    let shot = |h: &mut Headless, name: &str| {
        let path = std::path::Path::new(&dir).join(name);
        let r = h.request("ui.screenshot", json!({"path": path.to_string_lossy(), "headless": true}), T);
        assert_eq!(r["ok"], true, "{r}");
    };
    // View ▸ Map ▸ Location Filter open
    click(&mut h, "menu:View");
    let r = h.request("ui.hoverWidget", json!({"id": "menusub:Map"}), T);
    assert_eq!(r["ok"], true, "{r}");
    frames(&mut h, 4);
    let r = h.request("ui.hoverWidget", json!({"id": "menusub:Location Filter"}), T);
    assert_eq!(r["ok"], true, "{r}");
    frames(&mut h, 4);
    shot(&mut h, "map-view-menu.png");
    h.request("ui.key", json!({"key": "Escape"}), T);
    frames(&mut h, 3);
    h.request("ui.key", json!({"key": "Escape"}), T);
    frames(&mut h, 3);
    // pins hidden (H): the saved locations stay
    run(&mut h, "map.setView", json!({"lat": 50.2, "lon": 1.1, "zoom": 7}));
    settle_tiles(&mut h);
    key(&mut h, "H", false);
    frames(&mut h, 3);
    shot(&mut h, "map-pins-hidden.png");
    key(&mut h, "H", false);
    // Settings ▸ Map with a custom server being typed
    run(&mut h, "app.settings", json!({"tab": "map"}));
    frames(&mut h, 2);
    click(&mut h, "button:settingsMapSource-2");
    h.app.map.source_draft.url = "https://tiles.example.org/{z}/{x}/{y}.png".into();
    h.app.map.source_draft.attribution = "© Example tiles".into();
    frames(&mut h, 3);
    shot(&mut h, "map-settings.png");
}
