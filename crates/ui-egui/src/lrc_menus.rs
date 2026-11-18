//! The menu bar as Lightroom Classic lays it out (fork-owned): File, Edit, Window and Help in every
//! module, and between them the module's own menus (Library: Library · Photo · Metadata · View;
//! Develop: Develop · Photo · Settings · Tools · View; and so on), with Classic's item names and
//! order. The job file is the **Catalog** (File's first group), so "Library" only ever names the
//! module and its menu.
//!
//! [`crate::menubar::menu_bar`] reads these tables. A slot is one of:
//! - `---`: a separator;
//! - `@Label` or `@Label=Path`: a submenu shown as `Label` that gathers the registry's items whose
//!   menu path is `<menu>>Path` (`Other>Path` for another menu's), plus the generated rows of
//!   [`crate::menubar::expanded`] for `Path`; [`sub_layout`] orders it;
//! - `id`, `id=Label`, `id{json}=Label`: a command (wherever its own menu path points), shown under
//!   Classic's name, run with the JSON parameters when given;
//! - `soon.*`: a Classic item the app doesn't have yet ([`COMING_SOON`]): drawn greyed out
//!   with "Coming soon" on hover, never run. `docs/lrc-menu-gaps.md` lists them (a test keeps it
//!   in step).
//!
//! Own layout, from observing Lightroom Classic's menus (names and order only; nothing copied).

use crate::module::ModuleId;

/// The menus between Edit and Window, per module (Export, the app's own module, shows Library's).
pub fn titles(module: ModuleId) -> &'static [&'static str] {
    match module {
        ModuleId::Library | ModuleId::Export => &["File", "Edit", "Library", "Photo", "Metadata", "View", "Window", "Help"],
        ModuleId::Develop => &["File", "Edit", "Develop", "Photo", "Settings", "Tools", "View", "Window", "Help"],
        ModuleId::Map => &["File", "Edit", "Map", "Photo", "View", "Window", "Help"],
        ModuleId::Book => &["File", "Edit", "Book", "View", "Window", "Help"],
        ModuleId::Slideshow => &["File", "Edit", "Slideshow", "Play", "View", "Window", "Help"],
        ModuleId::Print => &["File", "Edit", "Print", "View", "Window", "Help"],
        ModuleId::Web => &["File", "Edit", "Web", "View", "Window", "Help"],
    }
}

/// Whether a menu lists only its slots. The shared menus (File, Edit, Window, Help) and Library's
/// own also take every other command whose menu path names them (the app's own items, after
/// Classic's); a module's own Photo / View / Settings… show Classic's rows only.
pub fn strict(module: ModuleId, title: &str) -> bool {
    !matches!(module, ModuleId::Library | ModuleId::Export) && !matches!(title, "File" | "Edit" | "Window" | "Help" | "Slideshow" | "Web")
}

/// A menu's slots in `module`.
pub fn layout(module: ModuleId, title: &str) -> &'static [&'static str] {
    use ModuleId::*;
    match (module, title) {
        (_, "File") => FILE,
        (_, "Edit") => EDIT,
        (_, "Window") => WINDOW,
        (_, "Help") => HELP,
        (Library | Export, "Library") => LIBRARY,
        (Library | Export, "Photo") => PHOTO,
        (Library | Export, "Metadata") => METADATA,
        (Library | Export, "View") => VIEW,
        (Develop, "Develop") => DEVELOP,
        (Develop, "Photo") => DEVELOP_PHOTO,
        (Develop, "Settings") => SETTINGS,
        (Develop, "Tools") => TOOLS,
        (Develop, "View") => DEVELOP_VIEW,
        (Map, "Map") => MAP,
        (Map, "Photo") => MAP_PHOTO,
        (Map, "View") => MAP_VIEW,
        (Book, "Book") => BOOK,
        (Book, "View") => BOOK_VIEW,
        (Slideshow, "Slideshow") => SLIDESHOW,
        (Slideshow, "Play") => PLAY,
        (Slideshow, "View") => SLIDESHOW_VIEW,
        (Print, "Print") => PRINT,
        (Print, "View") => PRINT_VIEW,
        (Web, "Web") => WEB,
        (Web, "View") => WEB_VIEW,
        _ => &[],
    }
}

