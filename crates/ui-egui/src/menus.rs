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

/// What Shift+H says it switched to (the pins themselves may be off screen).
fn tool_overlay_toast(mode: crate::state::ToolOverlay) -> String {
    use crate::state::ToolOverlay;
    crate::i18n::tr(match mode {
        ToolOverlay::Auto => "Tool Overlay: Auto",
        ToolOverlay::Always => "Tool Overlay: Always",
        ToolOverlay::Selected => "Tool Overlay: Selected",
        ToolOverlay::Never => "Tool Overlay: Never",
    })
    .to_string()
}

fn appearance_json(app: &LightcraftApp) -> Value {
    let s = &app.ui.settings;
    json!({"mode": s.appearance_mode, "darkTheme": s.dark_theme, "lightTheme": s.light_theme})
}

/// `view.appearance {mode?}`: set Appearance Mode, or step to the next one (Auto, Light, Dark).
/// The theme follows on the next frame ([`LightcraftApp::sync_theme`]).
fn appearance_command(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    use crate::state::AppearanceMode;
    let mode = match p.get("mode") {
        None | Some(Value::Null) => app.ui.settings.appearance_mode.next(),
        Some(v) => match v.as_str() {
            Some("auto") => AppearanceMode::Auto,
            Some("light") => AppearanceMode::Light,
            Some("dark") => AppearanceMode::Dark,
            _ => return Err(format!("view.appearance: unknown mode {v} (auto|light|dark)")),
        },
    };
    app.ui.settings.appearance_mode = mode;
    Ok(appearance_json(app))
}

/// `view.theme {theme}`: pick a theme; the mode becomes its family's (Dark or Light).
fn theme_command(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    let ids = crate::theme::ThemeKind::ALL.map(|k| k.id()).join("|");
    let Some(id) = p.get("theme").and_then(Value::as_str) else {
        return Err(format!("view.theme: `theme` is required ({ids})"));
    };
    let Some(kind) = crate::theme::ThemeKind::ALL.into_iter().find(|k| k.id() == id) else {
        return Err(format!("view.theme: unknown theme `{id}` ({ids})"));
    };
    app.ui.settings.select_theme(kind);
    Ok(appearance_json(app))
}

