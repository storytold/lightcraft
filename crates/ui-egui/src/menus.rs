//! UI-level commands (views, panels, zoom, tools, dialogs) and the menu model shared by the native
//! menu bar, the shortcut handler and the control channel.

use serde::Serialize;
use serde_json::{Value, json};

use crate::LccApp;
use crate::pick::{PickRequest, Picked};
use crate::state::{BeforeAfter, Dialog, RightPanel, ViewMode, Zoom};

/// (id, label, shortcut, menu path)
pub type UiCommand = (&'static str, &'static str, Option<&'static str>, &'static str);

/// The fork's UI commands and their dispatch (fork-owned).
#[path = "menus_fork.rs"]
mod fork;

/// The "Edit → Language" entries, one per language in [`crate::i18n::Locale::ALL`], so a language
/// has a matching menu command (also listed through the control channel). Tests check coverage.
pub const LANGUAGE_COMMANDS: &[UiCommand] = &[
    ("app.language.english", crate::i18n::Locale::En.name(), None, "Edit>Language"),
    // The picker order (`Locale::for_picker`): alphabetical by the language's own name after
    // English; Cyrillic, Georgian, then CJK names follow the Latin ones. A test keeps them equal.
    ("app.language.indonesian", crate::i18n::Locale::Id.name(), None, "Edit>Language"),
    ("app.language.czech", crate::i18n::Locale::Cs.name(), None, "Edit>Language"),
    ("app.language.german", crate::i18n::Locale::De.name(), None, "Edit>Language"),
    ("app.language.spanish", crate::i18n::Locale::Es.name(), None, "Edit>Language"),
    ("app.language.french", crate::i18n::Locale::Fr.name(), None, "Edit>Language"),
    ("app.language.italian", crate::i18n::Locale::It.name(), None, "Edit>Language"),
    ("app.language.polish", crate::i18n::Locale::Pl.name(), None, "Edit>Language"),
    ("app.language.portuguese", crate::i18n::Locale::PtBr.name(), None, "Edit>Language"),
    ("app.language.finnish", crate::i18n::Locale::Fi.name(), None, "Edit>Language"),
    ("app.language.swedish", crate::i18n::Locale::Sv.name(), None, "Edit>Language"),
    ("app.language.vietnamese", crate::i18n::Locale::Vi.name(), None, "Edit>Language"),
    ("app.language.russian", crate::i18n::Locale::Ru.name(), None, "Edit>Language"),
    ("app.language.ukrainian", crate::i18n::Locale::Uk.name(), None, "Edit>Language"),
    ("app.language.georgian", crate::i18n::Locale::Ka.name(), None, "Edit>Language"),
    ("app.language.japanese", crate::i18n::Locale::Ja.name(), None, "Edit>Language"),
    ("app.language.simplifiedChinese", crate::i18n::Locale::ZhHans.name(), None, "Edit>Language"),
    ("app.language.traditionalChinese", crate::i18n::Locale::ZhHant.name(), None, "Edit>Language"),
];

/// Every UI command: the languages, then everything else. `xtask parity` reads both tables from
/// this file, so an id listed in `docs/parity-checklist.md` is checked wherever it is declared.
pub fn ui_commands() -> impl Iterator<Item = &'static UiCommand> {
    LANGUAGE_COMMANDS.iter().chain(UI_COMMANDS).chain(fork::commands())
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
        "app.language.swedish" => Some(crate::i18n::Locale::Sv),
        "app.language.finnish" => Some(crate::i18n::Locale::Fi),
        "app.language.indonesian" => Some(crate::i18n::Locale::Id),
        "app.language.vietnamese" => Some(crate::i18n::Locale::Vi),
        _ => None,
    }
}

