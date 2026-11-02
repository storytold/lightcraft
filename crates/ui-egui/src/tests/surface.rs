//! Control surfaces in the app (`crate::surface`, Settings ▸ Controls, the on-screen readout).
//!
//! Scenarios, as a photographer with an MX Creative Console sees them:
//!
//! * The plugin asks to pair; LightCraft says so; I check the code and approve it in Settings ▸
//!   Controls; from then on the plugin connects with its token. Anything unpaired is refused.
//! * Turning the dial moves Exposure on the open photo, and the window shows the value. When I
//!   stop turning, that turn is one undo step.
//! * With "Dials change every selected photo" on, the turn lands on every selected photo, still as
//!   one undo step.
//! * Opening Crop puts the crop page on the keypad and Straighten on the dial.
//! * In Settings ▸ Controls I can press Learn, touch a key, and change what it does.
//! * My TactileBridge settings come over in one go; damaged files never crash anything.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::control::ControlRequest;
use crate::headless::Headless;
use crate::state::{Dialog, RightPanel, ViewMode};
use crate::surface::{EventSink, SurfaceStore};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// An in-memory settings file.
#[derive(Clone, Default)]
struct MemStore(Arc<std::sync::Mutex<Option<String>>>);

impl SurfaceStore for MemStore {
    fn load(&self) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn save(&self, text: &str) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(text.to_string());
        Ok(())
    }
}

fn demo_with(store: MemStore, enabled: bool) -> Headless {
    if enabled {
        let s = lightcraft_surface::SurfaceSettings { enabled: true, ..Default::default() };
        store.save(&s.to_json()).unwrap();
    }
    let services = Services { png: None, surface_store: Some(Box::new(store)), ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.settle(T);
    h
}

fn demo() -> Headless {
    demo_with(MemStore::default(), true)
}

/// A request on a TCP-like connection `conn` whose events go to `sink`.
fn request_on(h: &mut Headless, conn: u64, sink: &EventSink, method: &str, params: Value) -> Value {
    let (req, rx) = ControlRequest::new(method, params);
    let req = req.on_connection(conn, sink.clone());
    h.control_sender().send(req).unwrap();
    let t0 = Instant::now();
    loop {
        h.step();
        if let Ok(v) = rx.try_recv() {
            return v;
        }
        assert!(t0.elapsed() < T, "no reply to {method}");
    }
}

fn sink() -> (EventSink, Receiver<Value>) {
    let (tx, rx) = channel();
    (EventSink { tx, open: Arc::new(AtomicBool::new(true)) }, rx)
}

fn exposure(h: &Headless) -> f64 {
    let id = h.app.session.active().unwrap();
    h.app.session.develop_of(id).unwrap().light.exposure
}

/// Open the first photo in Detail with the Edit panel.
fn open_photo(h: &mut Headless) {
    let first = h.app.session.visible_cloned()[0];
    assert_eq!(h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [first.0]}}), T)["ok"], true);
    h.app.ui.view = ViewMode::Detail;
    h.app.ui.right = RightPanel::Edit;
    h.settle(T);
}

/// Frames until the dial gesture has ended (the default 800 ms at 60 frames a second).
fn finish_gesture(h: &mut Headless) {
    for _ in 0..70 {
        h.step();
    }
}

fn input(h: &mut Headless, events: Value) -> Value {
    h.request("engine.execute", json!({"command": "surface.input", "params": {"events": events}}), T)
}