/// Order inside a Classic submenu, by (menu, submenu label as shown). Submenus not listed here keep
/// `menubar`'s own order for their path.
pub fn sub_layout(title: &str, label: &str) -> Option<&'static [&'static str]> {
    SUB.iter().find(|(t, l, _)| *t == title && *l == label).map(|(_, _, s)| *s)
}

const FILE: &[&str] = &[
    "app.newLibrary=New Catalog…",
    "app.openLibrary=Open Catalog…",
    "@Open Recent=Libraries",
    "library.compact=Optimize Catalog",
    "app.renameLibrary=Rename Catalog…",
    // The app's own catalog care, beside Classic's
    "catalog.backup=Back Up Catalog",
    "file.backupLibrary=Back Up Catalog…",
    "file.restoreLibrary=Restore Catalog from Backup…",
    "catalog.checkIntegrity=Test Catalog Integrity",
    "---",
    "dialog.import=Import Photos and Video…",
    "dialog.importCatalog=Import from Another Catalog…",
    "file.importLightroom",
    "@Tethered Capture",
    "@Auto Import",
    "file.addFolder",
    "@Import from Device",
    "@More Imports",
    "---",
    "file.importPresets=Import Develop Profiles and Presets…",
    "---",
    "file.export",
    "app.exportPrevious",
    "@Export with Preset",
    "dialog.exportCatalog=Export as Catalog…",
    "@More Exports",
    "---",
    "dialog.emailPhoto",
    "---",
    "plugins.manager",
    "@Plug-in Extras=Plug-ins",
    "---",
    "library.showQuickCollection=Show Quick Collection",
    "album.saveQuick=Save Quick Collection…",
    "album.clearQuick=Clear Quick Collection",
    r#"album.setTarget{"id":null}=Set Quick Collection as Target"#,
    "---",
    "@Library Filters",
    "---",
    "dialog.print=Printer…",
    "dialog.pageSetup=Page Setup…",
    "---",
    "app.quit",
];

const EDIT: &[&str] = &[
    "edit.undo",
    "edit.redo",
    "---",
    "library.selectAll=Select All",
    "library.selectNone=Select None",
    "library.invertSelection",
    "---",
    "library.selectOnlyActive=Select Only Active Photo",
    "library.deselectActive",
    r#"library.selectBy{"flag":"pick"}=Select Flagged Photos"#,
    "library.deselectUnflagged",
    "---",
    "@Select by Flag",
    "@Select by Rating",
    "@Select by Color Label",
    "---",
    "app.settings=Preferences…",
    "dialog.catalogSettings=Catalog Settings…",
    "dialog.identityPlateSetup",
    "dialog.watermark",
    "---",
    "@Keymap",
    "@Language",
];

const LIBRARY: &[&str] = &[
    "dialog.newAlbum",
    "dialog.smartAlbum",
    "dialog.newFolder=New Collection Set…",
    "dialog.newDiskFolder=New Folder…",
    "dialog.newSmartAlbum",
    "---",
    "view.focusSearch",
    "dialog.advancedSearch",
    "library.findSimilar",
    "---",
    "dialog.cull",
    "---",
    "library.toggleFilters=Enable Filters",
    "@Filter by Preset",
    "@Filter by Flag",
    "@Filter by Rating",
    "@Filter by Color Label",
    "@Filter by Kind",
    "@Filter by Metadata",
    "@Filters",
    "---",
    "library.lockFilters",
    "library.rememberFilters",
    "library.showSubfolderPhotos",
    "---",
    "library.refine",
    "dialog.reviewComments",
    "---",
    "dialog.rename",
    "photo.convertToDng=Convert Photo to DNG…",
    "---",
    "photo.flatField=Flat-Field Correction…",
    "---",
    "file.findMissing=Find All Missing Photos",
    "library.findDuplicates=Find Duplicates…",
    "library.validateDng",
    "dialog.synchronizeFolder=Synchronize Folder…",
    "---",
    "faces.detect=Find Faces Again…",
    "faces.clearAndRescan",
    "---",
    "@Previews",
    "---",
    "library.findPreviousProcess",
    "---",
    "library.previous=Previous Selected Photo",
    "library.next=Next Selected Photo",
    "---",
    "@Immich",
];

