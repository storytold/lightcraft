//! Pixel-filter plug-ins end to end through engine commands: install, list, add at both stages,
//! render, undo, missing plug-in, analyze without `pc_analyze`.

use serde_json::{Value, json};

use crate::Session;

pub(crate) fn invert_module(id: &str) -> Vec<u8> {
    let manifest = format!(
        r#"{{"id":"{id}","name":"Invert","version":"1","kind":"filter","params":{{"amount":{{"type":"number","min":0,"max":1,"default":1}}}}}}"#
    );
    module(&manifest, None)
}

/// A filter module with `manifest`; `analysis`: it also exports `pc_analyze` returning that JSON.
pub(crate) fn module(manifest: &str, analysis: Option<&str>) -> Vec<u8> {
    module_with_merge(manifest, analysis, None)
}

/// [`module`], and with `merged` it also exports `pc_merge` returning that JSON.
pub(crate) fn module_with_merge(manifest: &str, analysis: Option<&str>, merged: Option<&str>) -> Vec<u8> {
    let export = |name: &str, at: usize, json: Option<&str>| match json {
        Some(a) => format!(
            r#"(data (i32.const {at}) "{a}")
  (func (export "{name}") (param i32 i32 i32 i32 i32 i32 i32 i32) (result i64) (i64.or (i64.shl (i64.const {len}) (i64.const 32)) (i64.const {at})))"#,
            a = a.replace('"', "\\\""),
            len = a.len()
        ),
        None => String::new(),
    };
    let analyze = format!("{}\n  {}", export("pc_analyze", 8192, analysis), export("pc_merge", 12288, merged));
    let src = format!(
        r#"(module
  (memory (export "memory") 1)
  (data (i32.const 16) "{m}")
  (func (export "pc_abi_version") (result i32) (i32.const 1))
  (func (export "pc_manifest") (result i64) (i64.or (i64.shl (i64.const {len}) (i64.const 32)) (i64.const 16)))
  (func (export "pc_alloc") (param $n i32) (result i32) (local $old i32)
    (local.set $old (memory.grow (i32.shr_u (i32.add (local.get $n) (i32.const 65535)) (i32.const 16))))
    (if (result i32) (i32.eq (local.get $old) (i32.const -1)) (then (i32.const 0)) (else (i32.mul (local.get $old) (i32.const 65536)))))
  (func (export "pc_filter") (param $buf i32) (param $len i32) (param i32 i32 i32 i32 i32 i32) (result i32) (local $i i32) (local $p i32)
    (block $d (loop $l
      (br_if $d (i32.ge_u (local.get $i) (local.get $len)))
      (local.set $p (i32.add (local.get $buf) (local.get $i)))
      (f32.store (local.get $p) (f32.sub (f32.const 1) (f32.load (local.get $p))))
      (local.set $i (i32.add (local.get $i) (i32.const 4)))
      (br $l)))
    (i32.const 0))
  {analyze})"#,
        m = manifest.replace('"', "\\\""),
        len = manifest.len()
    );
    wat::parse_str(&src).unwrap()
}

fn install(s: &mut Session, id: &str) -> Value {
    install_bytes(s, id, &invert_module(id))
}