#[test]
fn pairing_then_subscribe_and_turn_over_a_connection() {
    let mut h = demo();
    open_photo(&mut h);
    let (sink1, events1) = sink();
    // unpaired: hello says so, input and subscribe are refused
    let r = request_on(&mut h, 7, &sink1, "surface.hello", json!({"protocol": 1, "client": "test"}));
    assert_eq!(r["result"]["paired"], false, "{r}");
    let r = request_on(&mut h, 7, &sink1, "surface.input", json!({"events": [{"control": "dial", "ticks": 5}]}));
    assert_eq!(r["ok"], false, "{r}");
    assert!(r["error"].as_str().unwrap().contains("isn't paired"));
    // ask; Settings ▸ Controls shows the request with its code
    let r = request_on(&mut h, 7, &sink1, "surface.pair", json!({"client": "test", "name": "Test Plugin"}));
    assert_eq!(r["result"]["status"], "pending", "{r}");
    let (req, code) = (r["result"]["request"].as_str().unwrap().to_string(), r["result"]["code"].as_str().unwrap().to_string());
    assert_eq!(code.len(), 6);
    h.app.ui.dialog = Some(Dialog::Settings { tab: "controls".into() });
    h.settle(T);
    assert!(h.painted_text().iter().any(|t| t.contains(&code)), "the code is shown");
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:surfaceApprove-test"}), T)["ok"], true);
    h.settle(T);
    assert_eq!(h.app.surface.settings().paired.len(), 1);
    let r = request_on(&mut h, 7, &sink1, "surface.pair", json!({"request": req}));
    assert_eq!(r["result"]["status"], "approved", "{r}");
    let token = r["result"]["token"].as_str().unwrap().to_string();
    h.app.ui.dialog = None;
    // a new connection presents the token
    let (sink2, events2) = sink();
    let r = request_on(&mut h, 8, &sink2, "surface.hello", json!({"protocol": 1, "token": token}));
    assert_eq!(r["result"]["paired"], true, "{r}");
    let r = request_on(&mut h, 9, &sink2, "surface.hello", json!({"token": "forged"}));
    assert_eq!(r["result"]["paired"], false);
    let r = request_on(&mut h, 8, &sink2, "surface.subscribe", json!({"device": "mx-creative-console"}));
    assert_eq!(r["ok"], true, "{r}");
    let layout = &r["result"]["layout"];
    assert_eq!(layout["dials"][1]["binding"]["target"], "light.exposure");
    assert_eq!(layout["keys"].as_array().unwrap().len(), 9);
    // turn the dial: exposure moves, the readout shows it, the plugin hears the new value
    let before = exposure(&h);
    let r = request_on(&mut h, 8, &sink2, "surface.input", json!({"events": [{"control": "roller", "ticks": 3}, {"control": "roller", "ticks": 2}]}));
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..3 {
        h.step();
    }
    assert!(exposure(&h) > before, "{} > {before}", exposure(&h));
    assert!(h.painted_text().iter().any(|t| t == "Exposure"), "the on-screen readout");
    let pushed: Vec<Value> = events2.try_iter().collect();
    let last = pushed.iter().rev().find(|e| e["event"] == "surface.layout").unwrap_or_else(|| panic!("no layout pushed: {pushed:?}"));
    let shown: f64 = last["layout"]["dials"][1]["value"].as_str().unwrap().trim_start_matches('+').parse().unwrap();
    assert!((shown - exposure(&h)).abs() < 0.006, "the pushed value is the photo's: {shown} vs {}", exposure(&h));
    // the first connection never subscribed: nothing pushed to it
    assert!(events1.try_iter().next().is_none());
    // the turn is one undo step once it ends
    finish_gesture(&mut h);
    let after = exposure(&h);
    assert_eq!(h.request("engine.execute", json!({"command": "edit.undo"}), T)["ok"], true);
    assert!((exposure(&h) - before).abs() < 1e-9, "one undo puts the whole turn back: {} vs {before} (was {after})", exposure(&h));
    // forget it (the command Forget runs): the token stops working
    assert_eq!(h.request("engine.execute", json!({"command": "surface.forget", "params": {"client": "nobody"}}), T)["ok"], false);
    assert_eq!(h.request("engine.execute", json!({"command": "surface.forget", "params": {"client": "test"}}), T)["ok"], true);
    let r = request_on(&mut h, 10, &sink2, "surface.hello", json!({"token": token}));
    assert_eq!(r["result"]["paired"], false);
}

#[test]
fn apply_to_selected_changes_every_selected_photo_in_one_step() {
    let mut h = demo();
    open_photo(&mut h);
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(3).map(|p| p.0).collect();
    assert_eq!(h.request("engine.execute", json!({"command": "library.select", "params": {"ids": ids}}), T)["ok"], true);
    h.settle(T);
    let r = h.request("engine.execute", json!({"command": "surface.applyToSelected", "params": {"on": true}}), T);
    assert_eq!(r["result"]["on"], true, "{r}");
    let exp = |h: &Headless, id: u64| h.app.session.develop_of(lightcraft_catalog::PhotoId(id)).unwrap().light.exposure;
    let before: Vec<f64> = ids.iter().map(|i| exp(&h, *i)).collect();
    assert_eq!(input(&mut h, json!([{"control": "roller", "ticks": 10}]))["ok"], true);
    finish_gesture(&mut h);
    for (i, id) in ids.iter().enumerate() {
        assert!((exp(&h, *id) - before[i] - 0.10).abs() < 1e-6, "photo {id}: {} vs {}", exp(&h, *id), before[i]);
    }
    assert_eq!(h.request("engine.execute", json!({"command": "edit.undo"}), T)["ok"], true);
    for (i, id) in ids.iter().enumerate() {
        assert!((exp(&h, *id) - before[i]).abs() < 1e-9, "one undo step for all");
    }
}

#[test]
fn pages_follow_crop_and_keys_run_commands() {
    let mut h = demo();
    open_photo(&mut h);
    let state = |h: &mut Headless| h.request("engine.execute", json!({"command": "surface.state"}), T)["result"].clone();
    let s = state(&mut h);
    assert_eq!((s["keyPage"].as_str(), s["dialPage"].as_str()), (Some("Page 1"), Some("Dial Page")), "{s}");
    assert!(s["context"].as_str().unwrap().starts_with("develop"), "{s}");
    h.app.ui.right = RightPanel::Crop;
    h.settle(T);
    let s = state(&mut h);
    assert_eq!((s["context"].as_str(), s["keyPage"].as_str(), s["dialPage"].as_str()), (Some("crop"), Some("Crop"), Some("Crop")), "{s}");
    // the dial straightens
    assert_eq!(input(&mut h, json!([{"control": "dial", "ticks": 50}]))["ok"], true);
    finish_gesture(&mut h);
    let id = h.app.session.active().unwrap();
    assert!(h.app.session.develop_of(id).unwrap().crop.geometry.angle.abs() > 0.1);
    // back to Edit: the hand-picked pages come back
    h.app.ui.right = RightPanel::Edit;
    h.settle(T);
    assert_eq!(state(&mut h)["keyPage"], "Page 1");
    // a key runs its command: Page 1, key 2 = Next photo
    let before = h.app.session.active();
    assert_eq!(input(&mut h, json!([{"control": "key.2", "press": true}, {"control": "key.2", "press": false}]))["ok"], true);
    h.settle(T);
    assert_ne!(h.app.session.active(), before);
    // the dialpad's fourth button turns the dial page
    assert_eq!(input(&mut h, json!([{"control": "pad.4", "press": true}]))["ok"], true);
    assert_eq!(state(&mut h)["dialPage"], "White Balance");
}