const PHOTO: &[&str] = &[
    "album.toggleTarget=Add to Quick Collection",
    "@Add to Collection",
    "---",
    "view.detail=Zoom In on Loupe",
    "app.showInFinder",
    "app.showInFolders=Go to Folder in Library",
    "second.locked=Lock to Second Monitor",
    "---",
    "view.reference=Open in Reference View",
    "---",
    "@Edit In",
    "@Photo Merge",
    "dialog.enhance",
    "---",
    "@Stacking=Stack",
    "@People",
    "photo.virtualCopy",
    "photo.setCopyAsOriginal",
    "---",
    "photo.rotateLeft=Rotate Left (CCW)",
    "photo.rotateRight=Rotate Right (CW)",
    "photo.flipHorizontal",
    "photo.flipVertical",
    "---",
    "@Set Flag",
    "@Set Rating",
    "@Set Color Label",
    "photo.resetExportStatus",
    "view.autoAdvance",
    "---",
    "@Set Keyword",
    "panel.keywords=Add Keywords…",
    "---",
    "@Develop Settings",
    "---",
    "photo.delete=Remove Photo…",
    "album.removePhotos",
    "photo.restore",
    "photo.deletePermanently",
    "photo.removeFromCatalog=Remove Photo from Catalog",
    "library.deleteRejected=Delete Rejected Photos…",
];

const METADATA: &[&str] = &[
    "dialog.keywordShortcut",
    "keyword.toggleShortcut=Toggle Shortcut Keyword",
    "tool.painter=Enable Painting",
    "---",
    "@Keyword Set",
    "@Color Label Set",
    "---",
    "metadata.targetOnly",
    "---",
    "photo.copyMetadata=Copy Metadata…",
    "photo.pasteMetadata",
    "photo.syncMetadata",
    "develop.autoSync=Enable Auto Sync",
    "---",
    "@Metadata Presets=Metadata Preset",
    "---",
    "dialog.captureTime",
    "photo.setCaptureTime{\"fromExif\":true}=Revert Capture Time to Original",
    "---",
    "photo.saveMetadataToFile",
    "photo.readMetadataFromFile",
    "photo.updateDngPreview",
    "---",
    "file.importKeywords",
    "file.exportKeywords",
    "keyword.purgeUnused",
];

const VIEW: &[&str] = &[
    "panel.toolbar",
    "view.filterBar=Show Filter Bar",
    "---",
    "view.loupeToggle=Toggle Loupe View",
    "view.zoomToggle=Toggle Zoom View",
    "---",
    "view.zoomIn",
    "view.zoomOut",
    "view.zoom100=Zoom To 100%",
    "view.lockZoomPosition",
    "---",
    "view.thumbSizeUp=Increase Grid Size",
    "view.thumbSizeDown=Decrease Grid Size",
    "view.scrollByRow",
    "---",
    "@Sort",
    "---",
    "view.photoGrid=Grid",
    "view.loupe=Loupe",
    "view.compare",
    "view.survey",
    "view.people",
    "---",
    "module.develop=Go to Develop",
    "panel.crop=Crop",
    "panel.remove=Spot Removal",
    "panel.masking=Masking",
    "@Create New Mask",
    "tool.wbPicker=Adjust White Balance",
    "tool.guidedUpright",
    "---",
    "dialog.viewOptions",
    "@Grid View Style",
    "@Loupe Info",
    "@Loupe Overlay",
    "---",
    "view.mirrorImage",
];

const WINDOW: &[&str] = &[
    "view.slideshow=Impromptu Slideshow",
    "---",
    "@Panels",
    "@Screen Mode",
    "@Lights Out",
    "@Secondary Display",
    "---",
    "library.sourceBack",
    "library.sourceForward",
    "---",
    "module.library",
    "module.develop",
    "module.map",
    "module.book",
    "module.slideshow",
    "module.print",
    "module.web",
    "module.export",
    "module.previous",
    "---",
    "@Modules",
    "@Edit Panels",
    "@Edit Sections",
    "@Tools",
    "@Sidebar Sections",
    "@Appearance",
    "@Slideshow",
    "view.secondWindow",
];

const HELP: &[&str] = &[
    "app.whatsNew",
    "app.help",
    "---",
    "module.help",
    "app.shortcuts",
    "---",
    "app.feedback=Provide Feedback…",
    "app.website",
    "app.github",
    "app.discord",
    "---",
    "app.checkForUpdates=Updates…",
    "app.installUpdate",
    "app.systemInfo",
    "app.openLogFolder",
    "---",
    "app.about",
];

