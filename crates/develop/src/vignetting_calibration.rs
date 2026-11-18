//! Flat-field maps (Capture One's LCC "Uniform Light"), by reference. A map is measured from a reference shot through
//! a diffuser (`lcc_raw::vignetting_calibration`) and stored once per catalog, keyed by its [`VignettingMapId`] (the SHA-256 of its
//! canonical JSON). A photo's settings hold only that id ([`crate::Optics::vignetting_calibration`]) and the amount
//! ([`crate::Optics::vignetting_calibration_amount`]), so History steps, Versions, copy, paste and sync within a catalog carry the
//! reference, not the map. The pipeline gets the map from the catalog through the source's facts.
//!
//! Where settings leave the catalog (sidecars, presets, the clipboard, the control channel), the map travels with
//! them in their *portable form*: the settings JSON with the map under `optics.vignetting_calibration_map`
//! ([`embed_portable`]); reading them takes it out again ([`absorb_portable`]) for the catalog to store. A map is
//! self-contained (grid, sensor, window, the reference shot's facts): nothing ever reads the reference raw again.
//!
//! The gains are stored as their log2 (EV), quantised to 1/4096 EV in unsigned 16-bit little-endian codes, base64:
//! a 96 × 64 grid is about 16 KB of JSON. A map that doesn't read back whole (bad base64, the wrong number of codes,
//! an impossible grid or window) is no map.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;

/// Where the portable form of settings carries the map: `optics.vignetting_calibration_map` ([`embed_portable`]).
pub const PORTABLE_MAP_KEY: &str = "vignetting_calibration_map";

/// A flat-field map's id: the SHA-256 of its canonical JSON (the serialized [`EmbeddedVignettingMap`]), 64 lower-case
/// hex digits. One id names exactly one map, so a photo's settings (and their `hash64`) say which map renders.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct VignettingMapId(String);

impl VignettingMapId {
    /// The id of `map`.
    pub fn of(map: &EmbeddedVignettingMap) -> VignettingMapId {
        let bytes = serde_json::to_vec(map).unwrap_or_default();
        let digest = sha2::Sha256::digest(&bytes);
        VignettingMapId(digest.iter().map(|b| format!("{b:02x}")).collect())
    }

    /// `text` as an id, when it is one (64 lower-case hex digits).
    pub fn parse(text: &str) -> Option<VignettingMapId> {
        (text.len() == 64 && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))).then(|| VignettingMapId(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first eight digits (file names, logs).
    pub fn short(&self) -> &str {
        self.0.get(..8).unwrap_or(&self.0)
    }
}

impl<'de> Deserialize<'de> for VignettingMapId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        VignettingMapId::parse(&s).ok_or_else(|| serde::de::Error::custom("not a vignetting map id"))
    }
}

impl std::fmt::Display for VignettingMapId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The format this build writes and reads.
pub const EMBEDDED_VIGNETTING_CALIBRATION_VERSION: u32 = 1;
/// Fewest and most cells per side of a map's grid (as `lcc_raw::vignetting_calibration` measures them).
pub const MIN_VIGNETTING_CALIBRATION_GRID: usize = 2;
pub const MAX_VIGNETTING_CALIBRATION_GRID: usize = 256;
/// Codes per EV.
pub const VIGNETTING_CALIBRATION_EV_STEP: f64 = 4096.0;
/// Largest sensor side a map may name, in pixels.
const MAX_SENSOR_SIDE: usize = 1 << 20;
/// Longest text a map's source fields may hold.
const MAX_TEXT: usize = 256;
/// Longest base64 string of codes read: a 256 × 256 grid's (anything longer is refused before decoding).
const MAX_CODES_B64: usize = (MAX_VIGNETTING_CALIBRATION_GRID * MAX_VIGNETTING_CALIBRATION_GRID * 2).div_ceil(3) * 4;

/// The default amount, % of the measured correction.
pub const DEFAULT_VIGNETTING_CALIBRATION_PERCENT: f64 = 100.0;

