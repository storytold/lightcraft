//! UI-level commands (views, panels, zoom, tools, dialogs) and the menu model shared by the native
//! menu bar, the shortcut handler and the control channel.

use serde::Serialize;
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::pick::{PickRequest, Picked};
use crate::state::{BeforeAfter, Dialog, RightPanel, ViewMode, Zoom};

/// (id, label, shortcut, menu path)
pub type UiCommand = (&'static str, &'static str, Option<&'static str>, &'static str);

/// The "Edit → Language" entries, one per language in [`crate::i18n::Locale::ALL`], so a language
/// has a matching menu command (also listed through the control channel). Tests check coverage.
pub const LANGUAGE_COMMANDS: &[UiCommand] = &[
    ("app.language.english", crate::i18n::Locale::En.name(), None, "Edit>Language"),
    ("app.language.simplifiedChinese", crate::i18n::Locale::ZhHans.name(), None, "Edit>Language"),
    ("app.language.traditionalChinese", crate::i18n::Locale::ZhHant.name(), None, "Edit>Language"),
    ("app.language.japanese", crate::i18n::Locale::Ja.name(), None, "Edit>Language"),
    ("app.language.portuguese", crate::i18n::Locale::PtBr.name(), None, "Edit>Language"),
    ("app.language.french", crate::i18n::Locale::Fr.name(), None, "Edit>Language"),
    ("app.language.spanish", crate::i18n::Locale::Es.name(), None, "Edit>Language"),
    ("app.language.german", crate::i18n::Locale::De.name(), None, "Edit>Language"),
    ("app.language.russian", crate::i18n::Locale::Ru.name(), None, "Edit>Language"),
    ("app.language.ukrainian", crate::i18n::Locale::Uk.name(), None, "Edit>Language"),
    ("app.language.polish", crate::i18n::Locale::Pl.name(), None, "Edit>Language"),
    ("app.language.czech", crate::i18n::Locale::Cs.name(), None, "Edit>Language"),
    ("app.language.italian", crate::i18n::Locale::It.name(), None, "Edit>Language"),
    ("app.language.georgian", crate::i18n::Locale::Ka.name(), None, "Edit>Language"),
    ("app.language.lao", crate::i18n::Locale::Lo.name(), None, "Edit>Language"),
];

/// Every UI command: the languages, then everything else. `xtask parity` reads both tables from
/// this file, so an id listed in `docs/parity-checklist.md` is checked wherever it is declared.
pub fn ui_commands() -> impl Iterator<Item = &'static UiCommand> {
    LANGUAGE_COMMANDS.iter().chain(UI_COMMANDS)
}

/// The language a Language-menu command selects, if the id is one. The engine and the UI both go
/// through here, so the menu, the settings row and the control channel agree on the mapping.
pub fn language_from_command(id: &str) -> Option<crate::i18n::Locale> {
    match id {
        "app.language.english" => Some(crate::i18n::Locale::En),
        "app.language.simplifiedChinese" => Some(crate::i18n::Locale::ZhHans),
        "app.language.traditionalChinese" => Some(crate::i18n::Locale::ZhHant),
        "app.language.japanese" => Some(crate::i18n::Locale::Ja),
        "app.language.portuguese" => Some(crate::i18n::Locale::PtBr),
        "app.language.french" => Some(crate::i18n::Locale::Fr),
        "app.language.spanish" => Some(crate::i18n::Locale::Es),
        "app.language.german" => Some(crate::i18n::Locale::De),
        "app.language.russian" => Some(crate::i18n::Locale::Ru),
        "app.language.ukrainian" => Some(crate::i18n::Locale::Uk),
        "app.language.polish" => Some(crate::i18n::Locale::Pl),
        "app.language.czech" => Some(crate::i18n::Locale::Cs),
        "app.language.italian" => Some(crate::i18n::Locale::It),
        "app.language.georgian" => Some(crate::i18n::Locale::Ka),
        "app.language.lao" => Some(crate::i18n::Locale::Lo),
        _ => None,
    }
}

