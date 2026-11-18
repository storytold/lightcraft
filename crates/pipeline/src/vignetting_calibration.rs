//! A photo's flat-field map ([`lcc_develop::Optics::vignetting_calibration`], Capture One's LCC "Uniform Light") as the geometry
//! stage applies it: [`field_for`] resamples the map from sensor coordinates onto the frame [`crate::optics::Warp`]
//! works in (the source after the EXIF and the user orientation), as a [`FieldGain`] grid that `Warp::gain` and the
//! GPU's `warp_gain` sample. The gain is the same for every colour channel.
//!
//! Sources: own design.

use std::sync::Arc;

use lcc_develop::{DevelopSettings, EmbeddedVignettingMap, VignettingMapId};
use lcc_geom::{Orientation, Point};
use lcc_raw::SensorWindow;
use lcc_raw::vignetting_calibration::{FORMAT_VERSION, MAX_VIGNETTING_CALIBRATION_AMOUNT, VignettingMeasurement};

/// Nodes of a [`FieldGain`] per side.
pub const FIELD_NODES: usize = 64;

/// A flat-field map's gain over the geometry stage's source frame: `FIELD_NODES × FIELD_NODES` values in EV
/// (positive = brighten), node `(0, 0)` at the frame's top-left corner and node `(63, 63)` at its bottom-right
/// corner, so every position on the frame interpolates between nodes.
#[derive(Clone, PartialEq)]
pub struct FieldGain {
    /// Row-major EV per node.
    pub ev: Vec<f32>,
    /// The fraction of the measured correction that applies (0 to 1.2).
    pub amount: f32,
    /// A hash of `ev`, for cache keys (the frame's `Debug` is one).
    pub key: u64,
}

impl std::fmt::Debug for FieldGain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FieldGain({:016x} at {})", self.key, self.amount)
    }
}

impl FieldGain {
    fn new(ev: Vec<f32>, amount: f64) -> FieldGain {
        let mut key: u64 = 0xcbf2_9ce4_8422_2325;
        for b in ev.iter().flat_map(|e| e.to_bits().to_le_bytes()) {
            key ^= u64::from(b);
            key = key.wrapping_mul(0x0100_0000_01b3);
        }
        let amount = if amount.is_finite() { amount.clamp(0.0, MAX_VIGNETTING_CALIBRATION_AMOUNT) } else { 0.0 };
        FieldGain { ev, amount: amount as f32, key }
    }

    /// The correction in EV at full amount at normalised frame position `(u, v)` (0..1; outside: the edge's),
    /// bilinear between nodes.
    pub fn ev_at(&self, u: f64, v: f64) -> f64 {
        let n = FIELD_NODES - 1;
        let at = |t: f64| if t.is_finite() { (t * n as f64).clamp(0.0, n as f64) } else { 0.0 };
        let (x, y) = (at(u), at(v));
        let (x0, y0) = ((x.floor() as usize).min(n - 1), (y.floor() as usize).min(n - 1));
        let (fx, fy) = (x - x0 as f64, y - y0 as f64);
        let e = |c: usize, r: usize| f64::from(self.ev.get(r * FIELD_NODES + c).copied().unwrap_or(0.0));
        let top = e(x0, y0) + (e(x0 + 1, y0) - e(x0, y0)) * fx;
        let bottom = e(x0, y0 + 1) + (e(x0 + 1, y0 + 1) - e(x0, y0 + 1)) * fx;
        top + (bottom - top) * fy
    }

    /// The gain at source position `src` (px) of a `w × h` frame, at this field's amount.
    pub fn gain(&self, src: Point, w: f64, h: f64) -> f64 {
        let (u, v) = (if w > 0.0 { src.x / w } else { 0.0 }, if h > 0.0 { src.y / h } else { 0.0 });
        (f64::from(self.amount) * self.ev_at(u, v)).exp2()
    }
}

/// The develop settings' map as `lcc_raw` measures maps (`None` when it isn't a valid one).
pub fn map_of(e: &EmbeddedVignettingMap) -> Option<VignettingMeasurement> {
    let gains = (0..e.ev.0.len()).map(|i| e.ev_of(i).exp2() as f32).collect();
    let window = e.window.map(|[sw, sh, x, y, w, h]| SensorWindow { sensor: [sw, sh], origin: [x, y], size: [w, h] });
    let o = &e.source;
    let source = lcc_raw::vignetting_calibration::VignettingCalibrationSource {
        make: o.make.clone(),
        model: o.model.clone(),
        serial: o.serial.clone(),
        lens: o.lens.clone(),
        focal_length: o.focal_length,
        aperture: o.aperture,
        sha1: o.sha1.clone(),
    };
    let m = VignettingMeasurement { version: FORMAT_VERSION, source, sensor: e.sensor, grid: e.grid, gains, window };
    m.validate().ok().map(|()| m)
}