/// What the reference shot was, as its file said. Informational: it's shown with the map and used to suggest maps,
/// never to refuse one (the sensor geometry decides that).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VignettingMapOrigin {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub make: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lens: Option<String>,
    /// Millimetres.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focal_length: Option<f64>,
    /// F-number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aperture: Option<f64>,
    /// SHA-1 of the reference file (lower-case hex): how the catalog finds the reference photo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
    /// The reference file's name (no folder).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    /// When the reference was taken (ISO 8601, as the photo's metadata says).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured: Option<String>,
}

impl VignettingMapOrigin {
    pub fn is_empty(&self) -> bool {
        *self == VignettingMapOrigin::default()
    }

    fn is_valid(&self) -> bool {
        let text_ok = [&self.make, &self.model, &self.serial, &self.lens, &self.sha1, &self.file_name, &self.captured]
            .iter()
            .all(|t| t.as_ref().is_none_or(|t| t.len() <= MAX_TEXT));
        let num_ok = [self.focal_length, self.aperture].iter().all(|n| n.is_none_or(|n| n.is_finite() && n > 0.0));
        text_ok && num_ok
    }
}

/// A flat-field map as the catalog stores it (keyed by [`VignettingMapId::of`]) and the portable form of settings carries
/// it. Sensor coordinates are pixels of the reference's active area, unrotated, as in `lcc_raw::vignetting_calibration`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EmbeddedVignettingMap {
    pub version: u32,
    /// Width and height of the active area the grid spans, in sensor pixels.
    pub sensor: [usize; 2],
    /// Cells across and down.
    pub grid: [usize; 2],
    /// Where that active area sits on the camera's full sensor, when the reference's file said: full sensor width
    /// and height, origin x and y, size width and height (the size is [`Self::sensor`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<[usize; 6]>,
    #[serde(default, skip_serializing_if = "VignettingMapOrigin::is_empty")]
    pub source: VignettingMapOrigin,
    /// `grid[0] · grid[1]` gains, row-major, as EV codes ([`VIGNETTING_CALIBRATION_EV_STEP`] per EV, 0 = no gain).
    pub ev: EvCodes,
}

/// The gain codes of a map: serialized as base64 of little-endian `u16`s.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct EvCodes(pub Vec<u16>);

impl std::fmt::Debug for EvCodes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EvCodes({} codes)", self.0.len())
    }
}

impl Serialize for EvCodes {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let bytes: Vec<u8> = self.0.iter().flat_map(|c| c.to_le_bytes()).collect();
        s.serialize_str(&crate::segmask::base64_encode(&bytes))
    }
}

impl<'de> Deserialize<'de> for EvCodes {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let s = String::deserialize(d)?;
        if s.len() > MAX_CODES_B64 {
            return Err(D::Error::custom("too many vignetting map codes"));
        }
        let bytes = crate::segmask::base64_decode(&s).ok_or_else(|| D::Error::custom("vignetting map codes aren't base64"))?;
        if bytes.len() % 2 != 0 {
            return Err(D::Error::custom("an odd number of vignetting map code bytes"));
        }
        Ok(EvCodes(bytes.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect()))
    }
}

impl EmbeddedVignettingMap {
    /// A map from its gains in EV (row-major, `grid[0] · grid[1]` of them; negative and non-finite values count as
    /// no gain, values past what a code holds as its largest). `None` when the result isn't a valid map.
    pub fn from_ev(sensor: [usize; 2], grid: [usize; 2], window: Option<[usize; 6]>, source: VignettingMapOrigin, ev: &[f64]) -> Option<Self> {
        let codes = ev
            .iter()
            .map(|e| if e.is_finite() { (e.max(0.0) * VIGNETTING_CALIBRATION_EV_STEP).round().min(f64::from(u16::MAX)) as u16 } else { 0 })
            .collect();
        let m = EmbeddedVignettingMap { version: EMBEDDED_VIGNETTING_CALIBRATION_VERSION, sensor, grid, window, source, ev: EvCodes(codes) };
        m.is_valid().then_some(m)
    }

    /// The gain of cell `i` in EV (0 past the grid).
    pub fn ev_of(&self, i: usize) -> f64 {
        self.ev.0.get(i).map_or(0.0, |c| f64::from(*c) / VIGNETTING_CALIBRATION_EV_STEP)
    }

