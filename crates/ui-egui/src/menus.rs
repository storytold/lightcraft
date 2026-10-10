//! UI-level commands (views, panels, zoom, tools, dialogs) and the menu model shared by the native
//! menu bar, the shortcut handler and the control channel.

use serde::Serialize;
use serde_json::{Value, json};

use crate::DacApp;
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
];

/// Every UI command: the languages, then everything else. `xtask parity` reads both tables from
/// this file, so an id listed in `docs/parity.md` is checked wherever it is declared.
pub fn ui_commands() -> impl Iterator<Item = &'static UiCommand> {
    LANGUAGE_COMMANDS.iter().chain(UI_COMMANDS).chain(crate::module::SHELL_COMMANDS).chain(crate::map::COMMANDS).chain(crate::print_ui::COMMANDS)
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
        _ => None,
    }
}

pub const UI_COMMANDS: &[UiCommand] = &[
    ("modelSetup.cancel", "Cancel Pending Photo Action", None, ""),
    ("view.photoGrid", "Photo Grid", None, "View"),
    ("view.squareGrid", "Square Grid", None, "View"),
    // G: Photo Grid ↔ Square Grid (from other views: the photo grid)
    ("view.gridToggle", "Grid", Some("G"), ""),
    ("tool.guidedUpright", "Guided Upright", Some("Shift+G"), "Window>Tools"),
    ("view.detail", "Detail", Some("D"), "View"),
    ("view.compare", "Compare", Some("Shift+C"), "View"),
    ("view.survey", "Survey", Some("N"), "View"),
    ("view.people", "People", None, "View"),
    ("view.person", "Show Person", None, ""),
    ("view.faceBoxes", "Face Boxes", None, "View"),
    ("view.reference", "Reference View", Some("Shift+R"), "View"),
    ("photo.setReference", "Set as Reference Photo", None, ""),
    ("compare.swap", "Swap Compare Photos", None, "View"),
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
    ("view.zoomLevel", "Zoom Level", None, ""),
    ("view.zoomIn", "Zoom In", Some("Cmd+="), "View"),
    ("view.zoomOut", "Zoom Out", Some("Cmd+-"), "View"),
    ("view.clipping", "Show Clipping", Some("J"), "View"),
    ("view.visualizeHdr", "Visualize HDR", None, "View"),
    // in grids S expands/collapses stacks (the engine command it shadows)
    ("view.softProof", "Soft Proofing", Some("S"), "View"),
    ("view.histogram", "Histogram", Some("Cmd+Shift+H"), "View"),
    ("view.maskOverlay", "Show Mask Overlay", Some("O"), "View"),
    // Shift+O in the Masking panel (elsewhere it cycles the crop overlay)
    ("view.maskOverlayMode", "Cycle Mask Overlay Mode", None, "View"),
    ("view.maskOverlayColor", "Cycle Mask Overlay Color", None, "View"),
    ("view.maskPins", "Show Mask Pins", None, "View"),
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
    ("view.enterFullScreen", "Enter Full Screen", Some("Cmd+Shift+F"), "View"),
    ("view.infoOverlay", "Cycle Info Overlay", Some("Cmd+I"), "View"),
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
    ("section.light", "Light", Some("Cmd+1"), "Window>Edit Sections"),
    ("section.color", "Color", Some("Cmd+2"), "Window>Edit Sections"),
    ("section.effects", "Effects", Some("Cmd+3"), "Window>Edit Sections"),
    ("section.detail", "Detail", Some("Cmd+4"), "Window>Edit Sections"),
    ("section.optics", "Optics", Some("Cmd+5"), "Window>Edit Sections"),
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
    ("dialog.newAlbum", "New Album…", Some("Cmd+N"), "Library"),
    ("dialog.newFolder", "New Folder…", Some("Cmd+Shift+N"), "Library"),
    ("dialog.smartAlbum", "New Smart Album…", None, "Library"),
    ("view.photoCounts", "Show Photo Counts", None, "View"),
    ("view.slideshow", "Slideshow", Some("Cmd+Alt+Enter"), "View"),
    ("view.secondWindow", "Second Window", Some("Cmd+F11"), "Window"),
    ("tool.keywordPainter", "Keyword Painter", None, ""),
    ("tool.painter", "Painter", Some("Cmd+Alt+K"), "Photo"),
    ("view.gridCellStyle", "Grid Cell Style", None, ""),
    ("dialog.viewOptions", "View Options…", Some("Cmd+J"), "View"),
    ("view.cellCompact", "Compact Cells", None, "View>Grid View Style"),
    ("view.cellExpanded", "Expanded Cells", None, "View>Grid View Style"),
    ("view.cellIndex", "Show Index Numbers", None, "View>Grid View Style"),
    ("view.cellBadges", "Show Thumbnail Badges", None, "View>Grid View Style"),
    // Library's `=` / `-`, Home / End (module keys, see `crate::module::LIBRARY_KEYS`)
    ("view.thumbLarger", "Increase Thumbnail Size", None, "View"),
    ("view.thumbSmaller", "Decrease Thumbnail Size", None, "View"),
    ("library.first", "First Photo", None, ""),
    ("library.last", "Last Photo", None, ""),
    // Develop's ⇧Q: the selected spot's mode Remove → Heal → Clone
    ("spot.cycleMode", "Cycle Spot Mode", None, ""),
    ("metadata.panelPreset", "Metadata Panel Preset", None, ""),
    ("view.gridInfo", "Grid Info", None, ""),
    ("dialog.allMetadata", "All Metadata…", None, "Photo"),
    ("dialog.faceModel", "Add Face Model…", None, ""),
    ("dialog.newSmartAlbum", "New Smart Album from Filter…", Some("Cmd+Alt+N"), "Library"),
    ("dialog.createPreset", "Create Preset…", Some("Cmd+Shift+P"), "Photo"),
    ("dialog.autoStack", "Auto-Stack by Capture Time…", None, "Photo>Stack"),
    ("dialog.copySettings", "Choose Edit Settings to Copy…", Some("Cmd+Shift+C"), "Edit"),
    ("dialog.pasteSettings", "Paste Selected Settings…", Some("Cmd+Shift+V"), "Edit"),
    ("view.focusSearch", "Find…", Some("Cmd+F"), "Library"),
    ("dialog.export", "Export…", None, "File"),
    ("dialog.contactSheet", "Contact Sheet PDF…", None, "File"),
    ("app.contactSheet", "Export Contact Sheet PDF", None, ""),
    ("photo.editInExternal", "Edit in External Editor", Some("Cmd+Shift+E"), "Photo"),
    ("dialog.mergeHdr", "HDR…", Some("Ctrl+H"), "Photo>Photo Merge"),
    ("dialog.mergePanorama", "Panorama…", Some("Ctrl+M"), "Photo>Photo Merge"),
    ("dialog.mergeHdrPanorama", "HDR Panorama…", None, "Photo>Photo Merge"),
    ("merge.hdrLast", "HDR with Last Settings", Some("Ctrl+Shift+H"), "Photo>Photo Merge"),
    ("merge.panoramaLast", "Panorama with Last Settings", Some("Ctrl+Shift+M"), "Photo>Photo Merge"),
    ("merge.hdrPanoramaLast", "HDR Panorama with Last Settings", None, "Photo>Photo Merge"),
    ("file.addPhotos", "Import Photos…", Some("Cmd+Shift+I"), "File"),
    ("file.addFolder", "Import from Folder…", None, "File"),
    ("file.importLightroom", "Import Lightroom Catalog…", None, "File"),
    ("file.importImmich", "Import from Immich…", None, "File"),
    ("immich.connections", "Immich Connections…", None, "Library>Immich"),
    ("file.addFromDevice", "Import from Device", None, ""),
    ("file.findMissing", "Find Missing Photos…", None, "Library"),
    ("file.backupLibrary", "Back Up Library…", None, "File"),
    ("file.restoreLibrary", "Restore Library from Backup…", None, "File"),
    ("photo.locate", "Locate Missing File…", None, ""),
    ("dialog.saveMetadataPreset", "Save Metadata Preset…", None, ""),
    ("app.quit", "Quit {app}", Some("Cmd+Q"), "File"),
    ("file.importPresets", "Import Profiles & Presets…", None, "File"),
    ("file.exportPresets", "Export Presets…", None, "File"),
    ("file.importKeywords", "Import Keywords…", None, "File"),
    ("file.exportKeywords", "Export Keywords…", None, "File"),
    // Edit panel ▸ Curve ▸ Point Curve dropdown
    ("file.importCurvePresets", "Import Point Curve Presets…", None, ""),
    ("file.exportCurvePresets", "Export Point Curve Presets…", None, ""),
    ("app.settings", "Settings…", Some("Cmd+,"), "Edit"),
    ("app.openLibrary", "Open Library…", None, "File"),
    // catalogs (crate::catalog_ui)
    ("catalog.new", "New Catalog…", None, "File"),
    ("catalog.open", "Open Catalog…", None, "File"),
    ("catalog.openRecent", "Open Recent Catalog", None, ""),
    ("catalog.chooser", "Choose Catalog…", None, ""),
    ("catalog.promptAtStartup", "Show the Catalog Chooser at Startup", None, ""),
    ("dialog.catalogSettings", "Catalog Settings…", None, "Edit"),
    ("dialog.exportCatalog", "Export as Catalog…", None, "File"),
    ("dialog.importCatalog", "Import from Another Catalog…", None, "File"),
    // Settings ▸ Display
    ("app.displayProfile", "Display Profile", None, ""),
    ("app.about", "About {app}", None, "Help"),
    ("app.systemInfo", "System Info…", None, "Help"),
    ("app.openLogFolder", "Open Log Folder", None, "Help"),
    ("app.whatsNew", "What's New", None, "Help"),
    ("dialog.cull", "Assisted Culling…", None, "Photo"),
    ("app.help", "{app} Help", Some("F1"), "Help"),
    ("app.feedback", "Send Feedback…", None, "Help"),
    ("app.website", "{app} Website", None, "Help"),
    ("app.github", "{app} Source Code", None, "Help"),
    ("app.shortcuts", "Keyboard Shortcuts", Some("Cmd+/"), "Help"),
    ("app.setShortcut", "Set Keyboard Shortcut", None, ""),
    ("app.resetShortcuts", "Reset All Keyboard Shortcuts", None, ""),
    ("app.keymapSet", "Keymap Set", None, ""),
    ("app.keymapExport", "Export Keymap…", None, "Help"),
    ("app.keymapImport", "Import Keymap…", None, "Help"),
    ("app.export", "Export Now", None, ""),
    ("app.showInFinder", "Show in Finder", Some("Cmd+R"), "Photo"),
    ("dialog.rename", "Rename Photos…", Some("F2"), "Library"),
    ("dialog.labelNames", "Edit Color Label Names…", None, ""),
    ("dialog.captureTime", "Edit Capture Time…", None, "Photo"),
    ("photo.tagFromTracklog", "Auto-Tag from Tracklog…", None, "Photo"),
    ("app.exportPrevious", "Export with Previous", Some("Cmd+Alt+Shift+E"), "File"),
];

