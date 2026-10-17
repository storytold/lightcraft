//! Pixel-filter plug-ins end to end through engine commands: install, list, add at both stages,
//! render, undo, missing plug-in, analyze without `pc_analyze`.

use serde_json::{Value, json};

use crate::Session;

fn invert_module(id: &str) -> Vec<u8> {
    let manifest = format!(
        r#"{{"id":"{id}","name":"Invert","version":"1","kind":"filter","params":{{"amount":{{"type":"number","min":0,"max":1,"default":1}}}}}}"#
    );
    module(&manifest, None)
}

/// A filter module with `manifest`; `analysis`: it also exports `pc_analyze` returning that JSON.
fn module(manifest: &str, analysis: Option<&str>) -> Vec<u8> {
    let analyze = match analysis {
        Some(a) => format!(
            r#"(data (i32.const 8192) "{a}")
  (func (export "pc_analyze") (param i32 i32 i32 i32 i32 i32 i32 i32) (result i64) (i64.or (i64.shl (i64.const {len}) (i64.const 32)) (i64.const 8192)))"#,
            a = a.replace('"', "\\\""),
            len = a.len()
        ),
        None => String::new(),
    };
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