pub const UI_COMMANDS: &[UiCommand] = &[
    ("ui.dock", "Arrange Panel", None, ""),
    ("ui.dock.reset", "Reset Panel Layout", None, "Window"),
    ("modelSetup.cancel", "Cancel Pending Photo Action", None, ""),
    ("view.photoGrid", "Photo Grid", None, "View"),
    ("view.squareGrid", "Square Grid", None, "View"),
    // G: Photo Grid ↔ Square Grid (from other views: the photo grid)
    ("view.gridToggle", "Grid", Some("G"), ""),
    ("library.selectOnlyActive", "Select Only Active Photo", Some("Cmd+Shift+D"), "Edit"),
    ("tool.guidedUpright", "Guided Upright", Some("Shift+G"), "Window>Tools"),
    ("view.detail", "Detail", Some("D"), "View"),
    ("view.compare", "Compare", Some("Shift+C"), "View"),
    ("view.survey", "Survey", Some("N"), "View"),
    ("view.people", "People", None, "View"),
    ("view.person", "Show Person", None, ""),
    ("view.faceBoxes", "Face Boxes", None, "View"),
    ("view.focusPoint", "Show Focus Point", Some("Alt+F"), "View"),
    ("view.zoomToFocus", "Zoom to Focus Point", None, "View"),
    ("view.reference", "Reference View", Some("Shift+R"), "View"),
    ("photo.setReference", "Set as Reference Photo", None, ""),
    ("compare.swap", "Swap Compare Photos", None, "View"),
    ("compare.set", "Show in Compare", None, ""),
    ("compare.makeSelect", "Make Candidate the Select", None, "View"),
    ("view.autoAdvance", "Auto Advance", None, "Photo"),
    ("view.filmstrip", "Filmstrip", Some("/"), "View"),
    ("view.leftPanel", "My Photos Panel", Some("Cmd+Shift+L"), "View"),
    ("view.beforeAfter", "Compare Before and After", Some("Y"), "View"),
    ("view.beforeAfterSplit", "Before/After Split", Some("Shift+Y"), "View"),
    ("view.beforeAfterTopBottom", "Before/After Top/Bottom", Some("Alt+Y"), "View"),
    ("view.beforeAfterSplitTopBottom", "Before/After Split Top/Bottom", Some("Alt+Shift+Y"), "View"),
    ("view.showOriginal", "Show Original", Some("\\"), "View"),
    ("view.zoomFit", "Zoom to Fit", Some("Cmd+0"), "View"),
    ("view.zoom100", "Zoom 100%", Some("Cmd+Alt+0"), "View"),
    ("view.zoomToggle", "Toggle Zoom", Some("Z"), "View"),
    // the ratio a click (and Z / Space) zooms to
    ("view.clickZoom", "Click Zoom Ratio", None, ""),
    ("view.navigate", "Set Image Zoom and Pan", None, ""),
    ("view.zoomIn", "Zoom In", Some("Cmd+="), "View"),
    ("view.zoomOut", "Zoom Out", Some("Cmd+-"), "View"),
    // Library grids only (Photo Grid, Square Grid, People): the bottom-bar slider's step by key
    ("view.thumbSizeUp", "Increase Grid Thumbnail Size", Some("="), "View"),
    ("view.thumbSizeDown", "Decrease Grid Thumbnail Size", Some("-"), "View"),
    ("view.clipping", "Show Clipping", Some("J"), "View"),
    ("view.visualizeHdr", "Visualize HDR", None, "View"),
    // in grids S expands/collapses stacks (the engine command it shadows)
    ("view.softProof", "Soft Proofing", Some("S"), "View"),
    ("view.histogram", "Histogram", Some("Cmd+Shift+H"), "View"),
    ("view.maskOverlay", "Show Mask Overlay", Some("O"), "View"),
    // Ctrl+Shift+O / Cmd+Shift+O: every mask's region at once, not just the selected one
    ("view.maskOverlayAll", "Show All Mask Overlays", Some("Cmd+Shift+O"), "View"),
    // Shift+O in the Masking panel (elsewhere it cycles the crop overlay)
    ("view.maskOverlayMode", "Cycle Mask Overlay Mode", None, "View"),
    ("view.maskOverlayColor", "Cycle Mask Overlay Color", None, "View"),
    ("view.maskPins", "Show Mask Pins", None, "View"),
    // {mode?: auto | always | selected | never}: when spot and mask pins show (steps to the next
    // mode when omitted; Shift+H, like Classic's H for pins, since H opens Remove). View ▸ Tool
    // Overlay and the Remove panel list the modes.
    ("view.toolOverlay", "Cycle Tool Overlay", Some("Shift+H"), ""),
    ("view.visualizeSpots", "Visualize Spots", Some("A"), "View"),
    ("view.cropOverlay", "Cycle Crop Overlay", Some("Shift+O"), "View"),
    ("view.cropOverlayOrientation", "Cycle Crop Overlay Orientation", None, "View"),
    ("view.back", "Back to Grid", Some("Escape"), ""),
    ("tool.done", "Done", Some("Enter"), ""),
    ("view.filterBar", "Filter Bar", Some("Shift+F"), "View"),
    ("local.addRoot", "Add Folder to Local", None, ""),
    ("local.hide", "Remove from Local", None, ""),
    ("local.restoreHidden", "Show Hidden Local Locations", None, ""),
    ("view.fullScreenPreview", "Full Screen Preview", Some("F"), "View"),
    ("view.lightsOut", "Lights Out", Some("Shift+L"), "View"),
    ("view.enterFullScreen", "Enter Full Screen", Some("Cmd+Shift+F"), "View"),
    ("view.infoOverlay", "Cycle Info Overlay", Some("Cmd+I"), "View"),
    ("view.chooseLayoutImage", "Choose Layout Image…", None, "View"),
    ("view.layoutImage", "Show Layout Image", Some("Cmd+Alt+O"), "View"),
    ("view.layoutImageOpacity", "Layout Image Opacity", None, ""),
    ("view.layoutImageMatte", "Layout Image Matte", None, ""),
    ("view.navigator", "Navigator", None, "View"),
    ("panel.edit", "Edit", Some("E"), "Window"),
    ("panel.profiles", "Profile Browser", None, "Window"),
    ("panel.crop", "Crop & Rotate", Some("C"), "Window"),
    ("panel.remove", "Remove", Some("H"), "Window"),
    ("panel.masking", "Masking", Some("M"), "Window"),
    ("panel.redeye", "Red Eye", None, "Window"),
    ("panel.presets", "Presets", Some("Shift+P"), "Window"),
    ("panel.info", "Info", Some("I"), "Window"),
    ("panel.keywords", "Keywords", Some("K"), "Window"),
    ("panel.versions", "Versions", Some("Shift+V"), "Window"),
    ("panel.activity", "History", None, "Window"),
    ("panel.close", "Close Panel", None, ""),
    // {solo?}: true opens just that section (⌥-click on its header); otherwise opens or closes it
    ("section.light", "Light", Some("Cmd+1"), "Window>Edit Sections"),
    ("section.color", "Color", Some("Cmd+2"), "Window>Edit Sections"),
    ("section.effects", "Effects", Some("Cmd+3"), "Window>Edit Sections"),
    ("section.detail", "Detail", Some("Cmd+4"), "Window>Edit Sections"),
    ("section.optics", "Optics", Some("Cmd+5"), "Window>Edit Sections"),
    ("section.calibration", "Calibration", None, ""),
    // {on?}; no params toggles. Opening one Edit section closes the others.
    ("view.soloSections", "Solo Mode", None, "Window>Edit Sections"),
    ("tool.brush", "Brush", Some("B"), "Window>Tools"),
    ("tool.linear", "Linear Gradient", Some("L"), "Window>Tools"),
    ("tool.radial", "Radial Gradient", Some("R"), "Window>Tools"),
    ("tool.wbPicker", "White Balance Selector", Some("W"), "Window>Tools"),
    ("tool.none", "No Tool", None, ""),
    // brush size / feather of the active brush (Masking brush, Remove tool and its selected spot)
    ("brush.smaller", "Decrease Brush Size", Some("["), "Window>Tools"),
    ("brush.larger", "Increase Brush Size", Some("]"), "Window>Tools"),
    ("brush.featherLess", "Decrease Brush Feather", Some("Shift+["), "Window>Tools"),
    ("brush.featherMore", "Increase Brush Feather", Some("Shift+]"), "Window>Tools"),
    ("dialog.newAlbum", "New Album…", Some("Cmd+N"), "File"),
    ("dialog.newFolder", "New Folder…", Some("Cmd+Shift+N"), "File"),
    ("dialog.smartAlbum", "New Smart Album…", None, "File"),
    ("view.photoCounts", "Show Photo Counts", None, "View"),
    ("view.sidebarSection", "Show Sidebar Section", None, ""),
    ("view.sidebarMoveSection", "Move Sidebar Section", None, ""),
    ("view.sidebarReset", "Reset Sidebar Sections", None, ""),
    ("view.slideshow", "Slideshow", Some("Cmd+Alt+Enter"), "View"),
    ("view.slideshowOptions", "Slideshow Options…", None, "View"),
    ("dialog.slideshowExport", "Export Slideshow…", None, "View"),
    ("app.slideshowExport", "Export Slideshow", None, ""),
    // the slideshow's playback controls (Space, ←/→ and Esc while it plays)
    ("slideshow.togglePause", "Pause / Resume Slideshow", None, ""),
    ("slideshow.next", "Next Slide", None, ""),
    ("slideshow.previous", "Previous Slide", None, ""),
    ("slideshow.end", "End Slideshow", None, ""),
    ("view.secondWindow", "Second Window", Some("Cmd+F11"), "Window"),
    ("tool.keywordPainter", "Keyword Painter", None, ""),
    ("view.gridInfo", "Grid Info", None, ""),
    // {mode?: auto | light | dark}: Appearance Mode (cycles Auto, Light, Dark when omitted; the
    // top bar's appearance button). View ▸ Appearance lists the modes.
    ("view.appearance", "Next Appearance Mode", None, ""),
    // {theme: charcoal | midnight | silver | paper}: pick a theme and fix the mode to its family.
    ("view.theme", "Theme", None, ""),
    ("dialog.allMetadata", "All Metadata…", None, "Photo"),
    ("dialog.faceModel", "Add Face Model…", None, ""),
    ("dialog.newSmartAlbum", "New Smart Album from Filter…", Some("Cmd+Alt+N"), "File"),
    ("dialog.createPreset", "Create Preset…", Some("Cmd+Shift+P"), "Photo"),
    ("dialog.autoStack", "Auto-Stack by Capture Time…", None, "Photo>Stack"),
    ("dialog.copySettings", "Choose Edit Settings to Copy…", Some("Cmd+Shift+C"), "Edit"),
    ("dialog.pasteSettings", "Paste Selected Settings…", Some("Cmd+Shift+V"), "Edit"),
    ("view.focusSearch", "Find…", Some("Cmd+F"), "Edit"),
    ("dialog.export", "Export…", None, "File"),
    ("dialog.print", "Print…", Some("Cmd+P"), "File"),
    ("app.print", "Print to PDF", None, ""),
    ("dialog.contactSheet", "Contact Sheet PDF…", None, "File"),
    ("app.contactSheet", "Export Contact Sheet PDF", None, ""),
    ("photo.editInPhotoCraft", "Edit in PhotoCraft", Some("Cmd+Shift+E"), "Photo>Edit In"),
    ("photo.editInExternal", "Edit in Other Editor", Some("Cmd+Alt+E"), "Photo>Edit In"),
    // {ids?, app?}: the original files themselves (a raw stays a raw: no render, no copy), opened
    // in the other editor, e.g. a raw developer
    ("photo.openOriginalInExternal", "Open Original in Other Editor (RAW, no copy)", None, "Photo>Edit In"),
    ("dialog.mergeHdr", "HDR…", Some("Ctrl+H"), "Photo>Photo Merge"),
    ("dialog.mergePanorama", "Panorama…", Some("Ctrl+M"), "Photo>Photo Merge"),
    ("dialog.mergeHdrPanorama", "HDR Panorama…", None, "Photo>Photo Merge"),
    ("merge.hdrLast", "HDR with Last Settings", Some("Ctrl+Shift+H"), "Photo>Photo Merge"),
    ("merge.panoramaLast", "Panorama with Last Settings", Some("Ctrl+Shift+M"), "Photo>Photo Merge"),
    ("merge.hdrPanoramaLast", "HDR Panorama with Last Settings", None, "Photo>Photo Merge"),
    ("file.addPhotos", "Import Photos…", Some("Cmd+Shift+I"), "File"),
    ("file.addFolder", "Import from Folder…", None, "File"),
    ("file.importLightroom", "Import Lightroom Catalog…", None, "File"),
    ("file.addFromDevice", "Import from Device", None, ""),
    ("file.findMissing", "Find Missing Photos…", None, "File"),
    ("file.backupLibrary", "Back Up Library…", None, "File"),
    ("file.restoreLibrary", "Restore Library from Backup…", None, "File"),
    ("photo.locate", "Locate Missing File…", None, ""),
    ("dialog.saveMetadataPreset", "Save Metadata Preset…", None, ""),
    ("app.quit", "Quit LightCraft", Some("Cmd+Q"), "File"),
    ("file.importPresets", "Import Profiles & Presets…", None, "File"),
    ("file.exportPresets", "Export Presets…", None, "File"),
    // Masking panel ▸ Effect menu
    ("dialog.saveMaskEffect", "Save Current Settings as New Effect Preset…", None, ""),
    ("dialog.manageMaskEffects", "Manage Effect Presets…", None, ""),
    ("file.exportMaskEffects", "Export Effect Presets…", None, ""),
    ("file.importKeywords", "Import Keywords…", None, "File"),
    ("file.exportKeywords", "Export Keywords…", None, "File"),
    // Edit panel ▸ Curve ▸ Point Curve dropdown
    ("file.importCurvePresets", "Import Point Curve Presets…", None, ""),
    ("file.exportCurvePresets", "Export Point Curve Presets…", None, ""),
    ("app.settings", "Settings…", Some("Cmd+,"), "Edit"),
    ("app.newLibrary", "New Library…", None, "File"),
    ("app.openLibrary", "Open Library…", Some("Cmd+O"), "File"),
    ("app.recentLibraries", "Recent Libraries", None, ""),
    ("app.clearRecentLibraries", "Clear Recent Libraries", None, ""),
    // Settings ▸ Display
    ("app.displayProfile", "Display Profile", None, ""),
    ("app.about", "About LightCraft", None, "Help"),
    ("app.systemInfo", "System Info…", None, "Help"),
    ("app.openLogFolder", "Open Log Folder", None, "Help"),
    ("app.whatsNew", "What's New", None, "Help"),
    ("dialog.cull", "Assisted Culling…", None, "Photo"),
    ("app.help", "LightCraft Help", Some("F1"), "Help"),
    ("app.discord", "Join the ArtCraft Discord…", None, "Help"),
    ("app.feedback", "Send Feedback…", None, "Help"),
    ("app.website", "LightCraft Website", None, "Help"),
    ("app.github", "LightCraft on GitHub", None, "Help"),
    ("app.artcraft", "ArtCraft Website", None, "Help"),
    ("app.shortcuts", "Keyboard Shortcuts", Some("Cmd+/"), "Help"),
    ("app.setShortcut", "Set Keyboard Shortcut", None, ""),
    ("app.resetShortcuts", "Reset All Keyboard Shortcuts", None, ""),
    ("app.export", "Export Now", None, ""),
    ("app.showInFinder", "Show in Finder", Some("Cmd+R"), "Photo"),
    ("app.openSamModelFolder", "Show in File Manager", None, ""),
    ("dialog.rename", "Rename Photos…", Some("F2"), "Photo"),
    ("dialog.labelNames", "Edit Color Label Names…", None, ""),
    ("dialog.captureTime", "Edit Capture Time…", None, "Photo"),
    ("photo.tagFromTracklog", "Auto-Tag from Tracklog…", None, "Photo"),
    ("app.exportPrevious", "Export with Previous", Some("Cmd+Alt+Shift+E"), "File"),
];

