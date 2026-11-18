//! The catalog's vignetting maps (Capture One's LCC "Uniform Light"): each map once, keyed by its id (the SHA-256
//! of its canonical JSON, [`VignettingMapId::of`]), and linked to the reference photo it was measured from. A photo's
//! develop settings hold only the id (`Optics::vignetting_calibration`), so its History steps and Versions share the map.
//!
//! **Append-only.** No op removes a map: removing the reference photo, deleting its file, clearing History, undo
//! (the inverse of [`crate::Op::AddVignettingMap`] is an empty batch) and every save keep the table whole, so a photo
//! that references a map never loses it. A map is self-contained: nothing reads the reference raw again.
//!
//! **Withdrawn.** A map can be withdrawn as a reference ([`crate::Op::SetVignettingMapState`], undoable): Apply
//! doesn't pick it any more, the photos that use it keep it. Create adds a map withdrawn and then puts it in use, so
//! undoing a Create withdraws its map rather than leaving it to be picked as the newest.
//!
//! **The reference link.** Photo ids are unique only within one catalog, so a link is trusted only here: the
//! portable form of settings (sidecars, presets, the clipboard, peers) never carries it, Export / Import as Catalog
//! remaps it ([`crate::transfer`]), and [`reference_photo`] uses a stored id only when that photo's file has the
//! map's reference SHA-1, else finds the photo by that SHA-1.

use std::collections::HashMap;
use std::sync::Arc;

use lcc_develop::{DevelopSettings, EmbeddedVignettingMap, VignettingMapId};
use serde::{Deserialize, Serialize};

use crate::{Catalog, Photo, PhotoId};

/// One map in the catalog.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VignettingMapRecord {
    pub map: EmbeddedVignettingMap,
    /// The photo in this catalog the map was measured from, when known (see [`reference_photo`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub photo: Option<PhotoId>,
    /// When the map came into this catalog (ISO 8601).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub added: String,
    /// Withdrawn as a reference ([`crate::Op::SetVignettingMapState`]): Apply doesn't pick it. The photos that use
    /// it keep it, and the map stays in the table.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub withdrawn: bool,
}

/// What a photo is to Vignetting Calibration ([`Catalog::vignetting_reference_state`]): the reference of a map in
/// use, or of maps that are all withdrawn. A photo that is neither isn't a reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReferenceState {
    Reference,
    Withdrawn,
}

impl ReferenceState {
    /// The value the library filter and the smart-collection rule use: `reference`, `withdrawn`.
    pub fn as_str(self) -> &'static str {
        match self {
            ReferenceState::Reference => "reference",
            ReferenceState::Withdrawn => "withdrawn",
        }
    }
}

impl VignettingMapRecord {
    /// Whether the record can be used under `id`: a valid map whose own id is `id`.
    pub fn fits(&self, id: &VignettingMapId) -> bool {
        self.map.is_valid() && VignettingMapId::of(&self.map) == *id
    }
}

/// Read the table leniently: a row that doesn't parse, isn't a valid map or isn't filed under its own id is left out
/// (with a warning); the rest loads.
pub(crate) fn de_table<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<std::collections::BTreeMap<VignettingMapId, Arc<VignettingMapRecord>>, D::Error> {
    let raw = std::collections::BTreeMap::<String, serde_json::Value>::deserialize(d)?;
    let mut out = std::collections::BTreeMap::new();
    for (k, v) in raw {
        match read_row(&k, &v) {
            Some((id, r)) => {
                out.insert(id, Arc::new(r));
            }
            None => log::warn!("catalog: vignetting map {} is damaged and was left out", k.get(..8).unwrap_or(&k)),
        }
    }
    Ok(out)
}

/// A table row from its key and JSON, when it is a usable one.
pub(crate) fn read_row(key: &str, v: &serde_json::Value) -> Option<(VignettingMapId, VignettingMapRecord)> {
    let id = VignettingMapId::parse(key)?;
    let r: VignettingMapRecord = serde_json::from_value(v.clone()).ok()?;
    r.fits(&id).then_some((id, r))
}

/// The ids of the maps a settings value references.
fn ids_of(s: &DevelopSettings) -> Option<&VignettingMapId> {
    s.optics.vignetting_calibration.as_ref()
}

impl Photo {
    /// Every vignetting map this photo's settings reference: its current settings, History steps, Versions, the
    /// import look and state, and the Before view's.
    pub fn vignetting_map_ids(&self) -> impl Iterator<Item = &VignettingMapId> {
        std::iter::once(&self.develop)
            .chain(self.history.iter().map(|h| &h.settings))
            .chain(self.versions.iter().map(|v| &v.settings))
            .chain(self.import_look.iter())
            .chain(self.import_state.iter())
            .chain(self.before.iter())
            .filter_map(|s| ids_of(s))
    }
}