const DEVELOP: &[&str] = &[
    "version.create=New Snapshot",
    "dialog.createPreset=New Preset…",
    r#"ui.prompt{"title":"New Preset Folder","hint":"Folder name","command":"preset.createGroup","key":"name"}=New Preset Folder…"#,
    "history.clear=Clear History",
    "---",
    "library.previous=Previous Photo",
    "library.next=Next Photo",
];

const DEVELOP_PHOTO: &[&str] = &[
    "album.toggleTarget=Add to Quick Collection",
    "---",
    "app.showInFinder",
    "app.showInFolders=Go to Folder in Library",
    "second.locked=Lock to Second Monitor",
    "---",
    "view.reference=Open in Reference View",
    "---",
    "@Edit In",
    "@Photo Merge=Photo>Photo Merge",
    "dialog.enhance",
    "---",
    "photo.virtualCopy",
    "photo.setCopyAsOriginal",
    "---",
    "photo.rotateLeft=Rotate Left (CCW)",
    "photo.rotateRight=Rotate Right (CW)",
    "photo.flipHorizontal",
    "photo.flipVertical",
    "---",
    "@Set Flag=Photo>Set Flag",
    "@Set Rating=Photo>Set Rating",
    "@Set Color Label=Photo>Set Color Label",
    "view.autoAdvance",
    "---",
    "photo.saveMetadataToFile",
    "photo.readMetadataFromFile",
    "photo.updateDngPreview",
    "---",
    "photo.delete=Remove Photo…",
    "library.deleteRejected=Delete Rejected Photos…",
];

const SETTINGS: &[&str] = &[
    "develop.reset=Reset All Settings",
    "develop.updateProcess=Update to Current Process Version",
    "@Process Version",
    "---",
    "view.softProof=Proof",
    "---",
    "dialog.copySettings=Copy Settings…",
    "develop.paste=Paste Settings",
    "develop.pastePrevious",
    "---",
    "dialog.syncSettings",
    "dialog.syncCopies",
    "dialog.syncSnapshots",
    "develop.matchExposure",
    "---",
    "masks.updateAi=Update AI Settings",
    "---",
    "mask.deleteEmpty=Delete All Empty Masks",
    "---",
    "develop.autoSync=Enable Auto Sync",
    "---",
    r#"develop.wb{"mode":"auto"}=Auto White Balance"#,
    "develop.auto",
    "---",
    "develop.treatment=Convert to Black & White",
    "---",
    "develop.hdr=Toggle HDR Edit Mode",
    "---",
    "crop.reset",
    "crop.asShot",
    "crop.lockAspect=Constrain Aspect Ratio",
    "crop.sameAspect",
    "crop.rotateAspect",
    "---",
    "beforeAfter.copyAfterToBefore",
    "beforeAfter.copyBeforeToAfter",
    "beforeAfter.swap=Swap Before and After",
];

const TOOLS: &[&str] = &[
    "panel.crop=Crop",
    "panel.redeye=Red Eye",
    "---",
    "panel.remove=Remove",
    "@Remove Options",
    "---",
    "panel.masking=Masking",
    "@Create New Mask",
    "@Add to Mask with",
    "@Subtract from Mask with",
    "@Intersect Mask with",
    "---",
    "tool.guidedUpright",
    "tool.wbPicker",
    "tool.fringe",
    "tool.pointColor",
    "---",
    "@Target Adjustment",
    "---",
    "@Tool Overlay",
    "@Crop Guide Overlay",
    "@Mask Overlay",
    "---",
    // The app's brush keys
    "@Brush=Window>Tools",
];

const DEVELOP_VIEW: &[&str] = &[
    "panel.toolbar",
    "---",
    "view.zoomToggle=Toggle Zoom View",
    "---",
    "view.zoomIn",
    "view.zoomOut",
    "view.zoom100=Zoom To 100%",
    "view.zoomFit",
    "---",
    "view.reference=Open in Reference View",
    "@Before / After=View>Before/After",
    "view.softProof",
    "view.clipping",
    "---",
    "@Loupe Info=View>Loupe Info",
    "@Loupe Overlay=View>Loupe Overlay",
    "dialog.viewOptions",
    "---",
    "view.photoGrid=Library Grid",
    "view.loupe=Library Loupe",
    "view.compare=Library Compare",
    "view.survey=Library Survey",
    "view.people=Library People",
    "---",
    "@Zoom=View>Zoom",
];

