//! What the app remembers about each camera body's sensor: its hot, dead and stuck pixels, and the dust on it.
//!
//! **Where:** one JSON file per body in [`dir`] (`<PREFIX>_SENSOR_MAPS`, else `<config>/sensor-maps`), like the
//! camera profiles: a sensor belongs to a camera body, not to a library, so a map survives new libraries, re-imports
//! and removed folders, and one body shooting into several libraries builds one map. The files hold positions,
//! counts, dates and exposure figures — never image content. A damaged or hostile file is set aside (renamed
//! `.bad`) with a warning and a new map starts.
//!
//! **Body:** [`body_key`] — the body's serial number when the file has one, else `file:XX` from the first two
//! characters of the file name (as `feat/camera-profiling` names bodies, so the two agree). A map also records its
//! camera model and the raw data size, and is only used for files of that model and size.
//!
//! **Pixels:** every raw decode looks for defects at the learning threshold (`lcc_raw::defects`) and the
//! candidates are recorded here once per shot (a fingerprint of capture time, exposure and samples). A pixel seen
//! in [`CONFIRM_HITS`] photos from [`CONFIRM_BUCKETS`] different 5-minute periods (different scenes) is confirmed,
//! and every photo of the body has it corrected. Dark frames confirm at once. Entries expire: a one-off candidate not
//! seen again in [`SINGLETON_TTL`] photos, a confirmed one missed in many photos taken at exposures where it showed.
//!
//! **Dust:** spots seen in photos of the body (sensor coordinates: the developed frame before any rotation, 0..1),
//! matched across photos, with time windows (dust moves when lenses change or the sensor cleans itself) and its size
//! and strength per aperture. See `docs/sensor-defects.md`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};

use lcc_raw::defects::{Defect, DefectKind, Fix};
use serde::{Deserialize, Serialize};

/// Format version of the map files.
pub const VERSION: u32 = 1;
/// Largest map file read.
const MAX_FILE: u64 = 32 << 20;
/// Most pixel entries a map keeps (one-off candidates go first).
pub const MAX_PIXELS: usize = 50_000;
/// Most dust spots a map keeps.
pub const MAX_DUST: usize = 2_000;
/// Shot fingerprints remembered (so a photo decoded again is not counted again).
const MAX_SEEN: usize = 4_096;
/// A pixel is confirmed after this many photos ...
pub const CONFIRM_HITS: u32 = 3;
/// ... from this many different capture periods.
pub const CONFIRM_BUCKETS: u32 = 3;
/// A capture period, seconds.
pub const BUCKET_SECS: i64 = 300;
/// A one-off candidate not seen again in this many analysed photos is dropped.
pub const SINGLETON_TTL: u32 = 300;
/// A confirmed pixel missed in at least this many photos (and 20 × its hits) taken in conditions where it showed is
/// dropped (a camera service remapped it, or it recovered).
pub const EXPIRE_MISSES: u32 = 150;
/// Dust: a spot is confirmed after this many photos ...
pub const DUST_CONFIRM_HITS: u32 = 3;
/// ... from this many capture periods within one window.
pub const DUST_CONFIRM_BUCKETS: u32 = 2;
/// Photos where a dust spot could have shown and didn't, before its window closes.
pub const DUST_WINDOW_MISSES: u32 = 3;
/// A window covers photos up to this long before its first and after its last sighting (seconds).
pub const DUST_WINDOW_SLACK: i64 = 3600;

/// The key a camera body is known by: its serial number when the file has one; else `file:XX`, the first two
/// characters of the file name after any leading `_` (`_SA94454.ARW` → `file:SA`, as photographers often name files
/// per body); else `None`. (The same rule as camera profiles per body.)
pub fn body_key(serial: Option<&str>, file_name: Option<&str>) -> Option<String> {
    if let Some(s) = serial.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(s.to_string());
    }
    let name = std::path::Path::new(file_name?).file_stem()?.to_str()?.trim().trim_start_matches('_');
    let two: String = name.chars().take(2).collect();
    (two.chars().count() == 2 && two.chars().all(|c| c.is_ascii_alphanumeric())).then(|| format!("file:{two}"))
}

/// How a body reads to people: a file-name key's two characters (`file:SA` → `SA`), else `…` and the serial's last
/// four.
pub fn body_label(body: &str) -> String {
    if let Some(k) = body.strip_prefix("file:") {
        return k.to_string();
    }
    let s: Vec<char> = body.chars().filter(|c| !c.is_whitespace()).collect();
    format!("…{}", s.get(s.len().saturating_sub(4)..).map(|t| t.iter().collect::<String>()).unwrap_or_default())
}

/// Where the maps are kept (`None`: nowhere — maps live in memory for this run only).
pub fn dir() -> Option<PathBuf> {
    #[cfg(test)]
    {
        Some(test_dir().clone())
    }
    #[cfg(not(test))]
    {
        lcc_brand::env_os("SENSOR_MAPS").map(PathBuf::from).or_else(|| crate::config::config_dir().map(|d| d.join("sensor-maps")))
    }
}

/// Under test, the store of this test process, `<temp>/lc-sensor-maps-test-<pid>`, shared by its tests. Nothing
/// runs when a test process ends (a static's drop never does), so the first use in each process removes the
/// stores of earlier test processes left unchanged for six hours, which keeps runs from piling them up in Temp
/// (no test process runs that long, so a live one's store is never taken).
#[cfg(test)]
fn test_dir() -> &'static PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let temp = std::env::temp_dir();
        let own = format!("lc-sensor-maps-test-{}", std::process::id());
        let old = |e: &std::fs::DirEntry| {
            e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() > 6 * 3600)
        };
        for e in std::fs::read_dir(&temp).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("lc-sensor-maps-test-") && name != own && old(&e) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
        temp.join(own)
    })
}

/// File name of a body's map: letters, digits, `-` and `_` kept, anything else `_`.
pub fn file_name(body: &str) -> String {
    let name: String = body.trim().chars().take(80).map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    format!("{name}.json")
}

/// Where a pixel entry came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PixelSource {
    /// Seen in ordinary photos.
    #[default]
    Photos,
    /// Measured on a dark frame.
    DarkFrame,
}