impl crate::Op {
    /// Every vignetting map the settings this op writes reference (a batch's ops' too): what has to be in the
    /// table for them to render.
    pub fn vignetting_map_ids(&self, out: &mut impl FnMut(&VignettingMapId)) {
        use crate::Op;
        let mut one = |s: &DevelopSettings| {
            if let Some(id) = ids_of(s) {
                out(id);
            }
        };
        match self {
            Op::AddPhoto { photo } => photo.vignetting_map_ids().for_each(&mut *out),
            Op::SetDevelop { settings, .. } => one(settings),
            Op::SetHistory { history, .. } => history.iter().for_each(|h| one(&h.settings)),
            Op::PushHistory { step, .. } => one(&step.settings),
            Op::SetVersions { versions, .. } => versions.iter().for_each(|v| one(&v.settings)),
            Op::SetBefore { before: Some(b), .. } => one(b),
            Op::Batch { ops } => ops.iter().for_each(|o| o.vignetting_map_ids(out)),
            _ => {}
        }
    }
}

/// The reference photo of map `id` in this catalog, by the cross-catalog rule: the stored photo id only when that
/// photo is here (not deleted) and its file's SHA-1 is the map's reference SHA-1; else the first photo here with that
/// SHA-1; else none (the reference is no longer in this catalog; the map still applies).
pub fn reference_photo(cat: &Catalog, id: &VignettingMapId) -> Option<PhotoId> {
    reference_of(cat, cat.vignetting_map(id)?, None)
}

/// [`reference_photo`] of a record (one not in the table yet, too). `by_sha` is a lookup of the photos by
/// lower-case SHA-1 when the caller has one (else the photos are scanned).
fn reference_of(cat: &Catalog, r: &VignettingMapRecord, by_sha: Option<&HashMap<String, Vec<PhotoId>>>) -> Option<PhotoId> {
    let sha = r.map.source.sha1.as_deref().filter(|s| !s.is_empty())?;
    let here = |p: &Photo| !p.deleted && p.sha1.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(sha));
    if let Some(p) = r.photo.and_then(|pid| cat.photo(pid)).filter(|p| here(p)) {
        return Some(p.id);
    }
    // (a virtual copy shares its master's file: the master is the reference)
    let mut found: Vec<&Arc<Photo>> = match by_sha {
        Some(m) => m.get(&sha.to_ascii_lowercase()).into_iter().flatten().filter_map(|id| cat.photo(*id)).filter(|p| here(p)).collect(),
        None => cat.photos().filter(|p| here(p)).collect(),
    };
    found.sort_by_key(|p| (p.copy_of.is_some(), p.id));
    found.first().map(|p| p.id)
}

/// The reference photo of a record about to be added (see [`reference_photo`]).
pub(crate) fn reference_of_record(cat: &Catalog, r: &VignettingMapRecord) -> Option<PhotoId> {
    reference_of(cat, r, None)
}

/// Every reference photo in the catalog and its state: [`ReferenceState::Reference`] when any of the maps measured
/// from it is in use, else [`ReferenceState::Withdrawn`]. Derived from the table, never stored.
pub(crate) fn reference_states(cat: &Catalog) -> HashMap<PhotoId, ReferenceState> {
    let mut out = HashMap::new();
    if cat.vignetting_maps.is_empty() {
        return out;
    }
    let mut by_sha: HashMap<String, Vec<PhotoId>> = HashMap::new();
    for p in cat.photos() {
        if let Some(s) = p.sha1.as_deref().filter(|s| !s.is_empty()) {
            by_sha.entry(s.to_ascii_lowercase()).or_default().push(p.id);
        }
    }
    for r in cat.vignetting_maps.values() {
        if let Some(p) = reference_of(cat, r, Some(&by_sha)) {
            let state = if r.withdrawn { ReferenceState::Withdrawn } else { ReferenceState::Reference };
            let e = out.entry(p).or_insert(state);
            if state == ReferenceState::Reference {
                *e = state;
            }
        }
    }
    out
}

impl Catalog {
    /// What photo `id` is to Vignetting Calibration (`None`: not a reference). Built from the vignetting-map table
    /// on first use after any op that can change it, so it is never stale.
    pub fn vignetting_reference_state(&self, id: PhotoId) -> Option<ReferenceState> {
        self.vignetting_reference_states().get(&id).copied()
    }

    /// Whether photo `id` is the reference of a map in use: what the library filter, the smart-collection rule and the
    /// thumbnail marker show (a withdrawn reference is a normal photo to them).
    pub fn is_vignetting_reference(&self, id: PhotoId) -> bool {
        self.vignetting_reference_state(id) == Some(ReferenceState::Reference)
    }

    /// Every reference photo and its state ([`Catalog::vignetting_reference_state`]).
    pub fn vignetting_reference_states(&self) -> &HashMap<PhotoId, ReferenceState> {
        self.index.vignetting_references.get_or_init(|| reference_states(self))
    }
}

#[cfg(test)]
#[path = "tests_vignetting_maps.rs"]
mod tests;