pub const UI_COMMANDS: &[UiCommand] = &[
    ("ui.dock", "Arrange Panel", None, ""),
    ("ui.dock.reset", "Reset Panel Layout", None, "Window>Panels"),
    // {title, hint?, value?, command, params?, key}: asks for one line of text, then runs `command`
    ("ui.prompt", "Ask for a Name", None, ""),
    // re-reads midi.toml, so a controller map can be edited without restarting
    ("midi.reload", "Reload MIDI Map", None, ""),
    ("view.photoGrid", "Photo Grid", None, "View"),
    ("view.squareGrid", "Square Grid", None, "View>Grid View Style"),
    // G: Photo Grid ↔ Square Grid (from other views: the photo grid)
    ("view.gridToggle", "Grid", Some("G"), ""),
    // View ▸ Toggle Loupe View: the large view of the active photo <-> the grid style chosen last
    ("view.loupeToggle", "Toggle Loupe View", None, "View"),
    // Library ▸ New Folder… / Synchronize Folder…: act on the folder chosen in the Folders panel
    ("dialog.newDiskFolder", "New Folder…", None, "Library"),
    ("dialog.synchronizeFolder", "Synchronize Folder…", None, "Library"),
    // File ▸ Page Setup…: the Print module with its paper choice open
    ("dialog.pageSetup", "Page Setup…", None, "File"),
    ("library.selectOnlyActive", "Select Only Active Photo", Some("Cmd+Shift+D"), "Edit"),
    ("tool.guidedUpright", "Guided Upright", Some("Shift+G"), "Window>Tools"),
    ("view.detail", "Detail View", Some("D"), "View"),
    ("view.compare", "Compare", Some("Shift+C"), "View"),
    ("view.survey", "Survey", Some("N"), "View"),
    ("view.eyeFocus", "Eye Focus Strip", None, "View>Loupe Info"),
    ("view.people", "People", None, "View"),
    ("view.person", "Show Person", None, ""),
    ("view.faceBoxes", "Face Boxes", None, "View>Loupe Info"),
    ("view.focusPoint", "Show Focus Point", Some("Alt+F"), "View>Loupe Info"),
    ("view.zoomToFocus", "Zoom to Focus Point", None, "View>Loupe Info"),
    ("view.spaceZoomsTo", "Space Zooms To", None, ""),
    ("view.reference", "Reference View", Some("Shift+R"), "View>Before/After"),
    ("photo.setReference", "Set as Reference Photo", None, ""),
    ("compare.swap", "Swap Compare Photos", None, "View"),
    ("compare.set", "Show in Compare", None, ""),
    ("compare.makeSelect", "Make Candidate the Select", None, "View"),
    ("view.autoAdvance", "Auto Advance", None, "Photo"),
    // F6 (Classic's key, `shortcuts::ALIASES`); `/` is Deselect Active Photo
    ("view.filmstrip", "Filmstrip", None, "Window>Panels"),
    ("view.leftPanel", "My Photos Panel", Some("Cmd+Shift+L"), "Window>Panels"),
    ("view.rightPanel", "Right Panel", None, "Window>Panels"),
    ("view.beforeAfter", "Compare Before and After", Some("Y"), "View>Before/After"),
    ("view.beforeAfterSplit", "Before/After Split", Some("Shift+Y"), "View>Before/After"),
    ("view.beforeAfterTopBottom", "Before/After Top/Bottom", Some("Alt+Y"), "View>Before/After"),
    ("view.beforeAfterSplitTopBottom", "Before/After Split Top/Bottom", Some("Alt+Shift+Y"), "View>Before/After"),
    ("view.showOriginal", "Show Before", Some("\\"), "View>Before/After"),
    ("view.zoomFit", "Zoom to Fit", Some("Cmd+0"), "View>Zoom"),
    ("view.zoom100", "Zoom 100%", Some("Cmd+Alt+0"), "View>Zoom"),
    ("view.zoomToggle", "Toggle Zoom", Some("Z"), "View>Zoom"),
    // the ratio a click (and Z / Space) zooms to
    ("view.clickZoom", "Click Zoom Ratio", None, ""),
    ("view.navigate", "Set Image Zoom and Pan", None, ""),
    ("view.zoomLevel", "Zoom Level", None, ""),
    ("view.zoomIn", "Zoom In", Some("Cmd+="), "View>Zoom"),
    ("view.zoomOut", "Zoom Out", Some("Cmd+-"), "View>Zoom"),
    // Library grids only (Photo Grid, Square Grid, People): the bottom-bar slider's step by key
    ("view.thumbSizeUp", "Increase Grid Thumbnail Size", Some("="), "View>Grid View Style"),
    ("view.thumbSizeDown", "Decrease Grid Thumbnail Size", Some("-"), "View>Grid View Style"),
    ("view.clipping", "Show Clipping", Some("J"), "View"),
    ("view.visualizeHdr", "Visualize HDR", None, "View>Loupe Info"),
    // {on?}: HDR edits shown in HDR on an HDR display (off: their SDR rendition everywhere)
    ("view.hdrPreview", "HDR Preview", None, "View>Loupe Info"),
    ("view.colorReadout", "Color Readout", None, ""),
    ("view.colorReadoutRgb", "Show RGB Color Values", None, "View>Loupe Info"),
    ("view.colorReadoutHsl", "Show HSL Color Values", None, "View>Loupe Info"),
    ("view.colorReadoutLab", "Show Lab Color Values", None, "View>Loupe Info"),
    // in grids S expands/collapses stacks (the engine command it shadows)
    ("view.softProof", "Soft Proofing", Some("S"), "View"),
    ("view.histogram", "Histogram", Some("Cmd+Shift+H"), "Window>Panels"),
    // histograms & scopes (issue #206): which distribution the panel draws, and how tall its bins
    // are. Both take the value to select, or step to the next one with no parameter, so the menu,
    // a shortcut, the control channel and MCP all reach them by id.
    ("view.histogram.mode", "Histogram Mode", None, "Window>Panels"),
    ("view.histogram.scale", "Histogram Scale", None, "Window>Panels"),
    ("view.histogram.second", "Second Histogram", None, "Window>Panels"),
    ("view.maskOverlay", "Show Mask Overlay", Some("O"), "View>Tool Overlay"),
    // Ctrl+Shift+O / Cmd+Shift+O: every mask's region at once, not just the selected one
    ("view.maskOverlayAll", "Show All Mask Overlays", Some("Cmd+Shift+O"), "View>Tool Overlay"),
    // Shift+O in the Masking panel (elsewhere it cycles the crop overlay)
    ("view.maskOverlayMode", "Cycle Mask Overlay Mode", None, "View>Tool Overlay"),
    ("view.maskOverlayColor", "Cycle Mask Overlay Color", None, "View>Tool Overlay"),
    ("view.maskPins", "Show Mask Pins", None, "View>Tool Overlay"),
    ("view.maskOverlayAuto", "Automatically Toggle Mask Overlay", None, "View>Tool Overlay"),
    ("view.maskUnselectedPins", "Show Unselected Mask Pins", Some("Alt+V"), "View>Tool Overlay"),
    // {mode?: auto | always | selected | never}: when spot and mask pins show (steps to the next
    // mode when omitted; Shift+H, like Classic's H for pins, since H opens Remove). View ▸ Tool
    // Overlay and the Remove panel list the modes.
    ("view.toolOverlay", "Cycle Tool Overlay", Some("Shift+H"), ""),
    ("view.visualizeSpots", "Visualize Spots", Some("A"), "View>Tool Overlay"),
    // A while the Crop panel is open: the aspect lock as a first-class row. Visualize Spots keeps
    // the key outside the panel; the two are a contextual pair in shortcuts::handle
    ("crop.lockAspect", "Lock Aspect Ratio", Some("A"), ""),
    ("view.cropOverlay", "Cycle Crop Overlay", Some("Shift+O"), "View>Tool Overlay"),
    ("view.cropOverlayOrientation", "Cycle Crop Overlay Orientation", None, "View>Tool Overlay"),
    ("dialog.cropOverlays", "Choose Overlays to Cycle…", None, ""),
    ("dialog.bookPreferences", "Book Preferences…", None, ""),
    // {overlays: [guide names]}: which crop guides Cycle Crop Overlay visits (at least one)
    ("view.cropOverlays", "Set Crop Overlays to Cycle", None, ""),
    ("view.cropMaskOpacity", "Crop Mask Opacity", None, ""),
    ("photo.rate0", "Clear Rating", Some("0"), ""),
    ("photo.rate1", "Rate ★", Some("1"), ""),
    ("photo.rate2", "Rate ★★", Some("2"), ""),
    ("photo.rate3", "Rate ★★★", Some("3"), ""),
    ("photo.rate4", "Rate ★★★★", Some("4"), ""),
    ("photo.rate5", "Rate ★★★★★", Some("5"), ""),
    ("photo.labelRed", "Red Label", Some("6"), ""),
    ("photo.labelYellow", "Yellow Label", Some("7"), ""),
    ("photo.labelGreen", "Green Label", Some("8"), ""),
    ("photo.labelBlue", "Blue Label", Some("9"), ""),
    ("view.back", "Back to Grid", Some("Escape"), ""),
    ("tool.done", "Done", Some("Enter"), ""),
    ("view.filterBar", "Filter Bar", Some("Shift+F"), "Library>Filters"),
    ("local.addRoot", "Add Folder to Favorites", None, ""),
    ("local.hide", "Remove from Favorites", None, ""),
    ("local.restoreHidden", "Show Hidden Favorites", None, ""),
    ("view.fullScreenPreview", "Full Screen Preview", Some("F"), "Window>Screen Mode"),
    // Cmd+Shift+F is the Advanced Search (issue #846); full screen is F11, or Cmd+Ctrl+F as the
    // macOS system key (the aliases below)
    ("view.enterFullScreen", "Enter Full Screen", None, "Window>Screen Mode"),
    ("view.infoOverlay", "Cycle Info Overlay", Some("Cmd+I"), "View>Loupe Info"),
    ("view.shotInfo", "Shot Info", Some("Shift+I"), "View>Loupe Info"),
    ("view.lockZoomPosition", "Lock Zoom Position", Some("Cmd+Shift+="), "View>Zoom"),
    ("view.scrollByRow", "Scroll By Row", None, "View>Grid View Style"),
    ("view.mirrorImage", "Enable Mirror Image Mode", None, "View"),
    ("view.viewOptions", "View Options…", None, "View>Loupe Info"),
    ("view.chooseLayoutImage", "Choose Layout Image…", None, "View"),
    ("view.layoutImage", "Layout Image", Some("Cmd+Alt+O"), "View"),
    ("view.layoutImageOpacity", "Layout Image Opacity", None, ""),
    ("view.layoutImageMatte", "Layout Image Matte", None, ""),
    ("view.navigator", "Navigator", None, "Window>Panels"),
    ("panel.edit", "Edit", Some("E"), "Window>Edit Panels"),
    ("panel.profiles", "Profile Browser", None, "Window>Edit Panels"),
    ("panel.crop", "Crop & Rotate", Some("C"), "Window>Edit Panels"),
    ("panel.remove", "Remove", Some("H"), "Window>Edit Panels"),
    ("panel.masking", "Masking", Some("M"), "Window>Edit Panels"),
    ("panel.redeye", "Red Eye", None, "Window>Edit Panels"),
    // no default key: Shift+P is Lightroom Classic's pick-and-advance (a `shortcuts::ALIASES` entry)
    ("panel.presets", "Presets", None, ""),
    ("panel.info", "Info", Some("I"), ""),
    ("panel.keywords", "Keywords", Some("K"), ""),
    ("panel.versions", "Versions", Some("Shift+V"), ""),
    // Edit Keyword Tag for the keyword picked in the Keyword List (upstream #2879)
    ("panel.editKeywords", "Edit Keywords", Some("Cmd+Shift+K"), ""),
    ("panel.activity", "History", None, ""),
    ("panel.close", "Close Panel", None, ""),
    // the Color Mixer's layout {tab?: mixer|point, adjust?: hsl|color, group?: hue|sat|lum|all,
    // band?: red…magenta}: opens it in the Edit panel (the sliders are the mixer.* controls)
    ("view.colorMixer", "Color Mixer Layout", None, ""),
    ("panel.collapseRight", "Show/Hide Right Panel", None, "Window>Panels"),
    // {solo?}: true opens just that section (⌥-click on its header); otherwise opens or closes it
    // Lightroom Classic's ⌘1–⌘9, top to bottom (the fork's Tone Curve, Color Mixer, Color Grading and Transform are
    // in `menus_fork`; Color, split into those, stays a command for older scripts)
    ("section.light", "Basic", Some("Cmd+1"), "Window>Edit Sections"),
    ("section.color", "Color", None, ""),
    ("section.effects", "Effects", Some("Cmd+8"), "Window>Edit Sections"),
    ("section.detail", "Detail", Some("Cmd+5"), "Window>Edit Sections"),
    ("section.optics", "Lens Corrections", Some("Cmd+6"), "Window>Edit Sections"),
    ("section.calibration", "Calibration", Some("Cmd+9"), "Window>Edit Sections"),
    ("section.retouch", "Retouch", None, "Window>Edit Sections"),
    // {on?}; no params toggles. Opening one Edit section closes the others.
    ("view.soloSections", "Solo Mode", None, "Window>Edit Sections"),
    ("tool.brush", "Brush", Some("B"), "Window>Tools"),
    ("tool.linear", "Linear Gradient", Some("L"), "Window>Tools"),
    ("tool.radial", "Radial Gradient", Some("R"), "Window>Tools"),
    ("tool.wbPicker", "White Balance Selector", Some("W"), "Window>Tools"),
    ("tool.object", "Object Selection", None, "Window>Tools"),
    // Tools > Fringe Color Selector: arms the eyedropper; a click points Defringe at the colour under it
    ("tool.fringe", "Fringe Color Selector", None, ""),
    // Tools ▸ Create New Mask in Develop: what the Masking panel's tiles do (Classic keys: Shift+J; Shift+Q is Luminance Range;
    // `spot.cycleMode` is Alt+Shift+Q). People opens its picker, Color Range waits for a click on the photo.
    ("tool.people", "Select People", None, ""),
    ("tool.colorRange", "Color Range", None, ""),
    ("tool.luminanceRange", "Luminance Range", None, ""),
    // a Depth Range mask (needs the Depth Anything model: without it Settings ▸ AI Models opens)
    ("tool.depthRange", "Depth Range", None, ""),
    // Color Mixer ▸ Point Color with its picker armed
    ("tool.pointColor", "Point Color Selector", None, ""),
    // Remove Options ▸ Use Generative AI: the Remove tool's Generative mode on / off ({on?}; no params toggles); with no
    // service set up it opens Settings ▸ Generative Fill instead
    ("remove.useGenerative", "Use Generative AI", None, ""),
    // Remove Options ▸ Detect Objects: a Remove stroke picks the object under it ({on?}; no params toggles)
    ("remove.detectObjects", "Detect Objects", None, ""),
    ("tool.none", "No Tool", None, ""),
    ("tool.tat", "Targeted Adjustment", None, ""),
    ("mask.startComponent", "Combine Mask with", None, ""),
    ("dialog.keywordSet", "Edit Keyword Set…", None, ""),
    // {id?, component}: the component edited on the photo (a Brush component arms the brush on it)
    ("mask.editComponent", "Edit Mask Component", None, ""),
    // brush size / feather of the active brush (Masking brush, Remove tool and its selected spot)
    ("brush.smaller", "Decrease Brush Size", Some("["), "Window>Tools"),
    // the brush's Erase mode; {on} sets it (a console key holds it like Alt), none toggles
    ("brush.erase", "Erase", None, ""),
    ("brush.larger", "Increase Brush Size", Some("]"), "Window>Tools"),
    ("brush.featherLess", "Decrease Brush Feather", Some("Shift+["), "Window>Tools"),
    ("brush.featherMore", "Increase Brush Feather", Some("Shift+]"), "Window>Tools"),
    // the Masking brush's A / B presets (each keeps its own size, feather, flow and density)
    ("brush.preset", "Switch Brush A / B", None, "Window>Tools"),
    ("dialog.newAlbum", "New Collection…", Some("Cmd+N"), "Library"),
    // the active raw photo of a ColorChecker (a second selected one: the second chart, other light)
    ("dialog.cameraProfile", "Create Camera Profile from Color Chart…", None, "Photo>Develop Settings"),
    // in the camera profile dialog: {id? | path?} the second chart photo; without either, the picker opens
    ("cameraProfile.second", "Add Second Color Chart…", None, ""),
    ("cameraProfile.removeSecond", "Remove Second Color Chart", None, ""),
    // {allCameras?, close?}: the picker of library photos for the second chart
    ("cameraProfile.pickSecond", "Choose Second Color Chart", None, ""),
    // {slot?: 0 | 1}: look for the chart again / move the corners onto it / turn patch A01 a corner further
    ("cameraProfile.find", "Find Color Chart Again", None, ""),
    ("cameraProfile.align", "Align Corners to Color Chart", None, ""),
    ("cameraProfile.turn", "Turn Color Chart", None, ""),
    // {mode: fit | actual | in | out, slot?}: the chart view's zoom
    ("cameraProfile.zoom", "Zoom Color Chart View", None, ""),
    // {choice: replace | keepBoth | cancel, name?}: answer "a profile of that name is there"
    ("cameraProfile.resolve", "Resolve Camera Profile Name", None, ""),
    ("dialog.newFolder", "New Collection Set…", Some("Cmd+Shift+N"), "Library"),
    ("dialog.smartAlbum", "New Smart Collection…", None, "Library"),
    ("view.photoCounts", "Show Photo Counts", None, "Window>Panels"),
    ("view.sidebarSection", "Show Sidebar Section", None, ""),
    ("view.sidebarMoveSection", "Move Sidebar Section", None, ""),
    ("view.sidebarReset", "Reset Sidebar Sections", None, ""),
    ("view.slideshow", "Slideshow", Some("Cmd+Alt+Enter"), "Window>Slideshow"),
    ("view.slideshowOptions", "Slideshow Options…", None, "Window>Slideshow"),
    ("dialog.slideshowExport", "Export Slideshow…", None, "Window>Slideshow"),
    ("app.slideshowExport", "Export Slideshow", None, ""),
    // the slideshow's playback controls (Space, ←/→ and Esc while it plays)
    ("slideshow.togglePause", "Pause / Resume Slideshow", None, ""),
    ("slideshow.next", "Next Slide", None, ""),
    ("slideshow.previous", "Previous Slide", None, ""),
    ("slideshow.end", "End Slideshow", None, ""),
    ("view.secondWindow", "Second Window", Some("Cmd+F11"), "Window"),
    ("tool.keywordPainter", "Keyword Painter", None, ""),
    ("tool.painter", "Painter", Some("Cmd+Alt+K"), "Library"),
    ("view.gridCellStyle", "Grid Cell Style", None, ""),
    ("dialog.viewOptions", "View Options…", Some("Cmd+J"), "View>Grid View Style"),
    ("view.cellCompact", "Compact Cells", None, "View>Grid View Style"),
    ("view.cellExpanded", "Expanded Cells", None, "View>Grid View Style"),
    ("view.cellIndex", "Show Index Numbers", None, "View>Grid View Style"),
    ("view.cellBadges", "Show Thumbnail Badges", None, "View>Grid View Style"),
    // Library's `=` / `-`, Home / End (module keys, see `crate::module::LIBRARY_KEYS`)
    ("view.thumbLarger", "Increase Thumbnail Size", None, "View>Grid View Style"),
    ("view.thumbSmaller", "Decrease Thumbnail Size", None, "View>Grid View Style"),
    ("library.first", "First Photo", None, ""),
    ("library.last", "Last Photo", None, ""),
    // Develop's ⇧Q: the selected spot's mode Remove → Heal → Clone
    ("spot.cycleMode", "Cycle Spot Mode", None, ""),
    ("metadata.panelPreset", "Metadata Panel Preset", None, ""),
    ("view.gridInfo", "Grid Info", None, ""),
    // {mode?: auto | light | dark}: Appearance Mode (cycles Auto, Light, Dark when omitted; the
    // top bar's appearance button). View ▸ Appearance lists the modes.
    ("view.appearance", "Next Appearance Mode", None, ""),
    // {theme: charcoal | midnight | silver | paper}: pick a theme and fix the mode to its family.
    ("view.theme", "Theme", None, ""),
    ("dialog.allMetadata", "All Metadata…", None, "Metadata"),
    ("dialog.faceModel", "Add Face Model…", None, ""),
    ("dialog.newSmartAlbum", "New Smart Collection from Filter…", Some("Cmd+Alt+N"), "Library"),
    ("dialog.createPreset", "Create Preset…", Some("Cmd+Shift+P"), "Photo>Develop Settings"),
    ("dialog.presetsManager", "Manage Presets…", None, "Photo>Develop Settings"),
    // {query?}: the Profile Manager (looks, LUT profiles and camera profiles: hide, delete, rename)
    ("dialog.profileManager", "Manage Profiles…", None, "Photo>Develop Settings"),
    ("dialog.autoStack", "Auto-Stack by Capture Time…", None, "Photo>Stack"),
    ("dialog.copySettings", "Choose Edit Settings to Copy…", Some("Cmd+Shift+C"), "Photo>Develop Settings"),
    // {id}: Update Preset with Current Settings, pre-ticked with what the preset holds
    ("dialog.updatePreset", "Update Preset with Current Settings…", None, ""),
    ("dialog.pasteSettings", "Paste Selected Settings…", Some("Cmd+Shift+V"), "Photo>Develop Settings"),
    ("dialog.syncSettings", "Sync Settings…", Some("Cmd+Shift+S"), "Photo>Develop Settings"),
    ("dialog.syncCopies", "Sync Copies…", None, ""),
    ("dialog.syncSnapshots", "Sync Snapshots…", None, ""),
    ("view.focusSearch", "Find…", Some("Cmd+F"), "Library"),
    ("dialog.advancedSearch", "Advanced Search…", Some("Cmd+Shift+F"), "Library"),
    ("dialog.import", "Import…", Some("Cmd+Shift+I"), "File"),
    ("file.export", "Export…", None, "File"),
    ("dialog.export", "Export Dialog…", None, ""),
    ("dialog.print", "Print…", Some("Cmd+P"), "File"),
    ("app.print", "Print to PDF", None, ""),
    ("dialog.contactSheet", "Contact Sheet PDF…", None, "File>More Exports"),
    ("app.contactSheet", "Export Contact Sheet PDF", None, ""),
    ("photo.editInPhotoCraft", "Edit in PhotoCraft", Some("Cmd+Shift+E"), "Photo>Edit In"),
    ("photo.editInExternal", "Edit in Other Editor", Some("Cmd+Alt+E"), "Photo>Edit In"),
    ("dialog.editInPresets", "Edit In Presets…", None, "Photo>Edit In"),
    ("panel.actions", "Actions", None, ""),
    // {ids?, app?}: the original files themselves (a raw stays a raw: no render, no copy), opened
    // in the other editor, e.g. a raw developer
    ("photo.openOriginalInExternal", "Open Original in Other Editor (RAW, no copy)", None, "Photo>Edit In"),
    ("dialog.enhance", "Enhance…", Some("Cmd+Alt+I"), "Photo"),
    ("dialog.mergeHdr", "HDR…", Some("Ctrl+H"), "Photo>Photo Merge"),
    ("dialog.mergePanorama", "Panorama…", Some("Ctrl+M"), "Photo>Photo Merge"),
    ("dialog.mergeHdrPanorama", "HDR Panorama…", None, "Photo>Photo Merge"),
    ("dialog.mergeFocusStack", "Focus Stack…", None, "Photo>Photo Merge"),
    ("merge.hdrLast", "HDR with Last Settings", Some("Ctrl+Shift+H"), "Photo>Photo Merge"),
    ("merge.panoramaLast", "Panorama with Last Settings", Some("Ctrl+Shift+M"), "Photo>Photo Merge"),
    ("merge.hdrPanoramaLast", "HDR Panorama with Last Settings", None, "Photo>Photo Merge"),
    ("merge.focusStackLast", "Focus Stack with Last Settings", None, "Photo>Photo Merge"),
    ("file.addPhotos", "Import Photos…", None, "File"),
    // the workspace tabs (Settings ▸ Import ▸ Import as a tab; OneSlips LR-IMP-WINDOW)
    ("view.importTab", "Browse Tab", None, "Window>Modules"),
    ("view.libraryTab", "Library Tab", None, "Window>Modules"),
    // {what?: picks|selected|all}: the Import tab's big button
    ("import.start", "Import from the Import Tab", None, ""),
    // the Browse tab's "Develop without Import": the Library tab's Develop module on the browsed
    // folder's active photo, the edits to the XMP sidecar, the catalog untouched
    ("import.develop", "Develop without Import", None, ""),
    // {take?, previews?, copy?, moveFiles?, destination?, organize?, rename?, preset?, presets?, keywords?, metadataPreset?, album?, newAlbum?…}
    ("import.configure", "Import Tab Settings", None, ""),
    ("import.settings", "Show Import Tab Settings", None, ""),
    ("import.usePreset", "Use Import Preset", None, ""),
    ("import.saveSettings", "Save Import Preset", None, ""),
    ("file.addFolder", "Import from Folder…", None, "File"),
    // {path?, into?: open | new, name?, location?}: asks where the catalog goes unless `into` (or
    // Settings) says (#2672, `lightroom_destination`)
    ("file.importLightroom", "Import Lightroom or Capture One Catalog…", None, "File>More Imports"),
    ("file.importImmich", "Import from Immich…", None, "File>More Imports"),
    ("immich.connections", "Immich Settings…", None, "Library>Immich"),
    ("file.addFromDevice", "Import from Device", None, ""),
    // Android: the photos in DCIM/Camera, through the media store (Settings ▸ Folders)
    ("file.addCameraRoll", "Import Camera Roll…", None, ""),
    ("file.findMissing", "Find Missing Photos…", None, "Library"),
    ("library.validateDng", "Validate DNG Files", None, ""),
    ("file.backupLibrary", "Back Up Catalog…", None, "File>Libraries"),
    ("file.restoreLibrary", "Restore Catalog from Backup…", None, "File>Libraries"),
    ("photo.locate", "Locate Missing File…", None, ""),
    ("dialog.saveMetadataPreset", "Save Metadata Preset…", None, ""),
    // the keywords ⇧K adds or removes, typed with commas between them
    ("dialog.keywordShortcut", "Set Keyword Shortcut…", Some("Cmd+Alt+Shift+K"), ""),
    ("app.quit", "Quit {app}", Some("Cmd+Q"), "File"),
    ("file.importPresets", "Import Profiles & Presets…", None, "File>More Imports"),
    ("file.exportPresets", "Export Presets…", None, "File>More Exports"),
    // Masking panel ▸ Effect menu
    ("dialog.saveMaskEffect", "Save Current Settings as New Effect Preset…", None, ""),
    ("dialog.manageMaskEffects", "Manage Effect Presets…", None, ""),
    ("file.exportMaskEffects", "Export Effect Presets…", None, ""),
    // Metadata panel ▸ Preset menu, Export dialog ▸ preset column
    ("file.exportMetadataPresets", "Export Metadata Presets…", None, ""),
    ("file.exportExportPresets", "Export User Presets…", None, ""),
    ("file.importKeywords", "Import Keywords…", None, "File>More Imports"),
    ("file.exportKeywords", "Export Keywords…", None, "File>More Exports"),
    // the active photo's colour for other editors (`camera.exportDcp`, `develop.exportLut`)
    ("file.exportCameraProfile", "Export Camera Profile (.dcp)…", None, "File>More Exports"),
    ("file.exportLookLut", "Export Look as LUT (.cube)…", None, "File>More Exports"),
    // Edit panel ▸ Curve ▸ Point Curve dropdown
    ("file.importCurvePresets", "Import Point Curve Presets…", None, ""),
    ("file.exportCurvePresets", "Export Point Curve Presets…", None, ""),
    ("dialog.watermark", "Edit Watermarks…", None, "Edit"),
    ("app.settings", "Settings…", Some("Cmd+,"), "Edit"),
    ("app.autoImportToggle", "Enable Auto Import", None, "File>Auto Import"),
    ("app.newLibrary", "New Catalog…", None, "File>Libraries"),
    ("app.openLibrary", "Open Catalog…", Some("Cmd+O"), "File>Libraries"),
    ("app.renameLibrary", "Rename Catalog…", None, "File>Libraries"),
    // libraries (crate::catalog_ui; the ids keep their older catalog name)
    ("catalog.new", "New Catalog…", None, ""),
    ("catalog.open", "Open Catalog…", None, ""),
    ("catalog.openRecent", "Open Recent Catalog", None, ""),
    ("catalog.chooser", "Choose Catalog…", None, ""),
    ("catalog.welcome", "Welcome…", None, ""),
    ("catalog.promptAtStartup", "Ask Which Catalog to Open at Launch", None, ""),
    ("dialog.catalogSettings", "Catalog Settings…", None, "Edit"),
    ("dialog.exportCatalog", "Export as Catalog…", None, "File>Libraries"),
    ("dialog.importCatalog", "Import from Another Catalog…", None, "File>More Imports"),
    crate::legacy::IMPORT_LIBRARY,
    ("app.recentLibraries", "Recent Catalogs", None, ""),
    ("app.clearRecentLibraries", "Clear Recent Catalogs", None, ""),
    // Settings ▸ Display
    ("app.displayProfile", "Display Profile", None, ""),
    ("app.lensDbFolder", "Choose Lens Database Folder…", None, ""),
    ("app.sam3Folder", "Choose SAM 3 Model Folder…", None, ""),
    ("app.folders", "Folders", None, ""),
    ("app.exportFolder", "Choose Export Folder…", None, ""),
    ("app.about", "About {app}", None, "Help"),
    ("app.checkForUpdates", "Check for Updates…", None, "Help"),
    ("app.installUpdate", "Install Update and Relaunch", None, "Help"),
    ("app.openReleasePage", "Open the Release Page", None, ""),
    ("app.allFilesAccess", "Allow Access to All Files…", None, ""),
    ("app.systemInfo", "System Info…", None, "Help"),
    ("app.openLogFolder", "Open Log Folder", None, "Help"),
    // the frame-rate display (`frame_rate`): a debugging aid, never saved
    ("view.frameRate", "Frame Rate", None, "Help>Debug"),
    ("app.whatsNew", "What's New", None, "Help"),
    ("dialog.cull", "Assisted Culling…", None, "Library"),
    ("app.help", "{app} Help", Some("F1"), "Help"),
    ("app.feedback", "Send Feedback…", None, "Help"),
    ("app.website", "{app} Website", None, "Help"),
    ("app.github", "{app} Source Code", None, "Help"),
    ("app.discord", "Join the Discord", None, "Help"),
    ("app.shortcuts", "Keyboard Shortcuts", Some("Cmd+/"), "Help"),
    ("app.setShortcut", "Set Keyboard Shortcut", None, ""),
    ("app.resetShortcuts", "Reset All Keyboard Shortcuts", None, ""),
    ("app.keymapSet", "Keymap Set", None, ""),
    ("app.keymapExport", "Export Keymap…", None, "Edit>Keymap"),
    ("app.keymapImport", "Import Keymap…", None, "Edit>Keymap"),
    ("app.export", "Export Now", None, ""),
    ("app.showInFinder", "Show in Finder", Some("Cmd+R"), "Photo>File"),
    ("app.showInFolders", "Show in Folders", None, "Photo>File"),
    ("app.openSamModelFolder", "Show in File Manager", None, ""),
    ("dialog.aiModels", "AI Models…", None, ""),
    ("dialog.rename", "Rename Photos…", Some("F2"), "Library"),
    ("dialog.labelNames", "Edit Color Label Names…", None, ""),
    ("dialog.captureTime", "Edit Capture Time…", None, "Metadata"),
    ("metadata.targetOnly", "Show Metadata for Target Photo Only", None, "Metadata"),
    ("photo.tagFromTracklog", "Auto-Tag from Tracklog…", None, "Metadata"),
    ("app.exportPrevious", "Export with Previous", Some("Cmd+Alt+Shift+E"), "File"),
    // control surfaces (`surface`): what their keys run, and their settings for agents
    ("surface.maskPrevious", "Select Previous Mask", None, ""),
    ("surface.maskNext", "Select Next Mask", None, ""),
    ("surface.dialogConfirm", "Confirm Dialog", None, ""),
    ("surface.dialogCancel", "Cancel Dialog", None, ""),
    ("surface.pageNext", "Next Surface Page", None, ""),
    ("surface.pagePrevious", "Previous Surface Page", None, ""),
    ("surface.page", "Show Surface Page", None, ""),
    ("surface.profile", "Control Surface Profile", None, ""),
    ("surface.applyToSelected", "Dials Change All Selected Photos", None, ""),
    ("surface.followContext", "Surface Pages Follow the App", None, ""),
    ("surface.input", "Control Surface Input", None, ""),
    ("surface.state", "Control Surface State", None, ""),
    ("surface.settings", "Control Surface Settings", None, ""),
    ("surface.forget", "Forget a Paired Surface", None, ""),
    ("surface.import", "Import Control Surface Setup", None, ""),
    ("surface.export", "Export Control Surface Layout", None, ""),
    ("surface.plugin", "Surface Plugin Status", None, ""),
    ("surface.installPlugin", "Install the Surface Plugin", None, ""),
    ("surface.uninstallPlugin", "Remove the Surface Plugin", None, ""),
    // the on-screen overlay (`panels::surface_osd`): its settings, for keys and agents
    ("osd.settings", "Overlay Settings", None, ""),
    ("osd.mode", "Overlay: What It Shows", None, ""),
    ("osd.state", "Overlay State", None, ""),
];