fn panel(app: &mut LightcraftApp, ctx: &egui::Context, p: RightPanel, name: &str) {
    if app.ui.right == p && crate::docking::exposed(app, crate::docking::Panel::Tools) {
        app.ui.right = RightPanel::None;
        app.toast(ctx, crate::i18n::tr_format!("{name} Off", name = crate::i18n::tr(name)));
    } else {
        app.ui.right = p;
        app.toast(ctx, crate::i18n::tr_format!("{name} On", name = crate::i18n::tr(name)));
        if p.is_edit_tool() && !matches!(app.ui.view, ViewMode::Detail) {
            app.ui.view = ViewMode::Detail;
        }
    }
    if p != RightPanel::Masking && app.ui.tool != "wbPicker" {
        app.ui.tool.clear();
    }
    let _ = app.session.end_interaction();
}

/// `[` / `]` (size ×`k`) and ⇧`[` / ⇧`]` (feather +`df`) for the brush in use: the Remove tool's
/// (and its selected spot's) or the Masking brush's.
fn adjust_brush(app: &mut LightcraftApp, k: f32, df: f32) -> Value {
    if app.ui.right == RightPanel::Remove {
        app.ui.remove_size = (app.ui.remove_size * k).clamp(0.001, 0.25);
        app.ui.remove_feather = (app.ui.remove_feather + df).clamp(0.0, 100.0);
        if app.session.active_spot.is_some() {
            let mut p = json!({});
            if k != 1.0 {
                p["size"] = json!(app.ui.remove_size);
            }
            if df != 0.0 {
                p["feather"] = json!(app.ui.remove_feather);
            }
            let _ = app.run("spot.update", p);
        }
        json!({"size": app.ui.remove_size, "feather": app.ui.remove_feather})
    } else {
        app.ui.brush_size = (app.ui.brush_size * k).clamp(0.002, 0.5);
        app.ui.brush_feather = (app.ui.brush_feather + df).clamp(0.0, 100.0);
        json!({"size": app.ui.brush_size, "feather": app.ui.brush_feather})
    }
}