/// One defective sample of a body's sensor (full raw data coordinates).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PixelEntry {
    pub x: u32,
    pub y: u32,
    #[serde(default)]
    pub s: u8,
    pub kind: DefectKind,
    #[serde(default)]
    pub source: PixelSource,
    /// Photos it was found in, and in how many capture periods.
    pub hits: u32,
    pub buckets: u32,
    #[serde(default)]
    pub last_bucket: i64,
    /// The first few capture periods it was found in (distinct; enough to count up to confirmation whatever order
    /// photos are analysed in).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub periods: Vec<i64>,
    /// Photos analysed since it was last found.
    #[serde(default)]
    pub since: u32,
    /// Of those, photos taken within its exposure and ISO range (where it should have shown).
    #[serde(default)]
    pub misses: u32,
    /// Capture times (ISO 8601) of the first and last photo it was found in.
    #[serde(default)]
    pub first: String,
    #[serde(default)]
    pub last: String,
    /// Exposure times (seconds) and ISO speeds it was found at.
    #[serde(default)]
    pub exp_min: Option<f64>,
    #[serde(default)]
    pub exp_max: Option<f64>,
    #[serde(default)]
    pub iso_min: Option<u32>,
    #[serde(default)]
    pub iso_max: Option<u32>,
    /// Typical excess over its neighbours (normalised, 1 = white).
    #[serde(default)]
    pub level: f32,
    pub confirmed: bool,
}

/// A dust spot's appearance at one aperture.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApertureLook {
    /// f-number (rounded to a third of a stop).
    pub f: f32,
    /// Radius (fraction of the long edge) and darkening (absolute and relative) seen at it.
    pub radius: f64,
    pub strength: f32,
    #[serde(default)]
    pub dip: f32,
    pub n: u32,
}

/// A stretch of time a dust spot sat in one place.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DustWindow {
    /// Capture times (ISO 8601 and seconds) of the first and last photo it was seen in.
    pub first: String,
    pub last: String,
    pub t0: i64,
    pub t1: i64,
    pub hits: u32,
    pub buckets: u32,
    #[serde(default)]
    pub last_bucket: i64,
    /// The first few distinct capture periods it was seen in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub periods: Vec<i64>,
    /// Photos since the last sighting where it could have shown (smooth, bright place, aperture small enough) and
    /// didn't; at [`DUST_WINDOW_MISSES`] the window closes.
    pub misses: u32,
    pub closed: bool,
}

/// A dust spot on a body's sensor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DustEntry {
    /// Centre in the sensor frame (the developed area before rotation, 0..1 of its width and height).
    pub x: f64,
    pub y: f64,
    pub windows: Vec<DustWindow>,
    pub apertures: Vec<ApertureLook>,
    pub confirmed: bool,
}

impl DustEntry {
    /// Its look at f-number `f`: the nearest aperture seen, scaled (a wider aperture blurs the shadow wider and
    /// fainter: radius ∝ √(N_ref / N), darkening the other way). `None` before it was ever seen.
    pub fn look_at(&self, f: Option<f32>) -> Option<(f64, f32, f32)> {
        let f = f.filter(|f| f.is_finite() && *f > 0.0);
        let near = match f {
            Some(f) => self.apertures.iter().min_by(|a, b| (a.f.ln() - f.ln()).abs().total_cmp(&(b.f.ln() - f.ln()).abs()))?,
            None => self.apertures.iter().max_by_key(|a| a.n)?,
        };
        let k = f.map_or(1.0, |f| f64::from(near.f / f).sqrt().clamp(0.4, 2.5));
        Some((near.radius * k, near.strength / k as f32, near.dip / k as f32))
    }

    /// The smallest f-number it was seen at (it can show at that and smaller apertures).
    pub fn widest_seen(&self) -> Option<f32> {
        self.apertures.iter().map(|a| a.f).filter(|f| f.is_finite()).reduce(f32::min)
    }

    /// The window covering a photo taken at `t` (seconds), if any: inside one (with [`DUST_WINDOW_SLACK`]), or
    /// after the last one while nothing has shown it gone.
    pub fn window_at(&self, t: Option<i64>) -> Option<&DustWindow> {
        let Some(t) = t else { return self.windows.last().filter(|w| !w.closed) };
        self.windows
            .iter()
            .find(|w| t >= w.t0.saturating_sub(DUST_WINDOW_SLACK) && t <= w.t1.saturating_add(DUST_WINDOW_SLACK))
            .or_else(|| self.windows.last().filter(|w| !w.closed && t > w.t1))
    }
}

/// Everything remembered about one body's sensor.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BodyMap {
    pub version: u32,
    pub body: String,
    pub make: String,
    pub model: String,
    /// Full raw data size the pixel coordinates are in.
    pub sensor: [usize; 2],
    /// Photos analysed for pixels.
    pub photos: u32,
    pub seen: Vec<u64>,
    pub pixels: Vec<PixelEntry>,
    /// Aspect (width / height) of the developed frame the dust coordinates are in.
    pub aspect: f64,
    pub dust_photos: u32,
    pub dust_seen: Vec<u64>,
    pub dust: Vec<DustEntry>,
}

/// One analysed photo, as far as the maps care.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shot {
    /// Tells photos apart ([`lcc_raw::defects::sample_fingerprint`] mixed with the capture facts).
    pub fingerprint: u64,
    /// ISO 8601 capture time.
    pub captured: Option<String>,
    pub exposure: Option<f64>,
    pub iso: Option<u32>,
    pub f_number: Option<f32>,
}

impl Shot {
    /// Capture time in seconds (local wall clock).
    pub fn time(&self) -> Option<i64> {
        self.captured.as_deref().and_then(|c| lcc_catalog::dates::iso_seconds(c.get(..19).unwrap_or(c)))
    }

    /// Its capture period (a photo without a time is a period of its own).
    fn bucket(&self) -> i64 {
        self.time().map_or(self.fingerprint as i64 | i64::MIN, |t| t.div_euclid(BUCKET_SECS))
    }
}

/// A dust spot found on one photo, in sensor coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DustSeen {
    pub x: f64,
    pub y: f64,
    pub radius: f64,
    pub strength: f32,
    pub dip: f32,
}