fn panel(app: &mut LccApp, ctx: &egui::Context, p: RightPanel, name: &str) {
    app.ui.note_right();
    if app.ui.right == p && crate::docking::exposed(app, crate::docking::Panel::Tools) {
        // toggled off: back to exactly what the right column showed before this toggle opened it
        // (closed when it was closed); otherwise a tool closes back to the panel shown before it
        // (Edit's panels under a Develop drawer, as in Lightroom Classic), any other panel hides
        let before = app.ui.toggle_return.take().and_then(|(opened, before)| (opened == p && before != p).then_some(before));
        app.ui.right = match before {
            // a tool opened from another tool closes back to the panel under both
            Some(before) if p.is_tool() && before.is_tool() => app.ui.leave_tool(),
            Some(before) => {
                app.ui.tool_return = None;
                before
            }
            None if p.is_tool() => app.ui.leave_tool(),
            None => RightPanel::None,
        };
        app.ui.note_right();
        app.toast(ctx, crate::i18n::tr_format!("{name} Off", name = crate::i18n::tr(name)));
    } else {
        // a panel hidden behind another dock tab was not on show: nothing to come back to but it
        let before = if app.ui.right == p { RightPanel::None } else { app.ui.right };
        app.ui.toggle_return = Some((p, before));
        app.ui.right = p;
        app.ui.note_right();
        // opening a panel unfolds the side
        app.ui.right_collapsed = false;
        // opening a panel brings its edge back (F8 / Tab hid it)
        app.ui.right_edge = true;
        app.ui.hidden_panels.retain(|h| h.right_panel() != Some(p));
        app.toast(ctx, crate::i18n::tr_format!("{name} On", name = crate::i18n::tr(name)));
        if p.is_edit_tool() && !matches!(app.ui.view, ViewMode::Detail) {
            app.ui.view = ViewMode::Detail;
        }
    }
    if p != RightPanel::Masking && !matches!(app.ui.tool.as_str(), "wbPicker" | "fringe") {
        app.ui.tool.clear();
    }
    // an Edit panel picker doesn't follow into Masking, where it would claim the photo's clicks
    if app.ui.right == RightPanel::Masking && (app.ui.tool.starts_with("tat:") || matches!(app.ui.tool.as_str(), "pointColor" | "fringe")) {
        app.ui.tool.clear();
    }
    app.end_midi_interaction();
    let _ = app.session.end_interaction();
}

/// Use brush preset `slot` (0 = A, 1 = B): the settings in use are kept in the current slot first.
pub(crate) fn switch_brush_preset(app: &mut LccApp, slot: u8) -> Value {
    let u = &mut app.ui;
    let cur = usize::from(u.brush_slot.min(1));
    if let Some(p) = u.brush_presets.get_mut(cur) {
        *p = crate::state::BrushPreset { size: u.brush_size, feather: u.brush_feather, flow: u.brush_flow, density: u.brush_density };
    }
    u.brush_slot = slot.min(1);
    if let Some(p) = u.brush_presets.get(usize::from(u.brush_slot)).copied() {
        (u.brush_size, u.brush_feather, u.brush_flow, u.brush_density) = (p.size, p.feather, p.flow, p.density);
    }
    json!({"slot": if u.brush_slot == 0 { "a" } else { "b" }, "size": u.brush_size, "feather": u.brush_feather, "flow": u.brush_flow, "density": u.brush_density})
}

/// `[` / `]` (size ×`k`) and ⇧`[` / ⇧`]` (feather +`df`) for the brush in use: the Remove tool's
/// (and its selected spot's) or the Masking brush's.
pub(crate) fn adjust_brush(app: &mut LccApp, k: f32, df: f32) -> Value {
    if app.ui.right == RightPanel::Remove {
        if let Some(i) = app.session.active_spot
            && let Some(id) = app.session.active()
            && let Some(d) = app.session.develop_of(id)
            && let Some(sp) = d.spots.get(i)
        {
            let new_size = (sp.size as f32 * k).clamp(0.001, 0.25);
            let new_feather = (sp.feather as f32 + df).clamp(0.0, 100.0);
            let mut p = json!({"index": i});
            if k != 1.0 {
                p["size"] = json!(new_size);
            }
            if df != 0.0 {
                p["feather"] = json!(new_feather);
            }
            let _ = app.run("spot.update", p);
            return json!({"size": new_size, "feather": new_feather});
        }
        app.ui.remove_size = (app.ui.remove_size * k).clamp(0.001, 0.25);
        app.ui.remove_feather = (app.ui.remove_feather + df).clamp(0.0, 100.0);
        json!({"size": app.ui.remove_size, "feather": app.ui.remove_feather})
    } else {
        // the set in use: the eraser's while erasing; Size is a size on the screen (1..100)
        let erase = app.ui.brush_erasing();
        let s = app.ui.brush_set(erase);
        app.ui.set_brush_set(erase, crate::state::BrushPreset { size: s.size * k, feather: s.feather + df, ..s });
        let s = app.ui.brush_set(erase);
        json!({"size": s.size, "feather": s.feather, "erase": erase})
    }
}

/// The `parent` folder id of a `dialog.new*` command (none: the top level).
fn parent_param(p: &Value) -> Option<u64> {
    p.get("parent").and_then(Value::as_u64)
}

/// `file.addPhotos` / `file.addFolder` / `file.addFromDevice` `{album?}`: the review of the scan
/// just started opens with that album chosen (an album's context menu ▸ Import ▸ …). An error
/// means another scan is running. `file.addFolder {destination?}` opens it copying into that
/// folder (a folder's menu ▸ Import to This Folder…).
fn import_into_album(app: &mut LccApp, p: &Value, r: Result<Value, String>) -> Result<Value, String> {
    if r.is_ok()
        && let Some(t) = &mut app.scan
    {
        t.album = p.get("album").and_then(Value::as_u64);
        t.destination = p.get("destination").and_then(Value::as_str).filter(|d| !d.is_empty()).map(str::to_string);
    }
    r
}

