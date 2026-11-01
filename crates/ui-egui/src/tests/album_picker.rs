//! The shared album picker in smart rules and the import review: sidebar hierarchy, path search,
//! keyboard choices, and only valid destinations. Import also retains None and New album.

use std::time::Duration;

use lightcraft_catalog::Rule;
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// Dispatch a public command and return its successful engine result.
fn exec(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

/// Read the album ID returned by a successful creation command.
fn id(v: serde_json::Value) -> u64 {
    v["id"].as_u64().expect("an id")
}

/// Click a registered widget through the public control channel and let frames settle.
fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.settle(SETTLE);
}

/// Check the current frame registry for an exact widget ID.
fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

/// Read the Album rule value from the active smart-rule editor.
fn value(h: &Headless) -> serde_json::Value {
    let Some(crate::state::Dialog::SmartRules { rules, .. }) = &h.app.ui.dialog else { panic!("no rule editor") };
    let Some(Rule::Field { value, .. }) = rules.rules.first() else { panic!("no rule") };
    value.clone()
}

struct Library {
    h: Headless,
    utils: u64,
    excluded: u64,
    uncurated: u64,
    trip: u64,
}

/// [UTILS] > Excluded (smart), Uncurated (smart); Trip; and the rule editor on a new album with one
/// Album rule.
fn library() -> Library {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let utils = id(exec(&mut h, "album.create", json!({"name": "[UTILS]", "folder": true})));
    let excluded = id(exec(&mut h, "album.createSmart", json!({"name": "Excluded", "rules": {"rating": 1}, "parent": utils})));
    let uncurated = id(exec(&mut h, "album.createSmart", json!({"name": "Uncurated", "rules": {"rating": 0}, "parent": utils})));
    let trip = id(exec(&mut h, "album.create", json!({"name": "Trip", "addSelected": false})));
    exec(&mut h, "dialog.smartAlbum", json!({"name": "Travel"}));
    let Some(crate::state::Dialog::SmartRules { rules, .. }) = &mut h.app.ui.dialog else { panic!("no rule editor") };
    rules.rules = vec![serde_json::from_value(json!({"field": "album", "op": "isNot", "value": null})).unwrap()];
    h.settle(SETTLE);
    Library { h, utils, excluded, uncurated, trip }
}

/// With nothing typed, the picker shows the sidebar's tree: folders start closed and open on click.
#[test]
fn the_tree_opens_folder_by_folder() {
    let Library { mut h, utils, excluded, trip, .. } = library();
    click(&mut h, "albumPicker:rules-0");
    assert!(has(&h, &format!("albumPickerFolder:{utils}:rules-0")) && has(&h, &format!("albumPickerItem:{trip}:rules-0")));
    assert!(!has(&h, &format!("albumPickerItem:{excluded}:rules-0")), "inside a closed folder");
    click(&mut h, &format!("albumPickerFolder:{utils}:rules-0"));
    click(&mut h, &format!("albumPickerItem:{excluded}:rules-0"));
    assert_eq!(value(&h), json!(excluded));
    assert!(!has(&h, &format!("albumPickerItem:{trip}:rules-0")), "picking closes it");
    // it opens again on the chosen album's folder
    click(&mut h, "albumPicker:rules-0");
    assert!(has(&h, &format!("albumPickerItem:{excluded}:rules-0")));
}