/// What a known dust position looks like on a photo (see `lcc_pipeline::dust::Probe`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DustCheck {
    pub dip: f32,
    pub testable: bool,
}

fn round_f(f: f32) -> f32 {
    if !(f.is_finite() && f > 0.0) {
        return 0.0;
    }
    let k = (6.0 * f.log2()).round();
    (2f32.powf(k / 6.0) * 10.0).round() / 10.0
}

impl BodyMap {
    pub fn new(body: &str, make: &str, model: &str, sensor: [usize; 2]) -> BodyMap {
        BodyMap { version: VERSION, body: body.into(), make: make.into(), model: model.into(), sensor, ..Default::default() }
    }

    /// Whether the map describes a sensor of this model and size (an empty map takes any).
    pub fn fits(&self, model: &str, sensor: [usize; 2]) -> bool {
        (self.model.is_empty() || self.model == model) && (self.sensor == [0, 0] || self.sensor == sensor)
    }

    /// The samples every photo of the body has corrected.
    pub fn fixes(&self) -> Vec<Fix> {
        self.pixels.iter().filter(|p| p.confirmed).map(|p| Fix { x: p.x, y: p.y, s: p.s }).collect()
    }

    fn confirmed_set(&self) -> u64 {
        let mut h: u64 = 0x9e37_79b9_7f4a_7c15;
        for p in self.pixels.iter().filter(|p| p.confirmed) {
            h = h.rotate_left(5) ^ (u64::from(p.x) << 24 | u64::from(p.y) << 2 | u64::from(p.s));
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h ^ self.pixels.iter().filter(|p| p.confirmed).count() as u64
    }

    fn remember(seen: &mut Vec<u64>, fingerprint: u64) -> bool {
        if seen.contains(&fingerprint) {
            return false;
        }
        seen.push(fingerprint);
        if seen.len() > MAX_SEEN {
            let drop = seen.len() - MAX_SEEN;
            seen.drain(..drop);
        }
        true
    }

    /// Record one photo's pixel candidates. `false` when this shot was counted already. Confirmations, expiry and
    /// the size cap are applied.
    pub fn record_pixels(&mut self, shot: &Shot, found: &[Defect]) -> bool {
        if !Self::remember(&mut self.seen, shot.fingerprint) {
            return false;
        }
        self.photos = self.photos.saturating_add(1);
        let bucket = shot.bucket();
        let when = shot.captured.clone().unwrap_or_default();
        let mut index: HashMap<(u32, u32, u8), usize> = self.pixels.iter().enumerate().map(|(i, p)| ((p.x, p.y, p.s), i)).collect();
        let mut hit = vec![false; self.pixels.len()];
        for d in found {
            match index.get(&(d.x, d.y, d.s)).copied() {
                Some(i) => {
                    let Some(p) = self.pixels.get_mut(i) else { continue };
                    if let Some(h) = hit.get_mut(i) {
                        *h = true;
                    }
                    p.hits = p.hits.saturating_add(1);
                    note_period(&mut p.periods, &mut p.buckets, bucket);
                    p.last_bucket = bucket;
                    (p.since, p.misses) = (0, 0);
                    if !when.is_empty() && (p.first.is_empty() || when < p.first) {
                        p.first = when.clone();
                    }
                    if when > p.last {
                        p.last = when.clone();
                    }
                    p.exp_min = min_opt(p.exp_min, shot.exposure);
                    p.exp_max = max_opt(p.exp_max, shot.exposure);
                    p.iso_min = p.iso_min.min(shot.iso).or(p.iso_min).or(shot.iso);
                    p.iso_max = p.iso_max.max(shot.iso);
                    p.level += (d.level - p.level) / p.hits as f32;
                    if p.kind != d.kind && d.kind == DefectKind::Stuck {
                        p.kind = DefectKind::Stuck;
                    }
                    if p.hits >= CONFIRM_HITS && p.buckets >= CONFIRM_BUCKETS {
                        p.confirmed = true;
                    }
                }
                None => {
                    index.insert((d.x, d.y, d.s), self.pixels.len());
                    hit.push(true);
                    self.pixels.push(PixelEntry {
                        x: d.x,
                        y: d.y,
                        s: d.s,
                        kind: d.kind,
                        source: PixelSource::Photos,
                        hits: 1,
                        buckets: 1,
                        last_bucket: bucket,
                        periods: vec![bucket],
                        since: 0,
                        misses: 0,
                        first: when.clone(),
                        last: when.clone(),
                        exp_min: shot.exposure,
                        exp_max: shot.exposure,
                        iso_min: shot.iso,
                        iso_max: shot.iso,
                        level: d.level,
                        confirmed: false,
                    });
                }
            }
        }
        // the ones not found this time
        for (p, h) in self.pixels.iter_mut().zip(&hit) {
            if *h {
                continue;
            }
            p.since = p.since.saturating_add(1);
            let in_range = |lo: Option<f64>, hi: Option<f64>, v: Option<f64>| match (lo, hi, v) {
                (Some(lo), Some(hi), Some(v)) => v >= lo * 0.999 && v <= hi * 1.001,
                _ => true,
            };
            let exp = in_range(p.exp_min, p.exp_max, shot.exposure);
            let iso = in_range(p.iso_min.map(f64::from), p.iso_max.map(f64::from), shot.iso.map(f64::from));
            if p.confirmed && exp && iso {
                p.misses = p.misses.saturating_add(1);
            }
        }
        self.pixels.retain(|p| match (p.confirmed, p.source) {
            (_, PixelSource::DarkFrame) => true,
            (true, _) => p.misses < EXPIRE_MISSES.max(p.hits.saturating_mul(20)),
            (false, _) if p.hits <= 1 => p.since <= SINGLETON_TTL,
            (false, _) => p.since <= 3 * SINGLETON_TTL,
        });
        self.cap_pixels();
        true
    }

    fn cap_pixels(&mut self) {
        if self.pixels.len() <= MAX_PIXELS {
            return;
        }
        // keep confirmed and dark-frame entries, then the most often seen, then the most recent
        self.pixels.sort_by(|a, b| {
            let rank = |p: &PixelEntry| (p.confirmed || p.source == PixelSource::DarkFrame, p.hits, std::cmp::Reverse(p.since));
            rank(b).cmp(&rank(a))
        });
        self.pixels.truncate(MAX_PIXELS);
        self.pixels.sort_by_key(|p| (p.y, p.x, p.s));
    }

    /// Add a dark frame's hot pixels as confirmed entries.
    pub fn record_dark_frame(&mut self, df: &lcc_raw::darkframe::DarkFrame) -> usize {
        let when = df.captured.clone().unwrap_or_default();
        let mut index: HashMap<(u32, u32, u8), usize> = self.pixels.iter().enumerate().map(|(i, p)| ((p.x, p.y, p.s), i)).collect();
        let mut n = 0;
        for d in df.hot.iter().take(MAX_PIXELS) {
            n += 1;
            match index.get(&(d.x, d.y, d.s)).and_then(|&i| self.pixels.get_mut(i)) {
                Some(p) => {
                    p.confirmed = true;
                    p.source = PixelSource::DarkFrame;
                    p.exp_min = min_opt(p.exp_min, df.exposure);
                    p.exp_max = max_opt(p.exp_max, df.exposure);
                }
                None => {
                    index.insert((d.x, d.y, d.s), self.pixels.len());
                    self.pixels.push(PixelEntry {
                        x: d.x,
                        y: d.y,
                        s: d.s,
                        kind: d.kind,
                        source: PixelSource::DarkFrame,
                        hits: 1,
                        buckets: 1,
                        last_bucket: 0,
                        periods: Vec::new(),
                        since: 0,
                        misses: 0,
                        first: when.clone(),
                        last: when.clone(),
                        exp_min: df.exposure,
                        exp_max: df.exposure,
                        iso_min: df.iso,
                        iso_max: df.iso,
                        level: d.level,
                        confirmed: true,
                    });
                }
            }
        }
        self.cap_pixels();
        n
    }

    /// Distance between two sensor positions in long-edge units.
    fn dist(&self, a: (f64, f64), b: (f64, f64)) -> f64 {
        let aspect = if self.aspect.is_finite() && self.aspect > 0.0 { self.aspect } else { 1.5 };
        let (sx, sy) = if aspect >= 1.0 { (1.0, 1.0 / aspect) } else { (aspect, 1.0) };
        (((a.0 - b.0) * sx).powi(2) + ((a.1 - b.1) * sy).powi(2)).sqrt()
    }

    /// Record one photo's dust: the spots found on it, and `check` for what a known spot's position looks like on it
    /// (to close windows of spots that have gone). `false` when the shot was counted already.
    pub fn record_dust(&mut self, shot: &Shot, found: &[DustSeen], check: impl Fn(f64, f64, f64) -> Option<DustCheck>) -> bool {
        if !Self::remember(&mut self.dust_seen, shot.fingerprint) {
            return false;
        }
        self.dust_photos = self.dust_photos.saturating_add(1);
        let t = shot.time().unwrap_or(0);
        let when = shot.captured.clone().unwrap_or_default();
        let bucket = shot.bucket();
        let f = shot.f_number.map(round_f);
        let mut matched = vec![false; self.dust.len()];
        for d in found.iter().filter(|d| d.x.is_finite() && d.y.is_finite() && d.radius.is_finite()) {
            let near = self
                .dust
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let r = e.look_at(f).map_or(d.radius, |l| l.0).max(d.radius);
                    (i, self.dist((e.x, e.y), (d.x, d.y)), (1.5 * r).max(0.004))
                })
                .filter(|(_, dist, tol)| dist <= tol)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, ..)| i);
            let i = match near {
                Some(i) => i,
                None => {
                    if self.dust.len() >= MAX_DUST {
                        continue;
                    }
                    self.dust.push(DustEntry { x: d.x, y: d.y, windows: Vec::new(), apertures: Vec::new(), confirmed: false });
                    matched.push(false);
                    self.dust.len() - 1
                }
            };
            let Some(e) = self.dust.get_mut(i) else { continue };
            if let Some(m) = matched.get_mut(i) {
                if *m {
                    continue; // (two finds on one spot in one photo count once)
                }
                *m = true;
            }
            // the position drifts a little with each find (a running mean)
            let n = e.windows.iter().map(|w| w.hits).sum::<u32>().max(1) as f64;
            e.x += (d.x - e.x) / (n + 1.0);
            e.y += (d.y - e.y) / (n + 1.0);
            let open = e.windows.last().is_some_and(|w| !w.closed);
            if !open {
                e.windows.push(DustWindow { first: when.clone(), last: when.clone(), t0: t, t1: t, last_bucket: i64::MIN, ..Default::default() });
            }
            if let Some(w) = e.windows.last_mut() {
                w.hits = w.hits.saturating_add(1);
                w.misses = 0;
                note_period(&mut w.periods, &mut w.buckets, bucket);
                w.last_bucket = bucket;
                if t < w.t0 {
                    (w.t0, w.first) = (t, when.clone());
                }
                if t > w.t1 {
                    (w.t1, w.last) = (t, when.clone());
                }
                if w.hits >= DUST_CONFIRM_HITS && w.buckets >= DUST_CONFIRM_BUCKETS {
                    e.confirmed = true;
                }
            }
            let key = f.unwrap_or(0.0);
            match e.apertures.iter_mut().find(|a| (a.f - key).abs() < 0.05) {
                Some(a) => {
                    a.n = a.n.saturating_add(1);
                    let k = a.n as f64;
                    a.radius += (d.radius - a.radius) / k;
                    a.strength += (d.strength - a.strength) / k as f32;
                    a.dip += (d.dip - a.dip) / k as f32;
                }
                None => e.apertures.push(ApertureLook { f: key, radius: d.radius, strength: d.strength, dip: d.dip, n: 1 }),
            }
        }
        // spots not found: did this photo show they're gone?
        for (e, m) in self.dust.iter_mut().zip(&matched) {
            if *m {
                continue;
            }
            let aperture_ok = match (f, e.apertures.iter().map(|a| a.f).filter(|f| *f > 0.0).reduce(f32::min)) {
                (Some(f), Some(widest)) => f >= widest * 0.9,
                _ => true,
            };
            let Some((radius, _, dip)) = e.look_at(f) else { continue };
            if !aperture_ok {
                continue;
            }
            let c = check(e.x, e.y, radius);
            let Some(w) = e.windows.last_mut().filter(|w| !w.closed) else { continue };
            if let Some(c) = c
                && c.testable
                && c.dip < 0.3 * dip.max(0.01)
            {
                w.misses = w.misses.saturating_add(1);
                if w.misses >= DUST_WINDOW_MISSES {
                    w.closed = true;
                }
            }
        }
        true
    }

    /// The dust spots to remove on a photo taken at `t` with f-number `f`: (x, y in the sensor frame, radius,
    /// darkening, confirmed). Confirmed spots whose window covers the photo, plus unconfirmed ones seen more than
    /// once in a window covering it.
    pub fn dust_for(&self, t: Option<i64>, f: Option<f32>) -> Vec<(f64, f64, f64, f32, bool)> {
        self.dust
            .iter()
            .filter_map(|e| {
                let w = e.window_at(t)?;
                if !e.confirmed && w.hits < 2 {
                    return None;
                }
                let (r, strength, _) = e.look_at(f.map(round_f))?;
                Some((e.x, e.y, r, strength, e.confirmed))
            })
            .collect()
    }

    /// Counts for `camera.pixelMap.status` / `camera.dustMap.status`.
    pub fn summary(&self) -> serde_json::Value {
        let count = |kind: DefectKind, confirmed: bool| self.pixels.iter().filter(|p| p.kind == kind && p.confirmed == confirmed).count();
        serde_json::json!({
            "body": self.body,
            "label": body_label(&self.body),
            "make": self.make,
            "model": self.model,
            "sensor": self.sensor,
            "photos": self.photos,
            "pixels": {
                "confirmed": self.pixels.iter().filter(|p| p.confirmed).count(),
                "candidates": self.pixels.iter().filter(|p| !p.confirmed).count(),
                "hot": count(DefectKind::Hot, true),
                "dead": count(DefectKind::Dead, true),
                "stuck": count(DefectKind::Stuck, true),
                "fromDarkFrames": self.pixels.iter().filter(|p| p.source == PixelSource::DarkFrame).count(),
            },
            "dustPhotos": self.dust_photos,
            "dust": {
                "confirmed": self.dust.iter().filter(|d| d.confirmed).count(),
                "suggested": self.dust.iter().filter(|d| !d.confirmed).count(),
                "windows": self.dust.iter().map(|d| d.windows.len()).sum::<usize>(),
            },
        })
    }

    /// Validate a map read from disk: version, sizes and every number in range.
    fn valid(&self) -> bool {
        let (w, h) = (self.sensor[0] as u64, self.sensor[1] as u64);
        let pixels = self.pixels.len() <= MAX_PIXELS && self.pixels.iter().all(|p| u64::from(p.x) < w.max(1) && u64::from(p.y) < h.max(1) && p.s < 4);
        let dust = self.dust.len() <= MAX_DUST
            && self
                .dust
                .iter()
                .all(|d| (0.0..=1.0).contains(&d.x) && (0.0..=1.0).contains(&d.y) && d.windows.len() <= 10_000 && d.apertures.len() <= 200);
        self.version == VERSION && !self.body.is_empty() && pixels && dust && self.seen.len() <= 2 * MAX_SEEN && self.dust_seen.len() <= 2 * MAX_SEEN
    }
}

