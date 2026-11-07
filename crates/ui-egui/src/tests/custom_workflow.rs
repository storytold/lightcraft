use super::fixtures::SyntheticLibrary;
use crate::headless::Headless;
use crate::state::{RightPanel, ViewMode};
use serde_json::json;
use std::time::Duration;

const T: Duration = Duration::from_secs(30);

#[test]
fn metadata_panel_reads_sidecar_and_copies_complete_path() {
    let fixture = SyntheticLibrary::new("metadata-file-tags");
    let app = fixture.app();
    let id = app.session.selection.active.unwrap();
    let lightcraft_catalog::Source::File { path } = &app.session.catalog.photo(id).unwrap().source else { panic!("file fixture") };
    let path = path.clone();
    std::fs::write(
        std::path::Path::new(&path).with_extension("xmp"),
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/" dc:creator="Sidecar-only author" dc:title="2014:01:01"/></rdf:RDF></x:xmpmeta>"#,
    ).unwrap();
    let mut h = Headless::new(app, [1400.0, 1600.0], 1.0);
    h.app.run("panel.metadata", json!({})).unwrap();
    assert!(h.settle(T));
    let text = h.painted_text();
    assert!(text.iter().any(|s| s == &path), "complete path: {text:?}");
    assert!(text.iter().any(|s| s == "Sidecar-only author"), "file metadata: {text:?}");
    assert!(text.iter().any(|s| s == "2014:01:01"), "literal XMP title: {text:?}");
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:metadataCopyPath"}), T)["ok"], true);
    h.step();
    assert_eq!(h.view.clipboard, path);
}

#[test]
fn metadata_panel_follows_selection_without_edit_fields() {
    let fixture = SyntheticLibrary::with_photos("metadata-panel", 2);
    let mut h = Headless::new(fixture.app(), [1400.0, 900.0], 1.0);
    let ids = h.app.session.visible_cloned();
    h.app.run("photo.setMeta", json!({"ids": [ids[0].0], "title": "First metadata title"})).unwrap();
    h.app.run("photo.setMeta", json!({"ids": [ids[1].0], "title": "Second metadata title"})).unwrap();
    h.app.run("library.select", json!({"ids": [ids[0].0]})).unwrap();
    h.app.run("panel.metadata", json!({})).unwrap();
    h.settle(T);
    assert_eq!(h.app.ui.right, RightPanel::Metadata);
    assert!(h.painted_text().iter().any(|s| s == "First metadata title"));
    assert!(!h.app.widgets.iter().any(|(id, _)| id == "field:title" || id == "field:caption"));
    h.app.run("library.select", json!({"ids": [ids[1].0]})).unwrap();
    h.step();
    h.step();
    assert!(h.painted_text().iter().any(|s| s == "Second metadata title"));
    assert!(!h.painted_text().iter().any(|s| s == "First metadata title"));
    let saved = serde_json::to_string(&h.app.ui).unwrap();
    assert_eq!(serde_json::from_str::<crate::UiState>(&saved).unwrap().right, RightPanel::Metadata);
}

#[test]
fn settings_keys_records_space_and_grid_uses_it() {
    let fixture = SyntheticLibrary::new("settings-keys");
    let mut h = Headless::new(fixture.app(), [1400.0, 900.0], 1.0);
    h.app.ui.view = ViewMode::Detail;
    h.app.run("app.settings", json!({"tab": "keys"})).unwrap();
    h.step();
    h.step();
    assert!(h.app.widgets.iter().any(|(id, _)| id == "field:shortcutsSearch"));
    // The recorder must remain active while Settings owns the keyboard, just as in Help.
    h.app.recording_shortcut = Some("view.gridToggle".into());
    assert_eq!(h.request("ui.key", json!({"key": "Space"}), T)["ok"], true);
    h.step();
    h.step();
    assert_eq!(crate::shortcuts::shortcut_of(&h.app.ui.settings.keymap, "view.gridToggle"), Some("Space"));
    assert_eq!(h.app.ui.view, ViewMode::Detail);
    assert!(h.app.ui.dialog.is_some());
    h.app.ui.dialog = None;
    h.step();
    assert_eq!(h.request("ui.key", json!({"key": "Space"}), T)["ok"], true);
    h.step();
    h.step();
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid);
    assert!(h.app.space_hand.is_none());
    let saved = serde_json::to_string(&h.app.ui).unwrap();
    let back: crate::UiState = serde_json::from_str(&saved).unwrap();
    assert_eq!(crate::shortcuts::shortcut_of(&back.settings.keymap, "view.gridToggle"), Some("Space"));
}

