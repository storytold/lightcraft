//! The menu bar model: File, Edit, View, Photo, Window, Help, generated from the command registry
//! (engine commands with menu paths + [`crate::menus::ui_commands`]) with live labels, shortcuts,
//! enabled and checked state.
//!
//! One model drives every menu surface: the native macOS menu bar (built by the desktop host), the
//! in-window menu bar (web, Windows, Linux — [`show_in_window`]) and the control channel
//! (`ui.menu.tree`). Items are a command id plus parameters, run through [`run_item`] so they
//! behave exactly like their keyboard shortcuts.
#![allow(clippy::indexing_slicing, reason = "not yet converted to checked indexing: tracked in docs/gaps.md (indexing lint)")]

use serde::Serialize;
use serde_json::{Value, json};

use crate::LccApp;
use crate::menus::MenuEntry;
use crate::state::{RightPanel, ViewMode};

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MenuNode {
    Item {
        id: String,
        #[serde(skip_serializing_if = "Value::is_null")]
        params: Value,
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        shortcut: Option<String>,
        enabled: bool,
        /// `Some` for toggles and radio-style choices.
        #[serde(skip_serializing_if = "Option::is_none")]
        checked: Option<bool>,
    },
    Separator,
    Submenu {
        label: String,
        children: Vec<MenuNode>,
    },
}

impl MenuNode {
    /// `id|params` — a stable key for one item (native menu ids).
    pub fn key(id: &str, params: &Value) -> String {
        if params.is_null() { id.to_string() } else { format!("{id}|{params}") }
    }
}

/// User-defined names in parameterized menus are data, not message keys.
pub fn display_item_label<'a>(id: &str, params: &Value, label: &'a str) -> &'a str {
    if matches!(
        id,
        "metadata.applyPreset"
            | "album.addPhotos"
            | "label.applySet"
            | "catalog.openRecent"
            | "plugins.runItem"
            | "filter.applyPreset"
            | "keyword.toggleFromSet"
            | "keyword.useSet"
            | "map.style"
    ) || (id == "map.filter" && params.get("filter").and_then(Value::as_str).is_some_and(|f| f.starts_with("location:")))
        || (id == "app.export" && params.get("preset").is_some())
        || (id == "app.openLibrary" && params.get("path").is_some())
        || (id == "app.recentLibraries" && params.get("remove").is_some())
    {
        label
    } else {
        crate::i18n::tr(label)
    }
}

/// Whether AppKit offers a shortcut to its menu instead of the window. Plain letters stay
/// with egui even when the native menu displays them as key equivalents.
pub fn native_menu_delivers(sc: &str) -> bool {
    sc.split('+').any(|part| matches!(part, "Cmd" | "Ctrl") || part.strip_prefix('F').is_some_and(|n| n.parse::<u8>().is_ok()))
}

/// Native label and accelerator for a menu row. A grid row may borrow the cycle key as a
/// hint, but must not consume a modified key and turn cycling into direct grid selection.
/// AppKit has no separate shortcut-hint column: borrowed native-owned keys are appended to
/// the translated label instead, leaving their dispatch with egui. Ordinary G stays in the
/// native key-equivalent column because AppKit never hands it to the menu.
pub fn native_item_presentation<'a>(
    keymap: &crate::shortcuts::Keymap,
    id: &str,
    params: &Value,
    label: &str,
    shortcut: Option<&'a str>,
) -> (String, Option<&'a str>) {
    let label = display_item_label(id, params, label);
    let borrowed = matches!(id, "view.photoGrid" | "view.squareGrid") && shortcut != crate::shortcuts::shortcut_of(keymap, id);
    match shortcut.filter(|sc| borrowed && native_menu_delivers(sc)) {
        Some(sc) => (format!("{label} ({})", shortcut_text(sc, true)), None),
        None => (label.to_string(), shortcut),
    }
}

/// Cache key for native menu structure and effective accelerator ownership. A transfer from
/// a direct grid binding to a borrowed cycle hint must rebuild native handles and republish
/// their owned shortcuts even when the visible menu model is identical.
pub fn native_menu_structure(bar: &[(String, Vec<MenuNode>)], keymap: &crate::shortcuts::Keymap) -> String {
    /// Add stable identities and effective native keys; live checked values stay outside the cache key.
    fn walk(n: &[MenuNode], out: &mut String, keymap: &crate::shortcuts::Keymap) {
        for x in n {
            match x {
                MenuNode::Item { id, params, label, checked, shortcut, .. } => {
                    out.push_str(&MenuNode::key(id, params));
                    out.push(if checked.is_some() { 'c' } else { 'i' });
                    out.push_str(shortcut.as_deref().unwrap_or(""));
                    // A key can change owners while both visible hints stay identical.
                    out.push('|');
                    out.push_str(native_item_presentation(keymap, id, params, label, shortcut.as_deref()).1.unwrap_or(""));
                }
                MenuNode::Separator => out.push('-'),
                MenuNode::Submenu { label, children } => {
                    out.push('[');
                    out.push_str(label);
                    walk(children, out, keymap);
                    out.push(']');
                }
            }
            out.push(';');
        }
    }
    let mut s = format!("{};", crate::i18n::language().code());
    for (t, items) in bar {
        s.push_str(t);
        walk(items, &mut s, keymap);
    }
    s
}

/// The Library module's menus in order (the macOS app menu is added by the host); [`menus`] gives
/// the module on screen's.
pub const MENUS: &[&str] = &["File", "Edit", "Library", "Photo", "Metadata", "View", "Window", "Help"];

/// Order inside submenus whose items come from the registry: (menu, submenu, ids and `---`).
/// Items not listed follow in registry order, dialogs (labels ending in `…`) last.
const SUB_LAYOUT: &[(&str, &str, &[&str])] = &[
    (
        "Photo",
        "Develop Settings",
        &[
            "develop.copy",
            "dialog.copySettings",
            "develop.paste",
            "dialog.pasteSettings",
            "develop.pastePrevious",
            "dialog.syncSettings",
            "develop.autoSync",
            "---",
            "develop.treatment",
            "develop.matchExposure",
            "develop.updateProcess",
            "masks.updateAi",
            "---",
            "version.create",
            "dialog.createPreset",
        ],
    ),
    (
        "Photo",
        "File",
        &[
            "app.showInFinder",
            "app.showInFolders",
            "photo.reload",
            "photo.addToLibrary",
            "---",
            "photo.duplicate",
            "photo.convertToDng",
            "photo.renderToDng",
            "---",
            "photo.deleteFromDisk",
        ],
    ),
    ("Photo", "Rotate & Flip", &["photo.rotateLeft", "photo.rotateRight", "---", "photo.flipHorizontal", "photo.flipVertical"]),
    (
        "File",
        "Libraries",
        &[
            "app.newLibrary",
            "app.openLibrary",
            "---",
            "dialog.exportCatalog",
            "---",
            "catalog.backup",
            "file.backupLibrary",
            "file.restoreLibrary",
            "catalog.checkIntegrity",
            "library.compact",
        ],
    ),
    (
        "File",
        "More Imports",
        &[
            "dialog.importCatalog",
            crate::legacy::IMPORT_LIBRARY_ID,
            "file.importLightroom",
            "file.importImmich",
            "---",
            "file.importPresets",
            "file.importKeywords",
        ],
    ),
    ("Library", "Filters", &["view.filterBar", "library.toggleFilters", "library.clearFilter", "---", "library.refresh", "library.holdView"]),
    (
        "View",
        "Before/After",
        &[
            "view.showOriginal",
            "view.beforeAfter",
            "view.beforeAfterSplit",
            "view.beforeAfterTopBottom",
            "view.beforeAfterSplitTopBottom",
            "view.reference",
            "---",
        ],
    ),
    ("View", "Zoom", &["view.zoomIn", "view.zoomOut", "---", "view.zoomToggle", "view.zoomFit", "view.zoom100"]),
    (
        "View",
        "Loupe Info",
        &[
            "view.infoOverlay",
            "view.shotInfo",
            "view.focusPoint",
            "view.zoomToFocus",
            "view.faceBoxes",
            "view.eyeFocus",
            "view.visualizeHdr",
            "view.hdrPreview",
            "---",
            "view.colorReadoutRgb",
            "view.colorReadoutHsl",
            "view.colorReadoutLab",
        ],
    ),
    (
        "View",
        "Tool Overlay",
        &[
            "view.maskOverlay",
            "view.maskOverlayAll",
            "view.maskOverlayMode",
            "view.maskOverlayColor",
            "view.maskPins",
            "view.maskUnselectedPins",
            "view.maskOverlayAuto",
            "---",
            "view.visualizeSpots",
            "view.cropOverlay",
            "view.cropOverlayOrientation",
        ],
    ),
    (
        "View",
        "Grid View Style",
        &[
            "view.squareGrid",
            "---",
            "view.cellCompact",
            "view.cellExpanded",
            "view.cellIndex",
            "view.cellBadges",
            "---",
            "view.thumbSizeUp",
            "view.thumbSizeDown",
            "view.thumbLarger",
            "view.thumbSmaller",
        ],
    ),
    ("Window", "Edit Panels", &["panel.edit", "panel.profiles", "panel.crop", "panel.remove", "panel.masking", "panel.redeye"]),
    (
        "Window",
        "Panels",
        &[
            "view.leftPanel",
            "view.photoCounts",
            "view.filmstrip",
            "panel.toolbar",
            "view.navigator",
            "view.histogram",
            "view.histogram.mode",
            "view.histogram.scale",
            "view.histogram.second",
            "---",
        ],
    ),
    ("Window", "Screen Mode", &["view.fullScreenPreview", "view.enterFullScreen", "---"]),
];

/// Items that always end their menu in their own separator group, after the entries appended
/// because the layout doesn't list them (Quit is the last item of the in-window File menu).
const LAST: &[&str] = &["app.quit"];

/// Registry entries that are reached another way (parameterized commands are expanded into
/// submenus below; others duplicate a dialog command).
const HIDDEN: &[&str] = &[
    "photo.rate",
    "photo.flag",
    "photo.pick",
    "photo.reject",
    "photo.unflag",
    "photo.label",
    "photo.clearLabel",
    "library.sort",
    "library.shuffle",
    "album.addPhotos",
    "album.create",
    "library.import",
    "preset.create",
    // the Loupe Overlay submenu lists them with its opacity and matte choices
    "view.chooseLayoutImage",
    "view.layoutImage",
    // the same dialog as File ▸ Import Photos and Video… (it was listed twice)
    "file.addPhotos",
];

/// The selection includes a photo in Recently Deleted.
pub(crate) fn selection_deleted(app: &LccApp) -> bool {
    let s = &app.session;
    s.selection.ids.iter().copied().chain(s.selection.active).any(|id| s.catalog.photo(id).is_some_and(|p| p.deleted))
}