const MAP: &[&str] =
    &["map.focusSearch=Search…", "@Tracklog", "---", "@Location Filter", "---", "map.fit=Show All Pins", "map.reveal=Show Selected Pins"];

const MAP_PHOTO: &[&str] = &[
    "album.toggleTarget=Add to Quick Collection",
    "---",
    "app.showInFinder",
    "app.showInFolders=Go to Folder in Library",
    "---",
    "photo.virtualCopy",
    "---",
    "photo.rotateLeft=Rotate Left (CCW)",
    "photo.rotateRight=Rotate Right (CW)",
    "---",
    "@Set Flag=Photo>Set Flag",
    "@Set Rating=Photo>Set Rating",
    "@Set Color Label=Photo>Set Color Label",
    "---",
    r#"map.geotag{"clear":true}=Delete GPS Coordinates"#,
    "map.clearLocationMetadata",
];

const MAP_VIEW: &[&str] = &[
    "panel.toolbar",
    "view.filterBar=Show Filter Bar",
    "map.showInfo=Show Map Info",
    "map.showKey=Show Map Key",
    "map.toggleSavedLocations",
    "map.toggleLockMarkers",
    "---",
    "@Map Style",
];

const BOOK: &[&str] = &[
    r#"ui.prompt{"title":"Create Saved Book","hint":"Name","command":"book.save","key":"name"}=Create Saved Book…"#,
    "---",
    r#"book.go{"delta":-1}=Previous Page"#,
    r#"book.go{"delta":1}=Next Page"#,
    "---",
    "book.refreshCaptions",
    "dialog.bookPreferences",
    "---",
    "book.export",
];

const BOOK_VIEW: &[&str] = &[
    "book.headerBar=Show Header Bar",
    "book.toggleGuides",
    "book.infoOverlay=Show Info Overlay",
    "---",
    r#"book.view{"mode":"multi"}=Multi-Page View"#,
    r#"book.view{"mode":"spread"}=Spread View"#,
    r#"book.view{"mode":"single"}=Single Page View"#,
    r#"book.view{"mode":"zoomed"}=Zoomed Page View"#,
];

const SLIDESHOW: &[&str] = &[
    r#"ui.prompt{"title":"Create Saved Slideshow","hint":"Name","command":"slideshow.saveSlideshow","key":"name"}=Create Saved Slideshow…"#,
    r#"ui.prompt{"title":"New Template","hint":"Template name","command":"slideshow.saveTemplate","key":"name"}=New Template…"#,
    r#"ui.prompt{"title":"New Template Folder","hint":"Folder name","command":"slideshow.createTemplateFolder","key":"name"}=New Template Folder"#,
    "---",
    "slideshow.exportPdf",
    "slideshow.exportJpeg",
    "slideshow.exportVideo",
    "---",
    r#"ui.prompt{"title":"Add Text Overlay…","hint":"{Caption} · {Filename}","command":"slideshow.overlays","key":"addTextString"}=Add Text Overlay…"#,
    "---",
    "photo.rotateLeft=Rotate Left (CCW)",
    "photo.rotateRight=Rotate Right (CW)",
];

const PLAY: &[&str] = &[
    "slideshow.play",
    "slideshow.preview",
    "slideshow.togglePause=Pause",
    "slideshow.end=End Slideshow",
    "---",
    "slideshow.next",
    "slideshow.previous",
    "---",
    "@Content=Slideshow Content",
];

const SLIDESHOW_VIEW: &[&str] = &["panel.toolbar", "slideshow.toggleGuides"];

const PRINT: &[&str] = &[
    r#"ui.prompt{"title":"Create Saved Print","hint":"Name","command":"printui.saveCreation","key":"name"}=Create Saved Print…"#,
    r#"ui.prompt{"title":"New Template","hint":"Template name","command":"printui.saveTemplate","key":"name"}=New Template…"#,
    r#"ui.prompt{"title":"New Template Folder","hint":"Folder name","command":"printui.createTemplateFolder","key":"name"}=New Template Folder"#,
    "---",
    r#"printui.page{"delta":-1}=Previous Page"#,
    r#"printui.page{"delta":1}=Next Page"#,
    "---",
    "dialog.print=Print…",
    "dialog.pageSetup=Page Setup…",
];

