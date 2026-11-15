//! The Import tab (OneSlips LR-IMP-WINDOW), the default with Settings ▸ Import ▸ Import as a tab,
//! and the import review when that is off: both paths, in the Classic, Compact and Modern layouts.

use std::time::Duration;

use serde_json::{Value, json};

use crate::LccApp;
use crate::headless::Headless;
use crate::import_tab::Tab;
use crate::state::{Dialog, ViewMode};

const T: Duration = Duration::from_secs(10);
const SETTLE: Duration = Duration::from_secs(120);

fn app(size: [f32; 2], layout: &str) -> Headless {
    let services = crate::Services { png: None, ..Default::default() };
    let mut app = LccApp::new(lcc_engine::Session::new(), services);
    app.ui.view = ViewMode::PhotoGrid;
    match layout {
        "compact" => app.ui.settings.compact_layout = true,
        "modern" => app.ui.modern.enabled = true,
        _ => {}
    }
    Headless::new(app, size, 1.0)
}

fn exec(h: &mut Headless, command: &str, params: Value) -> Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command} {params}: {r}");
    h.settle(SETTLE);
    r["result"].clone()
}

fn try_exec(h: &mut Headless, command: &str, params: Value) -> Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    h.settle(SETTLE);
    r
}

/// A folder of `n` small PNGs (different bytes each) in the temporary folder.
fn folder(name: &str, n: u8) -> lcc_testkit::TempDir {
    let dir = lcc_testkit::TempDir::new(&format!("lc-ui-importtab-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
    for i in 0..n {
        let img = lcc_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 12) as u8, 40 * i, 255]);
        std::fs::write(dir.join(format!("p{i}.png")), lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();
    }
    dir
}

fn has_widget(h: &mut Headless, name: &str) -> bool {
    for _ in 0..3 {
        h.step();
    }
    h.app.widgets.iter().any(|(n, _)| n == name)
}

