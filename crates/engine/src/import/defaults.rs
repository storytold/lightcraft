use super::{Deserialize, Photo, ReadFromPhotos, Serialize, Session};

/// The develop settings a photo gets on import: raws start from their as-shot white balance with
/// default sharpening / colour noise reduction, and file-embedded lens corrections on (as the
/// camera intended); a user default preset ([`ImportDefaults`]) goes on top.
pub fn import_defaults(p: &Photo) -> lightcraft_develop::DevelopSettings {
    p.import_defaults()
}

/// Does the photo still look as the camera rendered it (its embedded camera preview is then a
/// fair stand-in)? Not when a default preset changed it on import.
pub fn has_import_look(p: &Photo) -> bool {
    *p.develop == p.camera_defaults()
}

/// User defaults applied on import (Settings → Import; saved in the library's `prefs.json`).
/// Presets are referenced by id; a preset that no longer exists is ignored.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImportDefaults {
    /// Preset applied to raw files (`None` = the LightCraft default).
    pub raw_preset: Option<String>,
    /// Use a camera's own default (below) when the photo's camera has one.
    pub per_camera: bool,
    /// Per-camera raw defaults, keyed by the camera's make + model as shown in Info
    /// (`"Canon EOS R5"`).
    pub cameras: Vec<CameraDefault>,
    /// Preset applied to non-raw images (JPEG, PNG, TIFF, HEIC…; `None` = none).
    pub other_preset: Option<String>,
    /// Copyright notice given to imported photos that don't carry one (empty = none).
    pub copyright: String,
    /// Creator given to imported photos that don't name one (empty = none).
    pub creator: String,
    /// Metadata preset applied to every imported photo (`metadata.*`; `None` = none).
    pub metadata_preset: Option<String>,
    /// Auto import: a watched folder whose new photos are added as they arrive (`None` = off),
    /// copied into the library's Originals instead of added in place when `auto_copy`, and an
    /// album they go to.
    pub auto_folder: Option<String>,
    pub auto_copy: bool,
    pub auto_album: Option<String>,
    /// Open the import review when a camera or memory card is connected.
    pub show_on_card: bool,
    /// What imports read from the photos themselves, unless one says otherwise (everything,
    /// until chosen): also photos that are dropped in, found by Auto Import or Synchronize
    /// Folder, or browsed in place.
    pub read: ReadFromPhotos,
    /// What was last ticked under Read in the dialogs that offer it, where that is not `read`:
    /// each opens as it was last used. Nothing else goes by it.
    #[serde(skip_serializing_if = "ReadLast::is_empty")]
    pub read_last: ReadLast,
}

/// A dialog with a Read row of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadDialog {
    /// The Import Photos review.
    Import,
    /// Synchronize Folder.
    Synchronize,
}

impl ReadDialog {
    pub const ALL: [ReadDialog; 2] = [ReadDialog::Import, ReadDialog::Synchronize];

    /// The name commands know it under.
    pub fn name(self) -> &'static str {
        match self {
            ReadDialog::Import => "import",
            ReadDialog::Synchronize => "synchronize",
        }
    }
}

/// What was last ticked under Read in each dialog ([`ImportDefaults::read_last`]): `None` for a
/// dialog that was last used as the library has it, which then goes on following the library.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReadLast {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import: Option<ReadFromPhotos>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synchronize: Option<ReadFromPhotos>,
}

impl ReadLast {
    pub fn is_empty(&self) -> bool {
        *self == ReadLast::default()
    }

    fn of(&mut self, dialog: ReadDialog) -> &mut Option<ReadFromPhotos> {
        match dialog {
            ReadDialog::Import => &mut self.import,
            ReadDialog::Synchronize => &mut self.synchronize,
        }
    }
}

impl ImportDefaults {
    /// What a dialog's Read row opens with: as it was last used, else as the library has it.
    pub fn read_in(&self, dialog: ReadDialog) -> ReadFromPhotos {
        let mut last = self.read_last;
        last.of(dialog).unwrap_or(self.read)
    }

    /// Remember how a dialog's Read row was used (`None`: forget it). Ticks that are the
    /// library's own choice are not kept: the dialog goes on following the library.
    pub fn remember_read(&mut self, dialog: ReadDialog, read: Option<ReadFromPhotos>) {
        *self.read_last.of(dialog) = read.filter(|r| *r != self.read);
    }

    /// Choose what the library reads. The dialogs follow it again: what was ticked in them was
    /// a departure from a choice that is no longer.
    pub fn set_read(&mut self, read: ReadFromPhotos) {
        if read != self.read {
            self.read = read;
            self.read_last = ReadLast::default();
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CameraDefault {
    /// Make + model (`Meta::camera`).
    pub camera: String,
    /// Preset id; `None` = the LightCraft default for this camera.
    pub preset: Option<String>,
}

impl ImportDefaults {
    /// The preset id that applies to a photo of this kind and camera, if any.
    pub fn preset_for(&self, raw: bool, camera: &str) -> Option<&str> {
        if !raw {
            return self.other_preset.as_deref();
        }
        if self.per_camera
            && let Some(c) = self.cameras.iter().find(|c| !camera.is_empty() && c.camera.eq_ignore_ascii_case(camera))
        {
            return c.preset.as_deref();
        }
        self.raw_preset.as_deref()
    }
}

/// Give a freshly imported photo its default settings: the camera defaults, then the matching
/// default preset ([`ImportDefaults::preset_for`]), remembered as the photo's import look.
pub fn apply_import_defaults(s: &Session, p: &mut Photo) {
    apply_import_defaults_beside(s, p, (false, false));
}

/// [`apply_import_defaults`] for a photo whose file has a copyright / a creator of its own
/// that was not read (`own`): it is not given the library's in its place.
pub(super) fn apply_import_defaults_beside(s: &Session, p: &mut Photo, own: (bool, bool)) {
    // metadata defaults fill gaps only: the file's own copyright / creator win, also when they
    // were left in the file
    let d = &s.import_defaults;
    if p.meta.copyright.trim().is_empty() && !d.copyright.trim().is_empty() && !own.0 {
        p.meta.copyright = d.copyright.trim().to_string();
    }
    if p.meta.creator.trim().is_empty() && !d.creator.trim().is_empty() && !own.1 {
        p.meta.creator = d.creator.trim().to_string();
    }
    if let Some(mp) = d.metadata_preset.as_ref().and_then(|n| s.metadata_presets.iter().find(|m| m.name.eq_ignore_ascii_case(n))) {
        crate::cmd::metadata::apply_to(&mut p.meta, &mp.fields);
    }
    let base = p.camera_defaults();
    // a raw shown from its embedded JPEG gets the rendered-file default, not the raw one
    let raw = p.develops_raw();
    let preset = s.import_defaults.preset_for(raw, &p.meta.camera).and_then(|id| s.presets.iter().find(|x| x.id == id));
    match preset {
        Some(pr) => {
            let look = std::sync::Arc::new(pr.apply(&base, 1.0));
            p.develop = look.clone();
            p.import_look = (*look != base).then_some(look);
        }
        None => {
            p.develop = std::sync::Arc::new(base);
            p.import_look = None;
        }
    }
}