/// Items only some hosts have are left out of the others' menus (the web build's library backup);
/// Restore and Delete Permanently replace Delete for photos in Recently Deleted.
fn host_supports(app: &LccApp, id: &str) -> bool {
    match id {
        "file.backupLibrary" => app.services.backup_library.is_some(),
        // the desktop's backup into the library's backup folder; a host with a backup of its own
        // (the web build's download) shows that one instead
        "catalog.backup" => app.services.backup_library.is_none(),
        "file.restoreLibrary" => app.services.restore_library.is_some(),
        "photo.restore" | "photo.deletePermanently" => selection_deleted(app),
        "photo.delete" => !selection_deleted(app),
        // only in an album that holds its photos (not a smart one)
        "album.removePhotos" => in_normal_album(app) && !selection_deleted(app),
        // only where the trash is on screen
        "library.emptyRecentlyDeleted" => app.session.source == lcc_engine::LibrarySource::RecentlyDeleted,
        // what only one view or kind of photo can use shows only there, so the menus stay short
        "compare.swap" | "compare.makeSelect" => app.ui.view == ViewMode::Compare,
        _ if id.starts_with("video.") => lcc_engine::cmd::video::active_video(&app.session).is_ok(),
        _ => true,
    }
}

/// The library shows an album that holds its photos (not a smart album, not a folder of albums).
pub(crate) fn in_normal_album(app: &LccApp) -> bool {
    match app.session.source {
        lcc_engine::LibrarySource::Album(a) => app.session.catalog.album(a).is_some_and(|a| !a.folder && !a.is_smart()),
        _ => false,
    }
}

pub(crate) fn item(id: &str, params: Value, label: impl Into<String>, shortcut: Option<&str>, enabled: bool, checked: Option<bool>) -> MenuNode {
    MenuNode::Item { id: id.into(), params, label: label.into(), shortcut: shortcut.map(str::to_string), enabled, checked }
}

/// Checked state of toggles and radio items.
pub fn checked(app: &LccApp, id: &str) -> Option<bool> {
    let u = &app.ui;
    let panel = |p: RightPanel| Some(u.right == p);
    match id {
        // Every language's command is checked when it is the active one.
        _ if crate::menus::language_from_command(id).is_some() => Some(crate::menus::language_from_command(id) == Some(u.language)),
        "develop.autoSync" => Some(app.session.auto_sync),
        "webui.guides" => Some(u.web.show_guides),
        "view.photoCounts" => Some(u.show_counts),
        "view.cellCompact" => Some(u.lib.cell_style == crate::libtools::CellStyle::Compact),
        "view.cellExpanded" => Some(u.lib.cell_style == crate::libtools::CellStyle::Expanded),
        "view.cellIndex" => Some(u.lib.cell_index),
        "view.cellBadges" => Some(u.lib.cell_badges),
        "view.secondWindow" => Some(u.second_window),
        "view.photoGrid" => Some(u.view == ViewMode::PhotoGrid),
        "view.squareGrid" => Some(u.view == ViewMode::SquareGrid),
        "view.detail" => Some(u.view == ViewMode::Detail),
        "view.compare" => Some(u.view == ViewMode::Compare),
        "view.survey" => Some(u.view == ViewMode::Survey),
        "view.people" => Some(u.view == ViewMode::People),
        "view.reference" => Some(u.view == ViewMode::Reference),
        "view.leftPanel" => Some(u.left_panel),
        "view.faceBoxes" => Some(u.face_boxes),
        crate::face_menu::DRAW_REGIONS => Some(u.draw_faces),
        "view.hdrPreview" => Some(u.hdr_preview),
        "view.focusPoint" => Some(u.settings.focus_point),
        "metadata.targetOnly" => Some(u.settings.metadata_target_only),
        "view.zoomToFocus" => Some(u.settings.zoom_to_focus),
        "view.filmstrip" => Some(u.filmstrip),
        "view.shotInfo" => Some(u.shot_info),
        "view.lockZoomPosition" => Some(u.settings.lock_zoom),
        "view.scrollByRow" => Some(u.settings.scroll_by_row),
        "view.mirrorImage" => Some(u.settings.mirror_image),
        "view.frameRate" => Some(u.frame_rate),
        "view.histogram" => Some(if crate::modern::on(app) { crate::modern::histogram_shown(app) } else { u.histogram }),
        "view.clipping" => Some(u.show_clipping),
        "view.visualizeHdr" => Some(u.hdr_visualize),
        "view.colorReadoutRgb" => Some(u.settings.color_readout == crate::color_readout::ColorReadout::Rgb),
        "view.colorReadoutHsl" => Some(u.settings.color_readout == crate::color_readout::ColorReadout::Hsl),
        "view.colorReadoutLab" => Some(u.settings.color_readout == crate::color_readout::ColorReadout::Lab),
        "view.softProof" => Some(u.soft_proof),
        "remove.useGenerative" => Some(crate::panels::genfill::generative_on(app)),
        "remove.detectObjects" => Some(crate::panels::genfill::detect_objects_on(app)),
        "view.maskOverlay" => Some(u.mask_overlay),
        "view.maskOverlayAll" => Some(u.mask_overlay_all),
        "view.maskPins" => Some(u.mask_pins),
        "view.maskUnselectedPins" => Some(u.mask_unselected_pins),
        "view.maskOverlayAuto" => Some(u.mask_overlay_auto),
        "view.showOriginal" => Some(u.before_after == crate::state::BeforeAfter::Original),
        "view.beforeAfter" => Some(u.before_after == crate::state::BeforeAfter::SideBySide),
        "view.beforeAfterSplit" => Some(u.before_after == crate::state::BeforeAfter::Split),
        "view.autoAdvance" => Some(u.auto_advance),
        "view.filterBar" => Some(u.filter_bar),
        "library.toggleFilters" => Some(!app.session.filters_off()),
        "view.soloSections" => Some(u.single_panel),
        "view.navigator" => Some(u.navigator),
        "view.layoutImage" => Some(u.layout_image.show && !u.layout_image.path.trim().is_empty()),
        "library.holdView" => Some(app.session.hold_view),
        "app.autoImportToggle" => Some(app.session.import_defaults.auto_active_folder().is_some()),
        "view.fullScreenPreview" => Some(u.fullscreen),
        "view.lightsOut" => Some(u.lights_out != crate::module::LightsOut::Off),
        "view.enterFullScreen" => Some(app.window_is_fullscreen),
        "panel.edit" => panel(RightPanel::Edit),
        "panel.profiles" => panel(RightPanel::Profiles),
        "panel.crop" => panel(RightPanel::Crop),
        "panel.remove" => panel(RightPanel::Remove),
        "panel.masking" => panel(RightPanel::Masking),
        "panel.redeye" => panel(RightPanel::RedEye),
        "panel.versions" => panel(RightPanel::Versions),
        "panel.activity" => panel(RightPanel::Activity),
        "panel.info" if crate::modern::on(app) => Some(app.ui.modern.info),
        "panel.info" => panel(RightPanel::Info),
        "panel.keywords" => panel(RightPanel::Keywords),
        "panel.presets" => Some(crate::panels::presets::shown(app)),
        "library.toggleAutoWriteXmp" => Some(app.session.xmp.auto_write),
        "map.showInfo" => Some(app.map.prefs.show_info),
        "map.showKey" => Some(app.map.prefs.show_key),
        "book.headerBar" => Some(u.book.header_bar),
        "book.infoOverlay" => Some(u.book.info_overlay),
        "printui.infoOverlay" => Some(app.print.info_overlay),
        "library.showSubfolderPhotos" => Some(app.session.folder_subfolders),
        "map.toggleLockMarkers" => Some(app.map.prefs.lock_markers),
        "map.toggleSavedLocations" => Some(app.map.prefs.show_saved),
        "book.toggleGuides" => Some(u.book.book.guides.show),
        "slideshow.toggleGuides" => Some(u.slides.settings.layout.show_guides),
        "printui.toggleGuides" => Some(app.print.show),
        "printui.toggleRulers" => Some(app.print.rulers),
        "library.lockFilters" => Some(app.session.lock_filters),
        "library.rememberFilters" => Some(app.session.remember_filters),
        _ => None,
    }
}

/// Labels that follow the state ("Undo Exposure", "Delete 3 Photos").
fn live_label(app: &LccApp, id: &str, label: &str) -> String {
    let n = app.session.selection.ids.len();
    match id {
        "edit.undo" => {
            app.session.undo.last().map(|e| crate::i18n::tr_format!("Undo {}", crate::i18n::tr(&e.label))).unwrap_or_else(|| "Undo".into())
        }
        "edit.redo" => {
            app.session.redo.last().map(|e| crate::i18n::tr_format!("Redo {}", crate::i18n::tr(&e.label))).unwrap_or_else(|| "Redo".into())
        }
        "photo.delete" if n > 1 => crate::i18n::tr_format!("Delete {n} Photos", n = n),
        "photo.virtualCopy" if n > 1 => crate::i18n::tr_format!("Create {n} Virtual Copies", n = n),
        "dialog.rename" if n > 1 => crate::i18n::tr_format!("Rename {n} Photos…", n = n),
        "dialog.rename" => crate::i18n::tr("Rename Photo…").to_string(),
        "app.showInFinder" => crate::i18n::tr(crate::menus::reveal_label()).to_string(),
        // Classic names the module: "Develop Shortcuts"
        "module.help" => crate::i18n::tr_format!("{module} Shortcuts", module = crate::i18n::tr(app.ui.module.label())),
        _ => label.to_string(),
    }
}

