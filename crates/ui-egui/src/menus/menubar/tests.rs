use super::*;

fn app() -> LightcraftApp {
    LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default())
}

fn find<'a>(nodes: &'a [MenuNode], id: &str) -> Option<&'a MenuNode> {
    nodes.iter().find_map(|n| match n {
        MenuNode::Item { id: i, .. } if i == id => Some(n),
        MenuNode::Submenu { children, .. } => find(children, id),
        _ => None,
    })
}

/// #2500: a direct-grid binding takes precedence over the borrowed cycle hint.
#[test]
fn grid_menu_shortcut_hints_preserve_direct_bindings_and_clicks() {
    let mut a = app();
    crate::shortcuts::assign(&mut a.ui.settings.keymap, "view.photoGrid", Some("Cmd+Shift+1")).unwrap();
    let bar = menu_bar(&a);
    let nodes = &bar.iter().find(|(title, _)| title == "View").unwrap().1;
    assert!(matches!(find(nodes, "view.photoGrid"), Some(MenuNode::Item { shortcut: Some(sc), .. }) if sc == "Cmd+Shift+1"));
    assert!(matches!(find(nodes, "view.squareGrid"), Some(MenuNode::Item { shortcut: Some(sc), .. }) if sc == "G"));
    // A hint never assigns the cycle key to another command.
    assert_eq!(crate::shortcuts::shortcut_of(&a.ui.settings.keymap, "view.squareGrid"), None);
    run_item(&mut a, "view.photoGrid", Value::Null).unwrap();
    assert_eq!(a.ui.view, ViewMode::PhotoGrid);
    run_item(&mut a, "view.photoGrid", Value::Null).unwrap();
    assert_eq!(a.ui.view, ViewMode::PhotoGrid, "clicking the active row does not cycle");
    run_item(&mut a, "view.squareGrid", Value::Null).unwrap();
    assert_eq!(a.ui.view, ViewMode::SquareGrid);
    run_item(&mut a, "view.gridToggle", Value::Null).unwrap();
    assert_eq!(a.ui.view, ViewMode::PhotoGrid);
}

/// #2500: native hints never install a borrowed modifier/function key as a direct-grid
/// accelerator. Plain G remains in AppKit's shortcut column and is dispatched by egui.
#[test]
fn grid_menu_shortcut_hints_keep_native_toggle_dispatch() {
    let mut keymap = crate::shortcuts::Keymap::new();
    for id in ["view.photoGrid", "view.squareGrid"] {
        let (label, accelerator) = native_item_presentation(&keymap, id, &Value::Null, "Photo Grid", Some("G"));
        assert_eq!(label, "Photo Grid");
        assert_eq!(accelerator, Some("G"));
        assert!(!native_menu_delivers(accelerator.unwrap()));
        for (key, text) in [("Cmd+Shift+G", "⇧⌘G"), ("Ctrl+G", "⌃G"), ("F3", "F3")] {
            crate::shortcuts::assign(&mut keymap, "view.gridToggle", Some(key)).unwrap();
            let (label, accelerator) = native_item_presentation(&keymap, id, &Value::Null, "Photo Grid", Some(key));
            assert_eq!(label, format!("Photo Grid ({text})"));
            assert_eq!(accelerator, None, "egui keeps {key}");
        }
    }
    crate::shortcuts::assign(&mut keymap, "view.photoGrid", Some("Cmd+Shift+1")).unwrap();
    let (label, accelerator) = native_item_presentation(&keymap, "view.photoGrid", &Value::Null, "Photo Grid", Some("Cmd+Shift+1"));
    assert_eq!(label, "Photo Grid");
    assert_eq!(accelerator, Some("Cmd+Shift+1"), "the row's own shortcut is a real accelerator");
}

/// #2500: the control channel runs the real keyboard path exactly once and menu commands
/// continue to select their named grid after the cycle shortcut is remapped.
#[test]
fn grid_menu_shortcut_hints_preserve_control_keyboard_dispatch() {
    let mut h = crate::headless::Headless::new(app(), [960.0, 720.0], 1.0);
    h.app.ui.view = ViewMode::PhotoGrid;
    for _ in 0..3 {
        h.step();
    }
    let timeout = std::time::Duration::from_secs(20);
    let press = |h: &mut crate::headless::Headless, params| {
        let reply = h.request("ui.key", params, timeout);
        assert_eq!(reply["ok"], true, "{reply}");
    };
    press(&mut h, json!({"key": "G"}));
    assert_eq!(h.app.ui.view, ViewMode::SquareGrid);
    press(&mut h, json!({"key": "G"}));
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid);
    crate::shortcuts::assign(&mut h.app.ui.settings.keymap, "view.gridToggle", Some("Cmd+Shift+G")).unwrap();
    press(&mut h, json!({"key": "G"}));
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid);
    press(&mut h, json!({"key": "G", "cmd": true, "shift": true}));
    assert_eq!(h.app.ui.view, ViewMode::SquareGrid);
    press(&mut h, json!({"key": "G", "cmd": true, "shift": true}));
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid);
    for _ in 0..2 {
        let reply = h.request("ui.menu.invoke", json!({"command": "view.squareGrid"}), timeout);
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(h.app.ui.view, ViewMode::SquareGrid);
    }
}

/// #2513 review: moving a direct-grid key to the cycle command leaves the visible model
/// unchanged, but must invalidate the native cache so its old accelerator is removed.
#[test]
fn grid_menu_shortcut_hints_invalidate_native_ownership_cache() {
    let mut a = app();
    crate::shortcuts::assign(&mut a.ui.settings.keymap, "view.gridToggle", None).unwrap();
    crate::shortcuts::assign(&mut a.ui.settings.keymap, "view.photoGrid", Some("Cmd+Shift+G")).unwrap();
    crate::shortcuts::assign(&mut a.ui.settings.keymap, "view.squareGrid", Some("Cmd+Shift+2")).unwrap();
    let before = menu_bar(&a);
    let direct = native_menu_structure(&before, &a.ui.settings.keymap);
    crate::shortcuts::assign(&mut a.ui.settings.keymap, "view.gridToggle", Some("Cmd+Shift+G")).unwrap();
    let after = menu_bar(&a);
    assert_eq!(before, after, "visible hint text does not expose the ownership change");
    assert_ne!(direct, native_menu_structure(&after, &a.ui.settings.keymap), "rebuild removes the stale direct-grid accelerator");
}