const PRINT_VIEW: &[&str] = &["printui.toggleGuides", "printui.toggleRulers", "printui.infoOverlay=Show Info Overlay"];

const WEB: &[&str] = &[
    r#"ui.prompt{"title":"Create Saved Web Gallery","hint":"Name","command":"web.saveGallery","key":"name"}=Create Saved Web Gallery…"#,
    r#"ui.prompt{"title":"New Template","hint":"Template name","command":"webui.saveTemplate","key":"name"}=New Template…"#,
    r#"ui.prompt{"title":"New Template Folder","hint":"Folder name","command":"webui.createTemplateFolder","key":"name"}=New Template Folder"#,
    "---",
    "webui.reload=Reload",
    "webui.export=Export Web Photo Gallery…",
    "webui.previewInBrowser",
];

const WEB_VIEW: &[&str] = &["panel.toolbar", "webui.guides=Show Guides"];

/// Classic's submenus: (menu, submenu label, slots).
const SUB: &[(&str, &str, &[&str])] = &[
    ("File", "Auto Import", &["app.autoImportToggle", r#"app.settings{"section":"autoImport"}=Auto Import Settings…"#]),
    (
        "File",
        "Library Filters",
        &[
            "library.toggleFilters=Enable Filters",
            "library.lockFilters",
            "library.rememberFilters",
            "---",
            "view.filterBar=Show Filter Bar",
            "library.clearFilter",
            "library.refresh",
            "library.holdView",
        ],
    ),
    ("Photo", "People", &["face.findSimilar", "face.moveToUnnamed", "face.removeRegion", "face.editName", "face.drawRegions"]),
    (
        "Photo",
        "Stacking",
        &[
            "stack.group",
            "stack.ungroup=Unstack",
            "stack.remove",
            "stack.split",
            "---",
            "stack.toggle=Collapse Stack",
            "stack.collapseAll=Collapse All Stacks",
            "stack.expandAll=Expand All Stacks",
            "---",
            "stack.setTop=Move to Top of Stack",
            "stack.moveUp=Move Up in Stack",
            "stack.moveDown=Move Down in Stack",
            "---",
            "dialog.autoStack",
        ],
    ),
    (
        "Photo",
        "Develop Settings",
        &[
            "develop.reset=Reset",
            "develop.updateProcess=Update to Current Process Version",
            "dialog.copySettings=Copy Settings…",
            "develop.paste=Paste Settings",
            "develop.pastePrevious",
            "dialog.syncSettings",
            "masks.updateAi=Update AI Settings",
            "mask.deleteEmpty=Delete All Empty Masks",
            r#"develop.wb{"mode":"auto"}=Auto White Balance"#,
            "develop.auto",
            "develop.treatment=Convert to Black & White",
            "develop.hdr=Toggle HDR Edit Mode",
            "develop.matchExposure",
            "---",
            // The app's own
            "develop.copy=Copy All Settings",
            "dialog.pasteSettings",
            "version.create",
            "dialog.createPreset",
        ],
    ),
    (
        "Tools",
        "Create New Mask",
        &[
            r#"mask.add{"kind":"subject"}=Select Subject"#,
            r#"mask.add{"kind":"sky"}=Select Sky"#,
            r#"mask.add{"kind":"background"}=Select Background"#,
            "tool.people=Select People",
            "@Select Landscape",
            "tool.object=Select Objects",
            "---",
            "tool.brush=Brush",
            "tool.linear=Linear Gradient",
            "tool.radial=Radial Gradient",
            "---",
            "tool.colorRange",
            "tool.luminanceRange",
            "tool.depthRange",
        ],
    ),
    ("View", "Create New Mask", &["tool.brush=Brush", "tool.linear=Linear Gradient", "tool.radial=Radial Gradient"]),
    ("Tools", "Remove Options", &["remove.useGenerative", "remove.detectObjects"]),
    ("Tools", "Target Adjustment", &[]),
    ("Tools", "Tool Overlay", &[]),
    (
        "Tools",
        "Crop Guide Overlay",
        &["view.cropOverlay=Cycle Grid Overlay", "view.cropOverlayOrientation=Cycle Grid Overlay Orientation", "dialog.cropOverlays"],
    ),
    (
        "Tools",
        "Mask Overlay",
        &[
            "view.maskOverlay=Show Overlay",
            "view.maskOverlayMode=Cycle Overlay Mode",
            "view.maskOverlayColor=Cycle Overlay Color",
            "view.maskOverlayAuto=Automatically Toggle Overlay",
            "view.maskUnselectedPins",
            "---",
            "view.maskPins",
            "view.maskOverlayAll",
            "view.visualizeSpots",
        ],
    ),
    ("Map", "Tracklog", &["map.loadTrack=Load Tracklog…", "map.clearTracks", "---", "map.autoTag=Auto-Tag Selected Photos"]),
    (
        "Window",
        "Lights Out",
        &[
            r#"view.lightsOut{"mode":"off"}=Lights On"#,
            r#"view.lightsOut{"mode":"dim"}=Lights Dim"#,
            r#"view.lightsOut{"mode":"black"}=Lights Off"#,
            "view.lightsOut=Next Light Mode",
        ],
    ),
    ("Edit", "Keymap", &["app.shortcuts=Keyboard Shortcuts…", "---"]),
];

/// A Classic menu item the app doesn't have yet. `wire` items are features the app has (in a
/// panel or a dialog) whose menu entry still needs a way to run without the panel (a dialog for
/// a name, a picker); the rest are new features.
pub struct ComingSoon {
    pub id: &'static str,
    pub label: &'static str,
    /// Classic's key, shown greyed beside the item while no other command has it.
    pub hint: Option<&'static str>,
    /// Rough size of the work: S (an afternoon), M (a few days), L (a week or more).
    pub size: char,
    pub wire: bool,
    pub what: &'static str,
}

// (every item is built; kept for the next one a menu table adds)
#[allow(dead_code)]
const fn soon(id: &'static str, label: &'static str, hint: Option<&'static str>, size: char, wire: bool, what: &'static str) -> ComingSoon {
    ComingSoon { id, label, hint, size, wire, what }
}

/// Every greyed "Coming soon" item.
pub const COMING_SOON: &[ComingSoon] = &[
    // File
    // Edit
    // Library
    // Photo
    // Metadata
    // View
    // Window
    // Develop
    // Map
    // Book
    // Slideshow
    // Print
    // Web
];

/// The "Coming soon" item `id`.
pub fn coming_soon(id: &str) -> Option<&'static ComingSoon> {
    COMING_SOON.iter().find(|s| s.id == id)
}

/// Whether `id` is a "Coming soon" item.
pub fn is_coming_soon(id: &str) -> bool {
    id.starts_with("soon.")
}

/// The hover text of a "Coming soon" item.
pub const COMING_SOON_HINT: &str = "Coming soon";

/// One parsed layout slot.
#[derive(Debug, PartialEq)]
pub enum Slot<'a> {
    Separator,
    /// (label shown, menu the path is under — `None`: this one —, path)
    Submenu(&'a str, Option<&'a str>, &'a str),
    /// (id, JSON parameters, label shown)
    Item(&'a str, Option<&'a str>, Option<&'a str>),
}

pub fn parse(slot: &str) -> Slot<'_> {
    if slot == "---" {
        return Slot::Separator;
    }
    if let Some(rest) = slot.strip_prefix('@') {
        let (label, path) = rest.split_once('=').unwrap_or((rest, rest));
        return match path.split_once('>') {
            Some((menu, path)) => Slot::Submenu(label, Some(menu), path),
            None => Slot::Submenu(label, None, path),
        };
    }
    // the JSON may hold `=` only inside strings we never write; the label follows the last `}`
    let (head, label) = match slot.rfind('}') {
        Some(end) => match slot.get(end + 1..).and_then(|r| r.strip_prefix('=')) {
            Some(label) => (slot.get(..=end).unwrap_or(slot), Some(label)),
            None => (slot, None),
        },
        None => match slot.split_once('=') {
            Some((h, l)) => (h, Some(l)),
            None => (slot, None),
        },
    };
    match head.find('{') {
        Some(i) => Slot::Item(head.get(..i).unwrap_or(head), head.get(i..), label),
        None => Slot::Item(head, None, label),
    }
}

/// Every layout's slots, for the checks and the gaps list: (module, menu, submenu, slots).
pub fn all_layouts() -> Vec<(ModuleId, &'static str, Option<&'static str>, &'static [&'static str])> {
    let mut out = Vec::new();
    for module in ModuleId::ALL {
        if module == ModuleId::Export {
            continue;
        }
        for title in titles(module) {
            let slots = layout(module, title);
            out.push((module, *title, None, slots));
            for slot in slots {
                if let Slot::Submenu(label, ..) = parse(slot)
                    && let Some(sub) = sub_layout(title, label)
                {
                    out.push((module, *title, Some(label), sub));
                }
            }
        }
    }
    out
}