/// The parameterized submenus.
pub(crate) fn expanded(app: &LccApp, name: &str) -> Option<Vec<MenuNode>> {
    // the state-driven choice submenus (`menu_choices`)
    if let Some(v) = crate::menu_choices::expanded(app, name) {
        return Some(v);
    }
    let active = app.session.active().and_then(|id| app.session.catalog.photo(id).cloned());
    let has = active.is_some();
    Some(match name {
        "Previews" => {
            // (the background preparation's build gives way to one asked for here; a build asked
            // for while another runs waits for its turn, so the builds stay offered — only the
            // destructive discards wait, and Stop covers the waiting ones too)
            let running = app.session.preview_build.as_ref().is_some_and(|b| b.running() && !b.background);
            let queued = !app.session.preview_queue.is_empty();
            let stoppable = app.session.preview_build.as_ref().is_some_and(|b| b.running()) || queued;
            let scope = if app.session.selection.ids.len() > 1 { "Selected" } else { "Visible" };
            vec![
                item(
                    "library.buildPreviews",
                    json!({"size": "standard", "edge": app.ui.settings.standard_preview_edge()}),
                    crate::i18n::tr_format!("Build Standard-Sized Previews ({scope})", scope = crate::i18n::tr(scope)),
                    None,
                    true,
                    None,
                ),
                item(
                    "library.buildPreviews",
                    json!({"size": "full"}),
                    crate::i18n::tr_format!("Build 1:1 Previews ({scope})", scope = crate::i18n::tr(scope)),
                    None,
                    true,
                    None,
                ),
                item("library.cancelPreviews", Value::Null, "Stop Building Previews", None, stoppable, None),
                item(
                    "library.discardPreviews",
                    json!({"size": "full"}),
                    crate::i18n::tr_format!("Discard 1:1 Previews ({scope})", scope = crate::i18n::tr(scope)),
                    None,
                    !running,
                    None,
                ),
                MenuNode::Separator,
                // (read and written on a worker thread: the originals may be on a slow drive)
                item(
                    "library.smartPreviews",
                    json!({"background": true}),
                    crate::i18n::tr_format!("Build Smart Previews ({scope})", scope = crate::i18n::tr(scope)),
                    None,
                    app.session.media.smart_dir.is_some(),
                    None,
                ),
                item(
                    "library.smartPreviews",
                    json!({"discard": true, "background": true}),
                    crate::i18n::tr_format!("Discard Smart Previews ({scope})", scope = crate::i18n::tr(scope)),
                    None,
                    app.session.media.smart_dir.is_some(),
                    None,
                ),
                MenuNode::Separator,
                item("library.clearPreviews", Value::Null, "Discard Preview Cache", None, true, None),
            ]
        }
        // Classic's Library ▸ Filter by Rating: the filter bar's rating; picking the stars it has again clears it
        "Filter by Rating" => {
            use lcc_catalog::RatingOp;
            let f = &app.session.filter;
            let mut v: Vec<MenuNode> = [
                ("≥  at least", "atLeast", RatingOp::AtLeast),
                ("=  exactly", "exactly", RatingOp::Exactly),
                ("≤  at most", "atMost", RatingOp::AtMost),
            ]
            .into_iter()
            .map(|(label, k, op)| item("library.filter", json!({"ratingOp": k}), label, None, true, Some(f.rating_op == op)))
            .collect();
            v.push(MenuNode::Separator);
            v.extend((1..=5u8).map(|r| {
                // picking the stars it has clears the rule, operator included: "= 0" would now be the unrated photos
                let params = if f.rating == r { json!({"rating": 0, "ratingOp": "atLeast"}) } else { json!({"rating": r}) };
                item("library.filter", params, "★".repeat(r as usize), None, true, Some(f.rating == r))
            }));
            v.push(MenuNode::Separator);
            v.push(item("library.filter", json!({"rating": 0, "ratingOp": "atLeast"}), "None", None, f.rating_filters(), None));
            v
        }
        // View ▸ Zoom: where Toggle Zoom (Space) centres while Zoom to Focus Point is on
        "Zoom" => {
            let mut v = vec![item("view.spaceZoomsTo", Value::Null, "Space Zooms To", None, false, None)];
            v.extend(crate::state::SpaceZoom::ALL.into_iter().map(|z| {
                let on = app.ui.settings.zoom_to_focus && app.ui.settings.space_zoom == z;
                item("view.spaceZoomsTo", json!({"to": z.id()}), z.label(), None, true, Some(on))
            }));
            v
        }
        "Set Rating" => (0..=5u8)
            .map(|r| {
                let label = if r == 0 { "None".to_string() } else { "★".repeat(r as usize) };
                let sc = ["0", "1", "2", "3", "4", "5"][r as usize];
                item("photo.rate", json!({"rating": r}), label, Some(sc), has, Some(active.as_ref().is_some_and(|p| p.rating == r)))
            })
            .collect(),
        "Set Flag" => [
            ("pick", "Pick", "P", lcc_catalog::Flag::Pick),
            ("reject", "Reject", "X", lcc_catalog::Flag::Reject),
            ("none", "Unflagged", "U", lcc_catalog::Flag::None),
        ]
        .into_iter()
        .map(|(f, label, sc, flag)| {
            item("photo.flag", json!({"flag": f}), label, Some(sc), has, Some(active.as_ref().is_some_and(|p| p.flag == flag)))
        })
        .collect(),
        "Set Color Label" => {
            let mut v: Vec<MenuNode> = lcc_catalog::ColorLabel::ALL
                .iter()
                .zip([Some("6"), Some("7"), Some("8"), Some("9"), None])
                .map(|(l, sc)| {
                    let name = format!("{l:?}");
                    let label = match app.session.catalog.custom_label_name(*l) {
                        Some(custom) => format!("{custom} ({})", crate::i18n::tr(&name)),
                        None => name.clone(),
                    };
                    item(
                        "photo.label",
                        json!({"label": name.to_lowercase()}),
                        label,
                        sc,
                        has,
                        Some(active.as_ref().is_some_and(|p| p.label == Some(*l))),
                    )
                })
                .collect();
            v.push(MenuNode::Separator);
            let clear_key = crate::shortcuts::shortcut_of(&app.ui.settings.keymap, "photo.clearLabel");
            v.push(item("photo.clearLabel", Value::Null, "None", clear_key, has, Some(active.as_ref().is_some_and(|p| p.label.is_none()))));
            v.push(MenuNode::Separator);
            // label sets: each a checkable item; Edit… names them
            let sets = lcc_engine::cmd::manage::label_sets_json(&app.session);
            let current = sets["current"].as_str().map(str::to_string);
            for set in sets["sets"].as_array().into_iter().flatten() {
                let name = set["name"].as_str().unwrap_or_default();
                v.push(item(
                    "label.applySet",
                    json!({"name": name}),
                    format!("{}: {name}", crate::i18n::tr("Label Set")),
                    None,
                    true,
                    Some(current.as_deref() == Some(name)),
                ));
            }
            v.push(item("dialog.labelNames", Value::Null, "Edit Label Names…", None, true, None));
            v
        }
        "Sidebar Sections" => {
            // one checkable item per section, in the order the sidebar lists them
            let layout = &app.ui.sidebar;
            let mut v: Vec<MenuNode> = layout
                .sections()
                .iter()
                .filter(|s| s.id.available())
                .map(|s| item("view.sidebarSection", json!({"section": s.id.id()}), s.id.title(), None, true, Some(!s.hidden)))
                .collect();
            v.push(MenuNode::Separator);
            v.push(item("view.sidebarReset", Value::Null, "Reset Sidebar Sections", None, !layout.is_default(), None));
            v
        }
        "Appearance" => {
            use crate::state::AppearanceMode;
            use crate::theme::ThemeKind;
            let s = &app.ui.settings;
            let mut v: Vec<MenuNode> = [
                ("auto", "Sync with System", AppearanceMode::Auto),
                ("light", "Light Mode", AppearanceMode::Light),
                ("dark", "Dark Mode", AppearanceMode::Dark),
            ]
            .into_iter()
            .map(|(mode, label, m)| item("view.appearance", json!({"mode": mode}), label, None, true, Some(s.appearance_mode == m)))
            .collect();
            v.push(item("view.appearance", Value::Null, "Next Appearance Mode", None, true, None));
            // the saved theme of each family is checked
            for group in [[ThemeKind::Charcoal, ThemeKind::Midnight], [ThemeKind::Silver, ThemeKind::Paper]] {
                v.push(MenuNode::Separator);
                for k in group {
                    let on = if k.is_dark() { s.dark_theme.kind() == k } else { s.light_theme.kind() == k };
                    v.push(item("view.theme", json!({"theme": k.id()}), k.label(), None, true, Some(on)));
                }
            }
            v
        }
        "Loupe Overlay" => {
            // Lightroom Classic's View ▸ Loupe Overlay ▸ Layout Image: choose the graphic, show it,
            // and how opaque it is and how dark the photo is outside it
            let li = &app.ui.layout_image;
            let chosen = !li.path.trim().is_empty();
            let mut v = vec![
                item("view.chooseLayoutImage", Value::Null, "Choose Layout Image…", None, true, None),
                item(
                    "view.layoutImage",
                    Value::Null,
                    "Layout Image",
                    crate::shortcuts::shortcut_of(&app.ui.settings.keymap, "view.layoutImage"),
                    chosen,
                    Some(li.show && chosen),
                ),
                MenuNode::Separator,
            ];
            let near = |a: f32, b: f32| (a - b).abs() < 0.005;
            for step in crate::state::LayoutImage::OPACITY_STEPS {
                let label = format!("{} {:.0}%", crate::i18n::tr("Opacity"), step * 100.0);
                v.push(item("view.layoutImageOpacity", json!({"opacity": step}), label, None, chosen, Some(near(li.opacity, step))));
            }
            v.push(MenuNode::Separator);
            for step in crate::state::LayoutImage::MATTE_STEPS {
                let label =
                    if step == 0.0 { crate::i18n::tr("No Matte").to_string() } else { format!("{} {:.0}%", crate::i18n::tr("Matte"), step * 100.0) };
                v.push(item("view.layoutImageMatte", json!({"matte": step}), label, None, chosen, Some(near(li.matte, step))));
            }
            v
        }
        "Tool Overlay" => {
            // when the spot and mask pins show; the last item steps to the next mode (Shift+H)
            use crate::state::ToolOverlay;
            let mut v: Vec<MenuNode> = ToolOverlay::ALL
                .into_iter()
                .map(|m| item("view.toolOverlay", json!({"mode": m.name()}), m.label(), None, true, Some(app.ui.tool_overlay == m)))
                .collect();
            v.push(MenuNode::Separator);
            let shortcut = crate::shortcuts::shortcut_of(&app.ui.settings.keymap, "view.toolOverlay");
            v.push(item("view.toolOverlay", Value::Null, "Cycle Tool Overlay", shortcut, true, None));
            v
        }
        "Grid View Style" => ["filename", "exposure", "date"]
            .into_iter()
            .zip(["File Name", "Exposure (shutter · aperture · ISO)", "Capture Date"])
            .map(|(k, label)| item("view.gridInfo", json!({"info": k}), label, None, true, Some(app.ui.grid_info == k)))
            .collect(),
        "Sort" => {
            use lcc_catalog::SortKey::*;
            let cur = app.session.sort;
            let mut v: Vec<MenuNode> = [
                ("Capture Date", CaptureDate, "captureDate"),
                ("Import Date", ImportDate, "importDate"),
                ("Modified Date", EditDate, "editDate"),
                ("File Name", FileName, "fileName"),
                ("Rating", Rating, "rating"),
                ("File Size", FileSize, "fileSize"),
                ("Random", Random, "random"),
                ("Quality", Quality, "quality"),
            ]
            .into_iter()
            .map(|(label, key, k)| item("library.sort", json!({"key": k}), label, None, true, Some(cur.key == key)))
            .collect();
            let in_album = crate::panels::grid::custom_order_album(app).is_some();
            v.push(item("library.sort", json!({"key": "custom"}), "Custom Order", None, in_album, Some(in_album && cur.key == Custom)));
            v.push(item("library.shuffle", json!({}), "Reshuffle", None, cur.key == Random, None));
            v.push(MenuNode::Separator);
            // a shuffle has no direction worth choosing
            v.push(item(
                "library.sort",
                json!({"ascending": true}),
                "Ascending",
                None,
                cur.key != Random,
                (cur.key != Random).then_some(cur.ascending),
            ));
            v.push(item(
                "library.sort",
                json!({"ascending": false}),
                "Descending",
                None,
                cur.key != Random,
                (cur.key != Random).then_some(!cur.ascending),
            ));
            v.push(MenuNode::Separator);
            use lcc_catalog::GroupBy;
            let groups = [
                ("Group by Date: Automatic", GroupBy::Auto, "auto"),
                ("Group by Day", GroupBy::Day, "day"),
                ("Group by Month", GroupBy::Month, "month"),
                ("Group by Year", GroupBy::Year, "year"),
                ("No Date Groups", GroupBy::None, "none"),
            ];
            v.extend(groups.into_iter().map(|(label, g, k)| item("library.sort", json!({"group": k}), label, None, true, Some(cur.group == g))));
            v
        }
        #[cfg(not(target_arch = "wasm32"))]
        "Plug-ins" => crate::panels::plugins::menu_items(app).into_iter().map(|(id, p, label)| item(&id, p, label, None, true, None)).collect(),
        "Libraries" => {
            let can = crate::panels::settings::can_switch_library(app).is_ok();
            let open = app.session.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone());
            let mut recent: Vec<String> = Vec::new();
            for r in &app.ui.settings.recent_libraries {
                if !recent.iter().any(|x| std::path::Path::new(x) == std::path::Path::new(r)) {
                    recent.push(r.clone());
                }
            }
            let recent = &recent;
            let label = |r: &str| {
                let d = std::path::Path::new(r);
                format!("{} ({})", crate::panels::settings::library_name(d), d.parent().map(|p| p.display().to_string()).unwrap_or_default())
            };
            let mut v: Vec<MenuNode> = recent
                .iter()
                .map(|r| {
                    let d = std::path::Path::new(r);
                    // a library a check found missing (moved, drive disconnected) is greyed out; the
                    // check runs on a worker (`RecentChecks`), never here: the menu is rebuilt every frame
                    let gone = app.recent_checks.exists(r) == Some(false);
                    item("app.openLibrary", json!({"path": r}), label(r), None, can && !gone, Some(open.as_deref() == Some(d)))
                })
                .collect();
            if v.is_empty() {
                v.push(item("app.clearRecentLibraries", Value::Null, "No Recent Catalogs", None, false, None));
            } else {
                v.push(MenuNode::Separator);
                // one entry at a time, missing ones too: only the list forgets it (issue #1290)
                let forget = recent.iter().map(|r| item("app.recentLibraries", json!({"remove": r}), label(r), None, true, None)).collect();
                v.push(MenuNode::Submenu { label: "Remove from List".into(), children: forget });
                v.push(item("app.clearRecentLibraries", Value::Null, "Clear Recent Catalogs", None, true, None));
            }
            v
        }
        "Import from Device" => {
            let devices = lcc_engine::devices::devices();
            if devices.is_empty() {
                vec![item("file.addFromDevice", Value::Null, "No Camera or Card Found", None, false, None)]
            } else {
                devices.into_iter().map(|d| item("file.addFromDevice", json!({"path": d.path}), format!("{}…", d.name), None, true, None)).collect()
            }
        }
        "Metadata Preset" => {
            let sel = !app.session.selection.ids.is_empty() || has;
            let mut v: Vec<MenuNode> = app
                .session
                .metadata_presets
                .iter()
                .map(|m| item("metadata.applyPreset", json!({"name": m.name}), m.name.clone(), None, sel, None))
                .collect();
            if !v.is_empty() {
                v.push(MenuNode::Separator);
            }
            v.push(item("dialog.saveMetadataPreset", Value::Null, "Save Metadata Preset…", None, has, None));
            v
        }
        // Classic's three Edit ▸ Select by submenus (the "Select by" list split up)
        "Select by Flag" => [("pick", "Flagged"), ("none", "Unflagged"), ("reject", "Rejected")]
            .into_iter()
            .map(|(f, label)| item("library.selectBy", json!({"flag": f}), label, None, true, None))
            .collect(),
        "Select by Rating" => {
            let stars = |r: u8| if r == 1 { crate::i18n::tr("1 Star").to_string() } else { crate::i18n::tr_format!("{n} Stars", n = r) };
            let mut v: Vec<MenuNode> =
                (1..=5u8).map(|r| item("library.selectBy", json!({"rating": r, "ratingOp": "eq"}), stars(r), None, true, None)).collect();
            v.push(MenuNode::Separator);
            v.extend((1..=4u8).map(|r| {
                item("library.selectBy", json!({"rating": r}), crate::i18n::tr_format!("{stars} and Higher", stars = stars(r)), None, true, None)
            }));
            v.push(MenuNode::Separator);
            v.push(item("library.selectBy", json!({"rating": 0, "ratingOp": "eq"}), "Unrated", None, true, None));
            v
        }
        "Select by Color Label" => lcc_catalog::ColorLabel::ALL
            .iter()
            .map(|l| {
                let name = format!("{l:?}");
                item("library.selectBy", json!({"label": name.to_lowercase()}), name, None, true, None)
            })
            .collect(),
        // Classic's Metadata ▸ Color Label Set: the sets, then Edit
        "Color Label Set" => {
            let sets = lcc_engine::cmd::manage::label_sets_json(&app.session);
            let current = sets["current"].as_str().map(str::to_string);
            let mut v = crate::menu_choices::choices(
                "label.applySet",
                sets["sets"].as_array().into_iter().flatten().map(|set| {
                    let name = set["name"].as_str().unwrap_or_default();
                    crate::menu_choices::Choice::new(name, json!({"name": name}), current.as_deref() == Some(name))
                }),
            );
            v.push(MenuNode::Separator);
            v.push(item("dialog.labelNames", Value::Null, "Edit…", None, true, None));
            v
        }
        // Classic's Settings ▸ Process Version: every process this build renders, the active photo's checked
        "Process Version" => {
            let current = active.as_ref().map(|p| p.develop.process);
            lcc_develop::Process::ALL
                .iter()
                .map(|p| {
                    let v = p.version();
                    item(
                        "develop.setProcess",
                        json!({"version": v.0}),
                        crate::i18n::tr_format!("Version: {}", v.0),
                        None,
                        has,
                        Some(current == Some(v)),
                    )
                })
                .collect()
        }
        "Select by" => {
            let mut v: Vec<MenuNode> = [("pick", "Picks"), ("reject", "Rejects"), ("none", "Unflagged")]
                .into_iter()
                .map(|(f, label)| item("library.selectBy", json!({"flag": f}), label, None, true, None))
                .collect();
            v.push(MenuNode::Separator);
            v.extend((1..=5u8).map(|r| {
                item("library.selectBy", json!({"rating": r}), crate::i18n::tr_format!("{} and higher", "★".repeat(r as usize)), None, true, None)
            }));
            // each rating on its own (#3311), with Unrated as the zero row
            v.push(MenuNode::Separator);
            v.extend((1..=5u8).map(|r| {
                item(
                    "library.selectBy",
                    json!({"rating": r, "ratingOp": "eq"}),
                    crate::i18n::tr_format!("{} only", "★".repeat(r as usize)),
                    None,
                    true,
                    None,
                )
            }));
            v.push(item("library.selectBy", json!({"rating": 0, "ratingOp": "eq"}), "Unrated", None, true, None));
            v.push(MenuNode::Separator);
            v.extend(lcc_catalog::ColorLabel::ALL.iter().map(|l| {
                let name = format!("{l:?}");
                item(
                    "library.selectBy",
                    json!({"label": name.to_lowercase()}),
                    crate::i18n::tr_format!("{name} Label", name = name),
                    None,
                    true,
                    None,
                )
            }));
            v
        }
        "Export with Preset" => {
            let sel = !app.session.selection.ids.is_empty() || has;
            let mut v: Vec<MenuNode> = Vec::new();
            let mut builtin = true;
            for (p, b) in app.session.all_export_presets() {
                if builtin && !b {
                    v.push(MenuNode::Separator);
                }
                builtin = b;
                v.push(item("app.export", json!({"preset": p.name, "background": true}), p.name, None, sel, None));
            }
            v.push(MenuNode::Separator);
            v.push(item("dialog.export", Value::Null, "Custom…", None, sel, None));
            v
        }
        "Add to Collection" => {
            let targets = app.session.selected();
            let sel = !targets.is_empty();
            // albums hold library photos (issue #684): a Local-only selection is told what to do first
            let local_only = crate::panels::grid::local_only(app, &targets);
            let mut v: Vec<MenuNode> = vec![];
            if local_only {
                v.push(item("photo.addToLibrary", Value::Null, crate::i18n::tr("Add to My Photos First"), None, false, None));
                v.push(MenuNode::Separator);
            }
            let mut albums: Vec<_> = app.session.catalog.albums().filter(|a| !a.folder && !a.is_smart()).map(|a| (a.name.clone(), a.id.0)).collect();
            albums.sort_by_key(|(n, _)| n.to_lowercase());
            let any = !albums.is_empty();
            v.extend(albums.into_iter().map(|(name, id)| item("album.addPhotos", json!({"id": id}), name, None, sel && !local_only, None)));
            if any {
                v.push(MenuNode::Separator);
            }
            v.push(item("dialog.newAlbum", Value::Null, "New Collection…", None, !local_only, None));
            v
        }
        _ => return None,
    })
}