#[test]
fn import_is_a_tab_by_default_and_browsing_shows_it() {
    let mut h = app([1400.0, 900.0], "classic");
    assert!(h.app.ui.settings.import_as_tab, "the default");
    h.settle(SETTLE);
    assert!(has_widget(&mut h, "module:import"), "Import is in the module bar with Library, Develop…");
    exec(&mut h, "dialog.import", json!({}));
    assert_eq!(h.app.ui.tab, Tab::Import);
    assert!(!matches!(h.app.ui.dialog, Some(Dialog::Import { .. })), "no review dialog");
    assert!(has_widget(&mut h, "panel:importTab") && has_widget(&mut h, "importGo"));
    let dir = folder("default", 3);
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    assert_eq!(h.app.session.visible_cloned().len(), 3);
    assert_eq!(h.app.ui.tab, Tab::Import);
    // the view going back to the library brings back the Library tab
    exec(&mut h, "library.source", json!({"kind": "all"}));
    assert_eq!(h.app.ui.tab, Tab::Library);
    // a browsed folder only ever shows in the Import tab
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    assert_eq!(h.app.ui.tab, Tab::Import);
    exec(&mut h, "view.libraryTab", json!({}));
    assert_ne!(h.app.session.source, lcc_engine::LibrarySource::Folder, "the Library tab shows the library");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn with_the_option_off_import_opens_the_review_as_before() {
    let mut h = app([1400.0, 900.0], "classic");
    h.app.ui.settings.import_as_tab = false;
    h.settle(SETTLE);
    assert!(!has_widget(&mut h, "module:import"), "no Import entry");
    exec(&mut h, "dialog.import", json!({}));
    assert!(matches!(h.app.ui.dialog, Some(Dialog::Import { .. })), "the review");
    h.app.ui.dialog = None;
    let dir = folder("off", 2);
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    assert_eq!(h.app.ui.tab, Tab::Library, "a browsed folder shows in the library's own view");
    assert!(!has_widget(&mut h, "panel:importTab"));
    assert!(try_exec(&mut h, "view.importTab", json!({}))["ok"] == false, "no tab to switch to");
    // the review imports as it did
    exec(&mut h, "dialog.import", json!({"path": dir.to_string_lossy()}));
    h.step_until(SETTLE, |h| matches!(&h.app.ui.dialog, Some(Dialog::Import { opts }) if opts.candidates.len() == 2));
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::Import { opts }) if opts.candidates.len() == 2));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_big_button_imports_the_picks_and_leaves_the_files_alone() {
    let mut h = app([1400.0, 900.0], "classic");
    let dir = folder("picks", 3);
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    let ids = h.app.session.visible_cloned();
    assert_eq!(ids.len(), 3);
    exec(&mut h, "photo.flag", json!({"ids": [ids[0].0], "flag": "pick"}));
    let targets = exec(&mut h, "import.targets", json!({}));
    assert_eq!(targets["picks"], 1, "{targets}");
    exec(&mut h, "import.configure", json!({"take": "picks", "keywords": "trip", "previews": "none"}));
    let r = exec(&mut h, "import.start", json!({}));
    assert_eq!(r["importing"], 1, "{r}");
    h.step_until(SETTLE, |h| h.app.import.is_none());
    assert_eq!(h.app.ui.tab, Tab::Library, "the photos come in in the Library tab");
    let p = h.app.session.catalog.photo(ids[0]).unwrap();
    assert!(!p.local, "the pick is in the library");
    assert!(p.meta.keywords.iter().any(|k| k == "trip"), "with the tab's keywords");
    assert!(h.app.session.catalog.photo(ids[1]).unwrap().local, "the others stay browsed");
    for i in 0..3 {
        assert!(dir.join(format!("p{i}.png")).is_file(), "added in place: no file moved");
    }
    // nothing left to take
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    assert!(try_exec(&mut h, "import.start", json!({"what": "picks"}))["ok"] == false);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn browsed_photos_cant_be_deleted_from_the_tab() {
    let mut h = app([1400.0, 900.0], "classic");
    let dir = folder("nodelete", 2);
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    let ids = h.app.session.visible_cloned();
    exec(&mut h, "library.select", json!({"ids": [ids[0].0]}));
    let r = try_exec(&mut h, "photo.delete", json!({}));
    assert_eq!(r["ok"], false, "{r}");
    let r = try_exec(&mut h, "photo.deleteFromDisk", json!({"confirm": true}));
    assert_eq!(r["ok"], false, "{r}");
    let p = h.app.session.catalog.photo(ids[0]).unwrap();
    assert!(p.local && !p.deleted);
    assert!(dir.join("p0.png").is_file() && dir.join("p1.png").is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_presets_keep_the_tabs_settings() {
    let mut h = app([1400.0, 900.0], "classic");
    exec(&mut h, "import.configure", json!({"copy": true, "keywords": "a, b", "previews": "full"}));
    exec(&mut h, "import.saveSettings", json!({"name": "Card"}));
    exec(&mut h, "import.configure", json!({"copy": false, "keywords": "", "previews": "none"}));
    exec(&mut h, "import.usePreset", json!({"name": "card"}));
    let s = &h.app.ui.settings.import_tab;
    assert!(s.opts.copy && s.opts.keywords == "a, b" && s.previews == "full", "{s:?}");
    assert!(try_exec(&mut h, "import.configure", json!({"candidates": []}))["ok"] == false);
    assert!(try_exec(&mut h, "import.configure", json!({"take": "most"}))["ok"] == false);
    assert!(try_exec(&mut h, "import.usePreset", json!({"name": "none such"}))["ok"] == false);
}

/// A Library filter must not hide the folder the Import tab browses (the tab has no filter bar of
/// its own): it is put aside while the tab is up, and back when the Library tab returns.
#[test]
fn library_filters_dont_hide_the_browsed_folder() {
    let mut h = app([1400.0, 900.0], "classic");
    let dir = folder("filtered", 3);
    exec(&mut h, "library.filter", json!({"rating": 5, "ratingOp": "atLeast"}));
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    assert_eq!(h.app.ui.tab, Tab::Import);
    assert!(h.app.session.filters_off(), "the filter is put aside");
    assert_eq!(h.app.session.visible_cloned().len(), 3, "every file of the folder is shown");
    // the filter comes back with the Library tab
    exec(&mut h, "view.libraryTab", json!({}));
    assert!(!h.app.session.filters_off());
    assert_eq!(h.app.session.visible_cloned().len(), 0, "the rating-5 filter is back");
    // and it is put aside again the next time the folder is browsed
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    assert_eq!(h.app.session.visible_cloned().len(), 3);
    exec(&mut h, "view.libraryTab", json!({}));
    assert!(!h.app.session.filters_off());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A module switch while the Browse tab is up leaves the tab first (the Classic module bar's rule,
/// kept for the Modern Develop pill, shortcuts and agents too): the tab's sheet must never end up
/// drawn under another module's chrome. Develop with a browsed photo active is the exception that
/// gives the rule a meaning: it develops the browsed photo without importing it.
#[test]
fn switching_modules_leaves_the_browse_tab_first() {
    for layout in ["classic", "modern"] {
        let mut h = app([1400.0, 900.0], layout);
        let dir = folder(&format!("switch-{layout}"), 2);
        exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
        assert_eq!(h.app.ui.tab, Tab::Import, "{layout}");
        // Develop on the browsed photo, in place: the tab hands over and the edit adds no
        // library photo
        exec(&mut h, "module.develop", json!({}));
        assert_eq!(h.app.ui.module, crate::module::ModuleId::Develop, "{layout}");
        assert_eq!(h.app.ui.tab, Tab::Library, "{layout}: the Browse tab handed over");
        h.settle(SETTLE);
        assert!(!has_widget(&mut h, "panel:importTab"), "{layout}: no Browse sheet under Develop");
        assert!(h.app.session.catalog.photos().filter(|p| p.in_library()).count() == 0, "{layout}");
        // and the way back works: the folder view is still on, so the Browse tab comes back
        exec(&mut h, "module.library", json!({}));
        assert_eq!(h.app.ui.module, crate::module::ModuleId::Library, "{layout}");
        assert_eq!(h.app.ui.tab, Tab::Import, "{layout}: the folder view brings Browse back");
        assert!(matches!(h.app.session.source, lcc_engine::LibrarySource::Folder), "{layout}");
        // another module leaves the tab without developing anything
        exec(&mut h, "module.map", json!({}));
        assert_eq!(h.app.ui.tab, Tab::Library, "{layout}: the Browse tab handed over");
        assert!(!has_widget(&mut h, "panel:importTab"), "{layout}: no Browse sheet under Map");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The tab in every layout: its bar and button are drawn, and on a narrow window the sources and
/// settings are a tap away.
#[test]
fn the_tab_works_in_compact_and_modern_layouts() {
    for (layout, size) in [("compact", [1000.0, 760.0]), ("modern", [1300.0, 900.0]), ("modern", [400.0, 860.0]), ("classic", [760.0, 700.0])] {
        let mut h = app(size, layout);
        let dir = folder(&format!("{layout}-{}", size[0] as u32), 2);
        exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
        assert_eq!(h.app.ui.tab, Tab::Import, "{layout}");
        h.settle(SETTLE);
        assert!(has_widget(&mut h, "panel:importTab"), "{layout} {size:?}");
        assert!(has_widget(&mut h, "importGo"), "{layout} {size:?}");
        if layout == "modern" {
            assert!(has_widget(&mut h, "button:importBackToLibrary"), "{size:?}: a way back to the library");
        }
        if size[0] < 900.0 {
            assert!(has_widget(&mut h, "button:importPane-settings"), "{layout} {size:?}: the settings a tap away");
        } else {
            assert!(has_widget(&mut h, "label:importFrom"), "{layout} {size:?}: the sources beside the photos");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The Import tab's settings column has the per-body Camera Profile rows the import review has
/// (for the photos it would take), and the choice made there reaches the imported raws.
#[test]
fn the_tab_offers_a_camera_profile_per_body_and_imports_with_it() {
    let dir = lcc_testkit::TempDir::new("lc-ui-importtab-bodies");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let corners = lcc_engine::chart_profiling::camprof::ChartCorners::from_rect(10.0, 10.0, 40.0, 30.0);
    for (name, serial) in [("SA0001.dng", "SN1"), ("SB0001.dng", "SN2")] {
        let bytes = lcc_engine::chart_profiling::demo_chart_dng_of_body(64, 48, &corners, "D55", true, Some(serial)).unwrap();
        std::fs::write(dir.join(name), bytes).unwrap();
    }
    for layout in ["classic", "compact", "modern"] {
        let mut h = app([1400.0, 900.0], layout);
        // (raws are read with the file system's probe)
        h.app.session = lcc_engine::Session::new().with_fs();
        exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
        exec(&mut h, "import.configure", json!({"take": "all", "previews": "none"}));
        h.step_until(SETTLE, |h| h.app.session.catalog.photos().filter(|p| p.local && lcc_engine::profile_bodies::key_of(p).is_some()).count() == 2);
        let ids = h.app.session.visible_cloned();
        let keys: Vec<String> = ids.iter().filter_map(|id| lcc_engine::profile_bodies::key_of(h.app.session.catalog.photo(*id)?)).collect();
        assert_eq!(keys.len(), 2, "{layout}: two bodies ({} photos)", ids.len());
        for k in &keys {
            assert!(has_widget(&mut h, &format!("combo:importBodyProfile:{k}")), "{layout}: a row for {k}");
        }
        if layout != "classic" {
            continue;
        }
        h.app.ui.settings.import_tab.opts.per_body.insert(keys[0].clone(), "lc".into());
        exec(&mut h, "import.start", json!({}));
        h.step_until(SETTLE, |h| h.app.import.is_none());
        let profile = |h: &Headless, k: &str| {
            let id = ids
                .iter()
                .find(|id| h.app.session.catalog.photo(**id).and_then(|p| lcc_engine::profile_bodies::key_of(p)).as_deref() == Some(k))
                .unwrap();
            h.app.session.catalog.photo(*id).unwrap().develop.profile.camera.clone()
        };
        assert_eq!(profile(&h, &keys[0]), "lc", "the body's choice");
        assert_eq!(profile(&h, &keys[1]), "", "the other body: automatic");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn recipe_preset(h: &mut Headless, name: &str, control: &str, value: f64) -> String {
    let r = exec(h, "preset.saveRecipe", json!({"name": name, "recipe": [{"id": control, "value": value}]}));
    r["id"].as_str().unwrap().to_string()
}

/// The preset stack (IMPORT-STACK, ported from OneSlips' Import panel): the Apply card lists the
/// presets in order (a single preset from older settings first), a row moves later or goes, and
/// the tab imports with the stack in that order.
#[test]
fn the_tab_imports_with_a_preset_stack_in_the_order_shown() {
    let mut h = app([1400.0, 1000.0], "classic");
    h.app.session = lcc_engine::Session::new().with_fs();
    let dir = folder("stack", 2);
    let bright = recipe_preset(&mut h, "Bright", "light.exposure", 0.5);
    let brighter = recipe_preset(&mut h, "Brighter", "light.exposure", 1.0);
    let contrast = recipe_preset(&mut h, "Contrast", "light.contrast", 20.0);
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    // an old single preset and a stack: one list, the single one first
    exec(&mut h, "import.configure", json!({"take": "all", "previews": "none", "preset": bright, "presets": [brighter, contrast]}));
    assert!(has_widget(&mut h, "label:importPreset-2"), "three rows");
    assert_eq!(h.app.ui.settings.import_tab.opts.presets, [bright.clone(), brighter.clone(), contrast.clone()]);
    assert!(h.app.ui.settings.import_tab.opts.preset.is_empty(), "folded into the stack");
    assert!(has_widget(&mut h, "combo:importPresetAdd") && has_widget(&mut h, "check:importPresetProfile"));
    // Bright goes after Brighter: it is applied last, so its exposure wins
    let r = h.request("ui.clickWidget", json!({"id": "icon:importPresetDown-0"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.settings.import_tab.opts.presets, [brighter.clone(), bright.clone(), contrast.clone()]);
    // Contrast goes
    let r = h.request("ui.clickWidget", json!({"id": "icon:importPresetRemove-2"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.settings.import_tab.opts.presets, [brighter.clone(), bright.clone()]);
    let ids = h.app.session.visible_cloned();
    exec(&mut h, "import.start", json!({}));
    h.step_until(SETTLE, |h| h.app.import.is_none());
    let p = h.app.session.catalog.photo(ids[0]).unwrap();
    assert!(!p.local);
    assert!((p.develop.light.exposure - 0.5).abs() < 1e-6, "Bright last: {}", p.develop.light.exposure);
    assert!(p.develop.light.contrast.abs() < 1e-6, "Contrast was taken off the stack");
    let steps: Vec<&str> = p.history.iter().filter(|s| s.label.starts_with("Preset: ")).map(|s| s.label.as_str()).collect();
    assert_eq!(steps, ["Preset: Brighter", "Preset: Bright"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The import review (Import as a tab off) shows the same stack, starts with the one used last,
/// imports with it in order and keeps what it ran with for the next import.
#[test]
fn the_review_imports_with_the_preset_stack_used_last_and_keeps_it() {
    let mut h = app([1400.0, 1000.0], "classic");
    h.app.ui.settings.import_as_tab = false;
    h.app.session = lcc_engine::Session::new().with_fs();
    let bright = recipe_preset(&mut h, "Bright", "light.exposure", 0.5);
    let contrast = recipe_preset(&mut h, "Contrast", "light.contrast", 20.0);
    h.app.ui.settings.import_tab.opts.presets = vec![bright.clone()];
    let dir = folder("review-stack", 2);
    exec(&mut h, "dialog.import", json!({"path": dir.to_string_lossy()}));
    h.step_until(SETTLE, |h| matches!(&h.app.ui.dialog, Some(Dialog::Import { opts }) if opts.candidates.len() == 2));
    let Some(Dialog::Import { opts }) = &mut h.app.ui.dialog else { panic!("no review") };
    assert_eq!(opts.presets, std::slice::from_ref(&bright), "the stack used last");
    opts.presets.push(contrast.clone());
    assert!(has_widget(&mut h, "label:importPreset-1") && has_widget(&mut h, "combo:importPresetAdd"), "the review lists the stack");
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step_until(SETTLE, |h| h.app.import.is_none());
    let photos: Vec<_> = h.app.session.catalog.photos().filter(|p| !p.local).cloned().collect();
    assert_eq!(photos.len(), 2);
    for p in photos {
        assert!((p.develop.light.exposure - 0.5).abs() < 1e-6 && (p.develop.light.contrast - 20.0).abs() < 1e-6, "both presets");
        let steps: Vec<&str> = p.history.iter().filter(|s| s.label.starts_with("Preset: ")).map(|s| s.label.as_str()).collect();
        assert_eq!(steps, ["Preset: Bright", "Preset: Contrast"]);
    }
    assert_eq!(h.app.ui.settings.import_tab.opts.presets, [bright, contrast], "kept for the next import");
    let _ = std::fs::remove_dir_all(&dir);
}

/// "Develop without Import" opens the browsed folder's active photo in the Library tab's Develop
/// module without importing it: the browse stays on, an edit lands on the photo (the sidecar
/// carries it, the catalog's library stays empty), and going back to the Browse tab brings the
/// folder view back.
#[test]
fn develop_without_import_edits_a_browsed_photo_in_place() {
    let mut h = app([1400.0, 900.0], "classic");
    h.app.session = lcc_engine::Session::new().with_fs();
    h.settle(SETTLE);
    let dir = folder("develop-local", 2);
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    assert_eq!(h.app.session.visible_cloned().len(), 2);
    let r = exec(&mut h, "import.develop", json!({}));
    assert_eq!(r["module"], "develop");
    assert_eq!(h.app.ui.tab, Tab::Library, "the Library tab shows the Develop module");
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Develop);
    assert_eq!(h.app.session.source, lcc_engine::LibrarySource::Folder, "the folder view stays on");
    // an edit on the browsed photo, without the photo joining the library
    let active = h.app.session.active().expect("the browsed photo is active");
    h.app.run("develop.set", json!({"control": "light.exposure", "value": 0.5})).unwrap();
    let p = h.app.session.catalog.photo(active).expect("the photo is on record");
    assert!((p.develop.light.exposure - 0.5).abs() < 1e-6, "the edit lands");
    assert!(p.local && !p.in_library(), "browsed only: no library photo was added");
    // the folder view comes back with the Browse tab
    exec(&mut h, "view.importTab", json!({}));
    assert_eq!(h.app.ui.tab, Tab::Import);
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Library);
    assert_eq!(h.app.session.visible_cloned().len(), 2, "browsing again");
    let _ = std::fs::remove_dir_all(&dir);
}

/// "Develop without Import" without a photo chosen refuses with a reason, and a library tab
/// switch while not browsing refuses too.
#[test]
fn develop_without_import_needs_a_browse_and_a_photo() {
    let mut h = app([1400.0, 900.0], "classic");
    h.settle(SETTLE);
    assert!(try_exec(&mut h, "import.develop", json!({}))["ok"] == false, "nothing browsed");
    let dir = folder("develop-guard", 1);
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    // the browse makes its first photo active; deselecting refuses
    exec(&mut h, "library.select", json!({"ids": []}));
    assert!(try_exec(&mut h, "import.develop", json!({}))["ok"] == false, "no photo chosen");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The browse quick filter: Show All / Show Not Imported in the bar — by default only the files
/// an import would take, Show All adds the ones the library has already with an "In catalog"
/// badge on their cells.
#[test]
fn browse_show_all_marks_photos_in_the_library() {
    let mut h = app([1400.0, 900.0], "classic");
    h.app.session = lcc_engine::Session::new().with_fs();
    h.settle(SETTLE);
    let dir = folder("show-all", 2);
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    assert!(has_widget(&mut h, "button:browseShow-0") && has_widget(&mut h, "button:browseShow-1"), "the quick filter is in the bar");
    // the first photo joins the library, as Add to My Photos does
    let first = h.app.session.visible_cloned()[0];
    exec(&mut h, "photo.addToLibrary", json!({"ids": [first.0]}));
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    assert_eq!(h.app.session.visible_cloned().len(), 1, "not imported is the default");
    // Show All: both files, the library one badged
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy(), "show": "all"}));
    assert_eq!(h.app.session.visible_cloned().len(), 2);
    let library_one = h.app.session.visible_cloned().into_iter().find(|id| h.app.session.catalog.photo(*id).is_some_and(|p| p.in_library()));
    assert_eq!(library_one, Some(first), "the library photo shows");
    h.step();
    assert!(
        library_one.is_some_and(|id| h.app.widgets.iter().any(|(n, _)| *n == format!("badge:inLibrary:{}", id.0))),
        "the In library badge is on its cell"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn widget_rect(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

/// The Import Settings column resizes by its left edge, as the side panels do, and the width is
/// part of the saved UI state (ui.json).
#[test]
fn the_import_settings_column_resizes_by_its_left_edge_and_remembers_it() {
    let mut h = app([1600.0, 900.0], "compact");
    h.settle(SETTLE);
    exec(&mut h, "dialog.import", json!({}));
    assert!(has_widget(&mut h, "panel:importSettings"));
    let before = widget_rect(&h, "panel:importSettings").unwrap();
    assert!((before.width() - crate::state::IMPORT_SETTINGS_WIDTH.default).abs() <= 1.0, "{before:?}");
    let (x, y) = (before.left() + 1.0, before.center().y);
    let r = h.request("ui.drag", json!({"x": x, "y": y, "toX": x - 120.0, "toY": y, "steps": 12}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..3 {
        h.step();
    }
    let w = h.app.ui.import_settings_width;
    assert!((w - (crate::state::IMPORT_SETTINGS_WIDTH.default + 120.0)).abs() <= 2.0, "{w}");
    assert!((widget_rect(&h, "panel:importSettings").unwrap().width() - w).abs() <= 1.0);
    // kept in the saved UI state
    let saved = serde_json::to_value(&h.app.ui).unwrap();
    let back: crate::state::UiState = serde_json::from_value(saved).unwrap();
    assert_eq!(back.import_settings_width, w);
}

/// The Import tab culls with Library's toolbar (flags, rating, labels, sort, thumbnail size) and
/// filter bar when they are on, as the Library grid does.
#[test]
fn the_import_tab_draws_the_toolbar_and_the_filter_bar_when_they_are_on() {
    let mut h = app([1600.0, 900.0], "classic");
    h.app.ui.toolbar = true;
    h.app.ui.filter_bar = true;
    h.settle(SETTLE);
    let dir = folder("toolbar", 2);
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    assert_eq!(h.app.ui.tab, Tab::Import);
    assert!(has_widget(&mut h, "importToolbar"), "the toolbar under the browsed photos");
    assert!(has_widget(&mut h, "filterbar"), "the filter bar above them");
    let bar = widget_rect(&h, "importToolbar").unwrap();
    let settings = widget_rect(&h, "panel:importSettings").unwrap();
    assert!(bar.right() <= settings.left() + 1.0, "under the photos, beside the settings: {bar:?} {settings:?}");
    // off: neither
    h.app.ui.toolbar = false;
    h.app.ui.filter_bar = false;
    assert!(!has_widget(&mut h, "importToolbar") && !has_widget(&mut h, "filterbar"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `n` synthetic presets in `folders` folders (favourites when `favorite`), named so that one of
/// them, `long`, is much longer than the rest.
fn many_presets(h: &mut Headless, n: usize, folders: usize, favorite: bool, long: &str) {
    let base = lcc_develop::Preset { builtin: false, ..h.app.session.presets.iter().next().expect("a built-in preset").clone() };
    for i in 0..n {
        let name = if i == 0 { long.to_string() } else { format!("{i:05} Look") };
        h.app.session.presets.push(lcc_develop::Preset {
            id: format!("user.synthetic.{i}"),
            name,
            group: format!("{:03}-Folder", i % folders.max(1)),
            favorite,
            ..base.clone()
        });
    }
}

fn open_add_preset(h: &mut Headless) {
    exec(h, "dialog.import", json!({}));
    assert!(has_widget(h, "combo:importPresetAdd"));
    let r = h.request("ui.clickWidget", json!({"id": "combo:importPresetAdd"}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..4 {
        h.step();
    }
}

/// The import bar with main's Show All / Show Not Imported and Photo Grid / Grid / Detail segments and
/// Develop without Import: the browsed folder's own name always shows whole (its parents fold into
/// "…" first, then the segments go), and nothing of the bar runs over the panels beside it (at
/// 1000 wide the segments wrapped and spilled over Import from).
#[test]
fn the_import_bar_keeps_the_folder_name_whole_and_stays_inside_its_row() {
    let root = folder("barfit", 0);
    let deep = root.join("Archive").join("2026").join("November").join("Demo Shoot").join("Ceremony Selects");
    std::fs::create_dir_all(&deep).unwrap();
    let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
    let img = lcc_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 12) as u8, 40, 255]);
    std::fs::write(deep.join("p.png"), lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();
    for size in [[1600.0, 1000.0], [1000.0, 800.0]] {
        let mut h = app(size, "compact");
        exec(&mut h, "library.browse", json!({"path": deep.to_string_lossy()}));
        assert!(has_widget(&mut h, "label:importWhere"), "{size:?}: the bar is up");
        let bar = widget_rect(&h, "importBar").expect("the bar");
        let label = widget_rect(&h, "label:importWhere").expect("the folder's label");
        let name = h
            .view
            .ctx
            .fonts_mut(|f| f.layout_no_wrap("Ceremony Selects".to_string(), egui::FontId::proportional(12.5), egui::Color32::WHITE).size().x);
        assert!(label.width() + 0.5 >= name, "{size:?}: the folder's name shows whole ({} of {name})", label.width());
        let items: Vec<(String, egui::Rect)> = h
            .app
            .widgets
            .iter()
            .filter(|(w, r)| {
                (w.starts_with("button:browse")
                    || w.starts_with("button:importTake-")
                    || w.starts_with("importCrumb:")
                    || w.contains("importGo")
                    || w.contains("importDevelop"))
                    && r.width() > 0.0
            })
            .cloned()
            .collect();
        assert!(items.iter().any(|(w, _)| w.contains("importGo")), "{size:?}: the Import button is on the bar");
        for (w, r) in &items {
            assert!(bar.expand(0.5).contains_rect(*r), "{size:?}: {w} {r:?} stays inside the bar {bar:?}");
            if !w.starts_with("importCrumb:") {
                assert!(r.left() >= label.right() - 0.5, "{size:?}: {w} {r:?} clears the folder's label {label:?}");
            }
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

fn text_width(h: &Headless, text: &str) -> f32 {
    h.view.ctx.fonts_mut(|f| f.layout_no_wrap(text.to_string(), egui::FontId::proportional(12.0), egui::Color32::WHITE).size().x)
}

/// LR-PRESET-PICKER: the Add a preset list is as wide as its longest name (a narrow list showed
/// "01 Ava Ce…"), within the screen; Manage Presets… sits beside it.
#[test]
fn the_preset_picker_is_as_wide_as_its_longest_name_within_the_screen() {
    let long = "01 Ava Ceremony Warm Film Look With Soft Highlights";
    for layout in ["classic", "compact"] {
        let mut h = app([1600.0, 900.0], layout);
        h.settle(SETTLE);
        many_presets(&mut h, 20, 3, true, long);
        open_add_preset(&mut h);
        let row = widget_rect(&h, "presetPick:import-preset:user.synthetic.0").expect("the long preset's row");
        assert!(row.width() >= text_width(&h, long), "{layout}: row {row:?} for a {} pt name", text_width(&h, long));
        assert!(has_widget(&mut h, "button:importPresetsManage"), "{layout}: Manage Presets… beside the list");
    }
    // a name wider than the screen: the list stops at the screen's edge
    let mut h = app([900.0, 700.0], "classic");
    h.settle(SETTLE);
    many_presets(&mut h, 5, 1, true, &"Very long name ".repeat(40));
    open_add_preset(&mut h);
    let row = widget_rect(&h, "presetPick:import-preset:user.synthetic.0").expect("the long preset's row");
    assert!(row.width() <= 900.0, "{row:?}");
}

/// With 10 000 presets in 40 folders, all on show, the picker lays out only the rows in view.
#[test]
fn the_preset_picker_lays_out_only_the_rows_in_view_of_ten_thousand() {
    let mut h = app([1600.0, 900.0], "compact");
    h.settle(SETTLE);
    many_presets(&mut h, 10_000, 40, true, "First");
    open_add_preset(&mut h);
    let rows = h.app.widgets.iter().filter(|(w, _)| w.starts_with("presetPick:import-preset:")).count();
    assert!(rows > 0 && rows < 80, "{rows} rows laid out of 10 000");
}

/// The Manage Presets… button opens the Presets Manager.
#[test]
fn manage_presets_opens_the_presets_manager() {
    let mut h = app([1600.0, 900.0], "classic");
    h.settle(SETTLE);
    exec(&mut h, "dialog.import", json!({}));
    assert!(has_widget(&mut h, "button:importPresetsManage"));
    let r = h.request("ui.clickWidget", json!({"id": "button:importPresetsManage"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.ui.dialog.is_some(), "a dialog is open: {:?}", h.app.ui.dialog);
}

/// The Import bar names the browsed folder after its parents as a breadcrumb (OneSlips LR-IMP-WINDOW 6e4fa44d0): the
/// nearest parent opens with a click; on a narrow bar the parents that don't fit go under "…", whose menu lists them,
/// and each one opens.
#[test]
fn the_import_bar_shows_the_folder_as_a_breadcrumb() {
    let deep_root = folder("crumbs", 0);
    let deep = deep_root.join("a").join("b").join("c").join("d").join("e");
    std::fs::create_dir_all(&deep).unwrap();
    let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
    let img = lcc_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 12) as u8, 40, 255]);
    std::fs::write(deep.join("p.png"), lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();
    let crumbs: Vec<String> = crate::panels::grid::crumbs(&deep.to_string_lossy()).into_iter().map(|(_, to)| to).collect();
    let parent = crumbs.len() - 2;

    // wide enough for the bar's Develop, View and Show controls to leave the breadcrumb room
    let mut h = app([2400.0, 900.0], "compact");
    exec(&mut h, "library.browse", json!({"path": deep.to_string_lossy()}));
    assert_eq!(h.app.ui.tab, Tab::Import);
    let id = format!("importCrumb:{parent}");
    assert!(has_widget(&mut h, &id), "the nearest parent {id} is in the bar");
    assert!(!has_widget(&mut h, &format!("importCrumb:{}", crumbs.len() - 1)), "the folder itself is the bar's label");
    assert_eq!(h.request("ui.clickWidget", json!({"id": id}), T)["ok"], true);
    h.settle(SETTLE);
    let browsed = h.app.session.browse.as_ref().map(|b| std::path::PathBuf::from(&b.path));
    assert_eq!(browsed.as_deref(), Some(deep.parent().unwrap()), "the parent opens");

    // narrow: the top of the path hides under "…", and its menu opens a hidden folder
    let mut h = app([900.0, 700.0], "compact");
    exec(&mut h, "library.browse", json!({"path": deep.to_string_lossy()}));
    assert!(has_widget(&mut h, "importCrumb:more"), "the parents that don't fit are under …");
    assert!(!has_widget(&mut h, "importCrumb:0"), "the first folder is hidden until the menu opens");
    assert_eq!(h.request("ui.clickWidget", json!({"id": "importCrumb:more"}), T)["ok"], true);
    assert!(has_widget(&mut h, "importCrumb:0"), "the menu lists it");
    let top = crumbs.len() - 6;
    let id = format!("importCrumb:{top}");
    assert!(has_widget(&mut h, &id), "the menu lists {id}");
    assert_eq!(h.request("ui.clickWidget", json!({"id": id}), T)["ok"], true);
    h.settle(SETTLE);
    let browsed = h.app.session.browse.as_ref().map(|b| std::path::PathBuf::from(&b.path));
    assert_eq!(browsed.as_deref(), Some(deep_root.path()), "the hidden folder opens");
    let _ = std::fs::remove_dir_all(&deep_root);
}

/// A folder browsed for the first time starts with its subfolders in (a folder picked to import
/// means the tree under it), the bar carries the Include subfolders toggle the Local folder header
/// and the import review have, and a re-browse of the same folder keeps the choice the toggle made.
#[test]
fn the_tab_browses_subfolders_by_default_and_the_bar_toggle_rebrowses() {
    let mut h = app([1400.0, 900.0], "classic");
    h.app.session = lcc_engine::Session::new().with_fs();
    h.settle(SETTLE);
    let dir = folder("sub-default", 2);
    let nested = dir.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
    let img = lcc_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 12) as u8, 99, 255]);
    std::fs::write(nested.join("deep.png"), lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();

    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    assert!(h.app.session.browse.as_ref().is_some_and(|b| b.subfolders), "the default");
    assert_eq!(h.app.session.visible_cloned().len(), 3, "the nested photo shows");
    assert!(has_widget(&mut h, "check:includeSubfolders"), "the toggle is in the bar");

    // the toggle re-browses flat; the choice then survives a re-browse of the same folder
    let r = h.request("ui.clickWidget", json!({"id": "check:includeSubfolders"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.session.visible_cloned().len(), 2, "flat");
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    assert_eq!(h.app.session.visible_cloned().len(), 2, "kept across a re-browse");
    // and back on
    let r = h.request("ui.clickWidget", json!({"id": "check:includeSubfolders"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.session.visible_cloned().len(), 3, "the nested photo is back");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The Import button takes what the browse shows: with subfolders in (the default), importing from
/// the tab takes the photos in the folder's subfolders too, in place.
#[test]
fn importing_from_the_tab_takes_the_nested_photos() {
    let mut h = app([1400.0, 900.0], "classic");
    h.app.session = lcc_engine::Session::new().with_fs();
    h.settle(SETTLE);
    let dir = folder("sub-import", 1);
    let nested = dir.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
    let img = lcc_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 12) as u8, 99, 255]);
    std::fs::write(nested.join("deep.png"), lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();

    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    assert_eq!(h.app.session.visible_cloned().len(), 2, "the nested photo shows");
    exec(&mut h, "import.configure", json!({"take": "all", "previews": "none"}));
    let r = exec(&mut h, "import.start", json!({}));
    assert_eq!(r["importing"], 2, "{r}");
    h.step_until(SETTLE, |h| h.app.import.is_none());
    assert_eq!(h.app.session.catalog.photos().filter(|p| p.in_library()).count(), 2, "both, the nested one included");
    assert!(nested.join("deep.png").is_file(), "added in place: no file moved");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Hides Local's built-in places (Pictures, Desktop, Downloads, Home), so a test's own Local row
/// sits where the test expects it. On Windows the temporary folder is inside Home: browsing a
/// test folder opens Home's tree down to it, and Home's subfolders on that machine push the
/// test's row past the window's bottom, where a click can't land.
fn hide_home_places(h: &mut Headless) {
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        h.app.ui.hidden_locations.extend(
            ["Pictures", "Desktop", "Downloads", ""]
                .map(|sub| if sub.is_empty() { home.clone() } else { std::path::Path::new(&home).join(sub).to_string_lossy().to_string() }),
        );
    }
}

/// Issue #881: the Import tab's Show Not Imported stays with that tab. Browsing the same folder
/// from the sidebar's Local afterwards shows its library photos too, as Folders does.
#[test]
fn local_shows_a_library_folders_photos_after_show_not_imported() {
    let dir = folder("local881", 2);
    let path = dir.to_string_lossy().to_string();
    let mut h = app([1400.0, 900.0], "classic");
    hide_home_places(&mut h);
    h.settle(SETTLE);
    exec(&mut h, "library.import", json!({"paths": [path]}));
    exec(&mut h, "library.browse", json!({"path": path, "show": "notImported"}));
    assert_eq!(h.app.session.visible().len(), 0, "every file is in the library already");
    exec(&mut h, "local.addRoot", json!({"path": path}));
    let row = format!("source:local:{path}");
    assert!(h.step_until(T, |h| h.app.widgets.iter().any(|(w, _)| *w == row)), "the Local row");
    let r = h.request("ui.clickWidget", json!({"id": row}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.session.browse.as_ref().is_some_and(|b| !b.not_imported));
    assert_eq!(h.app.session.visible().len(), 2, "the library photos show");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The import bar says what its buttons do and nothing twice: "Import" and "Develop" (how many,
/// into where, and "without Import" are in their tooltips), and no Photo Grid / Grid / Detail
/// switch (the bottom bar has the view buttons).
#[test]
fn the_import_bar_has_short_labels_and_no_view_switch() {
    let mut h = app([1600.0, 1000.0], "classic");
    let dir = folder("bar-labels", 2);
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    assert!(has_widget(&mut h, "importGo") && has_widget(&mut h, "importDevelop"), "both buttons are on the bar");
    assert!(!h.app.widgets.iter().any(|(n, _)| n.starts_with("button:browseView")), "no view switch on the bar");
    let text = h.painted_text();
    assert!(text.iter().any(|t| t == "Import") && text.iter().any(|t| t == "Develop"), "{text:?}");
    assert!(!text.iter().any(|t| t.contains("Develop without Import") || (t.starts_with("Import ") && t.contains(" into "))), "{text:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Develop, Print, Export and Slideshow chosen from the Browse tab (the module bar, a shortcut, an
/// agent) open on the browsed photo chosen there, without importing it — and moving between them
/// never bounces back to the Browse tab.
#[test]
fn modules_from_the_browse_tab_act_on_the_browsed_photo_without_import() {
    use crate::module::ModuleId;
    for layout in ["classic", "modern"] {
        let mut h = app([1400.0, 900.0], layout);
        let dir = folder(&format!("in-place-{layout}"), 3);
        for (command, module) in [("module.print", ModuleId::Print), ("module.export", ModuleId::Export), ("module.develop", ModuleId::Develop)] {
            // Library from a module on a browsed photo is the Browse tab again
            exec(&mut h, "module.library", json!({}));
            exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
            assert_eq!(h.app.ui.tab, Tab::Import, "{layout} {command}");
            let pick = h.app.session.visible_cloned()[1];
            exec(&mut h, "library.select", json!({"ids": [pick.0]}));
            exec(&mut h, command, json!({}));
            h.settle(SETTLE);
            if layout == "modern" && module == ModuleId::Export {
                modern_export_dialog_on(&mut h, &[pick], layout);
                continue;
            }
            assert_eq!(h.app.ui.module, module, "{layout} {command}: the module opened and stayed");
            assert_eq!(h.app.ui.tab, Tab::Library, "{layout} {command}: the Browse tab handed over");
            assert_eq!(h.app.session.source, lcc_engine::LibrarySource::Folder, "{layout} {command}: the folder view stays on");
            assert_eq!(h.app.session.active(), Some(pick), "{layout} {command}: on the photo chosen in Browse");
            assert!(h.app.session.catalog.photos().all(|p| !p.in_library()), "{layout} {command}: nothing was imported");
        }
        // from Develop on the browsed photo, Print, Export and Slideshow stay where they are (no bounce)
        let open = h.app.session.active();
        for (command, module) in [("module.print", ModuleId::Print), ("module.export", ModuleId::Export), ("module.slideshow", ModuleId::Slideshow)] {
            exec(&mut h, command, json!({}));
            h.settle(SETTLE);
            if layout == "modern" && module == ModuleId::Export {
                modern_export_dialog_on(&mut h, &open.into_iter().collect::<Vec<_>>(), layout);
                assert_eq!(h.app.ui.module, ModuleId::Develop, "{layout}: the dialog over Develop, no bounce to Browse");
                continue;
            }
            assert_eq!(h.app.ui.module, module, "{layout} {command} from Develop");
            assert_eq!(h.app.ui.tab, Tab::Library, "{layout} {command} from Develop: no bounce to Browse");
            assert_eq!(h.app.session.source, lcc_engine::LibrarySource::Folder, "{layout} {command}");
            assert_eq!(h.app.session.selected(), open.into_iter().collect::<Vec<_>>(), "{layout} {command}: the open photo is acted on");
            exec(&mut h, "module.develop", json!({}));
        }
        // a module about the library (Map) goes to the library, not back to Browse
        exec(&mut h, "module.map", json!({}));
        h.settle(SETTLE);
        assert_eq!((h.app.ui.module, h.app.ui.tab), (ModuleId::Map, Tab::Library), "{layout}");
        assert_ne!(h.app.session.source, lcc_engine::LibrarySource::Folder, "{layout}: Map shows the library");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Modern keeps the Export dialog (no room for the module's column): it opens on the browsed
/// photos, the folder view still on.
fn modern_export_dialog_on(h: &mut Headless, ids: &[lcc_catalog::PhotoId], layout: &str) {
    assert!(matches!(h.app.ui.dialog, Some(Dialog::Export { .. })), "{layout}: the Export dialog");
    assert_eq!(h.app.session.source, lcc_engine::LibrarySource::Folder, "{layout}: the browsed photos");
    assert_eq!(h.app.session.selected(), ids.to_vec(), "{layout}: what the dialog exports");
    h.app.ui.dialog = None;
}

/// The module bar's Print, picked while browsing, opens Print on the browsed photo (it went to the
/// library first and lost it).
#[test]
fn the_module_bar_opens_print_on_the_browsed_photo() {
    let mut h = app([1400.0, 900.0], "classic");
    let dir = folder("bar-print", 2);
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    let pick = h.app.session.visible_cloned()[1];
    exec(&mut h, "library.select", json!({"ids": [pick.0]}));
    assert!(has_widget(&mut h, "module:print"));
    assert_eq!(h.request("ui.clickWidget", json!({"id": "module:print"}), T)["ok"], true);
    h.settle(SETTLE);
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Print);
    assert_eq!(h.app.session.source, lcc_engine::LibrarySource::Folder, "the browsed photo, not the library");
    assert_eq!(h.app.session.selected(), vec![pick]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file the library has already (Show All shows it in Browse) opens as the library's own photo:
/// the module on the library, the same photo selected.
#[test]
fn a_file_in_the_library_opens_as_its_catalog_photo() {
    let mut h = app([1400.0, 900.0], "classic");
    h.app.session = lcc_engine::Session::new().with_fs();
    h.settle(SETTLE);
    let dir = folder("in-library", 2);
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    let first = h.app.session.visible_cloned()[0];
    exec(&mut h, "photo.addToLibrary", json!({"ids": [first.0]}));
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy(), "show": "all"}));
    exec(&mut h, "library.select", json!({"ids": [first.0]}));
    exec(&mut h, "module.develop", json!({}));
    h.settle(SETTLE);
    assert_eq!((h.app.ui.module, h.app.ui.tab), (crate::module::ModuleId::Develop, Tab::Library));
    assert_ne!(h.app.session.source, lcc_engine::LibrarySource::Folder, "the library, not the browsed folder");
    assert_eq!(h.app.session.active(), Some(first), "the catalog photo");
    assert!(h.app.session.catalog.photo(first).is_some_and(|p| p.in_library()));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Export from a browsed photo developed without import stays in Export and writes the file
/// (through `app.export`, as the module's Export button does).
#[test]
fn export_from_a_browsed_photo_writes_a_file() {
    let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, usize)>::new()));
    let sink = written.clone();
    let services = crate::Services {
        png: None,
        write_shared: Some(std::sync::Arc::new(move |path: &str, data: &[u8]| {
            sink.lock().unwrap().push((path.to_string(), data.len()));
            Ok(())
        })),
        ..Default::default()
    };
    let mut h = Headless::new(LccApp::new(lcc_engine::Session::new().with_fs(), services), [1400.0, 900.0], 1.0);
    let dir = folder("export-local", 2);
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    exec(&mut h, "import.develop", json!({}));
    h.app.run("develop.set", json!({"control": "light.exposure", "value": 0.5})).unwrap();
    exec(&mut h, "module.export", json!({}));
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Export);
    let opts = lcc_engine::export::ExportOptions {
        format: lcc_engine::export::ExportFormat::Png,
        destination_mode: lcc_engine::export::DestinationMode::SpecificFolder,
        ..Default::default()
    };
    let out = dir.join("out");
    crate::panels::dialogs::start_export(&mut h.app, &opts, false, &lcc_engine::export::Resize::long_edge(16), 0, &out.to_string_lossy(), "")
        .unwrap();
    h.step_until(SETTLE, |h| h.app.export.is_none());
    h.settle(SETTLE);
    let files = written.lock().unwrap().clone();
    assert_eq!(files.len(), 1, "one file: {files:?} {:?}", h.app.ui.toasts);
    assert!(files[0].0.ends_with(".png") && files[0].1 > 0, "{files:?}");
    assert_eq!((h.app.ui.module, h.app.ui.tab), (crate::module::ModuleId::Export, Tab::Library), "still in Export");
    assert_eq!(h.app.session.source, lcc_engine::LibrarySource::Folder);
    assert!(h.app.session.catalog.photos().all(|p| !p.in_library()), "nothing was imported");
    let _ = std::fs::remove_dir_all(&dir);
}