/// Typing narrows the albums to those whose path holds every word; Enter picks the first, the
/// arrow keys move through them.
#[test]
fn typing_finds_and_keys_pick() {
    let Library { mut h, excluded, uncurated, trip, .. } = library();
    click(&mut h, "albumPicker:rules-0");
    let r = h.request("ui.text", json!({"text": "utils"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(has(&h, &format!("albumPickerItem:{excluded}:rules-0")) && has(&h, &format!("albumPickerItem:{uncurated}:rules-0")));
    assert!(!has(&h, &format!("albumPickerItem:{trip}:rules-0")), "Trip doesn't match");
    for key in ["ArrowDown", "Enter"] {
        let r = h.request("ui.key", json!({"key": key}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    }
    assert_eq!(value(&h), json!(uncurated), "the second match");
}

/// Albums that can't be picked (the one being edited, one that would loop) are shown greyed, not
/// hidden, and clicking them does nothing.
#[test]
fn albums_that_would_loop_are_greyed() {
    let Library { mut h, excluded, .. } = library();
    let rules = json!({"ruleSet": {"rules": [{"field": "album", "op": "isNot", "value": excluded}]}});
    let travel = id(exec(&mut h, "album.createSmart", json!({"name": "Travel", "rules": rules})));
    exec(&mut h, "dialog.smartAlbum", json!({"id": excluded}));
    let Some(crate::state::Dialog::SmartRules { rules, .. }) = &mut h.app.ui.dialog else { panic!("no rule editor") };
    rules.rules = vec![serde_json::from_value(json!({"field": "album", "op": "is", "value": null})).unwrap()];
    h.settle(SETTLE);
    click(&mut h, "albumPicker:rules-0");
    assert!(has(&h, &format!("albumPickerItem:{travel}:rules-0")), "shown");
    click(&mut h, &format!("albumPickerItem:{travel}:rules-0"));
    assert_eq!(value(&h), json!(null), "but not picked");
}

/// The registered rectangle of `id` in the current frame.
fn rect_of(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

/// The height of the smallest area (the popup, not the dialog) that holds the search field, as egui keeps it between frames.
fn popup_height(h: &Headless, search: egui::Rect) -> f32 {
    let ctx = &h.view.ctx;
    let layers = ctx.memory(|m| m.areas().visible_layer_ids());
    layers
        .into_iter()
        .filter_map(|layer| egui::AreaState::load(ctx, layer.id))
        .filter(|a| a.rect().contains_rect(search))
        .filter_map(|a| a.size.map(|s| s.y))
        .fold(f32::MAX, f32::min)
}

/// Filtering to nothing shrinks the popup and clearing the filter grows it back (egui#5225),
/// with the search field in the same place throughout.
#[test]
fn the_popup_grows_back_when_the_search_is_cleared() {
    let Library { mut h, .. } = library();
    click(&mut h, "albumPicker:rules-0");
    let search = rect_of(&h, "albumPickerSearch:rules-0");
    let full = popup_height(&h, search);
    assert!(full > 80.0, "the open popup shows the tree: {full}");
    let r = h.request("ui.text", json!({"text": "zzzz"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let none = popup_height(&h, search);
    assert!(none < full - 20.0, "no match shrinks the popup: {none} vs {full}");
    assert_eq!(rect_of(&h, "albumPickerSearch:rules-0"), search, "the search field stays put");
    for _ in 0..4 {
        let r = h.request("ui.key", json!({"key": "Backspace"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    }
    let again = popup_height(&h, search);
    assert!((again - full).abs() < 1.0, "cleared, the popup is {again} tall, not {full} as when it opened");
    assert_eq!(rect_of(&h, "albumPickerSearch:rules-0"), search, "the search field stays put");
}

/// After opening a folder, typing still goes to the search: the box keeps the keyboard.
#[test]
fn typing_after_a_folder_click_still_searches() {
    let Library { mut h, utils, excluded, trip, .. } = library();
    click(&mut h, "albumPicker:rules-0");
    click(&mut h, &format!("albumPickerFolder:{utils}:rules-0"));
    assert!(has(&h, &format!("albumPickerItem:{excluded}:rules-0")));
    let r = h.request("ui.text", json!({"text": "trip"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(has(&h, &format!("albumPickerItem:{trip}:rules-0")) && !has(&h, &format!("albumPickerItem:{excluded}:rules-0")), "searching for trip");
}

/// The import review reuses the same tree, while accepting only regular album destinations.
mod import_review {
    use super::*;

    struct Review {
        h: Headless,
        folder: u64,
        nested: u64,
        album: u64,
        other: u64,
        smart: u64,
    }

    /// Open the standard import review fixture with nested and same-named albums.
    fn review() -> Review {
        review_at([1300.0, 950.0], 1.0)
    }

    /// Build the import review fixture at the requested logical size and display density.
    fn review_at(size: [f32; 2], density: f32) -> Review {
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo().with_fs(), Services { png: None, ..Default::default() });
        let mut h = Headless::new(app, size, density);
        let folder = id(exec(&mut h, "album.create", json!({"name": "Travel", "folder": true})));
        let nested = id(exec(&mut h, "album.create", json!({"name": "2026", "folder": true, "parent": folder})));
        let album = id(exec(&mut h, "album.create", json!({"name": "Favorites", "parent": nested, "addSelected": false})));
        let other = id(exec(&mut h, "album.create", json!({"name": "Favorites", "addSelected": false})));
        let smart = id(exec(&mut h, "album.createSmart", json!({"name": "Smart travel", "parent": folder, "rules": {"rating": 4}})));
        h.app.ui.dialog = Some(crate::state::Dialog::Import { opts: Box::new(crate::import::ImportDialog::new(Vec::new())) });
        h.settle(SETTLE);
        Review { h, folder, nested, album, other, smart }
    }

    /// Read the active import review without accepting a silently closed dialog.
    fn destination(h: &Headless) -> &crate::import::ImportDialog {
        let Some(crate::state::Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("import review remains open") };
        opts
    }

    /// #2362: the actual import dialog browses nested folders and remembers the chosen full path.
    #[test]
    fn import_album_picker_preserves_the_sidebar_hierarchy() {
        let Review { mut h, folder, nested, album, other, smart } = review();
        click(&mut h, "albumPicker:import-album");
        assert!(has(&h, &format!("albumPickerFolder:{folder}:import-album")));
        assert!(has(&h, &format!("albumPickerItem:{other}:import-album")));
        assert!(!has(&h, &format!("albumPickerItem:{album}:import-album")), "nested album starts folded");
        click(&mut h, &format!("albumPickerFolder:{folder}:import-album"));
        assert_eq!(destination(&h).album, None, "a folder is for browsing, not a destination");
        assert!(!has(&h, &format!("albumPickerItem:{smart}:import-album")), "smart albums cannot receive imports");
        click(&mut h, &format!("albumPickerFolder:{nested}:import-album"));
        click(&mut h, &format!("albumPickerItem:{album}:import-album"));
        assert_eq!(destination(&h).album, Some(album));
        assert!(destination(&h).new_album.is_empty());
        assert!(h.painted_text().iter().any(|t| t == "Travel / 2026 / Favorites"), "duplicate album names have their full path");
        click(&mut h, "albumPicker:import-album");
        assert!(has(&h, &format!("albumPickerItem:{album}:import-album")), "selected album's ancestors reopen");
    }

    /// Type through the public control channel and wait for the search or field to update.
    fn text(h: &mut Headless, value: &str) {
        let r = h.request("ui.text", json!({"text": value}), T);
        assert_eq!(r["ok"], true, "{r}");
        assert!(h.settle(SETTLE));
    }

    /// Send a public keyboard event and wait for its resulting UI state.
    fn key(h: &mut Headless, key: &str) {
        let r = h.request("ui.key", json!({"key": key}), T);
        assert_eq!(r["ok"], true, "{r}");
        assert!(h.settle(SETTLE));
    }

    /// Replace the pending new album name using normal field selection and typing.
    fn replace_name(h: &mut Headless, name: &str) {
        click(h, "field:importAlbumName");
        let r = h.request("ui.key", json!({"key": "a", "cmd": true}), T);
        assert_eq!(r["ok"], true, "{r}");
        text(h, name);
    }

    /// None, New and an existing album replace each other without retaining a stale pending name.
    #[test]
    fn import_album_picker_retains_none_new_and_existing_transitions() {
        let Review { mut h, album, .. } = review();
        click(&mut h, "albumPicker:import-album");
        click(&mut h, "importAlbum:new");
        assert_eq!(destination(&h).album, None);
        assert_eq!(destination(&h).new_album, "Imported Photos");
        replace_name(&mut h, "Été 東京");
        click(&mut h, "albumPicker:import-album");
        click(&mut h, "importAlbum:new");
        assert_eq!(destination(&h).new_album, "Été 東京", "choosing New again keeps the typed name");
        click(&mut h, "albumPicker:import-album");
        text(&mut h, "travel 2026 fav");
        key(&mut h, "Enter");
        assert_eq!(destination(&h).album, Some(album));
        assert!(destination(&h).new_album.is_empty(), "existing album replaces a pending new album");
        assert!(!has(&h, "field:importAlbumName"));
        click(&mut h, "albumPicker:import-album");
        click(&mut h, "importAlbum:none");
        assert_eq!(destination(&h).album, None);
        assert!(destination(&h).new_album.is_empty());
        click(&mut h, "albumPicker:import-album");
        click(&mut h, "importAlbum:new");
        replace_name(&mut h, "Leave no orphan album");
        click(&mut h, "albumPicker:import-album");
        click(&mut h, "importAlbum:none");
        assert!(destination(&h).new_album.is_empty(), "None clears a pending name too");
    }

    /// The picker must leave room for a usable name field; a full-width button followed by an
    /// inline name field made the resizable dialog grow on every frame, off the native window.
    #[test]
    fn import_album_picker_keeps_the_new_name_inside_the_dialog() {
        let Review { mut h, .. } = review();
        click(&mut h, "albumPicker:import-album");
        click(&mut h, "importAlbum:new");
        for _ in 0..20 {
            h.step();
        }
        let name = h.app.widgets.iter().find(|(id, _)| id == "field:importAlbumName").unwrap().1;
        assert!(name.width() >= 160.0, "the name field is usable: {name:?}");
        assert!(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1300.0, 950.0)).contains_rect(name), "name stays on screen: {name:?}");
        let picker = |h: &Headless| h.app.widgets.iter().find(|(id, _)| id == "albumPicker:import-album").unwrap().1;
        let before = picker(&h);
        for _ in 0..10 {
            h.step();
        }
        let after = picker(&h);
        assert!(
            (before.width() - after.width()).abs() < 0.5 && (before.left() - after.left()).abs() < 0.5,
            "the dialog stops growing: {before:?} -> {after:?}"
        );
    }

    /// Path search is case-insensitive and Escape preserves review edits and user-provided names.
    #[test]
    fn import_album_picker_search_escape_and_unicode_names() {
        let Review { mut h, folder, album, .. } = review();
        let unicode = id(exec(&mut h, "album.create", json!({"name": "La fête 東京", "parent": folder, "addSelected": false})));
        click(&mut h, "albumPicker:import-album");
        click(&mut h, "importAlbum:new");
        replace_name(&mut h, "Keep this name");
        click(&mut h, "albumPicker:import-album");
        text(&mut h, "no such album");
        assert!(h.painted_text().iter().any(|t| t == "No albums match"));
        key(&mut h, "Enter");
        assert!(has(&h, "albumPickerSearch:import-album"), "Enter with no result keeps the picker open");
        key(&mut h, "Escape");
        assert!(!has(&h, "albumPickerSearch:import-album"));
        assert_eq!(destination(&h).new_album, "Keep this name", "Escape leaves the review's edits intact");
        click(&mut h, "albumPicker:import-album");
        text(&mut h, "FÊTE");
        assert!(has(&h, &format!("albumPickerItem:{unicode}:import-album")));
        assert!(!has(&h, &format!("albumPickerItem:{album}:import-album")));
        key(&mut h, "Enter");
        assert_eq!(destination(&h).album, Some(unicode));
        assert!(h.painted_text().iter().any(|t| t == "Travel / La fête 東京"), "user names are kept verbatim");
    }

    /// The catalog custom order wins over alphabetical order in the import picker.
    #[test]
    fn import_album_picker_follows_custom_sidebar_order() {
        let Review { mut h, other, .. } = review();
        let last_by_name = id(exec(&mut h, "album.create", json!({"name": "Zebra", "addSelected": false})));
        exec(&mut h, "album.reorder", json!({"id": last_by_name, "before": other}));
        click(&mut h, "albumPicker:import-album");
        let top = |id| h.app.widgets.iter().find(|(w, _)| w == &format!("albumPickerItem:{id}:import-album")).unwrap().1.top();
        assert!(top(last_by_name) < top(other), "sidebar order takes priority over alphabetical order");
    }

    /// Match painted text inside the specified control instead of anywhere in the frame.
    fn button_text(h: &Headless, id: &str, expected: &str) -> bool {
        let rect = h.app.widgets.iter().find(|(w, _)| w == id).unwrap().1;
        h.view.painted_shapes().iter().any(|shape| matches!(shape, egui::Shape::Text(t) if t.galley.job.text == expected && rect.contains(t.pos)))
    }

    /// Long paths and editable names remain usable across themes, viewport sizes and display densities.
    #[test]
    fn import_album_picker_fits_long_names_across_themes_sizes_and_densities() {
        for (size, density) in [([960.0, 760.0], 1.0), ([1366.0, 900.0], 2.0), ([960.0, 760.0], 2.25)] {
            let Review { mut h, folder, .. } = review_at(size, density);
            let long = "La fête des photographies – 東京 – ".repeat(12);
            let album = id(exec(&mut h, "album.create", json!({"name": long, "parent": folder, "addSelected": false})));
            let Some(crate::state::Dialog::Import { opts }) = &mut h.app.ui.dialog else { panic!("review") };
            opts.album = Some(album);
            for theme in ["charcoal", "midnight", "paper", "silver"] {
                exec(&mut h, "view.theme", json!({"theme": theme}));
                let r = h.request("ui.set", json!({"language": "de"}), T);
                assert_eq!(r["ok"], true, "{r}");
                assert!(h.settle(SETTLE));
                let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, h.size);
                let rect = |h: &Headless, name: &str| h.app.widgets.iter().find(|(id, _)| id == name).unwrap().1;
                assert!(screen.contains_rect(rect(&h, "albumPicker:import-album")), "{theme} {size:?}: selected long path");
                click(&mut h, "albumPicker:import-album");
                text(&mut h, "fête");
                let row = rect(&h, &format!("albumPickerItem:{album}:import-album"));
                assert!(screen.contains_rect(row), "{theme} {size:?}: search result {row:?} inside {screen:?}");
                key(&mut h, "Escape");
                click(&mut h, "albumPicker:import-album");
                click(&mut h, "importAlbum:new");
                let name = rect(&h, "field:importAlbumName");
                assert!(name.width() >= 160.0 && screen.contains_rect(name), "{theme} {size:?}: name {name:?}");
                click(&mut h, "albumPicker:import-album");
                text(&mut h, "fête");
                key(&mut h, "Enter");
                assert_eq!(destination(&h).album, Some(album));
            }
        }
    }

    /// Every shipped locale paints its own destination choices and names a new album in its language.
    #[test]
    fn import_album_picker_localizes_choices_in_every_shipped_language() {
        use crate::i18n::Locale;
        let Review { mut h, .. } = review();
        for locale in Locale::ALL {
            let r = h.request("ui.set", json!({"language": locale.code()}), T);
            assert_eq!(r["ok"], true, "{r}");
            assert!(h.settle(SETTLE));
            assert!(button_text(&h, "albumPicker:import-album", locale.tr("None")), "{locale:?}: localized empty destination");
            click(&mut h, "albumPicker:import-album");
            assert!(button_text(&h, "importAlbum:new", locale.tr("New album…")), "{locale:?}: localized new-album option");
            click(&mut h, "importAlbum:new");
            assert_eq!(destination(&h).new_album, locale.tr("Imported Photos"));
            assert!(button_text(&h, "albumPicker:import-album", locale.tr("New album")));
            click(&mut h, "albumPicker:import-album");
            click(&mut h, "importAlbum:none");
        }
    }

    /// An empty catalog still offers None and New without inventing an existing destination.
    #[test]
    fn import_album_picker_handles_an_empty_catalog() {
        let app = LightcraftApp::new(lightcraft_engine::Session::new(), Services { png: None, ..Default::default() });
        let mut h = Headless::new(app, [1000.0, 800.0], 1.0);
        h.app.ui.dialog = Some(crate::state::Dialog::Import { opts: Box::new(crate::import::ImportDialog::new(Vec::new())) });
        assert!(h.settle(SETTLE));
        click(&mut h, "albumPicker:import-album");
        assert!(h.painted_text().iter().any(|t| t == "No albums yet"));
        click(&mut h, "importAlbum:new");
        assert!(has(&h, "field:importAlbumName"));
        click(&mut h, "albumPicker:import-album");
        click(&mut h, "importAlbum:none");
        assert!(destination(&h).new_album.is_empty());
    }

    struct Fixture(std::path::PathBuf);

    impl Drop for Fixture {
        /// Remove only the uniquely owned fixture directory after the test session releases it.
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Write an original procedural PNG in a unique temporary directory for a real import.
    fn fixture() -> Fixture {
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("lc-import-tree-{}-{unique}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        let pixels =
            lightcraft_raster::Rgba8 { width: 40, height: 30, data: (0..1200).map(|i| [(i % 40 * 6) as u8, 31, (i / 40 * 8) as u8, 255]).collect() };
        let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&pixels), &lightcraft_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(path.join("hierarchy.png"), png).unwrap();
        Fixture(path)
    }

    /// A real file scan, the real picker, and the real import worker: the intended album receives
    /// the photo, duplicate names never redirect it, and the import is still one undo step.
    /// A destination deleted while the review is open falls back to no album before confirmation.
    #[test]
    fn import_album_picker_delivers_to_the_chosen_destination_and_undoes() {
        use lightcraft_catalog::PhotoId;
        for choice in ["existing", "new", "none", "deleted"] {
            let fixture = fixture();
            let Review { mut h, album, other, .. } = review();
            let before: std::collections::HashSet<PhotoId> = h.app.session.catalog.photos().map(|p| p.id).collect();
            h.app.ui.dialog = None;
            exec(&mut h, "file.addPhotos", json!({"paths": [fixture.0.to_string_lossy()], "album": album}));
            assert!(h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. }))));
            assert!(h.settle(SETTLE));
            assert_eq!(destination(&h).album, Some(album), "album-context import keeps its preselection");
            click(&mut h, "albumPicker:import-album");
            assert!(has(&h, &format!("albumPickerItem:{album}:import-album")), "preselection opens the right ancestors");
            match choice {
                "existing" => {
                    text(&mut h, "travel 2026 fav");
                    key(&mut h, "Enter");
                }
                "new" => {
                    click(&mut h, "importAlbum:new");
                    replace_name(&mut h, "From card");
                }
                "deleted" => {
                    key(&mut h, "Escape");
                    exec(&mut h, "album.delete", json!({"id": album}));
                    assert!(h.settle(SETTLE));
                    assert_eq!(destination(&h).album, None, "a deleted destination matches the displayed None choice");
                    assert!(button_text(&h, "albumPicker:import-album", "None"));
                }
                _ => click(&mut h, "importAlbum:none"),
            }
            let undo = h.app.session.undo.len();
            let r = h.request("ui.dialog.confirm", json!({}), T);
            assert_eq!(r["ok"], true, "{r}");
            assert!(h.step_until(SETTLE, |h| h.app.import.is_none()));
            assert!(h.settle(SETTLE));
            let added: Vec<PhotoId> = h.app.session.catalog.photos().filter(|p| !before.contains(&p.id)).map(|p| p.id).collect();
            assert_eq!(added.len(), 1, "{choice}: exactly the scanned photo is imported");
            let receives = |id| h.app.session.catalog.album(lightcraft_catalog::AlbumId(id)).unwrap().photos.contains(&added[0]);
            assert!(!receives(other), "same-named album outside the chosen folder is untouched");
            match choice {
                "existing" => assert!(receives(album)),
                "new" => {
                    let created = h.app.session.catalog.albums().find(|a| a.name == "From card").unwrap();
                    assert!(created.photos.contains(&added[0]));
                    assert!(!receives(album));
                }
                _ => assert!(h.app.session.catalog.albums().all(|a| !a.photos.contains(&added[0]))),
            }
            assert_eq!(h.app.session.undo.len(), undo + 1, "{choice}: one undo step");
            exec(&mut h, "edit.undo", json!({}));
            assert!(h.settle(SETTLE));
            assert_eq!(h.app.session.catalog.len(), before.len());
            if choice == "new" {
                assert!(h.app.session.catalog.albums().all(|a| a.name != "From card"), "undo removes the new album too");
            }
        }
    }
}
