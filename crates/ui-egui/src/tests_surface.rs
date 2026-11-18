//! Control surfaces in the app (`crate::surface`, Settings ▸ Controls, the on-screen readout).
//!
//! Scenarios, as a photographer with an MX Creative Console sees them:
//!
//! * The plugin asks to pair; the app says so; I check the code and approve it in Settings ▸
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
use crate::{LccApp, Services};

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
        // a 1:1 dial feel and no per-value tuning: these tests count steps, not the shipped feel
        let motion = lcc_surface::settings::MotionSettings { speed: 1.0, acceleration: 1.0, ..Default::default() };
        let s = lcc_surface::SurfaceSettings { enabled: true, motion, tuning: Default::default(), ..Default::default() };
        store.save(&s.to_json()).unwrap();
    }
    let services = Services { png: None, surface_store: Some(Box::new(store)), ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
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
    // the roller straightens (the shipped layout keeps Zoom on the dial)
    assert_eq!(input(&mut h, json!([{"control": "roller", "ticks": 50}]))["ok"], true);
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
    // Touch to find (on while this page is open): touch key 5 on the surface: it is selected,
    // and nothing runs
    let clip = h.app.widgets.iter().find(|(w, _)| w == "check:surface.touchFind").map(|(_, r)| *r).unwrap();
    assert!(clip.bottom() < 900.0, "Touch to find is on screen: {clip:?}");
    assert!(h.app.surface.runtime.learning());
    let before = h.app.session.active();
    assert_eq!(input(&mut h, json!([{"control": "key.5", "press": true}]))["ok"], true);
    h.settle(T);
    assert_eq!(h.app.surface.editor.control.as_deref(), Some("key.5"));
    assert_eq!(h.app.session.active(), before, "a learnt press doesn't act");
    assert!(h.app.widgets.iter().any(|(w, _)| w == "field:surfaceActionSearch"), "the selected key's editor");
    // touching a dial switches the drawing to the Dialpad, and the photo doesn't change
    let exposure_before = exposure(&h);
    assert_eq!(input(&mut h, json!([{"control": "dial", "ticks": 1}]))["ok"], true);
    assert_eq!(input(&mut h, json!([{"control": "dial", "ticks": 3}]))["ok"], true);
    h.settle(T);
    assert_eq!(h.app.surface.editor.device, "dialpad");
    assert_eq!(h.app.surface.editor.control.as_deref(), Some("dial"));
    assert_eq!(exposure(&h), exposure_before, "a touched dial is found, not turned");
    assert!(h.app.widgets.iter().any(|(w, _)| w == "button:surfaceControl-dial"), "the Dialpad is drawn");
    // pick an action from the list: search, click
    h.app.surface.editor.filter = "Highlights".into();
    h.settle(T);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "item:surfaceAction-v.light.highlights"}), T)["ok"], true);
    h.settle(T);
    let p = h.app.surface.settings().active_profile().unwrap().clone();
    assert!(
        p.pages.iter().any(|pg| pg.bindings.get("dial") == Some(&lcc_surface::Binding::value("light.highlights"))),
        "the dial moves Highlights now"
    );
    // the other sections open
    for s in ["pages", "feel", "readout", "setup", "layout"] {
        assert_eq!(h.request("ui.clickWidget", json!({"id": format!("button:surfaceSection-{s}")}), T)["ok"], true);
        h.settle(T);
    }
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:surfaceDevice-keypad"}), T)["ok"], true);
    h.settle(T);
    // make key 5 put Contrast on the dial
    let profile = h.app.surface.settings().profile.clone();
    let mut s = h.app.surface.settings().clone();
    let page = s.profiles.iter_mut().find(|p| p.name == profile).unwrap().pages.iter_mut().find(|p| p.name == "Page 1").unwrap();
    page.bindings.insert("key.5".into(), lcc_surface::Binding::value("light.contrast"));
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
    assert_eq!(input(&mut h, json!([{"control": "key.5", "press": true}, {"control": "dial", "ticks": 4}]))["ok"], true);
    finish_gesture(&mut h);
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
    let dir = lcc_testkit::TempDir::new("lc-surface-tb");
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
    let mut h = Headless::new(LccApp::new(lcc_engine::Session::with_demo(), services), [1000.0, 700.0], 1.0);
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