    /// Whether the map can be used: this format, a grid of 2 to 256 cells per side over at least as many pixels, one
    /// code per cell, a window (when there is one) inside its sensor and the size of this map's active area, and
    /// source fields of sane size.
    pub fn is_valid(&self) -> bool {
        let [cols, rows] = self.grid;
        let [w, h] = self.sensor;
        let grid_ok = (MIN_VIGNETTING_CALIBRATION_GRID..=MAX_VIGNETTING_CALIBRATION_GRID).contains(&cols)
            && (MIN_VIGNETTING_CALIBRATION_GRID..=MAX_VIGNETTING_CALIBRATION_GRID).contains(&rows);
        let sensor_ok = w >= cols && h >= rows && w <= MAX_SENSOR_SIDE && h <= MAX_SENSOR_SIDE;
        let window_ok = self.window.is_none_or(|[sw, sh, x, y, ww, wh]| {
            sw <= MAX_SENSOR_SIDE
                && sh <= MAX_SENSOR_SIDE
                && [ww, wh] == self.sensor
                && x.checked_add(ww).is_some_and(|r| r <= sw)
                && y.checked_add(wh).is_some_and(|b| b <= sh)
        });
        self.version == EMBEDDED_VIGNETTING_CALIBRATION_VERSION
            && grid_ok
            && sensor_ok
            && cols * rows == self.ev.0.len()
            && window_ok
            && self.source.is_valid()
    }
}

/// A map read from untrusted JSON (a sidecar, a preset, a peer): `None` unless it reads back whole and is valid.
pub fn map_from_json(v: &Value) -> Option<EmbeddedVignettingMap> {
    serde_json::from_value::<EmbeddedVignettingMap>(v.clone()).ok().filter(EmbeddedVignettingMap::is_valid)
}

/// Make settings JSON `v` (whole or partial) portable: when it references a map that `lookup` knows, put the map at
/// `optics.vignetting_calibration_map`. Readers that don't know the key ignore it, as they ignore any unknown key.
pub fn embed_portable(v: &mut Value, lookup: impl FnOnce(&VignettingMapId) -> Option<EmbeddedVignettingMap>) {
    let Some(optics) = v.get_mut("optics").and_then(Value::as_object_mut) else { return };
    let Some(id) = optics.get("vignetting_calibration").and_then(Value::as_str).and_then(VignettingMapId::parse) else { return };
    if let Some(map) = lookup(&id).and_then(|m| serde_json::to_value(m).ok()) {
        optics.insert(PORTABLE_MAP_KEY.into(), map);
    }
}

/// Take the map out of portable settings JSON `v` ([`embed_portable`]): `v` is left referencing it by id, and the map
/// is returned for the catalog to store. The id is always the map's own: a portable form whose id disagrees with
/// its map (edited by hand, or damaged) gets the map's id. A map that doesn't read back whole is dropped; the id
/// stays, and resolves if the catalog already has that map.
pub fn absorb_portable(v: &mut Value) -> Option<(VignettingMapId, EmbeddedVignettingMap)> {
    let optics = v.get_mut("optics").and_then(Value::as_object_mut)?;
    let raw = optics.remove(PORTABLE_MAP_KEY)?;
    let map = map_from_json(&raw)?;
    let id = VignettingMapId::of(&map);
    optics.insert("vignetting_calibration".into(), Value::from(id.as_str()));
    Some((id, map))
}

/// The flat-field rule for partial settings (presets, `develop.merge`, `render --settings`): a partial naming a map
/// but no amount sets the amount to the default. Returns the partial as it should be merged.
pub(crate) fn complete_partial(partial: &Value) -> std::borrow::Cow<'_, Value> {
    let names_map = partial.get("optics").and_then(|o| o.get("vignetting_calibration")).is_some_and(|m| !m.is_null());
    let names_amount = partial.get("optics").and_then(|o| o.get("vignetting_calibration_amount")).is_some();
    if !names_map || names_amount {
        return std::borrow::Cow::Borrowed(partial);
    }
    let mut p = partial.clone();
    if let Some(o) = p.get_mut("optics").and_then(Value::as_object_mut) {
        o.insert("vignetting_calibration_amount".into(), Value::from(DEFAULT_VIGNETTING_CALIBRATION_PERCENT));
    }
    std::borrow::Cow::Owned(p)
}

#[cfg(test)]
#[path = "vignetting_calibration_tests.rs"]
mod tests;
