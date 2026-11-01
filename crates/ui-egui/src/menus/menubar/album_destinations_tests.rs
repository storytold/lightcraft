use super::*;

fn create(app: &mut LightcraftApp, name: &str, parent: Option<u64>, folder: bool) -> u64 {
    app.session.execute("album.create", &json!({"name": name, "parent": parent, "folder": folder, "addSelected": false})).unwrap()["id"]
        .as_u64()
        .unwrap()
}

fn children<'a>(nodes: &'a [MenuNode], name: &str) -> &'a [MenuNode] {
    nodes
        .iter()
        .find_map(|n| match n {
            MenuNode::Submenu { label, children, .. } if label == name => Some(children.as_slice()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing folder {name}: {nodes:?}"))
}

/// #3525: destinations follow folder structure and sidebar order; duplicate leaf names keep
/// their own IDs, smart/empty folders are omitted, and adding goes to the chosen album.
#[test]
fn add_to_album_destinations_preserve_folders_order_and_identity() {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
    let year = create(&mut app, "2026", None, true);
    let theme = create(&mut app, "Travel", Some(year), true);
    let first = create(&mut app, "Zebra", Some(theme), false);
    let second = create(&mut app, "Alpha", Some(theme), false);
    app.session.execute("album.reorder", &json!({"id": first, "before": second})).unwrap();
    let earlier = create(&mut app, "2025", None, true);
    let other = create(&mut app, "Alpha", Some(earlier), false);
    let empty = create(&mut app, "Empty", Some(year), true);
    app.session.execute("album.createSmart", &json!({"name": "Smart only", "parent": empty, "rules": {"rating": 1}})).unwrap();
    let nodes = expanded(&app, "Add to Album").unwrap();
    let destinations = children(children(&nodes, "2026"), "Travel");
    let ids: Vec<_> = destinations
        .iter()
        .filter_map(|n| match n {
            MenuNode::Item { id, params, .. } if id == "album.addPhotos" => params["id"].as_u64(),
            _ => None,
        })
        .collect();
    assert_eq!(ids, [first, second], "manual order beats alphabetical order");
    assert!(matches!(&children(&nodes, "2025")[0], MenuNode::Item { params, .. } if params["id"] == other));
    assert!(!format!("{nodes:?}").contains("Smart only"));
    assert!(!format!("{nodes:?}").contains("Empty"));
    assert!(matches!(nodes.last(), Some(MenuNode::Item { id, .. }) if id == "dialog.newAlbum"));
    let photo = app.session.catalog.photos().next().unwrap().id;
    app.session.execute("library.select", &json!({"ids": [photo.0]})).unwrap();
    run_item(&mut app, "album.addPhotos", json!({"id": second})).unwrap();
    assert!(app.session.catalog.album_photos(lightcraft_catalog::AlbumId(second)).contains(&photo));
    assert!(!app.session.catalog.album_photos(lightcraft_catalog::AlbumId(other)).contains(&photo));
}

/// Folder names can equal translation keys; names and stable folder identities are kept by
/// both the in-window/native presentation helpers and the serialized control-channel model.
#[test]
fn add_to_album_destinations_keep_user_names_and_native_cache_identity() {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Default::default());
    let folder = create(&mut app, "File", None, true);
    create(&mut app, "Photo Grid", Some(folder), false);
    let bar = menu_bar(&app);
    let nodes = expanded(&app, "Add to Album").unwrap();
    let MenuNode::Submenu { id, label, children } = &nodes[0] else { panic!("folder") };
    assert_eq!(id.as_deref(), Some(format!("albumFolder:{folder}").as_str()));
    crate::i18n::set_language(crate::i18n::Locale::De);
    assert_eq!(display_submenu_label(id.as_deref(), label), "File");
    assert_eq!(display_submenu_label(None, "File"), crate::i18n::tr("File"));
    let MenuNode::Item { id: command, params, label, enabled, .. } = &children[0] else { panic!("album") };
    assert_eq!(display_item_label(command, params, label), "Photo Grid");
    assert!(!enabled, "no photo selected: nested destinations are off");
    assert_eq!(serde_json::to_value(&nodes).unwrap()[0]["id"], format!("albumFolder:{folder}"));
    let before = native_menu_structure(&bar, &app.ui.settings.keymap);
    app.session.execute("album.rename", &json!({"id": folder, "name": "Renamed"})).unwrap();
    assert_ne!(before, native_menu_structure(&menu_bar(&app), &app.ui.settings.keymap));
    crate::i18n::set_language(crate::i18n::Locale::En);
}

/// Navigate actual egui submenus on both surfaces. Same-named folders and albums in different
/// branches must select the right destination and add the complete photo selection.
#[test]
fn add_to_album_destinations_both_menus_add_selection_to_the_chosen_branch() {
    use crate::headless::Headless;
    use std::time::Duration;
    let timeout = Duration::from_secs(30);
    for context in [false, true] {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default());
        app.ui.view = ViewMode::PhotoGrid;
        let year = create(&mut app, "2026", None, true);
        let trip = create(&mut app, "Trip", Some(year), true);
        let target = create(&mut app, "Favorites", Some(trip), false);
        let old = create(&mut app, "2025", None, true);
        let old_trip = create(&mut app, "Trip", Some(old), true);
        let other = create(&mut app, "Favorites", Some(old_trip), false);
        let photos: Vec<_> = app.session.visible_cloned().into_iter().take(2).collect();
        app.session.execute("library.select", &json!({"ids": photos.iter().map(|p| p.0).collect::<Vec<_>>()})).unwrap();
        let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
        h.settle(Duration::from_secs(120));
        let mut request = |command: &str, params: Value| {
            let r = h.request(command, params, timeout);
            assert_eq!(r["ok"], true, "{context}: {command}: {r}");
            h.step();
            h.step();
        };
        if context {
            request("ui.clickWidget", json!({"id": format!("thumb:{}", photos[0].0), "button": "right"}));
            request("ui.hoverWidget", json!({"id": "button:contextAddToAlbum"}));
        } else {
            request("ui.clickWidget", json!({"id": "menu:Photo"}));
            request("ui.hoverWidget", json!({"id": "menusub:Add to Album"}));
        }
        for folder in [year, trip] {
            request("ui.hoverWidget", json!({"id": format!("menusub:albumFolder:{folder}")}));
        }
        request("ui.clickWidget", json!({"id": format!("button:contextAlbum:{target}")}));
        assert_eq!(h.app.session.catalog.album_photos(lightcraft_catalog::AlbumId(target)), photos, "{context}");
        assert!(h.app.session.catalog.album_photos(lightcraft_catalog::AlbumId(other)).is_empty());
    }
}