/// Count capture period `b` once: kept in `periods` (the first 8 distinct ones; more than confirmation needs).
fn note_period(periods: &mut Vec<i64>, buckets: &mut u32, b: i64) {
    if periods.contains(&b) {
        return;
    }
    if periods.len() < 8 {
        periods.push(b);
        *buckets = (*buckets).max(periods.len() as u32);
    } else {
        *buckets = buckets.saturating_add(1);
    }
}

fn min_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn max_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

// ------------------------------------------------------------------------------------------------- the registry

/// The maps in use, read on first use and written back as they change.
#[derive(Default)]
struct Registry {
    maps: HashMap<String, BodyMap>,
    /// Bodies whose file was looked for.
    loaded: HashSet<String>,
    /// Photos recorded per body since its file was last written.
    unsaved: HashMap<String, u32>,
    /// Corrections per body (the confirmed pixels), rebuilt when they change.
    fixes: HashMap<String, Arc<Vec<Fix>>>,
    /// Every map file was read ([`generation`] needs them all).
    all_loaded: bool,
    /// The preview keys' starting point, read or made with the first [`Registry::combined`].
    baseline: Option<Arc<KeyBaseline>>,
    /// The bodies the published [`PreviewKeys`] know.
    keyed: HashSet<String>,
}

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
/// [`generation`]'s value (0: not worked out yet).
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// [`generation`] is part of every photo's render and preview keys, so a test that changes a body's corrections
/// changes the keys of the previews a test running alongside has just built, which then look missing or stale
/// (in the same test binary, `tests_sensor` rebuilding a body map while `tests_preview_prep` checks its builds).
/// Tests that look previews up by their keys hold this shared ([`keep_generation`]), tests that change body maps
/// hold it exclusively ([`change_generation`]).
#[cfg(test)]
static TEST_GENERATION: std::sync::RwLock<()> = std::sync::RwLock::new(());

