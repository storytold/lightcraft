//! Flat-field maps where settings cross the catalog's edge (design notes §10, *Storage*). Inside one catalog a
//! photo's settings reference a map by id and the catalog holds it once ([`lcc_catalog::vignetting_maps`]). Settings that
//! leave it (sidecars, presets, the clipboard, the control channel and MCP, peer sync) are written in their
//! *portable form*, with the map at `optics.vignetting_calibration_map` ([`Session::portable_settings`]); settings that come in
//! give their maps to the session ([`Session::absorb_settings`]), and the edit that uses them stores them first
//! ([`Session::with_vignetting_maps`], called by every commit). The portable form never carries a photo id: ids are this
//! catalog's only, so a map that arrives finds its reference photo by the raw's SHA-1
//! ([`lcc_catalog::reference_photo`]).

use std::collections::BTreeMap;

use lcc_catalog::{Catalog, Op, VignettingMapRecord};
use lcc_develop::{EmbeddedVignettingMap, VignettingMapId};
use serde_json::Value;

use crate::Session;

/// Most maps kept from incoming settings while no edit has used them yet.
const MAX_INCOMING: usize = 64;

/// The ops that store `maps` in `cat`, each one the catalog doesn't have yet, linked to no photo (a map that arrives
/// in portable settings never names one).
pub fn add_ops(cat: &Catalog, maps: &[(VignettingMapId, EmbeddedVignettingMap)], added: &str) -> Vec<Op> {
    let mut seen = std::collections::BTreeSet::new();
    maps.iter()
        .filter(|(id, _)| cat.vignetting_map(id).is_none() && seen.insert(id.clone()))
        .map(|(id, map)| Op::AddVignettingMap {
            id: id.clone(),
            record: Box::new(VignettingMapRecord { map: map.clone(), photo: None, added: added.to_string(), withdrawn: false }),
        })
        .collect()
}

/// The maps whose state `op` sets (a batch's ops' too, a few levels deep).
fn state_ops(op: &Op, out: &mut Vec<VignettingMapId>, depth: usize) {
    match op {
        Op::SetVignettingMapState { id, .. } if !out.contains(id) => out.push(id.clone()),
        Op::Batch { ops } if depth < 8 => ops.iter().for_each(|o| state_ops(o, out, depth + 1)),
        _ => {}
    }
}

impl Session {
    /// Make settings JSON `v` (whole or partial) portable: the map it references, from this catalog, goes with it.
    pub fn portable_settings(&self, v: &mut Value) {
        lcc_develop::embed_portable(v, |id| self.catalog.vignetting_map(id).map(|r| r.map.clone()));
    }

    /// `v` as portable settings: a copy of it with the map.
    pub fn portable(&self, v: &Value) -> Value {
        let mut out = v.clone();
        self.portable_settings(&mut out);
        out
    }

    /// Take the map out of incoming settings JSON `v` (leaving it referenced by id) and keep it until the edit that
    /// uses it is committed. Returns its id.
    pub fn absorb_settings(&mut self, v: &mut Value) -> Option<VignettingMapId> {
        let (_, map) = lcc_develop::absorb_portable(v)?;
        Some(self.offer_vignetting_calibration(map))
    }

    /// Keep `map` (that came in with settings: a paste, a preset, a peer's record) until the edit that uses it is
    /// committed ([`Self::with_vignetting_maps`]). An invalid map is left out. Returns its id.
    pub fn offer_vignetting_calibration(&mut self, map: EmbeddedVignettingMap) -> VignettingMapId {
        let id = VignettingMapId::of(&map);
        if map.is_valid() && self.catalog.vignetting_map(&id).is_none() && !self.incoming_vignetting_maps.contains_key(&id) {
            // (at most a few: the one with the smallest id goes, which is as good as any)
            if self.incoming_vignetting_maps.len() >= MAX_INCOMING
                && let Some(o) = self.incoming_vignetting_maps.keys().next().cloned()
            {
                self.incoming_vignetting_maps.remove(&o);
            }
            self.incoming_vignetting_maps.insert(id.clone(), map);
        }
        id
    }

