//! Modal application dialogs block background input while retaining their own controls.

use crate::headless::Headless;
use crate::{LccApp, Services};
use serde_json::json;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

fn demo(size: [f32; 2], ui: serde_json::Value) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, size, 1.0);
    let r = h.request("ui.set", ui, T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(Duration::from_secs(120));
    h
}

/// One session, several steps; each step was a test of its own and says what it checks.
#[test]
fn a_click_outside_closes_the_dialog_when_settings_allow_it_but_not_with_a_dropdown_open() {
    let mut h = demo([1400.0, 1000.0], json!({"view": "detail", "right": "edit"}));
    // a click outside closes the dialog when settings allow it
    // Settings → Close a dialog by clicking outside it: a click on the backdrop closes the dialog
    // like Esc, without reaching the panels below; a click inside it, or a drag that ends outside, keeps it.
    {
        let active = h.app.session.active().unwrap();
        let rating = h.app.session.catalog.photo(active).unwrap().rating;
        h.app.run("dialog.copySettings", json!({})).unwrap();
        h.step();
        // off by default: the background click is blocked and the dialog stays
        let r = h.request("ui.clickWidget", json!({"id": "star:bottom:5"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(h.app.ui.dialog.is_some(), "a stray click keeps the dialog unless Settings say otherwise");
        h.app.ui.settings.close_dialogs_outside = true;
        let r = h.request("ui.clickWidget", json!({"id": "copyGroup:none"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(h.app.ui.dialog.is_some(), "a click inside the dialog keeps it");
        let window = h.app.widgets.iter().find(|(id, _)| id == "dialog:window").map(|(_, r)| *r).unwrap();
        let r = h.request("ui.drag", json!({"x": window.center().x, "y": window.center().y, "toX": 10.0, "toY": 10.0}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(h.app.ui.dialog.is_some(), "a drag that ends outside the dialog keeps it");
        let r = h.request("ui.clickWidget", json!({"id": "star:bottom:5"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(h.app.ui.dialog.is_none(), "a click on the backdrop closes the dialog");
        assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, rating, "the click doesn't reach the panel below");
        // the click that opens a dialog lands outside its window; a quick one on a slow frame arrives
        // pressed and released in that one frame
        let gear = h.app.widgets.iter().find(|(id, _)| id == "icon:copyGear").map(|(_, r)| r.center()).unwrap();
        let press =
            |pressed| egui::Event::PointerButton { pos: gear, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        h.events.extend([egui::Event::PointerMoved(gear), press(true), press(false)]);
        h.step();
        h.step();
        assert!(h.app.ui.dialog.is_some(), "the click that opens a dialog doesn't close it");
        let r = h.request("ui.clickWidget", json!({"id": "star:bottom:5"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(h.app.ui.dialog.is_none(), "the next click on the backdrop does");
    }
    // a click outside an open dropdown keeps the dialog
    // With a dropdown open in the dialog, a click on the backdrop only closes the dropdown, even when
    // it arrives pressed and released in one frame (the dropdown closes while the dialog is drawn).
    {
        h.app.ui.settings.close_dialogs_outside = true;
        h.app.run("dialog.export", json!({})).unwrap();
        h.step();
        h.step();
        // (a wide window lists the presets beside the settings: the destination dropdown)
        let r = h.request("ui.clickWidget", json!({"id": "combo:exportDestinationMode"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(egui::Popup::is_any_open(&h.view.ctx), "the dropdown is open");
        let at = egui::pos2(10.0, 990.0);
        let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        h.events.extend([egui::Event::PointerMoved(at), press(true), press(false)]);
        h.step();
        h.step();
        assert!(!egui::Popup::is_any_open(&h.view.ctx), "the click closes the dropdown");
        assert!(h.app.ui.dialog.is_some(), "but not the dialog");
    }
}

/// One session, several steps; each step was a test of its own and says what it checks.
#[test]
fn modal_dialogs_close_menus_and_block_background_shortcuts_and_clicks() {
    let mut h = demo([1400.0, 1000.0], json!({"view": "detail", "right": "edit"}));
    // modal dialogs close background menus
    {
        let r = h.request("ui.clickWidget", json!({"id": "menu:Photo"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(egui::Popup::is_any_open(&h.view.ctx));
        h.app.run("dialog.copySettings", json!({})).unwrap();
        h.step();
        assert!(!egui::Popup::is_any_open(&h.view.ctx), "background menus cannot remain above the modal window");
        h.app.ui.dialog = None;
        h.step();
    }
    // modal dialogs block background shortcuts and keep escape
    {
        h.app.session.execute("photo.rate", &json!({"ids": [h.app.session.active().unwrap().0], "rating": 0})).unwrap();
        let active = h.app.session.active().unwrap();
        let rating = h.app.session.catalog.photo(active).unwrap().rating;
        h.app.run("dialog.copySettings", json!({})).unwrap();
        h.step();
        for key in ["5", "ArrowRight", "J"] {
            let r = h.request("ui.key", json!({"key": key}), T);
            assert_eq!(r["ok"], true, "{r}");
            h.step();
        }
        assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, rating);
        assert_eq!(h.app.session.active(), Some(active));
        assert!(!h.app.ui.show_clipping);
        let r = h.request("ui.key", json!({"key": "Escape"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(h.app.ui.dialog.is_none());
        let r = h.request("ui.key", json!({"key": "5"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, 5, "closing restores shortcuts");
    }
    // modal dialogs block background clicks and preserve choices
    // Application dialogs block the toolbar, including the gear that would reopen Copy Settings
    // and discard its choices. Their own controls work, and closing restores background input.
    {
        h.app.session.execute("photo.rate", &json!({"ids": [h.app.session.active().unwrap().0], "rating": 0})).unwrap();
        let active = h.app.session.active().unwrap();
        let rating = h.app.session.catalog.photo(active).unwrap().rating;
        h.app.run("dialog.copySettings", json!({})).unwrap();
        // Also exercise the first frame of a newly opened dialog.
        let r = h.request("ui.clickWidget", json!({"id": "star:bottom:5"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, rating, "the dialog blocks background ratings");
        let r = h.request("ui.clickWidget", json!({"id": "copyGroup:none"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        let choices = h.app.ui.dialog.clone();
        assert!(matches!(&choices, Some(crate::state::Dialog::CopySettings { groups, .. }) if groups.is_empty()));
        let r = h.request("ui.clickWidget", json!({"id": "icon:copyGear"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert_eq!(h.app.ui.dialog, choices, "a background click cannot reopen the dialog and reset its choices");
        let r = h.request("ui.clickWidget", json!({"id": "button:dialogCancel"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(h.app.ui.dialog.is_none());
        let r = h.request("ui.clickWidget", json!({"id": "star:bottom:5"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, 5, "closing restores toolbar input");
        // A different dialog uses the same barrier.
        h.app.run("app.settings", json!({})).unwrap();
        h.step();
        let r = h.request("ui.clickWidget", json!({"id": "star:bottom:1"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, 5);
        // (Settings fills the window; its ‹ closes it)
        let r = h.request("ui.clickWidget", json!({"id": "settings:back"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(h.app.ui.dialog.is_none());
    }
}

/// One session, several steps; each step was a test of its own and says what it checks.
#[test]
fn delete_rejected_asks_how_and_rename_asks_for_a_presets_text() {
    let mut h = demo([1400.0, 900.0], json!({"view": "photoGrid"}));
    // delete rejected photos asks how
    // Photo ▸ Delete Rejected Photos… always asks, with the number of rejects and how: removing them
    // (the default, undoable) or trashing their files too. Ported from OneSlips' LR-DELETE-REJECTED.
    {
        let ids: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
        h.app.session.execute("photo.flag", &json!({"ids": ids, "flag": "none"})).unwrap();
        h.app.session.execute("photo.flag", &json!({"ids": [ids[0], ids[1]], "flag": "reject"})).unwrap();
        let total = h.app.session.visible_cloned().len();
        crate::menubar::run_item(&mut h.app, "library.deleteRejected", json!({})).unwrap();
        assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::DeleteRejected { count: 2, recycle: false })), "{:?}", h.app.ui.dialog);
        h.step();
        let r = h.request("ui.dialog.confirm", json!({}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(h.app.ui.dialog.is_none(), "{:?}", h.app.ui.dialog);
        assert_eq!(h.app.session.visible_cloned().len(), total - 2);
        crate::menubar::run_item(&mut h.app, "library.deleteRejected", json!({})).unwrap();
        assert!(h.app.ui.dialog.is_none(), "nothing left to delete: no dialog");
    }
    // rename with a text preset asks for the text
    // Rename Photos: a naming preset sets the template, a `{text:…}` tag gets its own field, and the
    // rename uses what was typed. Ported from OneSlips' LR-LIB-RENAME.
    {
        let first = h.app.session.visible_cloned()[0];
        h.app.session.execute("library.select", &json!({"ids": [first.0]})).unwrap();
        h.app.ui.dialog = Some(crate::state::Dialog::Rename { template: "{name}".into(), start: 1, texts: Default::default() });
        h.settle(Duration::from_secs(60));
        // the built-in "Custom Text - Number" preset is the fourth
        let r = h.request("ui.clickWidget", json!({"id": "button:renamePreset-3"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(Duration::from_secs(60));
        assert!(h.app.widgets.iter().any(|(w, _)| w == "field:renameText-0"), "a field for the text tag");
        if let Some(crate::state::Dialog::Rename { texts, template, .. }) = h.app.ui.dialog.as_mut() {
            assert!(template.contains("{text:"), "{template}");
            texts.insert("Name".into(), "RA".into());
        }
        let r = h.request("ui.dialog.confirm", json!({}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(
            h.app.session.catalog.photo(first).is_some_and(|p| p.file_name.starts_with("RA-")),
            "{:?}",
            h.app.session.catalog.photo(first).map(|p| p.file_name.clone())
        );
    }
}

/// Show Alternatives: the bar over the frames around a photo appears, and Back leaves them.
#[test]
fn alternatives_bar_shows_and_goes_back() {
    let dir = lcc_testkit::TempDir::new("lcc-ui-alternatives");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut paths = Vec::new();
    for i in 0..5u8 {
        let p = dir.join(format!("IMG_{i:02}.jpg"));
        let img = lcc_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 13) as u8, i * 40, 255]);
        std::fs::write(&p, lcc_engine::export::encode_image(&img, &lcc_engine::export::ExportOptions::default()).unwrap()).unwrap();
        paths.push(p.to_string_lossy().into_owned());
    }
    let mut session = lcc_engine::Session::new().with_fs();
    session.execute("library.import", &json!({"paths": [paths[2]]})).unwrap();
    let app = LccApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.app.ui.view = crate::state::ViewMode::Detail;
    let id = h.app.session.visible_cloned()[0];
    h.app.session.execute("photo.showAlternatives", &json!({"id": id.0})).unwrap();
    for _ in 0..3 {
        h.step();
    }
    assert!(h.app.widgets.iter().any(|(w, _)| w == "alternatives:bar"));
    h.settle(Duration::from_secs(60));
    let r = h.request("ui.clickWidget", json!({"id": "button:alternativesBack"}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..4 {
        h.step();
    }
    assert!(h.app.session.alternatives.is_none(), "Back leaves the alternatives");
    assert_eq!(h.app.session.visible_cloned(), vec![id]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The windows that sit over the app and wait to be closed but aren't a `Dialog`: each one takes the
/// keyboard and the pointer while it is open (a plain key or a click doesn't rate the photo behind
/// it, and the native menu bar would drop its shortcuts too, as for a dialog), and gives them back
/// once it closes.
#[test]
fn every_window_that_waits_over_the_app_keeps_the_keys_from_the_photos() {
    use crate::keyboard::{MenuChoice, Owner, menu_choice, owner};
    type Step = fn(&mut Headless);
    // (the ⌘/ shortcuts overlay closes on any key: `tests_module_help` checks that key runs nothing)
    let rows: [(&str, Step, Step); 28] = [
        ("the quit question", |h| h.app.quit_prompt = Some(crate::QuitPrompt::Unsaved("changes not saved".into())), |h| h.app.quit_prompt = None),
        ("the settings-file notice", |h| h.app.notices.push("settings file unreadable".into()), |h| h.app.notices.clear()),
        (
            "Refine Photos (a catalog dialog)",
            |h| h.app.catalog_ui.dialog = Some(crate::catalog_ui::CatalogDialog::Refine { picks: 0, unflagged: 0 }),
            |h| click(h, "button:catalogCancel"),
        ),
        (
            "New Catalog (a catalog dialog)",
            |h| h.app.catalog_ui.dialog = Some(crate::catalog_ui::CatalogDialog::NewCatalog { parent: String::new(), name: "x".into() }),
            |h| click(h, "button:catalogCancel"),
        ),
        (
            "the catalog chooser over the open library",
            |h| {
                let mut p = crate::panels::library_problem::LibraryProblem::choose_later(&h.app.ui.settings);
                p.chooser.iter_mut().for_each(|c| c.over = Some(1));
                h.app.library_problem = Some(p);
            },
            |h| h.app.library_problem = None,
        ),
        (
            "the Export sheet",
            |h| {
                h.app.ui.modern.enabled = true;
                h.app.run("modern.exportSheet", json!({"on": true})).unwrap();
            },
            |h| {
                h.app.run("modern.exportSheet", json!({"on": false})).unwrap();
                h.app.ui.modern.enabled = false;
            },
        ),
        ("Identity Plate Editor", |h| drop(h.app.run("dialog.identityPlateSetup", json!({})).unwrap()), |h| click(h, "identity-plate-setup:close")),
        ("Find Duplicates", |h| drop(h.app.run("library.findDuplicates", json!({})).unwrap()), |h| click(h, "button:duplicates.close")),
        ("Review Comments", |h| h.app.review_comments.open = true, |h| click(h, "review-comments:close")),
        (
            "Import Warnings",
            |h| h.app.import_report = Some(crate::import_report::Report { open: true, ..crate::import_report::Report::new(1, &[]) }),
            |h| click(h, "button:importWarnings.close"),
        ),
        (
            "the camera-roll question",
            |h| {
                h.app.ui.camera_roll_asked = false;
                h.app.services.camera_roll = Some(Box::new(|| Err("no camera roll here".into())));
            },
            |h| click(h, "button:cameraRollNotNow"),
        ),
        ("Plug-in Manager", |h| drop(h.app.run("plugins.manager", json!({"open": true})).unwrap()), |h| click(h, "plugin-manager:close")),
        ("Plug-in Marketplace", |h| drop(h.app.run("plugins.marketplace", json!({"open": true})).unwrap()), |h| click(h, "plugin-marketplace:close")),
        (
            "the Marketplace's signer question",
            |h| {
                h.app.run("plugins.marketplace", json!({"open": true})).unwrap();
                crate::panels::marketplace::ask_signer();
            },
            // (Esc, above, answered the question; its Cancel: `the_marketplace_keeps_its_own_controls`)
            |h| {
                let (open, asking, _) = crate::panels::marketplace::showing();
                assert!(open && !asking, "Esc answers the signer question and leaves the Marketplace open");
                click(h, "plugin-marketplace:close");
            },
        ),
        ("Install Plug-in", |h| crate::panels::plugins::show_for_test(&h.view.ctx, "approve"), |h| click(h, "button:pluginInstallCancel")),
        ("A plug-in's own window", |h| crate::panels::plugins::show_for_test(&h.view.ctx, "dialog"), |h| click(h, "button:pluginRunCancel")),
        (
            "a plug-in update's question",
            |h| crate::panels::plugins::show_for_test(&h.view.ctx, "update"),
            |h| {
                click(h, "button:pluginUpdateCancel");
                click(h, "plugin-manager:close");
            },
        ),
        ("Getting Started", |h| crate::onboarding::open(&mut h.app, 0), |h| drop(crate::onboarding::close(&mut h.app, false))),
        (
            "a plug-in's permission question",
            |h| {
                let request = lcc_plugins::Request { group: lcc_plugins::Group::Catalog, value: None };
                let ask =
                    lcc_engine::cmd::plugins::PluginAsk { plugin: "p".into(), plugin_name: "P".into(), request, rerun: json!({}), prepare: false };
                h.app.session.plugin_asks.push(ask);
            },
            |h| {
                h.step();
                let r = h.request("ui.clickWidget", json!({"id": "button:pluginAskDeny"}), T);
                assert_eq!(r["ok"], true, "{r}");
            },
        ),
        ("Edit In Presets", |h| drop(h.app.run("dialog.editInPresets", json!({})).unwrap()), |h| click(h, "edit-in-presets:close")),
        ("Edit Photo", |_| crate::edit_in::ask_for_test(), |h| click(h, "button:editInCancel")),
        (
            "Edited Version Saved Again",
            |h| crate::panels::connections::ask_resave(h.app.session.active().unwrap().0),
            |h| click(h, "immich-resave:close"),
        ),
        ("Share the New Collection?", |h| h.app.peer_sync.set_status_for_test(new_collection_undecided()), |h| click(h, "button:syncUndecidedLater")),
        ("Tethered Capture Settings", |h| drop(h.app.run("tether.start", json!({})).unwrap()), |h| click(h, "tether-settings:close")),
        ("a Dialog", |h| drop(h.app.run("dialog.copySettings", json!({})).unwrap()), |h| h.app.ui.dialog = None),
        ("Edit Smart Collection…", |h| drop(h.app.run("dialog.smartAlbum", json!({"name": "Dates"})).unwrap()), |h| h.app.ui.dialog = None),
        (
            "New Collection (a text field with the focus)",
            |h| {
                h.app.run("dialog.newAlbum", json!({})).unwrap();
                h.step();
                h.step();
            },
            |h| h.app.ui.dialog = None,
        ),
        ("Settings", |h| drop(h.app.run("app.settings", json!({})).unwrap()), |h| h.app.ui.dialog = None),
    ];
    // the keys of the report: a rating, and the three menu bar shortcuts (⌘= / ⌘- zoom the photo,
    // ⌘⌫ deletes the rejected photos), which AppKit hands to the menu bar before the window
    let menu = [("view.zoomIn", "Cmd+=", "Equals"), ("view.zoomOut", "Cmd+-", "Minus"), ("library.deleteRejected", "Cmd+Delete", "Backspace")];
    let mut leaks = Vec::new();
    // the ones Esc closes, as before; the others wait for their own buttons (and Esc goes no
    // further: it doesn't take the loupe back to the grid behind them)
    let esc_closes = [
        "Refine Photos (a catalog dialog)",
        "New Catalog (a catalog dialog)",
        "the catalog chooser over the open library",
        "the Export sheet",
        "Plug-in Manager",
        "Plug-in Marketplace",
        // (the manager under it closes on Esc, and the question goes with it)
        "a plug-in update's question",
        "Getting Started",
        "Edit In Presets",
        "Edit Photo",
        "Edited Version Saved Again",
        "Share the New Collection?",
        "Tethered Capture Settings",
        "a Dialog",
        "Edit Smart Collection…",
        "New Collection (a text field with the focus)",
        "Settings",
    ];
    let fresh = || {
        let h = demo([1400.0, 1000.0], json!({"view": "detail", "right": "edit"}));
        assert_eq!(owner(&h.app, &h.view.ctx), Owner::Window, "nothing has the keyboard before the test");
        let active = h.app.session.active().unwrap();
        (h, active)
    };
    // every modal windows.json lists has a row here (or an owner check below: the catalog opening or
    // upgrading)
    let owner_checked = ["the catalog opening", "the catalog upgrading"];
    for w in crate::window_list::WINDOWS.iter().filter(|w| w.modal) {
        let survey = w.survey.as_str();
        assert!(
            rows.iter().any(|(name, ..)| *name == survey) || owner_checked.contains(&survey),
            "{}: windows.json lists it as a modal, but the survey has no row \"{survey}\"",
            w.name
        );
    }
    for &(name, open, close) in &rows {
        // a session of its own: a key that got through must not change what the next row sees
        let (mut h, active) = fresh();
        h.app.session.execute("photo.rate", &json!({"ids": [active.0], "rating": 0})).unwrap();
        h.app.session.execute("photo.flag", &json!({"ids": [active.0], "flag": "reject"})).unwrap();
        // the toolbar's stars, for a click behind the window (the Modern layout's Export sheet has
        // no toolbar behind it: a sheet covers the photo)
        let star = |h: &Headless, id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| r.center());
        open(&mut h);
        let behind = star(&h, "star:bottom:5");
        let (zoom, dialog) = (h.app.ui.zoom, h.app.ui.dialog.clone());
        let keys = owner(&h.app, &h.view.ctx);
        let typing = h.view.ctx.egui_wants_keyboard_input();
        for (command, sc, _) in menu {
            if menu_choice(keys, command, Some(sc), typing) != MenuChoice::Drop {
                leaks.push(format!("{name}: the menu bar would run {sc} behind it"));
            }
        }
        let r = h.request("ui.key", json!({"key": "5"}), T);
        assert_eq!(r["ok"], true, "{name}: {r}");
        h.step();
        for (_, _, key) in menu {
            let r = h.request("ui.key", json!({"key": key, "cmd": true}), T);
            assert_eq!(r["ok"], true, "{name}: {r}");
            h.step();
        }
        let photo = h.app.session.catalog.photo(active).unwrap();
        if photo.rating != 0 {
            leaks.push(format!("{name}: a plain key rated the photo behind it"));
        }
        if photo.deleted || h.app.ui.zoom != zoom || h.app.ui.dialog != dialog {
            leaks.push(format!("{name}: a command key reached the photo behind it"));
        }
        // (the window's own layer: it keeps out what isn't in the panels, a tap on the Modern grid;
        // the dimmed ones, the quit question, the notice, the chooser and the sheet, block with
        // their dimming instead)
        let dims = ["the quit question", "the settings-file notice", "the catalog chooser over the open library", "the Export sheet"];
        if dialog.is_none() && !dims.contains(&name) && h.view.ctx.memory(|m| m.top_modal_layer()).is_none() {
            leaks.push(format!("{name}: it doesn't take the modal layer"));
        }
        if let Some(at) = behind {
            let r = h.request("ui.click", json!({"x": at.x, "y": at.y}), T);
            assert_eq!(r["ok"], true, "{name}: {r}");
            h.step();
            if h.app.session.catalog.photo(active).unwrap().rating != 0 {
                leaks.push(format!("{name}: a click behind it rated the photo"));
            }
        }
        let view = h.app.ui.view;
        let r = h.request("ui.key", json!({"key": "Escape"}), T);
        assert_eq!(r["ok"], true, "{name}: {r}");
        h.step();
        h.step();
        let closed = owner(&h.app, &h.view.ctx) != Owner::Modal;
        if closed != esc_closes.contains(&name) {
            leaks.push(format!("{name}: Esc {} it", if closed { "closed" } else { "didn't close" }));
        }
        if h.app.ui.view != view {
            leaks.push(format!("{name}: Esc also left the loupe behind it"));
        }
        if !closed {
            close(&mut h);
        }
        h.step();
        let keys = owner(&h.app, &h.view.ctx);
        if keys != Owner::Window {
            leaks.push(format!("{name}: closing it leaves the keyboard with {keys:?}"));
        }
        for (command, sc, _) in menu {
            if menu_choice(keys, command, Some(sc), false) != MenuChoice::Run {
                leaks.push(format!("{name}: once it closes, {sc} still doesn't run"));
            }
        }
        let zoom = h.app.ui.zoom;
        let r = h.request("ui.key", json!({"key": "Equals", "cmd": true}), T);
        assert_eq!(r["ok"], true, "{name}: {r}");
        h.step();
        if h.app.ui.zoom == zoom {
            leaks.push(format!("{name}: once it closes, ⌘= doesn't zoom the photo"));
        }
        if let (Some(_), Some(at)) = (behind, star(&h, "star:bottom:4")) {
            let r = h.request("ui.click", json!({"x": at.x, "y": at.y}), T);
            assert_eq!(r["ok"], true, "{name}: {r}");
            h.step();
            if h.app.session.catalog.photo(active).unwrap().rating != 4 {
                leaks.push(format!("{name}: once it closes, a click on a star doesn't rate the photo"));
            }
        }
    }
    assert!(leaks.is_empty(), "every window that waits over the app keeps the keys:\n{}", leaks.join("\n"));

    // …and with nothing open, the same keys do reach the photo (so the checks above can fail)
    let (mut h, active) = fresh();
    h.app.session.execute("photo.flag", &json!({"ids": [active.0], "flag": "reject"})).unwrap();
    let zoom = h.app.ui.zoom;
    for key in ["5", "Equals"] {
        let r = h.request("ui.key", json!({"key": key, "cmd": key != "5"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
    }
    assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, 5, "with nothing open, 5 rates the photo");
    assert_ne!(h.app.ui.zoom, zoom, "with nothing open, ⌘= zooms the photo");
    let r = h.request("ui.key", json!({"key": "Backspace", "cmd": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(
        h.app.session.catalog.photo(active).unwrap().deleted || h.app.ui.dialog.is_some(),
        "with nothing open, ⌘⌫ deletes the rejected photo (or asks first)"
    );
    h.app.ui.dialog = None;

    // the full-screen preview draws none of the tool windows: one opened meanwhile (Find Duplicates
    // from the menu bar, an import's warnings) waits, and the keys stay with the photo on show
    h.app.ui.fullscreen = true;
    h.app.run("library.findDuplicates", json!({})).unwrap();
    assert_eq!(owner(&h.app, &h.view.ctx), Owner::Window, "a tool window that isn't drawn leaves the keys alone");
    h.app.ui.fullscreen = false;
    assert_eq!(owner(&h.app, &h.view.ctx), Owner::Modal, "and takes them once it shows");
    h.app.duplicates = None;
    // (the Plug-in Marketplace, too)
    h.app.ui.fullscreen = true;
    h.app.run("plugins.marketplace", json!({"open": true})).unwrap();
    h.step();
    assert_eq!(owner(&h.app, &h.view.ctx), Owner::Window, "the Marketplace isn't drawn in full screen and leaves the keys alone");
    h.app.ui.fullscreen = false;
    assert_eq!(owner(&h.app, &h.view.ctx), Owner::Modal, "the Marketplace takes the keys once it shows");
    h.app.run("plugins.marketplace", json!({"open": false})).unwrap();
    h.step();
    // the questions and the Edit In, Immich and tethering windows show in full screen too, and keep
    // the keys there
    // (their state outlives a session: each closes before the next)
    for (name, id) in [
        ("Edit In Presets", "edit-in-presets"),
        ("Edit Photo", "edit-in-ask"),
        ("Edited Version Saved Again", "immich-resave"),
        ("Share the New Collection?", "syncUndecided"),
        ("Tethered Capture Settings", "tether-settings"),
    ] {
        let Some(&(_, open, close)) = rows.iter().find(|row| row.0 == name) else { panic!("the survey has a row \"{name}\"") };
        let (mut h, _) = fresh();
        h.app.ui.fullscreen = true;
        open(&mut h);
        h.settle(T);
        assert_eq!(owner(&h.app, &h.view.ctx), Owner::Modal, "{name}: in full screen it keeps the keys");
        let top = h.view.ctx.memory(|m| m.top_modal_layer()).map(|l| l.id);
        assert_eq!(top, Some(egui::Id::new(id)), "{name}: it shows in full screen, holding the pointer");
        close(&mut h);
        h.step();
        assert_eq!(owner(&h.app, &h.view.ctx), Owner::Window, "{name}: closed, it gives the keys back");
    }

    // the catalog opening or upgrading (the owner only: with no open really running, the first
    // frame turns either into the window of a catalog that won't open)
    for (name, opening) in owner_checked.into_iter().zip([true, false]) {
        let p = crate::panels::library_problem::LibraryProblem::new("/nowhere", "");
        h.app.library_problem = Some(crate::panels::library_problem::LibraryProblem { opening, upgrading: !opening, ..p });
        assert_eq!(owner(&h.app, &h.view.ctx), Owner::Modal, "{name}: nothing runs behind it");
        h.app.library_problem = None;
    }

    // with no library open behind them, the launch windows leave the menu bar alone: it is their
    // way to Settings, About and Check for Updates (issue #929), and to another catalog
    let launch: [(&str, Step); 3] = [
        ("Welcome", |h| {
            h.app.catalog_ui.dialog = Some(crate::catalog_ui::CatalogDialog::Welcome {
                parent: String::new(),
                name: "x".into(),
                demo: false,
                updates: false,
                foreign: Vec::new(),
            })
        }),
        ("a catalog that couldn't be opened", |h| {
            h.app.library_problem = Some(crate::panels::library_problem::LibraryProblem::new("/nowhere", "unreadable"))
        }),
        ("the catalog chooser at launch", |h| {
            h.app.library_problem = Some(crate::panels::library_problem::LibraryProblem::choose_later(&h.app.ui.settings))
        }),
    ];
    for (name, open) in launch {
        open(&mut h);
        assert_eq!(owner(&h.app, &h.view.ctx), Owner::Window, "{name}: the menu bar still works over it");
        h.app.catalog_ui.dialog = None;
        h.app.library_problem = None;
    }
}

/// The windows `windows.json` makes exceptions leave the keys with the photos: with Actions (it
/// records them), Roles (it follows the photo) or Live View (shooting) open, 5 still rates the photo.
#[test]
fn the_exception_windows_leave_the_keys_with_the_photos() {
    use crate::keyboard::{Owner, owner};
    let shows = |h: &Headless, id: &str| h.view.ctx.memory(|m| m.areas().visible_layer_ids()).iter().any(|l| l.id == egui::Id::new(id));
    let rates = |h: &mut Headless, name: &str| {
        let active = h.app.session.active().unwrap();
        h.app.session.execute("photo.rate", &json!({"ids": [active.0], "rating": 0})).unwrap();
        assert_eq!(owner(&h.app, &h.view.ctx), Owner::Window, "{name}: the keys stay with the photos");
        let r = h.request("ui.key", json!({"key": "5"}), T);
        assert_eq!(r["ok"], true, "{name}: {r}");
        h.step();
        assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, 5, "{name}: with it open, 5 rates the photo");
    };
    let mut h = demo([1400.0, 1000.0], json!({"view": "detail", "right": "edit"}));
    h.app.run("panel.actions", json!({"show": true})).unwrap();
    h.settle(T);
    assert!(shows(&h, "actions-panel"), "Actions is open");
    rates(&mut h, "Actions");
    h.app.run("panel.actions", json!({"show": false})).unwrap();
    // (the masking panel draws it, under a mask)
    let r = h.request("engine.execute", json!({"command": "panel.masking"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.run("mask.add", json!({"kind": "radial"})).unwrap();
    h.app.ui.roles_window = true;
    h.settle(T);
    assert!(shows(&h, "rolesWindow"), "Roles is open");
    rates(&mut h, "Roles");

    // Live View: the simulated Nikon (it has live view), tethered into a catalog on disk
    let dir = lcc_testkit::TempDir::new("lcc-ui-live-view");
    let mut session = lcc_engine::Session::new().with_fs();
    session.open_library(dir.join("lib"), false).unwrap();
    let photo = dir.join("a.jpg");
    let img = lcc_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 13) as u8, 90, 255]);
    std::fs::write(&photo, lcc_engine::export::encode_image(&img, &lcc_engine::export::ExportOptions::default()).unwrap()).unwrap();
    session.execute("library.import", &json!({"paths": [photo.to_string_lossy()]})).unwrap();
    let mut h = Headless::new(LccApp::new(session, Services { png: None, ..Default::default() }), [1400.0, 1000.0], 1.0);
    h.app.ui.view = crate::state::ViewMode::Detail;
    h.app.run("tether.connect", json!({"device": "sim-nikon"})).unwrap();
    h.settle(T);
    click(&mut h, "tetherLiveView");
    h.settle(T);
    assert!(shows(&h, "tether-live-window"), "Live View is open");
    rates(&mut h, "Live View");
    let _ = h.app.run("tether.disconnect", json!({}));
    let _ = h.app.run("tether.stop", json!({}));
}

/// A syncing catalog with a new collection to ask about (Share the New Collection?).
fn new_collection_undecided() -> serde_json::Value {
    let group = json!({"id": "g", "mode": "catalogue", "keeper": "me", "invited": [], "devices": []});
    json!({"running": false, "paired": [],
        "catalogue": {"name": "Main", "photos": 0, "unkeyed": 0, "group": group, "collections": [], "undecided": [{"album": 7, "name": "New"}]}})
}

/// Over the Plug-in Marketplace its own controls still work: the search field takes what is typed
/// (5 there rates nothing), and its signer question's Cancel answers the question alone.
#[test]
fn the_marketplace_keeps_its_own_controls() {
    use crate::panels::marketplace::{ask_signer, showing};
    let mut h = demo([1400.0, 1000.0], json!({"view": "detail", "right": "edit"}));
    let active = h.app.session.active().unwrap();
    h.app.session.execute("photo.rate", &json!({"ids": [active.0], "rating": 0})).unwrap();
    h.app.run("plugins.marketplace", json!({"open": true})).unwrap();
    h.settle(T);
    click(&mut h, "marketplaceSearch");
    let r = h.request("ui.text", json!({"text": "5ab"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(showing().2, "5ab", "the search field takes the typing");
    assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, 0, "typing in the search field rates nothing behind it");
    ask_signer();
    h.settle(T);
    // the question holds the pointer, not the Marketplace under it
    let top = h.view.ctx.memory(|m| m.top_modal_layer()).map(|l| l.id);
    assert_eq!(top, Some(egui::Id::new("plugin-marketplace").with("signer")), "the signer question holds the modal layer");
    click(&mut h, "button:marketplaceSignerCancel");
    let (open, asking, _) = showing();
    assert!(open && !asking, "Cancel answers the question and leaves the Marketplace open");
}

/// In a window that closes on Esc, the first Esc in its text field only leaves the field.
#[test]
fn esc_in_a_marketplace_search_leaves_the_field_first() {
    use crate::panels::marketplace::showing;
    let mut h = demo([1400.0, 1000.0], json!({"view": "detail", "right": "edit"}));
    h.app.run("plugins.marketplace", json!({"open": true})).unwrap();
    h.settle(T);
    click(&mut h, "marketplaceSearch");
    let r = h.request("ui.text", json!({"text": "abc"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(showing().2, "abc");
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(showing().0, "the first Esc leaves the search field, and the Marketplace stays open");
    assert_eq!(showing().2, "", "the field's Esc takes back what was typed");
    assert!(!h.view.ctx.egui_wants_keyboard_input(), "the field let go of the keys");
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(!showing().0, "the next Esc closes it");
}

/// Holding \ peeks at the Before view only when the main window has the keyboard: behind a
/// window that waits over the app, or while typing in a text field, it shows nothing.
#[test]
fn holding_backslash_peeks_at_before_only_when_the_window_has_the_keys() {
    let mut h = demo([1400.0, 1000.0], json!({"view": "detail", "right": "edit"}));
    h.settle(T);
    // (with the keys, the press also runs the shortcut of `\`, the Before view, which stays on:
    // back off before each look)
    let peeks = |h: &mut Headless| {
        h.app.ui.before_after = crate::state::BeforeAfter::Off;
        h.step();
        let r = h.request("ui.key", json!({"key": "\\", "action": "down"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        let before = h.painted_text().iter().any(|t| t == "Before");
        let r = h.request("ui.key", json!({"key": "\\", "action": "up"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        h.app.ui.before_after = crate::state::BeforeAfter::Off;
        before
    };
    assert!(peeks(&mut h), "with nothing open, holding \\ shows Before");
    type Step = fn(&mut Headless);
    let over: [(&str, Step, Step); 3] = [
        ("Find Duplicates", |h| drop(h.app.run("library.findDuplicates", json!({})).unwrap()), |h| h.app.duplicates = None),
        ("a Dialog", |h| drop(h.app.run("dialog.copySettings", json!({})).unwrap()), |h| h.app.ui.dialog = None),
        (
            "Plug-in Marketplace",
            |h| drop(h.app.run("plugins.marketplace", json!({"open": true})).unwrap()),
            |h| drop(h.app.run("plugins.marketplace", json!({"open": false})).unwrap()),
        ),
    ];
    for (name, open, close) in over {
        open(&mut h);
        h.settle(T);
        assert!(!peeks(&mut h), "{name}: holding \\ behind it shows no Before");
        close(&mut h);
        h.settle(T);
    }
    click(&mut h, "field:search");
    assert!(!peeks(&mut h), "typing \\ in a text field shows no Before");
}

/// In a modal's text field the editing shortcuts edit the field: ⌘A selects its text, cut and
/// paste change it, and none of them (nor ⌘Z) reaches the photos behind (Select All, Undo).
#[test]
fn a_modal_text_field_keeps_its_editing_shortcuts() {
    use crate::keyboard::{MenuChoice, Owner, menu_choice, owner};
    let mut h = demo([1400.0, 1000.0], json!({"view": "detail", "right": "edit"}));
    let active = h.app.session.active().unwrap();
    // something for ⌘Z to undo behind the dialog, were it to get there
    let before = h.app.session.catalog.photo(active).unwrap().rating;
    let rating = if before == 1 { 2 } else { 1 };
    let r = h.request("ui.key", json!({"key": rating.to_string()}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, rating);
    let selected = h.app.session.selected();
    h.app.run("dialog.newAlbum", json!({})).unwrap();
    h.step();
    h.step();
    let name = |h: &Headless| match &h.app.ui.dialog {
        Some(crate::state::Dialog::NewAlbum { name, .. }) => name.clone(),
        other => panic!("New Collection closed: {other:?}"),
    };
    assert!(h.view.ctx.egui_wants_keyboard_input(), "the name field has the focus");
    assert_eq!(owner(&h.app, &h.view.ctx), Owner::Modal);
    for sc in crate::menubar::TEXT_EDIT_SHORTCUTS {
        assert_eq!(menu_choice(Owner::Modal, "edit.any", Some(sc), true), MenuChoice::TextField, "the menu bar hands {sc} to the field");
    }
    let req = |h: &mut Headless, what: &str, p: serde_json::Value| {
        let r = h.request(what, p, T);
        assert_eq!(r["ok"], true, "{what}: {r}");
        h.step();
    };
    req(&mut h, "ui.text", json!({"text": "Trip"}));
    assert_eq!(name(&h), "Trip");
    req(&mut h, "ui.key", json!({"key": "A", "cmd": true}));
    req(&mut h, "ui.clipboard", json!({"action": "cut"}));
    assert_eq!(name(&h), "", "⌘A selected the field's text and ⌘X cut it");
    req(&mut h, "ui.clipboard", json!({"action": "paste", "text": "Hike"}));
    assert_eq!(name(&h), "Hike", "⌘V pastes into the field");
    req(&mut h, "ui.clipboard", json!({"action": "copy"}));
    req(&mut h, "ui.key", json!({"key": "Z", "cmd": true}));
    assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, rating, "⌘Z stays in the field: the rating isn't undone");
    assert_eq!(h.app.session.selected(), selected, "⌘A stays in the field: the photos aren't all selected");
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::NewAlbum { .. })), "the dialog is still open");
    // …where, with the dialog closed, the same keys reach the photos (so the checks above can fail)
    h.app.ui.dialog = None;
    h.step();
    req(&mut h, "ui.key", json!({"key": "Z", "cmd": true}));
    assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, before, "with nothing open, ⌘Z undoes the rating");
    req(&mut h, "ui.key", json!({"key": "A", "cmd": true}));
    assert_ne!(h.app.session.selected(), selected, "with nothing open, ⌘A selects every photo");
}

/// Clicks one of the open window's own controls: they still work while it keeps the app's.
fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
}
