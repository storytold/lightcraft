//! Side-by-side screenshots of every window and dialog (ignored: run on LightCraft main and on the port with the
//! same file).
//!
//! `LC_SHOTS_DIR=<dir> cargo test -p lightcraft-ui-egui --lib -- port_screenshots --ignored --test-threads=1`
//! saves `NN-name.png` per scene at 1600x1000, each scene on a fresh app with the demo library. A scene that can't
//! be reached (a command refused, a dialog that did not open, a panic) is named in `missing.txt` instead of failing
//! the run. Only control commands and app state that exist on both sides are used, so the scene list, the numbers
//! and the names are the same on both.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(30);
const SETTLE: Duration = Duration::from_secs(120);
const SIZE: [f32; 2] = [1600.0, 1000.0];

type Scene = Box<dyn Fn(&mut Headless) -> Result<(), String>>;

fn fresh() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, SIZE, 1.0);
    h.settle(SETTLE);
    h
}

fn ask(h: &mut Headless, method: &str, params: Value) -> Result<(), String> {
    let r = h.request(method, params.clone(), T);
    if r["ok"] == true { Ok(()) } else { Err(format!("{method} {params}: {r}")) }
}

fn exec(h: &mut Headless, command: &str, params: Value) -> Result<(), String> {
    ask(h, "engine.execute", json!({"command": command, "params": params}))
}