/// Resolve a live menu row, including contextual keys and borrowed grid-cycle hints.
fn node(app: &LccApp, e: &MenuEntry) -> MenuNode {
    let shortcut = e.shortcut.as_deref();
    // in an album the Delete key removes from the album (issue #1805): the menu says so
    let delete_key = crate::shortcuts::shortcut_of(&app.ui.settings.keymap, "photo.delete");
    let shortcut = match e.id.as_str() {
        "view.photoGrid" | "view.squareGrid" => shortcut.or_else(|| crate::shortcuts::shortcut_of(&app.ui.settings.keymap, "view.gridToggle")),
        "album.removePhotos" => delete_key,
        "photo.delete" if matches!(app.session.source, lcc_engine::LibrarySource::Album(_)) => None,
        _ => shortcut,
    };
    item(&e.id, Value::Null, live_label(app, &e.id, &e.label), shortcut, e.enabled, checked(app, &e.id))
}

/// Drop leading, trailing and doubled separators (also inside submenus) and empty submenus.
fn tidy(v: Vec<MenuNode>) -> Vec<MenuNode> {
    let mut out: Vec<MenuNode> = Vec::with_capacity(v.len());
    for n in v {
        let n = match n {
            MenuNode::Submenu { label, children } => {
                let children = tidy(children);
                if children.is_empty() {
                    continue;
                }
                MenuNode::Submenu { label, children }
            }
            n => n,
        };
        if n == MenuNode::Separator && out.last().is_none_or(|l| *l == MenuNode::Separator) {
            continue;
        }
        out.push(n);
    }
    while out.last() == Some(&MenuNode::Separator) {
        out.pop();
    }
    out
}