/// `docs/lrc-menu-gaps.md`: every "Coming soon" item, grouped by module and menu (a shared menu
/// once, under "Every module").
pub fn gaps_doc() -> String {
    let mut s = String::from(
        "# Lightroom Classic menu items still to build\n\n\
         The menu bar follows Lightroom Classic's: the same menus per module, items, order and keys.\n\
         The items below are in the menus already, greyed out with **Coming soon** on hover; none of them runs\n\
         anything yet. Pick one, build it, and replace its `soon.*` slot in `crates/ui-egui/src/lrc_menus.rs` with\n\
         the real command (its entry in `COMING_SOON` goes, and this file is regenerated by\n\
         `cargo test -p lcc-ui-egui lrc_menu_gaps_doc -- --ignored`).\n\n\
         **Size:** S = an afternoon, M = a few days, L = a week or more. **Kind:** *wire-up* = the app already\n\
         does this in a panel or dialog and the menu item only needs to reach it (often a small name dialog\n\
         or picker); *feature* = new behaviour.\n\n\
         Generated from the menu tables; don't edit by hand.\n",
    );
    let mut seen: Vec<&str> = Vec::new();
    let mut section = String::new();
    for (module, title, sub, slots) in all_layouts() {
        let shared = matches!(title, "File" | "Edit" | "Window" | "Help");
        let heading = if shared { format!("Every module ▸ {title}") } else { format!("{} module ▸ {title}", module.label()) };
        let heading = match sub {
            Some(sub) => format!("{heading} ▸ {sub}"),
            None => heading,
        };
        let mut rows = Vec::new();
        for slot in slots {
            if let Slot::Item(id, ..) = parse(slot)
                && let Some(c) = coming_soon(id)
                && !seen.contains(&id)
            {
                seen.push(id);
                let key = c.hint.map(|k| format!(" ({})", k.replace("Cmd", "Ctrl").replace("Delete", "Backspace"))).unwrap_or_default();
                let kind = if c.wire { "wire-up" } else { "feature" };
                rows.push(format!("| {}{key} | {} | {} | {kind} | `{}` |", c.label, c.what, c.size, c.id));
            }
        }
        if rows.is_empty() {
            continue;
        }
        if heading != section {
            s.push_str(&format!("\n## {heading}\n\n| Item | What it does | Size | Kind | Slot |\n|---|---|---|---|---|\n"));
            section = heading;
        }
        for r in rows {
            s.push_str(&r);
            s.push('\n');
        }
    }
    s.push_str(
        "\n## Not in this list yet\n\n\
         - The long Classic submenus whose top item is here but whose every choice isn't: View ▸ Sort's extra keys\n\
           (Added Order, Edit Time, Edit Count, Pick, Label, Extension, File Type, Aspect Ratio, location and\n\
           exposure keys), Grid View Style ▸ Show Extras / Cycle View Style, Loupe Info ▸ Info 1 / Info 2.\n\
         - Window ▸ Panels with Classic's per-module keys (Ctrl+Shift+0–4, Ctrl+0–7): the app keeps its own\n\
           panel list there, and the Develop module's panel keys were not checked against Classic.\n\
         - Help's Classic-only entries (tutorials, the online services): the app has its own Help items.\n",
    );
    s
}

/// The "Coming soon" items as UI commands (no menu path: the layouts place them): `enabled()` is
/// false and running one is an error, so a key, the control channel or MCP can't run them either.
pub const SOON_COMMANDS: [crate::menus::UiCommand; COMING_SOON.len()] = {
    let mut out: [crate::menus::UiCommand; COMING_SOON.len()] = [("", "", None, ""); COMING_SOON.len()];
    let mut i = 0;
    while i < COMING_SOON.len() {
        out[i] = (COMING_SOON[i].id, COMING_SOON[i].label, None, "");
        i += 1;
    }
    out
};
