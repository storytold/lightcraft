//! The menu bar model: File, Edit, View, Photo, Window, Help, generated from the command registry
//! (engine commands with menu paths + [`crate::menus::ui_commands`]) with live labels, shortcuts,
//! enabled and checked state.
//!
//! One model drives every menu surface: the native macOS menu bar (built by the desktop host), the
//! in-window menu bar (web, Windows, Linux — [`show_in_window`]) and the control channel
//! (`ui.menu.tree`). Items are a command id plus parameters, run through [`run_item`] so they
//! behave exactly like their keyboard shortcuts.

use serde::Serialize;
use serde_json::{Value, json};

use crate::LightcraftApp;
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
    if matches!(id, "metadata.applyPreset" | "album.addPhotos" | "label.applySet")
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

/// Top-level menus in order (the macOS app menu is added by the host).
pub const MENUS: &[&str] = &["File", "Edit", "View", "Photo", "Window", "Help"];

/// Order and grouping per menu: command ids, `---` separators and `@Submenu` placeholders.
/// Entries with a menu path that aren't listed are appended at the end of their menu, before any
/// [`LAST`] item.
const LAYOUT: &[(&str, &[&str])] = &[
    (
        "File",
        &[
            "file.addPhotos",
            "file.addFolder",
            "@Import from Device",
            "---",
            "app.newLibrary",
            "app.openLibrary",
            "@Open Recent Library",
            "file.backupLibrary",
            "file.restoreLibrary",
            "---",
            "dialog.newAlbum",
            "dialog.newFolder",
            "dialog.smartAlbum",
            "dialog.newSmartAlbum",
            "---",
            "file.importPresets",
            "file.exportPresets",
            "file.importKeywords",
            "file.exportKeywords",
            "---",
            "dialog.export",
            "app.exportPrevious",
            "@Export with Preset",
            "---",
            "library.toggleAutoWriteXmp",
            "@Previews",
            "---",
            "app.quit",
        ],
    ),
    (
        "Edit",
        &[
            "edit.undo",
            "edit.redo",
            "---",
            "develop.copy",
            "dialog.copySettings",
            "develop.paste",
            "dialog.pasteSettings",
            "develop.sync",
            "develop.autoSync",
            "---",
            "library.selectAll",
            "library.selectNone",
            "@Select by",
            "---",
            "view.focusSearch",
            "---",
            "app.settings",
        ],
    ),
    (
        "View",
        &[
            "view.photoGrid",
            "view.squareGrid",
            "@Grid Info",
            "view.thumbSizeUp",
            "view.thumbSizeDown",
            "view.detail",
            "view.compare",
            "view.survey",
            "view.people",
            "view.map",
            "---",
            "view.leftPanel",
            "@Sidebar Sections",
            "view.photoCounts",
            "view.faceBoxes",
            "view.focusPoint",
            "view.zoomToFocus",
            "view.filmstrip",
            "view.histogram",
            "view.navigator",
            "view.infoOverlay",
            "@Loupe Overlay",
            "@Appearance",
            "---",
            "view.fullScreenPreview",
            "view.lightsOut",
            "view.enterFullScreen",
            "---",
            "view.showOriginal",
            "view.beforeAfter",
            "view.beforeAfterSplit",
            "@Before/After Settings",
            "---",
            "view.zoomIn",
            "view.zoomOut",
            "view.zoomToggle",
            "view.zoomFit",
            "view.zoom100",
            "---",
            "view.clipping",
            "view.visualizeHdr",
            "view.softProof",
            "view.maskOverlay",
            "view.maskOverlayAll",
            "view.maskOverlayMode",
            "view.maskOverlayColor",
            "view.maskPins",
            "@Tool Overlay",
            "view.cropOverlay",
            "---",
            "@Sort",
            "@Stacks",
            "view.filterBar",
            "library.toggleFilters",
            "library.clearFilter",
            "library.refresh",
            "library.holdView",
            "---",
            "compare.swap",
            "compare.makeSelect",
            "---",
            "view.viewOptions",
        ],
    ),
    (
        "Photo",
        &[
            "@Add to Album",
            "---",
            "@Set Rating",
            "@Set Flag",
            "@Set Color Label",
            "@Metadata Preset",
            "view.autoAdvance",
            "---",
            "photo.rotateLeft",
            "photo.rotateRight",
            "photo.flipHorizontal",
            "photo.flipVertical",
            "---",
            "version.create",
            "photo.virtualCopy",
            "@Stack",
            "@Photo Merge",
            "---",
            "develop.auto",
            "develop.treatment",
            "develop.reset",
            "develop.updateProcess",
            "dialog.createPreset",
            "---",
            "photo.saveMetadataToFile",
            "photo.readMetadataFromFile",
            "photo.reload",
            "photo.readFocusPoints",
            "app.showInFinder",
            "app.showInFolders",
            "dialog.rename",
            "dialog.captureTime",
            "photo.tagFromTracklog",
            "---",
            "album.removePhotos",
            "photo.delete",
            "photo.restore",
            "photo.deletePermanently",
        ],
    ),
    (
        "Window",
        &[
            "panel.edit",
            "panel.crop",
            "panel.remove",
            "panel.masking",
            "panel.redeye",
            "---",
            "panel.presets",
            "panel.versions",
            "panel.activity",
            "panel.info",
            "panel.keywords",
            "---",
            "@Edit Sections",
            "@Tools",
        ],
    ),
    (
        "Help",
        &[
            "app.discord",
            "app.feedback",
            "---",
            "app.website",
            "app.github",
            "app.artcraft",
            "---",
            "app.whatsNew",
            "app.shortcuts",
            "app.systemInfo",
            "app.openLogFolder",
            "---",
            "app.checkForUpdates",
            "app.about",
        ],
    ),
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
    "library.sort",
    "library.shuffle",
    "album.addPhotos",
    "album.create",
    "library.import",
    "preset.create",
];