/// Every command a layout slot may name, whatever its own menu path (none, too): the UI commands,
/// then the engine's.
fn all_commands(app: &LccApp) -> Vec<MenuEntry> {
    let keymap = &app.ui.settings.keymap;
    let mut v: Vec<MenuEntry> = crate::menus::ui_commands()
        .filter(|c| !crate::lrc_menus::is_coming_soon(c.0))
        .map(|(id, label, _, m)| MenuEntry {
            id: id.to_string(),
            label: label.to_string(),
            menu: if m.is_empty() { Vec::new() } else { m.split('>').map(str::to_string).collect() },
            shortcut: crate::shortcuts::shortcut_of(keymap, id).map(str::to_string),
            enabled: crate::menus::ui_enabled(app, id),
        })
        .collect();
    for c in app.session.commands() {
        if v.iter().any(|e| e.id == c.id) {
            continue;
        }
        v.push(MenuEntry {
            id: c.id.into(),
            label: c.label.into(),
            menu: c.menu.iter().map(|s| s.to_string()).collect(),
            shortcut: crate::shortcuts::shortcut_of(keymap, c.id).map(str::to_string),
            enabled: c.enabled,
        });
    }
    v
}

/// The key the module binds to `id` with `params` (Classic set), which the menu shows instead of
/// the global one: ⌘N is New Collection in Library, New Snapshot in Develop.
fn module_shortcut(app: &LccApp, id: &str, params: &Value) -> Option<String> {
    let want = if params.is_null() { json!({}) } else { params.clone() };
    crate::shortcuts::module_keys_of(app.ui.module)
        .iter()
        .chain(crate::shortcuts::classic_extra_keys())
        .filter(|(sc, ..)| !app.ui.settings.keymap.values().any(|user| user == sc))
        .find(|(_, kid, kp)| *kid == id && serde_json::from_str::<Value>(kp).unwrap_or_else(|_| json!({})) == want)
        .map(|(sc, ..)| sc.to_string())
}

/// The keys any command has now (the greyed "Coming soon" items show Classic's key only while it
/// is free).
fn taken_keys(app: &LccApp) -> Vec<(egui::Modifiers, egui::Key)> {
    let keymap = &app.ui.settings.keymap;
    let mut v: Vec<_> = crate::shortcuts::bindable()
        .iter()
        .filter_map(|b| crate::shortcuts::binding(keymap, b.id, b.default()))
        .filter_map(crate::shortcuts::parse)
        .collect();
    v.extend(
        crate::shortcuts::module_keys_of(app.ui.module)
            .iter()
            .chain(crate::shortcuts::classic_extra_keys())
            .filter_map(|(sc, ..)| crate::shortcuts::parse(sc)),
    );
    v.extend(crate::shortcuts::ALIASES.iter().filter_map(|(sc, ..)| crate::shortcuts::parse(sc)));
    v
}

/// What the menus are built from: every command once, the registry's (with a menu path) for the
/// leftovers, and whether an entry has been placed yet.
struct Build<'a> {
    app: &'a LccApp,
    all: Vec<MenuEntry>,
    /// Indices into `all` of entries with a menu path that are offered (not hidden, the host has them).
    listed: Vec<usize>,
    used: Vec<bool>,
    taken: Option<Vec<(egui::Modifiers, egui::Key)>>,
}

impl Build<'_> {
    /// A "Coming soon" row: greyed, Classic's key shown while it's free, never run.
    fn soon(&mut self, id: &str) -> Option<MenuNode> {
        let c = crate::lrc_menus::coming_soon(id)?;
        let app = self.app;
        let taken = self.taken.get_or_insert_with(|| taken_keys(app));
        let hint = c.hint.filter(|sc| crate::shortcuts::parse(sc).is_some_and(|k| !taken.contains(&k)));
        Some(item(c.id, Value::Null, c.label, hint, false, None))
    }

    /// The row for a command slot (`label`: Classic's name for it).
    fn command(&mut self, id: &str, params: Option<&str>, label: Option<&str>) -> Option<MenuNode> {
        if crate::lrc_menus::is_coming_soon(id) {
            return self.soon(id);
        }
        if !host_supports(self.app, id) {
            return None;
        }
        let i = self.all.iter().position(|e| e.id == id)?;
        if let Some(u) = self.listed.iter().position(|l| *l == i) {
            self.used[u] = true;
        }
        let app = self.app;
        let e = &self.all[i];
        let mut n = node(app, e);
        if let MenuNode::Item { label: l, params: p, shortcut, checked, .. } = &mut n {
            // Classic's name, unless the label follows the state ("Undo Exposure") or macOS names it
            if let Some(label) = label
                && *l == e.label
                && !(id == "app.quit" && cfg!(target_os = "macos"))
            {
                *l = label.to_string();
            }
            if let Some(json) = params {
                *p = serde_json::from_str(json).unwrap_or(Value::Null);
                // a parameterized row is one choice: no command-wide key or check
                *shortcut = None;
                *checked = None;
            }
            if let Some(sc) = module_shortcut(app, id, p) {
                *shortcut = Some(sc);
            }
        }
        Some(n)
    }

    /// The items of a submenu shown as `label` in menu `title`, gathering path `menu>path`.
    fn submenu(&mut self, title: &str, label: &str, menu: &str, path: &str) -> Vec<MenuNode> {
        // Zoom's generated choices (Space Zooms To) follow its commands
        let after = path == "Zoom";
        let generated = expanded(self.app, path).unwrap_or_default();
        let mut children = if after { Vec::new() } else { generated.clone() };
        children.push(MenuNode::Separator);
        let here = |e: &MenuEntry| e.menu.len() == 2 && e.menu[0] == menu && e.menu[1] == path;
        // a generated row stands for its registry entry (Previews listed Discard Preview Cache twice)
        for n in &generated {
            if let MenuNode::Item { id, .. } = n {
                for u in 0..self.listed.len() {
                    if self.all[self.listed[u]].id == *id && here(&self.all[self.listed[u]]) {
                        self.used[u] = true;
                    }
                }
            }
        }
        let classic = crate::lrc_menus::sub_layout(title, label);
        let order = classic.or_else(|| SUB_LAYOUT.iter().find(|(m, s, _)| *m == menu && *s == path).map(|(_, _, o)| *o)).unwrap_or(&[]);
        for slot in order {
            match crate::lrc_menus::parse(slot) {
                crate::lrc_menus::Slot::Separator => children.push(MenuNode::Separator),
                crate::lrc_menus::Slot::Submenu(..) => {}
                crate::lrc_menus::Slot::Item(id, params, l) => {
                    // the registry's own order (no Classic layout) takes only this path's unplaced entries
                    if classic.is_none() && !self.listed.iter().zip(&self.used).any(|(i, u)| !u && self.all[*i].id == id && here(&self.all[*i])) {
                        continue;
                    }
                    children.extend(self.command(id, params, l));
                }
            }
        }
        let mut dialogs = Vec::new();
        for u in 0..self.listed.len() {
            let e = &self.all[self.listed[u]];
            if !self.used[u] && here(e) {
                self.used[u] = true;
                // commands that open a dialog go last, after a separator
                if e.label.ends_with('…') { dialogs.push(node(self.app, e)) } else { children.push(node(self.app, e)) }
            }
        }
        if after {
            children.push(MenuNode::Separator);
            children.extend(generated);
        }
        if !dialogs.is_empty() {
            // Tethered Capture opens with Start Tethered Capture… (as in Classic); elsewhere
            // the dialog commands close the submenu
            if path == "Tethered Capture" {
                dialogs.push(MenuNode::Separator);
                dialogs.extend(children);
                return dialogs;
            }
            children.push(MenuNode::Separator);
            children.extend(dialogs);
        }
        children
    }
}

/// The menus of the module on screen, in order (Lightroom Classic's per-module menu bar).
pub fn menus(app: &LccApp) -> &'static [&'static str] {
    crate::lrc_menus::titles(app.ui.module)
}

/// The whole menu bar: (title, items) per menu of the module on screen ([`menus`]), laid out as
/// Lightroom Classic's ([`crate::lrc_menus`]); a shared menu (and Library's) then lists the other
/// commands whose menu path names it.
pub fn menu_bar(app: &LccApp) -> Vec<(String, Vec<MenuNode>)> {
    let all = all_commands(app);
    let listed: Vec<usize> = (0..all.len())
        .filter(|i| {
            let e = &all[*i];
            !e.menu.is_empty() && !HIDDEN.contains(&e.id.as_str()) && host_supports(app, &e.id)
        })
        .collect();
    let used = vec![false; listed.len()];
    let mut b = Build { app, all, listed, used, taken: None };
    let module = app.ui.module;
    let mut bar = Vec::new();
    for title in crate::lrc_menus::titles(module) {
        let mut items = Vec::new();
        let mut last = Vec::new();
        for slot in crate::lrc_menus::layout(module, title) {
            match crate::lrc_menus::parse(slot) {
                crate::lrc_menus::Slot::Separator => items.push(MenuNode::Separator),
                crate::lrc_menus::Slot::Submenu(label, menu, path) => {
                    let children = b.submenu(title, label, menu.unwrap_or(title), path);
                    items.push(MenuNode::Submenu { label: label.to_string(), children });
                }
                crate::lrc_menus::Slot::Item(id, params, label) => {
                    if let Some(n) = b.command(id, params, label) {
                        if LAST.contains(&id) { last.push(n) } else { items.push(n) }
                    }
                }
            }
        }
        if !crate::lrc_menus::strict(module, title) {
            // everything else that names this menu
            items.push(MenuNode::Separator);
            let mut extra_subs: Vec<String> = Vec::new();
            for u in 0..b.listed.len() {
                let e = &b.all[b.listed[u]];
                if b.used[u] || e.menu.first().map(String::as_str) != Some(title) {
                    continue;
                }
                if e.menu.len() == 1 {
                    b.used[u] = true;
                    items.push(node(app, e));
                } else if !extra_subs.contains(&e.menu[1]) {
                    extra_subs.push(e.menu[1].clone());
                }
            }
            for name in extra_subs {
                let children = b.submenu(title, &name, title, &name);
                items.push(MenuNode::Submenu { label: name, children });
            }
        }
        if !last.is_empty() {
            items.push(MenuNode::Separator);
            items.extend(last);
        }
        bar.push((title.to_string(), tidy(items)));
    }
    bar
}

/// Run a menu item. Rating, flag and label items behave like their keys (the active photo only in
/// Compare/Survey; Auto Advance moves on).
/// The text-editing shortcuts a focused text field owns (⌘A / ⌘C / ⌘X / ⌘V / ⌘Z / ⇧⌘Z).
pub const TEXT_EDIT_SHORTCUTS: &[&str] = &["Cmd+A", "Cmd+C", "Cmd+V", "Cmd+X", "Cmd+Z", "Cmd+Shift+Z"];