/// The `parent` folder id of a `dialog.new*` command (none: the top level).
fn parent_param(p: &Value) -> Option<u64> {
    p.get("parent").and_then(Value::as_u64)
}

/// `file.addPhotos` / `file.addFolder` / `file.addFromDevice` `{album?}`: the review of the scan
/// just started opens with that album chosen (an album's context menu ▸ Import ▸ …). An error
/// means another scan is running.
fn import_into_album(app: &mut LightcraftApp, p: &Value, r: Result<Value, String>) -> Result<Value, String> {
    if r.is_ok()
        && let Some(t) = &mut app.scan
    {
        t.album = p.get("album").and_then(Value::as_u64);
    }
    r
}

/// An sRGB colour from `"#rrggbb"` or `[r, g, b]` (0..255).
pub fn parse_rgb(v: &Value) -> Option<[u8; 3]> {
    if let Some(s) = v.as_str() {
        let h = s.strip_prefix('#').unwrap_or(s);
        if h.len() != 6 {
            return None;
        }
        let c = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
        return Some([c(0)?, c(2)?, c(4)?]);
    }
    let a = v.as_array()?;
    let c = |i: usize| a.get(i)?.as_f64().map(|x| x.clamp(0.0, 255.0).round() as u8);
    Some([c(0)?, c(1)?, c(2)?])
}

