//! The photo context menu: right-click a thumbnail in the grid or the filmstrip, the loupe or a
//! Compare / Survey photo. One menu everywhere, built from the menu bar's model
//! ([`crate::menubar::menu_bar`]), so its rows have the Photo menu's labels ("Delete 3 Photos"),
//! shortcuts, check marks and enabled state. The rows are grouped by what they do (see the
//! photo, describe it, organize it, develop it, its file, where it is, export, delete), with the
//! long tails in submenus (Edit Settings, Metadata, Rotate & Flip). Rows that only make sense
//! somewhere are left out elsewhere: Open in Detail in Detail, Compare and Survey with one photo,
//! the album rows outside an album, Locate Missing File… for a file that is there.
//!
//! This module builds the rows ([`nodes`]) and says why an off row is off ([`disabled_reason`]);
//! the right-click menus of the grid, the filmstrip, the loupe and Compare / Survey draw them in
//! the change that follows (parity row MENU-CTX-GRID).

use lightcraft_catalog::{Flag, PhotoId};
use lightcraft_engine::cmd::CommandInfo;
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::menubar::{self, MenuNode};
use crate::menus::MenuEntry;
use crate::state::ViewMode;

/// The first item for `id` in `nodes`, submenus included.
fn find_item<'a>(nodes: &'a [MenuNode], id: &str) -> Option<&'a MenuNode> {
    nodes.iter().find_map(|n| match n {
        MenuNode::Item { id: i, .. } if i == id => Some(n),
        MenuNode::Submenu { children, .. } => find_item(children, id),
        _ => None,
    })
}

/// Any registered command as a row, with or without a place in the menu bar.
fn registered(app: &LightcraftApp, infos: &[CommandInfo], id: &str) -> Option<MenuNode> {
    let keymap = &app.ui.settings.keymap;
    let e = match crate::menus::ui_commands().find(|c| c.0 == id) {
        Some((cid, label, sc, _)) => MenuEntry {
            id: cid.to_string(),
            label: label.to_string(),
            menu: Vec::new(),
            shortcut: crate::shortcuts::binding(keymap, cid, *sc).map(str::to_string),
            enabled: crate::menus::ui_enabled(app, cid),
        },
        None => {
            let c = infos.iter().find(|c| c.id == id)?;
            MenuEntry {
                id: c.id.into(),
                label: c.label.into(),
                menu: Vec::new(),
                shortcut: crate::shortcuts::shortcut_of(keymap, c.id).map(str::to_string),
                enabled: c.enabled,
            }
        }
    };
    Some(menubar::node(app, &e))
}

/// `n` with its label, parameters, enabled or checked state changed.
fn with(n: Option<MenuNode>, f: impl FnOnce(&mut String, &mut Value, &mut bool, &mut Option<bool>)) -> Option<MenuNode> {
    let mut n = n?;
    if let MenuNode::Item { label, params, enabled, checked, .. } = &mut n {
        f(label, params, enabled, checked);
    }
    Some(n)
}

/// The rating, flag and label rows checked by `photo`'s values (the menu bar checks the active
/// photo's, which a right-click on one of several selected photos may leave another). In Compare
/// and Survey a click there rates the active photo only (`compare::target_active`), so these rows
/// name `photo`: the one checked is the one changed.
fn recheck(app: &LightcraftApp, photo: PhotoId, mut sub: MenuNode) -> MenuNode {
    let culling = crate::panels::compare::culling(app);
    let Some(p) = app.session.catalog.photo(photo) else { return sub };
    if let MenuNode::Submenu { children, .. } = &mut sub {
        for c in children {
            let MenuNode::Item { id, params, checked, .. } = c else { continue };
            let on = match id.as_str() {
                "photo.rate" => params["rating"].as_u64().map(|r| r == u64::from(p.rating)),
                "photo.flag" => params["flag"].as_str().map(|f| match p.flag {
                    Flag::Pick => f == "pick",
                    Flag::Reject => f == "reject",
                    Flag::None => f == "none",
                }),
                "photo.label" => params["label"].as_str().map(|l| match p.label {
                    Some(c) => format!("{c:?}").to_lowercase() == l,
                    None => l == "none",
                }),
                _ => None,
            };
            if on.is_some() {
                *checked = on;
                if culling {
                    params["ids"] = json!([photo.0]);
                }
            }
        }
    }
    sub
}

/// Edit Settings ▸ Apply Preset: one submenu per preset group, in the Presets panel's order
/// (Favorites first, then the groups by name). Labels are in the UI language already (a user's
/// group and preset names as written): whatever draws them must not translate them again
/// ([`menubar::display_item_label`] leaves `preset.apply` rows alone).
fn presets(app: &LightcraftApp, any: bool) -> MenuNode {
    let list = &app.session.presets;
    let row = |p: &lightcraft_develop::Preset| {
        menubar::item("preset.apply", json!({"id": p.id}), crate::i18n::builtin_label(&p.name, p.builtin), None, any, None)
    };
    let mut by_name: std::collections::BTreeMap<&str, (bool, Vec<MenuNode>)> = std::collections::BTreeMap::new();
    for p in list.iter() {
        let g = by_name.entry(&p.group).or_default();
        g.0 |= p.builtin;
        g.1.push(row(p));
    }
    let starred: Vec<MenuNode> = list.iter().filter(|p| p.favorite).map(row).collect();
    let favorites = (!starred.is_empty()).then(|| MenuNode::Submenu { label: crate::i18n::tr("Favorites").into(), children: starred });
    let groups = by_name
        .into_iter()
        .map(|(name, (builtin, children))| MenuNode::Submenu { label: crate::i18n::builtin_label(name, builtin).into(), children });
    MenuNode::Submenu { label: "Apply Preset".into(), children: favorites.into_iter().chain(groups).collect() }
}