#[test]
fn settings_tab_learn_and_edit_a_key() {
    let store = MemStore::default();
    let mut h = demo_with(store.clone(), true);
    open_photo(&mut h);
    assert_eq!(h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": "controls"}}), T)["ok"], true);
    // the drawn device and its editor: Edit Keys and Dials… in Settings ▸ Controls
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:surfaceEditKeys"}), T)["ok"], true);
    h.settle(T);
    h.settle(T);
    for id in [
        "button:surfaceControl-key.1",
        "button:surfaceDevice-dialpad",
        "button:surfaceSection-feel",
        "check:surface.touchFind",
        "check:surface.enabled",
        "combo:surfaceProfile",
    ] {
        assert!(h.app.widgets.iter().any(|(w, _)| w == id), "{id}");
    }
    touch_to_find_selects_keys_and_dials(&mut h);
    an_action_picked_from_the_list_is_bound(&mut h);
    the_other_sections_open(&mut h);
    a_rebound_key_shows_runs_and_is_saved(&mut h, &store);
}

/// Touch to find (on while the Keys and Dials page is open): a touched key or dial is selected, and nothing runs.
fn touch_to_find_selects_keys_and_dials(h: &mut Headless) {
    // Touch to find (on while this page is open): touch key 5 on the surface: it is selected,
    // and nothing runs
    let clip = h.app.widgets.iter().find(|(w, _)| w == "check:surface.touchFind").map(|(_, r)| *r).unwrap();
    assert!(clip.bottom() < 900.0, "Touch to find is on screen: {clip:?}");
    assert!(h.app.surface.runtime.learning());
    let before = h.app.session.active();
    assert_eq!(input(h, json!([{"control": "key.5", "press": true}]))["ok"], true);
    h.settle(T);
    assert_eq!(h.app.surface.editor.control.as_deref(), Some("key.5"));
    assert_eq!(h.app.session.active(), before, "a learnt press doesn't act");
    assert!(h.app.widgets.iter().any(|(w, _)| w == "field:surfaceActionSearch"), "the selected key's editor");
    // touching a dial switches the drawing to the Dialpad, and the photo doesn't change
    let exposure_before = exposure(h);
    assert_eq!(input(h, json!([{"control": "dial", "ticks": 1}]))["ok"], true);
    assert_eq!(input(h, json!([{"control": "dial", "ticks": 3}]))["ok"], true);
    h.settle(T);
    assert_eq!(h.app.surface.editor.device, "dialpad");
    assert_eq!(h.app.surface.editor.control.as_deref(), Some("dial"));
    assert_eq!(exposure(h), exposure_before, "a touched dial is found, not turned");
    assert!(h.app.widgets.iter().any(|(w, _)| w == "button:surfaceControl-dial"), "the Dialpad is drawn");
}

/// An action picked from the list (search, click) is bound to the selected dial.
fn an_action_picked_from_the_list_is_bound(h: &mut Headless) {
    // pick an action from the list: search, click
    h.app.surface.editor.filter = "Highlights".into();
    h.settle(T);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "item:surfaceAction-v.light.highlights"}), T)["ok"], true);
    h.settle(T);
    let p = h.app.surface.settings().active_profile().unwrap().clone();
    assert!(
        p.pages.iter().any(|pg| pg.bindings.get("dial") == Some(&lightcraft_surface::Binding::value("light.highlights"))),
        "the dial moves Highlights now"
    );
}

/// The other sections of the Controls tab open, once the Keys and Dials window over them is closed.
fn the_other_sections_open(h: &mut Headless) {
    // the other sections open. They are in the Settings window, under the Keys and Dials window
    // while that is open: a covered button takes no click, so close it first (as its close box does)
    let covered = h.request("ui.clickWidget", json!({"id": "button:surfaceSection-feel"}), T);
    assert_eq!(covered["ok"], false, "a section button under the Keys and Dials window can't be clicked: {covered}");
    h.app.surface.editor.window = false;
    h.settle(T);
    assert!(!h.app.widgets.iter().any(|(w, _)| w == "window:surfaceEditor"), "the Keys and Dials window is closed");
    for s in ["pages", "feel", "readout", "setup"] {
        assert_eq!(h.request("ui.clickWidget", json!({"id": format!("button:surfaceSection-{s}")}), T)["ok"], true, "{s}");
        h.settle(T);
        assert_eq!(h.app.surface.editor.section, s);
    }
    // and Keys and Dials opens again from its button
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:surfaceEditKeys"}), T)["ok"], true);
    h.settle(T);
    assert!(h.app.surface.editor.window);
    assert!(h.app.widgets.iter().any(|(w, _)| w == "window:surfaceEditor"), "the Keys and Dials window is back");
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:surfaceDevice-keypad"}), T)["ok"], true);
    h.settle(T);
}

