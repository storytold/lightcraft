//! G19: plug-in export hooks run on a worker with an activity row. A test binary of its own: the
//! plug-in manager and the running hooks are process-wide, so the slow hook installed here would
//! otherwise run after other tests' exports (and add its row to their activity stacks).

use serde_json::json;

/// An export-hook plug-in that spins for `spins` loop turns before answering `{seen: true}`
/// (a stand-in for a slow upload).
fn slow_hook(id: &str, spins: u32) -> Vec<u8> {
    let manifest = format!(r#"{{"id":"{id}","name":"Slow","hooks":{{"export":true}}}}"#);
    let reply = r#"{"ok":{"seen":true}}"#;
    let esc = |s: &str| s.replace('"', "\\\"");
    let src = format!(
        r#"(module (memory (export "memory") 2)
  (data (i32.const 16) "{m}") (data (i32.const 8192) "{r}")
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.or (i64.shl (i64.const {ml}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 65536))
  (func (export "lcc_handle") (param i32 i32) (result i64) (local $n i32)
(local.set $n (i32.const {spins}))
(block (loop (br_if 1 (i32.eqz (local.get $n))) (local.set $n (i32.sub (local.get $n) (i32.const 1))) (br 0)))
(i64.or (i64.shl (i64.const {rl}) (i64.const 32)) (i64.const 8192))))"#,
        m = esc(&manifest),
        r = esc(reply),
        ml = manifest.len(),
        rl = reply.len()
    );
    wat::parse_str(&src).unwrap()
}

/// G19: a slow export hook runs on a worker: its row is in the activity stack, the window keeps
/// drawing frames while it runs, and its result reaches the export's result when it's done.
#[test]
fn export_hooks_run_off_the_ui_thread_with_an_activity_row() {
    // install into memory, never into the real user's settings folder (a failed run left the test
    // plug-in installed there)
    lcc_engine::cmd::plugins::use_in_memory_manager();
    let dir = std::env::temp_dir().join(format!("plugin-worker-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wasm = dir.join("slow.wasm");
    std::fs::write(&wasm, slow_hook("test.slowhook", 60_000_000)).unwrap();
    let app = lcc_ui_egui::LccApp::new(lcc_engine::Session::with_demo(), lcc_ui_egui::Services { png: None, ..Default::default() });
    let mut h = lcc_ui_egui::headless::Headless::new(app, [1000.0, 700.0], 1.0);
    h.app.session.execute("plugin.install", &json!({"path": wasm.display().to_string()})).unwrap();
    let files = [json!({"path": dir.join("x.jpg").display().to_string()})];
    h.app.last_export_result = Some(json!({"files": files, "cancelled": false}));
    let ctx = h.view.ctx.clone();
    let started = std::time::Instant::now();
    lcc_ui_egui::panels::plugins::after_export(&mut h.app, &ctx, &files, None);
    assert!(started.elapsed() < std::time::Duration::from_millis(500), "returned at once: {:?}", started.elapsed());
    assert!(lcc_ui_egui::panels::plugins::export_hooks_running());
    let row = h.app.session.activity.list().into_iter().find(|r| r.kind == "plugins").expect("an activity row");
    assert_eq!(row.label, "Running plug-in export hooks");
    assert!(row.cancellable);
    // The UI isn't blocked: `after_export` returned at once (above) and frames keep coming while the
    // hook runs. How many fit depends on the machine (a fast desktop finishes the spin loop within a
    // couple of frames, the first of which is slow), so the test asks for one, not a frame count.
    let (frames, mut while_running) = (h.frames(), 0);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while lcc_ui_egui::panels::plugins::export_hooks_running() && std::time::Instant::now() < deadline {
        h.step();
        while_running += 1;
    }
    assert!(!lcc_ui_egui::panels::plugins::export_hooks_running(), "the hooks finished");
    assert!(while_running >= 1 && h.frames() > frames, "frames went on while the hook ran: {while_running}");
    assert!(h.app.session.activity.list().iter().all(|r| r.kind != "plugins"), "the row is gone");
    let plugins = &h.app.last_export_result.as_ref().unwrap()["plugins"];
    assert!(plugins.as_array().unwrap().iter().any(|v| v["plugin"] == "test.slowhook" && v["ok"]["seen"] == true), "{plugins}");
    h.app.session.execute("plugin.uninstall", &json!({"id": "test.slowhook"})).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