#[test]
fn album_operations_are_live_and_plain_source_clears_them() {
    let fixture = SyntheticLibrary::with_photos("album-operations", 3);
    let mut app = fixture.app();
    let ids = app.session.visible_cloned();
    let a = app.run("album.create", json!({"name": "All"})).unwrap()["id"].as_u64().unwrap();
    let b = app.run("album.create", json!({"name": "Children"})).unwrap()["id"].as_u64().unwrap();
    app.run("album.addPhotos", json!({"id": a, "ids": [ids[0].0, ids[1].0]})).unwrap();
    app.run("album.addPhotos", json!({"id": b, "ids": [ids[1].0, ids[2].0]})).unwrap();
    app.run("library.source", json!({"kind": "album", "id": a})).unwrap();
    app.session.filter.flags = vec![lightcraft_catalog::Flag::Reject];
    app.session.filter.labels = vec![lightcraft_catalog::ColorLabel::Red];
    app.session.filter.only = vec![ids[2]];
    app.session.filter.only_none = true;
    app.run("view.combineAlbums", json!({"ids": [a, b]})).unwrap();
    assert!(app.session.filter.flags.is_empty());
    assert!(app.session.filter.labels.is_empty());
    assert!(app.session.filter.only.is_empty());
    assert!(!app.session.filter.only_none);
    app.session.filter.text = "narrow".into();
    app.session.filter.flags = vec![lightcraft_catalog::Flag::Pick];
    app.session.filter.labels = vec![lightcraft_catalog::ColorLabel::Blue];
    app.session.filter.only = vec![ids[1]];
    app.session.filter.only_none = true;
    app.ui.search = "narrow".into();
    app.run("view.combineAlbums", json!({"ids": [a, b], "operation": "intersection"})).unwrap();
    assert_eq!(app.session.filter.text, "narrow");
    assert_eq!(app.session.filter.flags, [lightcraft_catalog::Flag::Pick]);
    assert_eq!(app.session.filter.labels, [lightcraft_catalog::ColorLabel::Blue]);
    assert_eq!(app.session.filter.only, [ids[1]]);
    assert!(app.session.filter.only_none);
    assert_eq!(app.ui.search, "narrow");
    app.session.filter.text.clear();
    app.session.filter.flags.clear();
    app.session.filter.labels.clear();
    app.session.filter.only.clear();
    app.session.filter.only_none = false;
    app.ui.search.clear();
    for (operation, expected) in
        [("union", vec![ids[0], ids[1], ids[2]]), ("intersection", vec![ids[1]]), ("difference", vec![ids[0]]), ("exclusive", vec![ids[0], ids[2]])]
    {
        app.run("view.combineAlbums", json!({"ids": [a, b], "operation": operation})).unwrap();
        let visible = app.session.visible_cloned();
        assert_eq!(visible.len(), expected.len(), "{operation}");
        assert!(expected.iter().all(|id| visible.contains(id)), "{operation}");
    }
    app.run("view.combineAlbums", json!({"ids": [a, b], "operation": "difference"})).unwrap();
    app.run("album.addPhotos", json!({"id": b, "ids": [ids[0].0]})).unwrap();
    assert!(app.session.visible_cloned().is_empty());
    let before = app.session.filter.clone();
    assert!(app.run("view.combineAlbums", json!({"ids": [a, 999999]})).is_err());
    assert!(app.run("view.combineAlbums", json!({"ids": [a, b], "operation": false})).is_err());
    assert_eq!(app.session.filter, before);
    let result = app.run("library.source", json!({"kind": "album", "id": a})).unwrap();
    assert_eq!(result["count"], 2);
    assert_eq!(app.session.visible_cloned().len(), 2);
    assert!(!crate::album_combination::active(&app));
    app.run("view.combineAlbums", json!({"ids": [a, b]})).unwrap();
    app.run("library.toggleFilters", json!({})).unwrap();
    assert!(app.session.filters_off());
    app.run("album.delete", json!({"id": b})).unwrap();
    app.run("library.toggleFilters", json!({})).unwrap();
    assert!(!app.session.filters_off());
    assert!(app.ui.album_combination.is_none());
    assert!(app.session.filter.rule_set.is_none());
}

#[test]
fn command_click_album_row_combines_the_two_albums() {
    let fixture = SyntheticLibrary::with_photos("album-command-click", 2);
    let mut app = fixture.app();
    let a = app.run("album.create", json!({"name": "First album"})).unwrap()["id"].as_u64().unwrap();
    let b = app.run("album.create", json!({"name": "Second album"})).unwrap()["id"].as_u64().unwrap();
    app.ui.left_panel = true;
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    assert!(h.settle(T));
    assert_eq!(h.request("ui.clickWidget", json!({"id": format!("source:album:{a}")}), T)["ok"], true);
    assert_eq!(h.request("ui.clickWidget", json!({"id": format!("source:album:{b}"), "cmd": true}), T)["ok"], true);
    assert!(crate::album_combination::active(&h.app));
    assert_eq!(h.app.ui.album_combination.as_ref().unwrap().0, [a, b]);
}

#[test]
fn metadata_reads_for_another_photo_can_finish_before_a_slow_read() {
    let fixture = SyntheticLibrary::with_photos("metadata-slow-read", 2);
    let mut app = fixture.app();
    let ids = app.session.visible_cloned();
    let lightcraft_catalog::Source::File { path } = &app.session.catalog.photo(ids[0]).unwrap().source else { panic!("file fixture") };
    let slow_path = path.clone();
    let (release, wait) = std::sync::mpsc::channel();
    let wait = std::sync::Mutex::new(wait);
    app.session.media.file_bytes = Some(std::sync::Arc::new(move |path| {
        if path == slow_path {
            let _ = wait.lock().unwrap().recv_timeout(T);
            Err("slow original failed".into())
        } else {
            Err("other original failed".into())
        }
    }));
    app.run("library.select", json!({"ids": [ids[0].0]})).unwrap();
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    h.app.run("panel.metadata", json!({})).unwrap();
    assert!(h.step_until(T, |h| h.app.tasks.labels().iter().any(|name| name.starts_with("Metadata panel"))));
    h.app.run("library.select", json!({"ids": [ids[1].0]})).unwrap();
    assert!(h.step_until(T, |h| h.painted_text().iter().any(|text| text == "other original failed")));
    release.send(()).unwrap();
    assert!(h.settle(T));
    assert!(h.painted_text().iter().any(|text| text == "other original failed"));
    assert!(!h.painted_text().iter().any(|text| text == "slow original failed"));
}
