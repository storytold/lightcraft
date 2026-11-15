//! Settings ▸ Folders: every folder the app reads or writes, top to bottom, each with its current
//! path and a way to change it (and to go back to the default where there is one). The same
//! settings are also on their own tabs; this one gathers them so they can be set in one place.
//!
//! `app.folders` lists them (id, path, whether it was chosen or is the default, what can be done)
//! for agents and tests; `app.exportFolder {dir | reset}` sets the default export folder, the one
//! File ▸ Export starts in.

use egui::RichText;
use serde_json::{Value, json};

use super::settings::{check, hint, row};
use crate::LccApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// How often the list is read again while the tab is open (listing folders can touch the disk).
const REFRESH_SECS: f64 = 2.0;

/// One folder: `id` (stable, for widget ids and agents), `label`, the effective `path` (None: not
/// set / not available here), whether it was `custom`ised, and which of `change` / `reset` apply.
fn entry(id: &str, label: &str, path: Option<String>, custom: bool, change: bool, reset: bool, note: &str) -> Value {
    json!({"id": id, "label": label, "path": path, "custom": custom, "change": change, "reset": reset, "note": note})
}

/// The folders, in the order the tab shows them (`app.folders`).
pub fn folders(app: &mut LccApp) -> Value {
    let can_pick = app.services.pick_folder.is_some();
    let mut out = Vec::new();
    let library = app.session.library.as_ref().map(|l| l.dir.display().to_string());
    out.push(entry("library", "Library", library, true, can_pick, false, "The open catalog: its records, previews and settings."));
    let last = last_export_dir(app);
    let export = last.clone().or_else(|| Some(crate::control::default_export_dir()).filter(|d| !d.is_empty()));
    out.push(entry(
        "export",
        "Export",
        export,
        last.is_some(),
        true,
        last.is_some(),
        "Where File → Export puts photos unless you choose another folder.",
    ));
    if !cfg!(target_arch = "wasm32") {
        let auto = app.session.import_defaults.auto_folder.clone();
        let on = app.session.import_defaults.auto_active_folder().is_some();
        out.push(entry("autoImport", "Auto Import", auto, on, can_pick, on, "Photos that arrive here are added as soon as they're complete."));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let loc = app.session.execute("library.smartPreviewsLocation", &json!({})).unwrap_or(Value::Null);
        if !loc.is_null() {
            let custom = loc["custom"].as_bool().unwrap_or(false);
            let path = loc["path"].as_str().map(str::to_string);
            out.push(entry("smartPreviews", "Smart previews", path, custom, can_pick, custom, "Moving it moves the smart previews already built."));
        }
    }
    if let Some(access) = &app.services.storage_access {
        let state = if (access.granted)() { "All files" } else { "Only the files and folders you pick" };
        out.push(entry(
            "deviceStorage",
            "Device storage",
            Some(state.to_string()),
            true,
            true,
            false,
            "With access to all files, folders on the device are read where they are instead of being copied.",
        ));
    }
    if app.services.camera_roll.is_some() {
        let mut e = entry(
            "cameraRoll",
            "Camera roll",
            Some("DCIM/Camera".to_string()),
            true,
            true,
            false,
            "Adds the photos taken with this device to the catalog (asks for access to your photos). While watched, new photos are added on their own.",
        );
        e["watch"] = json!(app.ui.camera_roll_watch);
        out.push(e);
    }
    let st = app.session.lens_db.status();
    let lens = st["dir"].as_str().map(str::to_string);
    let env = st["origin"] == "env";
    out.push(entry(
        "lensProfiles",
        "Lens profiles",
        lens,
        st["configured"].is_string(),
        !env,
        false,
        "The lens database lens corrections fall back to.",
    ));
    for (id, label, cmd) in [
        ("faceModels", "Face models", "faces.models.list"),
        ("denoiseModels", "Denoise models", "denoise.models.list"),
        ("mlModels", "Semantic search models", "ml.models.list"),
    ] {
        let dir = app.session.execute(cmd, &json!({})).ok().and_then(|v| v["dir"].as_str().map(str::to_string));
        out.push(entry(id, label, dir, false, false, false, "Models you install are kept here."));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let dir = lcc_engine::config::config_dir().map(|d| d.display().to_string());
        let note = lcc_brand::env_var("CONFIG_DIR");
        out.push(entry(
            "settings",
            "Settings",
            dir,
            false,
            false,
            false,
            &format!("App settings, keymaps and downloads. Set {note} to keep them elsewhere."),
        ));
    }
    Value::Array(out)
}