fn steps(h: &mut Headless, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

/// Select the first `n` photos (the first is the active one).
fn select(h: &mut Headless, n: usize) -> Result<(), String> {
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(n).map(|p| p.0).collect();
    exec(h, "library.select", json!({"ids": ids}))?;
    h.settle(SETTLE);
    Ok(())
}

fn ui(h: &mut Headless, params: Value) -> Result<(), String> {
    ask(h, "ui.set", params)
}

/// Open a dialog by its serialized form.
fn dialog(h: &mut Headless, value: Value) -> Result<(), String> {
    let d: crate::state::Dialog = serde_json::from_value(value.clone()).map_err(|e| format!("{value}: {e}"))?;
    h.app.ui.dialog = Some(d);
    steps(h, 6);
    h.app.ui.dialog.is_some().then_some(()).ok_or_else(|| "the dialog closed itself".to_string())
}

/// Run a command that opens a dialog, with three photos selected.
fn command_dialog(h: &mut Headless, command: &str, params: Value) -> Result<(), String> {
    select(h, 3)?;
    exec(h, command, params)?;
    h.settle(SETTLE);
    steps(h, 4);
    h.app.ui.dialog.is_some().then_some(()).ok_or_else(|| format!("{command} opened no dialog"))
}

fn open_dialog(name: &'static str, value: Value) -> (&'static str, Scene) {
    (name, Box::new(move |h| dialog(h, value.clone())))
}

fn command(name: &'static str, command: &'static str, params: Value) -> (&'static str, Scene) {
    (name, Box::new(move |h| command_dialog(h, command, params.clone())))
}

fn view(name: &'static str, params: Value, selected: usize) -> (&'static str, Scene) {
    (
        name,
        Box::new(move |h| {
            select(h, selected)?;
            ui(h, params.clone())?;
            Ok(())
        }),
    )
}

fn panel(name: &'static str, panel: &'static str) -> (&'static str, Scene) {
    (
        name,
        Box::new(move |h| {
            select(h, 1)?;
            ui(h, json!({"view": "detail"}))?;
            exec(h, panel, json!({}))
        }),
    )
}

fn scenes() -> Vec<(&'static str, Scene)> {
    let mut all: Vec<(&'static str, Scene)> = Vec::new();

    // main views
    all.push(view("view-library-grid", json!({"view": "photoGrid", "leftPanel": true, "right": "edit"}), 0));
    all.push(view("view-square-grid", json!({"view": "squareGrid", "leftPanel": true}), 0));
    all.push(view("view-detail-edit", json!({"view": "detail", "right": "edit"}), 1));
    all.push(view("view-compare", json!({"view": "compare"}), 2));
    all.push(view("view-survey", json!({"view": "survey"}), 4));
    all.push(view("view-reference", json!({"view": "reference"}), 2));
    all.push(view("view-people", json!({"view": "people"}), 0));
    all.push((
        "view-person",
        Box::new(|h| {
            let name = h.app.session.catalog.people().into_iter().next().map(|p| p.name).ok_or("the demo library names no person")?;
            exec(h, "view.person", json!({"name": name}))
        }),
    ));

    // right panels (in the loupe)
    all.push(panel("panel-edit", "panel.edit"));
    all.push(panel("panel-profiles", "panel.profiles"));
    all.push(panel("panel-presets", "panel.presets"));
    all.push(panel("panel-crop", "panel.crop"));
    all.push(panel("panel-remove", "panel.remove"));
    all.push((
        "panel-masking",
        Box::new(|h| {
            select(h, 1)?;
            ui(h, json!({"view": "detail"}))?;
            exec(h, "panel.masking", json!({}))?;
            exec(h, "mask.add", json!({"kind": "linear", "start": [0.5, 0.2], "end": [0.5, 0.7]}))
        }),
    ));
    all.push(panel("panel-redeye", "panel.redeye"));
    all.push(panel("panel-versions-history", "panel.versions"));
    all.push(panel("panel-activity", "panel.activity"));
    all.push(panel("panel-keywords", "panel.keywords"));
    all.push(panel("panel-info", "panel.info"));

    // the menu bar with a menu open, and the photo menu
    for title in ["File", "Edit", "Photo", "View", "Help"] {
        all.push((
            { let name: &'static str = Box::leak(format!("menubar-{}", title.to_lowercase()).into_boxed_str()); name },
            Box::new(move |h| {
                select(h, 2)?;
                ui(h, json!({"view": "photoGrid", "leftPanel": true}))?;
                steps(h, 4);
                ask(h, "ui.clickWidget", json!({"id": format!("menu:{title}")}))?;
                steps(h, 6);
                Ok(())
            }),
        ));
    }
    all.push((
        "menu-photo-context",
        Box::new(|h| {
            select(h, 1)?;
            ui(h, json!({"view": "photoGrid", "leftPanel": true}))?;
            steps(h, 4);
            let id = h.app.session.visible_cloned().first().map(|p| p.0).ok_or("no photo")?;
            let rect = h.app.widgets.iter().find(|(w, _)| *w == format!("thumb:{id}")).map(|(_, r)| *r).ok_or("no thumb widget")?;
            ask(h, "ui.click", json!({"x": rect.center().x, "y": rect.center().y, "button": "right"}))?;
            steps(h, 6);
            Ok(())
        }),
    ));

    // launch windows: a notice, the library error window, the library chooser
    all.push((
        "launch-notice",
        Box::new(|h| {
            h.app.notices.push("The settings file could not be read and was reset.".into());
            steps(h, 6);
            Ok(())
        }),
    ));
    all.push((
        "launch-library-error",
        Box::new(|h| {
            h.app.library_problem = Some(crate::panels::library_problem::LibraryProblem::new("/no/such/library", "a test error"));
            steps(h, 6);
            Ok(())
        }),
    ));
    all.push((
        "launch-library-chooser",
        Box::new(|h| {
            h.app.ui.settings.recent_libraries = vec!["/home/me/Pictures/Family".into(), "/home/me/Pictures/Work".into(), "/mnt/disk/Archive".into()];
            h.app.library_problem = Some(crate::panels::library_problem::LibraryProblem::choose(&h.app.ui.settings));
            steps(h, 6);
            Ok(())
        }),
    ));

    // Settings, each tab
    for (tab, name) in [
        ("general", "settings-general"),
        ("import", "settings-import"),
        ("performance", "settings-performance"),
        ("display", "settings-display"),
        ("interface", "settings-interface-appearance"),
        ("slideshow", "settings-slideshow"),
        ("faces", "settings-faces"),
        ("denoise", "settings-ai-denoise"),
    ] {
        all.push((name, Box::new(move |h| exec(h, "app.settings", json!({"tab": tab})))));
    }

    // dialogs opened by their commands
    all.push(command("dlg-export", "dialog.export", json!({})));
    all.push(command("dlg-copy-settings", "dialog.copySettings", json!({})));
    all.push((
        "dlg-paste-settings",
        Box::new(|h| {
            select(h, 1)?;
            exec(h, "develop.copy", json!({}))?;
            exec(h, "dialog.pasteSettings", json!({}))?;
            steps(h, 6);
            h.app.ui.dialog.is_some().then_some(()).ok_or_else(|| "no dialog".to_string())
        }),
    ));
    all.push(command("dlg-create-preset", "dialog.createPreset", json!({})));
    all.push(command("dlg-new-album", "dialog.newAlbum", json!({})));
    all.push(command("dlg-new-folder", "dialog.newFolder", json!({})));
    all.push(command("dlg-new-smart-album", "dialog.newSmartAlbum", json!({})));
    all.push(command("dlg-smart-album-rules", "dialog.smartAlbum", json!({})));
    all.push(command("dlg-rename-photos", "dialog.rename", json!({})));
    all.push(command("dlg-label-names", "dialog.labelNames", json!({})));
    all.push(command("dlg-capture-time", "dialog.captureTime", json!({})));
    all.push(command("dlg-auto-stack", "dialog.autoStack", json!({})));
    all.push(command("dlg-cull", "dialog.cull", json!({})));
    all.push(command("dlg-all-metadata", "dialog.allMetadata", json!({})));
    all.push(command("dlg-keyword-shortcut", "dialog.keywordShortcut", json!({})));
    all.push(command("dlg-contact-sheet", "dialog.contactSheet", json!({})));
    all.push(command("dlg-slideshow-export", "dialog.slideshowExport", json!({})));
    all.push(command("dlg-print", "dialog.print", json!({})));
    all.push(command("dlg-merge-hdr", "dialog.mergeHdr", json!({})));
    all.push(command("dlg-merge-panorama", "dialog.mergePanorama", json!({})));
    all.push(command("dlg-face-model", "dialog.faceModel", json!({})));
    all.push(command("dlg-save-mask-effect", "dialog.saveMaskEffect", json!({})));
    all.push(command("dlg-manage-mask-effects", "dialog.manageMaskEffects", json!({})));
    all.push(command("dlg-shortcuts", "app.shortcuts", json!({})));
    all.push(command("dlg-view-options", "view.viewOptions", json!({})));

    // dialogs opened from their state
    all.push(open_dialog("dlg-about", json!({"kind": "about"})));
    all.push(open_dialog("dlg-whats-new", json!({"kind": "whatsNew"})));
    all.push(open_dialog("dlg-welcome-1", json!({"kind": "welcome", "page": 0})));
    all.push(open_dialog("dlg-welcome-3", json!({"kind": "welcome", "page": 2})));
    all.push(open_dialog("dlg-system-info", json!({"kind": "systemInfo", "rows": [["Version", "1.0"], ["Platform", "Linux"], ["Memory", "16 GiB"]]})));
    all.push(open_dialog("dlg-confirm-delete", json!({"kind": "confirmDelete", "count": 3})));
    all.push(open_dialog("dlg-remove-folder", json!({"kind": "removeFolder", "path": "/pics/trip", "name": "pics/trip", "count": 12, "disk": false})));
    all.push(open_dialog(
        "dlg-move-photos",
        json!({"kind": "movePhotos", "ids": [1, 2], "folder": "/pics/trip", "name": "trip", "dont_ask": false, "library": 0}),
    ));
    all.push(open_dialog(
        "dlg-synchronize-folder",
        json!({"kind": "synchronizeFolder", "path": "/pics/trip", "name": "pics/trip", "disk": false,
               "counts": {"offline": false, "new": 3, "duplicates": 0, "unreadable": 0, "missing": 1, "metadata": 2, "moved": 0},
               "import_new": true, "relink_moved": true, "remove_missing": false, "read_metadata": false}),
    ));
    all.push((
        "dlg-rename-album",
        Box::new(|h| {
            let id = h.app.session.catalog.albums().next().map(|a| a.id.0).ok_or("no album in the demo")?;
            dialog(h, json!({"kind": "renameAlbum", "id": id, "name": "Trip"}))
        }),
    ));
    all.push(open_dialog(
        "dlg-text-prompt",
        json!({"kind": "textPrompt", "title": "Rename Preset", "hint": "Preset name", "value": "Warm look", "command": "preset.rename", "params": {}, "key": "name"}),
    ));
    all.push(open_dialog("dlg-rename-keyword", json!({"kind": "renameKeyword", "from": "Travel", "to": "Travel"})));
    all.push(open_dialog("dlg-rename-person", json!({"kind": "renamePerson", "from": "Anna", "to": "Anna Smith", "merge": false})));
    all.push(open_dialog(
        "dlg-keyword-tag",
        json!({"kind": "keywordTag", "editing": null, "name": "Holiday", "parent": null, "inside": false, "synonyms": "vacation, trip",
               "include_on_export": true, "export_containing": true, "export_synonyms": true, "person": false, "add_to_selected": true}),
    ));
    all.push(open_dialog(
        "dlg-keyword-set",
        json!({"kind": "keywordSet", "replaces": null, "name": "Weddings", "slots": ["Bride", "Groom", "", "", "", "", "", "", ""]}),
    ));
    all.push(open_dialog("dlg-move-keyword", json!({"kind": "moveKeyword", "keyword": "Travel", "parent": null})));
    all.push(open_dialog("dlg-delete-keyword", json!({"kind": "deleteKeyword", "keyword": "Travel", "count": 4})));
    all.push(open_dialog("dlg-merge-keywords", json!({"kind": "mergeKeywords", "from": ["Trip", "Holiday"], "into": "Travel"})));
    all.push(open_dialog("dlg-denoise-model", json!({"kind": "denoiseModel", "info": {}, "accepted": false})));
    all.push(open_dialog("dlg-sam-model", json!({"kind": "samModel", "then": null})));
    all.push(open_dialog("dlg-lens-database", json!({"kind": "lensDatabase", "dir": "/tmp/lens", "mirrors": []})));
    all.push(open_dialog("dlg-lightroom-catalog", json!({"kind": "lightroomCatalog", "folder": "/Lightroom", "catalog": "/Lightroom/Catalog.lrcat"})));
    all.push(open_dialog(
        "dlg-lightroom-destination",
        json!({"kind": "lightroomDestination", "catalog": "/Lightroom/Catalog.lrcat", "library": "Photos", "photos": 120, "new_library": false,
               "name": "Catalog", "location": "/home/me", "dont_ask": false, "generation": 0}),
    ));
    all.push(open_dialog(
        "dlg-lightroom-result",
        json!({"kind": "lightroomResult", "summary": "Imported 120 photos", "library": null, "details": ["3 collections", "2 smart collections"], "first": null}),
    ));
    all.push(open_dialog(
        "dlg-save-effect",
        json!({"kind": "saveEffect", "name": "Soft glow", "group": "", "fields": ["exposure"], "amount": true, "brush": false}),
    ));
    all.push(open_dialog(
        "dlg-manage-effects",
        json!({"kind": "manageEffects", "search": "", "selected": [], "rename": null, "confirm_delete": false, "new_group": ""}),
    ));
    all.push(open_dialog("dlg-delete-effects", json!({"kind": "deleteEffects", "names": ["Soft glow"]})));

    // the import review: a folder of eight small pictures
    all.push((
        "dlg-import-review",
        Box::new(|h| {
            let folder = std::env::temp_dir().join(format!("lc-shots-import-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&folder);
            std::fs::create_dir_all(folder.join("day 2")).map_err(|e| e.to_string())?;
            for i in 0..8usize {
                let img = lightcraft_raster::Rgba8::from_fn(96, 64, |x, y| [((x * 2 + i * 30) % 256) as u8, (y * 4 % 256) as u8, (60 + i * 25) as u8, 255]);
                let png = lightcraft_engine::export::encode_image(
                    &img,
                    &lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() },
                )
                .map_err(|e| e.to_string())?;
                let at = if i < 5 { folder.clone() } else { folder.join("day 2") };
                std::fs::write(at.join(format!("IMG_{i:04}.png")), png).map_err(|e| e.to_string())?;
            }
            let opened = exec(h, "file.addFolder", json!({"path": folder.to_string_lossy()}));
            h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. })));
            opened?;
            matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. })).then_some(()).ok_or_else(|| "no import review".to_string())
        }),
    ));
    all
}