/// #2500: grid view rows show the separate cycle command's current shortcut.
#[test]
fn grid_menu_shortcut_hints_follow_the_cycle_key() {
    let mut a = app();
    let hint = |a: &LightcraftApp, id: &str| {
        let bar = menu_bar(a);
        let nodes = &bar.iter().find(|(title, _)| title == "View").expect("View menu").1;
        match find(nodes, id).expect("grid row") {
            MenuNode::Item { shortcut, .. } => shortcut.clone(),
            _ => panic!("grid row must be an item"),
        }
    };
    for id in ["view.photoGrid", "view.squareGrid"] {
        assert_eq!(hint(&a, id).as_deref(), Some("G"), "{id}");
    }
    crate::shortcuts::assign(&mut a.ui.settings.keymap, "view.gridToggle", Some("Cmd+Shift+G")).unwrap();
    for id in ["view.photoGrid", "view.squareGrid"] {
        assert_eq!(hint(&a, id).as_deref(), Some("Cmd+Shift+G"), "{id}");
    }
    crate::shortcuts::assign(&mut a.ui.settings.keymap, "view.gridToggle", None).unwrap();
    for id in ["view.photoGrid", "view.squareGrid"] {
        assert_eq!(hint(&a, id), None, "{id}");
    }
}

#[test]
fn view_menu_offers_the_hold_and_the_refresh_and_checks_the_hold_while_it_is_on() {
    let mut a = app();
    let view = |a: &LightcraftApp| menu_bar(a).into_iter().find(|(t, _)| t == "View").expect("View menu").1;
    let items = view(&a);
    assert!(matches!(find(&items, "library.refresh"), Some(MenuNode::Item { enabled: true, .. })));
    assert!(matches!(find(&items, "library.holdView"), Some(MenuNode::Item { checked: Some(false), .. })), "off unless asked for");
    a.session.execute("library.holdView", &json!({"on": true})).expect("hold the view");
    assert!(matches!(find(&view(&a), "library.holdView"), Some(MenuNode::Item { checked: Some(true), .. })));
}