/// Key 5 rebound to Contrast shows its new label, moves Contrast on the dial, and is saved.
fn a_rebound_key_shows_runs_and_is_saved(h: &mut Headless, store: &MemStore) {
    // make key 5 put Contrast on the dial
    let profile = h.app.surface.settings().profile.clone();
    let mut s = h.app.surface.settings().clone();
    let page = s.profiles.iter_mut().find(|p| p.name == profile).unwrap().pages.iter_mut().find(|p| p.name == "Page 1").unwrap();
    page.bindings.insert("key.5".into(), lightcraft_surface::Binding::value("light.contrast"));
    assert_eq!(h.request("engine.execute", json!({"command": "surface.settings", "params": {"settings": s}}), T)["ok"], true);
    h.settle(T);
    assert!(h.painted_text().iter().any(|t| t == "Contrast"), "the key shows its new label");
    h.app.ui.dialog = None;
    // (the keypad follows the dialog's page until the next frame; Touch to find ends with the page)
    h.settle(T);
    for _ in 0..40 {
        h.step();
    }
    assert!(!h.app.surface.runtime.learning(), "Touch to find stopped with the page");
    // press it: the dial moves Contrast until pressed again
    assert_eq!(input(h, json!([{"control": "key.5", "press": true}, {"control": "dial", "ticks": 4}]))["ok"], true);
    finish_gesture(h);
    let id = h.app.session.active().unwrap();
    assert!(h.app.session.develop_of(id).unwrap().light.contrast > 0.0);
    // saved
    for _ in 0..40 {
        h.step();
    }
    assert!(store.load().unwrap().unwrap().contains("light.contrast"));
}