fn panel(app: &mut DacApp, ctx: &egui::Context, p: RightPanel, name: &str) {
    if app.ui.right == p {
        app.ui.right = RightPanel::None;
        app.toast(ctx, crate::i18n::tr_format!("{name} Off", name = crate::i18n::tr(name)));
    } else {
        app.ui.right = p;
        // opening a panel brings its edge back (F8 / Tab hid it)
        app.ui.right_edge = true;
        app.ui.hidden_panels.retain(|h| h.right_panel() != Some(p));
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
fn adjust_brush(app: &mut DacApp, k: f32, df: f32) -> Value {
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

/// Handle UI commands; `None` means "not a UI command — send it to the engine".
pub fn run_ui_command(app: &mut DacApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if matches!(id, "library.inspectLightroom" | "library.importLightroom") {
        // the app's own context: a fresh one's repaints reach no window and its clock starts at zero
        let ctx = app.tasks.repaint.clone().unwrap_or_default();
        return Some(crate::lightroom_import::command(app, id, p, &ctx));
    }
    if let Some(language) = language_from_command(id) {
        app.ui.language = language;
        // Immediately, not on the next frame: the reply and anything else run this frame
        // (menus rebuilt from it, toasts) are already in the new language.
        crate::i18n::set_language(app.ui.language);
        return Some(Ok(json!(app.ui.language)));
    }
    if let Some(r) = crate::module::run(app, id, p) {
        return Some(r);
    }
    if let Some(r) = crate::catalog_ui::run(app, id, p) {
        return Some(r);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(r) = crate::panels::connections::run(app, id, p) {
        return Some(r);
    }
    if let Some(r) = crate::libtools::run(app, id, p) {
        return Some(r);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(r) = crate::panels::tether_bar::run(app, id, p) {
        return Some(r);
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
            Ok(json!({"info": app.ui.grid_info}))
        }
        "view.secondWindow" => {
            app.ui.second_window = p.get("show").and_then(Value::as_bool).unwrap_or(!app.ui.second_window);
            Ok(json!({"show": app.ui.second_window}))
        }
        "view.photoCounts" => {
            app.ui.show_counts = p.get("show").and_then(Value::as_bool).unwrap_or(!app.ui.show_counts);
            Ok(json!({"show": app.ui.show_counts}))
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
                .filter(|r| app.session.catalog.photo(dac_catalog::PhotoId(*r)).is_some())
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
        "compare.makeSelect" => crate::panels::compare::make_select(app),
        "view.autoAdvance" => {
            app.ui.auto_advance = !app.ui.auto_advance;
            app.toast(&ctx, if app.ui.auto_advance { "Auto Advance On" } else { "Auto Advance Off" });
            Ok(json!({"autoAdvance": app.ui.auto_advance}))
        }
        "view.back" => {
            if app.ui.dialog.is_some() {
                app.ui.dialog = None;
            } else if app.ui.lib.painter.is_some() {
                app.ui.lib.painter = None;
            } else if app.ui.fullscreen {
                app.ui.fullscreen = false;
                app.ui.slideshow = None;
            } else if !app.ui.tool.is_empty() {
                app.ui.tool.clear();
            } else if app.ui.view == ViewMode::People && app.ui.person_page.is_some() {
                app.ui.person_page = None;
            } else if matches!(app.ui.view, ViewMode::Compare | ViewMode::Survey) {
                app.ui.view = ViewMode::Detail;
            } else if app.ui.view == ViewMode::Detail {
                // a photo opened from a person's page goes back to that page, any other to the grid
                match app.ui.person_from.take() {
                    Some((name, photo)) if app.session.active().is_some_and(|a| a.0 == photo) => {
                        app.ui.view = ViewMode::People;
                        app.ui.person_page = Some(name);
                    }
                    _ => app.ui.view = ViewMode::PhotoGrid,
                }
            }
            Ok(Value::Null)
        }
        "view.slideshow" => {
            // {interval?: seconds (4)}: the photos in view, full screen, one after another
            let interval = p.get("interval").and_then(Value::as_f64).unwrap_or(4.0).clamp(0.5, 120.0);
            if app.session.active().is_none() {
                let first = app.session.visible_cloned().first().copied();
                match first {
                    Some(f) => {
                        let _ = app.run("library.select", json!({"ids": [f.0]}));
                    }
                    None => return Some(Err("no photos to show".into())),
                }
            }
            let now = ctx.input(|i| i.time);
            app.ui.slideshow = Some((interval, now + interval, false));
            app.ui.fullscreen = true;
            app.ui.zoom = Zoom::Fit;
            app.ui.tool.clear();
            let _ = app.session.end_interaction();
            app.toast(&ctx, crate::i18n::tr("Slideshow · Space pauses · Esc ends"));
            Ok(json!({"interval": interval}))
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
            // the same ratio a click on the photo zooms to
            app.ui.zoom = if app.ui.zoom == Zoom::Fit { Zoom::Percent(app.ui.click_zoom as f32) } else { Zoom::Fit };
            app.ui.zoom_anim = true;
            Ok(Value::Null)
        }
        "view.clickZoom" => {
            // {ratio?: 1|2|3|4|8|11} → {ratio}
            if let Some(r) = p.get("ratio").and_then(Value::as_f64) {
                let pct = (r * 100.0).round() as u32;
                if !crate::state::CLICK_ZOOMS.contains(&pct) {
                    return Some(Err(format!("view.clickZoom: ratio {r} (1, 2, 3, 4, 8 or 11)")));
                }
                app.ui.click_zoom = pct;
            }
            Ok(json!({"ratio": app.ui.click_zoom / 100}))
        }
        "view.zoomIn" | "view.zoomOut" => {
            // the fixed levels 1:4 … 11:1 (state::ZOOM_LEVELS); out of the first one is Fit
            let cur = match app.ui.zoom {
                Zoom::Percent(p) => p,
                _ => 25.0,
            };
            app.ui.zoom = match crate::state::zoom_step(cur, id == "view.zoomIn") {
                Some(p) => Zoom::Percent(p),
                None => Zoom::Fit,
            };
            Ok(Value::Null)
        }
        "view.zoomLevel" => {
            // {level: fit|fill|1:4|1:3|1:2|1:1|2:1|3:1|4:1|8:1|11:1} → {zoom}
            const LEVELS: &str = "fit, fill, 1:4, 1:3, 1:2, 1:1, 2:1, 3:1, 4:1, 8:1 or 11:1";
            let Some(level) = p.get("level").and_then(Value::as_str) else {
                return Some(Err(format!("view.zoomLevel: level is required ({LEVELS})")));
            };
            let Some(z) = crate::state::zoom_level(level) else {
                return Some(Err(format!("view.zoomLevel: unknown level `{level}` ({LEVELS})")));
            };
            app.ui.zoom = z;
            app.ui.zoom_anim = true;
            Ok(json!({"zoom": z}))
        }
        "view.navigate" => {
            // {zoom?: "fit"|"fill"|{percent: number}, pan?: [x, y]} (normalized image centre).
            // Validate the complete request before changing either part of the viewport.
            let zoom = match p.get("zoom") {
                Some(v) => match serde_json::from_value::<Zoom>(v.clone()) {
                    Ok(Zoom::Percent(p)) if !p.is_finite() || p <= 0.0 || p > crate::state::MAX_ZOOM => {
                        return Some(Err("view.navigate: zoom percent must be greater than 0 and at most 1100".into()));
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
            // {on?, space?, destWarning?, displayWarning?}; no params toggles
            let has = |k: &str| p.get(k).is_some();
            if let Some(s) = p.get("space").and_then(Value::as_str) {
                match dac_engine::pipeline::OutputSpace::parse(s) {
                    Some(sp) => app.ui.proof.space = sp,
                    None => return Some(Err(format!("view.softProof: unknown space {s:?} (srgb|displayP3|adobeRgb|proPhoto|rec2020)"))),
                }
            }
            if let Some(b) = p.get("destWarning").and_then(Value::as_bool) {
                app.ui.proof.dest_warning = b;
            }
            if let Some(b) = p.get("displayWarning").and_then(Value::as_bool) {
                app.ui.proof.display_warning = b;
            }
            app.ui.soft_proof = match p.get("on").and_then(Value::as_bool) {
                Some(b) => b,
                None if has("space") || has("destWarning") || has("displayWarning") => app.ui.soft_proof,
                None => !app.ui.soft_proof,
            };
            if app.ui.soft_proof && !matches!(app.ui.view, ViewMode::Detail | ViewMode::Reference) {
                app.ui.view = ViewMode::Detail;
            }
            let pr = app.ui.proof;
            Ok(json!({"on": app.ui.soft_proof, "space": pr.space, "destWarning": pr.dest_warning, "displayWarning": pr.display_warning}))
        }
        "view.histogram" => {
            app.ui.histogram = !app.ui.histogram;
            Ok(Value::Null)
        }
        "view.maskOverlay" => {
            app.ui.mask_overlay = p.get("show").and_then(Value::as_bool).unwrap_or(!app.ui.mask_overlay);
            Ok(json!({"maskOverlay": app.ui.mask_overlay}))
        }
        "view.maskOverlayMode" => {
            use dac_engine::pipeline::MaskView;
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
        "dialog.viewOptions" => {
            app.ui.dialog = Some(Dialog::ViewOptions);
            Ok(Value::Null)
        }
        "view.cellCompact" | "view.cellExpanded" => {
            let style = if id == "view.cellCompact" { "compact" } else { "expanded" };
            let r = app.run("view.gridCellStyle", json!({"style": style}));
            if r.is_ok() && !matches!(app.ui.view, ViewMode::PhotoGrid | ViewMode::SquareGrid) {
                app.ui.view = ViewMode::SquareGrid;
            }
            r
        }
        "view.cellIndex" => {
            let on = !app.ui.lib.cell_index;
            app.run("view.gridCellStyle", json!({"index": on}))
        }
        "view.cellBadges" => {
            let on = !app.ui.lib.cell_badges;
            app.run("view.gridCellStyle", json!({"badges": on}))
        }
        "view.thumbLarger" | "view.thumbSmaller" => {
            let k = if id == "view.thumbLarger" { 1.15 } else { 1.0 / 1.15 };
            app.ui.thumb_size = (app.ui.thumb_size * k).clamp(90.0, 480.0);
            Ok(json!({"thumbSize": app.ui.thumb_size}))
        }
        "library.first" | "library.last" => {
            let vis = app.session.visible_cloned();
            let pick = if id == "library.first" { vis.first() } else { vis.last() };
            match pick {
                Some(pid) => app.run("library.select", json!({"ids": [pid.0]})).map(|_| json!({"active": pid.0})),
                None => Ok(json!({"active": null})),
            }
        }
        "spot.cycleMode" => {
            let cur = app.session.active_spot.and_then(|i| {
                let d = app.session.active().and_then(|a| app.session.develop_of(a))?;
                d.spots.get(i).map(|s| (i, s.mode))
            });
            match cur {
                None => Err("select a spot first".to_string()),
                Some((i, mode)) => {
                    let next = match mode {
                        dac_develop::SpotMode::Remove => "heal",
                        dac_develop::SpotMode::Heal => "clone",
                        dac_develop::SpotMode::Clone => "remove",
                    };
                    app.run("spot.update", json!({"index": i, "mode": next})).map(|_| json!({"mode": next}))
                }
            }
        }
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
            app.ui.right = if app.ui.right == RightPanel::Profiles { RightPanel::Edit } else { RightPanel::Profiles };
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
            app.ui.presets = !app.ui.presets;
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
        s if s.starts_with("section.") => {
            let sec = &s["section.".len()..];
            if app.ui.right != RightPanel::Edit {
                app.ui.right = RightPanel::Edit;
            }
            app.ui.toggle_section(sec);
            Ok(Value::Null)
        }
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
                        .is_some_and(|d| d.geometry.upright == dac_develop::Upright::Guided);
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
            app.ui.dialog =
                Some(Dialog::NewAlbum { name: p.get("name").and_then(Value::as_str).unwrap_or("").into(), folder: true, parent: parent_param(p) });
            Ok(Value::Null)
        }
        "dialog.newAlbum" => {
            app.ui.dialog =
                Some(Dialog::NewAlbum { name: p.get("name").and_then(Value::as_str).unwrap_or("").into(), folder: false, parent: parent_param(p) });
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
                .and_then(|id| app.session.catalog.album(dac_catalog::AlbumId(id)).filter(|a| a.is_smart()).cloned());
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
                    rules: dac_catalog::RuleSet { rules: vec![crate::panels::rules_editor::new_rule()], ..Default::default() },
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
            let names = dac_catalog::ColorLabel::ALL.iter().map(|l| app.session.catalog.custom_label_name(*l).unwrap_or("").to_string()).collect();
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
        "dialog.contactSheet" => {
            app.ui.dialog = Some(Dialog::ContactSheet { options: dac_engine::contact_sheet::Options::default() });
            Ok(Value::Null)
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
            let opts = dac_engine::export::ExportOptions::from_json(&prev);
            // no previous export: 2048 px long edge; a previous full-size export: full size
            let full_size = opts.resize.is_none() && dac_engine::export::ExportOptions::has_size_param(&prev);
            let resize = opts.resize.unwrap_or_default();
            app.ui.dialog = Some(Dialog::Export { opts, full_size, resize, preset_name: String::new(), limit_kb: u("limitKb", 0) as u32, dir });
            Ok(Value::Null)
        }
        "merge.hdrLast" => crate::merge::start_last(app, "merge.hdr"),
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
        "app.about" => {
            app.ui.dialog = Some(Dialog::About);
            Ok(Value::Null)
        }
        "photo.editInExternal" => {
            // render an edit copy (stacked on the original), then open it in the editor
            let mut params = p.clone();
            if !params.is_object() {
                params = json!({});
            }
            let r = match app.session.execute("photo.editExternal", &params) {
                Ok(r) => r,
                Err(e) => return Some(Err(e.to_string())),
            };
            let path = r["path"].as_str().unwrap_or_default().to_string();
            if let Some(id) = r["id"].as_u64() {
                app.ui.external_edits.push(id);
            }
            let editor = p.get("app").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| app.ui.settings.external_editor.clone());
            if let Some(f) = app.services.open_with.as_mut()
                && let Err(e) = f(&path, &editor)
            {
                app.toast(&ctx, crate::i18n::tr_format!("Couldn't open the editor: {e}", e = e));
            }
            let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            app.toast(&ctx, crate::i18n::tr_format!("{name} opened for editing; it is stacked with the original", name = name));
            Ok(r)
        }
        "dialog.cull" => {
            app.ui.dialog = Some(Dialog::Cull { reject_below: 0.0, pick_best: true });
            Ok(Value::Null)
        }
        "app.whatsNew" => {
            app.ui.dialog = Some(Dialog::WhatsNew);
            Ok(json!({"text": crate::i18n::brand_fill(crate::panels::dialogs::WHATS_NEW)}))
        }
        "app.systemInfo" => {
            let info = app.session.execute("library.info", &json!({})).unwrap_or_default();
            let gpu = (dac_engine::gpu::ready() && dac_engine::gpu::available()).then(dac_engine::gpu::adapter_name).flatten();
            let mb = |b: u64| format!("{:.0} MB", b as f64 / (1u64 << 20) as f64);
            let mut rows = vec![
                ("Version".to_string(), env!("CARGO_PKG_VERSION").to_string()),
                ("System".to_string(), format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH)),
                ("CPU threads".to_string(), std::thread::available_parallelism().map(|n| n.get().to_string()).unwrap_or_else(|_| "?".into())),
                (
                    "GPU".to_string(),
                    gpu.unwrap_or_else(|| match dac_engine::gpu::unavailable_reason() {
                        Some(why) => crate::i18n::tr_format!("none (CPU rendering): {why}", why = why),
                        None => "none (CPU rendering)".into(),
                    }),
                ),
                ("GPU rendering".to_string(), if app.ui.settings.gpu { "on".into() } else { "off".into() }),
                ("Memory budget".to_string(), mb(dac_engine::memory::default_budget() as u64)),
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
            if let Some(f) = dac_engine::gpu::last_fallback() {
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
        "app.keymapSet" => crate::shortcuts::choose_set(app, p),
        "app.keymapExport" => {
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => x.to_string(),
                None => {
                    let name = format!("{} Keymap.json", dac_brand::DISPLAY_NAME);
                    let req = PickRequest::save(crate::i18n::tr("Export Keymap"), crate::i18n::tr("Keymap"), &["json"], name);
                    match crate::pick::ask(app, id, p, "path", req, |_| None) {
                        Picked::Now(v) => match v.into_iter().next() {
                            Some(x) => x,
                            None => return Some(Ok(Value::Null)),
                        },
                        Picked::Later => return Some(Ok(Value::Null)),
                        Picked::Unavailable => return Some(Err("no file dialog on this platform: pass {path}".into())),
                    }
                }
            };
            crate::shortcuts::export_file(app, &path)
        }
        "app.keymapImport" => {
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => x.to_string(),
                None => {
                    let req = PickRequest::file(crate::i18n::tr("Import Keymap"), crate::i18n::tr("Keymap"), &["json"]);
                    match crate::pick::ask(app, id, p, "path", req, |_| None) {
                        Picked::Now(v) => match v.into_iter().next() {
                            Some(x) => x,
                            None => return Some(Ok(Value::Null)),
                        },
                        Picked::Later => return Some(Ok(Value::Null)),
                        Picked::Unavailable => return Some(Err("no file dialog on this platform: pass {path}".into())),
                    }
                }
            };
            crate::shortcuts::import_file(app, &path)
        }
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
                    let req = PickRequest::files(None, crate::i18n::tr("Photos"), dac_engine::import::EXTENSIONS);
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
            crate::import::open(app, paths)
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
            let folder = match p.get("folder").and_then(Value::as_str) {
                Some(f) => Some(f.to_string()),
                None => match crate::pick::ask(app, id, p, "folder", PickRequest::folder(crate::i18n::tr("Find Missing Photos…")), |s| {
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
            let candidates = dac_engine::cmd::missing::find_candidates(&app.session.catalog);
            let work = move || dac_engine::cmd::missing::plan_find_missing(&candidates, &folder);
            let done = |app: &mut DacApp, ctx: &egui::Context, plan: Result<dac_engine::cmd::missing::FindPlan, String>| {
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
                        app.ui.last_find_missing = Some(v);
                    }
                    Err(e) => app.toast(ctx, e),
                }
            };
            app.ui.last_find_missing = None;
            if let Err(e) = crate::tasks::spawn(app, LABEL, Some("findMissing"), work, done) {
                return Some(Err(e));
            }
            if p.get("wait").and_then(Value::as_bool).unwrap_or(false) {
                let ctx = app.tasks.repaint.clone().unwrap_or_default();
                crate::tasks::wait(app, &ctx, std::time::Duration::from_secs(600));
                return Some(Ok(app.ui.last_find_missing.clone().unwrap_or(Value::Null)));
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
                    let req = PickRequest::file(crate::i18n::tr("Locate Missing File…"), crate::i18n::tr("Photos"), dac_engine::import::EXTENSIONS);
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
                None => match dac_engine::devices::devices_now().into_iter().next() {
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
            r
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
                Some(path) => crate::import::open(app, vec![path]),
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
            let profiles = if lut_paths.is_empty() {
                0
            } else {
                app.session
                    .execute("profile.import", &json!({"paths": lut_paths}))
                    .ok()
                    .and_then(|v| v["imported"].as_array().map(Vec::len))
                    .unwrap_or(0)
            };
            let preset_paths: Vec<&String> = paths.iter().filter(|p| !p.to_ascii_lowercase().ends_with(".cube")).collect();
            let r = if preset_paths.is_empty() {
                Ok(json!({"imported": [], "failed": [], "skipped": 0}))
            } else {
                app.session.execute("preset.import", &json!({"paths": preset_paths})).map_err(|e| e.to_string())
            };
            if profiles > 0 {
                app.toast(
                    &ctx,
                    crate::i18n::tr_format!(
                        "Imported {profiles} profile{} (Profile browser ▸ their groups)",
                        if profiles == 1 { "" } else { "s" },
                        profiles = profiles
                    ),
                );
            }
            if let Ok(v) = &r {
                let n = v["imported"].as_array().map_or(0, Vec::len);
                let failed = v["failed"].as_array().map_or(0, Vec::len);
                let mut msg = match (n, failed) {
                    (0, 0) => "No new presets".to_string(),
                    (n, 0) => crate::i18n::tr_format!("Imported {n} preset{}", if n == 1 { "" } else { "s" }, n = n),
                    (n, f) => crate::i18n::tr_format!(
                        "Imported {n} preset{}, {f} file{} not readable",
                        if n == 1 { "" } else { "s" },
                        if f == 1 { "" } else { "s" },
                        f = f,
                        n = n
                    ),
                };
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
                app.toast(&ctx, msg);
                if n > 0 {
                    app.ui.presets = true;
                }
            }
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
        "file.exportPresets" => {
            let group = p.get("group").and_then(Value::as_str).map(str::to_string);
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => Some(x.to_string()),
                None => {
                    let default_group = format!("{} Presets", dac_brand::DISPLAY_NAME);
                    let name = format!("{}.{}", group.as_deref().unwrap_or(&default_group), dac_brand::PRESET_EXT);
                    let req =
                        PickRequest::save(crate::i18n::tr("Export Presets"), crate::i18n::tr("{app} Preset"), &[dac_brand::PRESET_EXT], name.clone());
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
        "app.website" | "app.github" | "app.help" | "app.feedback" => {
            let url = crate::links::url_of(id).unwrap_or(crate::links::APP_PAGE);
            crate::links::open(app, url)
        }
        "app.exportPrevious" => match app.session.last_export.clone() {
            Some(prev) => crate::control::export_active(app, &dac_engine::export::ExportOptions::known_keys_only(&prev)),
            None => Err("nothing exported yet — use Export…".into()),
        },
        _ => return None,
    };
    Some(r)
}

pub fn ui_enabled(app: &DacApp, id: &str) -> bool {
    if let Some(e) = crate::module::enabled(app, id) {
        return e;
    }
    if let Some(e) = crate::catalog_ui::enabled(app, id) {
        return e;
    }
    match id {
        "dialog.contactSheet" | "app.contactSheet" => !cfg!(target_arch = "wasm32") && app.session.active().is_some() && app.export.is_none(),
        s if s.starts_with("panel.") || s.starts_with("tool.") || s.starts_with("section.") => app.session.active().is_some() || s == "panel.close",
        "app.export" | "dialog.export" | "dialog.createPreset" | "dialog.rename" | "dialog.captureTime" | "dialog.copySettings" => {
            app.session.active().is_some()
        }
        "photo.tagFromTracklog" => app.session.active().is_some() && app.services.pick_tracklog.is_some(),
        "app.exportPrevious" => app.session.active().is_some() && app.session.last_export.is_some(),
        "dialog.pasteSettings" => app.session.active().is_some() && app.session.clipboard.is_some(),
        "app.showInFinder" => {
            app.services.reveal.is_some()
                && app
                    .session
                    .active()
                    .and_then(|id| app.session.catalog.photo(id))
                    .is_some_and(|p| matches!(p.source, dac_engine::catalog::Source::File { .. }))
        }
        "file.exportPresets" => app.session.presets.iter().any(|p| !p.builtin),
        "file.exportCurvePresets" => !app.session.curve_presets.is_empty(),
        "view.compare" => app.session.catalog.len() > 1,
        "view.fullScreenPreview" | "view.infoOverlay" | "view.navigator" => app.session.active().is_some() || app.ui.fullscreen,
        "app.openLibrary" | "file.addFolder" => app.services.pick_folder.is_some() && !crate::lightroom_import::is_running(app),
        "file.backupLibrary" => app.services.backup_library.is_some(),
        "app.openLogFolder" => app.services.reveal.is_some() && app.services.log_file.is_some(),
        "file.restoreLibrary" => app.services.restore_library.is_some(),
        "compare.swap" | "compare.makeSelect" => app.ui.view == ViewMode::Compare,
        s if s.starts_with("dialog.merge") || (s.starts_with("merge.") && s.ends_with("Last")) => {
            app.session.targets(&serde_json::json!({})).len() >= 2 && app.merge.final_task.is_none()
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
pub fn menu_entries(app: &DacApp) -> Vec<MenuEntry> {
    let mut v: Vec<MenuEntry> = ui_commands()
        .filter(|c| !c.3.is_empty())
        .map(|(id, label, _, m)| MenuEntry {
            id: id.to_string(),
            label: label.to_string(),
            menu: m.split('>').map(str::to_string).collect(),
            shortcut: crate::shortcuts::shortcut_of(&app.ui.settings.keymap, id).map(str::to_string),
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

/// With Settings → General → "Confirm before deleting" on, open the confirmation dialog instead
/// of deleting; true when it did (the dialog's OK runs `photo.delete`).
pub fn confirm_delete(app: &mut DacApp) -> bool {
    if !app.ui.settings.confirm_delete {
        return false;
    }
    let count = app.session.targets(&json!({})).len();
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
fn open_log_folder(app: &mut DacApp) -> Result<Value, String> {
    let path = app.services.log_file.clone().ok_or("this session keeps no log file")?;
    let reveal = app.services.reveal.as_mut().ok_or("not available here")?;
    reveal(&path)?;
    Ok(json!({"path": path}))
}

/// Reveal the active photo's original in the system file manager.
fn show_in_finder(app: &mut DacApp) -> Result<Value, String> {
    let id = app.session.active().ok_or("no photo selected")?;
    let path = match app.session.catalog.photo(id).map(|p| p.source.clone()) {
        Some(dac_engine::catalog::Source::File { path }) => path,
        _ => return Err("this photo has no file (demo scene)".into()),
    };
    let reveal = app.services.reveal.as_mut().ok_or("not available here")?;
    reveal(&path)?;
    Ok(json!({"path": path}))
}