/// The folder the last export went to, if one is remembered.
fn last_export_dir(app: &LccApp) -> Option<String> {
    let dir = app.session.last_export.as_ref()?.get("dir")?.as_str()?.trim();
    (!dir.is_empty()).then(|| dir.to_string())
}

/// `app.exportFolder`: set the default export folder to `{dir}` (else one chosen in a dialog), or
/// back to `~/Pictures/<app> Exports` with `{reset: true}`. Saved with the library's settings.
pub fn export_folder(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    let reset = p.get("reset").and_then(Value::as_bool).unwrap_or(false);
    let dir = if reset {
        None
    } else {
        match p.get("dir").and_then(Value::as_str).map(str::trim).filter(|d| !d.is_empty()) {
            Some(d) => Some(d.to_string()),
            None => {
                let req = crate::pick::PickRequest::folder(crate::i18n::tr("Choose Export Folder"));
                match crate::pick::ask(app, "app.exportFolder", p, "dir", req, |s| s.pick_folder.as_mut().and_then(|f| f()).map(|x| vec![x])) {
                    crate::pick::Picked::Now(v) => match v.into_iter().next() {
                        Some(d) => Some(d),
                        None => return Ok(Value::Null),
                    },
                    crate::pick::Picked::Later => return Ok(Value::Null),
                    crate::pick::Picked::Unavailable => return Err("no folder dialog here: pass {dir}".into()),
                }
            }
        }
    };
    let mut last = match app.session.last_export.take() {
        Some(Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    match &dir {
        Some(d) => last.insert("dir".into(), json!(d)),
        None => last.remove("dir"),
    };
    app.session.last_export = Some(Value::Object(last));
    app.session.save_prefs().map_err(|e| format!("export folder not saved: {e}"))?;
    let effective = dir.unwrap_or_else(crate::control::default_export_dir);
    Ok(json!({"dir": effective}))
}

/// Change (`button:folderChange-{id}`) or reset (`button:folderReset-{id}`) one folder.
fn act(app: &mut LccApp, id: &str, reset: bool) -> Result<Value, String> {
    let pick = |app: &mut LccApp| app.services.pick_folder.as_mut().and_then(|f| f());
    match (id, reset) {
        ("library", _) => app.run("app.openLibrary", json!({})),
        ("export", false) => app.run("app.exportFolder", json!({})),
        ("export", true) => app.run("app.exportFolder", json!({"reset": true})),
        ("autoImport", false) => match pick(app) {
            Some(f) => app.run("library.autoImport", json!({"folder": f})),
            None => Ok(Value::Null),
        },
        ("autoImport", true) => app.run("library.autoImport", json!({"folder": null})),
        ("smartPreviews", false) => match pick(app) {
            Some(f) => app.run("library.smartPreviewsLocation", json!({"path": f, "existing": "move"})),
            None => Ok(Value::Null),
        },
        ("smartPreviews", true) => app.run("library.smartPreviewsLocation", json!({"reset": true, "existing": "move"})),
        ("lensProfiles", false) => app.run("app.lensDbFolder", json!({})),
        ("deviceStorage", false) => app.run("app.allFilesAccess", json!({})),
        ("cameraRoll", false) => crate::camera_roll::add_and_watch(app),
        _ => Ok(Value::Null),
    }
}

/// The Settings ▸ Folders tab.
pub fn settings_tab(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    use crate::i18n::tr;
    let cache = egui::Id::new("settings-folders");
    let now = ui.input(|i| i.time);
    let list = match ui.data(|d| d.get_temp::<(f64, Value)>(cache)) {
        Some((at, v)) if now - at < REFRESH_SECS => v,
        _ => {
            let v = folders(app);
            ui.data_mut(|d| d.insert_temp(cache, (now, v.clone())));
            v
        }
    };
    super::settings::heading_help(ui, t, tr("Folders"), "folders");
    hint(ui, t, tr("Every folder {app} uses. Changes apply at once; each is also on its own tab."));
    let mut action = None;
    for f in list.as_array().into_iter().flatten() {
        let id = f["id"].as_str().unwrap_or_default();
        let custom = f["custom"].as_bool().unwrap_or(false);
        let path = f["path"].as_str().map(str::to_string);
        ui.add_space(4.0);
        row(ui, t, f["label"].as_str().unwrap_or_default(), |ui| {
            let text = match &path {
                Some(p) if id == "deviceStorage" => tr(p).to_string(),
                Some(p) if custom || id == "library" => p.clone(),
                Some(p) => format!("{p}  ({})", tr("default")),
                None if id == "library" => tr("In memory (nothing is saved)").to_string(),
                None => tr("None").to_string(),
            };
            // a long path is shortened (in full on hover), so it never widens the dialog
            let label = egui::Label::new(RichText::new(&text).color(if path.is_some() { t.text } else { t.text_dim })).truncate();
            let l = ui.allocate_ui(egui::vec2(380.0, 20.0), |ui| ui.add(label)).inner.on_hover_text(&text);
            register(ui.ctx(), format!("label:folder-{id}"), l.rect);
        });
        let (change, reset) = (f["change"].as_bool().unwrap_or(false), f["reset"].as_bool().unwrap_or(false));
        crate::dialog_frame::control_row(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let change_label = if id == "cameraRoll" { "Import…" } else { "Change…" };
            if change && crate::widgets::text_button(ui, &format!("folderChange-{id}"), change_label, false).clicked() {
                action = Some((id.to_string(), false));
            }
            if reset {
                let label = if id == "autoImport" { "Turn Off" } else { "Use Default" };
                if crate::widgets::text_button(ui, &format!("folderReset-{id}"), label, false).clicked() {
                    action = Some((id.to_string(), true));
                }
            }
            // the camera roll's watch (`watch` on its row; a boolean only where a roll exists)
            if let Some(mut on) = f["watch"].as_bool()
                && check(ui, "folderWatchCameraRoll", &mut on, "Watch for new photos")
            {
                crate::camera_roll::set_watch(app, on);
                ui.data_mut(|d| d.remove::<(f64, Value)>(cache));
            }
            ui.label(RichText::new(tr(f["note"].as_str().unwrap_or_default())).size(11.0).color(t.text_dim));
        });
    }
    if let Some((id, reset)) = action {
        ui.data_mut(|d| d.remove::<(f64, Value)>(cache));
        if let Err(e) = act(app, &id, reset) {
            app.toast(ui.ctx(), e);
        }
    }
    if app.services.pick_folder.is_none() {
        hint(ui, t, tr("No folder dialog here: agents set these through the commands each tab uses."));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> LccApp {
        LccApp::new(lcc_engine::Session::new(), crate::Services::default())
    }

    fn find<'a>(list: &'a Value, id: &str) -> &'a Value {
        list.as_array().unwrap().iter().find(|f| f["id"] == id).unwrap()
    }

    #[test]
    fn lists_the_folders_set_elsewhere() {
        let mut app = app();
        let list = app.run("app.folders", json!({})).unwrap();
        for id in ["library", "export", "lensProfiles", "faceModels", "denoiseModels"] {
            assert!(list.as_array().unwrap().iter().any(|f| f["id"] == id), "{id} missing: {list}");
        }
    }

    #[test]
    fn export_folder_is_set_and_reset() {
        let mut app = app();
        let dir = std::env::temp_dir().join("lc-folders-export").display().to_string();
        app.run("app.exportFolder", json!({"dir": dir})).unwrap();
        let list = app.run("app.folders", json!({})).unwrap();
        assert_eq!(find(&list, "export")["path"], json!(dir));
        assert_eq!(find(&list, "export")["custom"], json!(true));
        // the export itself starts there
        assert_eq!(app.session.last_export.as_ref().unwrap()["dir"], json!(dir));
        app.run("app.exportFolder", json!({"reset": true})).unwrap();
        let list = app.run("app.folders", json!({})).unwrap();
        assert_eq!(find(&list, "export")["custom"], json!(false));
    }

    #[test]
    fn device_storage_access_is_shown_and_asked_for_where_the_host_has_it() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        // the desktop has no such question
        let mut app = app();
        let list = app.run("app.folders", json!({})).unwrap();
        assert!(!list.as_array().unwrap().iter().any(|f| f["id"] == "deviceStorage"));
        assert!(app.run("app.allFilesAccess", json!({})).is_err());
        // a host that asks (Android): the system's answer arrives later
        let granted = Arc::new(AtomicBool::new(false));
        let (read, set) = (granted.clone(), granted.clone());
        let services = crate::Services {
            storage_access: Some(crate::StorageAccess {
                granted: Box::new(move || read.load(Ordering::Relaxed)),
                ask: Box::new(move || {
                    set.store(true, Ordering::Relaxed);
                    Ok(())
                }),
            }),
            ..Default::default()
        };
        let mut app = LccApp::new(lcc_engine::Session::new(), services);
        let list = app.run("app.folders", json!({})).unwrap();
        assert_eq!(find(&list, "deviceStorage")["change"], json!(true));
        let r = app.run("app.allFilesAccess", json!({})).unwrap();
        assert_eq!(r["granted"], json!(true));
        assert!(granted.load(Ordering::Relaxed));
    }

    #[test]
    fn export_folder_without_a_dialog_is_an_error() {
        let mut app = app();
        assert!(app.run("app.exportFolder", json!({"dir": "   "})).is_err());
    }
}