/// An sRGB colour from `"#rrggbb"` or `[r, g, b]` (0..255).
pub fn parse_rgb(v: &Value) -> Option<[u8; 3]> {
    if let Some(s) = v.as_str() {
        let h = s.strip_prefix('#').unwrap_or(s);
        if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
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

fn appearance_json(app: &LccApp) -> Value {
    let s = &app.ui.settings;
    json!({"mode": s.appearance_mode, "darkTheme": s.dark_theme, "lightTheme": s.light_theme})
}

/// `view.appearance {mode?}`: set Appearance Mode, or step to the next one (Auto, Light, Dark).
/// The theme follows on the next frame ([`LccApp::sync_theme`]).
fn appearance_command(app: &mut LccApp, p: &Value) -> Result<Value, String> {
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
fn theme_command(app: &mut LccApp, p: &Value) -> Result<Value, String> {
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

/// `view.colorMixer`: the Color Mixer's tab, Adjust mode, HSL group and Color-mode band, each
/// optional; the Edit panel opens on it. Returns the layout now shown.
fn color_mixer_layout(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    use crate::panels::edit::{MIXER_ADJUST, MIXER_GROUPS};
    let choose = |key: &str, keys: &[&str]| -> Result<Option<String>, String> {
        match p.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(v)) if keys.contains(&v.as_str()) => Ok(Some(v.clone())),
            Some(other) => Err(format!("view.colorMixer: `{key}` must be one of {}, not {other}", keys.join(", "))),
        }
    };
    let adjust: Vec<&str> = MIXER_ADJUST.iter().map(|(_, k)| *k).collect();
    let groups: Vec<&str> = MIXER_GROUPS.iter().map(|(_, k)| *k).collect();
    let (tab, adjust, group, band) =
        (choose("tab", &["mixer", "point"])?, choose("adjust", &adjust)?, choose("group", &groups)?, choose("band", &lcc_develop::MIXER_BANDS)?);
    if let Some(tab) = tab
        && (tab == "point") != app.ui.flyout_open("pointColor")
    {
        app.ui.toggle_flyout("pointColor");
    }
    if let Some(v) = adjust {
        app.ui.mixer_adjust = v;
    }
    if let Some(v) = group {
        app.ui.mixer_group = v;
    }
    if let Some(v) = band {
        app.ui.mixer_mode = v;
    }
    crate::panels::edit::drop_hidden_mixer_tool(app);
    app.ui.right = RightPanel::Edit;
    app.ui.view = ViewMode::Detail;
    if !app.ui.section_open("mixer") {
        app.ui.toggle_section("mixer");
    }
    let tab = if app.ui.flyout_open("pointColor") { "point" } else { "mixer" };
    Ok(json!({"tab": tab, "adjust": app.ui.mixer_adjust, "group": app.ui.mixer_group, "band": app.ui.mixer_mode}))
}

/// `ui.prompt {title, hint?, value?, command, params?, key}`: the menu-callable way to ask for one
/// line of text. Opens the name prompt (`Dialog::TextPrompt`) titled `title` (greyed `hint` when
/// empty, `value` pre-filled); OK runs `command` with `{...params, <key>: <the text, trimmed>}`.
/// A menu slot uses it as `ui.prompt{"title":"Create Saved Print","hint":"Name","command":
/// "printui.saveCreation","key":"name"}=Create Saved Print…`; any command that wants a name from the
/// user and is not already a dialog can be reached this way. Errors on a missing `title`, `command`
/// or `key`, a `params` that is not an object, or `command` naming `ui.prompt` itself.
fn prompt_command(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    let text = |k: &str| p.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
    let title = text("title").ok_or("missing `title`")?;
    let command = text("command").ok_or("missing `command`")?;
    let key = text("key").ok_or("missing `key`")?;
    if command == "ui.prompt" {
        return Err("`command` can't be ui.prompt".into());
    }
    let params = match p.get("params") {
        None | Some(Value::Null) => json!({}),
        Some(v @ Value::Object(_)) => v.clone(),
        Some(_) => return Err("`params` must be an object".into()),
    };
    let hint = p.get("hint").and_then(Value::as_str).unwrap_or("");
    let value = p.get("value").and_then(Value::as_str).unwrap_or("");
    crate::panels::dialogs::prompt(app, title, hint, value, command, params, key);
    Ok(Value::Null)
}

/// Handle UI commands; `None` means "not a UI command — send it to the engine".
pub fn run_ui_command(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if matches!(id, "ui.dock" | "ui.dock.reset") {
        return Some(crate::docking::command(app, id == "ui.dock.reset", p));
    }
    if (id.starts_with("import.") || id.starts_with("view.") && id.ends_with("Tab"))
        && let Some(r) = crate::import_tab::run(app, id, p)
    {
        return Some(r);
    }
    if matches!(id, "library.inspectLightroom" | "library.importLightroom") {
        // the app's own context: a fresh one's repaints reach no window and its clock starts at zero
        let ctx = app.tasks.repaint.clone().unwrap_or_default();
        return Some(crate::lightroom_import::command(app, id, p, &ctx));
    }
    if id == "ui.prompt" {
        return Some(prompt_command(app, p));
    }
    if id == "photo.reload" {
        // the files are read in the background (a raw's decode takes a second or more)
        return crate::reload::command(app, p);
    }
    if id == "midi.reload" {
        let ctx = app.tasks.repaint.clone().unwrap_or_default();
        let result = match app.midi_reload.as_mut() {
            Some(reload) => reload(),
            None => Err("MIDI input is not available in this build".to_string()),
        };
        return Some(match result {
            Ok(said) => {
                app.toast(&ctx, said.clone());
                Ok(json!(said))
            }
            Err(e) => {
                app.toast_error(&ctx, e.clone());
                Err(e)
            }
        });
    }
    if id.starts_with("surface.")
        && let Some(r) = crate::surface::command(app, id, p)
    {
        return Some(r);
    }
    if id.starts_with("osd.") {
        return Some(crate::panels::surface_osd::command(app, id, p));
    }
    if let Some(language) = language_from_command(id) {
        app.ui.language = language;
        // Immediately, not on the next frame: the reply and anything else run this frame
        // (menus rebuilt from it, toasts) are already in the new language.
        crate::i18n::set_language(app.ui.language);
        return Some(Ok(json!(app.ui.language)));
    }
    // ←/→ in the full-screen slideshow (View ▸ Slideshow) step its playback; otherwise these ids are
    // the Slideshow module's (`slideshow_ui`), which shares them
    if app.ui.slideshow.is_some() && matches!(id, "slideshow.next" | "slideshow.previous") {
        let now = app.tasks.repaint.clone().unwrap_or_default().input(|i| i.time);
        return Some(crate::panels::slideshow::command(app, id, now));
    }
    if let Some(r) = fork::run(app, id, p) {
        return Some(r);
    }
    // the app's own context: a toast's time comes from its clock (a fresh context's clock is at 0,
    // and a toast set by it was long over by the app's clock: it never showed)
    let ctx = app.tasks.repaint.clone().unwrap_or_default();
    let r: Result<Value, String> = match id {
        "view.eyeFocus" => {
            let on = p.get("on").and_then(Value::as_bool).unwrap_or(!app.eye_focus_on());
            app.set_eye_focus(on);
            Ok(json!({"on": on, "where": if app.importing() { "import" } else { "library" }}))
        }
        "view.photoGrid" => {
            app.leave_fullscreen_preview();
            app.ui.view = ViewMode::PhotoGrid;
            app.ui.grid_style = ViewMode::PhotoGrid;
            Ok(Value::Null)
        }
        "view.squareGrid" => {
            app.leave_fullscreen_preview();
            app.ui.view = ViewMode::SquareGrid;
            app.ui.grid_style = ViewMode::SquareGrid;
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
                app.ui.grid_style = ViewMode::SquareGrid;
            }
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
        "view.loupeToggle" => {
            app.leave_fullscreen_preview();
            if app.ui.module == crate::module::ModuleId::Library && app.ui.view == ViewMode::Detail {
                app.ui.view = if app.ui.grid_style.is_grid() { app.ui.grid_style } else { ViewMode::PhotoGrid };
                Ok(Value::Null)
            } else {
                app.run("view.loupe", json!({}))
            }
        }
        "dialog.newDiskFolder" => {
            let path = match chosen_library_folder(app) {
                Ok(p) => p,
                Err(e) => return Some(Err(e)),
            };
            app.ui.dialog = Some(Dialog::TextPrompt {
                title: crate::i18n::tr_format!("New Folder in “{name}”", name = lcc_catalog::folders::folder_label(&path)),
                hint: "Folder name (created on disk)".into(),
                value: String::new(),
                command: "folder.create".into(),
                params: json!({"parent": path}),
                key: "name".into(),
            });
            Ok(Value::Null)
        }
        "dialog.synchronizeFolder" => {
            let path = match chosen_library_folder(app) {
                Ok(p) => p,
                Err(e) => return Some(Err(e)),
            };
            let tree = app.caches.folder_tree(&app.session.catalog);
            match crate::panels::left::library_node_for(&tree, &path) {
                // a disk, or a folder holding disks, is synchronized as such
                Some((n, name)) => crate::sync::open(app, &n.path, &name, n.volume || !n.selectable).map(|()| Value::Null),
                None => Err("No photo in this folder is in the catalog yet".into()),
            }
        }
        "dialog.pageSetup" => {
            if app.ui.module != crate::module::ModuleId::Print
                && let Err(e) = app.run("module.print", json!({}))
            {
                return Some(Err(e));
            }
            app.ui.right_collapsed = false;
            app.ui.right_edge = true;
            // the paper choice opens on the next frame
            app.print.open_paper = true;
            Ok(Value::Null)
        }
        "view.gridToggle" => {
            // G between the two grids; from a photo view it comes back to the grid style the user
            // chose last (#794), not always to Photo Grid
            if app.ui.fullscreen {
                // the full-screen preview sits over a view that is usually a grid already: G asks
                // for the grid the user chose, not a flip between the two grid styles
                app.leave_fullscreen_preview();
                app.ui.view = app.ui.grid_style;
            } else {
                app.ui.view = if app.ui.view.is_grid() {
                    if app.ui.view == ViewMode::PhotoGrid { ViewMode::SquareGrid } else { ViewMode::PhotoGrid }
                } else {
                    app.ui.grid_style
                };
                app.ui.grid_style = app.ui.view;
            }
            Ok(Value::Null)
        }
        "view.detail" => {
            app.leave_fullscreen_preview();
            app.ui.view = ViewMode::Detail;
            Ok(Value::Null)
        }
        "view.compare" => crate::panels::compare::enter_compare(app),
        "metadata.targetOnly" => {
            // {on?}; no params toggles. The Metadata panel shows and edits the active photo alone.
            app.ui.settings.metadata_target_only = p.get("on").and_then(Value::as_bool).unwrap_or(!app.ui.settings.metadata_target_only);
            Ok(json!({"on": app.ui.settings.metadata_target_only}))
        }
        "view.focusPoint" => {
            // {on?}; no params toggles. The camera's focus point (Sony, from the file) over the photo.
            app.ui.settings.focus_point = p.get("on").and_then(Value::as_bool).unwrap_or(!app.ui.settings.focus_point);
            Ok(json!({"on": app.ui.settings.focus_point}))
        }
        "view.spaceZoomsTo" => {
            // {to: focusPoint|eye} → {to}; it also turns Zoom to Focus Point on
            let to = p.get("to").and_then(Value::as_str).unwrap_or_default();
            let Some(z) = crate::state::SpaceZoom::ALL.into_iter().find(|z| z.id() == to) else {
                return Some(Err(format!("view.spaceZoomsTo: `to` is focusPoint or eye, not `{to}`")));
            };
            app.ui.settings.space_zoom = z;
            app.ui.settings.zoom_to_focus = true;
            Ok(json!({"to": z.id()}))
        }
        "view.zoomToFocus" => {
            // {on?}; no params toggles. Toggle Zoom (Z) centres on the focus point when there is one.
            app.ui.settings.zoom_to_focus = p.get("on").and_then(Value::as_bool).unwrap_or(!app.ui.settings.zoom_to_focus);
            Ok(json!({"on": app.ui.settings.zoom_to_focus}))
        }
        "view.hdrPreview" => {
            // {on?}; no params toggles. Off, an HDR display shows HDR edits as an SDR one does.
            app.ui.hdr_preview = p.get("on").and_then(Value::as_bool).unwrap_or(!app.ui.hdr_preview);
            Ok(json!({"on": app.ui.hdr_preview, "hdrDisplay": app.hdr_headroom.is_some()}))
        }
        "view.faceBoxes" => {
            app.ui.face_boxes = p.get("show").and_then(Value::as_bool).unwrap_or(!app.ui.face_boxes);
            Ok(json!({"show": app.ui.face_boxes}))
        }
        "view.people" => {
            // Everyone; the page just left stays one click away (the chip next to the title)
            app.leave_fullscreen_preview();
            if let Some(name) = app.ui.person_page.take() {
                app.ui.last_person = Some(name);
            }
            app.ui.person_from = None;
            app.ui.view = ViewMode::People;
            // everyone, not what the search box last narrowed the cards to
            app.ui.people_search.clear();
            Ok(json!({"people": app.session.catalog.people().len()}))
        }
        "view.person" => {
            // {name}: one person's page in the People view: their faces, and the faces that look like them
            let Some(name) = p.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()) else {
                return Some(Err("view.person: missing `name`".into()));
            };
            app.leave_fullscreen_preview();
            app.ui.view = ViewMode::People;
            app.ui.person_from = None;
            app.ui.person_page = Some(name.to_string());
            Ok(json!({"person": name}))
        }
        "view.survey" => {
            app.leave_fullscreen_preview();
            app.ui.view = ViewMode::Survey;
            Ok(json!({"photos": crate::panels::compare::survey_photos(app).len()}))
        }
        "view.reference" => {
            // the reference: the one set before, else the active photo (the next one becomes active)
            let vis = app.session.visible_cloned();
            let live = |id: u64| crate::panels::compare::live(app, lcc_catalog::PhotoId(id));
            let active = app.session.active().map(|a| a.0);
            let reference = app.ui.reference.filter(|r| live(*r)).or_else(|| active.filter(|a| live(*a)));
            let Some(r) = reference else {
                return Some(Err(match active {
                    Some(a) => format!("photo {a} is in Recently Deleted: restore it to use it as the reference"),
                    None => "select a photo to use as the reference".into(),
                }));
            };
            app.ui.reference = Some(r);
            if app.session.active().is_none_or(|a| a.0 == r)
                && let Some(i) = vis.iter().position(|x| x.0 == r)
                && let Some(next) = vis.get(i + 1).or(i.checked_sub(1).and_then(|j| vis.get(j)))
            {
                let _ = app.session.execute("library.select", &json!({"ids": [next.0]}));
            }
            app.leave_fullscreen_preview();
            app.ui.view = ViewMode::Reference;
            Ok(json!({"reference": r, "active": app.session.active().map(|a| a.0)}))
        }
        "photo.setReference" => {
            // only an absent (or null) `id` means the active photo; any other non-id is refused
            let id = match p.get("id").filter(|v| !v.is_null()) {
                Some(v) => match v.as_u64() {
                    Some(id) => Some(id),
                    None => return Some(Err(format!("photo.setReference: `id` must be a photo id, not {v}"))),
                },
                None => app.session.active().map(|a| a.0),
            };
            let Some(id) = id else { return Some(Err("select a photo to use as the reference".into())) };
            match app.session.catalog.photo(lcc_catalog::PhotoId(id)) {
                None => return Some(Err(format!("no photo {id}"))),
                Some(p) if p.deleted => return Some(Err(format!("photo {id} is in Recently Deleted: restore it to use it as the reference"))),
                Some(_) => {}
            }
            app.ui.reference = Some(id);
            Ok(json!({"reference": id}))
        }
        "compare.swap" => crate::panels::compare::swap(app),
        "compare.set" => {
            let Some(id) = p.get("id").and_then(Value::as_u64) else { return Some(Err("compare.set needs `id`".into())) };
            crate::panels::compare::set_photo(app, lcc_catalog::PhotoId(id), p.get("side").and_then(Value::as_str))
        }
        "compare.makeSelect" => crate::panels::compare::make_select(app),
        "view.autoAdvance" => {
            app.ui.auto_advance = !app.ui.auto_advance;
            app.toast(&ctx, crate::i18n::tr(if app.ui.auto_advance { "Auto Advance On" } else { "Auto Advance Off" }));
            Ok(json!({"autoAdvance": app.ui.auto_advance}))
        }
        "view.back" => {
            if app.session.active_spot.is_some() {
                let _ = app.run("spot.select", json!({"index": null}));
            } else if app.ui.dialog.is_some() {
                app.ui.dialog = None;
            } else if app.ui.lights_out != crate::module::LightsOut::Off {
                app.ui.lights_out = crate::module::LightsOut::Off;
            } else if app.ui.lib.painter.is_some() {
                app.ui.lib.painter = None;
            } else if app.ui.fullscreen {
                app.ui.fullscreen = false;
                app.ui.slideshow = None;
            } else if !app.ui.tool.is_empty() || app.ui.mask_new.is_some() {
                if app.ui.right == RightPanel::Masking {
                    crate::panels::masking::disarm(app);
                } else {
                    app.ui.tool.clear();
                    // a tool waiting for a new mask is put away with it (the mask selected before comes back)
                    app.ui.mask_new = None;
                }
            } else if app.ui.right == RightPanel::Crop && app.ui.view == ViewMode::Detail {
                // Esc leaves the crop tool (the crop stays), as C does; the next Esc goes back
                panel(app, &ctx, RightPanel::Crop, "Crop, Rotate, Geometry");
            } else if app.ui.view == ViewMode::People && app.ui.person_page.is_some() {
                app.ui.person_page = None;
            } else if matches!(app.ui.view, ViewMode::Compare | ViewMode::Survey | ViewMode::Reference) {
                app.ui.view = ViewMode::Detail;
            } else if app.ui.view == ViewMode::Detail {
                // a photo opened from a person's page (or the unnamed faces) goes back there, any other to the grid
                match app.ui.person_from.take() {
                    Some((name, photo)) if app.session.active().is_some_and(|a| a.0 == photo) => {
                        app.ui.view = ViewMode::People;
                        app.ui.person_page = name;
                    }
                    _ => {
                        let prev_grid = app
                            .ui
                            .layouts
                            .get(&crate::module::ModuleId::Library)
                            .and_then(|l| l.view)
                            // (not the People view: a photo opened from a person returns there by its own route, any other to the grid)
                            .filter(|v| !matches!(v, ViewMode::Detail | ViewMode::Reference | ViewMode::People));
                        // no view the Library was left in: the grid style the user chose last (#794)
                        let target_grid = prev_grid.unwrap_or(app.ui.grid_style);
                        if app.ui.module == crate::module::ModuleId::Develop {
                            let _ = crate::module::switch(app, crate::module::ModuleId::Library);
                        }
                        app.ui.view = target_grid;
                    }
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
                app.end_midi_interaction();
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
            app.toast(&ctx, crate::i18n::tr(label));
            Ok(json!({"infoOverlay": app.ui.info_overlay}))
        }
        "view.viewOptions" => {
            // `info1` / `info2` set the lines, `briefly` the Info that Show Briefly When Photo Changes
            // shows, and `loupeMessages` the loading / rendering message, for an agent; without any of
            // them the window opens
            let mut info = app.ui.settings.loupe_info;
            let mut set = false;
            // `loupeMessages` sets Show message when loading or rendering photos
            let messages = match p.get("loupeMessages") {
                None => app.ui.settings.loupe_messages,
                Some(v) => match v.as_bool() {
                    Some(on) => {
                        set = true;
                        on
                    }
                    None => return Some(Err("`loupeMessages`: true|false".into())),
                },
            };
            for (key, lines) in [("info1", &mut info.info1), ("info2", &mut info.info2)] {
                let Some(v) = p.get(key) else { continue };
                match serde_json::from_value(v.clone()) {
                    Ok(v) => *lines = v,
                    Err(_) => {
                        return Some(Err(format!("`{key}`: three of none|fileName|captureDate|dimensions|exposure|camera|lens|cameraAndLens")));
                    }
                }
                set = true;
            }
            if let Some(v) = p.get("briefly") {
                match serde_json::from_value(v.clone()) {
                    Ok(v) => info.briefly = v,
                    Err(_) => return Some(Err("`briefly`: off|basic|exposure (neither, Info 1 or Info 2)".into())),
                }
                set = true;
            }
            if set {
                app.ui.settings.loupe_info = info;
                app.ui.settings.loupe_messages = messages;
            } else {
                app.ui.dialog = Some(Dialog::ViewOptions);
            }
            Ok(json!({"loupeInfo": app.ui.settings.loupe_info, "loupeMessages": app.ui.settings.loupe_messages}))
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
            app.toast(&ctx, crate::i18n::tr("Layout Image On"));
            Ok(json!({"layoutImage": app.ui.layout_image}))
        }
        "view.layoutImage" => {
            let show = p.get("show").and_then(Value::as_bool).unwrap_or(!app.ui.layout_image.show);
            // nothing chosen yet: showing it chooses one first
            if show && app.ui.layout_image.path.trim().is_empty() {
                return run_ui_command(app, "view.chooseLayoutImage", &json!({}));
            }
            app.ui.layout_image.show = show;
            app.toast(&ctx, crate::i18n::tr(if show { "Layout Image On" } else { "Layout Image Off" }));
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
        "dialog.watermark" => {
            let prev = app.session.last_export.clone().unwrap_or_default();
            let opts = lcc_engine::export::ExportOptions::from_json(&prev);
            let wm = opts.watermark.unwrap_or_default();
            app.ui.dialog = Some(Dialog::Watermark { wm, preset_name: String::new() });
            Ok(Value::Null)
        }
        "app.settings" => {
            // a section opens its tab and is brought into view
            let section = p.get("section").and_then(Value::as_str);
            let section_tab = match section {
                None => None,
                Some(s) => match crate::panels::settings::SECTIONS.iter().find(|(id, _)| *id == s) {
                    Some((_, tab)) => Some(*tab),
                    None => {
                        let all: Vec<&str> = crate::panels::settings::SECTIONS.iter().map(|(id, _)| *id).collect();
                        return Some(Err(format!("unknown settings section `{s}` ({})", all.join("|"))));
                    }
                },
            };
            // without a tab, the section shown last (else General); `page` names the category as
            // `tab` does, whichever way Settings lists its categories (left, top, the sheet's rail)
            let last = app.ui.settings.settings_tab.clone();
            let last = if crate::settings_screen::known(&last) { last.as_str() } else { "general" };
            let asked = p.get("tab").or_else(|| p.get("page")).and_then(Value::as_str);
            let tab = section_tab.or(asked).unwrap_or(last);
            if !crate::panels::settings::TABS.iter().any(|(id, _)| *id == tab) && !crate::settings_screen::known(tab) {
                let tabs: Vec<&str> = crate::panels::settings::TABS.iter().map(|(id, _)| *id).collect();
                return Some(Err(format!("unknown settings tab `{tab}` ({})", tabs.join("|"))));
            }
            app.ui.dialog = Some(Dialog::Settings { tab: tab.into() });
            app.ui.settings_section = section.map(str::to_string);
            Ok(json!({"tab": tab, "section": section}))
        }
        "app.autoImportToggle" => {
            // {on?}: switch watching the folder on or off (kept when off); with no folder yet, Settings opens at it
            let on = p.get("on").and_then(Value::as_bool).unwrap_or(app.session.import_defaults.auto_active_folder().is_none());
            if on && app.session.import_defaults.auto_folder.is_none() {
                return run_ui_command(app, "app.settings", &json!({"section": "autoImport"}));
            }
            app.session.execute("library.autoImport", &json!({"enabled": on})).map_err(|e| e.to_string())
        }
        "app.openLibrary" => crate::panels::settings::open_library(app, p),
        "app.newLibrary" => crate::panels::settings::new_library(app, p),
        "app.renameLibrary" => crate::panels::settings::rename_library(app, p),
        "app.recentLibraries" => crate::panels::settings::recent_libraries(app, p),
        "app.clearRecentLibraries" => crate::panels::settings::recent_libraries(app, &json!({"clear": true})),
        "app.displayProfile" => crate::panels::settings::display_profile(app, p),
        "app.lensDbFolder" => crate::panels::settings::lens_db_folder(app, p),
        "app.sam3Folder" => crate::panels::ai_models::sam3_folder_command(app, p),
        "app.folders" => Ok(crate::panels::settings_folders::folders(app)),
        "app.exportFolder" => crate::panels::settings_folders::export_folder(app, p),
        "file.backupLibrary" | "file.restoreLibrary" => {
            let action = if id == "file.backupLibrary" { app.services.backup_library.as_mut() } else { app.services.restore_library.as_mut() };
            match action {
                Some(f) => f(&mut app.session),
                None => Err("not available here: on the desktop the catalog is a folder; back it up with your other files".into()),
            }
        }
        "view.filmstrip" => {
            app.ui.filmstrip = !app.ui.filmstrip;
            Ok(Value::Null)
        }
        "view.lockZoomPosition" | "view.scrollByRow" | "view.mirrorImage" => {
            // {on?: bool} (toggles when omitted) → {on}: view-only switches (`crate::view_state`)
            let flag = match id {
                "view.lockZoomPosition" => &mut app.ui.settings.lock_zoom,
                "view.scrollByRow" => &mut app.ui.settings.scroll_by_row,
                _ => &mut app.ui.settings.mirror_image,
            };
            *flag = p.get("on").and_then(Value::as_bool).unwrap_or(!*flag);
            Ok(json!({"on": *flag}))
        }
        "view.shotInfo" => {
            // {on?: bool} (toggles when omitted): the line of shot info under the loupe's photo
            app.ui.shot_info = p.get("on").and_then(Value::as_bool).unwrap_or(!app.ui.shot_info);
            Ok(json!({"on": app.ui.shot_info}))
        }
        "view.frameRate" => {
            // {show?: bool} (toggles when omitted)
            app.ui.frame_rate = p.get("show").and_then(Value::as_bool).unwrap_or(!app.ui.frame_rate);
            Ok(json!({"show": app.ui.frame_rate}))
        }
        "view.leftPanel" => {
            app.ui.left_panel = !app.ui.left_panel;
            Ok(Value::Null)
        }
        "view.rightPanel" => {
            // Classic's F8, as the top bar's right-panel button: fold the right side away and back
            // (`right_collapsed`; the panel that was open comes back). Develop with no panel open
            // opens Edit. In Library, `right == None` is its Classic column, not a hidden panel.
            let develop = app.ui.module == crate::module::ModuleId::Develop;
            if develop && app.ui.right == RightPanel::None {
                app.ui.right_collapsed = false;
                app.ui.right_edge = true;
                app.ui.right = RightPanel::Edit;
                if !matches!(app.ui.view, ViewMode::Detail) {
                    app.ui.view = ViewMode::Detail;
                }
            } else {
                crate::module::toggle_right(app);
                if app.ui.right_collapsed && app.ui.right.is_edit_tool() && !matches!(app.ui.tool.as_str(), "wbPicker" | "fringe") {
                    // hiding a tool ends it, as closing its panel does
                    app.ui.tool.clear();
                    let _ = app.session.end_interaction();
                }
            }
            Ok(json!({"open": !app.ui.right_collapsed}))
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
        // {on?}: the photo without its edit while on (the Compact histogram's Original Photo box);
        // no params toggles
        "view.showOriginal" => {
            let shown = app.ui.before_after == BeforeAfter::Original;
            let on = match bool_param(id, p, "on") {
                Ok(v) => v.unwrap_or(!shown),
                Err(e) => return Some(Err(e)),
            };
            if on != shown {
                app.ui.before_after = if on { BeforeAfter::Original } else { BeforeAfter::Off };
            }
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
            // {on?, space?, profile?: ICC path | null, intent?: perceptual|relative, paperInk?,
            // destWarning?, displayWarning?}; no params toggles
            let has = |k: &str| p.get(k).is_some();
            if let Some(s) = p.get("space").and_then(Value::as_str) {
                match lcc_engine::pipeline::OutputSpace::parse(s) {
                    Some(sp) => {
                        app.ui.proof.space = sp;
                        // a standard space replaces a device profile (unless one is given too)
                        app.ui.proof_profile = None;
                    }
                    None => return Some(Err(format!("view.softProof: unknown space {s:?} (srgb|displayP3|adobeRgb|proPhoto|rec2020)"))),
                }
            }
            if let Some(i) = p.get("intent") {
                match i.as_str().and_then(lcc_engine::proof::ProofIntent::parse) {
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
                Some(path) => match lcc_engine::proof::load(&path, app.ui.proof_intent, app.ui.proof_paper_ink) {
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
            // the Modern layout's floating histogram (shown small again), else Classic's panel
            if crate::modern::on(app) {
                crate::modern::toggle_histogram(app);
            } else {
                app.ui.histogram = !app.ui.histogram;
            }
            Ok(Value::Null)
        }
        "view.histogram.mode" => {
            // {mode?: rgb | luminance | channels | chroma | vectorscope | waveform | parade}; no parameter steps to the
            // next one, as the overlay-mode commands do. Selecting a mode shows the panel it is about.
            use lcc_raster::HistogramMode;
            let cur = HistogramMode::parse(&app.ui.histogram_mode).unwrap_or_default();
            let next = match p.get("mode").and_then(Value::as_str) {
                Some(m) => match HistogramMode::parse(m) {
                    Some(v) => v,
                    None => {
                        let names: Vec<&str> = HistogramMode::ALL.iter().map(|m| m.name()).collect();
                        return Some(Err(format!("view.histogram.mode: unknown mode `{m}` ({})", names.join("|"))));
                    }
                },
                None => cur.next(),
            };
            app.ui.histogram_mode = next.name().into();
            app.ui.histogram = true;
            Ok(json!({"mode": next.name()}))
        }
        "view.histogram.second" => {
            // {mode?: rgb | … | parade | off}: a second histogram under the first, drawing that
            // mode; no parameter turns it on (RGB Parade) or off
            use lcc_raster::HistogramMode;
            let next = match p.get("mode").and_then(Value::as_str) {
                Some("off" | "none" | "") => None,
                Some(m) => match HistogramMode::parse(m) {
                    Some(v) => Some(v),
                    None => {
                        let names: Vec<&str> = HistogramMode::ALL.iter().map(|m| m.name()).collect();
                        return Some(Err(format!("view.histogram.second: unknown mode `{m}` ({}|off)", names.join("|"))));
                    }
                },
                None if app.ui.histogram_second.is_empty() => Some(HistogramMode::Parade),
                None => None,
            };
            app.ui.histogram_second = next.map(|m| m.name().to_string()).unwrap_or_default();
            if next.is_some() {
                app.ui.histogram = true;
            }
            Ok(json!({"mode": next.map(|m| m.name())}))
        }
        "view.histogram.scale" => {
            // {scale?: linear | logarithmic}; no parameter steps to the other one
            use lcc_raster::HistogramScale;
            let cur = HistogramScale::parse(&app.ui.histogram_scale).unwrap_or_default();
            let next = match p.get("scale").and_then(Value::as_str) {
                Some(s) => match HistogramScale::parse(s) {
                    Some(v) => v,
                    None => {
                        let names: Vec<&str> = HistogramScale::ALL.iter().map(|m| m.name()).collect();
                        return Some(Err(format!("view.histogram.scale: unknown scale `{s}` ({})", names.join("|"))));
                    }
                },
                None => cur.next(),
            };
            app.ui.histogram_scale = next.name().into();
            app.ui.histogram = true;
            Ok(json!({"scale": next.name()}))
        }
        "view.maskOverlay" => {
            if let Some(auto) = p.get("auto").and_then(Value::as_bool) {
                app.ui.mask_overlay_auto = auto;
                if !auto {
                    app.ui.mask_peek_until = None;
                }
            }
            // O always turns off whatever overlay shows, All Mask Overlays included: one key to know
            let toggle = p.get("auto").is_none().then_some(!(app.ui.mask_overlay || app.ui.mask_overlay_all));
            if let Some(show) = p.get("show").and_then(Value::as_bool).or(toggle) {
                app.ui.mask_overlay = show;
                app.ui.mask_overlay_pinned = show;
                // an explicit O / checkbox overrides a picked mask's peek (issue #1109)
                app.ui.mask_peek_until = None;
                if !show {
                    app.ui.mask_overlay_all = false;
                }
            }
            Ok(json!({"maskOverlay": app.ui.mask_overlay, "auto": app.ui.mask_overlay_auto, "maskOverlayAll": app.ui.mask_overlay_all}))
        }
        "view.maskOverlayAll" => {
            // `on` sets it; without it Ctrl+Shift+O toggles (O also turns it off)
            app.ui.mask_overlay_all = p.get("on").and_then(Value::as_bool).unwrap_or(!app.ui.mask_overlay_all);
            Ok(json!({"maskOverlayAll": app.ui.mask_overlay_all}))
        }
        "view.maskOverlayMode" => {
            use lcc_engine::pipeline::MaskView;
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
            // ⇧O shows the overlay in its new look; the overlay menu's choice of look (`show: false`) leaves it as it
            // was, so picking a look never latches the overlay on (OneSlips 0c58de7ad: how it got stuck on)
            if p.get("show").and_then(Value::as_bool) != Some(false) {
                app.ui.mask_overlay = true;
            }
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
        "brush.preset" => {
            let want = match p.get("slot").and_then(Value::as_str) {
                None => 1 - app.ui.brush_slot.min(1),
                Some(s) if s.eq_ignore_ascii_case("a") => 0,
                Some(s) if s.eq_ignore_ascii_case("b") => 1,
                Some(other) => return Some(Err(format!("brush.preset: `slot` is \"a\" or \"b\", not {other:?}"))),
            };
            Ok(switch_brush_preset(app, want))
        }
        "brush.erase" => {
            let on = p.get("on").and_then(Value::as_bool).unwrap_or(!app.ui.brush_erase);
            if on && app.ui.tool != "brush" {
                // erasing needs the brush: pick it up on the selected mask
                if let Err(e) = app.run("tool.brush", json!({})) {
                    return Some(Err(e));
                }
            }
            app.ui.brush_erase = on;
            Ok(json!({"on": on}))
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
            if let Some(unselected) = p.get("unselected").and_then(Value::as_bool) {
                app.ui.mask_unselected_pins = unselected;
            }
            if let Some(show) = p.get("show").and_then(Value::as_bool) {
                app.ui.mask_pins = show;
            } else if p.get("unselected").is_none() {
                app.ui.mask_pins = !app.ui.mask_pins;
            }
            Ok(json!({"maskPins": app.ui.mask_pins, "unselected": app.ui.mask_unselected_pins}))
        }
        "view.maskOverlayAuto" => {
            app.ui.mask_overlay_auto = p.get("auto").and_then(Value::as_bool).unwrap_or(!app.ui.mask_overlay_auto);
            Ok(json!({"auto": app.ui.mask_overlay_auto}))
        }
        "view.maskUnselectedPins" => {
            app.ui.mask_unselected_pins = p.get("show").and_then(Value::as_bool).unwrap_or(!app.ui.mask_unselected_pins);
            Ok(json!({"show": app.ui.mask_unselected_pins}))
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
        // A under the Crop panel: the aspect lock (the panel's button runs the same toggle)
        "crop.lockAspect" => app.run("crop.aspect", json!({"aspect": "toggle"})),
        "view.cropOverlay" => {
            app.ui.crop_overlay = app.ui.crop_overlay.next_in(&app.ui.crop_overlay_cycle);
            Ok(json!({"overlay": app.ui.crop_overlay}))
        }
        "dialog.bookPreferences" => {
            app.ui.dialog = Some(Dialog::BookPrefs { prefs: app.ui.book_prefs });
            Ok(Value::Null)
        }
        "dialog.cropOverlays" => {
            app.ui.dialog = Some(Dialog::CropOverlays { picked: app.ui.crop_overlay_cycle.clone() });
            Ok(Value::Null)
        }
        // {overlays: ["thirds", "grid", "golden", "diagonal", "triangle", "spiral", "none"]}: the guides
        // Cycle Crop Overlay visits; at least one must stay
        "view.cropOverlays" => {
            use crate::state::CropOverlay;
            let Some(names) = p.get("overlays").and_then(Value::as_array) else {
                return Some(Err("view.cropOverlays: missing overlays (a list of guide names)".into()));
            };
            let mut picked: Vec<CropOverlay> = Vec::new();
            for n in names {
                let Ok(o) = serde_json::from_value::<CropOverlay>(n.clone()) else {
                    return Some(Err(format!("view.cropOverlays: unknown overlay {n}")));
                };
                if !picked.contains(&o) {
                    picked.push(o);
                }
            }
            if picked.is_empty() {
                return Some(Err("view.cropOverlays: at least one overlay must stay ticked".into()));
            }
            app.ui.crop_overlay_cycle = CropOverlay::ALL.into_iter().filter(|o| picked.contains(o)).collect();
            Ok(json!({"overlays": app.ui.crop_overlay_cycle}))
        }
        "view.cropMaskOpacity" => {
            let level = p
                .get("opacity")
                .and_then(Value::as_u64)
                .and_then(|v| u8::try_from(v).ok())
                .filter(|v| crate::state::CROP_MASK_OPACITY_LEVELS.contains(v));
            let Some(opacity) = level else {
                return Some(Err("view.cropMaskOpacity: opacity must be 0, 20, 50, 60, 80 or 100".into()));
            };
            app.ui.crop_mask_opacity = opacity;
            Ok(json!({"opacity": opacity}))
        }
        "photo.rate0" | "photo.rate1" | "photo.rate2" | "photo.rate3" | "photo.rate4" | "photo.rate5" => {
            let rating = id.strip_prefix("photo.rate").and_then(|n| n.parse::<u8>().ok()).unwrap_or(0);
            return Some(app.run("photo.rate", json!({"rating": rating})));
        }
        "photo.labelRed" | "photo.labelYellow" | "photo.labelGreen" | "photo.labelBlue" => {
            let label = id.strip_prefix("photo.label").unwrap_or("").to_lowercase();
            return Some(app.run("photo.label", json!({"label": label, "toggle": true})));
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
                app.ui.view = app.ui.grid_style;
            }
            // a hidden filter bar keeps no Metadata column search of its own
            if !app.ui.filter_bar {
                app.ui.filter_column_search.clear();
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
            // back on the tool used last (Heal stays Heal)
            if app.ui.right == RightPanel::Remove && app.ui.tool.is_empty() {
                let last =
                    crate::panels::right::REMOVE_MODES.iter().find(|(_, _, t, _)| *t == app.ui.remove_tool).map_or("remove", |(_, _, t, _)| *t);
                app.ui.tool = last.into();
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
            // the Modern layout's Info card (Library), else Classic's Info panel
            if crate::modern::on(app) {
                app.ui.modern.info = !app.ui.modern.info;
                if app.ui.modern.info && app.ui.module != crate::module::ModuleId::Library {
                    let _ = app.run("module.library", serde_json::json!({}));
                }
            } else {
                panel(app, &ctx, RightPanel::Info, "Info");
            }
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
        "panel.editKeywords" => {
            // the Keywords panel with its Keyword List open; the keyword picked there opens in
            // Edit Keyword Tag (with none picked, no dialog opens)
            if app.ui.right != RightPanel::Keywords {
                panel(app, &ctx, RightPanel::Keywords, "Keywords");
            }
            app.ui.folded_sections.retain(|s| s != "keywordList");
            if let Some(k) = app.ui.keyword_list_selected.clone().filter(|k| app.session.catalog.has_keyword(k)) {
                app.ui.dialog = Some(crate::panels::keyword_list::edit_dialog(app, &k));
            }
            Ok(Value::Null)
        }
        "panel.activity" => {
            panel(app, &ctx, RightPanel::Activity, "History");
            Ok(Value::Null)
        }
        "panel.presets" => {
            // the presets are a panel of Develop's left column, never a column of their own beside
            // (or in place of) the Edit panels: the command shows that panel, or folds it
            if crate::panels::presets::shown(app) {
                crate::panels::presets::set_shown(app, false);
                Ok(Value::Null)
            } else {
                crate::panels::presets::reveal(app).map(|()| Value::Null)
            }
        }
        "mask.editComponent" => crate::panels::masking::edit_component(app, p),
        "tool.done" => {
            // Return commits a tool panel (crop, remove, red eye, masking): back to Edit
            use RightPanel::*;
            if app.ui.dialog.is_none() && matches!(app.ui.right, Crop | Remove | RedEye | Masking) {
                app.end_midi_interaction();
                let _ = app.session.end_interaction();
                app.ui.tool.clear();
                if app.ui.right == RightPanel::Masking {
                    crate::panels::masking::disarm(app);
                }
                // back to the panel shown before the tool (hidden if it was)
                app.ui.right = app.ui.leave_tool();
            }
            Ok(Value::Null)
        }
        "view.colorMixer" => color_mixer_layout(app, p),
        "panel.close" => {
            app.ui.right = RightPanel::None;
            Ok(Value::Null)
        }
        "panel.collapseRight" => {
            crate::module::toggle_right(app);
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
                "none" => {
                    app.ui.tool.clear();
                    if app.ui.right == RightPanel::Masking {
                        crate::panels::masking::disarm(app);
                    }
                }
                "guidedUpright" => {
                    // Crop & Geometry with Guided Upright on, ready to draw guides
                    app.ui.right = RightPanel::Crop;
                    app.ui.view = ViewMode::Detail;
                    let guided = app
                        .session
                        .active()
                        .and_then(|id| app.session.develop_of(id))
                        .is_some_and(|d| d.geometry.upright == lcc_develop::Upright::Guided);
                    if !guided && let Err(e) = app.run("geometry.upright", json!({"mode": "guided"})) {
                        return Some(Err(e));
                    }
                    app.ui.tool = "guidedUpright".into();
                }
                "brush" => {
                    if p.get("toggle").and_then(Value::as_bool) == Some(true) && app.ui.tool == "brush" {
                        crate::panels::masking::disarm(app);
                        return Some(Ok(json!({"disarmed": "brush"})));
                    }
                    // A new mask is armed: the first stroke makes it. Add/Subtract are selection
                    // operations, not the brush's Erase mode: paint positively into a new component.
                    if p.get("new").and_then(Value::as_bool) == Some(true) {
                        if let Err(e) = crate::panels::masking::arm_new(app, "brush") {
                            return Some(Err(e));
                        }
                        app.ui.brush_erase = false;
                        app.ui.mask_overlay = true;
                        return Some(Ok(json!({"armed": "brush"})));
                    }
                    if let Some(op) = p.get("op") {
                        if let Err(e) = app.run("mask.addComponent", json!({"kind": "brush", "op": op})) {
                            return Some(Err(e));
                        }
                        app.ui.brush_erase = false;
                        app.ui.mask_overlay = true;
                        if let Some(d) = app.session.active().and_then(|id| app.session.develop_of(id))
                            && let Some(m) = d.masks.iter().find(|m| Some(m.id) == app.session.active_mask)
                        {
                            app.ui.active_mask_component = Some(m.components.len().saturating_sub(1));
                        }
                    }
                    app.ui.right = RightPanel::Masking;
                    app.ui.view = ViewMode::Detail;
                    app.ui.tool = "brush".into();
                }
                "linear" | "radial" => {
                    if p.get("toggle").and_then(Value::as_bool) == Some(true) && app.ui.tool == tool {
                        crate::panels::masking::disarm(app);
                        return Some(Ok(json!({"disarmed": tool})));
                    }
                    // armed: a drag on the photo draws the gradient (a click places a default one)
                    return Some(crate::panels::masking::arm_new(app, tool));
                }
                "object" => {
                    if p.get("toggle").and_then(Value::as_bool) == Some(true) && app.ui.tool == "object" {
                        crate::panels::masking::disarm(app);
                        return Some(Ok(json!({"disarmed": "object"})));
                    }
                    let r = if p.get("new").and_then(Value::as_bool) == Some(true) || app.session.active_mask.is_none() || app.ui.mask_new.is_some() {
                        crate::panels::masking::arm_new(app, "object")
                    } else if let Some(op) = p.get("op").and_then(Value::as_str) {
                        app.run("mask.addComponent", json!({"kind": "object", "op": op}))
                    } else {
                        app.ui.right = RightPanel::Masking;
                        app.ui.view = ViewMode::Detail;
                        app.ui.tool = "object".into();
                        Ok(json!({"armed": "object"}))
                    };
                    return Some(r);
                }
                // W again puts the selector away without sampling (OneSlips, test notes 2026-10-25 note 13)
                // {target: curve|hue|sat|lum; none or missing puts the targeted adjustment away}
                "tat" => match p.get("target").and_then(Value::as_str).filter(|t| !t.is_empty() && *t != "none") {
                    None => {
                        if app.ui.tool.starts_with("tat:") {
                            app.ui.tool.clear();
                        }
                    }
                    Some(t) => {
                        if let Err(e) = crate::panels::edit::start_tat(app, t) {
                            return Some(Err(e));
                        }
                    }
                },
                "wbPicker" if app.ui.tool == "wbPicker" && p.get("sample").is_none() => app.ui.tool.clear(),
                "wbPicker" => {
                    app.ui.right = RightPanel::Edit;
                    app.ui.view = ViewMode::Detail;
                    app.ui.tool = "wbPicker".into();
                    // {sample?: side of the averaged square in preview pixels}
                    if let Some(n) = p.get("sample").and_then(Value::as_f64).filter(|v| v.is_finite()) {
                        app.ui.wb_sample = crate::panels::detail::wb_sample_step(n);
                    }
                }
                "fringe" if app.ui.tool == "fringe" => app.ui.tool.clear(),
                "fringe" => {
                    app.ui.right = RightPanel::Edit;
                    app.ui.view = ViewMode::Detail;
                    app.ui.tool = "fringe".into();
                }
                "people" | "colorRange" | "luminanceRange" | "depthRange" => {
                    if app.session.active().is_none() {
                        return Some(Err(format!("tool.{tool}: no photo")));
                    }
                    app.cancel_brush_gesture();
                    crate::panels::masking::disarm(app);
                    app.ui.people_pick = None;
                    app.ui.concept_pick = false;
                    app.mask_pick_op = None;
                    app.ui.right = RightPanel::Masking;
                    app.ui.view = ViewMode::Detail;
                    let ctx = app.tasks.repaint.clone().unwrap_or_default();
                    crate::panels::masking::start_new(app, &ctx, tool);
                    return Some(Ok(json!({"started": tool})));
                }
                "pointColor" => {
                    let Some(photo) = app.session.active() else {
                        return Some(Err("tool.pointColor: no photo".into()));
                    };
                    let d = app.session.develop_of(photo).unwrap_or_default();
                    if crate::is_bw(&d) {
                        return Some(Err("tool.pointColor: Point Color needs a colour photo".into()));
                    }
                    if d.point_colors.len() >= lcc_develop::MAX_POINT_COLORS {
                        return Some(Err(format!("tool.pointColor: a photo keeps at most {} point colours", lcc_develop::MAX_POINT_COLORS)));
                    }
                    if let Err(e) = color_mixer_layout(app, &json!({"tab": "point"})) {
                        return Some(Err(e));
                    }
                    crate::panels::masking::disarm(app);
                    app.ui.tool = "pointColor".into();
                }
                other => return Some(Err(format!("unknown tool `{other}`"))),
            }
            Ok(Value::Null)
        }
        "remove.useGenerative" => crate::panels::genfill::use_generative(app, p),
        "remove.detectObjects" => crate::panels::genfill::detect_objects(app, p),
        "dialog.cameraProfile" => crate::chart_profile::open(app),
        "cameraProfile.second" => crate::chart_profile::choose_second(app, p),
        "cameraProfile.removeSecond" => crate::chart_profile::remove_second(app),
        "cameraProfile.pickSecond" => crate::chart_profile::pick_second(app, p),
        "cameraProfile.zoom" => crate::chart_profile::zoom(app, p),
        "cameraProfile.resolve" => crate::chart_profile::resolve_clash(app, p),
        "cameraProfile.find" | "cameraProfile.align" | "cameraProfile.turn" => crate::chart_profile::adjust(app, id, p),
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
                .and_then(|id| app.session.catalog.album(lcc_catalog::AlbumId(id)).filter(|a| a.is_smart()).cloned());
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
                    rules: lcc_catalog::RuleSet { rules: vec![crate::panels::rules_editor::new_rule()], ..Default::default() },
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
            // a time that doesn't fully read isn't offered as the starting point (`10:0` would read as
            // midnight); a year or a month alone (`1983-04`) is, as stated
            let time = app
                .session
                .active()
                .and_then(|id| app.session.catalog.photo(id))
                .map(|p| p.date())
                .filter(|d| lcc_catalog::dates::shift_iso(d, 0).is_some() || lcc_catalog::dates::partial_capture(d).is_some())
                .map(|d| d.replace('T', " "))
                .unwrap_or_default();
            let mode = p.get("mode").and_then(Value::as_str).unwrap_or("set").to_string();
            app.ui.dialog = Some(Dialog::CaptureTime {
                mode,
                time: time.get(..19).unwrap_or(&time).to_string(),
                days: 0,
                hours: 0,
                minutes: 0,
                zone: 0.0,
                each: false,
            });
            Ok(Value::Null)
        }
        "dialog.keywordSet" => {
            crate::panels::right::edit_keyword_set(app);
            Ok(Value::Null)
        }
        "mask.startComponent" => {
            // {kind, op: add|subtract|intersect}: Add / Subtract / Intersect Mask with (the Masking panel's menus)
            let kind = p.get("kind").and_then(Value::as_str).ok_or("missing `kind`");
            let op = p
                .get("op")
                .and_then(Value::as_str)
                .filter(|o| matches!(*o, "add" | "subtract" | "intersect"))
                .ok_or("`op` is add|subtract|intersect");
            let (kind, op) = match (kind, op) {
                (Ok(k), Ok(o)) => (k.to_string(), o.to_string()),
                (Err(e), _) | (_, Err(e)) => return Some(Err(format!("mask.startComponent: {e}"))),
            };
            if !crate::panels::masking::MASK_KINDS.iter().any(|(k, _)| *k == kind) {
                return Some(Err(format!("mask.startComponent: unknown mask kind `{kind}`")));
            }
            if app.session.active_mask.is_none() {
                return Some(Err("mask.startComponent: no mask is selected".into()));
            }
            app.ui.right = RightPanel::Masking;
            app.ui.view = ViewMode::Detail;
            crate::panels::masking::start_component(app, &ctx, &kind, &op);
            Ok(Value::Null)
        }
        "dialog.labelNames" => {
            let names = lcc_catalog::ColorLabel::ALL.iter().map(|l| app.session.catalog.custom_label_name(*l).unwrap_or("").to_string()).collect();
            app.ui.dialog = Some(Dialog::LabelNames { names, save_as: String::new() });
            Ok(Value::Null)
        }
        "dialog.rename" => {
            let targets = app.session.targets(p).unwrap_or_else(|_| app.session.selected());
            if targets.len() == 1 {
                let id = targets[0];
                let name = p
                    .get("name")
                    .or_else(|| p.get("template"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| app.session.catalog.photo(id).map(|ph| ph.file_name.clone()).unwrap_or_default());
                app.ui.dialog = Some(Dialog::RenamePhoto { id: id.0, name });
            } else if !targets.is_empty() {
                let template = p.get("template").and_then(Value::as_str).unwrap_or("{name}").to_string();
                app.ui.dialog =
                    Some(Dialog::Rename { template, start: p.get("start").and_then(Value::as_u64).unwrap_or(1) as u32, texts: Default::default() });
            }
            Ok(Value::Null)
        }
        "dialog.createPreset" => {
            app.ui.dialog = Some(Dialog::create_preset(&app.session));
            Ok(Value::Null)
        }
        "dialog.updatePreset" => {
            let Some(id) = p.get("id").and_then(Value::as_str).map(str::to_string) else { return Some(Err("missing `id`".into())) };
            let Some(preset) = app.session.presets.iter().find(|x| x.id == id) else { return Some(Err(format!("unknown preset `{id}`"))) };
            if preset.builtin {
                return Some(Err("built-in presets can't be changed".into()));
            }
            let (name, groups) = (preset.name.clone(), lcc_develop::SettingsPick::of_partial(&preset.settings).to_ids());
            app.ui.dialog = Some(Dialog::UpdatePreset { id, name, groups });
            Ok(Value::Null)
        }
        "dialog.presetsManager" => {
            let query = p.get("query").and_then(Value::as_str).unwrap_or("").to_string();
            app.ui.dialog = Some(Dialog::PresetsManager { query, confirm: None, rename: None });
            Ok(Value::Null)
        }
        "dialog.profileManager" => {
            let query = p.get("query").and_then(Value::as_str).unwrap_or("").to_string();
            app.ui.dialog = Some(Dialog::ProfileManager { query, confirm: None, rename: None });
            Ok(Value::Null)
        }
        "dialog.pasteSettings" => {
            let groups = app.session.dialogs.copy.to_ids();
            app.ui.dialog = Some(Dialog::PasteSettings { groups });
            Ok(Value::Null)
        }
        "view.focusSearch" => {
            // the search field lives in the top bar of the library and detail views alike
            app.ui.focus_search = true;
            Ok(Value::Null)
        }
        "dialog.advancedSearch" => {
            app.ui.dialog = Some(Dialog::AdvancedSearch { search: Default::default() });
            Ok(Value::Null)
        }
        "dialog.syncSettings" => {
            let groups = app.session.dialogs.copy.to_ids();
            let _ = groups;
            app.ui.dialog = Some(Dialog::SyncSettings { groups: app.session.dialogs.sync.to_ids(), controls: Vec::new() });
            Ok(Value::Null)
        }
        "dialog.syncCopies" => {
            app.ui.dialog = Some(Dialog::SyncCopies { groups: app.session.dialogs.sync.to_ids() });
            Ok(Value::Null)
        }
        "dialog.syncSnapshots" => {
            app.ui.dialog = Some(Dialog::SyncSnapshots { groups: app.session.dialogs.sync.to_ids() });
            Ok(Value::Null)
        }
        "dialog.copySettings" => {
            let groups = app.session.dialogs.copy.to_ids();
            app.ui.dialog = Some(Dialog::CopySettings { groups });
            Ok(Value::Null)
        }
        "dialog.slideshowExport" => {
            let format = match p.get("format").and_then(Value::as_str) {
                Some("jpeg") => lcc_engine::slideshow_export::Format::Jpeg,
                Some("video") => lcc_engine::slideshow_export::Format::Video,
                _ => lcc_engine::slideshow_export::Format::Pdf,
            };
            app.ui.dialog = Some(Dialog::SlideshowExport { settings: lcc_engine::slideshow_export::ExportSettings::for_format(format) });
            Ok(Value::Null)
        }
        "app.slideshowExport" => crate::slideshow_export::start(app, id, p),
        "dialog.contactSheet" => {
            app.ui.dialog = Some(Dialog::ContactSheet { options: lcc_engine::contact_sheet::Options::default() });
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
        // File ▸ Export…, ⇧E, the top bar's Export button: the Export module on the desktop, the
        // dialog on touch layouts (no room there for the module's settings column)
        "file.export" => {
            let to = if crate::modern::on(app) { "dialog.export" } else { "module.export" };
            app.run(to, json!({}))
        }
        "dialog.export" => {
            let prev = app.session.last_export.clone().unwrap_or_default();
            let u = |k: &str, d: u64| prev.get(k).and_then(Value::as_u64).unwrap_or(d);
            let dir = prev.get("dir").and_then(Value::as_str).map(str::to_string).unwrap_or_else(crate::control::default_export_dir);
            // stored settings: a depth the format lacks (saved before #1952) opens at the format's own
            let mut opts = lcc_engine::export::ExportOptions::from_json(&lcc_engine::export::ExportOptions::known_keys_only(&prev));
            // never exported: no folder picked for the person, it is asked for when the export
            // starts (Lightroom Classic's "Choose folder later"); not on the web, which has no folders
            if prev.get("destinationMode").is_none() && prev.get("dir").is_none() && !cfg!(target_arch = "wasm32") {
                opts.destination_mode = lcc_engine::export::DestinationMode::ChooseLater;
            }
            // no previous export: 2048 px long edge; a previous full-size export: full size
            let full_size = opts.resize.is_none() && lcc_engine::export::ExportOptions::has_size_param(&prev);
            let resize = opts.resize.unwrap_or_default();
            app.ui.dialog = Some(Dialog::Export { opts, full_size, resize, preset_name: String::new(), limit_kb: u("limitKb", 0) as u32, dir });
            Ok(Value::Null)
        }
        "merge.hdrLast" => crate::merge::start_last(app, "merge.hdr"),
        "merge.hdrBatch" => crate::merge::start_batch(app, p),
        "merge.panoramaLast" => crate::merge::start_last(app, "merge.panorama"),
        "merge.hdrPanoramaLast" => crate::merge::start_last(app, "merge.hdrPanorama"),
        "merge.focusStackLast" => crate::merge::start_last(app, "merge.focusStack"),
        "dialog.enhance" => crate::enhance_ui::open(app),
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
        "dialog.mergeFocusStack" => crate::merge::open(app, "merge.focusStack"),
        "app.openSamModelFolder" => crate::panels::dialogs::show_model_folder(app),
        "dialog.aiModels" => {
            app.ui.dialog = Some(Dialog::Settings { tab: "ai".into() });
            Ok(json!({"open": true}))
        }
        "app.about" => {
            app.ui.dialog = Some(Dialog::About);
            Ok(Value::Null)
        }
        // the updater is native only (the web build updates by reloading the page)
        #[cfg(target_arch = "wasm32")]
        "app.checkForUpdates" => Err("the web build updates by reloading the page".into()),
        #[cfg(not(target_arch = "wasm32"))]
        "app.checkForUpdates" => {
            // open the dialog and ask the feed: the check runs on a worker thread, so the window
            // appears at once and says "Checking…" while it waits
            app.ui.dialog = Some(Dialog::Update);
            match app.session.execute("update.check", &json!({})) {
                Ok(_) => Ok(app.session.updater.status()),
                // a check that cannot even be started shows why in the dialog
                Err(e) => Err(e.to_string()),
            }
        }
        #[cfg(target_arch = "wasm32")]
        "app.installUpdate" => Err("the web build updates by reloading the page".into()),
        #[cfg(not(target_arch = "wasm32"))]
        "app.installUpdate" => {
            // the same install the dialog's button runs, without the plan in front of it
            match app.session.execute("update.install", &json!({"confirm": true})) {
                Ok(v) => {
                    if let Some(message) = v["message"].as_str() {
                        app.toast(&ctx, message);
                    }
                    if v["exit"].as_bool().unwrap_or(false) {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    Ok(v)
                }
                // nothing downloaded yet: open the dialog, which explains what to do
                Err(e) => {
                    app.ui.dialog = Some(Dialog::Update);
                    Err(e.to_string())
                }
            }
        }
        // the web build has no updater: the list of releases
        #[cfg(target_arch = "wasm32")]
        "app.openReleasePage" => crate::links::open(app, &format!("{}/releases", lcc_brand::REPOSITORY.trim_end_matches('/'))),
        #[cfg(not(target_arch = "wasm32"))]
        "app.openReleasePage" => {
            // the offered release's own page, or the list of releases: never a URL from the caller
            let url = match app.session.updater.available() {
                Some(a) => lcc_engine::update::release_page(&a.tag),
                None => lcc_engine::update::releases_page(),
            };
            crate::links::open(app, &url)
        }
        // Android: ask for "All files access", so folders on the device are read in place
        "app.allFilesAccess" => match app.services.storage_access.as_mut() {
            None => Err("this platform reads files without asking".into()),
            Some(access) => {
                let asked = if (access.granted)() { Ok(()) } else { (access.ask)() };
                asked.map(|()| json!({"granted": (access.granted)()}))
            }
        },
        // from a menu, shortcut or the context menu (no params): ask which file to edit first, as
        // Lightroom does, unless Settings remembers the choice (#616)
        "photo.editInPhotoCraft" | "photo.editInExternal" => {
            let photocraft = id == "photo.editInPhotoCraft";
            if let Some(r) = crate::edit_in::ask_edit(app, &ctx, p, photocraft) {
                return Some(r);
            }
            edit_in(app, &ctx, p, photocraft)
        }
        "photo.openOriginalInExternal" => open_original_in(app, &ctx, p),
        "dialog.cull" => {
            let (burst_gap, similarity) = app.ui.cull_burst.unwrap_or((10.0, 0.93));
            app.ui.dialog = Some(Dialog::Cull {
                reject_below: 0.0,
                pick_best: true,
                suggest_stars: true,
                suggest_flags: false,
                burst_gap,
                similarity,
                closed_eyes: false,
                blurry_eyes: false,
                soft_eyes: false,
                everyone: false,
            });
            Ok(Value::Null)
        }
        "app.whatsNew" => {
            app.ui.dialog = Some(Dialog::WhatsNew);
            Ok(json!({"text": crate::i18n::brand_fill(crate::panels::dialogs::WHATS_NEW)}))
        }
        "app.systemInfo" => {
            let info = app.session.execute("library.info", &json!({})).unwrap_or_default();
            let gpu = (lcc_engine::gpu::ready() && lcc_engine::gpu::available()).then(lcc_engine::gpu::adapter_name).flatten();
            let mb = |b: u64| format!("{:.0} MB", b as f64 / (1u64 << 20) as f64);
            let mut rows = vec![
                ("Version".to_string(), env!("CARGO_PKG_VERSION").to_string()),
                ("System".to_string(), format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH)),
                ("CPU threads".to_string(), std::thread::available_parallelism().map(|n| n.get().to_string()).unwrap_or_else(|_| "?".into())),
                (
                    "GPU".to_string(),
                    gpu.unwrap_or_else(|| match lcc_engine::gpu::unavailable_reason() {
                        Some(why) => crate::i18n::tr_format!("none (CPU rendering): {why}", why = why),
                        None => "none (CPU rendering)".into(),
                    }),
                ),
                ("GPU rendering".to_string(), if app.ui.settings.gpu { "on".into() } else { "off".into() }),
                ("Memory budget".to_string(), mb(lcc_engine::memory::default_budget() as u64)),
                (
                    "Preview size".to_string(),
                    match app.ui.settings.preview_limit {
                        0 => "automatic".to_string(),
                        n => format!("{n} px"),
                    },
                ),
                ("Photos".to_string(), info["photos"].to_string()),
                ("Collections".to_string(), info["albums"].to_string()),
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
            if let Some(f) = lcc_engine::gpu::last_fallback() {
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
            let show = p.get("show").and_then(Value::as_str);
            crate::import::browse(app, path, p.get("subfolders").and_then(Value::as_bool), show)
        }
        "dialog.import" => {
            if crate::lightroom_import::is_running(app) {
                return Some(Err("finish Lightroom catalog import before adding photos".into()));
            }
            if let Some(path) = p.get("path").and_then(Value::as_str) {
                let subfolders = p.get("subfolders").and_then(Value::as_bool).unwrap_or(true);
                let r = crate::import::open_with_subfolders(app, vec![path.to_string()], subfolders);
                return Some(import_into_album(app, p, r));
            }
            if let Some(paths) = p.get("paths").and_then(Value::as_array) {
                let paths: Vec<String> = paths.iter().filter_map(Value::as_str).map(str::to_string).collect();
                if !paths.is_empty() {
                    let subfolders = p.get("subfolders").and_then(Value::as_bool).unwrap_or(true);
                    let r = crate::import::open_with_subfolders(app, paths, subfolders);
                    return Some(import_into_album(app, p, r));
                }
            }
            // Import as a tab (the default): Import… shows the Import tab; the review stays for
            // files and folders given outright
            if crate::import_tab::enabled(app) {
                return Some(crate::import_tab::switch(app, crate::import_tab::Tab::Import));
            }
            if !matches!(app.ui.dialog, Some(Dialog::Import { .. })) {
                app.ui.dialog = Some(Dialog::Import { opts: Box::new(crate::import::ImportDialog::empty()) });
            }
            Ok(Value::Null)
        }
        "file.addPhotos" => {
            if crate::lightroom_import::is_running(app) {
                return Some(Err("finish Lightroom catalog import before adding photos".into()));
            }
            let paths = match p.get("paths").and_then(Value::as_array) {
                Some(a) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
                None => {
                    let req = PickRequest::files(None, crate::i18n::tr("Photos"), lcc_engine::import::EXTENSIONS);
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
            let subfolders = p.get("subfolders").and_then(Value::as_bool).unwrap_or(true);
            let r = crate::import::open_with_subfolders(app, paths, subfolders);
            import_into_album(app, p, r)
        }
        "file.importLightroom" => {
            // where the catalog goes (#2672): checked before a file dialog opens
            let req = match crate::lightroom_destination::Request::parse(p) {
                Ok(req) => req,
                Err(e) => return Some(Err(e)),
            };
            let path = match p.get("path").and_then(Value::as_str).map(str::to_string) {
                Some(path) => Some(path),
                None => {
                    let req = PickRequest::file(
                        crate::i18n::tr("Import Lightroom or Capture One Catalog"),
                        crate::i18n::tr("Lightroom or Capture One Catalog"),
                        &["lrcat", "cocatalogdb"],
                    )
                    .with_filter(crate::i18n::tr("Lightroom Classic Catalog"), &["lrcat"])
                    .with_filter(crate::i18n::tr("Capture One Catalog"), &["cocatalogdb"]);
                    match crate::pick::ask(app, id, p, "path", req, |s| s.pick_lightroom_catalog.as_mut().and_then(|f| f()).map(|x| vec![x])) {
                        Picked::Now(v) => v.into_iter().next(),
                        Picked::Later | Picked::Unavailable => None,
                    }
                }
            };
            match path {
                Some(path) => crate::lightroom_destination::import(app, &path, &req),
                None => Ok(Value::Null),
            }
        }
        "app.quit" => {
            app.ui.quit = true;
            Ok(Value::Null)
        }
        "dialog.keywordShortcut" => {
            let now = app.session.keyword_shortcut.join(", ");
            crate::panels::dialogs::prompt(app, "Set Keyword Shortcut", "Separated by commas", &now, "keyword.setShortcut", json!({}), "keywords");
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
            let mut candidates = lcc_engine::cmd::missing::find_candidates(&app.session.catalog);
            if let Some(within) = within {
                candidates.retain(|candidate| lcc_catalog::query::folder_within(&candidate.path, &within));
            }
            #[cfg(test)]
            let test_delay = app.ui.find_missing_test_delay;
            let work = move || {
                #[cfg(test)]
                if let Some(d) = test_delay {
                    std::thread::sleep(d);
                }
                lcc_engine::cmd::missing::plan_find_missing(&candidates, &folder)
            };
            let done = |app: &mut LccApp, ctx: &egui::Context, plan: Result<lcc_engine::cmd::missing::FindPlan, String>| {
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
        "library.validateDng" => {
            // Library ▸ Validate DNG Files: the selection's DNGs have their raw-image digest recomputed on a worker
            // thread (cancel from the activity row); the answer is a dialog listing the damaged and unverifiable files
            const LABEL: &str = "Validate DNG Files";
            let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
            if app.tasks.is_running(LABEL) {
                return Some(Err("Validate DNG Files is already running".into()));
            }
            let ids = match app.session.targets(p) {
                Ok(ids) => ids,
                Err(e) => return Some(Err(e.to_string())),
            };
            let targets = lcc_engine::dng_validate::dng_targets(&app.session, &ids);
            if targets.is_empty() {
                return Some(Err("Validate DNG Files: no DNG files in the selection".into()));
            }
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let worker_cancel = cancel.clone();
            let total = targets.len() as u64;
            let work = move |task: lcc_engine::activity::TaskHandle| {
                task.progress(0, total);
                lcc_engine::dng_validate::validate(&targets, &worker_cancel, &|done| task.progress(done as u64, total))
            };
            let done = |app: &mut LccApp, _ctx: &egui::Context, r: lcc_engine::dng_validate::DngReport| {
                let summary = crate::i18n::tr_format!(
                    "Checked {checked} of {total} DNG files: {intact} intact, {damaged} damaged, {unverifiable} could not be verified",
                    checked = r.checked,
                    total = r.total,
                    intact = r.intact,
                    damaged = r.damaged.len(),
                    unverifiable = r.unverifiable.len()
                );
                let lines = |v: &[(String, String)]| v.iter().map(|(name, why)| format!("{name}: {why}")).collect::<Vec<_>>();
                app.ui.last_dng_validation = Some(r.to_json());
                app.ui.dialog = Some(Dialog::DngValidation { summary, damaged: lines(&r.damaged), unverifiable: lines(&r.unverifiable) });
            };
            app.ui.last_dng_validation = None;
            if let Err(e) = crate::tasks::spawn_tracked(app, LABEL, "validateDng", cancel, work, done) {
                return Some(Err(e));
            }
            if wait {
                let ctx = app.tasks.repaint.clone().unwrap_or_default();
                if !crate::tasks::wait_for(app, &ctx, LABEL, std::time::Duration::from_secs(600)) {
                    return Some(Err("Validate DNG Files: still running (it continues in the background)".into()));
                }
                return match app.ui.last_dng_validation.clone() {
                    Some(v) => Some(Ok(v)),
                    None => Some(Err("Validate DNG Files: worker terminated unexpectedly".into())),
                };
            }
            Ok(json!({"background": true, "total": total}))
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
                    let req = PickRequest::file(crate::i18n::tr("Locate Missing File…"), crate::i18n::tr("Photos"), lcc_engine::import::EXTENSIONS);
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
                None => match lcc_engine::devices::devices_now().into_iter().next() {
                    Some(d) => d.path,
                    None => return Some(Err("no camera or memory card found".into())),
                },
            };
            // Import as a tab: the card's photos are browsed there, for culling before the import
            if crate::import_tab::enabled(app) && p.get("path").is_none() && p.get("album").is_none() {
                let dcim = std::path::Path::new(&path).join("DCIM");
                let at = if dcim.is_dir() { dcim.to_string_lossy().to_string() } else { path };
                return Some(
                    crate::import_tab::switch(app, crate::import_tab::Tab::Import).and_then(|_| crate::import::browse(app, &at, None, None)),
                );
            }
            let r = crate::import::open(app, vec![path]);
            // only the scan just started (an error means another scan is running)
            if r.is_ok()
                && let Some(t) = &mut app.scan
            {
                t.copy = true;
            }
            import_into_album(app, p, r)
        }
        "file.addCameraRoll" => crate::camera_roll::start(app),
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
                // a folder picked here (not given by a script): the Import tab browses it
                Some(path) if crate::import_tab::enabled(app) && p.get("path").is_none() && p.get("album").is_none() => {
                    crate::import_tab::switch(app, crate::import_tab::Tab::Import).and_then(|_| crate::import::browse(app, &path, None, None))
                }
                Some(path) => {
                    let subfolders = p.get("subfolders").and_then(Value::as_bool).unwrap_or(true);
                    let r = crate::import::open_with_subfolders(app, vec![path], subfolders);
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
                    l.ends_with(".cube")
                        || l.ends_with(".zip")
                        || std::path::Path::new(p.as_str()).is_dir()
                        || lcc_engine::xmp_look::is_look_file(std::path::Path::new(p.as_str()))
                })
                .collect();
            // .dcp camera profiles (and those inside zips / folders): active for their camera from the next start
            let dcp_paths: Vec<&String> = paths
                .iter()
                .filter(|p| {
                    let l = p.to_ascii_lowercase();
                    l.ends_with(".dcp") || l.ends_with(".zip") || std::path::Path::new(p.as_str()).is_dir()
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
            if !dcp_paths.is_empty() {
                match app.session.execute("camera.importDcp", &json!({"paths": dcp_paths})) {
                    Ok(v) => {
                        let n = v["imported"].as_array().map_or(0, Vec::len);
                        if n > 0 {
                            parts.push(format!(
                                "Imported {n} camera profile{} (.dcp): pick one per photo in Profiles ▸ Camera Profile",
                                if n == 1 { "" } else { "s" }
                            ));
                        }
                        if v["restart"].as_bool() == Some(true) {
                            parts.push(crate::i18n::tr("A new camera default applies after a restart").to_string());
                        }
                        problems.extend(v["failed"].as_array().into_iter().flatten().map(|f| {
                            let name = f.get(0).and_then(Value::as_str).unwrap_or_default();
                            let reason = f.get(1).and_then(Value::as_str).unwrap_or_default();
                            format!("{name}: {reason}")
                        }));
                    }
                    Err(e) => problems.push(e.to_string()),
                }
            }
            let preset_paths: Vec<&String> = paths
                .iter()
                .filter(|p| {
                    let l = p.to_ascii_lowercase();
                    !l.ends_with(".cube") && !l.ends_with(".dcp") && !lcc_engine::xmp_look::is_look_file(std::path::Path::new(p.as_str()))
                })
                .collect();
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
                // Lightroom Classic metadata / export presets, XMP metadata templates, ours
                let added = |k: &str| v[k].as_array().map_or(0, |a| a.iter().filter(|r| r["skipped"] != true).count());
                let (metas, exports) = (added("metadataPresets"), added("exportPresets"));
                // what they couldn't carry over, named
                let not_carried = |k: &str, max: usize| {
                    let mut names: Vec<&str> = v[k]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .flat_map(|r| r["unmapped"].as_array().into_iter().flatten())
                        .filter_map(Value::as_str)
                        .collect();
                    names.sort_unstable();
                    names.dedup();
                    if names.is_empty() {
                        return String::new();
                    }
                    let shown: Vec<&str> = names.iter().take(max).copied().collect();
                    crate::i18n::tr_format!(" (not carried over: {}{})", shown.join(", "), if names.len() > max { "…" } else { "" })
                };
                if metas > 0 {
                    let msg = crate::i18n::tr_format!("Imported {n} metadata preset{}", if metas == 1 { "" } else { "s" }, n = metas);
                    parts.push(msg + &not_carried("metadataPresets", 3));
                }
                if exports > 0 {
                    let msg = crate::i18n::tr_format!("Imported {n} export preset{}", if exports == 1 { "" } else { "s" }, n = exports);
                    parts.push(msg + &not_carried("exportPresets", 10));
                    if let Some(first) =
                        v["exportPresets"].as_array().and_then(|a| a.iter().find(|r| r["skipped"] != true)).and_then(|r| r["name"].as_str())
                    {
                        crate::panels::dialogs::apply_export_preset_to_open_dialog(app, &ctx, first);
                    }
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
                        msg += &crate::i18n::tr_format!(" (not carried over: {}{})", names.join(", "), if skipped.len() > 3 { "…" } else { "" });
                    }
                    parts.push(msg);
                }
                // Show the new presets where they live, in Develop's left column, unfolded (issue
                // #469). Outside Develop the screen is left as it is: the panel is unfolded where
                // it lives for Develop's next visit, and the toast says what came in (issue #875).
                if n > 0 && app.ui.module == crate::module::ModuleId::Develop {
                    let _ = crate::panels::presets::reveal(app);
                } else if n > 0 && !crate::panels::presets::shown(app) {
                    crate::panels::presets::set_shown(app, true);
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
                        ". Capture One won't import a list with ; , < or > in a keyword: {names}",
                        names = refuses.iter().take(5).copied().collect::<Vec<_>>().join(" · ")
                    ));
                }
                // what the format can't hold (a name in brackets, a line break…) isn't in the file
                let left: Vec<&str> = v["unwritable"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                if !left.is_empty() {
                    msg.push_str(&crate::i18n::tr_format!(
                        ". Left out, as a keyword list can't hold them: {names}",
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
        "file.exportMetadataPresets" | "file.exportExportPresets" => {
            let meta = id == "file.exportMetadataPresets";
            let (ext, command) = if meta {
                (lcc_engine::cmd::metadata_files::META_EXT, "metadata.exportPresets")
            } else {
                (lcc_engine::cmd::export_preset_files::EXPORT_PRESET_EXT, "export.exportPresets")
            };
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => Some(x.to_string()),
                None => {
                    let name = format!("{}.{ext}", if meta { "Metadata Presets" } else { "Export Presets" });
                    let req = if meta {
                        PickRequest::save(
                            crate::i18n::tr("Export Metadata Presets…"),
                            crate::i18n::tr("{app} Metadata Presets"),
                            &["lcmeta"],
                            name.clone(),
                        )
                    } else {
                        PickRequest::save(
                            crate::i18n::tr("Export User Presets…"),
                            crate::i18n::tr("{app} Export Presets"),
                            &["lcexport"],
                            name.clone(),
                        )
                    };
                    match crate::pick::ask(app, id, p, "path", req, |s| s.save_preset_file.as_mut().map(|f| f(&name).into_iter().collect())) {
                        Picked::Now(v) => v.into_iter().next(),
                        Picked::Later => return Some(Ok(Value::Null)),
                        Picked::Unavailable => return Some(Err("no file dialog on this platform".into())),
                    }
                }
            };
            let Some(path) = path else { return Some(Ok(Value::Null)) };
            let mut params = json!({"path": path});
            if let Some(v) = p.get("names") {
                params["names"] = v.clone();
            }
            let r = app.session.execute(command, &params).map_err(|e| e.to_string());
            match &r {
                Ok(v) => {
                    let n = v["count"].as_u64().unwrap_or(0);
                    let s = if n == 1 { "" } else { "s" };
                    let msg = if meta {
                        crate::i18n::tr_format!("Exported {n} metadata preset{}", s, n = n)
                    } else {
                        crate::i18n::tr_format!("Exported {n} export preset{}", s, n = n)
                    };
                    app.toast(&ctx, msg);
                }
                Err(e) => app.toast_error(&ctx, e.clone()),
            }
            return Some(r);
        }
        "file.exportMaskEffects" => {
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => Some(x.to_string()),
                None => {
                    let name = format!("{}.lceffect", p.get("group").and_then(Value::as_str).unwrap_or("Effect Presets"));
                    let req = PickRequest::save(
                        crate::i18n::tr("Export Effect Presets"),
                        crate::i18n::tr("{app} Effect Presets"),
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
        "file.exportCameraProfile" | "file.exportLookLut" => {
            let dcp = id == "file.exportCameraProfile";
            let Some(active) = app.session.active() else { return Some(Err("no photo".into())) };
            let stem = app
                .session
                .catalog
                .photo(active)
                .and_then(|ph| std::path::Path::new(&ph.file_name).file_stem().map(|s| s.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "photo".into());
            let (ext, command): (&'static str, &str) = if dcp { ("dcp", "camera.exportDcp") } else { ("cube", "develop.exportLut") };
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => Some(x.to_string()),
                None => {
                    let name = format!("{stem}.{ext}");
                    let req = if dcp {
                        PickRequest::save(
                            crate::i18n::tr("Export Camera Profile (.dcp)…"),
                            crate::i18n::tr("DNG Camera Profile"),
                            &["dcp"],
                            name.clone(),
                        )
                    } else {
                        PickRequest::save(crate::i18n::tr("Export Look as LUT (.cube)…"), crate::i18n::tr("Cube LUT"), &["cube"], name.clone())
                    };
                    match crate::pick::ask(app, id, p, "path", req, |s| s.save_preset_file.as_mut().map(|f| f(&name).into_iter().collect())) {
                        Picked::Now(v) => v.into_iter().next(),
                        Picked::Later => return Some(Ok(Value::Null)),
                        Picked::Unavailable => return Some(Err("no file dialog on this platform".into())),
                    }
                }
            };
            let Some(path) = path else { return Some(Ok(Value::Null)) };
            let r = app.session.execute(command, &json!({"path": path, "ids": [active.0]})).map_err(|e| e.to_string());
            match &r {
                Ok(v) if dcp => {
                    app.toast(&ctx, crate::i18n::tr_format!("Exported the camera profile for {}", v["model"].as_str().unwrap_or_default()))
                }
                Ok(_) => app.toast(&ctx, crate::i18n::tr("Exported the look as a LUT")),
                Err(e) => app.toast_error(&ctx, e.clone()),
            }
            return Some(r);
        }
        "file.exportPresets" => {
            let group = p.get("group").and_then(Value::as_str).map(str::to_string);
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => Some(x.to_string()),
                None => {
                    let default_group = format!("{} Presets", lcc_brand::DISPLAY_NAME);
                    let name = format!("{}.{}", group.as_deref().unwrap_or(&default_group), lcc_brand::PRESET_EXT);
                    let req =
                        PickRequest::save(crate::i18n::tr("Export Presets"), crate::i18n::tr("{app} Preset"), &[lcc_brand::PRESET_EXT], name.clone());
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
        "photo.renderToDng" => return crate::render_dng_task::start(app, p),
        "photo.updateDngPreview" => return crate::dng_update_task::start(app, p),
        "app.showInFinder" => show_in_finder(app),
        "app.showInFolders" => show_in_folders(app, p.get("id").and_then(Value::as_u64).map(lcc_catalog::PhotoId)),
        "app.openLogFolder" => open_log_folder(app),
        "app.website" | "app.github" | "app.help" | "app.feedback" | "app.discord" => {
            let url = crate::links::url_of(id).unwrap_or(crate::links::APP_PAGE);
            crate::links::open(app, url)
        }
        "app.exportPrevious" => match app.session.last_export.clone() {
            Some(prev) => crate::control::export_active(app, &lcc_engine::export::ExportOptions::known_keys_only(&prev)),
            None => Err("nothing exported yet; use Export…".into()),
        },
        _ => return None,
    };
    Some(r)
}

/// The library folder the grid shows (chosen in the Folders panel), for the Library menu's folder items.
fn chosen_library_folder(app: &LccApp) -> Result<String, String> {
    match (&app.session.source, &app.session.library_folder) {
        (lcc_engine::LibrarySource::LibraryFolder, Some(p)) if !p.trim().is_empty() => Ok(p.clone()),
        _ => Err("Choose a folder in the Folders panel first".into()),
    }
}

/// Views whose bottom bar has the thumbnail-size slider and that draw tiles of that size.
fn thumb_size_view(app: &LccApp) -> bool {
    matches!(app.ui.view, ViewMode::PhotoGrid | ViewMode::SquareGrid | ViewMode::People)
}

pub fn ui_enabled(app: &LccApp, id: &str) -> bool {
    if crate::video_ui::blocked(app, id) {
        return false;
    }
    if let Some(e) = fork::enabled(app, id) {
        return e;
    }
    match id {
        "library.selectOnlyActive" => app.session.active().is_some(),
        "view.loupeToggle" => app.session.active().is_some() || app.ui.view == ViewMode::Detail,
        "dialog.newDiskFolder" | "dialog.synchronizeFolder" => chosen_library_folder(app).is_ok(),
        "view.thumbSizeUp" | "view.thumbSizeDown" => thumb_size_view(app),
        "dialog.contactSheet" | "app.contactSheet" | "dialog.print" | "app.print" => {
            !cfg!(target_arch = "wasm32") && app.session.active().is_some() && app.export.is_none()
        }
        "dialog.slideshowExport" | "app.slideshowExport" => {
            !cfg!(target_arch = "wasm32") && !app.session.slideshow_export.as_ref().is_some_and(|r| r.running())
        }
        s if s.starts_with("panel.")
            || s.starts_with("tool.")
            || s == "remove.useGenerative"
            || s == "remove.detectObjects"
            || s.starts_with("section.") =>
        {
            app.session.active().is_some() || s == "panel.close"
        }
        "app.export" | "dialog.export" | "file.export" | "dialog.createPreset" | "dialog.rename" | "dialog.captureTime" | "dialog.copySettings" => {
            app.session.active().is_some()
        }
        "photo.tagFromTracklog" => app.session.active().is_some() && app.services.pick_tracklog.is_some(),
        // nothing downloaded, nothing to install (the dialog's own button says the same)
        #[cfg(target_arch = "wasm32")]
        "app.installUpdate" => false,
        #[cfg(not(target_arch = "wasm32"))]
        "app.installUpdate" => app.session.updater.staged().is_some(),
        "photo.editInPhotoCraft" | "photo.editInExternal" => app.session.active().is_some() && app.services.open_with.is_some(),
        "app.exportPrevious" => app.session.active().is_some() && app.session.last_export.is_some(),
        "dialog.pasteSettings" => app.session.active().is_some() && app.session.clipboard.is_some(),
        "dialog.updatePreset" => app.session.active().is_some(),
        "mask.startComponent" => app.session.active().is_some() && app.session.active_mask.is_some(),
        "dialog.syncSettings" => app.session.active().is_some() && app.session.selection.ids.len() > 1,
        "dialog.syncCopies" => app.session.active().is_some_and(|id| {
            let master = app.session.catalog.photo(id).map(|p| p.copy_of.unwrap_or(p.id));
            master.is_some_and(|m| app.session.catalog.photos().any(|p| !p.deleted && p.id != id && (p.id == m || p.copy_of == Some(m))))
        }),
        "dialog.syncSnapshots" => app.session.active().and_then(|id| app.session.catalog.photo(id)).is_some_and(|p| !p.versions.is_empty()),
        "photo.openOriginalInExternal" => {
            // only a file type the app reads can be handed over (#2135); the extension alone
            // decides, so no disk or share is touched while a menu is drawn
            app.services.open_with.is_some()
                && app.session.active().and_then(|id| app.session.catalog.photo(id)).is_some_and(|p| match &p.source {
                    lcc_engine::catalog::Source::File { path } => lcc_engine::open_path::is_media(std::path::Path::new(path)),
                    _ => false,
                })
        }
        "app.showInFinder" => {
            app.services.reveal.is_some()
                && app
                    .session
                    .active()
                    .and_then(|id| app.session.catalog.photo(id))
                    .is_some_and(|p| matches!(p.source, lcc_engine::catalog::Source::File { .. }))
        }
        "app.showInFolders" => show_in_folders_enabled(app, None),
        "app.openSamModelFolder" => app.session.segmenter.dir.is_some() && app.services.open_with.is_some(),
        "dialog.aiModels" => lcc_engine::segment::Segmenter::AVAILABLE,
        "surface.maskPrevious" | "surface.maskNext" => {
            app.session.active().and_then(|id| app.session.develop_of(id)).is_some_and(|d| !d.masks.is_empty())
        }
        "surface.dialogConfirm" | "surface.dialogCancel" => app.ui.dialog.is_some(),
        "file.exportPresets" => app.session.presets.iter().any(|p| !p.builtin),
        // a raw on disk has a camera colour to export; any photo has a look
        "file.exportCameraProfile" => app
            .session
            .active()
            .and_then(|id| app.session.catalog.photo(id))
            .is_some_and(|p| p.develops_raw() && matches!(p.source, lcc_engine::catalog::Source::File { .. })),
        "file.exportLookLut" => app.session.active().is_some(),
        "file.exportMaskEffects" => !app.session.mask_effects.is_empty(),
        "file.exportMetadataPresets" => !app.session.metadata_presets.is_empty(),
        "file.exportExportPresets" => !app.session.export_presets.is_empty(),
        "dialog.saveMaskEffect" => app.session.active().is_some() && app.session.active_mask.is_some(),
        "file.exportCurvePresets" => !app.session.curve_presets.is_empty(),
        "view.compare" => app.session.catalog.len() > 1,
        "view.fullScreenPreview" | "view.infoOverlay" | "view.navigator" => app.session.active().is_some() || app.ui.fullscreen,
        "app.openLibrary" | "app.newLibrary" => app.services.pick_folder.is_some() && crate::panels::settings::can_switch_library(app).is_ok(),
        "app.renameLibrary" => app.session.library.as_ref().is_some_and(|l| l.on_disk) && crate::panels::settings::can_switch_library(app).is_ok(),
        "dialog.import" => !crate::lightroom_import::is_running(app),
        "file.addFolder" => (app.services.pick_folder.is_some() || app.services.picker.is_some()) && !crate::lightroom_import::is_running(app),
        "file.backupLibrary" => app.services.backup_library.is_some(),
        "app.openLogFolder" => app.services.reveal.is_some() && app.services.log_file.is_some(),
        "app.openReleasePage" => app.services.open_url.is_some(),
        "app.allFilesAccess" => app.services.storage_access.is_some(),
        "file.addCameraRoll" => app.services.camera_roll.is_some(),
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
pub fn menu_entries(app: &LccApp) -> Vec<MenuEntry> {
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

/// The Delete key in an album: a normal album loses the selected photos (`album.removePhotos`),
/// which stay in the library and their folder; a smart album keeps them (its rules decide) and
/// says so. True when the key was handled here, false outside albums (then it deletes the photos
/// from the library). Photo ▸ Delete Photo still deletes from the library everywhere.
pub fn delete_in_album(app: &mut LccApp, ctx: &egui::Context) -> bool {
    let lcc_engine::LibrarySource::Album(aid) = app.session.source else { return false };
    let Some(album) = app.session.catalog.album(aid) else { return false };
    if album.is_smart() {
        app.toast(ctx, crate::i18n::tr("Smart collections update automatically: change their rules, or use Photo ▸ Delete Photo"));
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
/// Photo ▸ Delete Rejected Photos…: ask how (remove, or remove and trash the files), with the
/// number of rejects the view has; with none, say so.
pub fn ask_delete_rejected(app: &mut LccApp) -> Result<Value, String> {
    let view = app.session.visible_cloned();
    let count = view.iter().filter(|id| app.session.catalog.photo(**id).is_some_and(|p| p.flag == lcc_catalog::Flag::Reject)).count();
    if count == 0 {
        if let Some(ctx) = app.tasks.repaint.clone() {
            app.toast(&ctx, crate::i18n::tr("No rejected photos here"));
        }
        return Ok(json!({"deleted": 0}));
    }
    app.ui.dialog = Some(Dialog::DeleteRejected { count, recycle: false });
    Ok(json!({"asking": count}))
}

pub fn confirm_delete(app: &mut LccApp) -> bool {
    app.ui.settings.confirm_delete && open_delete_dialog(app)
}

/// Photo ▸ Delete from Disk: always asks (whatever the Confirm setting says), in the Delete
/// dialog, whose Delete from Disk button runs `photo.deleteFromDisk` with `confirm: true`.
pub fn confirm_delete_from_disk(app: &mut LccApp) -> bool {
    open_delete_dialog(app)
}

/// The Delete dialog for the selection; false when nothing is selected.
fn open_delete_dialog(app: &mut LccApp) -> bool {
    let targets = app.session.selected();
    let count = targets.len();
    if count == 0 {
        return false;
    }
    let (_, disk_files) = lcc_engine::disk_delete::preview(&app.session.catalog, &targets);
    app.ui.dialog = Some(Dialog::ConfirmDelete { count, disk_files });
    true
}

/// Delete Permanently (`all`: Empty Recently Deleted) with "Confirm before deleting" on: open
/// the confirmation dialog instead; true when it did (its OK runs the command).
pub fn confirm_purge(app: &mut LccApp, all: bool) -> bool {
    if !app.ui.settings.confirm_delete {
        return false;
    }
    let count = if all { app.session.catalog.photos().filter(|p| p.deleted).count() } else { app.session.selected().len() };
    if count == 0 {
        return false;
    }
    app.ui.dialog = Some(Dialog::ConfirmPurge { count, all });
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
fn open_log_folder(app: &mut LccApp) -> Result<Value, String> {
    let path = app.services.log_file.clone().ok_or("this session keeps no log file")?;
    let reveal = app.services.reveal.as_mut().ok_or("not available here")?;
    reveal(&path)?;
    Ok(json!({"path": path}))
}

/// Reveal the active photo's original in the system file manager.
fn show_in_finder(app: &mut LccApp) -> Result<Value, String> {
    let id = app.session.active().ok_or("no photo selected")?;
    let path = match app.session.catalog.photo(id).map(|p| p.source.clone()) {
        Some(lcc_engine::catalog::Source::File { path }) => path,
        _ => return Err("this photo has no file (demo scene)".into()),
    };
    let reveal = app.services.reveal.as_mut().ok_or("not available here")?;
    reveal(&path)?;
    Ok(json!({"path": path}))
}

/// The folder the active photo was imported from, as the sidebar's Folders section counts it (a
/// photo straight in a disk's root is in the root). `None` for a demo scene, a photo only browsed
/// in Local or in Recently Deleted, and one on the startup disk's root, which has no row to show.
fn photo_folder(app: &LccApp, id: Option<lcc_catalog::PhotoId>) -> Option<String> {
    let photo = id.or_else(|| app.session.active()).and_then(|id| app.session.catalog.photo(id)).filter(|p| p.in_library())?;
    let lcc_engine::catalog::Source::File { path } = &photo.source else { return None };
    let dir = path.rfind(['/', '\\']).and_then(|i| path.get(..i.max(1)))?;
    (!lcc_catalog::folders::is_startup_disk(dir)).then(|| dir.to_string())
}

/// Whether Show in Folders has a row to show for photo `id` (the active one when `None`): its
/// folder must be a row of the tree, which stops `MAX_DEPTH` levels down. With no fresh tree to
/// look in, the folder alone decides, and the command still checks.
pub fn show_in_folders_enabled(app: &LccApp, id: Option<lcc_catalog::PhotoId>) -> bool {
    let Some(dir) = photo_folder(app, id) else { return false };
    app.caches.folder_tree_if_fresh(&app.session.catalog).is_none_or(|tree| crate::panels::left::folder_row_of(&tree, &dir).is_some())
}

/// Photo ▸ Show in Folders (Lightroom's Go to Folder in Library): show the folder of photo `id`
/// (the active one when `None`; the grid's menu passes the photo right-clicked) as a click on its
/// row in the sidebar's Folders section does, and bring the row into view: the sidebar, the
/// section and the folders above it open, and it scrolls there (`reveal_folder`).
fn show_in_folders(app: &mut LccApp, id: Option<lcc_catalog::PhotoId>) -> Result<Value, String> {
    let dir = photo_folder(app, id).ok_or("this photo is not in a folder of the catalog")?;
    let tree = app.caches.folder_tree(&app.session.catalog);
    // the row's own spelling (forward slashes): what its click hands `library.source`, and its id
    let path = crate::panels::left::folder_row_of(&tree, &dir).map(|n| n.path.clone()).ok_or("this photo's folder is not in Folders")?;
    app.run("library.source", json!({"kind": "libraryFolder", "path": path}))?;
    app.ui.left_panel = true;
    app.ui.reveal_folder = Some(path.clone());
    Ok(json!({"path": path}))
}

/// Photo ▸ Edit In: render a copy of each selected photo with its edits (stacked on its original)
/// as Settings ▸ External Editing asks for that editor, then open them there. `p` may override the
/// file (`format`, `colorSpace`, `bitDepth`) and the editor (`app`). PhotoCraft is checked first,
/// so a missing editor leaves no copy behind. Once the copies exist, an editor that won't open them
/// is reported in the result's `failed` (and a toast), not as an error.
pub(crate) fn edit_in(app: &mut LccApp, ctx: &egui::Context, p: &Value, photocraft: bool) -> Result<Value, String> {
    // every way in (menu, shortcut, context menu, the control channel) says why nothing opened
    edit_in_inner(app, ctx, p, photocraft).inspect_err(|e| app.toast(ctx, crate::i18n::tr_format!("Couldn't open the editor: {e}", e = e)))
}

fn edit_in_inner(app: &mut LccApp, ctx: &egui::Context, p: &Value, photocraft: bool) -> Result<Value, String> {
    let settings = &app.ui.settings;
    let (file, configured) = if photocraft {
        let path = settings.photocraft_path.trim();
        (settings.photocraft_file, if path.is_empty() { crate::state::PHOTOCRAFT.to_string() } else { path.to_string() })
    } else {
        (settings.external_file, settings.external_editor.clone())
    };
    let editor = p.get("app").and_then(Value::as_str).map_or(configured, str::to_string);
    if let Some(check) = app.services.check_editor.as_ref() {
        check(&editor)?;
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
            crate::edit_in::watch(app, std::path::Path::new(path), &[*id]);
        }
        // the copy is the app's own render, but it goes through the same gate as any other
        // path handed to the system opener (#2135)
        match lcc_engine::open_path::openable(path, "") {
            Err(e) => failed = Some(e),
            Ok(absolute) => {
                if let Some(Err(e)) = app.services.open_with.as_mut().map(|f| f(&absolute, &editor)) {
                    failed = Some(e);
                }
            }
        }
    }
    let mut r = r;
    if let Some(o) = r.as_object_mut() {
        o.insert("failed".into(), json!(failed));
    }
    if let Some(e) = failed {
        // the copies are in the library already (one undo step): an error would invite a retry
        // that makes another set, so the result says what failed and the toast says why
        app.toast(ctx, crate::i18n::tr_format!("Couldn't open the editor: {e}", e = e));
        return Ok(r);
    }
    match copies.as_slice() {
        [(path, _)] => {
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
fn open_original_in(app: &mut LccApp, ctx: &egui::Context, p: &Value) -> Result<Value, String> {
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

fn open_originals(app: &mut LccApp, p: &Value) -> Result<Value, String> {
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
        // the path may come from an imported catalog: only a photo file the app reads is
        // handed to the opener, never a program (#2135)
        match app.session.catalog.photo(id).map(|ph| (ph.file_name.clone(), ph.source.clone())) {
            Some((name, lcc_engine::catalog::Source::File { path })) => match lcc_engine::open_path::openable(&path, &name) {
                Ok(absolute) => files.push((id, absolute)),
                Err(reason) => skipped.push(json!({"id": id.0, "reason": reason})),
            },
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
