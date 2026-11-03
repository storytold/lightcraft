use super::{Deserialize, Serialize};

/// Settings group ids (`SettingsGroup` serde names) a new preset includes by default: everything
/// but crop, masks, spots and red eye.
pub fn default_preset_groups() -> Vec<String> {
    lightcraft_develop::SettingsGroup::default_copy().iter().filter_map(|g| serde_json::to_value(g).ok()?.as_str().map(str::to_string)).collect()
}

impl Dialog {
    /// A fresh Create Preset dialog.
    pub fn create_preset() -> Dialog {
        Dialog::CreatePreset { name: String::new(), group: "User Presets".into(), groups: default_preset_groups() }
    }
}

/// A face's name being typed in the loupe: which face, and what has been typed so far.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NameEdit {
    pub photo: u64,
    pub index: usize,
    pub text: String,
    /// Just opened: the text box takes the keyboard focus once.
    pub fresh: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Dialog {
    /// A Lightroom import result stays readable after its activity row disappears.
    LightroomResult {
        summary: String,
        library: Option<String>,
        details: Vec<String>,
        first: Option<u64>,
    },
    /// File ▸ Print…: the Print module. Its state is the session's (`print.*`); the page on show
    /// and the selected cell are [`UiState::print`].
    Print,
    ContactSheet {
        options: lightcraft_engine::contact_sheet::Options,
    },
    /// View ▸ Export Slideshow…: format, size, quality, frame rate, container.
    SlideshowExport {
        settings: lightcraft_engine::slideshow_export::ExportSettings,
    },
    /// `parent`: the folder to create it in (none: the top level). `include_selected`: the new album
    /// holds the selected photos (Lightroom's "Include selected photos"; folders hold none).
    NewAlbum {
        name: String,
        folder: bool,
        #[serde(default)]
        parent: Option<u64>,
        #[serde(default = "yes")]
        include_selected: bool,
    },
    RenameAlbum {
        id: u64,
        name: String,
    },
    /// One text field; OK runs `command` with `params` plus `{key: value}` (rename a preset or a
    /// version, move a preset to a new group…).
    TextPrompt {
        title: String,
        hint: String,
        value: String,
        command: String,
        params: serde_json::Value,
        key: String,
    },
    /// Edit Capture Time: `mode` "set" (`time`; the others shift along), "shift" (by `days`,
    /// `hours`, `minutes`) or "zone" (time-zone shift by `zone` hours).
    CaptureTime {
        mode: String,
        time: String,
        days: i32,
        hours: i32,
        minutes: i32,
        zone: f32,
    },
    /// The import review (File → Import Photos…).
    Import {
        opts: Box<crate::import::ImportDialog>,
    },
    /// Edit the colour label names (red, yellow, green, blue, purple; empty = the colour's name).
    LabelNames {
        names: Vec<String>,
        /// Also save the names as a label set of this name (empty = don't).
        save_as: String,
    },
    /// Batch rename the selected photos with a file-name template.
    Rename {
        template: String,
        start: u32,
    },
    /// Rename a keyword on every photo (children included).
    RenameKeyword {
        from: String,
        to: String,
    },
    /// Rename every face of `from` to `to`. `merge` is the user's yes to combining with the person
    /// `to` already names.
    RenamePerson {
        from: String,
        to: String,
        merge: bool,
    },
    /// Create a keyword, or edit one (`editing`): Lightroom Classic's Create / Edit Keyword Tag.
    KeywordTag {
        /// The keyword being edited; `None`: a new one.
        editing: Option<String>,
        name: String,
        /// A new keyword goes inside this one when `inside` is on (the keyword picked in the
        /// list, or the default parent).
        parent: Option<String>,
        inside: bool,
        /// Comma-separated.
        synonyms: String,
        include_on_export: bool,
        export_containing: bool,
        export_synonyms: bool,
        person: bool,
        /// A new keyword is given to the selected photos.
        add_to_selected: bool,
        /// Opened from the sidebar's Keywords header: the new keyword is shown there once made.
        #[serde(default)]
        reveal: bool,
    },
    /// Edit Keyword Set: a set's name and its nine slots (an empty one is an empty slot). `replaces`:
    /// the set being edited (renamed when the name changes); `None` saves a new set (from Recent
    /// Keywords).
    KeywordSet {
        replaces: Option<String>,
        name: String,
        slots: Vec<String>,
        /// Save as a new set, leaving the edited one (Lightroom Classic's Save as New Preset).
        #[serde(default)]
        as_new: bool,
    },
    /// Move a keyword inside `parent` (`None`: the top level) where one of its name is already:
    /// asks before merging the two.
    MoveKeyword {
        keyword: String,
        parent: Option<String>,
    },
    /// Delete a keyword from every photo and the keyword list, after asking.
    DeleteKeyword {
        keyword: String,
        /// Photos that have it (or one below it).
        count: usize,
    },
    /// Merge keywords into another one on every photo.
    MergeKeywords {
        from: Vec<String>,
        into: String,
    },
    /// Auto-stack by capture time: the largest gap between consecutive shots, in seconds.
    AutoStack {
        gap: f32,
    },
    /// Save the current view (source + filter) as a smart album.
    NewSmartAlbum {
        name: String,
        #[serde(default)]
        parent: Option<u64>,
    },
    /// Help ▸ What's New.
    WhatsNew,
    /// Photo ▸ Assisted Culling: reject photos below this focus score (0 = none), pick the best
    /// of each burst; shots at most `burst_gap` seconds apart and at least `similarity` alike are
    /// a burst.
    Cull {
        reject_below: f32,
        pick_best: bool,
        burst_gap: f32,
        similarity: f32,
    },
    /// Help ▸ System Info: (label, value) rows.
    DenoiseModel {
        info: serde_json::Value,
        accepted: bool,
    },
    /// A face model file the user chose: what it is, its licence terms, and the "I accept" box.
    FaceModel {
        path: String,
        /// The engine's `faces.models.inspect` answer.
        info: serde_json::Value,
        accepted: bool,
    },
    SystemInfo {
        rows: Vec<(String, String)>,
    },
    /// Every metadata field of a photo's file (`photo.allMetadata`), filtered by `search`.
    AllMetadata {
        title: String,
        rows: serde_json::Value,
        search: String,
    },
    /// Create (`id` None) or edit a smart album's rules.
    SmartRules {
        id: Option<u64>,
        name: String,
        rules: lightcraft_catalog::RuleSet,
        /// The folder a new smart album is created in (ignored when editing).
        #[serde(default)]
        parent: Option<u64>,
    },
    /// `groups`: the settings groups the preset includes (`SettingsGroup` ids).
    CreatePreset {
        name: String,
        group: String,
        #[serde(default = "default_preset_groups")]
        groups: Vec<String>,
    },
    CopySettings {
        groups: Vec<String>,
    },
    /// Paste Selected Settings: which of the copied groups to paste.
    PasteSettings {
        groups: Vec<String>,
    },
    /// `resize` is used unless `full_size`; `limit_kb` 0 = no limit; `dir` empty = default export folder.
    Export {
        opts: lightcraft_engine::export::ExportOptions,
        full_size: bool,
        resize: lightcraft_engine::export::Resize,
        /// Name typed for "Save as Preset".
        #[serde(default)]
        preset_name: String,
        limit_kb: u32,
        dir: String,
    },
    /// Photo Merge (HDR / Panorama / HDR Panorama) options; the preview lives in the app.
    Merge {
        opts: crate::merge::MergeDialog,
    },
    /// Settings (preferences): `tab` = general | import | performance | interface.
    Settings {
        tab: String,
    },
    /// Object and Describe masks need the SAM 3 model, which isn't installed: offer to download
    /// it (size, licence, progress). `then`: the AI mask to start once it is there (`kind`
    /// object|prompt, `op` new|add|subtract|intersect).
    SamModel {
        then: Option<(String, String)>,
        /// Why the download couldn't start (shown in the dialog).
        #[serde(default)]
        error: Option<String>,
    },
    /// Open Library was given a Lightroom Classic catalog (issue #891): offer File ▸ Import
    /// Lightroom Catalog… with `catalog` (the `.lrcat` found; `None`: ask for it) instead.
    /// `folder`: what was chosen.
    LightroomCatalog {
        folder: String,
        catalog: Option<String>,
    },
    /// File ▸ Import Lightroom Catalog…: where the catalog at `catalog` goes (issue #2672, see
    /// [`crate::lightroom_destination`]). Merge into the open library `library` (`photos` in it), or
    /// (`new_library`) create the library `name` in the folder `location`, switch to it and import
    /// there. `dont_ask`: merge from now on without asking. `generation`: the library the window
    /// was opened for (`Session::library_generation`): another one opened meanwhile isn't merged into.
    LightroomDestination {
        /// The `.lrcat` file.
        catalog: String,
        /// The open library's name (its folder's name).
        library: String,
        /// The photos in the open library.
        photos: usize,
        /// "Create a new library" is chosen (else "Merge into the open library").
        new_library: bool,
        /// The new library's name (prefilled from the catalog's file name).
        name: String,
        /// The folder the new library goes in (prefilled with the open library's parent folder).
        location: String,
        /// "Don't ask again (always merge into the open library)".
        dont_ask: bool,
        /// `Session::library_generation` when the window opened.
        generation: u64,
    },
    /// Save the selected mask's sliders as a mask effect preset: which sliders (`FIELDS` names,
    /// the colour as `color_sat`), whether it carries Amount and the brush settings.
    SaveEffect {
        name: String,
        group: String,
        fields: Vec<String>,
        amount: bool,
        brush: bool,
    },
    /// Manage Effect Presets: search, the selected effects, an inline rename (old, new),
    /// a delete waiting for confirmation, a new group's name being typed.
    ManageEffects {
        search: String,
        selected: Vec<String>,
        rename: Option<(String, String)>,
        confirm_delete: bool,
        new_group: String,
    },
    /// Confirm deleting mask effect presets.
    DeleteEffects {
        names: Vec<String>,
    },
    /// Confirm moving photos to Recently Deleted.
    ConfirmDelete {
        count: usize,
    },
    /// Confirm moving photos dropped on a Folders row into that folder on disk
    /// (`photo.moveToFolder`). `name`: the folder's name, as the question says it.
    MovePhotos {
        ids: Vec<u64>,
        folder: String,
        name: String,
        dont_ask: bool,
        /// The library the photos were dropped in (`Session::library_generation`): ids are
        /// numbered per library, so in another one they name other photos.
        library: u64,
    },
    /// Confirm taking a folder's photos out of the library (`library.removeFolder`).
    RemoveFolder {
        path: String,
        /// What the question calls it (a folder's last two names, a disk's name).
        name: String,
        count: usize,
        /// A whole disk or share (`library.removeFolder` takes it only on request).
        disk: bool,
    },
    /// Synchronize Folder: what changed in a library folder on disk, and what to do about it
    /// (`folder.synchronize`; the scan runs in [`crate::sync`]).
    SynchronizeFolder {
        path: String,
        /// What the dialog calls it (a folder's last two names).
        name: String,
        /// A whole disk, or a folder holding disks (`folder.synchronize` `disk`).
        #[serde(default)]
        disk: bool,
        /// What the scan found (`None` while it runs).
        counts: Option<SyncCounts>,
        import_new: bool,
        #[serde(default = "yes")]
        relink_moved: bool,
        remove_missing: bool,
        read_metadata: bool,
    },
    About,
    Shortcuts,
    /// View ▸ View Options (⌘J): the info overlay and the lines of Info 1 and Info 2. Changes apply
    /// as they are made.
    ViewOptions,
}

fn yes() -> bool {
    true
}

/// How many changes a Synchronize Folder scan found, by kind.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SyncCounts {
    /// The folder isn't there (moved, renamed, or on a disk that isn't connected).
    #[serde(default)]
    pub offline: bool,
    pub new: usize,
    pub duplicates: usize,
    pub unreadable: usize,
    pub missing: usize,
    pub metadata: usize,
    #[serde(default)]
    pub moved: usize,
}

impl SyncCounts {
    /// The folder is there and the scan found nothing to bring in, relink, remove or read: the
    /// dialog only says so (files it skipped are not changes).
    pub fn up_to_date(&self) -> bool {
        !self.offline && self.new + self.moved + self.missing + self.metadata == 0
    }
}