/// A measured map in the develop settings' form (`None` when it doesn't fit it).
pub fn embed(m: &VignettingMeasurement) -> Option<EmbeddedVignettingMap> {
    m.validate().ok()?;
    let ev: Vec<f64> = m.gains.iter().map(|g| f64::from(*g).max(1.0).log2()).collect();
    let window = m.window.map(|w| [w.sensor[0], w.sensor[1], w.origin[0], w.origin[1], w.size[0], w.size[1]]);
    let s = &m.source;
    let source = lcc_develop::VignettingMapOrigin {
        make: s.make.clone(),
        model: s.model.clone(),
        serial: s.serial.clone(),
        lens: s.lens.clone(),
        focal_length: s.focal_length,
        aperture: s.aperture,
        sha1: s.sha1.clone(),
        // (the file name and capture date are the caller's to add: a measured map doesn't know them)
        ..Default::default()
    };
    EmbeddedVignettingMap::from_ev(m.sensor, m.grid, window, source, &ev)
}

/// Where a render finds the flat-field maps the settings reference by id (the engine reads them from the catalog),
/// as [`crate::genpatch::PatchSource`] does for generative fill.
pub trait VignettingCalibrationSource: Send + Sync {
    /// The map with this id; `None` when the catalog doesn't have it.
    fn map(&self, id: &VignettingMapId) -> Option<Arc<EmbeddedVignettingMap>>;
}

/// The maps a source's renders can use ([`crate::SourceInfo::vignetting_maps`]).
#[derive(Clone)]
pub struct VignettingMapStore(pub Arc<dyn VignettingCalibrationSource>);

impl std::fmt::Debug for VignettingMapStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VignettingMapStore")
    }
}

impl PartialEq for VignettingMapStore {
    fn eq(&self, o: &Self) -> bool {
        std::ptr::addr_eq(Arc::as_ptr(&self.0), Arc::as_ptr(&o.0))
    }
}

/// A fixed set of maps (tests, tools, a single map handed in).
impl VignettingCalibrationSource for std::collections::BTreeMap<VignettingMapId, Arc<EmbeddedVignettingMap>> {
    fn map(&self, id: &VignettingMapId) -> Option<Arc<EmbeddedVignettingMap>> {
        self.get(id).cloned()
    }
}

impl VignettingMapStore {
    /// A store of these maps, each under its own id.
    pub fn of(maps: impl IntoIterator<Item = EmbeddedVignettingMap>) -> VignettingMapStore {
        let m: std::collections::BTreeMap<VignettingMapId, Arc<EmbeddedVignettingMap>> =
            maps.into_iter().map(|m| (VignettingMapId::of(&m), Arc::new(m))).collect();
        VignettingMapStore(Arc::new(m))
    }
}

/// The flat-field gain of settings `s` over a source with facts `info`, in the frame the geometry stage samples (the
/// source after the EXIF and then the `user` orientation). `None` when nothing applies: no map in the settings, the
/// Optics section switched off, the map not in `info`'s store (missing from the catalog), no placement (not a raw),
/// or a map that doesn't cover the photo (another sensor, or an image area it wasn't measured over).
pub fn field_for(info: &crate::SourceInfo, s: &DevelopSettings, user: Orientation) -> Option<Arc<FieldGain>> {
    if !s.section_enabled("optics") {
        return None;
    }
    let id = s.optics.vignetting_calibration.as_ref()?;
    let stored = info.vignetting_maps.as_ref()?.0.map(id)?;
    let map = map_of(&stored)?;
    let placement = info.placement.as_ref()?;
    let n = (FIELD_NODES - 1) as f64;
    let inv = user.inverse();
    let mut ev = Vec::with_capacity(FIELD_NODES * FIELD_NODES);
    for r in 0..FIELD_NODES {
        for c in 0..FIELD_NODES {
            let (u, v) = inv.map(c as f64 / n, r as f64 / n, 1.0, 1.0);
            let (x, y) = map.sensor_at(placement, u, v)?;
            ev.push(map.ev_at(x, y) as f32);
        }
    }
    Some(Arc::new(FieldGain::new(ev, s.optics.vignetting_calibration_amount / 100.0)))
}

#[cfg(test)]
#[path = "tests_vignetting_calibration.rs"]
mod tests;
