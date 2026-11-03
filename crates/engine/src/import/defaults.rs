use super::{Deserialize, Photo, Serialize, Session};

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
    // metadata defaults fill gaps only: the file's own copyright / creator win
    let d = &s.import_defaults;
    if p.meta.copyright.trim().is_empty() && !d.copyright.trim().is_empty() {
        p.meta.copyright = d.copyright.trim().to_string();
    }
    if p.meta.creator.trim().is_empty() && !d.creator.trim().is_empty() {
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