/// The Sort submenu is expanded by hand: Reshuffle appears once (not again from the registry),
/// only while sorting at random, and the direction items are off for a shuffle.
#[test]
fn sort_menu_lists_reshuffle_once_and_only_enables_it_for_random() {
    fn sort_children(bar: &[(String, Vec<MenuNode>)]) -> Vec<MenuNode> {
        let view = &bar.iter().find(|(t, _)| t == "View").expect("View menu").1;
        view.iter()
            .find_map(|n| match n {
                MenuNode::Submenu { label, children } if label == "Sort" => Some(children.clone()),
                _ => None,
            })
            .expect("Sort submenu")
    }
    let count = |nodes: &[MenuNode]| nodes.iter().filter(|n| matches!(n, MenuNode::Item { id, .. } if id == "library.shuffle")).count();
    let mut a = app();
    let kids = sort_children(&menu_bar(&a));
    assert_eq!(count(&kids), 1);
    assert!(matches!(find(&kids, "library.shuffle"), Some(MenuNode::Item { enabled: false, .. })), "off until Random is chosen");
    a.session.execute("library.sort", &json!({"key": "random"})).expect("sort at random");
    let kids = sort_children(&menu_bar(&a));
    assert_eq!(count(&kids), 1);
    assert!(matches!(find(&kids, "library.shuffle"), Some(MenuNode::Item { enabled: true, .. })));
    // no direction is shown as chosen while shuffling
    let dir_checked = |kids: &[MenuNode]| {
        kids.iter()
            .filter_map(|n| match n {
                MenuNode::Item { label, checked, .. } if label == "Ascending" || label == "Descending" => Some(*checked),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(dir_checked(&kids), vec![None, None]);
    a.session.execute("library.sort", &json!({"key": "fileName"})).expect("sort by name");
    assert!(dir_checked(&sort_children(&menu_bar(&a))).iter().all(Option::is_some), "checks come back for other keys");
}

/// File opens with the import entry points, worded as importing (not as adding a sidebar
/// folder): Import Photos… (⇧⌘I), Import from Folder…, Import from Device ▸.
#[test]
fn file_menu_starts_with_the_import_entry_points() {
    let bar = menu_bar(&app());
    let file = &bar.iter().find(|(t, _)| t == "File").expect("File menu").1;
    let labels: Vec<String> = file
        .iter()
        .take(3)
        .map(|n| match n {
            MenuNode::Item { label, shortcut, .. } => format!("{label}{}", shortcut.as_deref().map(|s| format!(" [{s}]")).unwrap_or_default()),
            MenuNode::Submenu { label, .. } => format!("{label} ▸"),
            MenuNode::Separator => "---".into(),
        })
        .collect();
    assert_eq!(labels[0], "Import Photos… [Cmd+Shift+I]");
    assert_eq!(labels[1..], ["Import from Folder…".to_string(), "Import from Device ▸".to_string()]);
    let all: Vec<MenuNode> = bar.iter().flat_map(|(_, v)| v.clone()).collect();
    let text = serde_json::to_string(&all).unwrap();
    assert!(!text.contains("Add Folder") && !text.contains("\"Add Photos"), "no add-folder wording left in the menus");
}

/// Back Up / Restore Library are the browser build's (its library lives in browser storage):
/// absent from the desktop's menus, present and wired to the host where it provides them.
#[test]
fn library_backup_items_follow_the_host() {
    let all = |app: &LightcraftApp| -> Vec<MenuNode> { menu_bar(app).into_iter().flat_map(|(_, v)| v).collect() };
    let mut desktop = app();
    assert!(find(&all(&desktop), "file.backupLibrary").is_none() && find(&all(&desktop), "file.restoreLibrary").is_none());
    assert!(run_item(&mut desktop, "file.backupLibrary", Value::Null).is_err(), "not available without the host");
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let c = calls.clone();
    let services = crate::Services {
        backup_library: Some(Box::new(move |s: &mut lightcraft_engine::Session| {
            c.set(c.get() + 1);
            Ok(json!({"photos": s.catalog.len()}))
        })),
        restore_library: Some(Box::new(|_: &mut lightcraft_engine::Session| Ok(json!({"started": true})))),
        ..Default::default()
    };
    let mut web = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let bar = all(&web);
    for id in ["file.backupLibrary", "file.restoreLibrary"] {
        assert!(matches!(find(&bar, id), Some(MenuNode::Item { enabled: true, .. })), "{id}");
    }
    let r = run_item(&mut web, "file.backupLibrary", Value::Null).unwrap();
    assert!(r["photos"].as_u64().unwrap() > 0);
    assert_eq!(calls.get(), 1);
}

#[test]
fn bar_follows_the_registry_and_state() {
    let mut app = app();
    let bar = menu_bar(&app);
    let titles: Vec<&str> = bar.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(titles, MENUS);
    let all: Vec<MenuNode> = bar.iter().flat_map(|(_, v)| v.clone()).collect();
    // every engine command with a menu path is reachable (parameterized ones via submenus; items
    // that follow the state, like Restore for deleted photos, when it applies)
    for c in lightcraft_engine::command_specs().iter().filter(|c| !c.menu.is_empty() && !HIDDEN.contains(&c.id) && host_supports(&app, c.id)) {
        assert!(find(&all, c.id).is_some(), "{} missing from the menu bar", c.id);
    }
    for id in ["photo.rate", "photo.flag", "photo.label", "library.sort", "album.addPhotos", "view.compare", "stack.group", "photo.virtualCopy"] {
        assert!(find(&all, id).is_some(), "{id}");
    }
    // no doubled or dangling separators
    fn check(v: &[MenuNode]) {
        assert_ne!(v.first(), Some(&MenuNode::Separator));
        assert_ne!(v.last(), Some(&MenuNode::Separator));
        for w in v.windows(2) {
            assert!(!(w[0] == MenuNode::Separator && w[1] == MenuNode::Separator));
        }
        for n in v {
            if let MenuNode::Submenu { children, .. } = n {
                check(children);
            }
        }
    }
    bar.iter().for_each(|(_, v)| check(v));
    // live state: undo label, enabled, checked
    let Some(MenuNode::Item { enabled, .. }) = find(&all, "edit.undo") else { panic!() };
    assert!(!enabled);
    run_item(&mut app, "photo.rate", json!({"rating": 3})).unwrap();
    let bar = menu_bar(&app);
    let all: Vec<MenuNode> = bar.iter().flat_map(|(_, v)| v.clone()).collect();
    let Some(MenuNode::Item { label, enabled, .. }) = find(&all, "edit.undo") else { panic!() };
    assert_eq!((label.as_str(), *enabled), ("Undo Set Rating", true));
    let rated: Vec<_> = all
        .iter()
        .filter_map(|n| match n {
            MenuNode::Submenu { label, children } if label == "Set Rating" => Some(children.clone()),
            _ => None,
        })
        .flatten()
        .filter(|n| matches!(n, MenuNode::Item { checked: Some(true), .. }))
        .collect();
    assert!(matches!(&rated[..], [MenuNode::Item { params, .. }] if params["rating"] == 3));
    let Some(MenuNode::Item { checked, .. }) = find(&all, "view.filmstrip") else { panic!() };
    assert_eq!(*checked, Some(app.ui.filmstrip));
}

#[test]
fn quit_ends_the_file_menu_after_unlisted_entries() {
    let app = app();
    // the File menu has registry entries the layout doesn't list (appended as fallbacks)
    let listed = LAYOUT.iter().find(|(t, _)| *t == "File").unwrap().1;
    let unlisted: Vec<String> = crate::menus::menu_entries(&app)
        .into_iter()
        .filter(|e| e.menu.first().map(String::as_str) == Some("File") && !listed.contains(&e.id.as_str()) && !HIDDEN.contains(&e.id.as_str()))
        .map(|e| e.id)
        .collect();
    assert!(!unlisted.is_empty(), "no fallback File entries to order");
    let bar = menu_bar(&app);
    let file = &bar.iter().find(|(t, _)| t == "File").unwrap().1;
    // Quit is the last item, alone in its separator group
    assert!(matches!(file.last(), Some(MenuNode::Item { id, .. }) if id == "app.quit"), "{file:?}");
    assert_eq!(file.get(file.len() - 2), Some(&MenuNode::Separator));
    // every fallback entry is still reachable, before Quit
    for id in &unlisted {
        assert!(find(&file[..file.len() - 1], id).is_some(), "{id} missing before Quit");
    }
}

#[test]
fn export_with_preset_submenu() {
    let mut app = app();
    let bar = menu_bar(&app);
    let file = &bar.iter().find(|(t, _)| t == "File").unwrap().1;
    let Some(MenuNode::Submenu { children, .. }) =
        file.iter().find(|n| matches!(n, MenuNode::Submenu { label, .. } if label == "Export with Preset"))
    else {
        panic!("no Export with Preset submenu")
    };
    let presets: Vec<_> = children
        .iter()
        .filter_map(|n| match n {
            MenuNode::Item { id, params, .. } if id == "app.export" => params["preset"].as_str().map(str::to_string),
            _ => None,
        })
        .collect();
    assert_eq!(presets.first().map(String::as_str), Some("JPEG (Small)"));
    let Some(MenuNode::Item { label, .. }) = find(children, "dialog.export") else {
        panic!("Custom… opens the dialog");
    };
    assert!(label.contains("Export"), "custom export label: {label:?}");
    // a user preset appears after the built-ins; choosing it exports with its settings
    app.session.execute("export.savePreset", &json!({"name": "Tiny PNG", "params": {"format": "png", "width": 40}})).unwrap();
    let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, Vec<u8>)>::new()));
    let w = written.clone();
    app.services.write = Some(Box::new(move |p: &str, b: &[u8]| {
        w.lock().unwrap().push((p.to_string(), b.to_vec()));
        Ok(())
    }));
    let r = run_item(&mut app, "app.export", json!({"preset": "Tiny PNG", "dir": "/nonexistent-lc-test"})).unwrap();
    assert_eq!(r["files"][0]["width"], 40, "{r}");
    let w = written.lock().unwrap();
    assert!(w[0].0.starts_with("/nonexistent-lc-test/") && w[0].0.ends_with(".png"), "{}", w[0].0);
    assert!(w[0].1.starts_with(b"\x89PNG"));
    // remembered (expanded) for Export with Previous, folder included
    let last = app.session.last_export.clone().unwrap();
    assert_eq!((last["format"].as_str(), last["width"].as_u64(), last.get("preset")), (Some("png"), Some(40), None));
    // a blank folder (the Export dialog's Folder field cleared) is refused with a clear message
    // instead of writing into the working directory
    let n = w.len();
    drop(w);
    let r = run_item(&mut app, "app.export", json!({"preset": "Tiny PNG", "dir": "  "}));
    assert_eq!(r.unwrap_err(), crate::control::NO_EXPORT_FOLDER);
    assert_eq!(written.lock().unwrap().len(), n, "nothing written without a folder");
    // "Same folder as original photo" needs no folder; a demo photo has none of its own, so it
    // gets its own error rather than a file written elsewhere (issue #956)
    let r = run_item(&mut app, "app.export", json!({"preset": "Tiny PNG", "dir": "  ", "destinationMode": "sameAsOriginal"}));
    let e = r.unwrap_err();
    assert!(e.contains("no folder on disk"), "{e}");
    assert_eq!(written.lock().unwrap().len(), n, "nothing written for a photo without a folder");
}