/// The selection includes a photo in Recently Deleted.
pub(crate) fn selection_deleted(app: &LightcraftApp) -> bool {
    let s = &app.session;
    s.selection.ids.iter().copied().chain(s.selection.active).any(|id| s.catalog.photo(id).is_some_and(|p| p.deleted))
}

/// Items only some hosts have are left out of the others' menus (the web build's library backup);
/// Restore and Delete Permanently replace Delete for photos in Recently Deleted.
fn host_supports(app: &LightcraftApp, id: &str) -> bool {
    match id {
        "file.backupLibrary" => app.services.backup_library.is_some(),
        "file.restoreLibrary" => app.services.restore_library.is_some(),
        "photo.restore" | "photo.deletePermanently" => selection_deleted(app),
        "photo.delete" => !selection_deleted(app),
        // only in an album that holds its photos (not a smart one)
        "album.removePhotos" => in_normal_album(app) && !selection_deleted(app),
        // only where the trash is on screen
        "library.emptyRecentlyDeleted" => app.session.source == lightcraft_engine::LibrarySource::RecentlyDeleted,
        _ => true,
    }
}

/// The library shows an album that holds its photos (not a smart album, not a folder of albums).
pub(crate) fn in_normal_album(app: &LightcraftApp) -> bool {
    match app.session.source {
        lightcraft_engine::LibrarySource::Album(a) => app.session.catalog.album(a).is_some_and(|a| !a.folder && !a.is_smart()),
        _ => false,
    }
}

fn item(id: &str, params: Value, label: impl Into<String>, shortcut: Option<&str>, enabled: bool, checked: Option<bool>) -> MenuNode {
    MenuNode::Item { id: id.into(), params, label: label.into(), shortcut: shortcut.map(str::to_string), enabled, checked }
}