/// A Logi Options+ install with the the app plugin, in memory: its data folder's token and port.
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
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.settle(T);
    open_photo(&mut h);
    assert!(h.app.surface.settings().enabled, "on by itself");
    let token = logi.token.lock().unwrap().clone().expect("the plugin got a token");
    assert_eq!(*logi.port.lock().unwrap(), Some(lcc_surface::settings::DEFAULT_PORT));
    for _ in 0..40 {
        h.step();
    }
    assert!(store.load().unwrap().unwrap().contains("logi-plugin"), "trusted and saved");
    // the plugin connects with it: paired at once, no code to approve
    let (s, events) = sink();
    let r = request_on(&mut h, 31, &s, "surface.hello", json!({"protocol": 1, "client": "logi-plugin", "token": token}));
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
    h.app.surface.editor.page = Some("Page 2".into());
    h.settle(T);
    assert_eq!(h.app.surface.runtime.pages().get(&lcc_surface::PageGroup::Keys).map(String::as_str), Some("Page 2"));
    let pushed: Vec<Value> = events.try_iter().collect();
    let last = pushed.iter().rev().find(|e| e["event"] == "surface.layout").expect("pushed");
    assert_eq!(last["layout"]["keyPage"], "Page 2", "the console follows the editor");
    // turned off by the user: stays off on the next start
    assert_eq!(h.request("ui.clickWidget", json!({"id": "check:surface.enabled"}), T)["ok"], true);
    h.settle(T);
    assert!(!h.app.surface.settings().enabled && h.app.surface.settings().off_by_user);
    crate::surface_plugin::auto_setup(&mut h.app);
    assert!(!h.app.surface.settings().enabled, "off stays off");
}

