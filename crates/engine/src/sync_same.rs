//! Auto Sync carries the active photo's VALUE to every selected photo, as Lightroom Classic does
//! (test notes 2026-10-24 note 18, the user's rule "same value for all synced values"). Where white
//! balance is a mode rather than a number, the mode travels and each photo applies it to itself:
//! As Shot gives each photo its own camera white, Auto each photo's own white from its pixels, a
//! light preset its numbers.
//!
//! A typed or dragged Temp or Tint is set on each photo's own scale: a raw edited in Kelvin gets
//! the same Kelvin (an old relative-scale value converted first, never read as Kelvin, which turned
//! photos blue: test notes 2026-10-15), and only the slider that moved changes (a Temp move keeps
//! each photo's own tint). A photo on another scale than the active one (a JPEG next to a raw) has
//! no "same" number and moves by as much as the active photo instead.

use std::collections::HashMap;

use lightcraft_catalog::PhotoId;
use lightcraft_develop::{DevelopSettings, WbMode};
use lightcraft_pipeline::SourceInfo;

use crate::Session;

/// White balance on this source is edited in Kelvin (a raw with a measured camera scale), not on
/// the relative scale (6500 K / 0 = as shot).
fn kelvin(info: &SourceInfo) -> bool {
    info.raw && !info.relative_wb
}

/// The temperature and tint `mode` stands for on a source with `info` (As Shot and the light
/// presets; `None` for Auto and Custom, which need the pixels or the numbers).
fn wb_of_mode(info: &SourceInfo, mode: WbMode) -> Option<(f64, f64)> {
    match mode {
        WbMode::AsShot => Some((info.as_shot_temp, info.as_shot_tint)),
        m => m.preset().map(|(t, ti)| {
            // presets are Kelvin on raws with that scale, relative to daylight (6500 K = as shot) elsewhere
            if kelvin(info) { (t, ti) } else { (6500.0 * t / 5500.0, ti) }
        }),
    }
}

/// `synced.wb` for a photo (source `oinfo`, settings `own` before the sync) when the active
/// photo's white balance went from `old` to `new` (source `ainfo`). `auto`: the photo's own Auto
/// white balance, when `new` is Auto (`None`: it couldn't be worked out, and the photo keeps its
/// white balance).
pub(crate) fn synced_wb(
    ainfo: &SourceInfo,
    old: &DevelopSettings,
    new: &DevelopSettings,
    oinfo: &SourceInfo,
    own: &DevelopSettings,
    auto: Option<(f64, f64)>,
    synced: &mut DevelopSettings,
) {
    use lightcraft_pipeline::local::{effective_wb, normalize_wb};
    let mut d = own.clone();
    normalize_wb(&mut d, oinfo);
    let mode = new.wb.mode;
    let resolved = match mode {
        WbMode::Auto => auto,
        WbMode::Custom => None,
        m => wb_of_mode(oinfo, m),
    };
    match (mode, resolved) {
        (WbMode::Custom, _) => {
            let (ot, oti) = effective_wb(oinfo, own);
            let (at0, ati0) = effective_wb(ainfo, old);
            let (at, ati) = effective_wb(ainfo, new);
            let same_scale = kelvin(ainfo) == kelvin(oinfo);
            // only what moved: a Temp move leaves each photo's own tint
            let pick = |own: f64, was: f64, now: f64| {
                if (now - was).abs() < 1e-9 {
                    own
                } else if same_scale {
                    now
                } else {
                    own + (now - was)
                }
            };
            d.wb.temp = pick(ot, at0, at);
            d.wb.tint = pick(oti, ati0, ati);
            d.wb.mode = WbMode::Custom;
        }
        (m, Some((t, ti))) => {
            d.wb.temp = t;
            d.wb.tint = ti;
            d.wb.mode = m;
        }
        // Auto couldn't be worked out on this photo (its pixels didn't load): it keeps its own
        (_, None) => {
            synced.wb = own.wb;
            return;
        }
    }
    let (t, ti) = (d.wb.temp, d.wb.tint);
    lightcraft_develop::controls::set(&mut d, "wb.temp", t);
    lightcraft_develop::controls::set(&mut d, "wb.tint", ti);
    synced.wb = d.wb;
}

impl Session {
    /// Each other selected photo's own Auto white balance, worked out on its own pixels, when
    /// Auto Sync is about to carry the active photo's (`id`) switch to Auto (`new`) over. Empty
    /// otherwise. A photo whose pixels don't load is left out (it keeps its white balance).
    pub(crate) fn own_auto_wb(&mut self, id: PhotoId, new: &DevelopSettings, label: &str) -> HashMap<PhotoId, (f64, f64)> {
        let mut own = HashMap::new();
        if !self.auto_sync_applies(id, label) || new.wb.mode != WbMode::Auto || self.develop_of(id).is_some_and(|old| old.wb == new.wb) {
            return own;
        }
        let others: Vec<PhotoId> = self.selection.ids.iter().copied().filter(|x| *x != id).collect();
        // decoded in parallel, a core's worth at a time: a big selection never holds more
        // decoded thumbnails at once than that, whatever the cache keeps
        let batch = rayon::current_num_threads().clamp(1, 16);
        for chunk in others.chunks(batch) {
            for (x, src) in self.sources_now(chunk, crate::media::SourceLevel::Thumb) {
                match src {
                    // with the facts its own decoder gave
                    Ok((src, info)) => {
                        own.insert(x, lightcraft_pipeline::auto::auto_wb(&src, &info));
                    }
                    Err(e) => log::warn!("Auto Sync: Auto white balance on photo {}: {e}", x.0),
                }
            }
        }
        own
    }
}