#[test]
fn tactilebridge_settings_import_and_damaged_files() {
    let dir = std::env::temp_dir().join(format!("lc-surface-tb-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("settings.json");
    std::fs::write(
        &path,
        r##"{"controls": {"globalSpeedMultiplier": 1.2, "adjustmentProfiles": {"Kelvin": {"speedMultiplier": 0.3}}},
            "osd": {"opacity": 0.6, "position": "Up"},
            "toolContext": {"contexts": {"Crop": {"buttons": ["SwitchCropOrientation"], "encoders": ["Zoom"]}}}}"##,
    )
    .unwrap();
    let mut h = demo();
    let r = h.request("engine.execute", json!({"command": "surface.import", "params": {"path": path.to_string_lossy()}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["result"]["profile"], "TactileBridge");
    let s = h.app.surface.settings();
    assert_eq!((s.motion.speed, s.tuning["wb.temp"].speed, s.osd.opacity), (1.2, 0.3, 0.6));
    assert!(r["result"]["report"]["mapped"].as_array().unwrap().len() > 3);
    // junk is an error, not a crash
    std::fs::write(&path, "{not json").unwrap();
    let r = h.request("engine.execute", json!({"command": "surface.import", "params": {"path": path.to_string_lossy()}}), T);
    assert_eq!(r["ok"], false);
    let r = h.request("engine.execute", json!({"command": "surface.import", "params": {"path": dir.join("missing.lp5").to_string_lossy()}}), T);
    assert_eq!(r["ok"], false);
    let _ = std::fs::remove_dir_all(&dir);
    // a damaged settings file: defaults, a notice, the file kept until something changes
    let store = MemStore::default();
    store.save("{ damaged").unwrap();
    let services = Services { png: None, surface_store: Some(Box::new(store.clone())), ..Default::default() };
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1000.0, 700.0], 1.0);
    h.settle(T);
    assert!(h.app.surface.load_error.is_some());
    assert_eq!(store.load().unwrap().as_deref(), Some("{ damaged"));
}

#[test]
fn surfaces_are_off_until_turned_on_and_hostile_input_is_refused() {
    let mut h = demo_with(MemStore::default(), false);
    let (sink1, _rx) = sink();
    let r = request_on(&mut h, 3, &sink1, "surface.subscribe", json!({}));
    assert!(r["error"].as_str().unwrap().contains("switched off"), "{r}");
    h.app.surface.runtime.settings_mut().enabled = true;
    for bad in [
        json!({}),
        json!({"events": 5}),
        json!({"events": [{"control": "dial"}]}),
        json!({"events": [{"ticks": 1}]}),
        json!({"events": [{"control": "dial", "ticks": "x"}]}),
    ] {
        let r = h.request("engine.execute", json!({"command": "surface.input", "params": bad}), T);
        assert_eq!(r["ok"], false, "{r}");
    }
    let many: Vec<Value> = (0..300).map(|_| json!({"control": "dial", "ticks": 1})).collect();
    assert_eq!(input(&mut h, Value::Array(many))["ok"], false);
    let r = input(&mut h, json!([{"control": "dial", "ticks": 1e300}, {"control": "key.1", "press": true, "page": 99999}]));
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
    let r = h.request("surface.icons", json!({"names": ["exposure", "nope"]}), T);
    assert!(r["result"]["exposure"].as_str().unwrap().contains("<svg"));
    assert!(r["result"].get("nope").is_none());
}

#[test]
fn a_plugin_lists_the_actions_runs_them_and_watches_their_values() {
    let mut h = demo();
    open_photo(&mut h);
    // the catalogue needs no pairing (names and icons only)
    let (s, events) = sink();
    let r = request_on(&mut h, 21, &s, "surface.catalog", json!({}));
    assert_eq!(r["ok"], true, "{r}");
    let entries = r["result"]["entries"].as_array().unwrap();
    let exp = entries.iter().find(|e| e["key"] == "v.light.exposure").expect("exposure is listed");
    assert_eq!((exp["label"].as_str(), exp["value"].as_bool(), exp["icon"].as_str()), (Some("Exposure"), Some(true), Some("exposure")));
    assert!(entries.iter().any(|e| e["key"] == "c.edit.undo" && e["value"] == false));
    assert!(r["result"]["groups"].as_array().unwrap().len() > 5);
    // turning an action from the plugin's own list moves that value, one undo step per turn
    let before = exposure(&h);
    let r = request_on(&mut h, 0, &s, "surface.input", json!({"events": [{"action": "v.light.exposure", "ticks": 4}]}));
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..3 {
        h.step();
    }
    assert!(exposure(&h) > before);
    finish_gesture(&mut h);
    // pressing it resets it
    request_on(
        &mut h,
        0,
        &s,
        "surface.input",
        json!({"events": [{"action": "v.light.exposure", "press": true}, {"action": "v.light.exposure", "press": false}]}),
    );
    h.settle(T);
    assert!(exposure(&h).abs() < 1e-9, "{}", exposure(&h));
    // unknown actions are refused, not guessed
    let r = request_on(&mut h, 0, &s, "surface.input", json!({"events": [{"action": "v.nope", "ticks": 1}]}));
    assert_eq!(r["ok"], false, "{r}");
    // watched values come with the layout
    let r = request_on(&mut h, 0, &s, "surface.watch", json!({"targets": ["light.contrast"]}));
    assert_eq!(r["ok"], false, "subscribe first: {r}");
    assert_eq!(request_on(&mut h, 0, &s, "surface.subscribe", json!({}))["ok"], true);
    let r = request_on(&mut h, 0, &s, "surface.watch", json!({"targets": ["light.contrast", "nope"]}));
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
    let pushed: Vec<Value> = events.try_iter().collect();
    let last = pushed.iter().rev().find(|e| e["event"] == "surface.layout").expect("a layout with values");
    assert_eq!(last["layout"]["values"]["light.contrast"]["value"], "0", "{last}");
    assert!(last["layout"]["values"].get("nope").is_none());
}

/// A Logi Options+ install with the LightCraft plugin, in memory: its data folder's token and port.
#[derive(Default)]
struct FakeLogi {
    token: std::sync::Mutex<Option<String>>,
    port: std::sync::Mutex<Option<u16>>,
}

impl crate::surface_plugin::PluginInstaller for FakeLogi {
    fn name(&self) -> String {
        "test plugin".into()
    }
    fn status(&self) -> crate::surface_plugin::PluginStatus {
        crate::surface_plugin::PluginStatus { host: Some("Logi Options+".into()), installed: Some("1.0.2".into()), package: None, package_path: None }
    }
    fn install(&self, _port: u16) -> Result<String, String> {
        Ok(String::new())
    }
    fn uninstall(&self) -> Result<String, String> {
        Ok(String::new())
    }
    fn plugin_token(&self) -> Option<String> {
        self.token.lock().unwrap().clone()
    }
    fn provision(&self, token: Option<&str>, port: u16) -> Result<(), String> {
        if let Some(t) = token {
            *self.token.lock().unwrap() = Some(t.to_string());
        }
        *self.port.lock().unwrap() = Some(port);
        Ok(())
    }
}

#[test]
fn an_installed_logi_plugin_works_with_no_setup_and_settings_show_the_edited_page() {
    // first run: no settings file, the plugin installed
    let store = MemStore::default();
    let logi = Arc::new(FakeLogi::default());
    let services = Services { png: None, surface_store: Some(Box::new(store.clone())), surface_plugin: Some(logi.clone()), ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.settle(T);
    open_photo(&mut h);
    assert!(h.app.surface.settings().enabled, "on by itself");
    let token = logi.token.lock().unwrap().clone().expect("the plugin got a token");
    assert_eq!(*logi.port.lock().unwrap(), Some(lightcraft_surface::settings::DEFAULT_PORT));
    for _ in 0..40 {
        h.step();
    }
    assert!(store.load().unwrap().unwrap().contains("lightcraft-logi"), "trusted and saved");
    // the plugin connects with it: paired at once, no code to approve
    let (s, events) = sink();
    let r = request_on(&mut h, 31, &s, "surface.hello", json!({"protocol": 1, "client": "lightcraft-logi", "token": token}));
    assert_eq!(r["result"]["paired"], true, "{r}");
    assert_eq!(request_on(&mut h, 31, &s, "surface.subscribe", json!({}))["ok"], true);
    assert_eq!(h.app.surface.pairing.waiting().count(), 0);
    // a second start keeps the same token (nothing rewritten)
    crate::surface_plugin::auto_setup(&mut h.app);
    assert_eq!(logi.token.lock().unwrap().as_deref(), Some(token.as_str()));
    // a dialog doesn't take over the keys (dialog pages are opt-in)
    let page_before = h.app.surface.runtime.pages().clone();
    h.app.ui.dialog = Some(Dialog::Settings { tab: "general".into() });
    h.settle(T);
    assert!(!h.app.surface.runtime.context().starts_with("dialog"), "{}", h.app.surface.runtime.context());
    assert_eq!(h.app.surface.runtime.pages(), &page_before);
    // Settings ▸ Controls shows the device the page being edited, live
    h.app.ui.dialog = Some(Dialog::Settings { tab: "controls".into() });
    h.app.surface.editor.window = true;
    h.app.surface.editor.page = Some("Page 2".into());
    h.settle(T);
    assert_eq!(h.app.surface.runtime.pages().get(&lightcraft_surface::PageGroup::Keys).map(String::as_str), Some("Page 2"));
    let pushed: Vec<Value> = events.try_iter().collect();
    let last = pushed.iter().rev().find(|e| e["event"] == "surface.layout").expect("pushed");
    assert_eq!(last["layout"]["keyPage"], "Page 2", "the console follows the editor");
    // turned off by the user: stays off on the next start (the Keys and Dials window closed, so
    // the Settings page under it takes the click)
    h.app.surface.editor.window = false;
    h.settle(T);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "check:surface.enabled"}), T)["ok"], true);
    h.settle(T);
    assert!(!h.app.surface.settings().enabled && h.app.surface.settings().off_by_user);
    crate::surface_plugin::auto_setup(&mut h.app);
    assert!(!h.app.surface.settings().enabled, "off stays off");
}