/// [`generation`] stays as it is while the guard lives (tests only; see [`TEST_GENERATION`]).
#[cfg(test)]
pub(crate) fn keep_generation() -> std::sync::RwLockReadGuard<'static, ()> {
    TEST_GENERATION.read().unwrap_or_else(PoisonError::into_inner)
}

/// The guard of a test that changes body maps, and with them [`generation`]: it waits until no test keeping the
/// generation runs (tests only; see [`TEST_GENERATION`]).
#[cfg(test)]
pub(crate) fn change_generation() -> std::sync::RwLockWriteGuard<'static, ()> {
    TEST_GENERATION.write().unwrap_or_else(PoisonError::into_inner)
}

fn with<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    let reg = REGISTRY.get_or_init(Default::default);
    let mut g = reg.lock().unwrap_or_else(PoisonError::into_inner);
    f(&mut g)
}

/// What the preview keys are worked out from, published whole so a key never waits for the registry (which writes
/// map files while it is held).
static PREVIEW_KEYS: OnceLock<RwLock<Arc<PreviewKeys>>> = OnceLock::new();

/// The largest baseline file read.
const MAX_BASELINE: u64 = 1 << 20;
/// The baseline's file in [`dir`] (not a `.json`: those are the bodies' maps).
const BASELINE_FILE: &str = "preview-key-baseline.dat";