#[test]
fn select_by_submenu_and_text_prompt() {
    let mut app = app();
    let bar = menu_bar(&app);
    let edit = &bar.iter().find(|(t, _)| t == "Edit").unwrap().1;
    let Some(MenuNode::Submenu { children, .. }) = edit.iter().find(|n| matches!(n, MenuNode::Submenu { label, .. } if label == "Select by")) else {
        panic!("no Select by submenu")
    };
    let Some(MenuNode::Item { id, params, .. }) = children.first() else { panic!() };
    let r = run_item(&mut app, id, params.clone()).unwrap();
    assert!(r["selected"].as_u64().is_some_and(|n| n > 0), "{r}");
    // the one-field prompt runs its command with the typed value
    app.session.execute("version.create", &json!({"name": "A"})).unwrap();
    crate::panels::dialogs::prompt(&mut app, "Rename Version", "Version name", "A", "version.rename", json!({"index": 0}), "name");
    if let Some(crate::state::Dialog::TextPrompt { value, .. }) = &mut app.ui.dialog {
        *value = "  Final  ".into();
    }
    let d = app.ui.dialog.take().unwrap();
    crate::panels::dialogs::confirm_dialog(&mut app, &d).unwrap();
    let id = app.session.active().unwrap();
    assert_eq!(app.session.catalog.photo(id).unwrap().versions[0].name, "Final");
}

#[test]
fn help_opens_the_docs_and_merge_last_needs_photos() {
    let mut app = app();
    let opened = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let o = opened.clone();
    app.services.open_url = Some(Box::new(move |u: &str| {
        o.lock().unwrap().push(u.to_string());
        Ok(())
    }));
    run_item(&mut app, "app.help", Value::Null).unwrap();
    assert_eq!(opened.lock().unwrap().as_slice(), [crate::links::HELP]);
    // merging with the last settings needs a selection of two or more
    let first = app.session.visible()[0].0;
    app.session.execute("library.select", &json!({"ids": [first]})).unwrap();
    assert!(run_item(&mut app, "merge.hdrLast", Value::Null).is_err());
}

/// #260: Help ▸ Open Log Folder reveals the log file the host names. Without one (the web,
/// `--memory`) or without a file manager to show it, the item is off and the command says why.
#[test]
fn open_log_folder_reveals_the_hosts_log_file() {
    let mut app = app();
    let help = |app: &LightcraftApp| menu_bar(app).into_iter().find(|(t, _)| t == "Help").map(|(_, items)| items).unwrap_or_default();
    assert!(matches!(find(&help(&app), "app.openLogFolder"), Some(MenuNode::Item { enabled: false, .. })), "listed in Help, off without a log");
    assert!(!crate::menus::ui_enabled(&app, "app.openLogFolder"));
    assert!(run_item(&mut app, "app.openLogFolder", Value::Null).is_err());
    let shown = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let s = shown.clone();
    app.services.reveal = Some(Box::new(move |p: &str| {
        s.lock().unwrap().push(p.to_string());
        Ok(())
    }));
    // a file manager alone is not enough: this session keeps no log
    assert!(!crate::menus::ui_enabled(&app, "app.openLogFolder"));
    assert!(run_item(&mut app, "app.openLogFolder", Value::Null).is_err());
    assert!(shown.lock().unwrap().is_empty());
    let log = "/home/a/.config/lightcraft/logs/lightcraft.log";
    app.services.log_file = Some(log.into());
    assert!(crate::menus::ui_enabled(&app, "app.openLogFolder"));
    assert!(matches!(find(&help(&app), "app.openLogFolder"), Some(MenuNode::Item { enabled: true, .. })));
    let r = run_item(&mut app, "app.openLogFolder", Value::Null).unwrap();
    assert_eq!(r["path"], log);
    assert_eq!(shown.lock().unwrap().as_slice(), [log]);
    // the file manager's failure is the command's
    app.services.reveal = Some(Box::new(|_: &str| Err("no file manager".into())));
    assert_eq!(run_item(&mut app, "app.openLogFolder", Value::Null).unwrap_err(), "no file manager");
}