/// A port changed in Settings reaches the installed plugin at once, without touching its token
/// (CodeRabbit on #2947).
#[test]
fn a_changed_port_is_handed_to_the_plugin() {
    let logi = Arc::new(FakeLogi::default());
    let services =
        Services { png: None, surface_store: Some(Box::new(MemStore::default())), surface_plugin: Some(logi.clone()), ..Default::default() };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    crate::surface_plugin::auto_setup(&mut app);
    let token = logi.token.lock().unwrap().clone();
    assert!(token.is_some());
    app.surface.runtime.settings_mut().port = 18123;
    app.surface.changed();
    crate::surface_plugin::provision_port(&mut app);
    assert_eq!(*logi.port.lock().unwrap(), Some(18123));
    assert_eq!(*logi.token.lock().unwrap(), token, "the token stays");
}

/// Bind `control` on the key page now showing.
fn bind_key(h: &mut Headless, control: &str, b: lightcraft_surface::Binding) {
    let page = h.app.surface.runtime.pages().get(&lightcraft_surface::PageGroup::Keys).cloned().unwrap();
    let s = h.app.surface.runtime.settings_mut();
    let cur = s.profile.clone();
    let p = s.profiles.iter_mut().find(|p| p.name == cur).unwrap();
    p.pages.iter_mut().find(|q| q.name == page).unwrap().bindings.insert(control.into(), b);
}

#[test]
fn console_erase_works_while_held_and_delete_needs_a_hold() {
    use lightcraft_surface::Binding;
    let mut h = demo();
    open_photo(&mut h);
    bind_key(&mut h, "key.1", Binding::command("brush.erase"));
    bind_key(&mut h, "key.4", Binding::command_with("tool.brush", json!({"new": true})));
    // Erase: on while the key is down (the brush comes up with it), off when it is let go
    assert_eq!(input(&mut h, json!([{"control": "key.1", "press": true}]))["ok"], true);
    h.settle(T);
    assert!(h.app.ui.brush_erase, "erasing while held");
    assert_eq!(h.app.ui.tool, "brush");
    assert_eq!(input(&mut h, json!([{"control": "key.1", "press": false}]))["ok"], true);
    h.settle(T);
    assert!(!h.app.ui.brush_erase, "back to painting when let go");
    // New Brush runs once and is never lit
    assert_eq!(input(&mut h, json!([{"control": "key.4", "press": true}, {"control": "key.4", "press": false}]))["ok"], true);
    h.settle(T);
    let layout = serde_json::to_value(crate::surface::layout(&h.app, &lightcraft_surface::devices::MX_CREATIVE_CONSOLE)).unwrap();
    let keys = layout["keys"].as_array().cloned().unwrap_or_default();
    let new_brush = keys.iter().find(|k| k["control"] == "key.4").cloned().unwrap_or_default();
    assert_ne!(new_brush["active"], true, "{new_brush}");
    // Delete (on the everyday page, the brush put away): a quick press does nothing but say to
    // hold the key
    assert_eq!(h.request("engine.execute", json!({"command": "tool.done"}), T)["ok"], true);
    h.settle(T);
    bind_key(&mut h, "key.3", Binding::command("photo.delete"));
    let n = h.app.session.visible_cloned().len();
    assert_eq!(input(&mut h, json!([{"control": "key.3", "press": true}, {"control": "key.3", "press": false}]))["ok"], true);
    let osd = h.request("engine.execute", json!({"command": "surface.state"}), T)["result"]["osd"].clone();
    assert!(osd["notice"].as_str().is_some_and(|t| t.starts_with("Hold the key")), "{osd}");
    h.settle(T);
    assert_eq!(h.app.session.visible_cloned().len(), n, "a quick press deletes nothing");
    // held for a moment: it runs
    assert_eq!(input(&mut h, json!([{"control": "key.3", "press": true}]))["ok"], true);
    for _ in 0..60 {
        h.step();
    }
    assert_eq!(input(&mut h, json!([{"control": "key.3", "press": false}]))["ok"], true);
    h.settle(T);
    assert_eq!(h.app.session.visible_cloned().len(), n - 1, "held: deleted");
}