/// Handle UI commands; `None` means "not a UI command — send it to the engine".
pub fn run_ui_command(app: &mut LightcraftApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if matches!(id, "ui.dock" | "ui.dock.reset") {
        return Some(crate::docking::command(app, id == "ui.dock.reset", p));
    }
    if matches!(id, "library.inspectLightroom" | "library.importLightroom") {
        // the app's own context: a fresh one's repaints reach no window and its clock starts at zero
        let ctx = app.tasks.repaint.clone().unwrap_or_default();
        return Some(crate::lightroom_import::command(app, id, p, &ctx));
    }
    if id == "photo.reload" {
        // the files are read in the background (a raw's decode takes a second or more)
        return crate::reload::command(app, p);
    }
    if let Some(language) = language_from_command(id) {
        app.ui.language = language;
        // Immediately, not on the next frame: the reply and anything else run this frame
        // (menus rebuilt from it, toasts) are already in the new language.
        crate::i18n::set_language(app.ui.language);
        return Some(Ok(json!(app.ui.language)));
    }
    // the app's own context: a toast's time comes from its clock (a fresh context's clock is at 0,
    // and a toast set by it was long over by the app's clock: it never showed)
    let ctx = app.tasks.repaint.clone().unwrap_or_default();
    let r: Result<Value, String> = match id {
        "view.photoGrid" => {
            app.ui.view = ViewMode::PhotoGrid;
            Ok(Value::Null)
        }
        "view.squareGrid" => {
            app.ui.view = ViewMode::SquareGrid;
            Ok(Value::Null)
        }
        "view.gridInfo" => {
            // {info?: filename | exposure | date} (cycles when omitted)
            let next = match p.get("info").and_then(Value::as_str) {
                Some(i @ ("filename" | "exposure" | "date")) => i.to_string(),
                Some(other) => return Some(Err(format!("view.gridInfo: unknown `{other}` (filename|exposure|date)"))),
                None => match app.ui.grid_info.as_str() {
                    "filename" => "exposure".into(),
                    "exposure" => "date".into(),
                    _ => "filename".into(),
                },
            };
            app.ui.grid_info = next;
            app.ui.show_filenames = true;
            // the caption is drawn on Square Grid cells only: from Photo Grid, show it there
            if app.ui.view == ViewMode::PhotoGrid {
                app.ui.view = ViewMode::SquareGrid;
            }
            Ok(json!({"info": app.ui.grid_info}))
        }
        "tool.keywordPainter" => {
            // {keyword?}: paint that keyword onto photos in the grid by clicking them; no keyword stops
            app.ui.keyword_painter = p.get("keyword").and_then(Value::as_str).map(str::trim).filter(|k| !k.is_empty()).map(str::to_string);
            if app.ui.keyword_painter.is_some() && !matches!(app.ui.view, ViewMode::PhotoGrid | ViewMode::SquareGrid) {
                app.ui.view = ViewMode::PhotoGrid;
            }
            if let Some(k) = app.ui.keyword_painter.clone() {
                app.toast(&ctx, crate::i18n::tr_format!("Painting “{k}”: click photos to add or remove it · Esc stops", k = k));
            }
            Ok(json!({"keyword": app.ui.keyword_painter}))
        }
        "view.secondWindow" => {
            app.ui.second_window = p.get("show").and_then(Value::as_bool).unwrap_or(!app.ui.second_window);
            Ok(json!({"show": app.ui.second_window}))
        }
        "view.photoCounts" => {
            app.ui.show_counts = p.get("show").and_then(Value::as_bool).unwrap_or(!app.ui.show_counts);
            Ok(json!({"show": app.ui.show_counts}))
        }
        "view.sidebarSection" => {
            // {section, show?, collapsed?}: show or hide a section of the left sidebar, fold or
            // unfold it; with neither, a shown section is hidden and a hidden one shown
            let args = sidebar_section_param(id, p).and_then(|s| Ok((s, bool_param(id, p, "show")?, bool_param(id, p, "collapsed")?)));
            let (section, show, collapsed) = match args {
                Ok(args) => args,
                Err(e) => return Some(Err(e)),
            };
            let layout = &mut app.ui.sidebar;
            match (show, collapsed) {
                (None, None) => layout.set_hidden(section, !layout.is_hidden(section)),
                _ => {
                    if let Some(show) = show {
                        layout.set_hidden(section, !show);
                    }
                    if let Some(collapsed) = collapsed {
                        layout.set_collapsed(section, collapsed);
                    }
                }
            }
            Ok(json!({"section": section.id(), "show": !layout.is_hidden(section), "collapsed": layout.is_collapsed(section)}))
        }
        "view.sidebarMoveSection" => {
            // {section, direction: up | down} one place among the shown sections, or
            // {section, before: <section> | null} just above another section (null: last)
            let section = match sidebar_section_param(id, p) {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            let layout = &mut app.ui.sidebar;
            let moved = match (p.get("direction"), p.get("before")) {
                (Some(d), None) => match d.as_str() {
                    Some("up") => layout.move_by(section, true),
                    Some("down") => layout.move_by(section, false),
                    _ => return Some(Err(format!("{id}: `direction` must be \"up\" or \"down\", not {d}"))),
                },
                (None, Some(Value::Null)) => layout.move_before(section, None),
                (None, Some(b)) => {
                    let before = b.as_str().and_then(crate::sidebar::SidebarSection::from_id);
                    let Some(before) = before else {
                        return Some(Err(format!("{id}: `before` must be a section ({}) or null, not {b}", crate::sidebar::SidebarSection::ids())));
                    };
                    layout.move_before(section, Some(before))
                }
                _ => return Some(Err(format!("{id}: give either `direction` (up|down) or `before` (a section, or null for last)"))),
            };
            let order: Vec<&str> = layout.sections().iter().map(|s| s.id.id()).collect();
            Ok(json!({"section": section.id(), "moved": moved, "order": order}))
        }
        "view.sidebarReset" => {
            // every section shown and open, in the usual order
            app.ui.sidebar = Default::default();
            Ok(json!({"sidebar": app.ui.sidebar}))
        }
        "view.appearance" => appearance_command(app, p),
        "view.theme" => theme_command(app, p),
        "library.selectOnlyActive" => {
            let Some(active) = app.session.active() else { return Some(Err("no active photo".into())) };
            app.run("library.select", json!({"ids": [active.0], "active": active.0}))
        }
        "view.gridToggle" => {
            app.ui.view = if app.ui.view == ViewMode::PhotoGrid { ViewMode::SquareGrid } else { ViewMode::PhotoGrid };
            Ok(Value::Null)
        }
        "view.detail" => {
            app.ui.view = ViewMode::Detail;
            Ok(Value::Null)
        }
        "view.compare" => crate::panels::compare::enter_compare(app),
        "view.focusPoint" => {
            // {on?}; no params toggles. The camera's focus point (Sony, from the file) over the photo.
            app.ui.settings.focus_point = p.get("on").and_then(Value::as_bool).unwrap_or(!app.ui.settings.focus_point);
            Ok(json!({"on": app.ui.settings.focus_point}))
        }
        "view.zoomToFocus" => {
            // {on?}; no params toggles. Toggle Zoom (Z) centres on the focus point when there is one.
            app.ui.settings.zoom_to_focus = p.get("on").and_then(Value::as_bool).unwrap_or(!app.ui.settings.zoom_to_focus);
            Ok(json!({"on": app.ui.settings.zoom_to_focus}))
        }
        "view.faceBoxes" => {
            app.ui.face_boxes = p.get("show").and_then(Value::as_bool).unwrap_or(!app.ui.face_boxes);
            Ok(json!({"show": app.ui.face_boxes}))
        }
        "view.people" => {
            // Everyone; the page just left stays one click away (the chip next to the title)
            if let Some(name) = app.ui.person_page.take() {
                app.ui.last_person = Some(name);
            }
            app.ui.person_from = None;
            app.ui.view = ViewMode::People;
            Ok(json!({"people": app.session.catalog.people().len()}))
        }
        "view.person" => {
            // {name}: one person's page in the People view: their faces, and the faces that look like them
            let Some(name) = p.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()) else {
                return Some(Err("view.person: missing `name`".into()));
            };
            app.ui.view = ViewMode::People;
            app.ui.person_from = None;
            app.ui.person_page = Some(name.to_string());
            Ok(json!({"person": name}))
        }
        "view.survey" => {
            app.ui.view = ViewMode::Survey;
            Ok(json!({"photos": crate::panels::compare::survey_photos(app).len()}))
        }
        "view.reference" => {
            // the reference: the one set before, else the active photo (the next one becomes active)
            let vis = app.session.visible_cloned();
            let reference = app
                .ui
                .reference
                .filter(|r| app.session.catalog.photo(lightcraft_catalog::PhotoId(*r)).is_some())
                .or_else(|| app.session.active().map(|a| a.0));
            let Some(r) = reference else { return Some(Err("select a photo to use as the reference".into())) };
            app.ui.reference = Some(r);
            if app.session.active().is_none_or(|a| a.0 == r)
                && let Some(i) = vis.iter().position(|x| x.0 == r)
                && let Some(next) = vis.get(i + 1).or(i.checked_sub(1).and_then(|j| vis.get(j)))
            {
                let _ = app.session.execute("library.select", &json!({"ids": [next.0]}));
            }
            app.ui.view = ViewMode::Reference;
            Ok(json!({"reference": r, "active": app.session.active().map(|a| a.0)}))
        }
        "photo.setReference" => {
            let id = p.get("id").and_then(Value::as_u64).or_else(|| app.session.active().map(|a| a.0));
            app.ui.reference = id;
            Ok(json!({"reference": id}))
        }
        "compare.swap" => crate::panels::compare::swap(app),
        "compare.set" => {
            let Some(id) = p.get("id").and_then(Value::as_u64) else { return Some(Err("compare.set needs `id`".into())) };
            crate::panels::compare::set_photo(app, lightcraft_catalog::PhotoId(id), p.get("side").and_then(Value::as_str))
        }
        "compare.makeSelect" => crate::panels::compare::make_select(app),
        "view.autoAdvance" => {
            app.ui.auto_advance = !app.ui.auto_advance;
            app.toast(&ctx, crate::i18n::tr(if app.ui.auto_advance { "Auto Advance On" } else { "Auto Advance Off" }));
            Ok(json!({"autoAdvance": app.ui.auto_advance}))
        }
        "view.back" => {
            if app.ui.dialog.is_some() {
                app.ui.dialog = None;
            } else if app.ui.lights_out != crate::state::LightsOut::On {
                app.ui.lights_out = crate::state::LightsOut::On;
            } else if app.ui.keyword_painter.is_some() {
                app.ui.keyword_painter = None;
            } else if app.ui.fullscreen {
                app.ui.fullscreen = false;
                app.ui.slideshow = None;
            } else if !app.ui.tool.is_empty() {
                app.ui.tool.clear();
            } else if app.ui.right == RightPanel::Crop && app.ui.view == ViewMode::Detail {
                // Esc leaves the crop tool (the crop stays), as C does; the next Esc goes back
                panel(app, &ctx, RightPanel::Crop, "Crop, Rotate, Geometry");
            } else if app.ui.view == ViewMode::People && app.ui.person_page.is_some() {
                app.ui.person_page = None;
            } else if matches!(app.ui.view, ViewMode::Compare | ViewMode::Survey) {
                app.ui.view = ViewMode::Detail;
            } else if app.ui.view == ViewMode::Detail {
                // a photo opened from a person's page (or the unnamed faces) goes back there, any other to the grid
                match app.ui.person_from.take() {
                    Some((name, photo)) if app.session.active().is_some_and(|a| a.0 == photo) => {
                        app.ui.view = ViewMode::People;
                        app.ui.person_page = name;
                    }
                    _ => app.ui.view = ViewMode::PhotoGrid,
                }
            }
            Ok(Value::Null)
        }
        "view.slideshow" => {
            // the photos in view, full screen, with the playback settings (see panels::slideshow)
            let now = ctx.input(|i| i.time);
            let r = crate::panels::slideshow::start(app, p, now);
            if r.is_ok() {
                app.toast(&ctx, crate::i18n::tr("Slideshow · Space pauses · Esc ends"));
            }
            r
        }
        "slideshow.togglePause" | "slideshow.next" | "slideshow.previous" | "slideshow.end" => {
            let now = ctx.input(|i| i.time);
            crate::panels::slideshow::command(app, id, now)
        }
        "view.slideshowOptions" => {
            app.ui.dialog = Some(Dialog::Settings { tab: "slideshow".into() });
            Ok(Value::Null)
        }
        "view.fullScreenPreview" => {
            app.ui.fullscreen = !app.ui.fullscreen;
            if !app.ui.fullscreen {
                app.ui.slideshow = None;
            }
            if app.ui.fullscreen {
                app.ui.zoom = Zoom::Fit;
                app.ui.tool.clear();
                let _ = app.session.end_interaction();
            }
            Ok(json!({"fullscreen": app.ui.fullscreen}))
        }
        "view.lightsOut" => {
            // {mode?: on | dim | off} (cycles when omitted)
            match p.get("mode").and_then(Value::as_str) {
                Some(m) => serde_json::from_value(json!(m)).map_err(|_| format!("unknown lights-out mode: {m} (on|dim|off)")),
                None => Ok(app.ui.lights_out.next()),
            }
            .map(|mode| {
                app.ui.lights_out = mode;
                json!({"lightsOut": mode})
            })
        }
        "view.enterFullScreen" => {
            // applied by the host's frame logic (it knows the window's current state)
            let on = p.get("on").and_then(Value::as_bool);
            app.ui.window_fullscreen = Some(on.unwrap_or(!app.window_is_fullscreen));
            Ok(json!({"windowFullscreen": app.ui.window_fullscreen}))
        }
        "view.infoOverlay" => {
            app.ui.info_overlay = match p.get("mode").and_then(Value::as_str) {
                Some(m) => match serde_json::from_value(json!(m)) {
                    Ok(v) => v,
                    Err(_) => return Some(Err(format!("unknown info overlay `{m}` (off|basic|exposure)"))),
                },
                None => app.ui.info_overlay.next(),
            };
            let label = match app.ui.info_overlay {
                crate::state::InfoOverlay::Off => "Info Overlay Off",
                crate::state::InfoOverlay::Basic => "Info Overlay: File",
                crate::state::InfoOverlay::Exposure => "Info Overlay: Exposure",
            };
            app.toast(&ctx, label);
            Ok(json!({"infoOverlay": app.ui.info_overlay}))
        }
        "view.chooseLayoutImage" => {
            let path = match p.get("path").and_then(Value::as_str) {
                Some(path) => path.trim().to_string(),
                None => {
                    let req = PickRequest::file(crate::i18n::tr("Choose Layout Image…"), "PNG", &["png"]);
                    match crate::pick::ask(app, id, p, "path", req, |s| s.pick_files.as_mut().map(|f| f())) {
                        Picked::Now(paths) => match paths.first() {
                            Some(path) => path.trim().to_string(),
                            None => return Some(Ok(Value::Null)),
                        },
                        Picked::Later => return Some(Ok(Value::Null)),
                        Picked::Unavailable => return Some(Err("no file dialog on this platform; supply path".into())),
                    }
                }
            };
            if path.is_empty() {
                return Some(Err("view.chooseLayoutImage: path is empty".into()));
            }
            let li = &mut app.ui.layout_image;
            li.path = path;
            li.show = true;
            li.revision = li.revision.wrapping_add(1);
            app.toast(&ctx, "Layout Image On");
            Ok(json!({"layoutImage": app.ui.layout_image}))
        }
        "view.layoutImage" => {
            let show = p.get("show").and_then(Value::as_bool).unwrap_or(!app.ui.layout_image.show);
            // nothing chosen yet: showing it chooses one first
            if show && app.ui.layout_image.path.trim().is_empty() {
                return run_ui_command(app, "view.chooseLayoutImage", &json!({}));
            }
            app.ui.layout_image.show = show;
            app.toast(&ctx, if show { "Layout Image On" } else { "Layout Image Off" });
            Ok(json!({"layoutImage": app.ui.layout_image}))
        }
        "view.layoutImageOpacity" | "view.layoutImageMatte" => {
            let key = if id == "view.layoutImageOpacity" { "opacity" } else { "matte" };
            let Some(v) = p.get(key).and_then(Value::as_f64).filter(|v| v.is_finite()) else {
                return Some(Err(format!("{id}: {key} (0..1) is required")));
            };
            let v = v.clamp(0.0, 1.0) as f32;
            if key == "opacity" {
                app.ui.layout_image.opacity = v;
            } else {
                app.ui.layout_image.matte = v;
            }
            Ok(json!({"layoutImage": app.ui.layout_image}))
        }
        "view.navigator" => {
            app.ui.navigator = !app.ui.navigator;
            Ok(json!({"navigator": app.ui.navigator}))
        }
        "app.settings" => {
            let tab = p.get("tab").and_then(Value::as_str).unwrap_or("general");
            if !crate::panels::settings::TABS.iter().any(|(id, _)| *id == tab) {
                let tabs: Vec<&str> = crate::panels::settings::TABS.iter().map(|(id, _)| *id).collect();
                return Some(Err(format!("unknown settings tab `{tab}` ({})", tabs.join("|"))));
            }
            app.ui.dialog = Some(Dialog::Settings { tab: tab.into() });
            Ok(Value::Null)
        }
        "app.openLibrary" => crate::panels::settings::open_library(app, p),
        "app.newLibrary" => crate::panels::settings::new_library(app, p),
        "app.recentLibraries" => crate::panels::settings::recent_libraries(app, p),
        "app.clearRecentLibraries" => crate::panels::settings::recent_libraries(app, &json!({"clear": true})),
        "app.displayProfile" => crate::panels::settings::display_profile(app, p),
        "file.backupLibrary" | "file.restoreLibrary" => {
            let action = if id == "file.backupLibrary" { app.services.backup_library.as_mut() } else { app.services.restore_library.as_mut() };
            match action {
                Some(f) => f(&mut app.session),
                None => Err("not available here: on the desktop the library is a folder; back it up with your other files".into()),
            }
        }
        "view.filmstrip" => {
            app.ui.filmstrip = !app.ui.filmstrip;
            Ok(Value::Null)
        }
        "view.leftPanel" => {
            app.ui.left_panel = !app.ui.left_panel;
            Ok(Value::Null)
        }
        "view.beforeAfter" => {
            app.ui.before_after = if app.ui.before_after == BeforeAfter::SideBySide { BeforeAfter::Off } else { BeforeAfter::SideBySide };
            Ok(Value::Null)
        }
        "view.beforeAfterSplit" => {
            app.ui.before_after = if app.ui.before_after == BeforeAfter::Split { BeforeAfter::Off } else { BeforeAfter::Split };
            Ok(Value::Null)
        }
        "view.beforeAfterTopBottom" => {
            app.ui.before_after = if app.ui.before_after == BeforeAfter::TopBottom { BeforeAfter::Off } else { BeforeAfter::TopBottom };
            Ok(Value::Null)
        }
        "view.beforeAfterSplitTopBottom" => {
            app.ui.before_after = if app.ui.before_after == BeforeAfter::SplitTopBottom { BeforeAfter::Off } else { BeforeAfter::SplitTopBottom };
            Ok(Value::Null)
        }
        "view.showOriginal" => {
            app.ui.before_after = if app.ui.before_after == BeforeAfter::Original { BeforeAfter::Off } else { BeforeAfter::Original };
            Ok(Value::Null)
        }
        "view.zoomFit" => {
            app.ui.zoom = Zoom::Fit;
            Ok(Value::Null)
        }
        "view.zoom100" => {
            app.ui.zoom = Zoom::Percent(100.0);
            Ok(Value::Null)
        }
        "view.zoomToggle" => {
            // the same ratio a click on the photo zooms to; zooming in centres on the camera's focus
            // point when the photo has one (View ▸ Zoom to Focus Point)
            let zoom_in = app.ui.zoom == Zoom::Fit;
            app.ui.zoom = if zoom_in { Zoom::Percent(app.ui.click_zoom as f32) } else { Zoom::Fit };
            app.ui.zoom_anim = true;
            if zoom_in
                && app.ui.settings.zoom_to_focus
                && let Some(pan) = app.session.active().and_then(|id| crate::panels::detail::focus_pan(app, id))
            {
                app.ui.pan = pan;
                return Some(Ok(json!({"focus": [pan.0, pan.1]})));
            }
            Ok(Value::Null)
        }
        "view.clickZoom" => {
            // {ratio?: 1|2|3|4|8} → {ratio}
            if let Some(r) = p.get("ratio").and_then(Value::as_f64) {
                let pct = (r * 100.0).round() as u32;
                if !crate::state::CLICK_ZOOMS.contains(&pct) {
                    return Some(Err(format!("view.clickZoom: ratio {r} (1, 2, 3, 4 or 8)")));
                }
                app.ui.click_zoom = pct;
            }
            Ok(json!({"ratio": app.ui.click_zoom / 100}))
        }
        "view.zoomIn" | "view.zoomOut" => {
            let steps = [25.0, 50.0, 100.0, 200.0, 400.0, 800.0];
            let cur = match app.ui.zoom {
                Zoom::Percent(p) => p,
                _ => 25.0,
            };
            let next = if id == "view.zoomIn" {
                steps.iter().find(|s| **s > cur).copied().unwrap_or(800.0)
            } else {
                steps.iter().rev().find(|s| **s < cur).copied().unwrap_or(0.0)
            };
            app.ui.zoom = if next == 0.0 { Zoom::Fit } else { Zoom::Percent(next) };
            Ok(Value::Null)
        }
        "view.thumbSizeUp" | "view.thumbSizeDown" => {
            // the bottom-bar slider's range, one `THUMB_SIZE_STEP` per call; {} → {size}
            if !thumb_size_view(app) {
                return Some(Err(format!("{id}: only in Photo Grid, Square Grid or People")));
            }
            let step = if id == "view.thumbSizeUp" { crate::state::THUMB_SIZE_STEP } else { -crate::state::THUMB_SIZE_STEP };
            app.ui.thumb_size = (app.ui.thumb_size + step).clamp(crate::state::THUMB_SIZE_MIN, crate::state::THUMB_SIZE_MAX);
            Ok(json!({"size": app.ui.thumb_size}))
        }
        "view.navigate" => {
            // {zoom?: "fit"|"fill"|{percent: number}, pan?: [x, y]} (normalized image centre).
            // Validate the complete request before changing either part of the viewport.
            let zoom = match p.get("zoom") {
                Some(v) => match serde_json::from_value::<Zoom>(v.clone()) {
                    Ok(Zoom::Percent(p)) if !p.is_finite() || p <= 0.0 || p > 800.0 => {
                        return Some(Err("view.navigate: zoom percent must be greater than 0 and at most 800".into()));
                    }
                    Ok(z) => z,
                    Err(e) => return Some(Err(format!("view.navigate: {e}"))),
                },
                None => app.ui.zoom,
            };
            let pan = match p.get("pan") {
                Some(v) => match serde_json::from_value::<(f32, f32)>(v.clone()) {
                    Ok((x, y)) if x.is_finite() && y.is_finite() && (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y) => (x, y),
                    _ => return Some(Err("view.navigate: pan must be [x, y] with finite coordinates from 0 to 1".into())),
                },
                None => app.ui.pan,
            };
            app.ui.zoom = zoom;
            app.ui.pan = pan;
            app.ui.zoom_anim = false;
            Ok(json!({"zoom": zoom, "pan": pan}))
        }
        "view.clipping" => {
            app.ui.show_clipping = !app.ui.show_clipping;
            Ok(Value::Null)
        }
        "view.visualizeHdr" => {
            // {on?}; no params toggles. Shown on photos edited in HDR (see `develop.hdr`).
            app.ui.hdr_visualize = p.get("on").and_then(Value::as_bool).unwrap_or(!app.ui.hdr_visualize);
            Ok(json!({"on": app.ui.hdr_visualize}))
        }
        "view.softProof" => {
            // {on?, space?, profile?: ICC path | null, intent?: perceptual|relative, paperInk?,
            // destWarning?, displayWarning?}; no params toggles
            let has = |k: &str| p.get(k).is_some();
            if let Some(s) = p.get("space").and_then(Value::as_str) {
                match lightcraft_engine::pipeline::OutputSpace::parse(s) {
                    Some(sp) => {
                        app.ui.proof.space = sp;
                        // a standard space replaces a device profile (unless one is given too)
                        app.ui.proof_profile = None;
                    }
                    None => return Some(Err(format!("view.softProof: unknown space {s:?} (srgb|displayP3|adobeRgb|proPhoto|rec2020)"))),
                }
            }
            if let Some(i) = p.get("intent") {
                match i.as_str().and_then(lightcraft_engine::proof::ProofIntent::parse) {
                    Some(i) => app.ui.proof_intent = i,
                    None => return Some(Err("view.softProof: intent must be perceptual or relative".into())),
                }
            }
            if let Some(b) = p.get("paperInk").and_then(Value::as_bool) {
                app.ui.proof_paper_ink = b;
            }
            let path = match p.get("profile") {
                Some(Value::String(s)) => Some(s.clone()),
                Some(Value::Null) => {
                    app.ui.proof_profile = None;
                    None
                }
                Some(_) => return Some(Err("view.softProof: profile must be an ICC file path or null".into())),
                None => app.ui.proof_profile.as_ref().map(|(f, _)| f.clone()),
            };
            match path {
                Some(path) => match lightcraft_engine::proof::load(&path, app.ui.proof_intent, app.ui.proof_paper_ink) {
                    Ok(pp) => {
                        app.ui.proof.profile = pp.id;
                        app.ui.proof_profile = Some((path, pp.name));
                    }
                    Err(e) => return Some(Err(format!("view.softProof: {e}"))),
                },
                None => app.ui.proof.profile = 0,
            }
            if let Some(b) = p.get("destWarning").and_then(Value::as_bool) {
                app.ui.proof.dest_warning = b;
            }
            if let Some(b) = p.get("displayWarning").and_then(Value::as_bool) {
                app.ui.proof.display_warning = b;
            }
            app.ui.soft_proof = match p.get("on").and_then(Value::as_bool) {
                Some(b) => b,
                None if ["space", "destWarning", "displayWarning", "profile", "intent", "paperInk"].iter().any(|k| has(k)) => app.ui.soft_proof,
                None => !app.ui.soft_proof,
            };
            if app.ui.soft_proof && !matches!(app.ui.view, ViewMode::Detail | ViewMode::Reference) {
                app.ui.view = ViewMode::Detail;
            }
            let pr = app.ui.proof;
            let profile = app.ui.proof_profile.as_ref().map(|(f, n)| json!({"path": f, "name": n}));
            Ok(json!({"on": app.ui.soft_proof, "space": pr.space, "profile": profile, "intent": app.ui.proof_intent.name(),
                "paperInk": app.ui.proof_paper_ink, "destWarning": pr.dest_warning, "displayWarning": pr.display_warning}))
        }
        "view.histogram" => {
            app.ui.histogram = !app.ui.histogram;
            Ok(Value::Null)
        }
        "view.maskOverlay" => {
            app.ui.mask_overlay = p.get("show").and_then(Value::as_bool).unwrap_or(!app.ui.mask_overlay);
            Ok(json!({"maskOverlay": app.ui.mask_overlay}))
        }
        "view.maskOverlayAll" => {
            app.ui.mask_overlay_all = !app.ui.mask_overlay_all;
            Ok(json!({"maskOverlayAll": app.ui.mask_overlay_all}))
        }
        "view.maskOverlayMode" => {
            use lightcraft_engine::pipeline::MaskView;
            let cur = MaskView::parse(&app.ui.mask_overlay_mode).unwrap_or_default();
            let next = match p.get("mode").and_then(Value::as_str) {
                Some(m) => match MaskView::parse(m) {
                    Some(v) => v,
                    None => {
                        let names: Vec<&str> = MaskView::ALL.iter().map(|v| v.name()).collect();
                        return Some(Err(format!("view.maskOverlayMode: unknown mode `{m}` ({})", names.join("|"))));
                    }
                },
                None => cur.next(),
            };
            app.ui.mask_overlay_mode = next.name().into();
            app.ui.mask_overlay = true;
            app.toast(&ctx, next.label());
            Ok(json!({"mode": next.name()}))
        }
        "view.maskOverlayColor" => {
            // no params: the next of the panel's swatch colours
            if p.get("color").is_none() && p.get("opacity").is_none() {
                let all = crate::panels::masking::OVERLAY_COLORS;
                let i = all.iter().position(|c| *c == app.ui.mask_overlay_color).map_or(0, |i| (i + 1) % all.len());
                app.ui.mask_overlay_color = all[i];
            }
            if let Some(c) = p.get("color") {
                match parse_rgb(c) {
                    Some(rgb) => app.ui.mask_overlay_color = rgb,
                    None => return Some(Err("view.maskOverlayColor: `color` is \"#rrggbb\" or [r, g, b]".into())),
                }
            }
            if let Some(o) = p.get("opacity").and_then(Value::as_f64) {
                app.ui.mask_overlay_opacity = o.clamp(0.0, 100.0) as f32;
            }
            let [r, g, b] = app.ui.mask_overlay_color;
            Ok(json!({"color": format!("#{r:02x}{g:02x}{b:02x}"), "opacity": app.ui.mask_overlay_opacity}))
        }
        "brush.smaller" | "brush.larger" | "brush.featherLess" | "brush.featherMore" => {
            let (k, df) = match id {
                "brush.smaller" => (1.0 / 1.2, 0.0),
                "brush.larger" => (1.2, 0.0),
                "brush.featherLess" => (1.0, -10.0),
                _ => (1.0, 10.0),
            };
            Ok(adjust_brush(app, k, df))
        }
        "view.maskPins" => {
            app.ui.mask_pins = p.get("show").and_then(Value::as_bool).unwrap_or(!app.ui.mask_pins);
            Ok(json!({"maskPins": app.ui.mask_pins}))
        }
        "view.toolOverlay" => {
            use crate::state::ToolOverlay;
            let mode = match p.get("mode") {
                None | Some(Value::Null) => app.ui.tool_overlay.next(),
                Some(v) => match v.as_str().and_then(ToolOverlay::parse) {
                    Some(m) => m,
                    None => return Some(Err(format!("view.toolOverlay: unknown mode {v} (auto|always|selected|never)"))),
                },
            };
            app.ui.tool_overlay = mode;
            app.toast(&ctx, tool_overlay_toast(mode));
            Ok(json!({"mode": mode.name()}))
        }
        "view.visualizeSpots" => {
            // like Lightroom's A: opens the Remove tool with the view on, or toggles it there
            if app.ui.right == RightPanel::Remove {
                app.ui.visualize_spots = !app.ui.visualize_spots;
            } else {
                app.ui.right = RightPanel::Remove;
                app.ui.visualize_spots = true;
            }
            Ok(Value::Null)
        }
        "view.cropOverlay" => {
            use crate::state::CropOverlay::*;
            app.ui.crop_overlay = match app.ui.crop_overlay {
                Thirds => Grid,
                Grid => Golden,
                Golden => Diagonal,
                Diagonal => Triangle,
                Triangle => Spiral,
                Spiral => None,
                None => Thirds,
            };
            Ok(json!({"overlay": app.ui.crop_overlay}))
        }
        "view.cropOverlayOrientation" => {
            app.ui.crop_overlay_orient = (app.ui.crop_overlay_orient + 1) % 4;
            Ok(json!({"orientation": app.ui.crop_overlay_orient}))
        }
        "local.addRoot" => {
            // a folder kept in Local's sidebar (saved with the UI state); nothing on disk changes
            let Some(path) = p.get("path").and_then(Value::as_str) else { return Some(Err("local.addRoot needs a path".into())) };
            let abs = std::path::absolute(path).map(|a| a.to_string_lossy().trim_end_matches(['/', '\\']).to_string()).unwrap_or(path.into());
            let abs = if abs.is_empty() { path.to_string() } else { abs };
            if !std::path::Path::new(&abs).is_dir() {
                return Some(Err(format!("{path}: not a folder")));
            }
            use crate::panels::left::same_folder;
            if !app.ui.local_roots.iter().any(|r| same_folder(r, &abs)) {
                app.ui.local_roots.push(abs.clone());
            }
            app.ui.hidden_locations.retain(|h| !same_folder(h, &abs));
            Ok(json!({"roots": app.ui.local_roots}))
        }
        "local.hide" => match p.get("path").and_then(Value::as_str) {
            Some(path) => {
                if !app.ui.hidden_locations.iter().any(|h| crate::panels::left::same_folder(h, path)) {
                    app.ui.hidden_locations.push(path.to_string());
                }
                Ok(json!({"hidden": app.ui.hidden_locations}))
            }
            None => Err("local.hide needs a path".into()),
        },
        "local.restoreHidden" => {
            // one path, or (no path) every hidden location
            match p.get("path").and_then(Value::as_str) {
                Some(path) => app.ui.hidden_locations.retain(|h| !crate::panels::left::same_folder(h, path)),
                None => app.ui.hidden_locations.clear(),
            }
            Ok(json!({"hidden": app.ui.hidden_locations}))
        }
        "view.soloSections" => bool_param(id, p, "on").map(|on| {
            app.ui.set_single_panel(on.unwrap_or(!app.ui.single_panel));
            json!({"soloSections": app.ui.single_panel})
        }),
        "view.filterBar" => {
            app.ui.filter_bar = !app.ui.filter_bar;
            if app.ui.filter_bar && !matches!(app.ui.view, ViewMode::PhotoGrid | ViewMode::SquareGrid) {
                app.ui.view = ViewMode::PhotoGrid;
            }
            Ok(json!({"filterBar": app.ui.filter_bar}))
        }
        "panel.edit" => {
            panel(app, &ctx, RightPanel::Edit, "Edit");
            Ok(Value::Null)
        }
        "panel.profiles" => {
            // toggles between the profile browser and the Edit panel it belongs to
            app.ui.right = if app.ui.right == RightPanel::Profiles && crate::docking::exposed(app, crate::docking::Panel::Tools) {
                RightPanel::Edit
            } else {
                RightPanel::Profiles
            };
            if !matches!(app.ui.view, ViewMode::Detail) {
                app.ui.view = ViewMode::Detail;
            }
            Ok(json!({"open": app.ui.right == RightPanel::Profiles}))
        }
        "panel.crop" => {
            panel(app, &ctx, RightPanel::Crop, "Crop, Rotate, Geometry");
            Ok(Value::Null)
        }
        "panel.remove" => {
            panel(app, &ctx, RightPanel::Remove, "Remove");
            if app.ui.right == RightPanel::Remove && app.ui.tool.is_empty() {
                app.ui.tool = "remove".into();
            }
            Ok(Value::Null)
        }
        "panel.masking" => {
            panel(app, &ctx, RightPanel::Masking, "Masking");
            Ok(Value::Null)
        }
        "panel.redeye" => {
            panel(app, &ctx, RightPanel::RedEye, "Red Eye");
            Ok(Value::Null)
        }
        "panel.info" => {
            panel(app, &ctx, RightPanel::Info, "Info");
            Ok(Value::Null)
        }
        "panel.keywords" => {
            panel(app, &ctx, RightPanel::Keywords, "Keywords");
            Ok(Value::Null)
        }
        "panel.versions" => {
            panel(app, &ctx, RightPanel::Versions, "Versions");
            Ok(Value::Null)
        }
        "panel.activity" => {
            panel(app, &ctx, RightPanel::Activity, "History");
            Ok(Value::Null)
        }
        "panel.presets" => {
            app.ui.presets = !app.ui.presets || !crate::docking::exposed(app, crate::docking::Panel::Presets);
            if app.ui.presets && app.ui.view != ViewMode::Detail {
                app.ui.view = ViewMode::Detail;
            }
            Ok(Value::Null)
        }
        "tool.done" => {
            // Return commits a tool panel (crop, remove, red eye, masking): back to Edit
            use RightPanel::*;
            if app.ui.dialog.is_none() && matches!(app.ui.right, Crop | Remove | RedEye | Masking) {
                let _ = app.session.end_interaction();
                app.ui.tool.clear();
                app.ui.right = Edit;
            }
            Ok(Value::Null)
        }
        "panel.close" => {
            app.ui.right = RightPanel::None;
            app.ui.presets = false;
            Ok(Value::Null)
        }
        s if s.starts_with("section.") => bool_param(s, p, "solo").and_then(|solo| {
            // only the Edit sections listed above: an unknown one would close the real ones
            if !UI_COMMANDS.iter().any(|c| c.0 == s) {
                return Err(format!("{s}: no such Edit section"));
            }
            let sec = &s["section.".len()..];
            if app.ui.right != RightPanel::Edit {
                app.ui.right = RightPanel::Edit;
            }
            if solo == Some(true) {
                app.ui.solo_section(sec);
            } else {
                app.ui.toggle_section(sec);
            }
            Ok(Value::Null)
        }),
        s if s.starts_with("tool.") => {
            let tool = &s["tool.".len()..];
            match tool {
                "none" => app.ui.tool.clear(),
                "guidedUpright" => {
                    // Crop & Geometry with Guided Upright on, ready to draw guides
                    app.ui.right = RightPanel::Crop;
                    app.ui.view = ViewMode::Detail;
                    let guided = app
                        .session
                        .active()
                        .and_then(|id| app.session.develop_of(id))
                        .is_some_and(|d| d.geometry.upright == lightcraft_develop::Upright::Guided);
                    if !guided && let Err(e) = app.run("geometry.upright", json!({"mode": "guided"})) {
                        return Some(Err(e));
                    }
                    app.ui.tool = "guidedUpright".into();
                }
                "brush" => {
                    // A new mask and Add/Subtract are selection operations, not the brush's
                    // Erase mode. Paint positively into a component with the requested op.
                    let create = if p.get("new").and_then(Value::as_bool) == Some(true) {
                        Some(app.run("mask.add", json!({"kind": "brush"})))
                    } else {
                        p.get("op").map(|op| app.run("mask.addComponent", json!({"kind": "brush", "op": op})))
                    };
                    if let Some(result) = create {
                        if let Err(e) = result {
                            return Some(Err(e));
                        }
                        app.ui.brush_erase = false;
                        app.ui.mask_overlay = true;
                    }
                    app.ui.right = RightPanel::Masking;
                    app.ui.view = ViewMode::Detail;
                    app.ui.tool = "brush".into();
                }
                "linear" | "radial" => {
                    app.ui.right = RightPanel::Masking;
                    app.ui.view = ViewMode::Detail;
                    app.ui.tool = tool.into();
                    return Some(app.session.execute("mask.add", &json!({"kind": tool})).map_err(|e| e.to_string()));
                }
                "wbPicker" => {
                    app.ui.right = RightPanel::Edit;
                    app.ui.view = ViewMode::Detail;
                    app.ui.tool = "wbPicker".into();
                }
                other => return Some(Err(format!("unknown tool `{other}`"))),
            }
            Ok(Value::Null)
        }
        "dialog.newFolder" => {
            app.ui.dialog = Some(Dialog::NewAlbum {
                name: p.get("name").and_then(Value::as_str).unwrap_or("").into(),
                folder: true,
                parent: parent_param(p),
                include_selected: false,
            });
            Ok(Value::Null)
        }
        "dialog.newAlbum" => {
            // {name?, parent?, includeSelected? (true)}
            app.ui.dialog = Some(Dialog::NewAlbum {
                name: p.get("name").and_then(Value::as_str).unwrap_or("").into(),
                folder: false,
                parent: parent_param(p),
                include_selected: p.get("includeSelected").and_then(Value::as_bool).unwrap_or(true),
            });
            Ok(Value::Null)
        }
        "dialog.autoStack" => {
            app.ui.dialog = Some(Dialog::AutoStack { gap: p.get("gap").and_then(Value::as_f64).unwrap_or(60.0) as f32 });
            Ok(Value::Null)
        }
        "dialog.allMetadata" => {
            let r = match app.session.execute("photo.allMetadata", p) {
                Ok(r) => r,
                Err(e) => return Some(Err(e.to_string())),
            };
            let title = app.session.active().and_then(|id| app.session.catalog.photo(id)).map(|p| p.file_name.clone()).unwrap_or_default();
            app.ui.dialog = Some(Dialog::AllMetadata { title, rows: r, search: String::new() });
            Ok(Value::Null)
        }
        "dialog.smartAlbum" => {
            // {id?}: edit that smart album's rules; without: a new one starting at Rating ≥ 3
            let album = p
                .get("id")
                .and_then(Value::as_u64)
                .and_then(|id| app.session.catalog.album(lightcraft_catalog::AlbumId(id)).filter(|a| a.is_smart()).cloned());
            app.ui.dialog = Some(match album {
                Some(a) => Dialog::SmartRules {
                    id: Some(a.id.0),
                    name: a.name.clone(),
                    // older rules read as they always matched (RuleSet::upgrade)
                    rules: a
                        .smart
                        .as_ref()
                        .and_then(|f| f.rule_set.clone())
                        .map(|mut r| {
                            r.upgrade();
                            r
                        })
                        .unwrap_or_default(),
                    parent: None,
                },
                None => Dialog::SmartRules {
                    id: None,
                    name: p.get("name").and_then(Value::as_str).unwrap_or("").into(),
                    rules: lightcraft_catalog::RuleSet { rules: vec![crate::panels::rules_editor::new_rule()], ..Default::default() },
                    parent: parent_param(p),
                },
            });
            Ok(Value::Null)
        }
        "dialog.newSmartAlbum" => {
            app.ui.dialog = Some(Dialog::NewSmartAlbum { name: p.get("name").and_then(Value::as_str).unwrap_or("").into(), parent: parent_param(p) });
            Ok(Value::Null)
        }
        "dialog.captureTime" => {
            let time = app.session.active().and_then(|id| app.session.catalog.photo(id)).map(|p| p.date().replace('T', " ")).unwrap_or_default();
            let mode = p.get("mode").and_then(Value::as_str).unwrap_or("set").to_string();
            app.ui.dialog =
                Some(Dialog::CaptureTime { mode, time: time.get(..19).unwrap_or(&time).to_string(), days: 0, hours: 0, minutes: 0, zone: 0.0 });
            Ok(Value::Null)
        }
        "dialog.labelNames" => {
            let names =
                lightcraft_catalog::ColorLabel::ALL.iter().map(|l| app.session.catalog.custom_label_name(*l).unwrap_or("").to_string()).collect();
            app.ui.dialog = Some(Dialog::LabelNames { names, save_as: String::new() });
            Ok(Value::Null)
        }
        "dialog.rename" => {
            let template = p.get("template").and_then(Value::as_str).unwrap_or("{name}").to_string();
            app.ui.dialog = Some(Dialog::Rename { template, start: p.get("start").and_then(Value::as_u64).unwrap_or(1) as u32 });
            Ok(Value::Null)
        }
        "dialog.createPreset" => {
            app.ui.dialog = Some(Dialog::create_preset());
            Ok(Value::Null)
        }
        "dialog.pasteSettings" => {
            let groups =
                app.session.copy_groups.iter().filter_map(|g| serde_json::to_value(g).ok().and_then(|v| v.as_str().map(str::to_string))).collect();
            app.ui.dialog = Some(Dialog::PasteSettings { groups });
            Ok(Value::Null)
        }
        "view.focusSearch" => {
            // the search field lives in the top bar of the library and detail views alike
            app.ui.focus_search = true;
            Ok(Value::Null)
        }
        "dialog.copySettings" => {
            let groups =
                app.session.copy_groups.iter().filter_map(|g| serde_json::to_value(g).ok().and_then(|v| v.as_str().map(str::to_string))).collect();
            app.ui.dialog = Some(Dialog::CopySettings { groups });
            Ok(Value::Null)
        }
        "dialog.slideshowExport" => {
            let format = match p.get("format").and_then(Value::as_str) {
                Some("jpeg") => lightcraft_engine::slideshow_export::Format::Jpeg,
                Some("video") => lightcraft_engine::slideshow_export::Format::Video,
                _ => lightcraft_engine::slideshow_export::Format::Pdf,
            };
            app.ui.dialog = Some(Dialog::SlideshowExport { settings: lightcraft_engine::slideshow_export::ExportSettings::for_format(format) });
            Ok(Value::Null)
        }
        "app.slideshowExport" => crate::slideshow_export::start(app, id, p),
        "dialog.contactSheet" => {
            app.ui.dialog = Some(Dialog::ContactSheet { options: lightcraft_engine::contact_sheet::Options::default() });
            Ok(Value::Null)
        }
        "dialog.print" => {
            app.ui.print = Default::default();
            app.ui.dialog = Some(Dialog::Print);
            Ok(Value::Null)
        }
        "app.print" => {
            if app.export.is_some() {
                return Some(Err("an export is already running".into()));
            }
            let mut params = p.clone();
            if p.get("path").is_none() {
                let req = PickRequest::save("Print to PDF", "PDF", &["pdf"], "Print.pdf");
                let paths = match crate::pick::ask(app, id, p, "path", req, |_| None) {
                    Picked::Now(paths) => paths,
                    Picked::Later => return Some(Ok(Value::Null)),
                    Picked::Unavailable => return Some(Err("no save dialog on this platform; supply path".into())),
                };
                let Some(path) = paths.first() else { return Some(Ok(Value::Null)) };
                params["path"] = json!(path);
            }
            crate::export_task::start_print(app, &params)
        }
        "app.contactSheet" => {
            if app.export.is_some() {
                return Some(Err("an export is already running".into()));
            }
            let mut params = p.clone();
            if p.get("path").is_none() {
                let req = PickRequest::save("Contact Sheet PDF", "PDF", &["pdf"], "Contact Sheet.pdf");
                let paths = match crate::pick::ask(app, id, p, "path", req, |_| None) {
                    Picked::Now(paths) => paths,
                    Picked::Later => return Some(Ok(Value::Null)),
                    Picked::Unavailable => return Some(Err("no save dialog on this platform; supply path".into())),
                };
                let Some(path) = paths.first() else { return Some(Ok(Value::Null)) };
                params["path"] = json!(path);
            }
            crate::export_task::start_contact_sheet(app, &params)
        }
        "dialog.export" => {
            let prev = app.session.last_export.clone().unwrap_or_default();
            let u = |k: &str, d: u64| prev.get(k).and_then(Value::as_u64).unwrap_or(d);
            let dir = prev.get("dir").and_then(Value::as_str).map(str::to_string).unwrap_or_else(crate::control::default_export_dir);
            let opts = lightcraft_engine::export::ExportOptions::from_json(&prev);
            // no previous export: 2048 px long edge; a previous full-size export: full size
            let full_size = opts.resize.is_none() && lightcraft_engine::export::ExportOptions::has_size_param(&prev);
            let resize = opts.resize.unwrap_or_default();
            app.ui.dialog = Some(Dialog::Export { opts, full_size, resize, preset_name: String::new(), limit_kb: u("limitKb", 0) as u32, dir });
            Ok(Value::Null)
        }
        "merge.hdrLast" => crate::merge::start_last(app, "merge.hdr"),
        "merge.hdrBatch" => crate::merge::start_batch(app, p),
        "merge.panoramaLast" => crate::merge::start_last(app, "merge.panorama"),
        "merge.hdrPanoramaLast" => crate::merge::start_last(app, "merge.hdrPanorama"),
        "dialog.mergeHdr" => crate::merge::open(app, "merge.hdr"),
        "dialog.faceModel" => {
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => Some(x.to_string()),
                None => app.services.pick_model_file.as_mut().and_then(|f| f().into_iter().next()),
            };
            match path {
                Some(path) => crate::panels::faces::open_dialog(app, &path),
                None => Ok(Value::Null),
            }
        }
        "dialog.mergePanorama" => crate::merge::open(app, "merge.panorama"),
        "dialog.mergeHdrPanorama" => crate::merge::open(app, "merge.hdrPanorama"),
        "app.openSamModelFolder" => crate::panels::dialogs::show_model_folder(app),
        "app.about" => {
            app.ui.dialog = Some(Dialog::About);
            Ok(Value::Null)
        }
        "photo.editInPhotoCraft" => edit_in(app, &ctx, p, true),
        "photo.editInExternal" => edit_in(app, &ctx, p, false),
        "photo.openOriginalInExternal" => open_original_in(app, &ctx, p),
        "dialog.cull" => {
            app.ui.dialog = Some(Dialog::Cull { reject_below: 0.0, pick_best: true });
            Ok(Value::Null)
        }
        "app.whatsNew" => {
            app.ui.dialog = Some(Dialog::WhatsNew);
            Ok(json!({"text": crate::panels::dialogs::WHATS_NEW}))
        }
        "app.systemInfo" => {
            let info = app.session.execute("library.info", &json!({})).unwrap_or_default();
            let gpu = (lightcraft_engine::gpu::ready() && lightcraft_engine::gpu::available()).then(lightcraft_engine::gpu::adapter_name).flatten();
            let mb = |b: u64| format!("{:.0} MB", b as f64 / (1u64 << 20) as f64);
            let mut rows = vec![
                ("Version".to_string(), env!("CARGO_PKG_VERSION").to_string()),
                ("System".to_string(), format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH)),
                ("CPU threads".to_string(), std::thread::available_parallelism().map(|n| n.get().to_string()).unwrap_or_else(|_| "?".into())),
                (
                    "GPU".to_string(),
                    gpu.unwrap_or_else(|| match lightcraft_engine::gpu::unavailable_reason() {
                        Some(why) => crate::i18n::tr_format!("none (CPU rendering): {why}", why = why),
                        None => "none (CPU rendering)".into(),
                    }),
                ),
                ("GPU rendering".to_string(), if app.ui.settings.gpu { "on".into() } else { "off".into() }),
                ("Memory budget".to_string(), mb(lightcraft_engine::memory::default_budget() as u64)),
                (
                    "Preview size".to_string(),
                    match app.ui.settings.preview_limit {
                        0 => "automatic".to_string(),
                        n => format!("{n} px"),
                    },
                ),
                ("Photos".to_string(), info["photos"].to_string()),
                ("Albums".to_string(), info["albums"].to_string()),
            ];
            if let Some(dir) = info["dir"].as_str().or(info["path"].as_str()) {
                rows.push(("Library".into(), dir.to_string()));
            }
            rows.push(("Frame time".into(), format!("{:.1} ms ({:.0} fps)", app.perf.frame_ms, app.perf.fps)));
            rows.push((
                "Frame update".into(),
                format!("{:.1} ms (logic {:.1} ms; slowest {:.0} ms)", app.perf.update_ms, app.perf.logic_ms, app.perf.max_update_ms),
            ));
            rows.push(("Last loupe render".into(), format!("{:.0} ms", app.renderer.last_main_ms)));
            if let Some(f) = lightcraft_engine::gpu::last_fallback() {
                rows.push(("Last GPU fallback".into(), f));
            }
            let r = json!(rows.iter().map(|(k, v)| json!({"label": k, "value": v})).collect::<Vec<_>>());
            if p.get("open").and_then(Value::as_bool).unwrap_or(true) {
                app.ui.dialog = Some(Dialog::SystemInfo { rows });
            }
            Ok(r)
        }
        "app.shortcuts" => {
            app.ui.dialog = Some(Dialog::Shortcuts);
            Ok(Value::Null)
        }
        "app.setShortcut" => crate::shortcuts::set_shortcut(app, p),
        "app.resetShortcuts" => {
            app.ui.settings.keymap.clear();
            Ok(Value::Null)
        }
        "library.browse" if !cfg!(target_arch = "wasm32") => {
            if crate::lightroom_import::is_running(app) {
                return Some(Err("finish Lightroom catalog import before browsing folders".into()));
            }
            // listed and read in the background (see `import::browse`)
            let path = p.get("path").and_then(Value::as_str)?;
            crate::import::browse(app, path, p.get("subfolders").and_then(Value::as_bool))
        }
        "file.addPhotos" => {
            if crate::lightroom_import::is_running(app) {
                return Some(Err("finish Lightroom catalog import before adding photos".into()));
            }
            let paths = match p.get("paths").and_then(Value::as_array) {
                Some(a) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
                None => {
                    let req = PickRequest::files(None, crate::i18n::tr("Photos"), lightcraft_engine::import::EXTENSIONS);
                    match crate::pick::ask(app, id, p, "paths", req, |s| s.pick_files.as_mut().map(|f| f())) {
                        Picked::Now(paths) => paths,
                        Picked::Later | Picked::Unavailable => return Some(Ok(Value::Null)),
                    }
                }
            };
            if paths.is_empty() {
                return Some(Ok(Value::Null));
            }
            // review first: the import dialog lists what was found
            let r = crate::import::open(app, paths);
            import_into_album(app, p, r)
        }
        "file.importLightroom" => {
            let path = match p.get("path").and_then(Value::as_str).map(str::to_string) {
                Some(path) => Some(path),
                None => {
                    let req =
                        PickRequest::file(crate::i18n::tr("Import Lightroom Catalog"), crate::i18n::tr("Lightroom Classic Catalog"), &["lrcat"]);
                    match crate::pick::ask(app, id, p, "path", req, |s| s.pick_lightroom_catalog.as_mut().and_then(|f| f()).map(|x| vec![x])) {
                        Picked::Now(v) => v.into_iter().next(),
                        Picked::Later | Picked::Unavailable => None,
                    }
                }
            };
            match path {
                Some(path) => app.run("library.importLightroom", json!({"path":path})),
                None => Ok(Value::Null),
            }
        }
        "app.quit" => {
            app.ui.quit = true;
            Ok(Value::Null)
        }
        "dialog.saveMetadataPreset" => {
            // from the active photo's copyright, creator and place
            crate::panels::dialogs::prompt(app, "Save Metadata Preset", "Preset name", "", "metadata.savePreset", json!({}), "name");
            Ok(Value::Null)
        }
        "file.findMissing" => {
            let within = match p.get("within") {
                None | Some(Value::Null) => None,
                Some(Value::String(path)) if !path.is_empty() => Some(path.clone()),
                Some(Value::String(_)) => return Some(Err("file.findMissing: `within` must not be empty".into())),
                Some(value) => return Some(Err(format!("file.findMissing: `within` must be a folder path, not {value}"))),
            };
            let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
            let timeout = match p.get("timeout") {
                None | Some(Value::Null) => std::time::Duration::from_secs(600),
                Some(Value::Number(n)) => {
                    if let Some(s) = n.as_f64() {
                        if s.is_finite() && s >= 0.0 {
                            match std::time::Duration::try_from_secs_f64(s) {
                                Ok(d) => d,
                                Err(_) => return Some(Err("file.findMissing: `timeout` is too large".into())),
                            }
                        } else {
                            return Some(Err("file.findMissing: `timeout` must be a non-negative number of seconds".into()));
                        }
                    } else {
                        return Some(Err("file.findMissing: `timeout` must be a number of seconds".into()));
                    }
                }
                Some(other) => return Some(Err(format!("file.findMissing: `timeout` must be a number of seconds, not {other}"))),
            };
            let title = if within.is_some() { "Locate Missing Files…" } else { "Find Missing Photos…" };
            let folder = match p.get("folder").and_then(Value::as_str) {
                Some(f) => Some(f.to_string()),
                None => match crate::pick::ask(app, id, p, "folder", PickRequest::folder(crate::i18n::tr(title)), |s| {
                    s.pick_folder.as_mut().and_then(|f| f()).map(|x| vec![x])
                }) {
                    Picked::Now(v) => v.into_iter().next(),
                    Picked::Later | Picked::Unavailable => None,
                },
            };
            let Some(folder) = folder else { return Some(Ok(Value::Null)) };
            // the search (checking every photo's file, walking the folder) runs on a worker thread;
            // the relinking happens back here, as one undo step
            const LABEL: &str = "Find Missing Photos";
            if app.tasks.is_running(LABEL) {
                return Some(Err("Find Missing Photos is already searching".into()));
            }
            let mut candidates = lightcraft_engine::cmd::missing::find_candidates(&app.session.catalog);
            if let Some(within) = within {
                candidates.retain(|candidate| lightcraft_catalog::query::folder_within(&candidate.path, &within));
            }
            #[cfg(test)]
            let test_delay = app.ui.find_missing_test_delay;
            let work = move || {
                #[cfg(test)]
                if let Some(d) = test_delay {
                    std::thread::sleep(d);
                }
                lightcraft_engine::cmd::missing::plan_find_missing(&candidates, &folder)
            };
            let done = |app: &mut LightcraftApp, ctx: &egui::Context, plan: Result<lightcraft_engine::cmd::missing::FindPlan, String>| {
                // relinked under the session as it is now: photos relinked meanwhile and files
                // now in use are skipped
                let r = plan.and_then(|plan| app.run("library.findMissing", plan.to_json()));
                match r {
                    Ok(v) => {
                        let n = v["found"].as_array().map_or(0, Vec::len);
                        let left = v["missing"].as_u64().unwrap_or(0);
                        let unsure = v["ambiguous"].as_array().map_or(0, Vec::len);
                        let unsure = if unsure > 0 {
                            crate::i18n::tr_format!(" ({unsure} with several look-alike files: use Locate)", unsure = unsure)
                        } else {
                            String::new()
                        };
                        app.toast(
                            ctx,
                            crate::i18n::tr_format!(
                                "Found {n} missing photo{}; {left} still missing{unsure}",
                                if n == 1 { "" } else { "s" },
                                left = left,
                                n = n,
                                unsure = unsure
                            ),
                        );
                        app.ui.last_find_missing = Some(v.clone());
                        app.ui.last_find_missing_result = Some(Ok(v));
                    }
                    Err(e) => {
                        app.toast(ctx, e.clone());
                        app.ui.last_find_missing_result = Some(Err(e));
                    }
                }
            };
            app.ui.last_find_missing = None;
            app.ui.last_find_missing_result = None;
            if let Err(e) = crate::tasks::spawn(app, LABEL, Some("findMissing"), work, done) {
                return Some(Err(e));
            }
            if wait {
                let ctx = app.tasks.repaint.clone().unwrap_or_default();
                let completed = crate::tasks::wait_for(app, &ctx, LABEL, timeout);
                if !completed {
                    return Some(Err("Find Missing Photos: search timed out (search continues in the background)".into()));
                }
                return match app.ui.last_find_missing_result.take() {
                    Some(Ok(v)) => Some(Ok(v)),
                    Some(Err(e)) => Some(Err(e)),
                    None => Some(Err("Find Missing Photos: worker terminated unexpectedly".into())),
                };
            }
            Ok(json!({"background": true}))
        }
        "photo.tagFromTracklog" => {
            // a GPX file → GPS for the selected photos by capture time (one undo step)
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => Some(x.to_string()),
                None => {
                    let req = PickRequest::file(crate::i18n::tr("Auto-Tag from Tracklog"), crate::i18n::tr("GPS Track Log"), &["gpx"]);
                    match crate::pick::ask(app, id, p, "path", req, |s| s.pick_tracklog.as_mut().map(|f| f())) {
                        Picked::Now(v) => v.into_iter().next(),
                        Picked::Later | Picked::Unavailable => None,
                    }
                }
            };
            let Some(path) = path else { return Some(Ok(Value::Null)) };
            let mut params = p.as_object().cloned().unwrap_or_default();
            params.insert("path".into(), json!(path));
            let ask_zone = !params.contains_key("offset");
            let params = Value::Object(params);
            if ask_zone {
                // GPX times are UTC, camera clocks are local: ask for the camera's zone, then come back here
                crate::panels::dialogs::prompt(
                    app,
                    "Auto-Tag from Tracklog",
                    "Camera time zone, e.g. -07:00 (empty: UTC)",
                    "",
                    "photo.tagFromTracklog",
                    params,
                    "offset",
                );
                return Some(Ok(Value::Null));
            }
            let r = app.run("photo.autoTagTracklog", params);
            if let Ok(v) = &r {
                let n = v["tagged"].as_u64().unwrap_or(0);
                let sk = &v["skipped"];
                let mut msg = crate::i18n::tr_format!("Tagged {n} photo{} from the tracklog", if n == 1 { "" } else { "s" }, n = n);
                let outside = sk["outside"].as_u64().unwrap_or(0);
                if outside > 0 {
                    msg += &crate::i18n::tr_format!("; {outside} outside its time range", outside = outside);
                }
                let kept = sk["hasGps"].as_u64().unwrap_or(0);
                if kept > 0 {
                    msg += &crate::i18n::tr_format!("; {kept} already had a location", kept = kept);
                }
                let ctx = app.tasks.repaint.clone().unwrap_or_default();
                app.toast(&ctx, msg);
            }
            r
        }
        "photo.locate" => {
            let Some(id) = app.session.active() else { return Some(Err("no photo selected".into())) };
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => Some(x.to_string()),
                None => {
                    let req =
                        PickRequest::file(crate::i18n::tr("Locate Missing File…"), crate::i18n::tr("Photos"), lightcraft_engine::import::EXTENSIONS);
                    match crate::pick::ask(app, "photo.locate", p, "path", req, |s| s.pick_files.as_mut().map(|f| f())) {
                        Picked::Now(v) => v.into_iter().next(),
                        Picked::Later | Picked::Unavailable => None,
                    }
                }
            };
            match path {
                Some(path) => app.run("photo.relink", json!({"id": id.0, "path": path})),
                None => Ok(Value::Null),
            }
        }
        "file.addFromDevice" => {
            // a camera / card: review its DCIM folder, copying into the library by default
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => x.to_string(),
                None => match lightcraft_engine::devices::devices_now().into_iter().next() {
                    Some(d) => d.path,
                    None => return Some(Err("no camera or memory card found".into())),
                },
            };
            let r = crate::import::open(app, vec![path]);
            // only the scan just started (an error means another scan is running)
            if r.is_ok()
                && let Some(t) = &mut app.scan
            {
                t.copy = true;
            }
            import_into_album(app, p, r)
        }
        "file.addFolder" => {
            // a folder (searched recursively) into the import review
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => Some(x.to_string()),
                None => match crate::pick::ask(app, id, p, "path", PickRequest::folder(crate::i18n::tr("Import from Folder…")), |s| {
                    s.pick_folder.as_mut().and_then(|f| f()).map(|x| vec![x])
                }) {
                    Picked::Now(v) => v.into_iter().next(),
                    Picked::Later | Picked::Unavailable => None,
                },
            };
            match path {
                Some(path) => {
                    let r = crate::import::open(app, vec![path]);
                    import_into_album(app, p, r)
                }
                None => Ok(Value::Null),
            }
        }
        "file.importPresets" => {
            let paths = match p.get("paths").and_then(Value::as_array) {
                Some(a) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
                None => {
                    let req = PickRequest::files(
                        Some(crate::i18n::tr("Import Presets").into()),
                        crate::i18n::tr("Presets & Profiles"),
                        crate::pick::PRESET_EXTENSIONS,
                    );
                    match crate::pick::ask(app, id, p, "paths", req, |s| s.pick_preset_files.as_mut().map(|f| f())) {
                        Picked::Now(paths) => paths,
                        Picked::Later => return Some(Ok(Value::Null)),
                        Picked::Unavailable => return Some(Err("no file dialog on this platform".into())),
                    }
                }
            };
            if paths.is_empty() {
                return Some(Ok(Value::Null));
            }
            // .cube LUTs (and those inside zips / folders) become profiles
            let lut_paths: Vec<&String> = paths
                .iter()
                .filter(|p| {
                    let l = p.to_ascii_lowercase();
                    l.ends_with(".cube") || l.ends_with(".zip") || std::path::Path::new(p.as_str()).is_dir()
                })
                .collect();
            // one toast for both halves (a second toast would replace the first)
            let mut parts: Vec<String> = Vec::new();
            // `name: reason` for each file that couldn't be imported
            let mut problems: Vec<String> = Vec::new();
            let mut profiles_result = Value::Null;
            if !lut_paths.is_empty() {
                match app.session.execute("profile.import", &json!({"paths": lut_paths})) {
                    Ok(v) => {
                        let profiles = v["imported"].as_array().map_or(0, Vec::len);
                        if profiles > 0 {
                            parts.push(crate::i18n::tr_format!(
                                "Imported {profiles} profile{} (Profile browser ▸ their groups)",
                                if profiles == 1 { "" } else { "s" },
                                profiles = profiles
                            ));
                        }
                        problems.extend(v["failed"].as_array().into_iter().flatten().map(|f| {
                            let name = f.get(0).and_then(Value::as_str).unwrap_or_default();
                            let reason = f.get(1).and_then(Value::as_str).unwrap_or_default();
                            format!("{name}: {reason}")
                        }));
                        profiles_result = v;
                    }
                    Err(e) => {
                        problems.push(e.to_string());
                        profiles_result = json!({"error": e.to_string()});
                    }
                }
            }
            let preset_paths: Vec<&String> = paths.iter().filter(|p| !p.to_ascii_lowercase().ends_with(".cube")).collect();
            let r = if preset_paths.is_empty() {
                Ok(json!({"imported": [], "failed": [], "skipped": 0}))
            } else {
                app.session.execute("preset.import", &json!({"paths": preset_paths})).map_err(|e| e.to_string())
            };
            if let (false, Ok(v)) = (preset_paths.is_empty(), &r) {
                let n = v["imported"].as_array().map_or(0, Vec::len);
                let failed = v["failed"].as_array().map_or(0, Vec::len);
                // Lightroom local adjustment presets land in the Masking panel's Effect menu: part of the same toast
                let effects = v["effects"].as_array().map_or(0, Vec::len);
                if effects > 0 {
                    parts.push(crate::panels::mask_effects::import_toast(v));
                }
                let profile_news = !parts.is_empty() || !problems.is_empty();
                let msg = match (n, failed) {
                    // a zip of LUTs (or only local presets): the other half already says what happened
                    (0, 0) if profile_news => None,
                    (0, 0) => Some("No new presets".to_string()),
                    (n, 0) => Some(crate::i18n::tr_format!("Imported {n} preset{}", if n == 1 { "" } else { "s" }, n = n)),
                    (n, f) => Some(crate::i18n::tr_format!(
                        "Imported {n} preset{}, {f} file{} not readable",
                        if n == 1 { "" } else { "s" },
                        if f == 1 { "" } else { "s" },
                        f = f,
                        n = n
                    )),
                };
                if let Some(mut msg) = msg {
                    // settings with no counterpart here (the other editor's profiles, masks…)
                    let mut skipped: Vec<&str> = v["imported"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .flat_map(|i| i["unmapped"].as_array().into_iter().flatten())
                        .filter_map(Value::as_str)
                        .collect();
                    skipped.sort_unstable();
                    skipped.dedup();
                    if !skipped.is_empty() {
                        let names: Vec<&str> = skipped.iter().take(3).copied().collect();
                        msg += &crate::i18n::tr_format!(" — not carried over: {}{}", names.join(", "), if skipped.len() > 3 { "…" } else { "" });
                    }
                    parts.push(msg);
                }
                if n > 0 {
                    app.ui.presets = true;
                }
            }
            let mut msg = parts.join(" · ");
            if !problems.is_empty() {
                let detail =
                    format!("{}{}", problems.iter().take(2).cloned().collect::<Vec<_>>().join("; "), if problems.len() > 2 { "…" } else { "" });
                if msg.is_empty() {
                    msg = crate::i18n::tr_format!("Import failed: {e}", e = detail);
                } else {
                    msg += &crate::i18n::tr_format!(" · {} failed: {}", problems.len(), detail);
                }
                app.toast_error(&ctx, msg);
            } else if !msg.is_empty() {
                app.toast(&ctx, msg);
            }
            let r = r.map(|mut v| {
                if !lut_paths.is_empty()
                    && let Some(o) = v.as_object_mut()
                {
                    o.insert("profiles".into(), profiles_result);
                }
                v
            });
            return Some(r);
        }
        "file.importKeywords" => {
            // a keyword list file: Lightroom Classic's, Capture One's, Photo Supreme's (.utf8)
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => x.to_string(),
                None => {
                    let req = PickRequest::file(crate::i18n::tr("Import Keywords"), crate::i18n::tr("Keyword Lists"), &["txt", "utf8"]);
                    match crate::pick::ask(app, id, p, "path", req, |s| s.pick_keyword_list.as_mut().map(|f| f())) {
                        Picked::Now(v) => match v.into_iter().next() {
                            Some(x) => x,
                            None => return Some(Ok(Value::Null)),
                        },
                        Picked::Later => return Some(Ok(Value::Null)),
                        Picked::Unavailable => return Some(Err("no file dialog on this platform".into())),
                    }
                }
            };
            let r = app.session.execute("keyword.import", &json!({"path": path})).map_err(|e| e.to_string());
            match &r {
                Ok(v) => {
                    let (added, updated) = (v["added"].as_u64().unwrap_or(0), v["updated"].as_u64().unwrap_or(0));
                    let mut msg = crate::i18n::tr_format!("Added {n} keyword{}", if added == 1 { "" } else { "s" }, n = added);
                    if updated > 0 {
                        msg.push_str(&crate::i18n::tr_format!("; {n} gained synonyms", n = updated));
                    }
                    app.toast(&ctx, msg);
                }
                Err(e) => app.toast(&ctx, e.clone()),
            }
            return Some(r);
        }
        "file.exportKeywords" => {
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => x.to_string(),
                None => {
                    let name = "Keywords.txt";
                    let req = PickRequest::save(crate::i18n::tr("Export Keywords"), crate::i18n::tr("Keyword Lists"), &["txt"], name);
                    match crate::pick::ask(app, id, p, "path", req, |s| s.save_keyword_list.as_mut().map(|f| f(name).into_iter().collect())) {
                        Picked::Now(v) => match v.into_iter().next() {
                            Some(x) => x,
                            None => return Some(Ok(Value::Null)),
                        },
                        Picked::Later => return Some(Ok(Value::Null)),
                        Picked::Unavailable => return Some(Err("no file dialog on this platform".into())),
                    }
                }
            };
            let r = app.session.execute("keyword.export", &json!({"path": path})).map_err(|e| e.to_string());
            if let Err(e) = &r {
                app.toast(&ctx, e.clone());
            }
            if let Ok(v) = &r {
                let n = v["keywords"].as_u64().unwrap_or(0);
                let mut msg = crate::i18n::tr_format!("Exported {n} keyword{}", if n == 1 { "" } else { "s" }, n = n);
                // Capture One's importer refuses ; , < > in a list: say which to rename first
                let refuses: Vec<&str> = v["captureOneRefuses"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                if !refuses.is_empty() {
                    msg.push_str(&crate::i18n::tr_format!(
                        " — Capture One won't import a list with ; , < or > in a keyword: {names}",
                        names = refuses.iter().take(5).copied().collect::<Vec<_>>().join(" · ")
                    ));
                }
                // what the format can't hold (a name in brackets, a line break…) isn't in the file
                let left: Vec<&str> = v["unwritable"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                if !left.is_empty() {
                    msg.push_str(&crate::i18n::tr_format!(
                        " — left out, as a keyword list can't hold them: {names}",
                        // escaped: a line break in a name would break the toast
                        names = left.iter().take(5).map(|n| n.escape_debug().to_string()).collect::<Vec<_>>().join(" · ")
                    ));
                }
                app.toast(&ctx, msg);
            }
            return Some(r);
        }
        "dialog.saveMaskEffect" => {
            if app.session.active_mask.is_none() {
                return Some(Err("select a mask first".into()));
            }
            crate::panels::mask_effects::open_save(app);
            Ok(Value::Null)
        }
        "dialog.manageMaskEffects" => {
            crate::panels::mask_effects::open_manage(app);
            Ok(Value::Null)
        }
        "file.exportMaskEffects" => {
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => Some(x.to_string()),
                None => {
                    let name = format!("{}.lceffect", p.get("group").and_then(Value::as_str).unwrap_or("Effect Presets"));
                    let req = PickRequest::save(
                        crate::i18n::tr("Export Effect Presets"),
                        crate::i18n::tr("LightCraft Effect Presets"),
                        &["lceffect"],
                        name.clone(),
                    );
                    match crate::pick::ask(app, id, p, "path", req, |s| s.save_preset_file.as_mut().map(|f| f(&name).into_iter().collect())) {
                        Picked::Now(v) => v.into_iter().next(),
                        Picked::Later => return Some(Ok(Value::Null)),
                        Picked::Unavailable => return Some(Err("no file dialog on this platform".into())),
                    }
                }
            };
            let Some(path) = path else { return Some(Ok(Value::Null)) };
            let mut params = json!({"path": path});
            for k in ["names", "group"] {
                if let Some(v) = p.get(k) {
                    params[k] = v.clone();
                }
            }
            let r = app.session.execute("mask.exportEffects", &params).map_err(|e| e.to_string());
            match &r {
                Ok(v) => app.toast(&ctx, crate::i18n::tr_format!("Exported {} effect preset{}", v["count"], if v["count"] == 1 { "" } else { "s" })),
                Err(e) => app.toast_error(&ctx, e.clone()),
            }
            return Some(r);
        }
        "file.exportPresets" => {
            let group = p.get("group").and_then(Value::as_str).map(str::to_string);
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => Some(x.to_string()),
                None => {
                    let name = format!("{}.lcpreset", group.as_deref().unwrap_or("LightCraft Presets"));
                    let req = PickRequest::save(crate::i18n::tr("Export Presets"), crate::i18n::tr("LightCraft Preset"), &["lcpreset"], name.clone());
                    match crate::pick::ask(app, id, p, "path", req, |s| s.save_preset_file.as_mut().map(|f| f(&name).into_iter().collect())) {
                        Picked::Now(v) => v.into_iter().next(),
                        Picked::Later => return Some(Ok(Value::Null)),
                        Picked::Unavailable => return Some(Err("no file dialog on this platform".into())),
                    }
                }
            };
            let Some(path) = path else { return Some(Ok(Value::Null)) };
            let mut params = json!({"path": path});
            if let Some(g) = group {
                params["group"] = json!(g);
            }
            if let Some(ids) = p.get("ids") {
                params["ids"] = ids.clone();
            }
            let r = app.session.execute("preset.export", &params).map_err(|e| e.to_string());
            if let Ok(v) = &r {
                app.toast(&ctx, crate::i18n::tr_format!("Exported {} preset{}", v["count"], if v["count"] == 1 { "" } else { "s" }));
            }
            return Some(r);
        }
        "file.importCurvePresets" => {
            let paths: Vec<String> = match p.get("paths").and_then(Value::as_array) {
                Some(a) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
                None => {
                    let req = PickRequest::files(
                        Some(crate::i18n::tr("Import Point Curve Presets").into()),
                        crate::i18n::tr("Point Curve Presets"),
                        crate::pick::CURVE_PRESET_EXTENSIONS,
                    );
                    match crate::pick::ask(app, id, p, "paths", req, |s| s.pick_curve_preset_files.as_mut().map(|f| f())) {
                        Picked::Now(paths) => paths,
                        Picked::Later => return Some(Ok(Value::Null)),
                        Picked::Unavailable => return Some(Err("no file dialog on this platform".into())),
                    }
                }
            };
            if paths.is_empty() {
                return Some(Ok(Value::Null));
            }
            let r = app.session.execute("curve.importPresets", &json!({"paths": paths})).map_err(|e| e.to_string());
            if let Ok(v) = &r {
                let n = v["imported"].as_array().map_or(0, Vec::len);
                let failed = v["failed"].as_array().map_or(0, Vec::len);
                let mut msg = crate::i18n::tr_format!("Imported {n} point curve preset{}", if n == 1 { "" } else { "s" }, n = n);
                if failed > 0 {
                    msg += &crate::i18n::tr_format!(", {failed} file{} not readable", if failed == 1 { "" } else { "s" }, failed = failed);
                }
                app.toast(&ctx, msg);
            }
            return Some(r);
        }
        "file.exportCurvePresets" => {
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => Some(x.to_string()),
                None => {
                    let req = PickRequest::save(
                        crate::i18n::tr("Export Point Curve Presets"),
                        crate::i18n::tr("Point Curve Presets"),
                        &["lccurve"],
                        "Point Curves.lccurve",
                    );
                    match crate::pick::ask(app, id, p, "path", req, |s| {
                        s.save_curve_preset_file.as_mut().map(|f| f("Point Curves.lccurve").into_iter().collect())
                    }) {
                        Picked::Now(v) => v.into_iter().next(),
                        Picked::Later => return Some(Ok(Value::Null)),
                        Picked::Unavailable => return Some(Err("no file dialog on this platform".into())),
                    }
                }
            };
            let Some(path) = path else { return Some(Ok(Value::Null)) };
            let mut params = json!({"path": path});
            if let Some(n) = p.get("names") {
                params["names"] = n.clone();
            }
            let r = app.session.execute("curve.exportPresets", &params).map_err(|e| e.to_string());
            if let Ok(v) = &r {
                app.toast(&ctx, crate::i18n::tr_format!("Exported {} point curve preset{}", v["count"], if v["count"] == 1 { "" } else { "s" }));
            }
            return Some(r);
        }
        "app.export" => crate::control::export_active(app, p),
        "app.showInFinder" => show_in_finder(app),
        "app.openLogFolder" => open_log_folder(app),
        "app.discord" | "app.website" | "app.github" | "app.artcraft" | "app.help" | "app.feedback" => {
            let url = crate::links::url_of(id).unwrap_or(crate::links::WEBSITE);
            crate::links::open(app, url)
        }
        "app.exportPrevious" => match app.session.last_export.clone() {
            Some(prev) => crate::control::export_active(app, &lightcraft_engine::export::ExportOptions::known_keys_only(&prev)),
            None => Err("nothing exported yet — use Export…".into()),
        },
        _ => return None,
    };
    Some(r)
}

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
            app.services.open_with.is_some()
                && app
                    .session
                    .active()
                    .and_then(|id| app.session.catalog.photo(id))
                    .is_some_and(|p| matches!(p.source, lightcraft_engine::catalog::Source::File { .. }))
        }
        "app.showInFinder" => {
            app.services.reveal.is_some()
                && app
                    .session
                    .active()
                    .and_then(|id| app.session.catalog.photo(id))
                    .is_some_and(|p| matches!(p.source, lightcraft_engine::catalog::Source::File { .. }))
        }
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