/// The left-sidebar section a `view.sidebar*` command names (`section`), which this build must have.
fn sidebar_section_param(command: &str, p: &Value) -> Result<crate::sidebar::SidebarSection, String> {
    use crate::sidebar::SidebarSection;
    let Some(name) = p.get("section").and_then(Value::as_str) else {
        return Err(format!("{command}: `section` is needed ({})", SidebarSection::ids()));
    };
    let section = SidebarSection::from_id(name).ok_or_else(|| format!("{command}: unknown section `{name}` ({})", SidebarSection::ids()))?;
    if !section.available() {
        return Err(format!("{command}: there is no `{name}` section here"));
    }
    Ok(section)
}

/// An optional true / false parameter: absent or null is `None`, anything else is refused.
fn bool_param(command: &str, p: &Value, key: &str) -> Result<Option<bool>, String> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(other) => Err(format!("{command}: `{key}` must be true or false, not {other}")),
    }
}
mod files;
mod run;
mod ui_commands;

pub use files::{confirm_delete, delete_in_album, reveal_label, show_in_folders_enabled};
pub use run::{parse_rgb, run_ui_command};
pub use ui_commands::{LANGUAGE_COMMANDS, UI_COMMANDS, language_from_command, ui_commands};

pub mod menu_level;
pub mod menubar;