#[test]
fn a_loupedeck_plus_works_as_labelled_and_takes_a_loupedeck_profile() {
    let mut h = demo();
    open_photo(&mut h);
    // no setup: the knob labelled Exposure moves Exposure
    let before = exposure(&h);
    assert_eq!(input(&mut h, json!([{"control": "ld.r33", "ticks": 20}]))["ok"], true);
    finish_gesture(&mut h);
    assert!(exposure(&h) > before, "the Exposure knob");
    // a Loupedeck profile: that knob now moves Temperature, Undo stays on its button
    let dir = std::env::temp_dir().join(format!("lc-surface-ld-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("ProfileInfo.json");
    let mut rotate = vec![Value::Null; 127];
    rotate[33] = json!("$Photo___Temperature");
    let mut press = vec![Value::Null; 127];
    press[95] = json!("$Photo___Undo");
    press[76] = json!("$@Generic___@Macro___0123");
    let profile = json!({"displayName": "Mine", "deviceType": "Loupedeck15", "layout": {"layoutModes": [
        {"modeName": "loupe", "workspaces": [{"pressActions": press, "rotateActions": rotate}]}]}});
    std::fs::write(&path, profile.to_string()).unwrap();
    let r = h.request("engine.execute", json!({"command": "surface.import", "params": {"path": path.to_string_lossy()}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["result"]["device"], "loupedeck-plus");
    assert_eq!(r["result"]["report"]["dropped"].as_array().map(Vec::len), Some(1), "the macro is listed as left out");
    for p in &h.app.surface.settings().profiles {
        assert_eq!(p.base.get("ld.r33"), Some(&lightcraft_surface::Binding::value("wb.temp")), "{}", p.name);
    }
    let id = h.app.session.active().unwrap();
    let (exp, temp) = {
        let d = h.app.session.develop_of(id).unwrap();
        (d.light.exposure, d.wb.temp)
    };
    assert_eq!(input(&mut h, json!([{"control": "ld.r33", "ticks": 20}]))["ok"], true);
    finish_gesture(&mut h);
    let d = h.app.session.develop_of(id).unwrap();
    assert_eq!(d.light.exposure, exp);
    assert_ne!(d.wb.temp, temp);
    // a damaged Loupedeck profile is an error, not a crash
    std::fs::write(&path, r#"{"deviceType": "Loupedeck15", "layout": 7}"#).unwrap();
    let r = h.request("engine.execute", json!({"command": "surface.import", "params": {"path": path.to_string_lossy()}}), T);
    assert_eq!(r["ok"], false);
    let _ = std::fs::remove_dir_all(&dir);
}

fn widget(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("{id} is drawn"))
}

#[test]
fn the_dialpad_is_drawn_like_the_device() {
    let mut h = demo();
    assert_eq!(h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": "controls"}}), T)["ok"], true);
    // the drawn device and its editor: Edit Keys and Dials… in Settings ▸ Controls
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:surfaceEditKeys"}), T)["ok"], true);
    h.settle(T);
    h.app.surface.editor.device = "dialpad".into();
    h.settle(T);
    let r = |id: &str| widget(&h, &format!("button:surfaceControl-{id}"));
    let (b1, b2, roller, dial, b3, b4) = (r("pad.1"), r("pad.2"), r("roller"), r("dial"), r("pad.3"), r("pad.4"));
    // buttons 1 and 2 top left, the roller top right
    assert!(b1.center().x < b2.center().x && b2.center().x < roller.center().x, "{b1:?} {b2:?} {roller:?}");
    assert!((b1.center().y - b2.center().y).abs() < 2.0 && (b2.center().y - roller.center().y).abs() < 4.0);
    assert!(roller.width() > roller.height(), "the roller is a horizontal wheel");
    // the big dial in the middle, the biggest control
    assert!(dial.top() > b1.bottom() && dial.bottom() < b3.bottom(), "{dial:?}");
    assert!(dial.width() > 2.0 * b1.width());
    // button 3 bottom left, button 4 bottom right
    assert!(b3.center().x < dial.center().x && b4.center().x > dial.center().x);
    assert!((b3.center().y - b4.center().y).abs() < 2.0 && b3.top() > dial.center().y);
    for a in [b1, b2, roller, dial, b3, b4] {
        for b in [b1, b2, roller, dial, b3, b4] {
            if a != b {
                assert!(!a.shrink(1.0).intersects(b.shrink(1.0)), "controls don't overlap: {a:?} {b:?}");
            }
        }
    }
}

#[test]
fn the_actions_ring_takes_actions_dragged_onto_its_slots() {
    let mut h = demo();
    assert_eq!(h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": "controls"}}), T)["ok"], true);
    // the drawn device and its editor: Edit Keys and Dials… in Settings ▸ Controls
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:surfaceEditKeys"}), T)["ok"], true);
    h.settle(T);
    h.settle(T);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:surfaceDevice-ring"}), T)["ok"], true);
    h.settle(T);
    // eight slots around a ring, with the built-in actions
    let slots: Vec<egui::Rect> = (1..=8).map(|i| widget(&h, &format!("button:surfaceControl-ring.{i}"))).collect();
    let centre = slots.iter().fold(egui::Vec2::ZERO, |a, r| a + r.center().to_vec2()) / 8.0;
    let radii: Vec<f32> = slots.iter().map(|r| (r.center().to_vec2() - centre).length()).collect();
    assert!(radii.iter().all(|r| (r - radii[0]).abs() < 2.0 && *r > 40.0), "on a circle: {radii:?}");
    assert!(slots[0].center().y < centre.y - 30.0, "slot 1 at the top");
    let base = |h: &Headless, c: &str| h.app.surface.settings().active_profile().unwrap().base.get(c).cloned();
    assert_eq!(base(&h, "ring.1"), Some(lightcraft_surface::Binding::command("edit.undo")), "zero setup: the ring has actions");
    // drag "Next photo"-like action from the list onto slot 3
    h.app.surface.editor.filter = "Highlights".into();
    h.settle(T);
    let item = "item:surfaceAction-v.light.highlights";
    let to = widget(&h, "button:surfaceControl-ring.3").center();
    let r = h.request("ui.dragWidget", json!({"id": item, "toX": to.x, "toY": to.y, "steps": 12}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
    assert_eq!(base(&h, "ring.3"), Some(lightcraft_surface::Binding::value("light.highlights")), "dropped on slot 3");
    assert_eq!(h.app.surface.editor.control.as_deref(), Some("ring.3"), "and selected");
    // the plugin sees the ring's slots in the layout
    let layout = serde_json::to_value(crate::surface::layout(&h.app, &lightcraft_surface::devices::MX_CREATIVE_CONSOLE)).unwrap();
    let buttons = layout["buttons"].as_array().cloned().unwrap_or_default();
    assert!(buttons.iter().any(|b| b["control"] == "ring.3" && b["binding"]["target"] == "light.highlights"), "{layout}");
}

#[test]
fn a_dial_refuses_a_one_press_action_dropped_on_it() {
    let mut h = demo();
    assert_eq!(h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": "controls"}}), T)["ok"], true);
    // the drawn device and its editor: Edit Keys and Dials… in Settings ▸ Controls
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:surfaceEditKeys"}), T)["ok"], true);
    h.settle(T);
    h.app.surface.editor.device = "dialpad".into();
    h.app.surface.editor.control = Some("pad.1".into());
    h.app.surface.editor.filter = "edit.undo".into();
    h.settle(T);
    let before = h.app.surface.settings().clone();
    let key = lightcraft_surface::catalog::key_of(&lightcraft_surface::Binding::command("edit.undo"));
    let to = widget(&h, "button:surfaceControl-dial").center();
    let r = h.request("ui.dragWidget", json!({"id": format!("item:surfaceAction-{key}"), "toX": to.x, "toY": to.y, "steps": 12}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
    assert_eq!(*h.app.surface.settings(), before, "nothing changed");
    assert!(h.app.surface.editor.message.as_deref().is_some_and(|m| m.contains("dial")), "{:?}", h.app.surface.editor.message);
}

/// Profiles are found by name: Duplicate twice makes two names, a typed name that is taken gets a
/// number, and deleting the active profile makes another one active instead of leaving none.
#[test]
fn duplicated_profiles_get_their_own_names_and_deleting_one_keeps_one_active() {
    use crate::panels::controls::{delete_profile, duplicate_profile, unique_name};
    let mut s = lightcraft_surface::SurfaceSettings::default();
    let first = s.profile.clone();
    assert!(duplicate_profile(&mut s, ""));
    assert!(duplicate_profile(&mut s, ""));
    let names: Vec<&str> = s.profiles.iter().map(|p| p.name.as_str()).collect();
    let unique: std::collections::BTreeSet<&str> = names.iter().copied().collect();
    assert_eq!(unique.len(), names.len(), "{names:?}");
    assert!(duplicate_profile(&mut s, &first));
    assert_eq!(s.profile, format!("{first} 2"));
    assert!(s.active_profile().is_some());
    delete_profile(&mut s);
    assert!(s.active_profile().is_some(), "another profile is active: {}", s.profile);
    while s.profiles.len() > 1 {
        delete_profile(&mut s);
    }
    delete_profile(&mut s);
    assert_eq!(s.profiles.len(), 1, "the last profile stays");
    assert_eq!(unique_name("Page 1", |n| n == "Page 1" || n == "Page 1 2"), "Page 1 3");
}