/// A menu item reached by a text-editing shortcut while a text field has the keyboard focus
/// (issue #281: the macOS menu bar can still deliver ⌘A to Library ▸ Select All): the field gets
/// the edit (select its text, copy, paste…) instead of the command. Returns whether it did.
pub fn forward_to_text_field(app: &mut LccApp, ctx: &egui::Context, shortcut: Option<&str>) -> bool {
    let Some(sc) = shortcut.filter(|sc| TEXT_EDIT_SHORTCUTS.contains(sc)) else { return false };
    if !ctx.egui_wants_keyboard_input() {
        return false;
    }
    let command = egui::Modifiers::COMMAND;
    let key = |key: egui::Key, modifiers: egui::Modifiers| {
        [true, false].map(|pressed| egui::Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers })
    };
    match sc {
        "Cmd+C" => app.synthetic.push(egui::Event::Copy),
        "Cmd+X" => app.synthetic.push(egui::Event::Cut),
        "Cmd+V" => ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste),
        "Cmd+A" => app.synthetic.extend(key(egui::Key::A, command)),
        "Cmd+Z" => app.synthetic.extend(key(egui::Key::Z, command)),
        _ => app.synthetic.extend(key(egui::Key::Z, command | egui::Modifiers::SHIFT)),
    }
    ctx.request_repaint();
    true
}

pub fn run_item(app: &mut LccApp, id: &str, params: Value) -> Result<Value, String> {
    let mut params = if params.is_null() { json!({}) } else { params };
    let culling_cmd = matches!(
        id,
        "photo.rate"
            | "photo.flag"
            | "photo.label"
            | "photo.clearLabel"
            | "photo.pick"
            | "photo.reject"
            | "photo.unflag"
            | "photo.cycleFlag"
            | "photo.increaseFlag"
            | "photo.decreaseFlag"
    );
    if culling_cmd {
        crate::panels::compare::target_active(app, &mut params);
    }
    if id == "library.deleteRejected" && params.get("files").is_none() {
        // always asked: it can move files to the trash
        return crate::menus::ask_delete_rejected(app);
    }
    if id == "photo.delete" && crate::menus::confirm_delete(app) {
        return Ok(Value::Null);
    }
    // moves files: always asks first, even with "Confirm before deleting" off (without a
    // selection the command itself reports why nothing happens)
    if id == "photo.deleteFromDisk" && crate::menus::confirm_delete_from_disk(app) {
        return Ok(Value::Null);
    }
    if matches!(id, "photo.deletePermanently" | "library.emptyRecentlyDeleted")
        && crate::menus::confirm_purge(app, id == "library.emptyRecentlyDeleted")
    {
        return Ok(Value::Null);
    }
    let was = app.session.active();
    let r = app.run(id, params);
    if culling_cmd && r.is_ok() && app.auto_advance_on() {
        crate::panels::compare::advance(app, was);
    }
    r
}

/// Human-readable shortcut text for menus: `Cmd+Shift+Z` → `⌘⇧Z` on macOS, `Ctrl+Shift+Z` elsewhere.
pub fn shortcut_text(sc: &str, mac: bool) -> String {
    if mac {
        let mut mods = String::new();
        let mut key = "";
        for part in sc.split('+') {
            match part {
                "Ctrl" => mods.push('⌃'),
                "Alt" => mods.push('⌥'),
                "Shift" => mods.push('⇧'),
                "Cmd" => mods.push('⌘'),
                k => key = k,
            }
        }
        let key = match key {
            "Delete" => "⌫",
            "Escape" => "⎋",
            "Left" => "←",
            "Right" => "→",
            "Up" => "↑",
            "Down" => "↓",
            k => k,
        };
        format!("{mods}{key}")
    } else {
        sc.replace("Cmd", "Ctrl")
    }
}

/// Horizontal gap between in-window menu titles' text. Each title sits in a highlight [`TITLE_PAD`]
/// wider than its text on both sides; the rest of the gap is the layout's item spacing.
/// The bar tightens the gap down to [`MIN_TITLE_GAP`] before it collapses into one "Menu" button.
const TITLE_GAP: f32 = 20.0;
const MIN_TITLE_GAP: f32 = 10.0;
/// Room between a title's text and the edge of its hover / open highlight.
const TITLE_PAD: f32 = 4.0;
/// Height of a title's highlight; its menu hangs just below it.
const TITLE_H: f32 = 26.0;
/// Gap between a title's highlight and the top of its menu.
const MENU_GAP: f32 = 2.0;

/// A top-level menu title, laid out but not yet drawn: [`paint_title`] draws it once the frame
/// knows whether its menu is open.
fn title_button(ui: &mut egui::Ui, text: &str, font: egui::FontId, color: egui::Color32) -> (egui::Response, std::sync::Arc<egui::Galley>) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, color);
    let size = egui::vec2(galley.size().x + 2.0 * TITLE_PAD, TITLE_H);
    let (_, r) = ui.allocate_exact_size(size, egui::Sense::click());
    crate::widgets::announce(&r, text);
    (r, galley)
}

/// Draw a menu title: highlighted while the pointer is on it or its menu is open, darker while
/// pressed, like the titles of a native menu bar.
fn paint_title(ui: &egui::Ui, r: &egui::Response, galley: std::sync::Arc<egui::Galley>, open: bool) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let fill = if r.is_pointer_button_down_on() {
        Some(t.pressed)
    } else if open || r.hovered() {
        Some(t.hover)
    } else {
        None
    };
    if let Some(fill) = fill {
        ui.painter().rect_filled(r.rect, 4.0, fill);
    }
    let pos = r.rect.center() - galley.size() / 2.0;
    ui.painter().galley(pos, galley, t.text_label);
}

/// Width of the in-window menu bar's titles at their full spacing.
pub fn bar_width(ui: &egui::Ui, module: crate::module::ModuleId) -> f32 {
    let t = crate::theme::Tokens::get(ui.ctx());
    crate::lrc_menus::titles(module)
        .iter()
        .map(|m| ui.painter().layout_no_wrap(crate::i18n::tr(m).to_string(), t.font(13.0), t.text).size().x + TITLE_GAP)
        .sum::<f32>()
}

/// The in-window menu bar (hosts without a native one): one dropdown per menu, or a single
/// "Menu" button when the space is too narrow. Returns the width used.
pub fn show_in_window(app: &mut LccApp, ui: &mut egui::Ui, max_width: f32) -> f32 {
    let t = crate::theme::Tokens::get(ui.ctx());
    let bar = menu_bar(app);
    let font = t.font(13.0);
    let widths: Vec<f32> =
        bar.iter().map(|(title, _)| ui.painter().layout_no_wrap(crate::i18n::tr(title).to_string(), font.clone(), t.text).size().x).collect();
    let text: f32 = widths.iter().sum();
    let n = widths.len().max(1) as f32;
    let gap = ((max_width - text) / n).clamp(MIN_TITLE_GAP, TITLE_GAP);
    let total = text + gap * n;
    let mut clicked: Option<(String, Value)> = None;
    let start = ui.cursor().left();
    let mac = ui.ctx().os() == egui::os::OperatingSystem::Mac;
    // Menus hang below the whole top bar and never grow past the window (#189): a tall menu
    // scrolls instead of egui sliding it up over the titles.
    let bar_bottom = Some(ui.max_rect().bottom());
    if total <= max_width {
        let saved = ui.spacing().item_spacing.x;
        ui.spacing_mut().item_spacing.x = (gap - 2.0 * TITLE_PAD).max(0.0);
        // All titles first, then their popups, so a title the pointer moves onto can take over
        // from the open menu before either is drawn (no frame with two menus or none).
        let (titles, galleys): (Vec<egui::Response>, Vec<_>) = bar
            .iter()
            .map(|(title, _)| {
                let (r, g) = title_button(ui, crate::i18n::tr(title), font.clone(), t.text_label);
                crate::widgets::register(ui.ctx(), format!("menu:{title}"), r.rect);
                (r, g)
            })
            .unzip();
        let confirmed = switch_open_title(ui.ctx(), &titles);
        for (r, g) in titles.iter().zip(galleys) {
            paint_title(ui, r, g, egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(r)));
        }
        for ((_, items), r) in bar.iter().zip(&titles) {
            // (opening and closing by click is done in `switch_open_title`)
            // (the click confirming a menu a hover opened must not close it again)
            let close = if confirmed == Some(egui::Popup::default_response_id(r)) {
                egui::PopupCloseBehavior::IgnoreClicks
            } else {
                egui::PopupCloseBehavior::CloseOnClick
            };
            egui::Popup::menu(r)
                .gap(MENU_GAP)
                .close_behavior(close)
                .open_memory(None)
                .show(|ui| crate::menu_level::level(ui, 1, bar_bottom, |ui| nodes_ui(ui, items, mac, &mut clicked, 1, bar_bottom)));
        }
        ui.spacing_mut().item_spacing.x = saved;
    } else {
        let (r, g) = title_button(ui, crate::i18n::tr("Menu"), font.clone(), t.text_label);
        crate::widgets::register(ui.ctx(), "menu:all", r.rect);
        paint_title(ui, &r, g, egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&r)));
        egui::Popup::menu(&r).gap(MENU_GAP).show(|ui| {
            crate::menu_level::level(ui, 1, bar_bottom, |ui| {
                for (title, items) in &bar {
                    submenu(ui, title, menu_text(crate::i18n::tr(title)), 1, bar_bottom, |ui| nodes_ui(ui, items, mac, &mut clicked, 2, bar_bottom));
                }
            });
        });
    }
    if let Some((id, params)) = clicked {
        let r = run_item(app, &id, params);
        // an export that can't start (e.g. no folder), or a backup / restore the host refuses
        // (changes still unsaved, nothing stored in this browser), says why instead of doing nothing
        if let Err(e) = r
            && matches!(id.as_str(), "app.export" | "app.exportPrevious" | "file.backupLibrary" | "file.restoreLibrary")
        {
            app.toast(ui.ctx(), e);
        }
    }
    ui.cursor().left() - start
}

/// Native menu-bar behaviour for the top-level titles: while one menu is open, resting the pointer
/// on another title closes it and opens that one, and a click on another title opens it in that
/// same click (instead of only closing the open menu as a click outside). Without an open menu
/// hovering does nothing; clicking a title toggles its menu. egui keeps one popup open at a time.
/// Returns the menu whose title was just clicked right after a hover opened it.
fn switch_open_title(ctx: &egui::Context, titles: &[egui::Response]) -> Option<egui::Id> {
    let popup = egui::Popup::default_response_id;
    let key = egui::Id::new("lc-menubar-hover-opened");
    let open = titles.iter().position(|r| egui::Popup::is_id_open(ctx, popup(r)));
    // the menu a hover opened, until it closes or is clicked
    let mut confirmed = None;
    let mut hover_opened: Option<egui::Id> = ctx.data(|d| d.get_temp(key)).filter(|id| open.is_some_and(|o| popup(&titles[o]) == *id));
    for (i, r) in titles.iter().enumerate() {
        if r.clicked() {
            // the click that follows a hover switch confirms the menu the hover opened
            if hover_opened == Some(popup(r)) {
                confirmed = Some(popup(r));
            } else {
                egui::Popup::toggle_id(ctx, popup(r));
            }
            hover_opened = None;
        } else if r.hovered() && open.is_some_and(|o| o != i) {
            egui::Popup::open_id(ctx, popup(r));
            hover_opened = Some(popup(r));
            // a pointer move alone doesn't schedule the frame that draws the new menu
            ctx.request_repaint();
        }
    }
    // (leaving the titles ends it: a later click on the open title closes the menu)
    let hover_opened = hover_opened.filter(|_| titles.iter().any(|r| r.hovered()));
    ctx.data_mut(|d| match hover_opened {
        Some(id) => {
            d.insert_temp(key, id);
        }
        None => {
            d.remove::<egui::Id>(key);
        }
    });
    confirmed
}