/// A number made from every body's corrections (their confirmed pixels): it changes when any of them changes, and
/// is the same from one run to the next while they don't. Says when decoded sources are out of date; a preview's
/// key has [`preview_key`], which only its own body's corrections change.
pub fn generation() -> u64 {
    match GENERATION.load(Ordering::Relaxed) {
        0 => with(|r| {
            r.load_all();
            r.combined()
        }),
        g => g,
    }
}

/// The sensor-map part of a preview's cache key, for a photo with this catalogued `serial` and `file_name`:
/// it changes when the corrections of the photo's own body change, and not when another body's do.
/// `develops_raw` false: no map applies to the photo, so nothing changes it.
pub fn preview_key(develops_raw: bool, serial: &str, file_name: &str) -> u64 {
    generation();
    let keys = PREVIEW_KEYS.get_or_init(Default::default).read().unwrap_or_else(PoisonError::into_inner).clone();
    keys.key_of(develops_raw, serial, file_name)
}

/// Each body with a map and a number for its corrections (0: none): a body whose number moved has new corrections.
pub fn body_corrections() -> HashMap<String, u64> {
    generation();
    PREVIEW_KEYS.get_or_init(Default::default).read().unwrap_or_else(PoisonError::into_inner).terms.clone()
}

/// The corrections in use when the app first kept per-body preview keys, kept in a small file beside the maps. A
/// body whose corrections are still these keeps the number previews were cached under until then
/// ([`Self::generation`]), so updating the app rebuilds nothing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct KeyBaseline {
    /// [`generation`] at that moment (1: no body had corrections).
    generation: u64,
    /// Each body with corrections then, and [`body_term`] of them.
    bodies: HashMap<String, u64>,
}

impl KeyBaseline {
    /// The baseline saved in `dir`, else a new one from the corrections now (saved there). A damaged file only means
    /// a new baseline: previews cached under the old one are rendered again.
    fn read_or_make(dir: Option<&std::path::Path>, generation: u64, terms: &HashMap<String, u64>) -> KeyBaseline {
        let path = dir.map(|d| d.join(BASELINE_FILE));
        let saved = path.as_ref().filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() <= MAX_BASELINE)).and_then(|p| std::fs::read(p).ok());
        if let Some(b) = saved.and_then(|t| serde_json::from_slice::<KeyBaseline>(&t).ok()).filter(|b| b.generation != 0) {
            return b;
        }
        let made = KeyBaseline { generation, bodies: terms.iter().filter(|(_, t)| **t != 0).map(|(b, t)| (b.clone(), *t)).collect() };
        if let Some(path) = path {
            let saved = path.parent().map_or(Ok(()), std::fs::create_dir_all).map_err(|e| e.to_string()).and_then(|()| {
                let json = serde_json::to_vec(&made).map_err(|e| e.to_string())?;
                lcc_catalog::safe_file::write_atomic_nosync(&path, &json).map_err(|e| e.to_string())
            });
            if let Err(e) = saved {
                log::warn!("sensor maps: {}: {e}", path.display());
            }
        }
        made
    }

    fn term(&self, body: &str) -> u64 {
        self.bodies.get(body).copied().unwrap_or(0)
    }
}

/// Every body's corrections as the preview keys see them.
#[derive(Clone, Debug, Default)]
struct PreviewKeys {
    /// [`generation`].
    generation: u64,
    /// Every body with a map, and [`body_term`] of its corrections (0: none).
    terms: HashMap<String, u64>,
    baseline: Arc<KeyBaseline>,
}

impl PreviewKeys {
    fn term(&self, body: &str) -> u64 {
        self.terms.get(body).copied().unwrap_or(0)
    }

    fn key_of(&self, develops_raw: bool, serial: &str, file_name: &str) -> u64 {
        if !develops_raw {
            return self.baseline.generation;
        }
        let by_serial = body_key(Some(serial), None);
        let by_name = body_key(None, Some(file_name));
        // catalogued without a serial, and no map goes by its file name: the file may state a serial the catalog
        // doesn't have (it was browsed before the app kept them), so any body's change may be this photo's
        if by_serial.is_none() && !by_name.as_ref().is_some_and(|b| self.terms.contains_key(b)) {
            return self.generation;
        }
        // both names: a decode goes by the serial it reads from the file, or by the file name when it reads none
        let bodies = || by_serial.iter().chain(by_name.iter());
        if bodies().all(|b| self.term(b) == self.baseline.term(b)) {
            return self.baseline.generation;
        }
        bodies().fold(1u64, |g, b| g.wrapping_add(self.term(b))).max(1)
    }
}