#[test]
fn find_missing_and_locate() {
    let dir = std::env::temp_dir().join(format!("lc-ui-missing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("a")).unwrap();
    std::fs::create_dir_all(dir.join("b")).unwrap();
    let img = lightcraft_raster::Rgba8::from_fn(16, 12, |x, y| [(x * 9) as u8, (y * 11) as u8, 50, 255]);
    let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
    std::fs::write(dir.join("a/one.png"), lightcraft_engine::export::encode_image(&img, &o).unwrap()).unwrap();
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Default::default());
    app.session.execute("library.import", &json!({"paths": [dir.join("a").to_string_lossy()]})).unwrap();
    std::fs::rename(dir.join("a/one.png"), dir.join("b/one.png")).unwrap();
    let r = run_item(&mut app, "file.findMissing", json!({"folder": dir.join("b").to_string_lossy(), "wait": true})).unwrap();
    assert_eq!(r["found"].as_array().map(Vec::len), Some(1), "{r}");
    // Locate: an explicit file
    std::fs::rename(dir.join("b/one.png"), dir.join("one-renamed.png")).unwrap();
    let id = app.session.catalog.photos().next().unwrap().id;
    app.session.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    run_item(&mut app, "photo.locate", json!({"path": dir.join("one-renamed.png").to_string_lossy()})).unwrap();
    let p = app.session.catalog.photo(id).unwrap();
    assert_eq!(p.file_name, "one-renamed.png");
    let _ = std::fs::remove_dir_all(&dir);
}

// Regression test for #49: the in-window menu headings (File, Edit, ...) sat only egui's default
// 8 px apart, too close to read or aim at.
#[test]
fn in_window_menu_headings_are_spaced_apart() {
    let mut a = app();
    a.native_menu = false;
    let mut h = crate::headless::Headless::new(a, [1400.0, 900.0], 1.0);
    h.step();
    h.step();
    let rect = |id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("{id} is on screen"));
    let titles: Vec<_> = ["File", "Edit", "View", "Photo", "Window", "Help"].iter().map(|t| rect(&format!("menu:{t}"))).collect();
    for pair in titles.windows(2) {
        let gap = pair[1].left() - pair[0].right();
        assert!(gap >= 24.0 - 0.5, "headings {:?} and {:?} are only {gap} px apart", pair[0], pair[1]);
    }
}

/// A backup the host refuses (changes still unsaved, issue #1508) says why when started from
/// the menu, as a refused export does; the refusal used to reach only the control channel.
#[test]
fn a_refused_backup_from_the_menu_shows_why() {
    // the host refuses, as the web one does while changes are still unsaved (issue #1508)
    let refusal = "Backup not started: 1 change could not be saved to browser storage yet";
    let services =
        crate::Services { backup_library: Some(Box::new(move |_: &mut lightcraft_engine::Session| Err(refusal.to_string()))), ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = crate::headless::Headless::new(app, [1400.0, 900.0], 1.0);
    let t = std::time::Duration::from_secs(30);
    h.settle(t);
    for id in ["menu:File", "menuitem:file.backupLibrary"] {
        let r = h.request("ui.clickWidget", json!({"id": id}), t);
        assert_eq!(r["ok"], true, "{id}: {r}");
        h.step();
    }
    // (read at once: a toast keeps the UI repainting until it expires, so settling would outlive it)
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.contains(refusal), "the refusal is shown: {toast:?}");
}