/// A submenu row showing `text` at `depth`, whose rows `children` draws one level deeper,
/// bounded like every level. The row is registered as `menusub:<label>` (the untranslated
/// label), and while its submenu is open it is the anchor the next frame's room is measured from.
fn submenu(ui: &mut egui::Ui, label: &str, text: egui::RichText, depth: usize, bar_bottom: Option<f32>, children: impl FnOnce(&mut egui::Ui)) {
    let r = crate::widgets::menu_button(ui, text, |ui| crate::menu_level::level(ui, depth + 1, bar_bottom, children));
    crate::widgets::register(ui.ctx(), format!("menusub:{label}"), r.response.rect);
    if r.inner.is_some() {
        crate::menu_level::set_anchor(ui.ctx(), depth + 1, r.response.rect);
    }
}

/// Menu text: Inter at its regular weight, a touch larger and with a little more room between the
/// letters than the panels' text, so the rows read open and crisp like a native menu's rather than
/// condensed (egui draws Inter unhinted, and at 13 pt its default tracking runs letters together).
pub const MENU_TEXT_SIZE: f32 = 13.5;
pub const MENU_LETTER_SPACING: f32 = 0.3;
/// Room kept between an item's name and its shortcut.
const SHORTCUT_GAP: &str = "        ";

fn menu_text(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).size(MENU_TEXT_SIZE).extra_letter_spacing(MENU_LETTER_SPACING)
}

/// A native menu's row: about 27 pt tall, the text inset from the highlight's edges.
fn menu_text_style(ui: &mut egui::Ui) {
    ui.spacing_mut().button_padding = egui::vec2(10.0, 4.0);
}