/// What a body's confirmed pixels add to the keys (0: it has none).
fn body_term(body: &str, m: &BodyMap) -> u64 {
    if !m.pixels.iter().any(|p| p.confirmed) {
        return 0;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for c in body.bytes() {
        h = (h ^ u64::from(c)).wrapping_mul(0x0100_0000_01b3);
    }
    h ^ m.confirmed_set()
}

fn read_file(body: &str) -> Option<BodyMap> {
    let path = dir()?.join(file_name(body));
    let meta = std::fs::metadata(&path).ok()?;
    let bad = |why: String| {
        log::warn!("sensor map {}: {why}; starting a new one (the old file is kept as .bad)", path.display());
        let _ = std::fs::rename(&path, path.with_extension("json.bad"));
        None
    };
    if meta.len() > MAX_FILE {
        return bad("too large".into());
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => return bad(e.to_string()),
    };
    match serde_json::from_str::<BodyMap>(&text) {
        Ok(m) if m.valid() && m.body == body => Some(m),
        Ok(_) => bad("not a valid map for this body".into()),
        Err(e) => bad(e.to_string()),
    }
}

impl Registry {
    fn map(&mut self, body: &str) -> Option<&mut BodyMap> {
        if self.loaded.insert(body.to_string())
            && let Some(m) = read_file(body)
        {
            self.fixes.insert(body.to_string(), Arc::new(m.fixes()));
            self.maps.insert(body.to_string(), m);
        }
        self.maps.get_mut(body)
    }

    fn save(&mut self, body: &str) -> Result<(), String> {
        self.unsaved.remove(body);
        let (Some(dir), Some(m)) = (dir(), self.maps.get(body)) else { return Ok(()) };
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let json = serde_json::to_vec(m).map_err(|e| e.to_string())?;
        let path = dir.join(file_name(body));
        lcc_catalog::safe_file::write_atomic(&path, &json).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn refresh_fixes(&mut self, body: &str, before: Option<u64>) {
        let Some(m) = self.maps.get(body) else { return };
        let changed = before != Some(m.confirmed_set());
        if changed {
            self.fixes.insert(body.to_string(), Arc::new(m.fixes()));
        }
        // (a new map counts too: it says a photo's file name is what its body goes by)
        if (changed || !self.keyed.contains(body)) && GENERATION.load(Ordering::Relaxed) != 0 {
            self.combined();
        }
    }

    /// Read every map file in [`dir`] (once).
    fn load_all(&mut self) {
        if self.all_loaded {
            return;
        }
        self.all_loaded = true;
        for b in body_names_on_disk() {
            let _ = self.map(&b);
        }
    }

    /// Work out [`generation`] from the maps in memory (all of them, after [`Self::load_all`]) and store it.
    fn combined(&mut self) -> u64 {
        let terms: HashMap<String, u64> = self.maps.iter().map(|(b, m)| (b.clone(), body_term(b, m))).collect();
        let g = terms.values().fold(1u64, |g, t| g.wrapping_add(*t)).max(1);
        let baseline = match &self.baseline {
            Some(b) => b.clone(),
            None => Arc::new(KeyBaseline::read_or_make(dir().as_deref(), g, &terms)),
        };
        self.baseline = Some(baseline.clone());
        self.keyed = terms.keys().cloned().collect();
        let keys = Arc::new(PreviewKeys { generation: g, terms, baseline });
        *PREVIEW_KEYS.get_or_init(Default::default).write().unwrap_or_else(PoisonError::into_inner) = keys;
        GENERATION.store(g, Ordering::Relaxed);
        g
    }
}

/// The corrections for a photo of `body` (`model`, raw data size `sensor`): its confirmed pixels, when the map fits.
pub fn fixes_for(body: &str, model: &str, sensor: [usize; 2]) -> Arc<Vec<Fix>> {
    with(|r| {
        let fits = r.map(body).is_some_and(|m| m.fits(model, sensor));
        if fits { r.fixes.get(body).cloned().unwrap_or_default() } else { Arc::default() }
    })
}

/// Record a photo's pixel candidates for `body` (creating its map). `Some(true)` when the body's corrections
/// changed (a pixel was confirmed or expired), `None` when the map doesn't fit this file (another model or size
/// under the same key) or the shot was counted already.
pub fn record_pixels(body: &str, make: &str, model: &str, sensor: [usize; 2], shot: &Shot, found: &[Defect]) -> Option<bool> {
    with(|r| {
        let m = match r.map(body) {
            Some(m) => m,
            None => {
                r.maps.insert(body.to_string(), BodyMap::new(body, make, model, sensor));
                r.maps.get_mut(body)?
            }
        };
        if !m.fits(model, sensor) {
            return None;
        }
        if m.sensor == [0, 0] {
            (m.sensor, m.model, m.make) = (sensor, model.to_string(), make.to_string());
        }
        let before = m.confirmed_set();
        if !m.record_pixels(shot, found) {
            return None;
        }
        let changed = before != m.confirmed_set();
        r.refresh_fixes(body, Some(before));
        let n = r.unsaved.entry(body.to_string()).or_default();
        *n += 1;
        if (changed || *n >= 8)
            && let Err(e) = r.save(body)
        {
            log::warn!("sensor map {body}: {e}");
        }
        Some(changed)
    })
}

/// Run `f` on `body`'s map (created for `make` / `model` / `sensor` when there is none), save it and refresh the
/// corrections. `Err` when the map is for another sensor.
pub fn update<R>(body: &str, make: &str, model: &str, sensor: [usize; 2], f: impl FnOnce(&mut BodyMap) -> R) -> Result<R, String> {
    with(|r| {
        if r.map(body).is_none() {
            r.maps.insert(body.to_string(), BodyMap::new(body, make, model, sensor));
        }
        let m = r.maps.get_mut(body).ok_or("no map")?;
        if !m.fits(model, sensor) {
            return Err(format!("the map of body {} is for a {} ({}×{}), not this {model}", body_label(body), m.model, m.sensor[0], m.sensor[1]));
        }
        if m.sensor == [0, 0] {
            (m.sensor, m.model, m.make) = (sensor, model.to_string(), make.to_string());
        }
        let before = m.confirmed_set();
        let out = f(m);
        r.refresh_fixes(body, Some(before));
        r.save(body)?;
        Ok(out)
    })
}

/// A copy of `body`'s map.
pub fn get(body: &str) -> Option<BodyMap> {
    with(|r| r.map(body).cloned())
}

/// Every body with a map (in memory or on disk), sorted.
pub fn bodies() -> Vec<String> {
    let mut out: Vec<String> = with(|r| r.maps.keys().cloned().collect());
    out.extend(body_names_on_disk());
    out.sort();
    out.dedup();
    out
}

/// The bodies named by the map files in [`dir`].
fn body_names_on_disk() -> Vec<String> {
    let mut out = Vec::new();
    if let Some(d) = dir()
        && let Ok(rd) = std::fs::read_dir(&d)
    {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            if let Some(m) = std::fs::metadata(&p).ok().filter(|m| m.len() <= MAX_FILE).and_then(|_| std::fs::read_to_string(&p).ok()) {
                // (only the body name is needed; a damaged file is dealt with when the body is used)
                #[derive(Deserialize)]
                struct Head {
                    body: String,
                }
                if let Ok(h) = serde_json::from_str::<Head>(&m) {
                    out.push(h.body);
                }
            }
        }
    }
    out
}

/// Forget `body`'s pixels (`dust: false`), dust (`pixels: false`) or both, on disk too.
pub fn clear(body: &str, pixels: bool, dust: bool) -> Result<(), String> {
    with(|r| {
        let Some(m) = r.map(body) else { return Ok(()) };
        let before = m.confirmed_set();
        if pixels {
            (m.pixels, m.seen, m.photos) = (Vec::new(), Vec::new(), 0);
        }
        if dust {
            (m.dust, m.dust_seen, m.dust_photos) = (Vec::new(), Vec::new(), 0);
        }
        r.refresh_fixes(body, Some(before));
        r.save(body)
    })
}

/// Bodies of files decoded in this run, by path (most recent [`MAX_PATHS`]).
static PATHS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
const MAX_PATHS: usize = 200_000;

/// Remember which body the file at `path` came from.
pub fn remember_path(path: &str, body: &str) {
    let mut m = PATHS.get_or_init(Default::default).lock().unwrap_or_else(PoisonError::into_inner);
    if m.len() >= MAX_PATHS {
        m.clear();
    }
    m.insert(path.to_string(), body.to_string());
}

/// The body of the file at `path`, when a decode in this run found it.
pub fn body_of_path(path: &str) -> Option<String> {
    PATHS.get_or_init(Default::default).lock().unwrap_or_else(PoisonError::into_inner).get(path).cloned()
}

/// Keep a dark frame's record (`<dir>/dark-frames/<body>-<time>.json`) for whatever wants its noise figures later.
pub fn save_dark_frame(body: &str, df: &lcc_raw::darkframe::DarkFrame) -> Result<PathBuf, String> {
    let d = dir().ok_or("no folder for sensor maps")?.join("dark-frames");
    std::fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    let when: String =
        df.captured.as_deref().unwrap_or("undated").chars().take(19).map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let stem = file_name(body);
    let path = d.join(format!("{}-{when}-iso{}.json", stem.trim_end_matches(".json"), df.iso.unwrap_or(0)));
    let json = serde_json::to_vec_pretty(df).map_err(|e| e.to_string())?;
    lcc_catalog::safe_file::write_atomic(&path, &json).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Write every map with unsaved photos.
pub fn flush() {
    with(|r| {
        let bodies: Vec<String> = r.unsaved.keys().cloned().collect();
        for b in bodies {
            if let Err(e) = r.save(&b) {
                log::warn!("sensor map {b}: {e}");
            }
        }
    });
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod preview_key_tests {
    use super::*;

    fn keys(generation: u64, terms: &[(&str, u64)], baseline: &[(&str, u64)], base_generation: u64) -> PreviewKeys {
        let map = |l: &[(&str, u64)]| l.iter().map(|(b, t)| (b.to_string(), *t)).collect::<HashMap<_, _>>();
        PreviewKeys { generation, terms: map(terms), baseline: Arc::new(KeyBaseline { generation: base_generation, bodies: map(baseline) }) }
    }

    /// Updating the app keeps every key a library's previews were cached under.
    #[test]
    fn unchanged_bodies_keep_the_number_previews_are_cached_under() {
        let k = keys(77, &[("SN-A", 30), ("SN-B", 46), ("file:SA", 0)], &[("SN-A", 30), ("SN-B", 46)], 77);
        for (serial, name) in [("SN-A", "a.CR3"), ("SN-B", "b.CR3"), ("", "_SA0001.ARW")] {
            assert_eq!(k.key_of(true, serial, name), 77, "{serial} {name}");
        }
        assert_eq!(k.key_of(false, "", "a.jpg"), 77);
        // no body has corrections and none had: the number the app has always used
        assert_eq!(keys(1, &[("SN-A", 0)], &[], 1).key_of(true, "SN-A", "a.CR3"), 1);
    }

    #[test]
    fn a_change_moves_only_that_bodys_key_and_going_back_restores_it() {
        let base = [("SN-A", 30), ("SN-B", 46)];
        let k = keys(99, &[("SN-A", 52), ("SN-B", 46)], &base, 77);
        let a = k.key_of(true, "SN-A", "a.CR3");
        assert_ne!(a, 77);
        assert_eq!(k.key_of(true, "SN-B", "b.CR3"), 77, "the other body keeps its previews");
        assert_eq!(k.key_of(false, "", "a.jpg"), 77, "no map applies to a JPEG");
        assert_ne!(keys(121, &[("SN-A", 74), ("SN-B", 46)], &base, 77).key_of(true, "SN-A", "a.CR3"), a);
        assert_eq!(keys(77, &base, &base, 77).key_of(true, "SN-A", "a.CR3"), 77);
    }

    /// A decode may go by the file name although the catalog knows a serial, so either body's change counts.
    #[test]
    fn a_photo_follows_both_of_its_possible_bodies() {
        let k = keys(99, &[("file:71", 52)], &[], 1);
        assert_ne!(k.key_of(true, "SN-A", "_71_0001.CR3"), 1);
    }

    /// A photo catalogued without a serial, with no map under its file name, may belong to any body.
    #[test]
    fn a_photo_of_an_unknown_body_follows_every_change() {
        let k = keys(99, &[("SN-A", 52)], &[("SN-A", 30)], 77);
        assert_eq!(k.key_of(true, "", "_71_0001.CR3"), 99);
    }

    #[test]
    fn the_baseline_survives_a_restart_and_a_damaged_file_only_starts_a_new_one() {
        let dir = std::env::temp_dir().join(format!("lc-key-baseline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let terms: HashMap<String, u64> = [("SN-A".to_string(), 30), ("SN-B".to_string(), 0)].into_iter().collect();
        let first = KeyBaseline::read_or_make(Some(&dir), 31, &terms);
        assert_eq!(first.bodies.len(), 1, "only bodies with corrections");
        let later = KeyBaseline::read_or_make(Some(&dir), 500, &HashMap::new());
        assert_eq!(later, first, "the saved one stays what it was");
        assert_eq!(std::path::Path::new(BASELINE_FILE).extension().and_then(|x| x.to_str()), Some("dat"), "not read as a body's map");
        std::fs::write(dir.join(BASELINE_FILE), b"{ not json").unwrap();
        assert_eq!(KeyBaseline::read_or_make(Some(&dir), 500, &HashMap::new()).generation, 500);
        assert_eq!(KeyBaseline::read_or_make(None, 8, &terms).generation, 8, "no folder: for this run only");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
