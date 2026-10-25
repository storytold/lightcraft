//! Tests of the photo menu's model ([`super::nodes`], [`super::disabled_reason`]): what it offers, how it
//! groups it, and that it matches the menu bar.

use super::*;
use crate::state::ViewMode;
use serde_json::json;

fn app() -> LightcraftApp {
    LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default())
}

fn photos(a: &mut LightcraftApp) -> Vec<PhotoId> {
    a.session.visible_cloned()
}

fn select(a: &mut LightcraftApp, ids: &[PhotoId]) {
    a.run("library.select", json!({"ids": ids.iter().map(|p| p.0).collect::<Vec<_>>()})).unwrap();
}

/// Every item, submenus included, with the submenu labels it sits under.
fn items(nodes: &[MenuNode]) -> Vec<(Vec<String>, MenuNode)> {
    fn walk(n: &[MenuNode], path: &mut Vec<String>, out: &mut Vec<(Vec<String>, MenuNode)>) {
        for x in n {
            match x {
                MenuNode::Submenu { label, children } => {
                    path.push(label.clone());
                    walk(children, path, out);
                    path.pop();
                }
                MenuNode::Item { .. } => out.push((path.clone(), x.clone())),
                MenuNode::Separator => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(nodes, &mut Vec::new(), &mut out);
    out
}

fn find<'a>(nodes: &'a [(Vec<String>, MenuNode)], id: &str, params: &Value) -> Option<&'a MenuNode> {
    nodes.iter().map(|(_, n)| n).find(|n| matches!(n, MenuNode::Item { id: i, params: p, .. } if i == id && (params.is_null() || p == params)))
}

fn has(a: &LightcraftApp, photo: PhotoId, id: &str) -> bool {
    find(&items(&nodes(a, photo)), id, &Value::Null).is_some()
}

/// What the Photo menu can do to the selected photos, the right-click menu can do too (a mode
/// like Auto Advance and library-wide tools aside), so a new Photo command can't be left out.
#[test]
fn the_photo_menu_actions_are_all_offered() {
    let mut a = app();
    let p = photos(&mut a)[0];
    select(&mut a, &[p]);
    let bar = crate::menubar::menu_bar(&a);
    let menu = &bar.iter().find(|(t, _)| t == "Photo").expect("Photo menu").1;
    let ours = items(&nodes(&a, p));
    // a mode, library-wide tools, and Add to My Photos (offered only for Local photos)
    let not_for_a_photo =
        ["view.autoAdvance", "dialog.cull", "photo.readFocusPoints", "develop.updateProcess", "metadata.downloadPlaceNames", "photo.addToLibrary"];
    let missing: Vec<String> = items(menu)
        .into_iter()
        .filter_map(|(_, n)| match n {
            MenuNode::Item { id, params, .. } if !not_for_a_photo.contains(&id.as_str()) && find(&ours, &id, &params).is_none() => {
                Some(MenuNode::key(&id, &params))
            }
            _ => None,
        })
        .collect();
    assert!(missing.is_empty(), "the Photo menu has these and the right-click menu doesn't: {missing:?}");
}

/// The everyday actions a photo library's menu has beyond the Photo menu: metadata, edit
/// settings, the target album, Compare and the file's path.
#[test]
fn copy_paste_sync_and_the_target_album_are_offered() {
    let mut a = app();
    let v = photos(&mut a);
    select(&mut a, &[v[0], v[1]]);
    let ours = items(&nodes(&a, v[0]));
    for id in [
        "photo.copyMetadata",
        "photo.pasteMetadata",
        "develop.copy",
        "develop.paste",
        "dialog.pasteSettings",
        "develop.sync",
        "develop.auto",
        "album.toggleTarget",
        "view.compare",
        "view.survey",
        "photo.duplicate",
        "photo.convertToDng",
        "photo.flipHorizontal",
        "dialog.captureTime",
        "photo.copyPath",
        "app.exportPrevious",
    ] {
        assert!(find(&ours, id, &Value::Null).is_some(), "{id} is missing");
    }
    // presets, by group
    let preset = a.session.presets.first().expect("demo presets").id.clone();
    assert!(find(&ours, "preset.apply", &json!({"id": preset})).is_some(), "Apply Preset lists the presets");
}

/// Groups stay short and the long tails sit in submenus: Edit Settings and Metadata.
#[test]
fn actions_are_grouped_with_the_long_tails_in_submenus() {
    let mut a = app();
    let p = photos(&mut a)[0];
    select(&mut a, &[p]);
    let n = nodes(&a, p);
    let ours = items(&n);
    let under = |id: &str| ours.iter().find(|(_, x)| matches!(x, MenuNode::Item { id: i, .. } if i == id)).map(|(path, _)| path.clone());
    assert_eq!(under("develop.copy"), Some(vec!["Edit Settings".to_string()]));
    assert_eq!(under("develop.reset"), Some(vec!["Edit Settings".to_string()]));
    assert_eq!(under("photo.copyMetadata"), Some(vec!["Metadata".to_string()]));
    assert_eq!(under("dialog.captureTime"), Some(vec!["Metadata".to_string()]));
    assert_eq!(under("photo.flipVertical"), Some(vec!["Rotate & Flip".to_string()]));
    let mut group = 0;
    for x in &n {
        if *x == MenuNode::Separator {
            group = 0;
        } else {
            group += 1;
            assert!(group <= 6, "a group of more than 6 rows: {n:?}");
        }
    }
    // Delete stands alone at the end
    assert!(matches!(n.last(), Some(MenuNode::Item { id, .. }) if id == "photo.delete"), "{:?}", n.last());
    assert_eq!(n.get(n.len().saturating_sub(2)), Some(&MenuNode::Separator));
}

/// The labels, shortcuts and enabled state are the menu bar's (one model): "Delete 3 Photos".
#[test]
fn rows_have_the_menu_bars_labels_and_shortcuts() {
    let mut a = app();
    let v = photos(&mut a);
    select(&mut a, &v[..3]);
    let ours = items(&nodes(&a, v[0]));
    let bar = crate::menubar::menu_bar(&a);
    // the Photo menu's rows first (its Add to Album ▸ New Album… has no shortcut, File's has)
    let theirs: Vec<_> =
        bar.iter().filter(|(t, _)| t == "Photo").chain(bar.iter().filter(|(t, _)| t != "Photo")).flat_map(|(_, n)| items(n)).collect();
    let mut compared = 0;
    for (_, n) in &ours {
        let MenuNode::Item { id, params, label, shortcut, .. } = n else { continue };
        if id.starts_with("view.") {
            continue; // "Open in Detail" rather than the View menu's "Detail View"
        }
        if let Some(MenuNode::Item { label: l, shortcut: s, .. }) = find(&theirs, id, params) {
            assert_eq!((label, shortcut), (l, s), "{id}");
            compared += 1;
        }
    }
    assert!(compared > 20, "only {compared} rows compared");
    let delete = find(&ours, "photo.delete", &Value::Null).expect("delete");
    assert!(matches!(delete, MenuNode::Item { label, shortcut: Some(_), .. } if label == "Delete 3 Photos"), "{delete:?}");
}

/// The rating, flag and label submenus check the right-clicked photo's values, also when it
/// isn't the active one of a selection.
#[test]
fn rating_flag_and_label_check_the_right_clicked_photo() {
    let mut a = app();
    let v = photos(&mut a);
    let (active, clicked) = (v[0], v[1]);
    a.run("photo.rate", json!({"ids": [clicked.0], "rating": 4})).unwrap();
    a.run("photo.flag", json!({"ids": [clicked.0], "flag": "pick"})).unwrap();
    a.run("photo.label", json!({"ids": [clicked.0], "label": "green"})).unwrap();
    a.run("photo.rate", json!({"ids": [active.0], "rating": 1})).unwrap();
    select(&mut a, &[active, clicked]);
    assert_eq!(a.session.active(), Some(active));
    let ours = items(&nodes(&a, clicked));
    let checked = |id: &str, params: Value| match find(&ours, id, &params) {
        Some(MenuNode::Item { checked, .. }) => *checked,
        other => panic!("{id} {params}: {other:?}"),
    };
    assert_eq!(checked("photo.rate", json!({"rating": 4})), Some(true));
    assert_eq!(checked("photo.rate", json!({"rating": 1})), Some(false));
    assert_eq!(checked("photo.flag", json!({"flag": "pick"})), Some(true));
    assert_eq!(checked("photo.label", json!({"label": "green"})), Some(true));
    assert_eq!(checked("photo.label", json!({"label": "none"})), Some(false));
}

/// Only what makes sense where the menu opened: no "Open in Detail" in Detail, Compare and
/// Survey once several photos are chosen.
#[test]
fn view_rows_follow_the_view_and_the_selection() {
    let mut a = app();
    let v = photos(&mut a);
    select(&mut a, &[v[0]]);
    a.ui.view = ViewMode::PhotoGrid;
    assert!(has(&a, v[0], "view.detail"));
    assert!(!has(&a, v[0], "view.compare"), "Compare needs two photos");
    a.ui.view = ViewMode::Detail;
    assert!(!has(&a, v[0], "view.detail"), "already in Detail (the filmstrip)");
    select(&mut a, &[v[0], v[1]]);
    assert!(has(&a, v[0], "view.compare") && has(&a, v[0], "view.survey"));
    a.ui.view = ViewMode::Compare;
    assert!(!has(&a, v[0], "view.compare"), "already comparing");
    let detail = find(&items(&nodes(&a, v[0])), "view.detail", &Value::Null).cloned();
    assert!(matches!(&detail, Some(MenuNode::Item { label, checked: None, .. }) if label == "Open in Detail"), "{detail:?}");
}

/// A row that is off says why, where the reason isn't obvious.
#[test]
fn rows_that_are_off_say_why() {
    let mut a = app();
    let p = photos(&mut a)[0];
    select(&mut a, &[p]);
    let ours = items(&nodes(&a, p));
    for id in ["dialog.mergeHdr", "develop.paste", "photo.copyPath"] {
        assert!(matches!(find(&ours, id, &Value::Null), Some(MenuNode::Item { enabled: false, .. })), "{id} should be off here");
        assert!(disabled_reason(&a, id, &Value::Null).is_some_and(|r| !r.is_empty()), "{id} says nothing");
    }
    assert_eq!(disabled_reason(&a, "dialog.mergeHdr", &Value::Null).as_deref(), Some("Select 2 or more photos"));
}

/// A photo in Recently Deleted gets Restore and Delete Permanently instead of Delete.
#[test]
fn a_deleted_photo_offers_restore() {
    let mut a = app();
    a.ui.settings.confirm_delete = false;
    let p = photos(&mut a)[0];
    select(&mut a, &[p]);
    a.run("photo.delete", json!({})).unwrap();
    select(&mut a, &[p]);
    assert!(has(&a, p, "photo.restore") && has(&a, p, "photo.deletePermanently"));
    assert!(!has(&a, p, "photo.delete"));
}

/// Give a photo a file on disk (the path is only read, never opened).
fn on_disk(a: &mut LightcraftApp, id: PhotoId, path: &str) {
    let name = path.rsplit('/').next().unwrap_or(path).to_string();
    a.session
        .catalog
        .apply(lightcraft_catalog::Op::SetFile { id, file_name: name, source: lightcraft_catalog::Source::File { path: path.into() } })
        .unwrap();
}

/// The rating, flag and label rows with their parameters.
fn culling_rows(a: &LightcraftApp, photo: PhotoId) -> Vec<Value> {
    items(&nodes(a, photo))
        .into_iter()
        .filter_map(|(_, n)| match n {
            MenuNode::Item { id, params, .. } if matches!(id.as_str(), "photo.rate" | "photo.flag" | "photo.label") => Some(params),
            _ => None,
        })
        .collect()
}

/// In Compare and Survey a rating, flag or label row names the right-clicked photo (a click
/// there would otherwise change only the active one); elsewhere it changes the selection.
#[test]
fn culling_rows_name_the_photo_in_compare_and_survey() {
    let mut a = app();
    let v = photos(&mut a);
    select(&mut a, &[v[0], v[1]]);
    let rows = culling_rows(&a, v[1]);
    assert!(!rows.is_empty() && rows.iter().all(|p| p.get("ids").is_none()), "in the grid: {rows:?}");
    for view in [ViewMode::Compare, ViewMode::Survey] {
        a.ui.view = view;
        let rows = culling_rows(&a, v[1]);
        assert!(!rows.is_empty() && rows.iter().all(|p| p["ids"] == json!([v[1].0])), "{view:?}: {rows:?}");
    }
}

/// Add to My Photos is offered for a Local photo only; a selection of Local photos gets an Add to
/// Album row that is off and says to add them first.
#[test]
fn local_photos_are_offered_add_to_my_photos() {
    let mut a = app();
    let p = photos(&mut a)[0];
    select(&mut a, &[p]);
    assert!(!has(&a, p, "photo.addToLibrary"));
    a.session.catalog.apply(lightcraft_catalog::Op::SetLocal { id: p, local: true }).unwrap();
    let ours = items(&nodes(&a, p));
    assert!(matches!(find(&ours, "photo.addToLibrary", &Value::Null), Some(MenuNode::Item { enabled: true, .. })));
    assert!(matches!(find(&ours, "album.addPhotos", &Value::Null), Some(MenuNode::Item { enabled: false, .. })));
    assert_eq!(disabled_reason(&a, "album.addPhotos", &Value::Null), Some(crate::panels::grid::albums_hint()));
}

/// Inside an album, Remove from Album and Set as Album Cover (of that album, with this photo);
/// outside one, neither.
#[test]
fn album_rows_only_inside_an_album() {
    let mut a = app();
    let v = photos(&mut a);
    select(&mut a, &[v[2]]);
    assert!(!has(&a, v[2], "album.removePhotos") && !has(&a, v[2], "album.setCover"));
    let album = a.session.execute("album.create", &json!({"name": "Kept"})).unwrap()["id"].as_u64().unwrap();
    a.session.execute("album.addPhotos", &json!({"id": album, "ids": [v[2].0]})).unwrap();
    a.run("library.source", json!({"kind": "album", "id": album})).unwrap();
    select(&mut a, &[v[2]]);
    assert!(has(&a, v[2], "album.removePhotos"));
    let ours = items(&nodes(&a, v[2]));
    assert!(find(&ours, "album.setCover", &json!({"id": album, "photo": v[2].0})).is_some(), "the cover row names the album and the photo");
}

/// Locate Missing File… only for a photo whose original is known to be gone.
#[test]
fn locate_only_for_a_missing_original() {
    let mut a = app();
    let v = photos(&mut a);
    on_disk(&mut a, v[0], "/pics/here.jpg");
    on_disk(&mut a, v[1], "/pics/gone.jpg");
    a.session.media.availability.set_probe(std::sync::Arc::new(|path: &str| path != "/pics/gone.jpg"));
    select(&mut a, &[v[0]]);
    assert!(!has(&a, v[0], "photo.locate"));
    select(&mut a, &[v[1]]);
    assert!(has(&a, v[1], "photo.locate"));
}

/// The rows about one photo carry it: Find Similar, Show in Folders and Set as Reference ask for
/// the right-clicked photo, not the active one.
#[test]
fn photo_rows_carry_the_right_clicked_photo() {
    let mut a = app();
    let v = photos(&mut a);
    select(&mut a, &[v[0], v[3]]);
    assert_eq!(a.session.active(), Some(v[0]));
    let ours = items(&nodes(&a, v[3]));
    for id in ["library.findSimilar", "app.showInFolders", "photo.setReference"] {
        assert!(find(&ours, id, &json!({"id": v[3].0})).is_some(), "{id} names the right-clicked photo");
    }
}

/// Set as Reference Photo is off for a photo in Recently Deleted, and says why.
#[test]
fn a_deleted_photo_cant_be_the_reference() {
    let mut a = app();
    a.ui.settings.confirm_delete = false;
    let p = photos(&mut a)[0];
    select(&mut a, &[p]);
    a.run("photo.delete", json!({})).unwrap();
    select(&mut a, &[p]);
    let ours = items(&nodes(&a, p));
    assert!(matches!(find(&ours, "photo.setReference", &Value::Null), Some(MenuNode::Item { enabled: false, .. })));
    assert_eq!(disabled_reason(&a, "photo.setReference", &json!({"id": p.0})).as_deref(), Some("Restore the photo to use it as the reference"));
}

/// Export with Preset holds Export with Previous and the presets, not a second Export… (the row
/// above it is that).
#[test]
fn export_with_preset_has_no_second_export() {
    let mut a = app();
    let p = photos(&mut a)[0];
    select(&mut a, &[p]);
    let ours = items(&nodes(&a, p));
    let export: Vec<&MenuNode> = ours.iter().filter(|(path, _)| path.iter().any(|s| s == "Export with Preset")).map(|(_, n)| n).collect();
    assert!(export.iter().any(|n| matches!(n, MenuNode::Item { id, .. } if id == "app.exportPrevious")));
    assert!(!export.iter().any(|n| matches!(n, MenuNode::Item { id, .. } if id == "dialog.export")));
    assert_eq!(ours.iter().filter(|(_, n)| matches!(n, MenuNode::Item { id, .. } if id == "dialog.export")).count(), 1);
}

/// Apply Preset lists the groups as the Presets panel does: Favorites first, then by name.
#[test]
fn apply_preset_groups_follow_the_presets_panel() {
    let mut a = app();
    let p = photos(&mut a)[0];
    select(&mut a, &[p]);
    let star = a.session.presets.iter().position(|x| x.builtin).expect("a stock preset");
    a.session.presets[star].favorite = true;
    let starred = a.session.presets[star].id.clone();
    let mut groups: Vec<String> = a.session.presets.iter().map(|x| x.group.clone()).collect();
    groups.sort_unstable();
    groups.dedup();
    let apply = nodes(&a, p)
        .into_iter()
        .find_map(|n| match n {
            MenuNode::Submenu { label, children } if label == "Edit Settings" => children.into_iter().find_map(|c| match c {
                MenuNode::Submenu { label, children } if label == "Apply Preset" => Some(children),
                _ => None,
            }),
            _ => None,
        })
        .expect("Edit Settings ▸ Apply Preset");
    let labels: Vec<&str> = apply.iter().filter_map(|n| if let MenuNode::Submenu { label, .. } = n { Some(label.as_str()) } else { None }).collect();
    let mut want = vec!["Favorites"];
    want.extend(groups.iter().map(String::as_str));
    assert_eq!(labels, want);
    let MenuNode::Submenu { children, .. } = &apply[0] else { panic!("Favorites") };
    assert!(children.iter().any(|c| matches!(c, MenuNode::Item { params, .. } if params["id"] == json!(starred))));
}

/// The reasons of rows that need the desktop app or a file say so.
#[test]
fn host_and_file_reasons() {
    let mut a = app();
    let p = photos(&mut a)[0];
    select(&mut a, &[p]);
    assert!(a.services.reveal.is_none() && a.services.open_with.is_none(), "a test app has neither");
    let why = |id: &str| disabled_reason(&a, id, &json!({"id": p.0}));
    assert_eq!(why("app.showInFinder").as_deref(), Some("This version of LightCraft can't open the file manager"));
    assert_eq!(why("photo.openOriginalInExternal").as_deref(), Some("Opening other programs needs the desktop app"));
    assert_eq!(why("app.showInFolders").as_deref(), Some("This photo is not in a folder of the library"));
}

/// Copy File Path puts each selected photo's path on the clipboard once (a virtual copy shares its
/// original's file), one per line, and says how many.
#[test]
fn copy_file_path_copies_each_file_once() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
    let mut h = crate::headless::Headless::new(app, [1200.0, 800.0], 1.0);
    let v = h.app.session.visible_cloned();
    on_disk(&mut h.app, v[0], "/pics/a.jpg");
    on_disk(&mut h.app, v[1], "/pics/b.jpg");
    let ask = |h: &mut crate::headless::Headless, ids: Vec<u64>| {
        h.app.session.execute("library.select", &json!({"ids": ids})).unwrap();
        let r = h.request("engine.execute", json!({"command": "photo.copyPath"}), std::time::Duration::from_secs(20));
        h.step();
        h.step();
        r
    };
    // (a frame first: commands run outside a frame copy through the context it last drew)
    h.settle(std::time::Duration::from_secs(120));
    let r = ask(&mut h, vec![v[0].0]);
    assert_eq!(r["result"]["paths"], json!(["/pics/a.jpg"]), "{r}");
    assert_eq!(h.app.copied.as_deref(), Some("/pics/a.jpg"));
    assert_eq!(h.app.ui.toast.as_ref().map(|t| t.0.as_str()), Some("Copied the file path"));
    h.app.session.execute("library.select", &json!({"ids": [v[0].0]})).unwrap();
    let copy = h.app.session.execute("photo.virtualCopy", &json!({})).unwrap();
    let copy = copy["ids"][0].as_u64().or_else(|| copy["id"].as_u64()).expect("the virtual copy's id");
    let r = ask(&mut h, vec![v[0].0, copy, v[1].0]);
    assert_eq!(r["result"]["paths"], json!(["/pics/a.jpg", "/pics/b.jpg"]), "{r}");
    assert_eq!(h.app.copied.as_deref(), Some("/pics/a.jpg\n/pics/b.jpg"));
    assert_eq!(h.app.ui.toast.as_ref().map(|t| t.0.as_str()), Some("Copied 2 file paths"));
}