/// `depth` is 1 for a top-level menu's rows; `bar_bottom` is where the menu bar ends.
pub(crate) fn nodes_ui(
    ui: &mut egui::Ui,
    nodes: &[MenuNode],
    mac: bool,
    clicked: &mut Option<(String, Value)>,
    depth: usize,
    bar_bottom: Option<f32>,
) {
    ui.set_min_width(220.0);
    menu_text_style(ui);
    for n in nodes {
        match n {
            MenuNode::Separator => {
                ui.separator();
            }
            MenuNode::Submenu { label, children } => {
                submenu(ui, label, menu_text(format!("      {}", crate::i18n::tr(label))), depth, bar_bottom, |ui| {
                    nodes_ui(ui, children, mac, clicked, depth + 1, bar_bottom)
                });
            }
            MenuNode::Item { id, params, label, shortcut, enabled, checked } => {
                // a gutter for check marks, like native menus
                let mut b = egui::Button::new(menu_text(format!("      {}", display_item_label(id, params, label))));
                if let Some(sc) = shortcut {
                    // a native menu's gap between the name and its key
                    b = b.shortcut_text(menu_text(format!("{SHORTCUT_GAP}{}", shortcut_text(sc, mac))));
                }
                let mut r = ui.add_enabled(*enabled, b);
                // a Classic item still to build: greyed, and says so on hover
                if crate::lrc_menus::is_coming_soon(id) {
                    r = r.on_disabled_hover_text(crate::i18n::tr(crate::lrc_menus::COMING_SOON_HINT));
                }
                crate::widgets::register(ui.ctx(), format!("menuitem:{id}"), r.rect);
                if *checked == Some(true) {
                    let t = crate::theme::Tokens::get(ui.ctx());
                    let c = egui::Rect::from_center_size(egui::pos2(r.rect.left() + 11.0, r.rect.center().y), egui::vec2(13.0, 13.0));
                    crate::icons::paint(ui.painter(), c, crate::icons::Icon::Check, if *enabled { t.text } else { t.text_disabled });
                }
                if r.clicked() {
                    *clicked = Some((id.clone(), params.clone()));
                    ui.close();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> LccApp {
        LccApp::new(lcc_engine::Session::with_demo(), Default::default())
    }

    /// Every menu fits a laptop screen without scrolling: no more top-level rows than Lightroom
    /// Classic's longest menus (File, Photo) plus the app's own, and submenus never open a
    /// third level.
    #[test]
    fn menus_stay_short_and_two_levels_deep() {
        // (Library ▸ View in Compare: Classic's rows, Compare's two, the app's six)
        const MAX_ROWS: usize = 38;
        let mut a = app();
        let check = |a: &LccApp| {
            for (title, items) in menu_bar(a) {
                let rows = items.iter().filter(|n| **n != MenuNode::Separator).count();
                assert!(rows <= MAX_ROWS, "{title} has {rows} top-level rows");
                for n in &items {
                    if let MenuNode::Submenu { label, children } = n {
                        assert!(!children.iter().any(|c| matches!(c, MenuNode::Submenu { .. })), "{title} ▸ {label} nests a submenu");
                    }
                }
            }
        };
        for m in [
            crate::module::ModuleId::Library,
            crate::module::ModuleId::Develop,
            crate::module::ModuleId::Map,
            crate::module::ModuleId::Book,
            crate::module::ModuleId::Slideshow,
            crate::module::ModuleId::Print,
            crate::module::ModuleId::Web,
        ] {
            a.ui.module = m;
            check(&a);
        }
        a.ui.module = crate::module::ModuleId::Library;
        a.ui.view = ViewMode::Compare;
        check(&a);
    }

    fn find<'a>(nodes: &'a [MenuNode], id: &str) -> Option<&'a MenuNode> {
        nodes.iter().find_map(|n| match n {
            MenuNode::Item { id: i, .. } if i == id => Some(n),
            MenuNode::Submenu { children, .. } => find(children, id),
            _ => None,
        })
    }

    /// View ▸ Zoom holds Space Zooms To: picking a choice checks it (and Zoom to Focus Point).
    #[test]
    fn space_zooms_to_lives_in_view_zoom() {
        let mut a = app();
        let zoom = |a: &LccApp| -> Vec<MenuNode> {
            let bar = menu_bar(a);
            let view = &bar.iter().find(|(t, _)| t == "View").expect("View menu").1;
            view.iter()
                .find_map(|n| match n {
                    MenuNode::Submenu { label, children } if label == "Zoom" => Some(children.clone()),
                    _ => None,
                })
                .expect("View ▸ Zoom")
        };
        let eye = |nodes: &[MenuNode]| {
            nodes.iter().find_map(|n| match n {
                MenuNode::Item { id, params, checked, .. } if id == "view.spaceZoomsTo" && params["to"] == "eye" => Some(*checked),
                _ => None,
            })
        };
        assert_eq!(eye(&zoom(&a)), Some(Some(false)));
        run_item(&mut a, "view.spaceZoomsTo", json!({"to": "eye"})).unwrap();
        assert_eq!(eye(&zoom(&a)), Some(Some(true)));
        assert!(a.ui.settings.zoom_to_focus);
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
            for (key, text) in [("Cmd+Shift+G", "⌘⇧G"), ("Ctrl+G", "⌃G"), ("F3", "F3")] {
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
        let hint = |a: &LccApp, id: &str| {
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
    fn library_menu_offers_the_hold_and_the_refresh_and_checks_the_hold_while_it_is_on() {
        let mut a = app();
        // (File ▸ Library Filters holds them, as in Classic)
        let view = |a: &LccApp| -> Vec<MenuNode> { menu_bar(a).into_iter().flat_map(|(_, v)| v).collect() };
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

    /// File's import group (after the catalog items, as in Classic) is worded as importing, not as
    /// adding a sidebar folder: Import Photos and Video… (⇧⌘I, the dialog that also takes
    /// folders), then the app's Import from Folder… and Import from Device ▸ in the same group.
    #[test]
    fn file_menu_import_group_is_worded_as_importing() {
        let bar = menu_bar(&app());
        let file = &bar.iter().find(|(t, _)| t == "File").expect("File menu").1;
        let labels: Vec<String> = file
            .iter()
            .map(|n| match n {
                MenuNode::Item { label, shortcut, .. } => format!("{label}{}", shortcut.as_deref().map(|s| format!(" [{s}]")).unwrap_or_default()),
                MenuNode::Submenu { label, .. } => format!("{label} ▸"),
                MenuNode::Separator => "---".into(),
            })
            .collect();
        let at = labels.iter().position(|l| l == "Import Photos and Video… [Cmd+Shift+I]").expect("Import Photos and Video…");
        assert_eq!(labels.get(at.saturating_sub(1)).map(String::as_str), Some("---"), "it opens a group");
        let group: Vec<&str> = labels[at..].iter().take_while(|l| *l != "---").map(String::as_str).collect();
        assert!(group.contains(&"Import from Folder…") && group.contains(&"Import from Device ▸"), "{group:?}");
        let all: Vec<MenuNode> = bar.iter().flat_map(|(_, v)| v.clone()).collect();
        let text = serde_json::to_string(&all).unwrap();
        assert!(!text.contains("Add Folder") && !text.contains("\"Add Photos"), "no add-folder wording left in the menus");
    }

    /// Back Up / Restore Library are the browser build's (its library lives in browser storage):
    /// absent from the desktop's menus, present and wired to the host where it provides them.
    #[test]
    fn library_backup_items_follow_the_host() {
        let all = |app: &LccApp| -> Vec<MenuNode> { menu_bar(app).into_iter().flat_map(|(_, v)| v).collect() };
        let mut desktop = app();
        assert!(find(&all(&desktop), "file.backupLibrary").is_none() && find(&all(&desktop), "file.restoreLibrary").is_none());
        assert!(run_item(&mut desktop, "file.backupLibrary", Value::Null).is_err(), "not available without the host");
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let c = calls.clone();
        let services = crate::Services {
            backup_library: Some(Box::new(move |s: &mut lcc_engine::Session| {
                c.set(c.get() + 1);
                Ok(json!({"photos": s.catalog.len()}))
            })),
            restore_library: Some(Box::new(|_: &mut lcc_engine::Session| Ok(json!({"started": true})))),
            ..Default::default()
        };
        let mut web = LccApp::new(lcc_engine::Session::with_demo(), services);
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
        for c in lcc_engine::command_specs().iter().filter(|c| !c.menu.is_empty() && !HIDDEN.contains(&c.id) && host_supports(&app, c.id)) {
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
        let listed: Vec<&str> = crate::lrc_menus::layout(crate::module::ModuleId::Library, "File")
            .iter()
            .filter_map(|s| match crate::lrc_menus::parse(s) {
                crate::lrc_menus::Slot::Item(id, ..) => Some(id),
                _ => None,
            })
            .collect();
        let unlisted: Vec<String> = crate::menus::menu_entries(&app)
            .into_iter()
            .filter(|e| {
                e.menu.first().map(String::as_str) == Some("File")
                    && !listed.contains(&e.id.as_str())
                    && !HIDDEN.contains(&e.id.as_str())
                    && host_supports(&app, &e.id)
            })
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
        assert!(find(children, "dialog.export").is_some(), "Custom… opens the dialog");
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
        let Some(MenuNode::Submenu { children, .. }) =
            edit.iter().find(|n| matches!(n, MenuNode::Submenu { label, .. } if label == "Select by Rating"))
        else {
            panic!("no Select by Rating submenu")
        };
        let Some(MenuNode::Item { id, params, .. }) = children.iter().rev().find(|n| matches!(n, MenuNode::Item { .. })) else { panic!() };
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

    /// #3311: Select by offers each rating on its own beside "and higher", and an exact row selects
    /// only the photos in view with that rating.
    #[test]
    fn select_by_offers_exact_ratings() {
        let mut app = app();
        let ids = app.session.visible().to_vec();
        for (i, id) in ids.iter().enumerate() {
            app.session.execute("library.select", &json!({"ids": [id]})).unwrap();
            app.session.execute("photo.rate", &json!({"rating": i % 6})).unwrap();
        }
        let bar = menu_bar(&app);
        let edit = &bar.iter().find(|(t, _)| t == "Edit").unwrap().1;
        let Some(MenuNode::Submenu { children, .. }) =
            edit.iter().find(|n| matches!(n, MenuNode::Submenu { label, .. } if label == "Select by Rating"))
        else {
            panic!("no Select by Rating submenu")
        };
        let exact = |r: u8| {
            children.iter().find_map(|n| match n {
                MenuNode::Item { id, params, .. } if id == "library.selectBy" && *params == json!({"rating": r, "ratingOp": "eq"}) => {
                    Some(params.clone())
                }
                _ => None,
            })
        };
        for r in 1..=5 {
            assert!(exact(r).is_some(), "no row for exactly {r} stars");
        }
        let visible = app.session.visible().to_vec();
        let want: Vec<_> = visible.iter().copied().filter(|&p| app.session.catalog.photo(p).unwrap().rating == 3).collect();
        assert!(!want.is_empty() && want.len() < visible.len(), "the demo holds photos rated 3 and others");
        run_item(&mut app, "library.selectBy", exact(3).unwrap()).unwrap();
        assert_eq!(app.session.selection.ids, want);
    }

    #[test]
    fn edit_watermarks_opens_dialog_and_saves_preset() {
        let mut app = app();
        let bar = menu_bar(&app);
        let edit = &bar.iter().find(|(t, _)| t == "Edit").expect("Edit menu").1;
        assert!(find(edit, "dialog.watermark").is_some(), "dialog.watermark in Edit menu");

        run_item(&mut app, "dialog.watermark", Value::Null).unwrap();
        let Some(crate::state::Dialog::Watermark { wm, preset_name }) = &mut app.ui.dialog else {
            panic!("expected Watermark dialog");
        };
        assert_eq!(wm.text, "");

        wm.text = "© Test Watermark".to_string();
        *preset_name = "Copyright Preset".to_string();

        let d = app.ui.dialog.take().unwrap();
        crate::panels::dialogs::confirm_dialog(&mut app, &d).unwrap();

        let presets = app.session.all_export_presets();
        let saved = presets.iter().find(|(p, _)| p.name == "Copyright Preset");
        assert!(saved.is_some(), "preset saved");
        let o = lcc_engine::export::ExportOptions::from_json(&saved.unwrap().0.params);
        assert_eq!(o.watermark.map(|w| w.text).as_deref(), Some("© Test Watermark"));
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
        let help = |app: &LccApp| menu_bar(app).into_iter().find(|(t, _)| t == "Help").map(|(_, items)| items).unwrap_or_default();
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
        let log = format!("/home/a/.config/app/logs/{}.log", lcc_brand::BINARY);
        let log = log.as_str();
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
        let dir = lcc_testkit::TempDir::new("lc-ui-missing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::create_dir_all(dir.join("b")).unwrap();
        let img = lcc_raster::Rgba8::from_fn(16, 12, |x, y| [(x * 9) as u8, (y * 11) as u8, 50, 255]);
        let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
        std::fs::write(dir.join("a/one.png"), lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();
        let mut app = LccApp::new(lcc_engine::Session::new().with_fs(), Default::default());
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
    // 8 px apart, too close to read or aim at. (The fork's longer bar narrows the gap down to
    // MIN_TITLE_GAP when the window is tight, never below.)
    #[test]
    fn in_window_menu_headings_are_spaced_apart() {
        let mut a = app();
        a.native_menu = false;
        let mut h = crate::headless::Headless::new(a, [1400.0, 900.0], 1.0);
        h.step();
        h.step();
        let rect = |id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("{id} is on screen"));
        let titles: Vec<_> = menus(&h.app).iter().map(|t| rect(&format!("menu:{t}"))).collect();
        for pair in titles.windows(2) {
            let gap = pair[1].left() - pair[0].right();
            // (the registered rects are the titles' highlights, TITLE_PAD wider than the text)
            assert!(gap >= MIN_TITLE_GAP - 2.0 * TITLE_PAD - 0.5, "headings {:?} and {:?} are only {gap} px apart", pair[0], pair[1]);
        }
    }

    /// A backup the host refuses (changes still unsaved, issue #1508) says why when started from
    /// the menu, as a refused export does; the refusal used to reach only the control channel.
    #[test]
    fn a_refused_backup_from_the_menu_shows_why() {
        // the host refuses, as the web one does while changes are still unsaved (issue #1508)
        let refusal = "Backup not started: 1 change could not be saved to browser storage yet";
        let services =
            crate::Services { backup_library: Some(Box::new(move |_: &mut lcc_engine::Session| Err(refusal.to_string()))), ..Default::default() };
        let app = LccApp::new(lcc_engine::Session::with_demo(), services);
        let mut h = crate::headless::Headless::new(app, [1400.0, 900.0], 1.0);
        let t = std::time::Duration::from_secs(30);
        h.settle(t);
        // (Back Up Catalog… sits in File's catalog group)
        for id in ["menu:File", "menuitem:file.backupLibrary"] {
            let r = h.request("ui.clickWidget", json!({"id": id}), t);
            assert_eq!(r["ok"], true, "{id}: {r}");
            h.step();
        }
        // (read at once: a toast keeps the UI repainting until it expires, so settling would outlive it)
        let toast = h.app.ui.toast().map(|t| t.0.clone()).unwrap_or_default();
        assert!(toast.contains(refusal), "the refusal is shown: {toast:?}");
    }

    /// #1528: `file.findMissing` with `wait: true` must propagate folder validation and search
    /// failures as errors instead of returning `Ok(null)`.
    #[test]
    fn find_missing_waited_invalid_folder_returns_error_with_zero_mutations() {
        let dir = lcc_testkit::TempDir::new("lc-ui-missing-err");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dummy = dir.join("dummy.png");
        let img = lcc_raster::Rgba8::from_fn(8, 8, |_, _| [10, 20, 30, 255]);
        let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
        std::fs::write(&dummy, lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();

        let mut app = LccApp::new(lcc_engine::Session::new().with_fs(), Default::default());
        app.session.execute("library.import", &json!({"paths": [dummy.to_string_lossy()]})).unwrap();
        // remove the file on disk so the photo is missing
        std::fs::remove_file(&dummy).unwrap();
        let id = app.session.catalog.photos().next().unwrap().id;
        let original_path = match &app.session.catalog.photo(id).unwrap().source {
            lcc_catalog::Source::File { path } => path.clone(),
            _ => panic!("expected file source"),
        };
        let undo_count_before = app.session.undo.len();

        // 1. Point at a nonexistent directory
        let bad_dir = dir.join("nonexistent_subdir");
        let err = run_item(&mut app, "file.findMissing", json!({"folder": bad_dir.to_string_lossy(), "wait": true})).unwrap_err();
        assert!(err.contains("not a folder"), "expected 'not a folder' error, got: {err}");
        assert!(app.ui.toast().map(|t| t.0.contains("not a folder")).unwrap_or(false), "toast should reflect failure");
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
            lcc_catalog::Source::File { path } => path.clone(),
            _ => panic!("expected file source"),
        };
        assert_eq!(path_after, original_path, "missing photo path must not change on failure");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #1528: `file.findMissing` with `wait: true` returns structured results on success,
    /// and background mode (`wait: false` or omitted) continues returning `{"background": true}`.
    #[test]
    fn find_missing_waited_structured_results_and_background_mode() {
        let dir = lcc_testkit::TempDir::new("lc-ui-missing-bg");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("orig")).unwrap();
        std::fs::create_dir_all(dir.join("found")).unwrap();
        let img = lcc_raster::Rgba8::from_fn(8, 8, |_, _| [10, 20, 30, 255]);
        let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
        std::fs::write(dir.join("orig/pic.png"), lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();

        let mut app = LccApp::new(lcc_engine::Session::new().with_fs(), Default::default());
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
        let dir = lcc_testkit::TempDir::new("lc-ui-missing-to");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut app = LccApp::new(lcc_engine::Session::new().with_fs(), Default::default());
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
        let dir = lcc_testkit::TempDir::new("lc-ui-missing-multi");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("orig")).unwrap();
        std::fs::create_dir_all(dir.join("found")).unwrap();
        let img = lcc_raster::Rgba8::from_fn(8, 8, |_, _| [10, 20, 30, 255]);
        let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
        std::fs::write(dir.join("orig/pic.png"), lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();

        let mut app = LccApp::new(lcc_engine::Session::new().with_fs(), Default::default());
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
        let dir = lcc_testkit::TempDir::new("lc-ui-tracklog");
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
        // in the Metadata menu; without a file dialog (headless) it is disabled
        let photo_menu = menu_bar(&app).into_iter().find(|(t, _)| t == "Metadata").unwrap().1;
        let Some(MenuNode::Item { enabled, .. }) = find(&photo_menu, "photo.tagFromTracklog") else { panic!("not in the Metadata menu") };
        assert!(!enabled);
        // a file without an offset asks for the camera's time zone; confirming runs the tagging
        run_item(&mut app, "photo.tagFromTracklog", json!({"path": gpx.to_string_lossy()})).unwrap();
        let Some(crate::state::Dialog::TextPrompt { value, .. }) = &mut app.ui.dialog else { panic!("no time-zone prompt") };
        *value = " +02:00 ".into();
        let d = app.ui.dialog.take().unwrap();
        let r = crate::panels::dialogs::confirm_dialog(&mut app, &d).unwrap();
        assert_eq!(r["tagged"], 1, "{r}");
        let p = app.session.catalog.photo(lcc_engine::catalog::PhotoId(id)).unwrap();
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

    #[test]
    fn shortcut_text_per_platform() {
        assert_eq!(shortcut_text("Cmd+Shift+Z", true), "⌘⇧Z");
        assert_eq!(shortcut_text("Cmd+Shift+Z", false), "Ctrl+Shift+Z");
        assert_eq!(shortcut_text("Delete", true), "⌫");
    }
}