/// Checked state of toggles and radio items.
pub fn checked(app: &LightcraftApp, id: &str) -> Option<bool> {
    let u = &app.ui;
    let panel = |p: RightPanel| Some(u.right == p);
    match id {
        // Every language's command is checked when it is the active one.
        _ if crate::menus::language_from_command(id).is_some() => Some(crate::menus::language_from_command(id) == Some(u.language)),
        "develop.autoSync" => Some(app.session.auto_sync),
        "view.photoCounts" => Some(u.show_counts),
        "view.secondWindow" => Some(u.second_window),
        "view.photoGrid" => Some(u.view == ViewMode::PhotoGrid),
        "view.squareGrid" => Some(u.view == ViewMode::SquareGrid),
        "view.detail" => Some(u.view == ViewMode::Detail),
        "view.compare" => Some(u.view == ViewMode::Compare),
        "view.survey" => Some(u.view == ViewMode::Survey),
        "view.people" => Some(u.view == ViewMode::People),
        "view.map" => Some(u.view == ViewMode::Map),
        "view.reference" => Some(u.view == ViewMode::Reference),
        "view.leftPanel" => Some(u.left_panel),
        "view.faceBoxes" => Some(u.face_boxes),
        "view.focusPoint" => Some(u.settings.focus_point),
        "view.zoomToFocus" => Some(u.settings.zoom_to_focus),
        "view.filmstrip" => Some(u.filmstrip),
        "view.frameRate" => Some(u.frame_rate),
        "view.histogram" => Some(u.histogram),
        "view.clipping" => Some(u.show_clipping),
        "view.visualizeHdr" => Some(u.hdr_visualize),
        "view.softProof" => Some(u.soft_proof),
        "view.maskOverlay" => Some(u.mask_overlay),
        "view.maskOverlayAll" => Some(u.mask_overlay_all),
        "view.maskPins" => Some(u.mask_pins),
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
        "view.fullScreenPreview" => Some(u.fullscreen),
        "view.lightsOut" => Some(u.lights_out != crate::state::LightsOut::On),
        "view.enterFullScreen" => Some(app.window_is_fullscreen),
        "panel.edit" => panel(RightPanel::Edit),
        "panel.profiles" => panel(RightPanel::Profiles),
        "panel.crop" => panel(RightPanel::Crop),
        "panel.remove" => panel(RightPanel::Remove),
        "panel.masking" => panel(RightPanel::Masking),
        "panel.redeye" => panel(RightPanel::RedEye),
        "panel.versions" => panel(RightPanel::Versions),
        "panel.activity" => panel(RightPanel::Activity),
        "panel.info" => panel(RightPanel::Info),
        "panel.keywords" => panel(RightPanel::Keywords),
        "panel.presets" => Some(u.presets),
        "library.toggleAutoWriteXmp" => Some(app.session.xmp.auto_write),
        _ => None,
    }
}

/// Labels that follow the state ("Undo Exposure", "Delete 3 Photos").
fn live_label(app: &LightcraftApp, id: &str, label: &str) -> String {
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
        "app.showInFinder" => crate::i18n::tr(crate::menus::reveal_label()).to_string(),
        _ => label.to_string(),
    }
}