/// The menu for `photo` (the right-click selects it unless it is one of the selected photos).
pub fn nodes(app: &LightcraftApp, photo: PhotoId) -> Vec<MenuNode> {
    let any = !app.session.selected().is_empty();
    nodes_with(app, photo, &app.session.commands(), presets(app, any))
}

/// [`nodes`], with `infos` ([`lightcraft_engine::Session::commands`]) and Apply Preset at hand.
fn nodes_with(app: &LightcraftApp, photo: PhotoId, infos: &[CommandInfo], presets: MenuNode) -> Vec<MenuNode> {
    let bar = menubar::menu_bar(app);
    let menu = |title: &str| bar.iter().find(|(t, _)| t == title).map(|(_, n)| n.as_slice()).unwrap_or_default();
    // a menu's own row before one of the same command inside a submenu (Photo ▸ Add to Album holds
    // a hint row for Add to My Photos)
    let top = |id: &str| bar.iter().find_map(|(_, n)| n.iter().find(|x| matches!(x, MenuNode::Item { id: i, .. } if i == id))).cloned();
    let row = |id: &str| top(id).or_else(|| bar.iter().find_map(|(_, n)| find_item(n, id)).cloned()).or_else(|| registered(app, infos, id));
    let sub = |title: &str, label: &str| menu(title).iter().find(|n| matches!(n, MenuNode::Submenu { label: l, .. } if l == label)).cloned();
    let submenu =
        |label: &str, children: Vec<Option<MenuNode>>| MenuNode::Submenu { label: label.into(), children: children.into_iter().flatten().collect() };
    let selected = app.session.selected();
    let several = selected.len() > 1;
    let view = app.ui.view;
    let sep = || Some(MenuNode::Separator);
    let open = |id: &str, label: &'static str| {
        with(row(id), |l, _, _, c| {
            *l = label.into();
            *c = None;
        })
    };
    let mut v: Vec<Option<MenuNode>> = Vec::new();

    // see it
    if view != ViewMode::Detail {
        v.push(open("view.detail", "Open in Detail"));
    }
    if several && view != ViewMode::Compare {
        v.push(open("view.compare", "Open in Compare"));
    }
    if several && view != ViewMode::Survey {
        v.push(open("view.survey", "Open in Survey"));
    }
    v.push(sub("Photo", "Edit In"));
    v.push(sep());

    // describe it
    for s in ["Set Rating", "Set Flag", "Set Color Label"] {
        v.push(sub("Photo", s).map(|s| recheck(app, photo, s)));
    }
    v.push(row("panel.addKeywords"));
    let copied_meta = app.session.meta_clipboard.is_some();
    v.push(Some(submenu(
        "Metadata",
        vec![
            row("photo.copyMetadata"),
            with(row("photo.pasteMetadata"), |_, _, e, _| *e &= copied_meta),
            sub("Photo", "Metadata Preset"),
            sep(),
            row("dialog.captureTime"),
            row("photo.tagFromTracklog"),
            row("metadata.reverseGeocode"),
            sep(),
            row("photo.saveMetadataToFile"),
            row("photo.readMetadataFromFile"),
            sep(),
            row("dialog.allMetadata"),
            row("faces.detect"),
        ],
    )));
    v.push(sep());

    // organize it
    // (offered when the right-clicked photo is Local: a folder being browsed shows Local photos
    // alone, so a mixed selection is rare, and Photo ▸ Add to Album says the rest)
    if app.session.catalog.photo(photo).is_some_and(|p| p.local) {
        v.push(row("photo.addToLibrary"));
    }
    v.push(row("album.toggleTarget"));
    if crate::panels::grid::local_only(app, &selected) {
        // albums hold library photos (issue #684): the row is off and says to add them first
        v.push(Some(menubar::item("album.addPhotos", Value::Null, crate::i18n::tr("Add to Album"), None, false, None)));
    } else {
        v.push(sub("Photo", "Add to Album"));
    }
    if let lightcraft_engine::LibrarySource::Album(aid) = app.session.source
        && menubar::in_normal_album(app)
        && !menubar::selection_deleted(app)
    {
        v.push(row("album.removePhotos"));
        v.push(with(row("album.setCover"), |_, p, _, _| *p = json!({"id": aid.0, "photo": photo.0})));
    }
    v.push(sub("Photo", "Stack"));
    v.push(with(row("library.findSimilar"), |_, p, _, _| *p = json!({"id": photo.0})));
    v.push(sep());

    // develop it
    let copied = app.session.clipboard.is_some();
    v.push(Some(submenu(
        "Edit Settings",
        vec![
            row("develop.copy"),
            row("dialog.copySettings"),
            with(row("develop.paste"), |_, _, e, _| *e &= copied),
            with(row("dialog.pasteSettings"), |_, _, e, _| *e &= copied),
            row("develop.pastePrevious"),
            row("develop.sync"),
            row("develop.matchExposure"),
            sep(),
            row("develop.auto"),
            row("develop.treatment"),
            Some(presets),
            row("dialog.createPreset"),
            sep(),
            with(row("photo.setReference"), |_, p, e, _| {
                *p = json!({"id": photo.0});
                *e &= crate::panels::compare::live(app, photo);
            }),
            row("develop.reset"),
        ],
    )));
    v.push(row("photo.virtualCopy"));
    v.push(row("version.create"));
    v.push(sub("Photo", "Photo Merge"));
    v.push(Some(submenu(
        "Rotate & Flip",
        vec![row("photo.rotateLeft"), row("photo.rotateRight"), sep(), row("photo.flipHorizontal"), row("photo.flipVertical")],
    )));
    v.push(sep());

    // its file
    v.push(row("dialog.rename"));
    if app.services.pick_folder.is_some() || app.services.picker.is_some() {
        v.push(with(row("photo.moveToFolder"), |l, _, _, _| *l = "Move to Folder…".into()));
    }
    v.push(row("photo.duplicate"));
    v.push(row("photo.convertToDng"));
    v.push(row("photo.reload"));
    v.push(sep());

    // where it is: the original moved or its drive is gone (a cached answer, checked off the UI thread)
    let missing = matches!(app.session.catalog.photo(photo).map(|p| &p.source), Some(lightcraft_catalog::Source::File { path }) if app.session.media.availability.is_offline(path));
    if missing {
        v.push(row("photo.locate"));
    }
    v.push(row("app.showInFinder"));
    v.push(with(row("app.showInFolders"), |_, p, e, _| {
        *p = json!({"id": photo.0});
        *e = crate::menus::show_in_folders_enabled(app, Some(photo));
    }));
    v.push(row("photo.copyPath"));
    v.push(sep());

    // export it
    v.push(row("dialog.export"));
    let mut export = vec![row("app.exportPrevious"), sep()];
    if let Some(MenuNode::Submenu { children, .. }) = sub("File", "Export with Preset") {
        // (its Custom… is the Export… row above)
        export.extend(children.into_iter().filter(|n| !matches!(n, MenuNode::Item { id, .. } if id == "dialog.export")).map(Some));
    }
    v.push(Some(submenu("Export with Preset", export)));
    v.push(sep());

    // delete it (Restore and Delete Permanently in Recently Deleted: the menu bar's choice)
    for id in ["photo.delete", "photo.restore", "photo.deletePermanently"] {
        v.push(menu("Photo").iter().find(|n| matches!(n, MenuNode::Item { id: i, .. } if i == id)).cloned());
    }
    menubar::tidy(v.into_iter().flatten().collect())
}