    /// A step that withdrew a vignetting map or put one back in use (a commit, an undo, a redo) writes that state to
    /// the map's library file too, so other catalogs' Apply follows it. Best effort: a failed write is logged.
    pub(crate) fn vignetting_after(&self, op: &Op) {
        let mut ids = Vec::new();
        state_ops(op, &mut ids, 0);
        if ids.is_empty() {
            return;
        }
        let Some(dir) = self.vignetting_calibration_dir() else { return };
        for id in ids {
            if let Some(r) = self.catalog.vignetting_map(&id)
                && let Err(e) = crate::vignetting_calibration_library::set_withdrawn(&dir, &r.map, r.withdrawn)
            {
                log::warn!("vignetting calibration library: {e}");
            }
        }
    }

    /// The flat-field library's folder ([`crate::vignetting_calibration_library::dir`], unless overridden); `None`: no library.
    pub fn vignetting_calibration_dir(&self) -> Option<std::path::PathBuf> {
        match &self.vignetting_calibration_dir_override {
            Some(d) => d.clone(),
            None => crate::vignetting_calibration_library::dir(),
        }
    }

    /// Settings copied onto photo `id` (Sync, Paste: from `before` to `after`) that bring a vignetting map the photo can't
    /// take (another lens or camera, a zoom at another focal length, an image area the map doesn't cover, not a raw)
    /// keep the photo's own map and amount instead, and say why. A photo that can't be placed (not a raw, or a raw
    /// whose file is offline) keeps what was copied: the map does nothing on the first and fits the second or not once
    /// its file is back.
    pub(crate) fn keep_fitting_vignetting_calibration(
        &self,
        id: lcc_catalog::PhotoId,
        before: &lcc_develop::DevelopSettings,
        after: &mut lcc_develop::DevelopSettings,
    ) -> Option<crate::vignetting_calibration_library::Refusal> {
        use crate::vignetting_calibration_library::Refusal;
        let map_id = after.optics.vignetting_calibration.as_ref().filter(|m| before.optics.vignetting_calibration.as_ref() != Some(*m))?;
        let map = self.catalog.vignetting_map(map_id).map(|r| &r.map).or_else(|| self.incoming_vignetting_maps.get(map_id))?;
        let photo = self.catalog.photo(id)?;
        let why =
            crate::vignetting_calibration_library::match_lookup(map, photo, &self.photo_placement(id)).refused.filter(|w| *w != Refusal::NotRaw)?;
        after.optics.vignetting_calibration = before.optics.vignetting_calibration.clone();
        after.optics.vignetting_calibration_amount = before.optics.vignetting_calibration_amount;
        Some(why)
    }

    /// Where photo `id`'s picture sits on its sensor, for matching a flat-field map to it: read from the raw's header
    /// only ([`crate::vignetting_calibration_placement::read`]), and in the app by a worker thread, never by the caller
    /// ([`crate::vignetting_calibration_placement`]). `Known(None)`: not a raw on disk, or not one this build places.
    pub fn photo_placement(&self, id: lcc_catalog::PhotoId) -> crate::vignetting_calibration_placement::Lookup {
        use crate::vignetting_calibration_placement::Lookup;
        let Some(p) = self.catalog.photo(id) else { return Lookup::Known(None) };
        let lcc_catalog::Source::File { path } = &p.source else { return Lookup::Known(None) };
        if !p.develops_raw() {
            return Lookup::Known(None);
        }
        self.vignetting_calibration_placements.get(id, path)
    }

    /// `op` with the maps it needs stored first: every id its settings reference that the catalog lacks and the
    /// session holds from incoming settings ([`Self::absorb_settings`]) gets an [`Op::AddVignettingMap`] ahead of it, in
    /// one batch. An id nobody has stays as it is (the photo renders without the map and says so).
    pub(crate) fn with_vignetting_maps(&self, op: Op) -> Op {
        if self.incoming_vignetting_maps.is_empty() {
            return op;
        }
        let mut wanted: BTreeMap<VignettingMapId, EmbeddedVignettingMap> = BTreeMap::new();
        op.vignetting_map_ids(&mut |id| {
            if self.catalog.vignetting_map(id).is_none()
                && let Some(m) = self.incoming_vignetting_maps.get(id)
            {
                wanted.insert(id.clone(), m.clone());
            }
        });
        if wanted.is_empty() {
            return op;
        }
        let maps: Vec<(VignettingMapId, EmbeddedVignettingMap)> = wanted.into_iter().collect();
        let mut ops = add_ops(&self.catalog, &maps, &(self.clock)());
        ops.push(op);
        Op::Batch { ops }
    }
}