/// The Delete key in an album: a normal album loses the selected photos (`album.removePhotos`),
/// which stay in the library and their folder; a smart album keeps them (its rules decide) and
/// says so. True when the key was handled here, false outside albums (then it deletes the photos
/// from the library). Photo ▸ Delete Photo still deletes from the library everywhere.
pub fn delete_in_album(app: &mut LightcraftApp, ctx: &egui::Context) -> bool {
    let lightcraft_engine::LibrarySource::Album(aid) = app.session.source else { return false };
    let Some(album) = app.session.catalog.album(aid) else { return false };
    if album.is_smart() {
        app.toast(ctx, crate::i18n::tr("Smart albums update automatically: change their rules, or use Photo ▸ Delete Photo"));
        return true;
    }
    if app.session.selected().is_empty() {
        return true;
    }
    match app.run("album.removePhotos", json!({"id": aid.0})) {
        Ok(r) => {
            let n = r["removed"].as_u64().unwrap_or(0);
            let name = r["album"].as_str().unwrap_or_default().to_string();
            let what = crate::i18n::tr_format!("{n} photo{}", if n == 1 { "" } else { "s" }, n = n);
            app.toast(ctx, crate::i18n::tr_format!("Removed {what} from {name}", what = what, name = name));
        }
        Err(e) => app.toast(ctx, e),
    }
    true
}