/// #1528: `file.findMissing` with `wait: true` must propagate folder validation and search
/// failures as errors instead of returning `Ok(null)`.
#[test]
fn find_missing_waited_invalid_folder_returns_error_with_zero_mutations() {
    let dir = std::env::temp_dir().join(format!("lc-ui-missing-err-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dummy = dir.join("dummy.png");
    let img = lightcraft_raster::Rgba8::from_fn(8, 8, |_, _| [10, 20, 30, 255]);
    let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
    std::fs::write(&dummy, lightcraft_engine::export::encode_image(&img, &o).unwrap()).unwrap();

    let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Default::default());
    app.session.execute("library.import", &json!({"paths": [dummy.to_string_lossy()]})).unwrap();
    // remove the file on disk so the photo is missing
    std::fs::remove_file(&dummy).unwrap();
    let id = app.session.catalog.photos().next().unwrap().id;
    let original_path = match &app.session.catalog.photo(id).unwrap().source {
        lightcraft_catalog::Source::File { path } => path.clone(),
        _ => panic!("expected file source"),
    };
    let undo_count_before = app.session.undo.len();

    // 1. Point at a nonexistent directory
    let bad_dir = dir.join("nonexistent_subdir");
    let err = run_item(&mut app, "file.findMissing", json!({"folder": bad_dir.to_string_lossy(), "wait": true})).unwrap_err();
    assert!(err.contains("not a folder"), "expected 'not a folder' error, got: {err}");
    assert!(app.ui.toast.as_ref().map(|t| t.0.contains("not a folder")).unwrap_or(false), "toast should reflect failure");
    assert!(app.ui.last_find_missing.is_none(), "last_find_missing must remain None on failure");

    // 2. Invalid timeout parameters are rejected cleanly
    let err = run_item(&mut app, "file.findMissing", json!({"folder": dir.to_string_lossy(), "wait": true, "timeout": -1.0})).unwrap_err();
    assert!(err.contains("non-negative number of seconds"), "{err}");
    let err = run_item(&mut app, "file.findMissing", json!({"folder": dir.to_string_lossy(), "wait": true, "timeout": "invalid"})).unwrap_err();
    assert!(err.contains("number of seconds"), "{err}");
    let err = run_item(&mut app, "file.findMissing", json!({"folder": dir.to_string_lossy(), "wait": true, "timeout": 1e300})).unwrap_err();
    assert!(err.contains("too large"), "{err}");

    // Catalog and photo path must not have been mutated
    assert_eq!(app.session.undo.len(), undo_count_before, "no undo steps added on failed search");
    let path_after = match &app.session.catalog.photo(id).unwrap().source {
        lightcraft_catalog::Source::File { path } => path.clone(),
        _ => panic!("expected file source"),
    };
    assert_eq!(path_after, original_path, "missing photo path must not change on failure");

    let _ = std::fs::remove_dir_all(&dir);
}

/// #1528: `file.findMissing` with `wait: true` returns structured results on success,
/// and background mode (`wait: false` or omitted) continues returning `{"background": true}`.
#[test]
fn find_missing_waited_structured_results_and_background_mode() {
    let dir = std::env::temp_dir().join(format!("lc-ui-missing-bg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("orig")).unwrap();
    std::fs::create_dir_all(dir.join("found")).unwrap();
    let img = lightcraft_raster::Rgba8::from_fn(8, 8, |_, _| [10, 20, 30, 255]);
    let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
    std::fs::write(dir.join("orig/pic.png"), lightcraft_engine::export::encode_image(&img, &o).unwrap()).unwrap();

    let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Default::default());
    app.session.execute("library.import", &json!({"paths": [dir.join("orig").to_string_lossy()]})).unwrap();
    std::fs::rename(dir.join("orig/pic.png"), dir.join("found/pic.png")).unwrap();

    // Background mode (wait omitted): returns {"background": true} immediately
    let bg = run_item(&mut app, "file.findMissing", json!({"folder": dir.join("found").to_string_lossy()})).unwrap();
    assert_eq!(bg, json!({"background": true}));

    // Wait for background task to complete
    let ctx = app.tasks.repaint.clone().unwrap_or_default();
    assert!(crate::tasks::wait(&mut app, &ctx, std::time::Duration::from_secs(5)));
    let last = app.ui.last_find_missing.as_ref().unwrap();
    assert_eq!(last["found"].as_array().map(Vec::len), Some(1));

    // Now move again and run with wait: true -> returns structured results directly
    std::fs::create_dir_all(dir.join("found2")).unwrap();
    std::fs::rename(dir.join("found/pic.png"), dir.join("found2/pic.png")).unwrap();
    let r = run_item(&mut app, "file.findMissing", json!({"folder": dir.join("found2").to_string_lossy(), "wait": true})).unwrap();
    assert!(r.get("found").is_some(), "must have 'found' array: {r}");
    assert!(r.get("missing").is_some(), "must have 'missing' count: {r}");
    assert!(r.get("ambiguous").is_some(), "must have 'ambiguous' array: {r}");
    assert_eq!(r["found"].as_array().map(Vec::len), Some(1));

    let _ = std::fs::remove_dir_all(&dir);
}

/// #1528: `file.findMissing` with `wait: true` when timed out returns an actionable error
/// indicating timeout and that the search continues in the background, rather than returning `Ok(null)`.
#[test]
fn find_missing_waited_timeout_behavior() {
    let dir = std::env::temp_dir().join(format!("lc-ui-missing-to-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Default::default());
    // Injected test-only worker delay so wait times out deterministically
    app.ui.find_missing_test_delay = Some(std::time::Duration::from_millis(100));
    let err = run_item(&mut app, "file.findMissing", json!({"folder": dir.to_string_lossy(), "wait": true, "timeout": 0.005})).unwrap_err();

    assert!(err.contains("search timed out"), "error should mention timeout: {err}");
    assert!(err.contains("search continues in the background"), "error should document continuation: {err}");

    // The background task continues running in app.tasks
    assert!(app.tasks.is_running("Find Missing Photos"), "task should continue running in background");

    // Clean up the running task so no worker hangs
    let ctx = app.tasks.repaint.clone().unwrap_or_default();
    assert!(crate::tasks::wait_for(&mut app, &ctx, "Find Missing Photos", std::time::Duration::from_secs(5)));
    assert!(!app.tasks.is_running("Find Missing Photos"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// #1528: `file.findMissing` with `wait: true` must return when its own search finishes,
/// even if unrelated background tasks remain active in `app.tasks`.
#[test]
fn find_missing_waited_completes_while_unrelated_task_remains_active() {
    let dir = std::env::temp_dir().join(format!("lc-ui-missing-multi-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("orig")).unwrap();
    std::fs::create_dir_all(dir.join("found")).unwrap();
    let img = lightcraft_raster::Rgba8::from_fn(8, 8, |_, _| [10, 20, 30, 255]);
    let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
    std::fs::write(dir.join("orig/pic.png"), lightcraft_engine::export::encode_image(&img, &o).unwrap()).unwrap();

    let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Default::default());
    app.session.execute("library.import", &json!({"paths": [dir.join("orig").to_string_lossy()]})).unwrap();
    std::fs::rename(dir.join("orig/pic.png"), dir.join("found/pic.png")).unwrap();

    // Spawn an unrelated background task that blocks on a channel until released
    let (tx_unblock, rx_unblock) = std::sync::mpsc::channel::<()>();
    crate::tasks::spawn(
        &mut app,
        "Unrelated Long Background Task",
        None,
        move || {
            let _ = rx_unblock.recv();
        },
        |_app, _ctx, _| {},
    )
    .unwrap();
    assert!(app.tasks.is_running("Unrelated Long Background Task"));

    // Run file.findMissing with wait: true: must complete without waiting for the unrelated task
    let r = run_item(&mut app, "file.findMissing", json!({"folder": dir.join("found").to_string_lossy(), "wait": true, "timeout": 5.0})).unwrap();
    assert_eq!(r["found"].as_array().map(Vec::len), Some(1), "Find Missing must complete successfully: {r}");

    // The unrelated task must STILL be active
    assert!(app.tasks.is_running("Unrelated Long Background Task"), "unrelated task must still be running");

    // Clean up the blocked background task
    let _ = tx_unblock.send(());
    let ctx = app.tasks.repaint.clone().unwrap_or_default();
    assert!(crate::tasks::wait(&mut app, &ctx, std::time::Duration::from_secs(5)));
    assert!(app.tasks.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]

fn auto_tag_from_tracklog() {
    let dir = std::env::temp_dir().join(format!("lc-ui-tracklog-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let gpx = dir.join("walk.gpx");
    std::fs::write(
        &gpx,
        r#"<gpx version="1.1" xmlns="http://www.topografix.com/GPX/1/1"><trk><trkseg>
                <trkpt lat="46.0" lon="7.0"><time>2026-05-01T10:00:00Z</time></trkpt>
                <trkpt lat="46.002" lon="7.004"><time>2026-05-01T10:02:00Z</time></trkpt>
            </trkseg></trk></gpx>"#,
    )
    .unwrap();
    let mut app = app();
    let id = app.session.visible()[0].0;
    app.session.execute("library.select", &json!({"ids": [id]})).unwrap();
    app.session.execute("photo.setCaptureTime", &json!({"time": "2026-05-01T12:01:00", "each": true})).unwrap();
    app.session.execute("photo.setMeta", &json!({"gps": null})).unwrap();
    // in the Photo menu; without a file dialog (headless) it is disabled
    let photo_menu = menu_bar(&app).into_iter().find(|(t, _)| t == "Photo").unwrap().1;
    let Some(MenuNode::Item { enabled, .. }) = find(&photo_menu, "photo.tagFromTracklog") else { panic!("not in the Photo menu") };
    assert!(!enabled);
    // a file without an offset asks for the camera's time zone; confirming runs the tagging
    run_item(&mut app, "photo.tagFromTracklog", json!({"path": gpx.to_string_lossy()})).unwrap();
    let Some(crate::state::Dialog::TextPrompt { value, .. }) = &mut app.ui.dialog else { panic!("no time-zone prompt") };
    *value = " +02:00 ".into();
    let d = app.ui.dialog.take().unwrap();
    let r = crate::panels::dialogs::confirm_dialog(&mut app, &d).unwrap();
    assert_eq!(r["tagged"], 1, "{r}");
    let p = app.session.catalog.photo(lightcraft_engine::catalog::PhotoId(id)).unwrap();
    assert_eq!(p.meta.gps, Some((46.001, 7.002)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn metadata_preset_menu() {
    let mut app = app();
    app.session.execute("photo.setMeta", &json!({"copyright": "© Me", "creator": "Me"})).unwrap();
    run_item(&mut app, "dialog.saveMetadataPreset", Value::Null).unwrap();
    if let Some(crate::state::Dialog::TextPrompt { value, .. }) = &mut app.ui.dialog {
        *value = "Mine".into();
    }
    let d = app.ui.dialog.take().unwrap();
    crate::panels::dialogs::confirm_dialog(&mut app, &d).unwrap();
    let bar = menu_bar(&app);
    let all: Vec<MenuNode> = bar.iter().flat_map(|(_, v)| v.clone()).collect();
    let Some(MenuNode::Item { id, params, .. }) = find(&all, "metadata.applyPreset") else { panic!("no preset item") };
    assert_eq!(params["name"], "Mine");
    let other = app.session.visible()[1];
    app.session.execute("library.select", &json!({"ids": [other.0]})).unwrap();
    run_item(&mut app, id, params.clone()).unwrap();
    assert_eq!(app.session.catalog.photo(other).unwrap().meta.copyright, "© Me");
}

/// Native menu accelerators never pass through `shortcuts::handle`: a command queued by the
/// native menu must obey the same blocking-window/prompt policy. Settings… and About must not
/// open behind the blocking window, and Quit stays available for its existing confirmation.
#[test]
fn native_menu_commands_respect_the_blocking_window_policy() {
    use crate::panels::library_problem::LibraryProblem;
    use crate::state::Dialog;

    let mut h = crate::headless::Headless::new(app(), [1000.0, 760.0], 1.0);
    h.settle(std::time::Duration::from_secs(20));
    h.app.ui.view = ViewMode::PhotoGrid;
    h.app.library_problem = Some(LibraryProblem::new("/no/such/library", "a test error"));

    assert_eq!(native_command_policy(&h.app, "app.settings"), NativeCommand::Blocked);
    assert_eq!(native_command_policy(&h.app, "app.about"), NativeCommand::Blocked);
    assert_eq!(native_command_policy(&h.app, "view.gridToggle"), NativeCommand::Blocked);
    assert_eq!(native_command_policy(&h.app, "app.quit"), NativeCommand::Quit, "Quit keeps its own confirmation");
    assert_eq!(run_native_command(&mut h.app, "app.settings", Value::Null), NativeCommand::Blocked);
    assert!(h.app.ui.dialog.is_none(), "Settings must not open behind the blocking library window");
    assert_eq!(run_native_command(&mut h.app, "app.about", Value::Null), NativeCommand::Blocked);
    assert!(h.app.ui.dialog.is_none(), "About must not open behind the blocking library window");
    assert_eq!(run_native_command(&mut h.app, "view.gridToggle", Value::Null), NativeCommand::Blocked);
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid, "a menu command did not act on the library behind the window");
    assert_eq!(
        run_native_command(&mut h.app, "app.quit", Value::Null),
        NativeCommand::Quit,
        "Quit stays for the host's existing tasks/unsaved-changes confirmation"
    );
    assert!(h.app.ui.dialog.is_none());

    // The launch list owns input the same way.
    let settings = crate::state::AppSettings {
        recent_libraries: vec!["/no/such/library-one".into()],
        library_path: "/no/such/library-one".into(),
        ..Default::default()
    };
    h.app.library_problem = Some(LibraryProblem::choose(&settings));
    assert_eq!(native_command_policy(&h.app, "app.about"), NativeCommand::Blocked);
    assert_eq!(run_native_command(&mut h.app, "app.about", Value::Null), NativeCommand::Blocked);
    assert!(h.app.ui.dialog.is_none(), "About must not open behind the launch list");
    // The launch list's own New Library prompt is not a background barrier: it keeps its
    // priority instead of being treated as an unrelated dialog behind the list.
    h.app.ui.dialog = Some(Dialog::TextPrompt {
        title: "New Library".into(),
        hint: "Library name".into(),
        value: "New Library".into(),
        command: "app.newLibrary".into(),
        params: json!({}),
        key: "name".into(),
    });
    assert_eq!(native_command_policy(&h.app, "app.settings"), NativeCommand::Run, "the launch list's own prompt is not a background barrier");
    h.app.ui.dialog = None;

    // A quit/settings notice owns input in the same way.
    h.app.library_problem = None;
    h.app.notices.push("The settings file could not be read and was reset.".into());
    assert_eq!(native_command_policy(&h.app, "app.settings"), NativeCommand::Blocked);
    assert_eq!(run_native_command(&mut h.app, "app.settings", Value::Null), NativeCommand::Blocked);
    assert!(h.app.ui.dialog.is_none(), "Settings must not open behind a notice");

    // Once nothing blocks input, the same queued command runs normally.
    h.app.notices.clear();
    assert_eq!(native_command_policy(&h.app, "app.settings"), NativeCommand::Run);
    assert_eq!(run_native_command(&mut h.app, "app.settings", Value::Null), NativeCommand::Run);
    assert!(matches!(h.app.ui.dialog, Some(Dialog::Settings { .. })), "the command runs when no barrier owns input");
}

#[test]
fn shortcut_text_per_platform() {
    assert_eq!(shortcut_text("Cmd+Shift+Z", true), "⇧⌘Z");
    assert_eq!(shortcut_text("Cmd+Shift+Z", false), "Ctrl+Shift+Z");
    assert_eq!(shortcut_text("Delete", true), "⌫");
}

/// AppKit and Apple's Human Interface Guidelines write modifiers ⌃⌥⇧⌘ whatever order the binding
/// was spelled in, so the hint on a button matches the menu bar's own column.
#[test]
fn shortcut_text_orders_mac_modifiers_control_option_shift_command() {
    assert_eq!(shortcut_text("Cmd+Alt+Shift+C", true), "⌥⇧⌘C");
    assert_eq!(shortcut_text("Shift+Alt+Ctrl+Cmd+K", true), "⌃⌥⇧⌘K");
    assert_eq!(shortcut_text("Cmd+Ctrl+F", true), "⌃⌘F");
    assert_eq!(shortcut_text("Shift+Cmd+Up", true), "⇧⌘↑");
    assert_eq!(shortcut_text("Shift+Alt+Y", true), "⌥⇧Y");
    assert_eq!(shortcut_text("Cmd+Alt+Shift+C", false), "Ctrl+Alt+Shift+C", "other platforms keep the binding as written");
}

/// Every item of a menu, submenus included.
fn flat(nodes: &[MenuNode]) -> Vec<&MenuNode> {
    nodes
        .iter()
        .flat_map(|n| match n {
            MenuNode::Submenu { children, .. } => flat(children),
            MenuNode::Item { .. } => vec![n],
            MenuNode::Separator => vec![],
        })
        .collect()
}

/// Given the menu bar, Then an item that turns something on or off is named for the thing,
/// with its check mark saying whether it is on: "Filmstrip ✓", never "Show Filmstrip ✓".
/// Show Original is a view (\), not a switch: it keeps Lightroom's name.
#[test]
fn toggles_are_named_for_the_thing_not_show() {
    let mut a = app();
    a.ui.view = ViewMode::Detail;
    let shows: Vec<String> = menu_bar(&a)
        .iter()
        .flat_map(|(_, items)| flat(items).into_iter().cloned().collect::<Vec<_>>())
        .filter_map(|n| match n {
            MenuNode::Item { id, params, label, checked: Some(_), .. }
                if params.is_null() && label.starts_with("Show ") && id != "view.showOriginal" =>
            {
                Some(format!("{id}: {label}"))
            }
            _ => None,
        })
        .collect();
    assert!(shows.is_empty(), "toggles named \"Show …\": {shows:#?}");
}

/// Given every command with a menu path, Then no two of them share a label, and none is named
/// like a menu: the menus, the keyboard shortcuts list and the control channel tell them apart
/// by name ("Detail" the view and "Detail" the Edit section were two things called the same).
#[test]
fn no_two_menu_commands_share_a_label() {
    let entries = crate::menus::menu_entries(&app());
    let mut clashes = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        if MENUS.contains(&e.label.as_str()) {
            clashes.push(format!("{} is called like the {} menu", e.id, e.label));
        }
        if let Some(other) = entries[..i].iter().find(|o| o.label == e.label && o.id != e.id) {
            clashes.push(format!("{} and {} are both {:?}", other.id, e.id, e.label));
        }
    }
    assert!(clashes.is_empty(), "{clashes:#?}");
}

/// Edit lists Select Only Active Photo with the other Select rows and Paste Settings from Previous
/// with the other Paste rows, not after Settings…, near the end of the menu.
#[test]
fn edit_menu_places_the_select_and_paste_rows_with_their_kin() {
    let a = app();
    let bar = menu_bar(&a);
    let nodes = &bar.iter().find(|(title, _)| title == "Edit").unwrap().1;
    let pos = |id: &str| {
        nodes.iter().position(|n| matches!(n, MenuNode::Item { id: i, .. } if i == id)).unwrap_or_else(|| panic!("{id} is a top-level Edit row"))
    };
    assert_eq!(pos("library.selectOnlyActive"), pos("library.selectNone") + 1, "Select Only Active Photo follows Deselect All");
    assert_eq!(pos("develop.pastePrevious"), pos("dialog.pasteSettings") + 1, "Paste Settings from Previous follows Paste Selected Settings…");
    assert!(pos("app.settings") > pos("library.selectOnlyActive"), "Settings… stays after them");
    let after = &nodes[pos("app.settings") + 1..];
    assert!(!after.iter().any(|n| matches!(n, MenuNode::Item { .. })), "no row follows Settings… but a submenu: {after:?}");
}