fn install_bytes(s: &mut Session, id: &str, bytes: &[u8]) -> Value {
    let dir = std::env::temp_dir().join(format!("lcc-filter-test-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("f.wasm");
    std::fs::write(&path, bytes).unwrap();
    let r = s.execute("plugin.install", &json!({"path": path})).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    r
}

fn render(s: &mut Session) -> lcc_pipeline::Rendered {
    let id = s.active().unwrap();
    let src = s.source_now(id, crate::media::SourceLevel::Thumb).unwrap();
    let info = s.source_info(id);
    let d = s.develop_of(id).unwrap();
    lcc_pipeline::render(&src, &info, &d, &lcc_pipeline::RenderRequest::fit(64, 64))
}

fn mean(i: &lcc_raster::Rgba8) -> f64 {
    i.data.iter().map(|p| (p[0] as f64 + p[1] as f64 + p[2] as f64) / 3.0).sum::<f64>() / i.data.len() as f64
}

#[test]
fn install_list_add_render_undo() {
    let mut s = Session::with_demo();
    let info = install(&mut s, "test.engine.invert");
    assert_eq!(info["kind"], "filter");
    let listed = s.execute("plugin.list", &json!({})).unwrap();
    let f = listed["plugins"].as_array().unwrap().iter().find(|p| p["id"] == "test.engine.invert").unwrap().clone();
    assert_eq!(f["kind"], "filter");
    assert!(f["params"]["amount"].is_object());
    assert_eq!(f["analyze"], false);
    assert!(f["moduleHash"].as_str().unwrap().len() == 64);

    let before = render(&mut s);
    let r = s.execute("develop.pluginFilter.add", &json!({"plugin": "test.engine.invert", "stage": "display"})).unwrap();
    let uid = r["uid"].as_u64().unwrap();
    let stack = s.execute("develop.pluginFilter.list", &json!({})).unwrap();
    assert_eq!(stack["filters"][0]["params"]["amount"], 1.0, "defaults filled from the manifest");
    let after = render(&mut s);
    assert!(after.warnings.is_empty(), "{:?}", after.warnings);
    assert!((mean(&before.image) + mean(&after.image) - 255.0).abs() < 2.0, "display invert");
    // analysis is unsupported for this module
    assert!(s.execute("develop.pluginFilter.analyze", &json!({"uid": uid})).is_err());
    // a wrong value type is refused by the manifest
    assert!(s.execute("develop.pluginFilter.add", &json!({"plugin": "test.engine.invert", "params": {"amount": "x"}})).is_err());
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(render(&mut s).image.data, before.image.data, "undo removes the effect");

    // Source stage: the develop pipeline acts on the inverted source
    s.execute("develop.pluginFilter.add", &json!({"plugin": "test.engine.invert", "stage": "source"})).unwrap();
    let src = render(&mut s);
    assert!(src.warnings.is_empty());
    assert_ne!(src.image.data, before.image.data);

    // uninstalled: renders unfiltered with a warning
    s.execute("plugin.uninstall", &json!({"id": "test.engine.invert"})).unwrap();
    let gone = render(&mut s);
    assert!(!gone.warnings.is_empty());
    assert_eq!(gone.image.data, before.image.data);
}

/// `plugin.list` gives a filter its module's size, and `plugin.enable` on one says filters can't be
/// turned off (not that it isn't installed).
#[test]
fn a_filter_lists_its_size_and_cant_be_turned_off() {
    let mut s = Session::with_demo();
    let bytes = invert_module("test.engine.size");
    install_bytes(&mut s, "test.engine.size", &bytes);
    let listed = s.execute("plugin.list", &json!({})).unwrap();
    let f = listed["plugins"].as_array().unwrap().iter().find(|p| p["id"] == "test.engine.size").unwrap().clone();
    assert_eq!(f["size"], json!(bytes.len()), "the module's size in bytes");
    let e = s.execute("plugin.enable", &json!({"id": "test.engine.size", "enabled": false})).unwrap_err().to_string();
    assert!(e.contains("can't be turned off"), "says why: {e}");
    assert!(!e.contains("is installed"), "not that it isn't installed: {e}");
    s.execute("plugin.uninstall", &json!({"id": "test.engine.size"})).unwrap();
}

#[test]
fn a_filters_pictures_are_listed_and_served() {
    let mut s = Session::with_demo();
    let id = "test.engine.pictures";
    let svg = "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 4 4'><circle cx='2' cy='2' r='2'/></svg>";
    let light = svg.replace("circle", "ellipse rx='2' ry='1' ");
    let manifest = format!(
        r#"{{"id":"{id}","name":"P","version":"1","kind":"filter","icon":"{svg}","header":{{"dark":"{svg}","light":"{light}"}},"params":{{}}}}"#
    );
    let info = install_bytes(&mut s, id, &module(&manifest, None));
    assert_eq!(info["icon"], true);
    assert_eq!(info["header"], true);
    let listed = s.execute("plugin.list", &json!({})).unwrap();
    let row = listed["plugins"].as_array().unwrap().iter().find(|p| p["id"] == id).unwrap();
    assert_eq!(row["icon"], true);
    assert_eq!(s.execute("plugin.picture", &json!({"id": id, "picture": "icon"})).unwrap()["svg"], svg);
    assert_eq!(s.execute("plugin.picture", &json!({"id": id})).unwrap()["svg"], svg, "the header, dark by default");
    assert_eq!(s.execute("plugin.picture", &json!({"id": id, "theme": "light"})).unwrap()["svg"], light);
    assert!(s.execute("plugin.picture", &json!({"id": id, "picture": "poster"})).is_err());
    // a filter without pictures lists them as absent and serves none
    let plain = install(&mut s, "test.engine.plain");
    assert_eq!(plain["icon"], false);
    assert_eq!(s.execute("plugin.picture", &json!({"id": "test.engine.plain", "picture": "icon"})).unwrap()["svg"], Value::Null);
    assert!(s.execute("plugin.picture", &json!({"id": "test.engine.nowhere"})).is_err());
}

#[test]
fn plugin_list_keeps_manifest_param_order_and_labels() {
    let mut s = Session::with_demo();
    let m = r#"{"id":"test.engine.order","name":"Order","kind":"filter","params":{"zeta":{"type":"number","min":0,"max":1,"default":0},"dynamic_range":{"type":"number","min":0,"max":1,"default":0},"alpha":{"type":"bool","label":"First letter"}}}"#;
    install_bytes(&mut s, "test.engine.order", &module(m, None));
    let listed = s.execute("plugin.list", &json!({})).unwrap();
    let f = listed["plugins"].as_array().unwrap().iter().find(|p| p["id"] == "test.engine.order").unwrap().clone();
    assert_eq!(f["paramOrder"], json!(["zeta", "dynamic_range", "alpha"]));
    assert_eq!(f["params"]["dynamic_range"]["label"], "Dynamic range");
    assert_eq!(f["params"]["alpha"]["label"], "First letter");
    assert_eq!(f["stage"], "display");
    s.execute("plugin.uninstall", &json!({"id": "test.engine.order"})).unwrap();
}

#[test]
fn add_without_stage_uses_the_plugins_stage() {
    let mut s = Session::with_demo();
    let m = r#"{"id":"test.engine.src","name":"Src","kind":"filter","stage":"source"}"#;
    install_bytes(&mut s, "test.engine.src", &module(m, None));
    // the built-in hint table covers plug-ins that don't declare one yet
    let hinted = r#"{"id":"io.github.smyrgeorge.film-lab.convert","name":"Film","kind":"filter"}"#;
    install_bytes(&mut s, "io.github.smyrgeorge.film-lab.convert", &module(hinted, None));
    install(&mut s, "test.engine.plain");
    for (id, want) in [("test.engine.src", "source"), ("io.github.smyrgeorge.film-lab.convert", "source"), ("test.engine.plain", "display")] {
        s.execute("develop.pluginFilter.add", &json!({"plugin": id})).unwrap();
        let st = s.execute("develop.pluginFilter.list", &json!({})).unwrap();
        assert_eq!(st["filters"].as_array().unwrap().last().unwrap()["stage"], want, "{id}");
    }
    // an explicit stage wins
    s.execute("develop.pluginFilter.add", &json!({"plugin": "test.engine.src", "stage": "display"})).unwrap();
    let st = s.execute("develop.pluginFilter.list", &json!({})).unwrap();
    assert_eq!(st["filters"].as_array().unwrap().last().unwrap()["stage"], "display");
    for id in ["test.engine.src", "io.github.smyrgeorge.film-lab.convert", "test.engine.plain"] {
        s.execute("plugin.uninstall", &json!({"id": id})).unwrap();
    }
}

#[test]
fn render_warnings_name_their_entry() {
    let mut s = Session::with_demo();
    s.execute("develop.pluginFilter.add", &json!({"plugin": "test.engine.absent", "module_hash": "ab", "name": "Gone"})).unwrap();
    let r = render(&mut s);
    let f = s.develop_of(s.active().unwrap()).unwrap().plugin_filters[0].clone();
    assert!(r.warnings.iter().any(|w| lcc_pipeline::plugin_filters::warning_message(w, &f).is_some()), "{:?}", r.warnings);
}

fn pump_until_idle(s: &mut Session) {
    let start = std::time::Instant::now();
    while s.plugin_analyses_running() {
        assert!(start.elapsed() < std::time::Duration::from_secs(30), "measurement never finished");
        s.plugin_analyze_pump();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn analyze_runs_in_the_background_and_lands_one_history_step() {
    let mut s = Session::with_demo();
    let m = r#"{"id":"test.engine.measure","name":"Measure","kind":"filter"}"#;
    install_bytes(&mut s, "test.engine.measure", &module(m, Some(r#"{"base":[0.8,0.5,0.3]}"#)));
    let uid = s.execute("develop.pluginFilter.add", &json!({"plugin": "test.engine.measure"})).unwrap()["uid"].as_u64().unwrap();
    let u0 = s.undo.len();
    let r = s.execute("develop.pluginFilter.analyze", &json!({"uid": uid})).unwrap();
    assert_eq!(r["started"], true);
    assert!(s.plugin_analyzing(s.active().unwrap(), uid as u32));
    assert!(s.activity.list().iter().any(|t| t.kind == "pluginAnalyze"), "an activity row while it runs");
    pump_until_idle(&mut s);
    assert_eq!(s.undo.len(), u0 + 1, "one History step");
    let st = s.execute("develop.pluginFilter.list", &json!({})).unwrap();
    assert_eq!(st["filters"][0]["analysis"], json!({"base": [0.8, 0.5, 0.3]}));
    assert!(!s.activity.list().iter().any(|t| t.kind == "pluginAnalyze"), "the row is gone");

    // cancelled: nothing is stored
    s.execute("develop.pluginFilter.setAnalysis", &json!({"uid": uid, "analysis": null})).unwrap();
    let u1 = s.undo.len();
    s.execute("develop.pluginFilter.analyze", &json!({"uid": uid})).unwrap();
    let task = s.activity.list().into_iter().find(|t| t.kind == "pluginAnalyze").unwrap();
    s.activity.cancel(task.id).unwrap();
    pump_until_idle(&mut s);
    assert_eq!(s.undo.len(), u1, "a cancelled measurement adds no History step");
    let st = s.execute("develop.pluginFilter.list", &json!({})).unwrap();
    assert!(st["filters"][0]["analysis"].is_null());
    assert!(s.activity.list().is_empty() || !s.activity.list().iter().any(|t| t.kind == "pluginAnalyze"));

    // `wait` measures before returning
    let r = s.execute("develop.pluginFilter.analyze", &json!({"uid": uid, "wait": true})).unwrap();
    assert_eq!(r["analysis"]["base"][0], 0.8);
    s.execute("plugin.uninstall", &json!({"id": "test.engine.measure"})).unwrap();
}

#[test]
fn a_batch_measures_each_selected_photo_and_a_roll_shares_one_measurement() {
    let mut s = Session::with_demo();
    let m = r#"{"id":"test.engine.roll","name":"Roll","kind":"filter"}"#;
    install_bytes(&mut s, "test.engine.roll", &module_with_merge(m, Some(r#"{"base":[0.8,0.5,0.3]}"#), Some(r#"{"base":[0.7,0.5,0.3]}"#)));
    let info = s.execute("plugin.list", &json!({})).unwrap();
    let me = info["plugins"].as_array().unwrap().iter().find(|p| p["id"] == "test.engine.roll").unwrap().clone();
    assert_eq!((me["analyze"].as_bool(), me["merge"].as_bool()), (Some(true), Some(true)));
    let ids: Vec<u64> = s.catalog.photos().take(3).map(|p| p.id.0).collect();
    assert_eq!(ids.len(), 3);
    s.execute("library.select", &json!({"ids": ids})).unwrap();
    let uid = s.execute("develop.pluginFilter.add", &json!({"plugin": "test.engine.roll", "params": {}})).unwrap()["uid"].as_u64().unwrap();
    let active = s.active().unwrap();
    assert_eq!(active.0, ids[0]);

    // the other photos have no entry yet: a batch gives them a copy of this one (without a
    // measurement) before measuring, so a roll needs no step before it
    s.execute("develop.pluginFilter.set", &json!({"uid": uid, "params": {"amount": 0.25}})).unwrap();
    let r = s.execute("develop.pluginFilter.analyze", &json!({"uid": uid, "ids": ids, "wait": true})).unwrap();
    assert_eq!(r["analyses"].as_array().unwrap().len(), 3);
    for id in &ids {
        let d = s.develop_of(crate::catalog::PhotoId(*id)).unwrap();
        assert_eq!(d.plugin_filters.len(), 1, "photo {id}");
        assert_eq!(d.plugin_filters[0].params["amount"], 0.25);
        assert_eq!(d.plugin_filters[0].analysis, Some(json!({"base": [0.8, 0.5, 0.3]})));
    }
    for id in &ids[1..] {
        s.execute("library.select", &json!({"ids": [id]})).unwrap();
        s.execute("develop.pluginFilter.remove", &json!({"uid": 1})).unwrap();
    }
    s.execute("library.select", &json!({"ids": ids})).unwrap();
    s.execute("develop.pluginFilter.setAnalysis", &json!({"uid": uid, "analysis": null})).unwrap();

    // Apply to Selected: the entry (parameters, stage, on/off) lands on the others, one History step
    s.execute("develop.pluginFilter.set", &json!({"uid": uid, "stage": "source", "enabled": false})).unwrap();
    let u0 = s.undo.len();
    let r = s.execute("develop.pluginFilter.applyTo", &json!({"uid": uid})).unwrap();
    assert_eq!(r["changed"], 2);
    assert_eq!(s.undo.len(), u0 + 1);
    for id in &ids[1..] {
        let d = s.develop_of(crate::catalog::PhotoId(*id)).unwrap();
        let e = d.plugin_filters.iter().find(|f| f.plugin == "test.engine.roll").expect("applied");
        assert!(!e.enabled && e.stage == lcc_develop::FilterStage::Source && e.analysis.is_none());
    }
    // applied again with a measurement: replaced in place, uid kept
    s.execute("develop.pluginFilter.setAnalysis", &json!({"uid": uid, "analysis": {"base": [1, 1, 1]}})).unwrap();
    s.execute("develop.pluginFilter.applyTo", &json!({"uid": uid, "ids": [ids[1]]})).unwrap();
    let d1 = s.develop_of(crate::catalog::PhotoId(ids[1])).unwrap();
    assert_eq!(d1.plugin_filters.len(), 1);
    assert_eq!(d1.plugin_filters[0].analysis, Some(json!({"base": [1, 1, 1]})));
    // …and without it, the photo's own measurement stays
    s.execute("develop.pluginFilter.applyTo", &json!({"uid": uid, "ids": [ids[1]], "measurement": false})).unwrap();
    let d1 = s.develop_of(crate::catalog::PhotoId(ids[1])).unwrap();
    assert_eq!(d1.plugin_filters[0].analysis, Some(json!({"base": [1, 1, 1]})));

    // a batch: each photo gets its own measurement (here the same module output)
    let r = s.execute("develop.pluginFilter.analyze", &json!({"uid": uid, "ids": ids})).unwrap();
    assert_eq!(r["started"], true);
    assert!(s.plugin_analyzing(crate::catalog::PhotoId(ids[2]), 1));
    assert!(s.activity.list().iter().any(|t| t.kind == "pluginAnalyze" && t.label.contains("3 photos")), "{:?}", s.activity.list());
    pump_until_idle(&mut s);
    for id in &ids {
        let d = s.develop_of(crate::catalog::PhotoId(*id)).unwrap();
        assert_eq!(d.plugin_filters[0].analysis, Some(json!({"base": [0.8, 0.5, 0.3]})), "photo {id}");
    }
    assert!(s.plugin_analyze_error(crate::catalog::PhotoId(ids[2]), 1).is_none());

    // a roll: one merged measurement on every photo, marked with how many it came from
    let r = s.execute("develop.pluginFilter.analyze", &json!({"uid": uid, "ids": ids, "roll": true, "wait": true})).unwrap();
    assert_eq!(r["analysis"], json!({"base": [0.7, 0.5, 0.3], "_roll": 3}));
    assert_eq!(r["analyses"].as_array().unwrap().len(), 3);
    for id in &ids {
        let d = s.develop_of(crate::catalog::PhotoId(*id)).unwrap();
        assert_eq!(d.plugin_filters[0].analysis.as_ref().unwrap()["_roll"], 3, "photo {id}");
    }
    // a cancelled roll stores nothing new
    s.execute("develop.pluginFilter.setAnalysis", &json!({"uid": uid, "analysis": null})).unwrap();
    s.execute("develop.pluginFilter.analyze", &json!({"uid": uid, "ids": ids, "roll": true})).unwrap();
    let task = s.activity.list().into_iter().find(|t| t.kind == "pluginAnalyze").unwrap();
    s.activity.cancel(task.id).unwrap();
    pump_until_idle(&mut s);
    assert!(s.develop_of(active).unwrap().plugin_filters[0].analysis.is_none());

    // a module without pc_merge can't do a roll
    let m2 = r#"{"id":"test.engine.noroll","name":"NoRoll","kind":"filter"}"#;
    install_bytes(&mut s, "test.engine.noroll", &module(m2, Some(r#"{"b":1}"#)));
    let uid2 = s.execute("develop.pluginFilter.add", &json!({"plugin": "test.engine.noroll"})).unwrap()["uid"].as_u64().unwrap();
    let err = s.execute("develop.pluginFilter.analyze", &json!({"uid": uid2, "roll": true})).unwrap_err().to_string();
    assert!(err.contains("unsupported"), "{err}");
    // hostile: ids that aren't photos
    assert!(s.execute("develop.pluginFilter.analyze", &json!({"uid": uid, "ids": [u64::MAX - 1]})).is_err());
    assert!(s.execute("develop.pluginFilter.analyze", &json!({"uid": uid, "ids": "3"})).is_err());
    s.execute("plugin.uninstall", &json!({"id": "test.engine.roll"})).unwrap();
    s.execute("plugin.uninstall", &json!({"id": "test.engine.noroll"})).unwrap();
}

#[test]
fn add_to_puts_a_default_entry_on_the_selected_photos_that_lack_one() {
    let mut s = Session::with_demo();
    let m = r#"{"id":"test.engine.addto","name":"AddTo","kind":"filter","stage":"source","shortcut":"Ctrl+Alt+N","params":{"amount":{"type":"number","min":0,"max":1,"default":0.4}}}"#;
    install_bytes(&mut s, "test.engine.addto", &module(m, None));
    let info = s.execute("plugin.list", &json!({})).unwrap();
    let me = info["plugins"].as_array().unwrap().iter().find(|p| p["id"] == "test.engine.addto").unwrap().clone();
    assert_eq!(me["shortcut"], "Ctrl+Alt+N");
    let ids: Vec<u64> = s.catalog.photos().take(3).map(|p| p.id.0).collect();
    s.execute("library.select", &json!({"ids": ids})).unwrap();
    // the active photo has one already (with its own value): kept; the other two get the defaults
    s.execute("develop.pluginFilter.add", &json!({"plugin": "test.engine.addto", "params": {"amount": 0.9}})).unwrap();
    let u0 = s.undo.len();
    let r = s.execute("develop.pluginFilter.addTo", &json!({"plugin": "test.engine.addto"})).unwrap();
    assert_eq!((r["added"].as_u64(), r["had"].as_u64()), (Some(2), Some(1)));
    assert_eq!(s.undo.len(), u0 + 1, "one History step for the batch");
    for (i, id) in ids.iter().enumerate() {
        let d = s.develop_of(crate::catalog::PhotoId(*id)).unwrap();
        let f = &d.plugin_filters[0];
        assert_eq!(f.plugin, "test.engine.addto");
        assert_eq!(f.stage, lcc_develop::FilterStage::Source);
        assert_eq!(f.params["amount"], if i == 0 { json!(0.9) } else { json!(0.4) }, "photo {id}");
    }
    // again: nothing to add, no History step
    let r = s.execute("develop.pluginFilter.addTo", &json!({"plugin": "test.engine.addto"})).unwrap();
    assert_eq!((r["added"].as_u64(), r["had"].as_u64()), (Some(0), Some(3)));
    assert_eq!(s.undo.len(), u0 + 1);
    // hostile: an unknown plug-in, ids that aren't photos
    assert!(s.execute("develop.pluginFilter.addTo", &json!({"plugin": "test.engine.nope"})).is_err());
    assert!(s.execute("develop.pluginFilter.addTo", &json!({"plugin": "test.engine.addto", "ids": "3"})).is_err());
    s.execute("plugin.uninstall", &json!({"id": "test.engine.addto"})).unwrap();
}

/// A photo may carry two entries of one plug-in: a measurement asked for the second one lands on
/// it, not on the first, on the active photo; the other photos' first entry of the plug-in is used.
#[test]
fn two_entries_of_one_plugin_measure_the_one_asked_for() {
    let mut s = Session::with_demo();
    let m = r#"{"id":"test.engine.twiceroll","name":"TwiceRoll","kind":"filter"}"#;
    install_bytes(&mut s, "test.engine.twiceroll", &module_with_merge(m, Some(r#"{"base":[0.8,0.5,0.3]}"#), Some(r#"{"base":[0.7,0.5,0.3]}"#)));
    let ids: Vec<u64> = s.catalog.photos().take(2).map(|p| p.id.0).collect();
    s.execute("library.select", &json!({"ids": ids})).unwrap();
    let u1 = s.execute("develop.pluginFilter.add", &json!({"plugin": "test.engine.twiceroll"})).unwrap()["uid"].as_u64().unwrap();
    let u2 = s.execute("develop.pluginFilter.add", &json!({"plugin": "test.engine.twiceroll"})).unwrap()["uid"].as_u64().unwrap();
    assert_ne!(u1, u2);
    let active = s.active().unwrap();
    // alone: the second entry is measured, the first stays unmeasured
    s.execute("develop.pluginFilter.analyze", &json!({"uid": u2})).unwrap();
    assert!(s.plugin_analyzing(active, u2 as u32) && !s.plugin_analyzing(active, u1 as u32));
    pump_until_idle(&mut s);
    let d = s.develop_of(active).unwrap();
    assert!(d.plugin_filters[0].analysis.is_none() && d.plugin_filters[1].analysis.is_some());
    // as a roll with another photo: the second entry here, the other photo's only one there
    let r = s.execute("develop.pluginFilter.analyze", &json!({"uid": u2, "ids": ids, "roll": true, "wait": true})).unwrap();
    assert_eq!(r["analyses"].as_array().unwrap().len(), 2);
    let d = s.develop_of(active).unwrap();
    assert!(d.plugin_filters[0].analysis.is_none());
    assert_eq!(d.plugin_filters[1].analysis.as_ref().unwrap()["_roll"], 2);
    let other = s.develop_of(crate::catalog::PhotoId(ids[1])).unwrap();
    assert_eq!(other.plugin_filters.len(), 1);
    assert_eq!(other.plugin_filters[0].analysis.as_ref().unwrap()["_roll"], 2);
    // a uid no entry has is refused
    assert!(s.execute("develop.pluginFilter.analyze", &json!({"uid": 99, "ids": ids})).is_err());
    s.execute("plugin.uninstall", &json!({"id": "test.engine.twiceroll"})).unwrap();
}

/// A filter whose `pc_analyze` echoes its `params` JSON (`_image` included) back to the caller.
fn echo_analyze_module(id: &str) -> Vec<u8> {
    let manifest = format!(r#"{{"id":"{id}","name":"Echo","version":"1","kind":"filter","params":{{}}}}"#);
    let src = format!(
        r#"(module
  (memory (export "memory") 1)
  (data (i32.const 16) "{m}")
  (func (export "pc_abi_version") (result i32) (i32.const 1))
  (func (export "pc_manifest") (result i64) (i64.or (i64.shl (i64.const {len}) (i64.const 32)) (i64.const 16)))
  (func (export "pc_alloc") (param $n i32) (result i32) (local $old i32)
    (local.set $old (memory.grow (i32.shr_u (i32.add (local.get $n) (i32.const 65535)) (i32.const 16))))
    (if (result i32) (i32.eq (local.get $old) (i32.const -1)) (then (i32.const 0)) (else (i32.mul (local.get $old) (i32.const 65536)))))
  (func (export "pc_filter") (param i32 i32 i32 i32 i32 i32 i32 i32) (result i32) (i32.const 0))
  (func (export "pc_analyze") (param i32 i32 i32 i32 i32 i32) (param $pp i32) (param $pl i32) (result i64)
    (i64.or (i64.shl (i64.extend_i32_u (local.get $pl)) (i64.const 32)) (i64.extend_i32_u (local.get $pp)))))"#,
        m = manifest.replace('"', "\\\""),
        len = manifest.len()
    );
    wat::parse_str(&src).unwrap()
}

#[test]
fn untagged_16bit_tiff_and_raw_propagate_source_provenance_to_filter_and_analyze() {
    use lcc_codecs::{EncodeImage, EncodeMeta, Samples, TiffCompression, encode_tiff};

    let px16 = vec![12_000u16; 16 * 16 * 3];
    let tiff16 = encode_tiff(&EncodeImage::new(16, 16, 3, Samples::U16(&px16)), TiffCompression::None, &EncodeMeta::default()).unwrap();
    let (_, info16) = crate::files::load_bytes(&tiff16, 256).unwrap();
    assert_eq!(info16.space_origin.as_deref(), Some("untagged"));
    assert_eq!(info16.bits_per_sample, Some(16));
    assert!(!info16.float);

    let px32 = vec![0.25f32; 8 * 8 * 3];
    let tiff32 = encode_tiff(&EncodeImage::new(8, 8, 3, Samples::F32(&px32)), TiffCompression::None, &EncodeMeta::default()).unwrap();
    let (_, info32) = crate::files::load_bytes(&tiff32, 256).unwrap();
    assert_eq!(info32.space_origin.as_deref(), Some("untagged"));
    assert_eq!(info32.bits_per_sample, Some(32));
    assert!(info32.float);

    let dng = crate::tests_xmp::synthetic_dng_with(None, lcc_meta::Metadata::default());
    let (_, info_raw) = crate::files::load_bytes(&dng, 256).unwrap();
    assert_eq!(info_raw.space_origin.as_deref(), Some("raw"));
    assert!(info_raw.bits_per_sample.is_some());
    assert!(!info_raw.float);

    // End-to-end through Session: import the untagged 16-bit TIFF and run pc_analyze.
    let dir = std::env::temp_dir().join(format!("lcc-filter-src-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("lib");
    let tiff_path = dir.join("scan.tif");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&tiff_path, &tiff16).unwrap();

    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let imp = s.execute("library.import", &json!({"paths": [tiff_path.to_string_lossy()]})).unwrap();
    let id = imp["imported"][0].as_u64().unwrap();
    s.execute("library.select", &json!({"ids": [id]})).unwrap();

    install_bytes(&mut s, "test.engine.echo", &echo_analyze_module("test.engine.echo"));
    let uid = s.execute("develop.pluginFilter.add", &json!({"plugin": "test.engine.echo", "stage": "source"})).unwrap()["uid"].as_u64().unwrap();
    let r = s.execute("develop.pluginFilter.analyze", &json!({"uid": uid, "wait": true})).unwrap();
    assert_eq!(r["analysis"]["_image"]["draft"], false);
    assert_eq!(r["analysis"]["_image"]["source"], json!({"origin": "untagged", "bitsPerSample": 16}));
    s.execute("plugin.uninstall", &json!({"id": "test.engine.echo"})).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