/// With Settings → General → "Confirm before deleting" on, open the confirmation dialog instead
/// of deleting; true when it did (the dialog's OK runs `photo.delete`).
pub fn confirm_delete(app: &mut LightcraftApp) -> bool {
    if !app.ui.settings.confirm_delete {
        return false;
    }
    let count = app.session.selected().len();
    if count == 0 {
        return false;
    }
    app.ui.dialog = Some(Dialog::ConfirmDelete { count });
    true
}

/// Platform-appropriate label for revealing a file in the system file manager.
pub fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Show in Finder"
    } else if cfg!(target_os = "windows") {
        "Show in Explorer"
    } else {
        "Show in File Manager"
    }
}

/// Help ▸ Open Log Folder: reveal the host's log file in the system file manager, so it can be
/// attached to a report without hunting for the settings folder (#260).
fn open_log_folder(app: &mut LightcraftApp) -> Result<Value, String> {
    let path = app.services.log_file.clone().ok_or("this session keeps no log file")?;
    let reveal = app.services.reveal.as_mut().ok_or("not available here")?;
    reveal(&path)?;
    Ok(json!({"path": path}))
}

/// Reveal the active photo's original in the system file manager.
fn show_in_finder(app: &mut LightcraftApp) -> Result<Value, String> {
    let id = app.session.active().ok_or("no photo selected")?;
    let path = match app.session.catalog.photo(id).map(|p| p.source.clone()) {
        Some(lightcraft_engine::catalog::Source::File { path }) => path,
        _ => return Err("this photo has no file (demo scene)".into()),
    };
    let reveal = app.services.reveal.as_mut().ok_or("not available here")?;
    reveal(&path)?;
    Ok(json!({"path": path}))
}