fn save(h: &mut Headless, dir: &std::path::Path, file: &str) {
    h.settle(SETTLE);
    steps(h, 8);
    let pixels = h.paint();
    let image =
        lightcraft_raster::Rgba8 { width: pixels.size[0], height: pixels.size[1], data: pixels.pixels.iter().map(|c| c.to_array()).collect() };
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(dir.join(format!("{file}.png")), png).unwrap();
    eprintln!("SHOT {file} {}x{}", pixels.size[0], pixels.size[1]);
}

#[test]
#[ignore = "writes screenshots; run on purpose"]
fn port_screenshots() {
    let Some(dir) = std::env::var_os("LC_SHOTS_DIR") else { panic!("set LC_SHOTS_DIR") };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut missing: Vec<String> = Vec::new();
    for (n, (name, scene)) in scenes().into_iter().enumerate() {
        let file = format!("{:02}-{name}", n + 1);
        let done = catch_unwind(AssertUnwindSafe(|| {
            let mut h = fresh();
            let result = scene(&mut h);
            match result {
                Ok(()) => {
                    save(&mut h, &dir, &file);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }));
        let why = match done {
            Ok(Ok(())) => continue,
            Ok(Err(e)) => e,
            Err(panic) => format!("panicked: {}", panic.downcast_ref::<String>().cloned().or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default()),
        };
        eprintln!("SHOT-MISSING {file}: {why}");
        missing.push(format!("{file}: {}", why.lines().next().unwrap_or("")));
    }
    std::fs::write(dir.join("missing.txt"), missing.join("\n")).unwrap();
}