/// Why a row that is off is off; `infos` is [`lightcraft_engine::Session::commands`]. Asked only
/// about a row that is off: for some ids the answer doesn't depend on the row being off.
fn reason(app: &LightcraftApp, infos: &[CommandInfo], id: &str, params: &Value) -> Option<String> {
    let why = match id {
        _ if (id.starts_with("dialog.merge") || id.starts_with("merge.")) && app.session.selected().len() < 2 => "Select 2 or more photos",
        "develop.paste" | "dialog.pasteSettings" if app.session.clipboard.is_none() => "Copy edit settings first",
        "photo.pasteMetadata" if app.session.meta_clipboard.is_none() => "Copy metadata first",
        "album.addPhotos" if params.is_null() => return Some(crate::panels::grid::albums_hint()),
        "photo.copyPath" => "The selected photos have no file on disk",
        "app.showInFinder" if app.services.reveal.is_none() => "This version of LightCraft can't open the file manager",
        "app.showInFinder" => "This photo has no file on disk",
        "app.showInFolders" => "This photo is not in a folder of the library",
        "photo.editInPhotoCraft" | "photo.editInExternal" | "photo.openOriginalInExternal" if app.services.open_with.is_none() => {
            "Opening other programs needs the desktop app"
        }
        "photo.openOriginalInExternal" => "Only a photo file LightCraft reads can be opened this way",
        "photo.setReference" => "Restore the photo to use it as the reference",
        _ => return infos.iter().find(|c| c.id == id).and_then(|c| c.disabled_reason.clone()),
    };
    Some(crate::i18n::tr(why).to_string())
}

/// Why a row is off (the text shown while the pointer rests on a row that is off). Asked only
/// about a row that is off: for some ids (Copy File Path, Show in Folders, Set as Reference) the
/// answer is the same whether or not it is.
pub fn disabled_reason(app: &LightcraftApp, id: &str, params: &Value) -> Option<String> {
    reason(app, &app.session.commands(), id, params)
}

#[cfg(test)]
#[path = "photo_menu_tests.rs"]
mod tests;