/// Photo ▸ Edit In: render a copy of each selected photo with its edits (stacked on its original)
/// as Settings ▸ External Editing asks for that editor, then open them there. `p` may override the
/// file (`format`, `colorSpace`, `bitDepth`) and the editor (`app`). PhotoCraft is checked first,
/// so a missing editor leaves no copy behind.
fn edit_in(app: &mut LightcraftApp, ctx: &egui::Context, p: &Value, photocraft: bool) -> Result<Value, String> {
    let settings = &app.ui.settings;
    let (file, configured) = if photocraft {
        let path = settings.photocraft_path.trim();
        (settings.photocraft_file, if path.is_empty() { crate::state::PHOTOCRAFT.to_string() } else { path.to_string() })
    } else {
        (settings.external_file, settings.external_editor.clone())
    };
    let editor = p.get("app").and_then(Value::as_str).map_or(configured, str::to_string);
    if let Some(check) = app.services.check_editor.as_ref()
        && let Err(e) = check(&editor)
    {
        app.toast(ctx, e.clone());
        return Err(e);
    }
    let mut params = file.params();
    if let (Some(o), Some(given)) = (params.as_object_mut(), p.as_object()) {
        for (k, v) in given.iter().filter(|(k, _)| k.as_str() != "app") {
            o.insert(k.clone(), v.clone());
        }
    }
    let r = app.session.execute("photo.editExternal", &params).map_err(|e| e.to_string())?;
    // every selected photo got a copy (#920); each opens in the editor
    let copies: Vec<(String, Option<u64>)> = r["copies"]
        .as_array()
        .map(|c| c.iter().map(|c| (c["path"].as_str().unwrap_or_default().to_string(), c["id"].as_u64())).collect())
        .unwrap_or_else(|| vec![(r["path"].as_str().unwrap_or_default().to_string(), r["id"].as_u64())]);
    let mut failed = None;
    for (path, id) in &copies {
        if let Some(id) = id {
            app.ui.external_edits.push(*id);
        }
        if let Some(Err(e)) = app.services.open_with.as_mut().map(|f| f(path, &editor)) {
            failed = Some(e);
        }
    }
    match (failed, copies.as_slice()) {
        (Some(e), _) => app.toast(ctx, crate::i18n::tr_format!("Couldn't open the editor: {e}", e = e)),
        (None, [(path, _)]) => {
            let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            app.toast(ctx, crate::i18n::tr_format!("{name} opened for editing; it is stacked with the original", name = name));
        }
        _ => app.toast(ctx, crate::i18n::tr_format!("{n} photos opened for editing; each is stacked with its original", n = copies.len())),
    }
    Ok(r)
}