/// (id, label, shortcut, menu path)
pub type UiCommand = (&'static str, &'static str, Option<&'static str>, &'static str);

/// Views whose bottom bar has the thumbnail-size slider and that draw tiles of that size.
fn thumb_size_view(app: &LightcraftApp) -> bool {
    matches!(app.ui.view, ViewMode::PhotoGrid | ViewMode::SquareGrid | ViewMode::People)
}

pub fn ui_enabled(app: &LightcraftApp, id: &str) -> bool {
    match id {
        "library.selectOnlyActive" => app.session.active().is_some(),
        "view.thumbSizeUp" | "view.thumbSizeDown" => thumb_size_view(app),
        "dialog.contactSheet" | "app.contactSheet" | "dialog.print" | "app.print" => {
            !cfg!(target_arch = "wasm32") && app.session.active().is_some() && app.export.is_none()
        }
        "dialog.slideshowExport" | "app.slideshowExport" => {
            !cfg!(target_arch = "wasm32") && !app.session.slideshow_export.as_ref().is_some_and(|r| r.running())
        }
        s if s.starts_with("panel.") || s.starts_with("tool.") || s.starts_with("section.") => app.session.active().is_some() || s == "panel.close",
        "app.export" | "dialog.export" | "dialog.createPreset" | "dialog.rename" | "dialog.captureTime" | "dialog.copySettings" => {
            app.session.active().is_some()
        }
        "photo.tagFromTracklog" => app.session.active().is_some() && app.services.pick_tracklog.is_some(),
        "photo.editInPhotoCraft" | "photo.editInExternal" => app.session.active().is_some() && app.services.open_with.is_some(),
        "app.exportPrevious" => app.session.active().is_some() && app.session.last_export.is_some(),
        "dialog.pasteSettings" => app.session.active().is_some() && app.session.clipboard.is_some(),
        "photo.openOriginalInExternal" => {
            // only a file type LightCraft reads can be handed over (#2135); the extension alone
            // decides, so no disk or share is touched while a menu is drawn
            app.services.open_with.is_some()
                && app.session.active().and_then(|id| app.session.catalog.photo(id)).is_some_and(|p| match &p.source {
                    lightcraft_engine::catalog::Source::File { path } => lightcraft_engine::open_path::is_media(std::path::Path::new(path)),
                    _ => false,
                })
        }
        "app.showInFinder" => {
            app.services.reveal.is_some()
                && app
                    .session
                    .active()
                    .and_then(|id| app.session.catalog.photo(id))
                    .is_some_and(|p| matches!(p.source, lightcraft_engine::catalog::Source::File { .. }))
        }
        "app.showInFolders" => show_in_folders_enabled(app, None),
        "app.openSamModelFolder" => app.session.segmenter.dir.is_some() && app.services.open_with.is_some(),
        "file.exportPresets" => app.session.presets.iter().any(|p| !p.builtin),
        "file.exportMaskEffects" => !app.session.mask_effects.is_empty(),
        "dialog.saveMaskEffect" => app.session.active().is_some() && app.session.active_mask.is_some(),
        "file.exportCurvePresets" => !app.session.curve_presets.is_empty(),
        "view.compare" => app.session.catalog.len() > 1,
        "view.fullScreenPreview" | "view.infoOverlay" | "view.navigator" => app.session.active().is_some() || app.ui.fullscreen,
        "app.openLibrary" | "app.newLibrary" => app.services.pick_folder.is_some() && crate::panels::settings::can_switch_library(app).is_ok(),
        "file.addFolder" => app.services.pick_folder.is_some() && !crate::lightroom_import::is_running(app),
        "file.backupLibrary" => app.services.backup_library.is_some(),
        "app.openLogFolder" => app.services.reveal.is_some() && app.services.log_file.is_some(),
        "file.restoreLibrary" => app.services.restore_library.is_some(),
        "app.clearRecentLibraries" => !app.ui.settings.recent_libraries.is_empty(),
        "compare.swap" | "compare.makeSelect" | "compare.set" => app.ui.view == ViewMode::Compare,
        "slideshow.togglePause" | "slideshow.next" | "slideshow.previous" | "slideshow.end" => app.ui.slideshow.is_some(),
        s if s.starts_with("dialog.merge") || (s.starts_with("merge.") && s.ends_with("Last")) => {
            app.session.selected().len() >= 2 && app.merge.final_task.is_none()
        }
        _ => true,
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct MenuEntry {
    pub id: String,
    pub label: String,
    pub menu: Vec<String>,
    pub shortcut: Option<String>,
    pub enabled: bool,
}

/// The flattened menu model (UI commands + engine commands with menu paths).
pub fn menu_entries(app: &LightcraftApp) -> Vec<MenuEntry> {
    let mut v: Vec<MenuEntry> = ui_commands()
        .filter(|c| !c.3.is_empty())
        .map(|(id, label, sc, m)| MenuEntry {
            id: id.to_string(),
            label: label.to_string(),
            menu: m.split('>').map(str::to_string).collect(),
            shortcut: crate::shortcuts::binding(&app.ui.settings.keymap, id, *sc).map(str::to_string),
            enabled: ui_enabled(app, id),
        })
        .collect();
    for c in app.session.commands() {
        if !c.menu.is_empty() {
            v.push(MenuEntry {
                id: c.id.into(),
                label: c.label.into(),
                menu: c.menu.iter().map(|s| s.to_string()).collect(),
                shortcut: crate::shortcuts::shortcut_of(&app.ui.settings.keymap, c.id).map(str::to_string),
                enabled: c.enabled,
            });
        }
    }
    v
}
