//! File ▸ More Exports ▸ Export with Several Presets… (process recipes, as Capture One calls them):
//! tick several export presets in `Dialog::RunPresets` (drawn in `panels::dialogs`) and export the
//! selection with all of them at once. The run is `lcc_engine::run_presets` on a worker: each photo
//! is decoded once for every preset, and its one row in the activity stack has ✕.
//!
//! Sources: own design after Capture One's process recipes (behaviour only).

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::LccApp;
use crate::json_ext::JsonExt as _;
use crate::pick::{PickRequest, Picked};
use crate::state::Dialog;

/// UI commands: (id, label, shortcut, menu).
pub const COMMANDS: &[crate::menus::UiCommand] = &[
    ("dialog.runPresets", "Export with Several Presets…", None, "File>More Exports"),
    // {presetIds, ids?, dir?}: asks for a folder when a picked preset names none
    ("app.runPresets", "Export with Several Presets", None, ""),
];

pub fn enabled(app: &LccApp, id: &str) -> Option<bool> {
    COMMANDS.iter().any(|c| c.0 == id).then(|| !app.session.selected().is_empty() && app.export.is_none())
}

pub fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    Some(match id {
        "dialog.runPresets" => {
            app.ui.dialog = Some(Dialog::RunPresets { picked: Vec::new() });
            Ok(Value::Null)
        }
        "app.runPresets" => {
            if app.export.is_some() {
                return Some(Err("an export is already running".into()));
            }
            let mut params = p.clone();
            if p.get("dir").is_none() && needs_folder(app, p) {
                let paths = match crate::pick::ask(app, id, p, "dir", PickRequest::folder(crate::i18n::tr("Export To")), |_| None) {
                    Picked::Now(paths) => paths,
                    Picked::Later => return Some(Ok(Value::Null)),
                    Picked::Unavailable => return Some(Err("no folder dialog on this platform; supply dir".into())),
                };
                let Some(dir) = paths.first() else { return Some(Ok(Value::Null)) };
                *params.field_mut("dir") = json!(dir);
            }
            start(app, &params)
        }
        _ => return None,
    })
}

/// Whether one of the presets `p.presetIds` names no folder of its own (so the run needs `dir`).
fn needs_folder(app: &LccApp, p: &Value) -> bool {
    let names = p.get("presetIds").and_then(Value::as_array).cloned().unwrap_or_default();
    names.iter().filter_map(Value::as_str).any(|name| {
        let Ok(params) = app.session.export_params(&json!({"preset": name})) else { return false };
        let same = params.get("destinationMode").and_then(Value::as_str).is_some_and(|m| m.eq_ignore_ascii_case("sameAsOriginal"));
        !same && params.get("dir").and_then(Value::as_str).is_none_or(|d| d.trim().is_empty())
    })
}

/// The dialog's `export.runPresets` params.
pub fn dialog_params(d: &Dialog) -> Option<Value> {
    let Dialog::RunPresets { picked } = d else { return None };
    Some(json!({"presetIds": picked}))
}

/// Prepare on the UI thread (every argument checked), run on a worker with one activity row.
pub fn start(app: &mut LccApp, params: &Value) -> Result<Value, String> {
    if app.export.is_some() {
        return Err("an export is already running".into());
    }
    let prepared = lcc_engine::run_presets::prepare(&mut app.session, params)?;
    let total = prepared.len();
    let progress = Arc::new(Mutex::new((0, String::new())));
    let cancel = Arc::new(AtomicBool::new(false));
    let (p, c) = (progress.clone(), cancel.clone());
    let (tx, rx) = channel();
    let work = move || {
        let report = prepared.run(&c, &mut |done, name| {
            if let Ok(mut g) = p.lock() {
                *g = (done, name.to_string());
            }
        });
        let _ = tx.send(Ok((report.files.clone(), None, report.cancelled)));
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::Builder::new().name("run-presets".into()).spawn(work).map_err(|e| e.to_string())?;
    #[cfg(target_arch = "wasm32")]
    work();
    let row = app.session.activity.start("export", "Exporting", lcc_engine::activity::Cancel::Flag(cancel.clone()));
    row.progress(0, total as u64);
    app.export = Some(crate::export_task::ExportTask::new(total, progress, cancel, rx, row));
    Ok(json!({"background": true, "total": total}))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn dialog_picks_presets_and_runs_them_together() {
        let dir = lcc_testkit::TempDir::new("lcc-ui-run-presets");
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), crate::Services::default());
        assert!(crate::menus::menu_entries(&app).iter().any(|e| e.id == "dialog.runPresets" && e.menu == ["File", "More Exports"]));
        for (name, edge) in [("A", 40), ("B", 56)] {
            let d = dir.join(name);
            app.session
                .execute("export.savePreset", &json!({"name": name, "params": {"format": "jpeg", "longEdge": edge, "dir": d.to_string_lossy()}}))
                .unwrap();
        }
        let ids: Vec<u64> = app.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
        app.session.execute("library.select", &json!({"ids": ids})).unwrap();
        app.run("dialog.runPresets", json!({})).unwrap();
        let Some(Dialog::RunPresets { picked }) = app.ui.dialog.as_mut() else { panic!("no dialog") };
        picked.extend(["A".to_string(), "B".to_string()]);
        let params = dialog_params(app.ui.dialog.as_ref().unwrap()).unwrap();
        app.ui.dialog = None;
        app.run("app.runPresets", params).unwrap();
        let rows = app.session.activity.list();
        assert_eq!(rows.iter().filter(|r| r.label == "Exporting").count(), 1);
        let mut h = crate::headless::Headless::new(app, [1000.0, 800.0], 1.0);
        let deadline = Instant::now() + Duration::from_secs(120);
        while h.app.last_export_result.is_none() {
            h.step();
            assert!(Instant::now() < deadline, "the run did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
        for name in ["A", "B"] {
            assert_eq!(std::fs::read_dir(dir.join(name)).unwrap().count(), 2, "{name}");
        }
        // an unknown preset is refused before anything runs
        assert!(h.app.run("app.runPresets", json!({"presetIds": ["Nope"]})).is_err());
    }
}