/// Most originals one Open Original hands the editor (each starts it once).
const MAX_ORIGINALS: usize = 50;

/// Photo ▸ Edit In ▸ Open Original in Other Editor: hand each selected photo's original file
/// itself (a raw stays a raw: nothing is rendered, copied or added to the library) to the editor
/// set in Settings ▸ External Editing, e.g. a raw developer. `p` may give `ids` and the editor
/// (`app`). Demo photos and missing files are skipped and listed; when no photo has a file to
/// open, it fails saying why.
fn open_original_in(app: &mut LightcraftApp, ctx: &egui::Context, p: &Value) -> Result<Value, String> {
    let r = open_originals(app, p);
    if let Err(e) = &r {
        app.toast(ctx, e.clone());
    }
    let r = r?;
    let opened = r["opened"].as_array().map_or(0, Vec::len);
    match (r["failed"].as_str(), r["opened"][0]["path"].as_str()) {
        (Some(e), _) => app.toast(ctx, crate::i18n::tr_format!("Couldn't open the editor: {e}", e = e)),
        (None, Some(path)) if opened == 1 => {
            let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            app.toast(ctx, crate::i18n::tr_format!("{name} opened in the other editor", name = name));
        }
        _ => app.toast(ctx, crate::i18n::tr_format!("{n} originals opened in the other editor", n = opened)),
    }
    Ok(r)
}