/// The parameterized submenus.
pub(crate) fn expanded(app: &LightcraftApp, name: &str) -> Option<Vec<MenuNode>> {
    let active = app.session.active().and_then(|id| app.session.catalog.photo(id).cloned());
    let has = active.is_some();
    Some(match name {
        "Previews" => {
            let running = app.session.preview_build.as_ref().is_some_and(|b| !b.finished.load(std::sync::atomic::Ordering::Relaxed));
            let scope = if app.session.selection.ids.len() > 1 { "Selected" } else { "Visible" };
            vec![
                item(
                    "library.buildPreviews",
                    json!({"size": "standard", "edge": app.ui.settings.standard_preview_edge()}),
                    crate::i18n::tr_format!("Build Standard-Sized Previews ({scope})", scope = crate::i18n::tr(scope)),
                    None,
                    !running,
                    None,
                ),
                item(
                    "library.buildPreviews",
                    json!({"size": "full"}),
                    crate::i18n::tr_format!("Build 1:1 Previews ({scope})", scope = crate::i18n::tr(scope)),
                    None,
                    !running,
                    None,
                ),
                item("library.cancelPreviews", Value::Null, "Stop Building Previews", None, running, None),
                MenuNode::Separator,
                // (read and written on a worker thread: the originals may be on a slow drive)
                item(
                    "library.smartPreviews",
                    json!({"background": true}),
                    crate::i18n::tr_format!("Build Smart Previews ({scope})", scope = crate::i18n::tr(scope)),
                    None,
                    app.session.media.smart_dir.is_some() && !running,
                    None,
                ),
                item(
                    "library.smartPreviews",
                    json!({"discard": true, "background": true}),
                    crate::i18n::tr_format!("Discard Smart Previews ({scope})", scope = crate::i18n::tr(scope)),
                    None,
                    app.session.media.smart_dir.is_some() && !running,
                    None,
                ),
                MenuNode::Separator,
                item("library.clearPreviews", Value::Null, "Discard Preview Cache", None, true, None),
            ]
        }
        "Set Rating" => (0..=5u8)
            .map(|r| {
                let label = if r == 0 { "None".to_string() } else { "★".repeat(r as usize) };
                let sc = ["0", "1", "2", "3", "4", "5"][r as usize];
                item("photo.rate", json!({"rating": r}), label, Some(sc), has, Some(active.as_ref().is_some_and(|p| p.rating == r)))
            })
            .collect(),
        "Set Flag" => [
            ("pick", "Pick", "P", lightcraft_catalog::Flag::Pick),
            ("reject", "Reject", "X", lightcraft_catalog::Flag::Reject),
            ("none", "Unflagged", "U", lightcraft_catalog::Flag::None),
        ]
        .into_iter()
        .map(|(f, label, sc, flag)| {
            item("photo.flag", json!({"flag": f}), label, Some(sc), has, Some(active.as_ref().is_some_and(|p| p.flag == flag)))
        })
        .collect(),
        "Set Color Label" => {
            let mut v: Vec<MenuNode> = lightcraft_catalog::ColorLabel::ALL
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
            v.push(item("photo.label", json!({"label": "none"}), "None", None, has, Some(active.as_ref().is_some_and(|p| p.label.is_none()))));
            v.push(MenuNode::Separator);
            // label sets: each a checkable item; Edit… names them
            let sets = lightcraft_engine::cmd::manage::label_sets_json(&app.session);
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
        "Grid Info" => ["filename", "exposure", "date"]
            .into_iter()
            .zip(["File Name", "Exposure (shutter · aperture · ISO)", "Capture Date"])
            .map(|(k, label)| item("view.gridInfo", json!({"info": k}), label, None, true, Some(app.ui.grid_info == k)))
            .collect(),
        "Sort" => {
            use lightcraft_catalog::SortKey::*;
            let cur = app.session.sort;
            let mut v: Vec<MenuNode> = [
                ("Capture Date", CaptureDate, "captureDate"),
                ("Import Date", ImportDate, "importDate"),
                ("Modified Date", EditDate, "editDate"),
                ("File Name", FileName, "fileName"),
                ("Rating", Rating, "rating"),
                ("File Size", FileSize, "fileSize"),
                ("Random", Random, "random"),
            ]
            .into_iter()
            .map(|(label, key, k)| item("library.sort", json!({"key": k}), label, None, true, Some(cur.key == key)))
            .collect();
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
            use lightcraft_catalog::GroupBy;
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
        "Open Recent Library" => {
            let can = crate::panels::settings::can_switch_library(app).is_ok();
            let open = app.session.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone());
            let recent = &app.ui.settings.recent_libraries;
            let label = |r: &str| {
                let d = std::path::Path::new(r);
                format!("{} — {}", crate::panels::settings::library_name(d), d.parent().map(|p| p.display().to_string()).unwrap_or_default())
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
                v.push(item("app.clearRecentLibraries", Value::Null, "No Recent Libraries", None, false, None));
            } else {
                v.push(MenuNode::Separator);
                // one entry at a time, missing ones too: only the list forgets it (issue #1290)
                let forget = recent.iter().map(|r| item("app.recentLibraries", json!({"remove": r}), label(r), None, true, None)).collect();
                v.push(MenuNode::Submenu { label: "Remove from List".into(), children: forget });
                v.push(item("app.clearRecentLibraries", Value::Null, "Clear Recent Libraries", None, true, None));
            }
            v
        }
        "Import from Device" => {
            let devices = lightcraft_engine::devices::devices();
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
        "Select by" => {
            let mut v: Vec<MenuNode> = [("pick", "Picks"), ("reject", "Rejects"), ("none", "Unflagged")]
                .into_iter()
                .map(|(f, label)| item("library.selectBy", json!({"flag": f}), label, None, true, None))
                .collect();
            v.push(MenuNode::Separator);
            v.extend((1..=5u8).map(|r| {
                item("library.selectBy", json!({"rating": r}), crate::i18n::tr_format!("{} and higher", "★".repeat(r as usize)), None, true, None)
            }));
            v.push(item("library.selectBy", json!({"rating": 0, "ratingOp": "eq"}), "Unrated", None, true, None));
            v.push(MenuNode::Separator);
            v.extend(lightcraft_catalog::ColorLabel::ALL.iter().map(|l| {
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
        "Add to Album" => {
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
            v.push(item("dialog.newAlbum", Value::Null, "New Album…", None, !local_only, None));
            v
        }
        _ => return None,
    })
}

/// Resolve a live menu row, including contextual keys and borrowed grid-cycle hints.
fn node(app: &LightcraftApp, e: &MenuEntry) -> MenuNode {
    // Shift+P picks and advances in Library; don't advertise it on Presets in those views.
    let shortcut = if e.id == "panel.presets" && crate::shortcuts::library_grid(app) { None } else { e.shortcut.as_deref() };
    // in an album the Delete key removes from the album (issue #1805): the menu says so
    let delete_key = crate::shortcuts::shortcut_of(&app.ui.settings.keymap, "photo.delete");
    let shortcut = match e.id.as_str() {
        "view.photoGrid" | "view.squareGrid" => shortcut.or_else(|| crate::shortcuts::shortcut_of(&app.ui.settings.keymap, "view.gridToggle")),
        "album.removePhotos" => delete_key,
        "photo.delete" if matches!(app.session.source, lightcraft_engine::LibrarySource::Album(_)) => None,
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

/// The whole menu bar: (title, items) per menu in [`MENUS`] order.
pub fn menu_bar(app: &LightcraftApp) -> Vec<(String, Vec<MenuNode>)> {
    let entries: Vec<MenuEntry> =
        crate::menus::menu_entries(app).into_iter().filter(|e| !HIDDEN.contains(&e.id.as_str()) && host_supports(app, &e.id)).collect();
    let mut used = vec![false; entries.len()];
    let mut bar = Vec::new();
    for title in MENUS {
        let layout = LAYOUT.iter().find(|(t, _)| t == title).map(|(_, l)| *l).unwrap_or(&[]);
        let mut items = Vec::new();
        let mut last = Vec::new();
        let sub = |name: &str, used: &mut [bool]| -> Vec<MenuNode> {
            let mut children = expanded(app, name).unwrap_or_default();
            let mut dialogs = Vec::new();
            for (i, e) in entries.iter().enumerate() {
                if !used[i] && e.menu.len() == 2 && e.menu[0] == *title && e.menu[1] == name {
                    used[i] = true;
                    // commands that open a dialog go last, after a separator
                    if e.label.ends_with('…') { dialogs.push(node(app, e)) } else { children.push(node(app, e)) }
                }
            }
            if !dialogs.is_empty() {
                children.push(MenuNode::Separator);
                children.extend(dialogs);
            }
            children
        };
        for slot in layout {
            if *slot == "---" {
                items.push(MenuNode::Separator);
            } else if let Some(name) = slot.strip_prefix('@') {
                let children = sub(name, &mut used);
                items.push(MenuNode::Submenu { label: name.to_string(), children });
            } else if let Some(i) = entries.iter().position(|e| e.id == *slot && e.menu.first().map(String::as_str) == Some(title)) {
                used[i] = true;
                if LAST.contains(slot) { last.push(node(app, &entries[i])) } else { items.push(node(app, &entries[i])) }
            }
        }
        // everything else that names this menu
        items.push(MenuNode::Separator);
        let mut extra_subs: Vec<String> = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            if used[i] || e.menu.first().map(String::as_str) != Some(title) {
                continue;
            }
            if e.menu.len() == 1 {
                used[i] = true;
                items.push(node(app, e));
            } else if !extra_subs.contains(&e.menu[1]) {
                extra_subs.push(e.menu[1].clone());
            }
        }
        for name in extra_subs {
            let children = sub(&name, &mut used);
            items.push(MenuNode::Submenu { label: name, children });
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
pub fn run_item(app: &mut LightcraftApp, id: &str, params: Value) -> Result<Value, String> {
    let mut params = if params.is_null() { json!({}) } else { params };
    let culling_cmd = matches!(
        id,
        "photo.rate"
            | "photo.flag"
            | "photo.label"
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
    if id == "photo.delete" && crate::menus::confirm_delete(app) {
        return Ok(Value::Null);
    }
    let was = app.session.active();
    let r = app.run(id, params);
    if culling_cmd && r.is_ok() && app.ui.auto_advance {
        crate::panels::compare::advance(app, was);
    }
    r
}

/// What the native menu bar may do with one command it queued.
///
/// AppKit delivers menu accelerators and clicks straight to the host, so they never pass through
/// [`crate::shortcuts::handle`]. The explicit policy below is the equivalent guard for that path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeCommand {
    /// Run the command through the ordinary menu path.
    Run,
    /// Quit stays available: the host still runs the existing tasks/unsaved-changes confirmation.
    Quit,
    /// A quit/settings notice or the blocking library window owns input: drop the command.
    Blocked,
}

/// The safety policy for commands queued by the native menu bar.
///
/// While a prompt or the blocking library window owns input, no queued command may act on the
/// library behind it — including Settings… and About, which would otherwise open their dialog
/// behind the blocking window. Quit is the one exception: the window itself offers Quit, and the
/// host's existing confirmation ([`crate::panels::notices::may_close`]) still decides whether the
/// process may end.
pub fn native_command_policy(app: &LightcraftApp, id: &str) -> NativeCommand {
    if id == "app.quit" {
        return NativeCommand::Quit;
    }
    if crate::panels::notices::blocks_input(app) || crate::panels::library_problem::blocks_dialog_input(app) {
        return NativeCommand::Blocked;
    }
    NativeCommand::Run
}

/// Run one command resolved from a native menu event under [`native_command_policy`].
///
/// The host calls this for every queued native command, so a macOS accelerator, click or About/
/// Settings item respects the notice and blocking-library barriers. Ordinary dialogs retain
/// their existing native-menu behavior.
pub fn run_native_command(app: &mut LightcraftApp, id: &str, params: Value) -> NativeCommand {
    let policy = native_command_policy(app, id);
    match policy {
        NativeCommand::Run => {
            let _ = run_item(app, id, params);
        }
        NativeCommand::Blocked => log::debug!("native menu: {id} is blocked while another window owns input"),
        NativeCommand::Quit => {}
    }
    policy
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

/// Horizontal gap between in-window menu titles. The buttons are frameless, and egui gives
/// frameless buttons no padding, so the gap has to come from the layout's item spacing.
const TITLE_GAP: f32 = 24.0;

/// Width of the in-window menu bar's titles.
pub fn bar_width(ui: &egui::Ui) -> f32 {
    let t = crate::theme::Tokens::get(ui.ctx());
    MENUS.iter().map(|m| ui.painter().layout_no_wrap(crate::i18n::tr(m).to_string(), t.font(13.0), t.text).size().x + TITLE_GAP).sum::<f32>()
}

/// The in-window menu bar (hosts without a native one): one dropdown per menu, or a single
/// "Menu" button when the space is too narrow. Returns the width used.
pub fn show_in_window(app: &mut LightcraftApp, ui: &mut egui::Ui, max_width: f32) -> f32 {
    let t = crate::theme::Tokens::get(ui.ctx());
    let bar = menu_bar(app);
    let font = t.font(13.0);
    let widths: Vec<f32> = bar
        .iter()
        .map(|(title, _)| ui.painter().layout_no_wrap(crate::i18n::tr(title).to_string(), font.clone(), t.text).size().x + TITLE_GAP)
        .collect();
    let total: f32 = widths.iter().sum();
    let mut clicked: Option<(String, Value)> = None;
    let start = ui.cursor().left();
    let mac = ui.ctx().os() == egui::os::OperatingSystem::Mac;
    // Menus hang below the whole top bar and never grow past the window (#189): a tall menu
    // scrolls instead of egui sliding it up over the titles.
    let bar_bottom = Some(ui.max_rect().bottom());
    if total <= max_width {
        let saved = ui.spacing().item_spacing.x;
        ui.spacing_mut().item_spacing.x = TITLE_GAP;
        // All titles first, then their popups, so a title the pointer moves onto can take over
        // from the open menu before either is drawn (no frame with two menus or none).
        let titles: Vec<egui::Response> = bar
            .iter()
            .map(|(title, _)| {
                let r = ui.add(egui::Button::new(egui::RichText::new(crate::i18n::tr(title)).font(font.clone()).color(t.text_label)).frame(false));
                crate::widgets::register(ui, format!("menu:{title}"), r.rect);
                r
            })
            .collect();
        let confirmed = switch_open_title(ui.ctx(), &titles);
        for ((_, items), r) in bar.iter().zip(&titles) {
            // (opening and closing by click is done in `switch_open_title`)
            // (the click confirming a menu a hover opened must not close it again)
            let close = if confirmed == Some(egui::Popup::default_response_id(r)) {
                egui::PopupCloseBehavior::IgnoreClicks
            } else {
                egui::PopupCloseBehavior::CloseOnClick
            };
            egui::Popup::menu(r)
                .close_behavior(close)
                .open_memory(None)
                .show(|ui| crate::menu_level::level(ui, 1, bar_bottom, |ui| nodes_ui(ui, items, mac, &mut clicked, 1, bar_bottom)));
        }
        ui.spacing_mut().item_spacing.x = saved;
    } else {
        let r = ui.add(egui::Button::new(egui::RichText::new(crate::i18n::tr("Menu")).font(font.clone()).color(t.text_label)).frame(false));
        crate::widgets::register(ui, "menu:all", r.rect);
        egui::Popup::menu(&r).show(|ui| {
            crate::menu_level::level(ui, 1, bar_bottom, |ui| {
                for (title, items) in &bar {
                    submenu(ui, title, crate::i18n::tr(title).to_string(), 1, bar_bottom, |ui| nodes_ui(ui, items, mac, &mut clicked, 2, bar_bottom));
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
    let key = egui::Id::unique("lc-menubar-hover-opened");
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
fn submenu(ui: &mut egui::Ui, label: &str, text: String, depth: usize, bar_bottom: Option<f32>, children: impl FnOnce(&mut egui::Ui)) {
    let r = crate::widgets::menu_button(ui, text, |ui| crate::menu_level::level(ui, depth + 1, bar_bottom, children));
    crate::widgets::register(ui, format!("menusub:{label}"), r.response.rect);
    if r.inner.is_some() {
        crate::menu_level::set_anchor(ui.ctx(), depth + 1, r.response.rect);
    }
}

/// `depth` is 1 for a top-level menu's rows; `bar_bottom` is where the menu bar ends.
fn nodes_ui(ui: &mut egui::Ui, nodes: &[MenuNode], mac: bool, clicked: &mut Option<(String, Value)>, depth: usize, bar_bottom: Option<f32>) {
    ui.set_min_width(220.0);
    for n in nodes {
        match n {
            MenuNode::Separator => {
                ui.separator();
            }
            MenuNode::Submenu { label, children } => {
                submenu(ui, label, format!("      {}", crate::i18n::tr(label)), depth, bar_bottom, |ui| {
                    nodes_ui(ui, children, mac, clicked, depth + 1, bar_bottom)
                });
            }
            MenuNode::Item { id, params, label, shortcut, enabled, checked } => {
                // a gutter for check marks, like native menus
                let mut b = egui::Button::new(format!("      {}", display_item_label(id, params, label)));
                if let Some(sc) = shortcut {
                    b = b.shortcut_text(shortcut_text(sc, mac));
                }
                let r = ui.add_enabled(*enabled, b);
                crate::widgets::register(ui, format!("menuitem:{id}"), r.rect);
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
        let Some(MenuNode::Submenu { children, .. }) = edit.iter().find(|n| matches!(n, MenuNode::Submenu { label, .. } if label == "Select by"))
        else {
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
        let services = crate::Services {
            backup_library: Some(Box::new(move |_: &mut lightcraft_engine::Session| Err(refusal.to_string()))),
            ..Default::default()
        };
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
        assert_eq!(shortcut_text("Cmd+Shift+Z", true), "⌘⇧Z");
        assert_eq!(shortcut_text("Cmd+Shift+Z", false), "Ctrl+Shift+Z");
        assert_eq!(shortcut_text("Delete", true), "⌫");
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
}