#[test]
fn white_balance_on_several_photos_moves_each_from_its_own_value() {
    // the console's dial with Apply to Selected on photos whose white balance differs: each is
    // moved by the turn on its own scale (as Quick Develop does), never given the active
    // photo's Kelvin; Tint and the other values the same way; one undo step
    let mut h = demo();
    open_photo(&mut h);
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(3).map(|p| p.0).collect();
    let wb = |h: &Headless, id: u64| {
        let d = h.app.session.develop_of(lcc_catalog::PhotoId(id)).unwrap();
        (d.wb.temp, d.wb.tint, d.light.contrast)
    };
    // different white balances (and one left As Shot)
    for (id, temp, tint) in [(ids[1], 4200.0, 12.0), (ids[2], 9000.0, -20.0)] {
        let r =
            h.request("engine.execute", json!({"command": "develop.set", "params": {"ids": [id], "values": {"wb.temp": temp, "wb.tint": tint}}}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    assert_eq!(h.request("engine.execute", json!({"command": "library.select", "params": {"ids": ids}}), T)["ok"], true);
    h.settle(T);
    assert_eq!(h.request("engine.execute", json!({"command": "surface.applyToSelected", "params": {"on": true}}), T)["ok"], true);
    let before: Vec<_> = ids.iter().map(|i| wb(&h, *i)).collect();
    let (s, _rx) = sink();
    for (action, ticks) in [("v.wb.temp", 6), ("v.wb.tint", 4), ("v.light.contrast", 5)] {
        let r = request_on(&mut h, 0, &s, "surface.input", json!({"events": [{"action": action, "ticks": ticks}]}));
        assert_eq!(r["ok"], true, "{r}");
        // live: the other photos move during the turn, not after it
        for _ in 0..3 {
            h.step();
        }
        if action == "v.wb.temp" {
            assert!(wb(&h, ids[1]).0 > before[1].0, "photo 2 follows the dial while it turns");
            assert!(h.app.session.interaction.is_some(), "still the same gesture");
        }
        finish_gesture(&mut h);
    }
    let after: Vec<_> = ids.iter().map(|i| wb(&h, *i)).collect();
    // the two Custom photos keep their difference: moved by the same amount, not set to one value
    let d1 = (after[1].0 - before[1].0, after[1].1 - before[1].1, after[1].2 - before[1].2);
    let d2 = (after[2].0 - before[2].0, after[2].1 - before[2].1, after[2].2 - before[2].2);
    assert!(d1.0 > 0.0 && (d1.0 - d2.0).abs() < 1e-6, "temp moved by the same amount: {d1:?} {d2:?}");
    assert!(d1.1 > 0.0 && (d1.1 - d2.1).abs() < 1e-6, "tint too: {d1:?} {d2:?}");
    assert!(d1.2 > 0.0 && (d1.2 - d2.2).abs() < 1e-6, "contrast too: {d1:?} {d2:?}");
    assert!((after[1].0 - after[2].0).abs() > 4000.0, "not copied from one photo: {after:?}");
    // the active photo moved too
    assert!(after[0] != before[0]);
    // one undo step per turn: three undos put everything back
    for _ in 0..3 {
        assert_eq!(h.request("engine.execute", json!({"command": "edit.undo"}), T)["ok"], true);
    }
    let back: Vec<_> = ids.iter().map(|i| wb(&h, *i)).collect();
    assert_eq!(back[1], before[1]);
    assert_eq!(back[2], before[2]);
}

/// Bind `control` on the key page now showing.
fn bind_key(h: &mut Headless, control: &str, b: lcc_surface::Binding) {
    let page = h.app.surface.runtime.pages().get(&lcc_surface::PageGroup::Keys).cloned().unwrap();
    let s = h.app.surface.runtime.settings_mut();
    let cur = s.profile.clone();
    let p = s.profiles.iter_mut().find(|p| p.name == cur).unwrap();
    p.pages.iter_mut().find(|q| q.name == page).unwrap().bindings.insert(control.into(), b);
}

#[test]
fn console_erase_works_while_held_and_delete_needs_a_hold() {
    use lcc_surface::Binding;
    let mut h = demo();
    open_photo(&mut h);
    bind_key(&mut h, "key.1", Binding::command("brush.erase"));
    // Erase: on while the key is down (the brush comes up with it), off when it is let go
    assert_eq!(input(&mut h, json!([{"control": "key.1", "press": true}]))["ok"], true);
    h.settle(T);
    assert!(h.app.ui.brush_erase, "erasing while held");
    assert_eq!(h.app.ui.tool, "brush");
    assert_eq!(input(&mut h, json!([{"control": "key.1", "press": false}]))["ok"], true);
    h.settle(T);
    assert!(!h.app.ui.brush_erase, "back to painting when let go");
    // New Brush runs once and is never lit (on the page the brush brings up: Brush, note 24 of 2026-10-24)
    assert_eq!(h.app.surface.runtime.context(), "masking.mask.brush");
    bind_key(&mut h, "key.4", Binding::command_with("tool.brush", json!({"new": true})));
    assert_eq!(input(&mut h, json!([{"control": "key.4", "press": true}, {"control": "key.4", "press": false}]))["ok"], true);
    h.settle(T);
    let layout = serde_json::to_value(crate::surface::layout(&h.app, &lcc_surface::devices::MX_CREATIVE_CONSOLE)).unwrap();
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
    let dir = lcc_testkit::TempDir::new("lc-surface-ld");
    let mut h = demo();
    open_photo(&mut h);
    // no setup: the knob labelled Exposure moves Exposure
    let before = exposure(&h);
    assert_eq!(input(&mut h, json!([{"control": "ld.r33", "ticks": 20}]))["ok"], true);
    finish_gesture(&mut h);
    assert!(exposure(&h) > before, "the Exposure knob");
    // a Loupedeck profile: that knob now moves Temperature, Undo stays on its button
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
        assert_eq!(p.base.get("ld.r33"), Some(&lcc_surface::Binding::value("wb.temp")), "{}", p.name);
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
fn the_ring_takes_dragged_actions_and_a_dial_refuses_a_one_press_action() {
    let mut h = demo();
    assert_eq!(h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": "controls"}}), T)["ok"], true);
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
    assert_eq!(base(&h, "ring.1"), Some(lcc_surface::Binding::command("edit.undo")), "zero setup: the ring has actions");
    // drag "Next photo"-like action from the list onto slot 3
    h.app.surface.editor.filter = "Highlights".into();
    h.settle(T);
    let item = "item:surfaceAction-v.light.highlights";
    let to = widget(&h, "button:surfaceControl-ring.3").center();
    let r = h.request("ui.dragWidget", json!({"id": item, "toX": to.x, "toY": to.y, "steps": 12}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
    assert_eq!(base(&h, "ring.3"), Some(lcc_surface::Binding::value("light.highlights")), "dropped on slot 3");
    assert_eq!(h.app.surface.editor.control.as_deref(), Some("ring.3"), "and selected");
    // the plugin sees the ring's slots in the layout
    let layout = serde_json::to_value(crate::surface::layout(&h.app, &lcc_surface::devices::MX_CREATIVE_CONSOLE)).unwrap();
    let buttons = layout["buttons"].as_array().cloned().unwrap_or_default();
    assert!(buttons.iter().any(|b| b["control"] == "ring.3" && b["binding"]["target"] == "light.highlights"), "{layout}");
    // a dial refuses a one-press action dropped on it
    h.app.surface.editor.device = "dialpad".into();
    h.app.surface.editor.control = Some("pad.1".into());
    h.app.surface.editor.filter = "edit.undo".into();
    h.settle(T);
    let before = h.app.surface.settings().clone();
    let key = lcc_surface::catalog::key_of(&lcc_surface::Binding::command("edit.undo"));
    let to = widget(&h, "button:surfaceControl-dial").center();
    let r = h.request("ui.dragWidget", json!({"id": format!("item:surfaceAction-{key}"), "toX": to.x, "toY": to.y, "steps": 12}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
    assert_eq!(*h.app.surface.settings(), before, "nothing changed");
    assert!(h.app.surface.editor.message.as_deref().is_some_and(|m| m.contains("dial")), "{:?}", h.app.surface.editor.message);
}

/// Client note 21: a preset on a key through the shared picker, labelled with the preset's name and
/// drawn with the preset icon (with the AI mark for an AI-mask preset); parameters an action
/// doesn't take are flagged.
#[test]
fn a_preset_goes_on_a_key_through_the_picker() {
    use lcc_surface::Binding;
    let mut h = demo();
    open_photo(&mut h);
    assert_eq!(h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": "controls"}}), T)["ok"], true);
    let click = |h: &mut Headless, id: &str| {
        let r = h.request("ui.clickWidget", json!({"id": id}), T);
        assert_eq!(r["ok"], true, "{id}: {r}");
        h.settle(T);
    };
    h.settle(T);
    click(&mut h, "button:surfaceControl-key.1");
    click(&mut h, "combo:surfacePreset");
    let id = h.app.session.presets.iter().find(|p| p.builtin).unwrap().id.clone();
    click(&mut h, &format!("button:surfacePreset-{id}"));
    let page = h.app.surface.runtime.pages().get(&lcc_surface::PageGroup::Keys).cloned().unwrap();
    let bound = h.app.surface.settings().active_profile().and_then(|p| p.page(&page).and_then(|q| q.bindings.get("key.1").cloned()));
    assert_eq!(bound, Some(Binding::command_with("preset.apply", json!({"id": id}))));
    let b = bound.unwrap();
    let name = crate::surface::preset_label(&h.app, &id);
    assert_eq!(crate::surface::preset_key_label(&h.app, &b), Some(name));
    assert_eq!(crate::surface::preset_key_icon(&h.app, &b), Some("preset-apply"));
    // a preset id pasted on another action is flagged
    bind_key(&mut h, "key.1", Binding::command_with("develop.autoSync", json!({"id": id})));
    h.settle(T);
    assert!(crate::panels::controls::unused_params_for_test("develop.autoSync", &json!({"id": "x"})) == vec!["id".to_string()]);
    assert!(crate::panels::controls::unused_params_for_test("preset.apply", &json!({"id": "x", "amount": 50})).is_empty());
}

/// Test notes 2026-10-25 note 1: loading a format 3 file merges its two Masks panel pages and says so once as a
/// toast in plain words; the "Settings file problem" window is for problems only, and nothing was wrong.
#[test]
fn an_old_file_merging_its_mask_pages_says_so_in_a_toast_not_the_problem_window() {
    let mut v = serde_json::to_value(lcc_surface::SurfaceSettings { enabled: true, ..Default::default() }).unwrap();
    v["version"] = json!(3);
    v["profiles"][0]["contexts"]["masking.mask"] = json!(["Mask"]);
    let store = MemStore::default();
    store.save(&v.to_string()).unwrap();
    let services = Services { png: None, surface_store: Some(Box::new(store.clone())), ..Default::default() };
    let mut h = Headless::new(LccApp::new(lcc_engine::Session::with_demo(), services), [1400.0, 900.0], 1.0);
    for _ in 0..3 {
        h.step();
    }
    assert!(h.app.notices.is_empty(), "no problem window: {:?}", h.app.notices);
    assert!(!h.painted_text().iter().any(|t| t.contains("Settings file problem")), "{:?}", h.painted_text());
    assert!(h.painted_text().iter().any(|t| t.contains("Console: Masking and Mask are now one Masks page")), "{:?}", h.painted_text());
    assert!(h.app.surface.load_error.is_none());
    // the save runs in the background: under a loaded machine it can take longer than one settle
    let saved = || store.load().unwrap().is_some_and(|t| t.contains("\"version\": 4") || t.contains("\"version\":4"));
    let deadline = Instant::now() + Duration::from_secs(60);
    while !saved() && Instant::now() < deadline {
        h.settle(T);
    }
    assert!(saved(), "saved in the new form");
}