fn open_originals(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    let mut ids = app.session.targets(p).map_err(|e| e.to_string())?;
    ids.dedup();
    if ids.is_empty() {
        return Err("no photo selected".into());
    }
    if ids.len() > MAX_ORIGINALS {
        return Err(format!("{} photos selected: open at most {MAX_ORIGINALS} originals at a time", ids.len()));
    }
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    for id in ids {
        match app.session.catalog.photo(id).map(|ph| &ph.source) {
            Some(lightcraft_engine::catalog::Source::File { path }) if std::path::Path::new(path).is_file() => files.push((id, path.clone())),
            Some(lightcraft_engine::catalog::Source::File { path }) => {
                skipped.push(json!({"id": id.0, "reason": format!("the original is missing: {path}")}))
            }
            Some(_) => skipped.push(json!({"id": id.0, "reason": "a demo photo has no original file"})),
            None => skipped.push(json!({"id": id.0, "reason": "no such photo"})),
        }
    }
    if files.is_empty() {
        let why = skipped.first().and_then(|s| s["reason"].as_str()).unwrap_or("no original file");
        return Err(if skipped.len() == 1 {
            format!("nothing to open: {why}")
        } else {
            format!("nothing to open: none of these photos has its original file ({why})")
        });
    }
    let editor = p.get("app").and_then(Value::as_str).map_or_else(|| app.ui.settings.external_editor.clone(), str::to_string);
    if let Some(check) = app.services.check_editor.as_ref() {
        check(&editor)?;
    }
    // all at once where the host can: an editor that keeps one window (DxO PhotoLab) loses the
    // files that arrive while it is still starting
    if let Some(open_all) = app.services.open_all_with.as_mut() {
        let paths: Vec<String> = files.iter().map(|(_, path)| path.clone()).collect();
        open_all(&paths, &editor).map_err(|e| format!("Couldn't open the editor: {e}"))?;
        let opened: Vec<Value> = files.iter().map(|(id, path)| json!({"id": id.0, "path": path})).collect();
        return Ok(json!({"opened": opened, "skipped": skipped, "app": editor, "failed": null}));
    }
    let open = app.services.open_with.as_mut().ok_or("opening another application isn't available here")?;
    let mut opened = Vec::new();
    let mut failed = None;
    for (id, path) in files {
        match open(&path, &editor) {
            Ok(()) => opened.push(json!({"id": id.0, "path": path})),
            Err(e) => failed = Some(e),
        }
    }
    if opened.is_empty()
        && let Some(e) = failed
    {
        return Err(format!("Couldn't open the editor: {e}"));
    }
    Ok(json!({"opened": opened, "skipped": skipped, "app": editor, "failed": failed}))
}
