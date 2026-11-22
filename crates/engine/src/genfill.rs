//! Generative fill, the session's side (`docs/generative-fill.md`).
//!
//! * **Patches on disk.** A generated variation is a `.lcpatch` file in the library's `genfill`
//!   folder (`genfill/<photo id>/<generation id>-<n>.lcpatch`), never in the catalog or XMP; the
//!   spot's [`GenFill`] refers to it. Renders get the files through [`Session::genfill_store`]
//!   ([`GenStore`]), read on the render thread and kept in a small shared cache. A missing or
//!   damaged file renders the spot as a content-aware Fill and the panel offers Generate again.
//! * **Generating** ([`Session::genfill_start`]): only when the user clicked Generate, only to a
//!   provider whose terms they accepted ([`Session::genfill_consent`]), only with their own key.
//!   A worker thread loads the photo's full-size source, cuts the context crop around the spot
//!   ([`plan`]), sends it with the fixed transform ([`Neutral`]), brings the results back to
//!   linear as-shot light, matches them to the photo around the hole and writes the patches. The
//!   job holds an activity row with ✕ the whole time.
//! * **Keys and consent** live in the app's settings folder (see [`lcc_genfill::cloud::keys`]).
//! * **A paid result is never lost** (test note 5). Each generation's patches and a small record of it
//!   (`<id>.json`: the stroke it was made for, the service, the prompt) are written before it is shown. A finished
//!   generation always lands on a spot, also when the spot was edited, moved or deleted meanwhile. Generate Again
//!   keeps the spot's earlier generations among its variations. A spot that shows a fill is not moved or resized
//!   by accident (its brush size is the next stroke's); one moved before this was so can be put back
//!   ([`lcc_develop::Spot::put_back`]). Every generation in the library's folder can be restored
//!   ([`Session::genfill_results`], [`Session::genfill_restore`]). A generation that failed after the service
//!   accepted it is fetched, not paid for again, when it is tried again within a few minutes ([`Resume`]).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use lcc_catalog::{Photo, PhotoId};
use lcc_develop::{DevelopSettings, GenFill, GenStroke, Spot, SpotMode};
use lcc_genfill::PatchLayer;
#[cfg(not(target_arch = "wasm32"))]
use lcc_genfill::patch::PatchInfo;
use lcc_geom::Point;
use lcc_pipeline::genpatch::{GenStore, PatchSource};
use lcc_raster::{Plane, Rgb32f};

/// Name of the folder (in the library) the patches are kept in.
pub const DIR: &str = "genfill";
/// Most variations one Generate makes.
pub const MAX_VARIATIONS: u32 = 4;
/// Most requests one Generate sends (one per variation for some services).
pub const MAX_REQUESTS: u32 = 16;
/// What a generation says, before anything is sent, when its area can't go in one request at
/// [`MIN_SCALE`] or more (decided wording: a fill is never sent in pieces).
pub const TOO_LARGE: &str = "The model can't take an area this large. Please select a smaller area.";
/// The lowest resolution (sent size ÷ native size) a fill is sent at: a larger area would come back
/// too soft to use, and is refused with [`TOO_LARGE`] instead.
pub const MIN_SCALE: f64 = 0.5;
/// Largest patch file read.
const MAX_FILE: u64 = 1 << 30;
/// Bytes of patches kept in memory.
const CACHE_BYTES: usize = 1 << 30;

/// Patch files read so far, by path (`None`: missing or unreadable, re-checked after a while).
#[derive(Default)]
pub struct PatchCache {
    map: Mutex<HashMap<PathBuf, (Option<Arc<PatchLayer>>, web_time::Instant)>>,
}

impl PatchCache {
    fn get(&self, path: &Path) -> Option<Arc<PatchLayer>> {
        let mut map = self.map.lock().unwrap_or_else(PoisonError::into_inner);
        // answers are re-checked every few seconds: a file that appears or goes (deleted, an offline
        // drive) is noticed
        if let Some((hit, at)) = map.get_mut(path) {
            if at.elapsed().as_secs() < 3 {
                return hit.clone();
            }
            if hit.is_some() && path.is_file() {
                *at = web_time::Instant::now();
                return hit.clone();
            }
        }
        let read = read_patch(path).ok().map(Arc::new);
        let used: usize = map.values().filter_map(|(l, _)| l.as_ref()).map(|l| l.bytes()).sum();
        if used + read.as_ref().map_or(0, |l| l.bytes()) > CACHE_BYTES {
            map.clear();
        }
        map.insert(path.to_path_buf(), (read.clone(), web_time::Instant::now()));
        read
    }

    /// Forget `path` (it was just written).
    pub fn forget(&self, path: &Path) {
        self.map.lock().unwrap_or_else(PoisonError::into_inner).remove(path);
    }
}

/// Read a generation's record (`None`: missing or unreadable).
pub fn read_record(path: &Path) -> Option<Record> {
    if std::fs::metadata(path).ok()?.len() > 16 * 1024 * 1024 {
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Read a patch file (size-capped; a damaged file is an error).
pub fn read_patch(path: &Path) -> Result<PatchLayer, String> {
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > MAX_FILE {
        return Err("patch file too large".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    PatchLayer::from_bytes(&bytes).map_err(|e| e.to_string())
}

/// The patches of one photo, for renders.
struct PhotoPatches {
    dir: PathBuf,
    cache: Arc<PatchCache>,
}

impl PatchSource for PhotoPatches {
    fn patch(&self, id: &str, variant: u32) -> Option<Arc<PatchLayer>> {
        if !GenFill::valid_id(id) {
            return None;
        }
        self.cache.get(&self.dir.join(format!("{id}-{variant}.lcpatch")))
    }
}

/// A session's own folder name (a new generation id).
#[derive(Clone, Debug)]
pub struct Scratch(pub String);

impl Default for Scratch {
    fn default() -> Self {
        // (no process ids in the web build, nor generative fill)
        #[cfg(not(target_arch = "wasm32"))]
        return Scratch(new_id(0));
        #[cfg(target_arch = "wasm32")]
        return Scratch("web".into());
    }
}

/// What the user agreed to, per provider (`provider id → date`).
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct ConsentFile {
    accepted: BTreeMap<String, String>,
}

/// Where a generation stands.
#[derive(Clone, Debug, PartialEq)]
pub enum JobState {
    Running { done: u32, total: u32 },
    Done,
    Failed(String),
    Cancelled,
}

/// A running or finished generation.
pub struct Job {
    pub photo: PhotoId,
    pub spot: usize,
    pub stroke: u64,
    pub provider: String,
    /// The spot as it was when the generation started (the result goes back on it whatever happens meanwhile).
    spot_at: Spot,
    state: Mutex<JobState>,
    result: Mutex<Option<Generated>>,
    cancel: Arc<AtomicBool>,
    #[cfg(not(target_arch = "wasm32"))]
    resume: Resume,
}

/// What trying a failed generation again needs to fetch, rather than pay for again, what the service already
/// accepted: the same generation (its id, so its files are the same) and the requests the service accepted.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub struct Resume {
    pub photo: PhotoId,
    pub stroke: u64,
    pub provider: String,
    pub prompt: String,
    pub variations: u32,
    pub id: String,
    pub accepted: Arc<lcc_genfill::cloud::Accepted>,
    pub at: web_time::Instant,
}

/// How long a service keeps a result to fetch (Black Forest Labs: 10 minutes): a Retry later sends a new request.
#[cfg(not(target_arch = "wasm32"))]
const RESUME_FOR: std::time::Duration = std::time::Duration::from_secs(9 * 60);

impl Job {
    pub fn state(&self) -> JobState {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    fn set(&self, s: JobState) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = s;
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// What a finished generation made.
#[derive(Clone, Debug)]
pub struct Generated {
    pub fill: GenFill,
    pub warnings: Vec<String>,
}

/// The session's generative fill state.
#[derive(Default)]
pub struct GenfillState {
    #[cfg(not(target_arch = "wasm32"))]
    pub keys: lcc_genfill::cloud::keys::KeyStore,
    consent_path: Option<PathBuf>,
    /// Consent given this session when there is no settings folder (tests, a disabled folder).
    consent_mem: BTreeMap<String, String>,
    /// Run generations in the background and apply them in [`Session::genfill_poll`] (the app);
    /// otherwise `spot.generate` waits (CLI, MCP, tests).
    pub background: bool,
    /// A local mock server to send to instead of the providers (tests; `<PREFIX>_GENFILL_MOCK`
    /// does the same for a whole process). Only loopback addresses are accepted.
    pub mock: Option<String>,
    pub cache: Arc<PatchCache>,
    /// This session's own folder name for a library that is not on disk ([`Session::genfill_dir`]).
    pub scratch: Scratch,
    pub job: Option<Arc<Job>>,
    /// The last finished job's warnings or error, for the panel.
    pub last: Option<Result<Generated, String>>,
    /// A generation that failed after the service accepted (and billed) it: Generate or Retry on the same spot with
    /// the same choices fetches its results instead of sending it again.
    #[cfg(not(target_arch = "wasm32"))]
    pub resume: Option<Resume>,
}

impl GenfillState {
    fn consent_file(&self) -> ConsentFile {
        let Some(p) = &self.consent_path else { return ConsentFile { accepted: self.consent_mem.clone() } };
        std::fs::read(p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// Whether the user accepted `provider`'s terms (the consent dialog).
    pub fn consented(&self, provider: &str) -> bool {
        self.consent_file().accepted.contains_key(provider)
    }

    pub fn set_consent(&mut self, provider: &str, accept: bool, today: &str) -> Result<(), String> {
        let mut f = self.consent_file();
        if accept {
            f.accepted.insert(provider.to_string(), today.to_string());
        } else {
            f.accepted.remove(provider);
        }
        match &self.consent_path {
            None => {
                self.consent_mem = f.accepted;
                Ok(())
            }
            Some(p) => {
                if let Some(d) = p.parent() {
                    std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
                }
                std::fs::write(p, serde_json::to_vec_pretty(&f).unwrap_or_default()).map_err(|e| format!("could not save the consent: {e}"))
            }
        }
    }

    pub fn running(&self) -> bool {
        self.job.as_ref().is_some_and(|j| matches!(j.state(), JobState::Running { .. }))
    }

    /// Where generative fill stands, as `genfill.status` and the control protocol's `ui.inspect`
    /// (`genfill`) report it: `{running, done, total, provider, photo, spot, last: {ok, warnings?,
    /// id?, error?} | null}`.
    pub fn status_json(&self) -> serde_json::Value {
        use serde_json::json;
        let (running, done, total) = match self.job.as_ref().map(|j| j.state()) {
            Some(JobState::Running { done, total }) => (true, done, total),
            _ => (false, 0, 0),
        };
        let last = match &self.last {
            Some(Ok(g)) => json!({"ok": true, "warnings": g.warnings, "id": g.fill.id}),
            Some(Err(e)) => json!({"ok": false, "error": e}),
            None => serde_json::Value::Null,
        };
        let job = self.job.as_ref();
        json!({"running": running, "done": done, "total": total, "provider": job.map(|j| j.provider.clone()), "photo": job.map(|j| j.photo.0), "spot": job.map(|j| j.spot), "last": last})
    }
}

// ------------------------------------------------------------------------------------------
// Planning the crop

/// The hole of a spot in source pixels and how it is sent.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// Source size (px) the plan is for.
    pub src_w: usize,
    pub src_h: usize,
    /// The stroke's dabs in source px: centre and hole radius (grown like Fill).
    pub dabs: Vec<(f64, f64, f64)>,
    /// Blend margin outside the hole and the ring of original pixels around it (px).
    pub feather: f64,
    pub ring: f64,
    pub opacity: f32,
    /// The patch rectangle (source px, `[x0, y0, x1, y1)`).
    pub rect: [usize; 4],
    /// What is sent: the one crop, in source px, with the size it is sent at.
    pub tile: Tile,
    /// Sent size ÷ native size (1 = native; below 1 the result is softer than the photo).
    pub scale: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    /// `[x0, y0, x1, y1)` in source px.
    pub crop: [usize; 4],
    pub send: (usize, usize),
}

/// Provider limits for [`plan`].
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_side: usize,
    /// Fixed sizes (the crop is cut to the nearest aspect), or none.
    pub sizes: &'static [(u32, u32)],
}

impl Limits {
    /// `provider`'s limits.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn for_provider(provider: lcc_genfill::cloud::Provider) -> Limits {
        Limits { max_side: provider.info().max_side as usize, sizes: provider.info().sizes }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn smooth(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0).max(1e-9)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Plan sending `spot` of a photo developed with `s` (orientation, lens corrections and geometry
/// place the stroke on the source), for a source of `src_w × src_h` px.
pub fn plan(
    spot: &Spot,
    s: &DevelopSettings,
    lens: Option<&lcc_develop::EmbeddedLens>,
    src_w: usize,
    src_h: usize,
    lim: Limits,
) -> Result<Plan, String> {
    if src_w < 16 || src_h < 16 {
        return Err("the photo is too small".into());
    }
    let frame = lcc_pipeline::geometry::Frame::with_lens(src_w, src_h, s, false, lens);
    let long = frame.ow.max(frame.oh);
    let r_t = spot.size * long;
    if !r_t.is_finite() || r_t <= 0.0 {
        return Err("the spot has no size".into());
    }
    let mut dabs = Vec::new();
    for p in spot.points.iter().take(20_000) {
        let c = lcc_pipeline::genpatch::transformed_to_source(&frame, *p);
        let ex = lcc_pipeline::genpatch::transformed_to_source(&frame, Point::new(p.x + r_t / frame.ow, p.y));
        let ey = lcc_pipeline::genpatch::transformed_to_source(&frame, Point::new(p.x, p.y + r_t / frame.oh));
        let r = (c.dist(ex) + c.dist(ey)) / 2.0;
        if c.x.is_finite() && c.y.is_finite() && r.is_finite() && r > 0.0 {
            dabs.push((c.x, c.y, r * f64::from(lcc_pipeline::spots::GROW) + 1.0));
        }
    }
    if dabs.is_empty() {
        return Err("the spot is outside the photo".into());
    }
    let r = dabs.iter().map(|d| d.2).fold(0.0f64, f64::max);
    let feather = (r * (0.08 + 0.25 * (spot.feather / 100.0).clamp(0.0, 1.0))).max(3.0);
    let ring = (r * 0.3).max(16.0);
    let pad = r + feather + ring;
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (x, y, _) in &dabs {
        (x0, y0, x1, y1) = (x0.min(x - pad), y0.min(y - pad), x1.max(x + pad), y1.max(y + pad));
    }
    let clampx = |v: f64| v.clamp(0.0, src_w as f64) as usize;
    let clampy = |v: f64| v.clamp(0.0, src_h as f64) as usize;
    let rect = [clampx(x0.floor()), clampy(y0.floor()), clampx(x1.ceil()), clampy(y1.ceil())];
    if rect[2] <= rect[0] + 2 || rect[3] <= rect[1] + 2 {
        return Err("the spot is outside the photo".into());
    }
    let (rw, rh) = (rect[2] - rect[0], rect[3] - rect[1]);
    // the send size: a fixed size of the nearest aspect, or up to the provider's longest side
    let send_for = |w: usize, h: usize| -> (usize, usize) {
        match lim.sizes.iter().min_by(|a, b| {
            let ar = w as f64 / h.max(1) as f64;
            let da = (f64::from(a.0) / f64::from(a.1) / ar).ln().abs();
            let db = (f64::from(b.0) / f64::from(b.1) / ar).ln().abs();
            da.total_cmp(&db)
        }) {
            Some(&(a, b)) => (a as usize, b as usize),
            None => (w.min(lim.max_side), h.min(lim.max_side)),
        }
    };
    // context: the hole and a generous margin, as much as the provider takes at native size
    let ctx = (rw.max(rh) as f64 * 0.75).max(256.0) as usize;
    let want = (rw + 2 * ctx, rh + 2 * ctx);
    let minimum = (rw + 2 * 48, rh + 2 * 48);
    let (sw, sh) = send_for(want.0, want.1);
    let fits = |w: usize, h: usize| minimum.0 <= w && minimum.1 <= h;
    let crop_around = |cw: usize, ch: usize, cx: f64, cy: f64| -> [usize; 4] {
        let (cw, ch) = (cw.min(src_w), ch.min(src_h));
        let x0 = (cx - cw as f64 / 2.0).clamp(0.0, (src_w - cw) as f64) as usize;
        let y0 = (cy - ch as f64 / 2.0).clamp(0.0, (src_h - ch) as f64) as usize;
        [x0, y0, x0 + cw, y0 + ch]
    };
    let (cx, cy) = ((rect[0] + rect[2]) as f64 / 2.0, (rect[1] + rect[3]) as f64 / 2.0);
    let opacity = (spot.opacity / 100.0).clamp(0.0, 1.0) as f32;
    if fits(sw, sh) {
        // native: the crop is the send size in source pixels (a small photo is sent whole, upscaled)
        let crop = crop_around(sw, sh, cx, cy);
        let send = if lim.sizes.is_empty() { (crop[2] - crop[0], crop[3] - crop[1]) } else { (sw, sh) };
        let scale = (send.0 as f64 / (crop[2] - crop[0]) as f64).min(1.0);
        return Ok(Plan { src_w, src_h, dabs, feather, ring, opacity, rect, tile: Tile { crop, send }, scale });
    }
    // too big for one native crop: one downscaled crop, the area with a little context, sent
    // smaller (a softer patch). Never several pieces: below MIN_SCALE nothing is sent at all.
    let (sw, sh) = send_for(minimum.0, minimum.1);
    let k = (sw as f64 / minimum.0 as f64).min(sh as f64 / minimum.1 as f64).min(1.0);
    let (cw, ch) = (((sw as f64 / k).ceil() as usize).min(src_w), ((sh as f64 / k).ceil() as usize).min(src_h));
    let crop = crop_around(cw, ch, cx, cy);
    let scale = (sw as f64 / (crop[2] - crop[0]) as f64).min(sh as f64 / (crop[3] - crop[1]) as f64).min(1.0);
    if scale < MIN_SCALE {
        return Err(TOO_LARGE.into());
    }
    Ok(Plan { src_w, src_h, dabs, feather, ring, opacity, rect, tile: Tile { crop, send: (sw, sh) }, scale })
}

impl Plan {
    /// Distance (px) from source pixel centre `(x, y)` to the nearest dab's hole edge (≤ 0 inside).
    fn field(&self) -> Plane {
        let [x0, y0, x1, y1] = self.rect;
        let (w, h) = (x1 - x0, y1 - y0);
        let mut d = Plane { width: w, height: h, data: vec![f32::MAX; w * h] };
        let reach = self.feather + self.ring + 2.0;
        for (cx, cy, r) in &self.dabs {
            let (ax, bx) = (((cx - r - reach).floor().max(x0 as f64)) as usize, ((cx + r + reach).ceil().min(x1 as f64)).max(0.0) as usize);
            let (ay, by) = (((cy - r - reach).floor().max(y0 as f64)) as usize, ((cy + r + reach).ceil().min(y1 as f64)).max(0.0) as usize);
            for y in ay..by {
                for x in ax..bx {
                    let v = ((x as f64 + 0.5 - cx).hypot(y as f64 + 0.5 - cy) - r) as f32;
                    if let Some(p) = d.data.get_mut((y - y0) * w + (x - x0))
                        && v < *p
                    {
                        *p = v;
                    }
                }
            }
        }
        d
    }

    /// Requests a generation of `variations` takes with `provider` (`remove`: no prompt): one
    /// for the whole area (some services take one per variation).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn requests(&self, provider: lcc_genfill::cloud::Provider, variations: u32, remove: bool) -> u32 {
        lcc_genfill::cloud::requests_for(provider, variations, remove)
    }
}

/// Crop `[x0,y0,x1,y1)` of `img` resized to `send`.
fn cut(img: &Rgb32f, c: [usize; 4], send: (usize, usize)) -> Rgb32f {
    let (w, h) = (c[2].saturating_sub(c[0]).max(1), c[3].saturating_sub(c[1]).max(1));
    let crop = Rgb32f::from_fn(w, h, |x, y| img.data.get((c[1] + y) * img.width + c[0] + x).copied().unwrap_or_default());
    if (w, h) == send { crop } else { lcc_raster::resample::resize(&crop, send.0, send.1, lcc_raster::resample::Filter::Mitchell) }
}

/// The mask of the crop at its send size (255 = generate): the hole's pixels.
fn tile_mask(p: &Plan, field: &Plane, t: &Tile) -> Vec<u8> {
    let (cw, ch) = ((t.crop[2] - t.crop[0]) as f64, (t.crop[3] - t.crop[1]) as f64);
    let (sw, sh) = t.send;
    let mut m = vec![0u8; sw * sh];
    let [rx0, ry0, rx1, ry1] = p.rect;
    // a send pixel is masked when any source pixel under it is in the hole (so nothing is missed)
    let k = (cw / sw as f64).max(ch / sh as f64).max(1.0);
    for y in 0..sh {
        for x in 0..sw {
            let (sx, sy) = (t.crop[0] as f64 + (x as f64 + 0.5) * cw / sw as f64, t.crop[1] as f64 + (y as f64 + 0.5) * ch / sh as f64);
            let (ix, iy) = (sx as usize, sy as usize);
            if ix < rx0 || iy < ry0 || ix >= rx1 || iy >= ry1 {
                continue;
            }
            let d = field.data.get((iy - ry0) * field.width + (ix - rx0)).copied().unwrap_or(f32::MAX);
            if f64::from(d) < k + 1.0
                && let Some(v) = m.get_mut(y * sw + x)
            {
                *v = 255;
            }
        }
    }
    m
}

/// The most memory one variation's copy of the filled area may take (2 GiB: a 170 MP area).
#[cfg(not(target_arch = "wasm32"))]
const MAX_VARIATION_BYTES: usize = 2 << 30;

/// A copy of `base` for one variation to be pasted into, or an error when it is past
/// [`MAX_VARIATION_BYTES`] or can't be reserved (never an allocation abort).
#[cfg(not(target_arch = "wasm32"))]
fn variation_canvas(base: &Rgb32f) -> Result<Rgb32f, String> {
    let too_big = || format!("not enough memory to fill a {}×{} area; paint a smaller area", base.width, base.height);
    let pixels = base.width.checked_mul(base.height).filter(|n| *n == base.data.len());
    if pixels.and_then(|n| n.checked_mul(std::mem::size_of::<[f32; 3]>())).is_none_or(|b| b > MAX_VARIATION_BYTES) {
        return Err(too_big());
    }
    let mut data = Vec::new();
    data.try_reserve_exact(base.data.len()).map_err(|_| too_big())?;
    data.extend_from_slice(&base.data);
    Ok(Rgb32f { width: base.width, height: base.height, data })
}

#[cfg(not(target_arch = "wasm32"))]
/// One request's results for a tile: decoded to linear light at the crop's native size.
fn receive(img: &lcc_genfill::cloud::Image, neutral: lcc_genfill::Neutral, t: &Tile) -> Option<Rgb32f> {
    let lin = neutral.decode(img.width, img.height, &img.rgb)?;
    Some(to_native(lin, t))
}

/// A result at its crop's native size. Sent smaller, the model's own grain comes back enlarged as
/// blotches: it is smoothed away (the photo's own grain is added when the patch renders).
#[cfg(not(target_arch = "wasm32"))]
fn to_native(lin: Rgb32f, t: &Tile) -> Rgb32f {
    let (cw, ch) = (t.crop[2] - t.crop[0], t.crop[3] - t.crop[1]);
    if (lin.width, lin.height) == (cw, ch) {
        return lin;
    }
    let up = lcc_raster::resample::resize(&lin, cw, ch, lcc_raster::resample::Filter::Mitchell);
    let k = (t.send.0 as f64 / cw.max(1) as f64).min(t.send.1 as f64 / ch.max(1) as f64);
    if k < 0.9 && k > 0.0 { lcc_raster::blur::gaussian(&up, (0.8 / k) as f32) } else { up }
}

#[cfg(not(target_arch = "wasm32"))]
/// Paste the result into the working patch `gen` (rect-sized) where the hole and its margin are.
fn paste(p: &Plan, field: &Plane, t: &Tile, res: &Rgb32f, gen_img: &mut Rgb32f) {
    let [rx0, ry0, rx1, ry1] = p.rect;
    let reach = (p.feather + p.ring * 0.5) as f32;
    for y in t.crop[1].max(ry0)..t.crop[3].min(ry1) {
        for x in t.crop[0].max(rx0)..t.crop[2].min(rx1) {
            let i = (y - ry0) * gen_img.width + (x - rx0);
            if field.data.get(i).is_some_and(|d| *d < reach)
                && let (Some(g), Some(v)) = (gen_img.data.get_mut(i), res.data.get((y - t.crop[1]) * res.width + (x - t.crop[0])))
            {
                *g = *v;
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// The finished patch of one variation: matched to the photo, with its blend weight.
fn finish(src: &Rgb32f, p: &Plan, field: &Plane, mut gen_img: Rgb32f, provider: &str, neutral: lcc_genfill::Neutral) -> Result<PatchLayer, String> {
    let [rx0, ry0, rx1, ry1] = p.rect;
    let orig = Rgb32f::from_fn(rx1 - rx0, ry1 - ry0, |x, y| src.data.get((ry0 + y) * src.width + rx0 + x).copied().unwrap_or_default());
    let area: Vec<bool> = field.data.iter().map(|d| f64::from(*d) < p.feather).collect();
    let ring: Vec<bool> = field.data.iter().map(|d| f64::from(*d) >= p.feather && f64::from(*d) < p.feather + p.ring).collect();
    let _ = lcc_genfill::matching::match_to_surroundings(&mut gen_img, &orig, &area, &ring);
    // outside the blend area the patch is the photo itself (its weight is 0 there)
    for (i, a) in area.iter().enumerate() {
        if !a && let (Some(g), Some(o)) = (gen_img.data.get_mut(i), orig.data.get(i)) {
            *g = *o;
        }
    }
    let alpha = Plane {
        width: field.width,
        height: field.height,
        data: field.data.iter().map(|d| ((1.0 - smooth(0.0, p.feather, f64::from(*d))) as f32) * p.opacity).collect(),
    };
    let (sw, sh) = (p.src_w as f64, p.src_h as f64);
    let info = PatchInfo {
        rect: [rx0 as f64 / sw, ry0 as f64 / sh, rx1 as f64 / sw, ry1 as f64 / sh],
        source_w: p.src_w as u32,
        source_h: p.src_h as u32,
        scale: p.scale,
        provider: provider.to_string(),
        neutral,
        ..Default::default()
    };
    PatchLayer::new(info, gen_img, alpha).map_err(|e| e.to_string())
}

/// Write `layer` to `path` (a temporary name, then renamed).
pub fn write_patch(path: &Path, layer: &PatchLayer) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("could not create the generative fill folder: {e}"))?;
    }
    let tmp = path.with_extension("lcpatch.tmp");
    std::fs::write(&tmp, layer.to_bytes()).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("could not save the generated patch: {e}")
    })?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("could not save the generated patch: {e}")
    })
}

/// Write a generation's record (a temporary name, then renamed).
pub fn write_record(path: &Path, r: &Record) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(r).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("could not save the generated fill's record: {e}")
    })
}

/// Most generations listed for one photo.
const MAX_LISTED: usize = 256;

/// A generation found in a photo's folder ([`Session::genfill_results`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub id: String,
    pub variants: u32,
    pub provider: String,
    pub prompt: String,
    /// Seconds since 1970 (the file's time when there is no record).
    pub made: u64,
    /// The spot that keeps it (shown or among its earlier generations), if any.
    pub spot: Option<usize>,
    /// Whether that spot shows it now.
    pub shown: bool,
}

/// A new generation id: 16 hex digits from the time, the process and a counter.
pub fn new_id(salt: u64) -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let t = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    let mut h = 0xcbf2_9ce4_8422_2325u64
        ^ t
        ^ salt.rotate_left(17)
        ^ u64::from(std::process::id()) << 40
        ^ N.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h = (h ^ (h >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h = (h ^ (h >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    format!("{:016x}", h ^ (h >> 31))
}

/// The encoded crop a generation would send first (the consent dialog shows it), its mask and
/// the transform: `(w, h, rgb8, mask)`.
pub fn preview_crop(src: &Rgb32f, p: &Plan) -> Option<(usize, usize, Vec<u8>, Vec<u8>)> {
    let t = &p.tile;
    let field = p.field();
    let crop = cut(src, t.crop, t.send);
    let neutral = lcc_genfill::Neutral::for_crop(&crop);
    Some((t.send.0, t.send.1, neutral.encode(&crop), tile_mask(p, &field, t)))
}

/// What a generation needs, taken from the session on its thread.
#[cfg(not(target_arch = "wasm32"))]
pub struct Work {
    pub provider: lcc_genfill::cloud::Provider,
    pub key: String,
    pub endpoint: lcc_genfill::cloud::Endpoint,
    pub spot: Spot,
    pub settings: Arc<DevelopSettings>,
    pub source: crate::media::SourceRef,
    pub header: lcc_pipeline::SourceInfo,
    pub prompt: String,
    pub variations: u32,
    pub dir: PathBuf,
    pub id: String,
    /// What the service accepted so far (kept for a Retry).
    pub accepted: Arc<lcc_genfill::cloud::Accepted>,
}

/// The record of a generation kept next to its patches (`<id>.json`), so it can be restored onto the photo
/// whatever happened to its spot.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Record {
    pub fill: GenFill,
    /// When it was made (seconds since 1970).
    #[serde(default)]
    pub made: u64,
}

/// Name of generation `id`'s record file.
pub fn record_name(id: &str) -> String {
    format!("{id}.json")
}

/// Run a generation (on any thread): load the full-size source, send the crop(s), write the
/// patches. `progress(done, total)` after each request.
#[cfg(not(target_arch = "wasm32"))]
pub fn run(w: &Work, cancel: &AtomicBool, cache: &PatchCache, progress: &dyn Fn(u32, u32)) -> Result<Generated, String> {
    use lcc_genfill::cloud::{Control, Request, generate};
    let source = w.source.load_source()?;
    let src = source.image.clone();
    let info = source.info_or(w.header.clone());
    let lim = Limits::for_provider(w.provider);
    let p = plan(&w.spot, &w.settings, info.lens.as_ref(), src.width, src.height, lim)?;
    let remove = w.prompt.trim().is_empty();
    let n = w.variations.clamp(1, MAX_VARIATIONS);
    let total = p.requests(w.provider, n, remove);
    if total > MAX_REQUESTS {
        return Err(format!("this would take {total} requests; paint a smaller area or ask for fewer variations"));
    }
    let field = p.field();
    let [rx0, ry0, rx1, ry1] = p.rect;
    let base = Rgb32f::from_fn(rx1 - rx0, ry1 - ry0, |x, y| src.data.get((ry0 + y) * src.width + rx0 + x).copied().unwrap_or_default());
    let ctl =
        Control { cancel, stall: std::time::Duration::from_secs(180), deadline: std::time::Duration::from_secs(600), accepted: Some(&w.accepted) };
    progress(0, total);
    let seed = u64::from_str_radix(&w.id, 16).unwrap_or(1);
    // one request for the whole area (never several pieces)
    let t = &p.tile;
    let crop = cut(&src, t.crop, t.send);
    let neutral = lcc_genfill::Neutral::for_crop(&crop);
    let req = Request {
        width: t.send.0,
        height: t.send.1,
        rgb: neutral.encode(&crop),
        mask: tile_mask(&p, &field, t),
        prompt: w.prompt.clone(),
        variations: n,
        seed,
    };
    let out = generate(w.provider, &w.key, &w.endpoint, &req, &ctl).map_err(|e| e.to_string())?;
    progress(total, total);
    let count = out.len().min(n as usize);
    if count == 0 {
        return Err("the provider sent no image".into());
    }
    let photo_dir = &w.dir;
    let variants = count as u32;
    let fill = GenFill {
        id: w.id.clone(),
        variants,
        variant: 0,
        provider: w.provider.info().id.to_string(),
        prompt: w.prompt.trim().to_string(),
        stroke: w.spot.stroke_key(),
        scale: p.scale,
        made_for: Some(GenStroke { points: w.spot.points.clone(), size: w.spot.size }),
        earlier: Vec::new(),
        place: 0,
    };
    // one variation at a time, its copy of the area reserved fallibly: the memory a fill takes is one
    // area's worth whatever the number of variations (patches written before the record are unused
    // until it is, so a failure part-way leaves nothing shown)
    for (k, img) in out.iter().take(count).enumerate() {
        let res = receive(img, neutral, t).ok_or("the provider sent an unreadable image")?;
        let mut g = variation_canvas(&base)?;
        paste(&p, &field, t, &res, &mut g);
        let layer = finish(&src, &p, &field, g, w.provider.info().id, neutral)?;
        let path = photo_dir.join(fill.file_name(k as u32));
        write_patch(&path, &layer)?;
        cache.forget(&path);
    }
    // the record that lets it be restored whatever happens to its spot (written before it is shown)
    let made = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    write_record(&photo_dir.join(record_name(&w.id)), &Record { fill: fill.clone(), made })?;
    let mut warnings = Vec::new();
    if p.scale < 0.95 {
        warnings.push(format!(
            "generated at {:.0} % of the photo's resolution: the filled area is softer than the rest (paint a smaller area for a sharper fill)",
            p.scale * 100.0
        ));
    }
    if w.endpoint.is_mock() {
        warnings.push("made by the local mock server (<PREFIX>_GENFILL_MOCK), not a provider".into());
    }
    Ok(Generated { fill, warnings })
}

impl crate::Session {
    /// Keep API keys and consent in the app's settings folder ([`crate::config::config_dir`]).
    pub fn with_default_genfill(mut self) -> Self {
        let dir = crate::config::config_dir();
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.genfill.keys = lcc_genfill::cloud::keys::KeyStore::new(dir.as_ref().map(|d| d.join("genfill-keys.json")));
        }
        self.genfill.consent_path = dir.map(|d| d.join("genfill-consent.json"));
        self
    }

    /// The folder photo `id`'s patches are kept in: the library's `genfill/<id>`, or (a library
    /// that is not on disk) a folder of this session's in the system's temporary folder (photo ids of
    /// libraries that are not on disk repeat, so their fills must not meet).
    pub fn genfill_dir(&self, id: PhotoId) -> PathBuf {
        let root = self
            .library
            .as_ref()
            .filter(|l| l.on_disk)
            .map(|l| l.dir.join(DIR))
            .unwrap_or_else(|| std::env::temp_dir().join("lcc-genfill").join(&self.genfill.scratch.0));
        root.join(id.0.to_string())
    }

    /// The patches photo `p` shows, for its renders (`None` when no spot shows one).
    pub fn genfill_store(&self, p: &Photo) -> Option<GenStore> {
        if !p.develop.spots.iter().any(|s| s.shown_gen().is_some()) {
            return None;
        }
        Some(GenStore(Arc::new(PhotoPatches { dir: self.genfill_dir(p.id), cache: self.genfill.cache.clone() })))
    }

    /// Whether spot `index` of photo `id` shows a patch that can't be read (missing file).
    pub fn genfill_missing(&self, id: PhotoId, index: usize) -> bool {
        let Some(p) = self.catalog.photo(id) else { return false };
        let Some(g) = p.develop.spots.get(index).and_then(|s| s.shown_gen()) else { return false };
        !self.genfill_dir(id).join(g.file_name(g.variant)).is_file()
    }

    /// The first crop generating spot `index` with `provider` would send, as it is sent (8-bit,
    /// through the fixed transform) and its mask, from the photo's preview-size source (the
    /// framing is the real one; the real crop has the photo's full resolution).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn genfill_preview(
        &mut self,
        id: PhotoId,
        index: usize,
        provider: lcc_genfill::cloud::Provider,
    ) -> Result<(usize, usize, Vec<u8>, Vec<u8>), String> {
        let p = self.catalog.photo(id).ok_or("no such photo")?.clone();
        let spot = p.develop.spots.get(index).cloned().ok_or("no such spot")?;
        let src = self.source_now(id, crate::media::SourceLevel::Preview)?;
        // the native plan, scaled to this source (so the framing is the one that will be sent)
        let (nw, nh) = (p.width.max(1) as usize, p.height.max(1) as usize);
        let lim = Limits::for_provider(provider);
        let native = plan(&spot, &p.develop, p.embedded_lens.as_deref(), nw, nh, lim)?;
        let k = src.width as f64 / nw as f64;
        let sc = |v: usize, max: usize| ((v as f64 * k).round() as usize).min(max);
        let mut small = native.clone();
        small.src_w = src.width;
        small.src_h = src.height;
        small.dabs = native.dabs.iter().map(|(x, y, r)| (x * k, y * k, r * k)).collect();
        small.feather *= k;
        small.ring *= k;
        small.rect = [sc(native.rect[0], src.width), sc(native.rect[1], src.height), sc(native.rect[2], src.width), sc(native.rect[3], src.height)];
        small.tile = {
            let t = &native.tile;
            let crop = [sc(t.crop[0], src.width), sc(t.crop[1], src.height), sc(t.crop[2], src.width), sc(t.crop[3], src.height)];
            let (cw, ch) = ((crop[2] - crop[0]).max(1), (crop[3] - crop[1]).max(1));
            // at most the crop's own pixels at this size (a preview is never upscaled)
            let f = (cw as f64 / t.send.0 as f64).min(ch as f64 / t.send.1 as f64).min(1.0);
            Tile { crop, send: (((t.send.0 as f64 * f).round() as usize).max(1), ((t.send.1 as f64 * f).round() as usize).max(1)) }
        };
        preview_crop(&src, &small).ok_or_else(|| "nothing to send".into())
    }

    /// [`Session::genfill_preview`] by provider id (an error where generative fill isn't available).
    pub fn genfill_preview_by_id(&mut self, id: PhotoId, index: usize, provider: &str) -> Result<(usize, usize, Vec<u8>, Vec<u8>), String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let pr = lcc_genfill::cloud::Provider::from_id(provider).ok_or("unknown provider")?;
            self.genfill_preview(id, index, pr)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (id, index, provider);
            Err("generative fill is not available in the web version".into())
        }
    }

    /// Accept (or withdraw) `provider`'s terms.
    pub fn genfill_consent(&mut self, provider: &str, accept: bool) -> Result<(), String> {
        let today: String = (self.clock)().chars().take(10).collect();
        self.genfill.set_consent(provider, accept, &today)
    }

    /// Start generating spot `index` of photo `id` (see the module docs); waits unless
    /// [`GenfillState::background`]. Returns the finished generation when it waited.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn genfill_start(
        &mut self,
        id: PhotoId,
        index: usize,
        provider: lcc_genfill::cloud::Provider,
        prompt: &str,
        variations: u32,
    ) -> Result<Option<Generated>, String> {
        use crate::activity::Cancel;
        let pid = provider.info().id;
        if self.genfill.running() {
            return Err("a generative fill is already running; wait for it or cancel it".into());
        }
        if !self.genfill.consented(pid) {
            return Err(format!("accept {}'s terms first (the consent dialog, or genfill.consent)", provider.info().name));
        }
        let (key, _) = self.genfill.keys.get(provider).ok_or_else(|| lcc_genfill::cloud::GenError::NoKey.to_string())?;
        let p = self.catalog.photo(id).ok_or("no such photo")?.clone();
        let spot = p.develop.spots.get(index).cloned().ok_or("no such spot")?;
        if spot.points.is_empty() {
            return Err("the spot has no area".into());
        }
        if prompt.chars().count() > 1000 {
            return Err("the prompt is too long (at most 1000 characters)".into());
        }
        // an area too large for one request is refused here, before any activity or upload (the
        // worker plans again on the full-size source)
        if p.width >= 16 && p.height >= 16 {
            plan(&spot, &p.develop, p.embedded_lens.as_deref(), p.width as usize, p.height as usize, Limits::for_provider(provider))?;
        }
        // a generation the service accepted before failing is fetched, not sent (and paid for) again
        let n = variations.clamp(1, MAX_VARIATIONS);
        let resume = self
            .genfill
            .resume
            .clone()
            .filter(|r| r.photo == id && r.stroke == spot.stroke_key() && r.provider == pid && r.prompt == prompt.trim() && r.variations == n)
            .filter(|r| r.at.elapsed() < RESUME_FOR);
        let (gen_id, accepted) = match &resume {
            Some(r) => (r.id.clone(), r.accepted.clone()),
            None => (new_id(id.0), Arc::new(lcc_genfill::cloud::Accepted::default())),
        };
        let work = Work {
            provider,
            key,
            endpoint: match &self.genfill.mock {
                Some(m) => lcc_genfill::cloud::Endpoint::mock(m)?,
                None => lcc_genfill::cloud::Endpoint::for_provider(provider),
            },
            spot: spot.clone(),
            settings: p.develop.clone(),
            source: self.media.source_ref_uncached(&p, crate::media::SourceLevel::Full),
            header: crate::media::source_info(&p),
            prompt: prompt.trim().to_string(),
            variations: n,
            dir: self.genfill_dir(id),
            id: gen_id.clone(),
            accepted: accepted.clone(),
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let job = Arc::new(Job {
            photo: id,
            spot: index,
            stroke: spot.stroke_key(),
            provider: pid.to_string(),
            spot_at: spot.clone(),
            state: Mutex::new(JobState::Running { done: 0, total: 0 }),
            result: Mutex::new(None),
            cancel: cancel.clone(),
            resume: Resume {
                photo: id,
                stroke: spot.stroke_key(),
                provider: pid.to_string(),
                prompt: prompt.trim().to_string(),
                variations: n,
                id: gen_id,
                accepted,
                at: web_time::Instant::now(),
            },
        });
        let guard = self.activity.start("genfill", "Generating fill", Cancel::Flag(cancel.clone()));
        guard.detail(provider.info().name);
        let handle = guard.handle();
        let cache = self.genfill.cache.clone();
        let j = job.clone();
        let body = move || {
            let progress = |d: u32, t: u32| {
                handle.progress(u64::from(d), u64::from(t));
                j.set(JobState::Running { done: d, total: t });
            };
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&work, &cancel, &cache, &progress)))
                .unwrap_or_else(|_| Err("the generative fill stopped unexpectedly".into()));
            match r {
                Ok(g) => {
                    *j.result.lock().unwrap_or_else(PoisonError::into_inner) = Some(g);
                    j.set(JobState::Done);
                }
                Err(_) if cancel.load(Ordering::Relaxed) => j.set(JobState::Cancelled),
                Err(e) => j.set(JobState::Failed(e)),
            }
        };
        self.genfill.job = Some(job.clone());
        if self.genfill.background {
            std::thread::Builder::new()
                .name("genfill".into())
                .spawn(move || {
                    let _guard = guard;
                    body();
                })
                .map_err(|e| format!("could not start the generative fill: {e}"))?;
            return Ok(None);
        }
        body();
        drop(guard);
        match self.genfill_poll().into_iter().next() {
            Some(Err(e)) => Err(e),
            Some(Ok(g)) => Ok(Some(g)),
            None => Err("the generative fill ended without a result".into()),
        }
    }

    /// Apply a finished generation to its spot (one undo step) and report it. Cheap when nothing
    /// is running: the app calls it every frame.
    pub fn genfill_poll(&mut self) -> Option<Result<Generated, String>> {
        let job = self.genfill.job.clone()?;
        // what the service accepted before a failure or a cancel is kept for a Retry (it is billed)
        #[cfg(not(target_arch = "wasm32"))]
        let kept = |s: &mut Self| -> bool {
            if job.resume.accepted.is_empty() {
                return false;
            }
            s.genfill.resume = Some(Resume { at: web_time::Instant::now(), ..job.resume.clone() });
            true
        };
        #[cfg(target_arch = "wasm32")]
        let kept = |_: &mut Self| false;
        let out = match job.state() {
            JobState::Running { .. } => return None,
            JobState::Cancelled => {
                kept(self);
                Err("cancelled".to_string())
            }
            JobState::Failed(e) => {
                if kept(self) {
                    Err(format!("{e}. The service already made it: Retry within a few minutes fetches it without paying again."))
                } else {
                    Err(e)
                }
            }
            JobState::Done => {
                #[cfg(not(target_arch = "wasm32"))]
                if self.genfill.resume.as_ref().is_some_and(|r| r.id == job.resume.id) {
                    self.genfill.resume = None;
                }
                let g = job.result.lock().unwrap_or_else(PoisonError::into_inner).take();
                match g {
                    None => Err("the generative fill ended without a result".into()),
                    Some(mut g) => self.genfill_apply(&job, &g.fill).map(|note| {
                        g.warnings.extend(note);
                        g
                    }),
                }
            }
        };
        self.genfill.job = None;
        self.genfill.last = Some(out.clone());
        Some(out)
    }

    /// Put a finished generation on its spot (one undo step). It always lands: on the spot it was made for, wherever
    /// that spot went meanwhile (spots added or deleted before it), else back on its stroke (the spot was moved or
    /// resized meanwhile), else on a spot of its own again (the spot was deleted). Generate Again keeps the spot's
    /// earlier generations. Returns a note when the spot had changed.
    fn genfill_apply(&mut self, job: &Job, fill: &GenFill) -> Result<Option<String>, String> {
        let mut d = (*self.develop_of(job.photo).ok_or("the photo is gone")?).clone();
        let same = |s: &Spot| s.stroke_key() == job.stroke && s.mode == SpotMode::Generate;
        let found = d
            .spots
            .get(job.spot)
            .filter(|s| same(s))
            .map(|_| job.spot)
            .or_else(|| d.spots.iter().position(same))
            .or_else(|| d.spots.get(job.spot).filter(|s| s.mode == SpotMode::Generate).map(|_| job.spot));
        let (index, note) = match found {
            Some(i) if d.spots.get(i).is_some_and(same) => (i, None),
            Some(i) => (i, Some("The spot was changed while it was generating: it is back where the fill was made.".to_string())),
            None => {
                d.spots.push(Spot { mode: SpotMode::Generate, generated: None, ..job.spot_at.clone() });
                (d.spots.len() - 1, Some("The spot was removed while it was generating: it is back with its fill.".to_string()))
            }
        };
        let sp = d.spots.get_mut(index).ok_or("the spot is gone")?;
        sp.mode = SpotMode::Generate;
        sp.generated = Some(match &sp.generated {
            Some(prev) => prev.followed_by(fill.clone()),
            None => fill.clone(),
        });
        sp.put_back();
        self.set_develop(job.photo, d, "Generative Fill").map_err(|e| e.to_string())?;
        if self.active() == Some(job.photo) && note.is_some() {
            self.active_spot = Some(index);
        }
        Ok(note)
    }

    /// The generations in photo `id`'s folder, newest first: each one's record, and the spot that keeps it.
    pub fn genfill_results(&self, id: PhotoId) -> Vec<Found> {
        let dir = self.genfill_dir(id);
        let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
        let mut found: BTreeMap<String, Found> = BTreeMap::new();
        for e in entries.flatten().take(MAX_LISTED * 8) {
            let name = e.file_name().to_string_lossy().to_string();
            let Some((which, variant)) = name.strip_suffix(".lcpatch").and_then(|s| s.split_once('-')) else { continue };
            let (Some(v), true) = (variant.parse::<u32>().ok(), GenFill::valid_id(which)) else { continue };
            let made = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            let f = found.entry(which.to_string()).or_insert_with(|| Found {
                id: which.to_string(),
                variants: 0,
                provider: String::new(),
                prompt: String::new(),
                made,
                spot: None,
                shown: false,
            });
            f.variants = f.variants.max(v.saturating_add(1));
        }
        let spots = self.develop_of(id).map(|d| d.spots.clone()).unwrap_or_default();
        let mut out: Vec<Found> = found
            .into_values()
            .map(|mut f| {
                if let Some(r) = read_record(&dir.join(record_name(&f.id))) {
                    (f.provider, f.prompt) = (r.fill.provider, r.fill.prompt);
                    f.made = if r.made > 0 { r.made } else { f.made };
                }
                f.spot = spots.iter().position(|s| s.generated.as_ref().is_some_and(|g| g.generations().iter().any(|x| x.id == f.id)));
                f.shown = spots.iter().any(|s| s.shown_gen().is_some_and(|g| g.id == f.id));
                f
            })
            .collect();
        out.sort_by(|a, b| b.made.cmp(&a.made).then(a.id.cmp(&b.id)));
        out.truncate(MAX_LISTED);
        out
    }

    /// Show generation `which` of photo `id` again (one undo step): on the spot that keeps it (put back on its stroke
    /// if the spot moved), else on a new spot where it was made (from its record, the photo's history, or the patch
    /// itself). Returns the spot's index, which becomes the selected spot.
    pub fn genfill_restore(&mut self, id: PhotoId, which: &str) -> Result<usize, String> {
        if !GenFill::valid_id(which) {
            return Err("not a generated fill's id".into());
        }
        let dir = self.genfill_dir(id);
        if !dir.join(format!("{which}-0.lcpatch")).is_file() {
            return Err("this generated fill is not in the catalog any more".into());
        }
        let mut d = (*self.develop_of(id).ok_or("no such photo")?).clone();
        let kept = d.spots.iter().position(|s| s.generated.as_ref().is_some_and(|g| g.generations().iter().any(|x| x.id == which)));
        let index = match kept {
            Some(i) => {
                let sp = d.spots.get_mut(i).ok_or("no such spot")?;
                sp.mode = SpotMode::Generate;
                if let Some(g) = &mut sp.generated {
                    let gens = g.generations();
                    let first: u32 = gens.iter().take_while(|x| x.id != which).map(|x| x.variants.max(1)).fold(0, u32::saturating_add);
                    if g.id != which {
                        g.choose(first);
                    }
                }
                sp.put_back();
                i
            }
            None => {
                let mut sp = self.genfill_spot_for(id, which).ok_or("could not tell where this generated fill belongs")?;
                sp.put_back();
                d.spots.push(sp);
                d.spots.len() - 1
            }
        };
        self.set_develop(id, d, "Restore Generated Fill").map_err(|e| e.to_string())?;
        if self.active() == Some(id) {
            self.active_spot = Some(index);
        }
        Ok(index)
    }

    /// A Generate spot showing generation `which` where it was made: from its record, else from a step of the photo's
    /// history that showed it, else around its patch (fills made before records were kept).
    fn genfill_spot_for(&self, id: PhotoId, which: &str) -> Option<Spot> {
        let dir = self.genfill_dir(id);
        if let Some(r) = read_record(&dir.join(record_name(which)))
            && let Some(at) = r.fill.made_for.clone()
        {
            let fill = GenFill { stroke: at.key(), ..r.fill };
            return Some(Spot { mode: SpotMode::Generate, points: at.points, size: at.size, generated: Some(fill), ..Default::default() });
        }
        let p = self.catalog.photo(id)?;
        for step in p.history.iter().rev() {
            for s in &step.settings.spots {
                if let Some(g) = s.shown_gen().filter(|g| g.id == which) {
                    let fill =
                        GenFill { made_for: Some(GenStroke { points: s.points.clone(), size: s.size }), earlier: Vec::new(), place: 0, ..g.clone() };
                    return Some(Spot { generated: Some(fill), ..s.clone() });
                }
            }
        }
        // only the patch: a spot over it (the patch carries its own shape, the spot marks where it is)
        let layer = read_patch(&dir.join(format!("{which}-0.lcpatch"))).ok()?;
        let [x0, y0, x1, y1] = layer.info.rect;
        let frame = lcc_pipeline::geometry::Frame::with_lens(
            p.width.max(1) as usize,
            p.height.max(1) as usize,
            &p.develop,
            false,
            p.embedded_lens.as_deref(),
        );
        let (sw, sh) = (f64::from(layer.info.source_w.max(1)), f64::from(layer.info.source_h.max(1)));
        // its centre, oriented (lens corrections and perspective left out: the spot only marks the place)
        let (cx, cy) = frame.orient.map((x0 + x1) / 2.0, (y0 + y1) / 2.0, 1.0, 1.0);
        let fin = |v: f64, lo: f64, hi: f64, or: f64| if v.is_finite() { v.clamp(lo, hi) } else { or };
        let at = GenStroke {
            points: vec![Point::new(fin(cx, 0.0, 1.0, 0.5), fin(cy, 0.0, 1.0, 0.5))],
            size: fin(((x1 - x0) * sw).min((y1 - y0) * sh) / 2.0 / sw.max(sh), 0.002, 0.25, 0.02),
        };
        let variants = (0..MAX_VARIATIONS).take_while(|v| dir.join(format!("{which}-{v}.lcpatch")).is_file()).count() as u32;
        let fill = GenFill {
            id: which.to_string(),
            variants: variants.max(1),
            provider: layer.info.provider.clone(),
            stroke: at.key(),
            scale: layer.info.scale,
            made_for: Some(at.clone()),
            ..Default::default()
        };
        Some(Spot { mode: SpotMode::Generate, points: at.points, size: at.size, generated: Some(fill), ..Default::default() })
    }

    /// Put spot `index` of photo `id` back on the stroke its fill was made for (one undo step): from the fill's
    /// record of it, else from a step of the photo's history where the spot showed it.
    pub fn genfill_put_back(&mut self, id: PhotoId, index: usize) -> Result<(), String> {
        let mut d = (*self.develop_of(id).ok_or("no such photo")?).clone();
        let sp = d.spots.get_mut(index).ok_or("no such spot")?;
        let g = sp.generated.clone().ok_or("this spot has no generated fill")?;
        if sp.shown_gen().is_some() {
            return Ok(());
        }
        if !sp.put_back() {
            let p = self.catalog.photo(id).ok_or("no such photo")?;
            let old = p
                .history
                .iter()
                .rev()
                .flat_map(|h| h.settings.spots.iter())
                .find(|s| s.stroke_key() == g.stroke)
                .ok_or("where this fill was made is not known any more")?;
            (sp.points, sp.size) = (old.points.clone(), old.size);
            if let Some(f) = &mut sp.generated {
                f.made_for = Some(GenStroke { points: old.points.clone(), size: old.size });
            }
        }
        sp.mode = SpotMode::Generate;
        self.set_develop(id, d, "Put Back Generated Fill").map_err(|e| e.to_string())
    }

    /// Cancel the running generation (if any).
    pub fn genfill_cancel(&mut self) -> bool {
        match &self.genfill.job {
            Some(j) if matches!(j.state(), JobState::Running { .. }) => {
                j.cancel();
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_variation_past_the_memory_budget_is_an_error() {
        let base = Rgb32f::filled(6, 4, [0.25, 0.5, 0.75]);
        assert_eq!(variation_canvas(&base).expect("a small area copies"), base);
        // claims 2^20 × 2^20 pixels (12 TiB): refused before anything is reserved
        let huge = Rgb32f { width: 1 << 20, height: 1 << 20, data: Vec::new() };
        assert!(variation_canvas(&huge).is_err());
        let overflow = Rgb32f { width: usize::MAX, height: 3, data: Vec::new() };
        assert!(variation_canvas(&overflow).is_err());
    }

    fn spot(x: f64, y: f64, size: f64) -> Spot {
        Spot { mode: SpotMode::Generate, points: vec![Point::new(x, y)], size, ..Default::default() }
    }

    const OPENAI: Limits = Limits { max_side: 1536, sizes: &[(1024, 1024), (1536, 1024), (1024, 1536)] };
    const FREE: Limits = Limits { max_side: 2048, sizes: &[] };

    #[test]
    fn a_small_hole_is_sent_at_native_size_with_context() {
        let s = DevelopSettings::default();
        let p = plan(&spot(0.5, 0.5, 0.02), &s, None, 6000, 4000, OPENAI).unwrap();
        let t = &p.tile;
        assert_eq!(t.send, (t.crop[2] - t.crop[0], t.crop[3] - t.crop[1]), "native: one source pixel per sent pixel");
        assert!(matches!(t.send, (1024, 1024) | (1536, 1024)));
        assert!((p.scale - 1.0).abs() < 1e-9);
        // the crop holds the whole patch rectangle
        assert!(t.crop[0] <= p.rect[0] && t.crop[2] >= p.rect[2] && t.crop[1] <= p.rect[1] && t.crop[3] >= p.rect[3]);
        let free = plan(&spot(0.5, 0.5, 0.02), &s, None, 6000, 4000, FREE).unwrap();
        assert!(free.tile.crop[2] - free.tile.crop[0] <= 2048);
    }

    /// Never several pieces (decided): a larger area goes as one crop, sent smaller; one that
    /// would come back at under half the photo's resolution is refused with the decided wording,
    /// a long thin one (a wire across the frame) too.
    #[test]
    fn a_large_area_goes_as_one_crop_or_not_at_all() {
        let s = DevelopSettings::default();
        let mid = plan(&spot(0.5, 0.5, 0.06), &s, None, 6000, 4000, OPENAI).unwrap();
        assert!(mid.scale < 0.95 && mid.scale >= MIN_SCALE, "{}", mid.scale);
        let t = &mid.tile;
        assert!(t.crop[0] <= mid.rect[0] && t.crop[2] >= mid.rect[2] && t.crop[1] <= mid.rect[1] && t.crop[3] >= mid.rect[3]);
        // the 4-piece case of the first real test: people painted out of a tall portrait
        let big = Spot { points: vec![Point::new(0.45, 0.4), Point::new(0.5, 0.5), Point::new(0.55, 0.6)], ..spot(0.0, 0.0, 0.09) };
        assert_eq!(plan(&big, &s, None, 5464, 8192, OPENAI).unwrap_err(), TOO_LARGE);
        let wire = Spot { points: (0..60).map(|i| Point::new(0.1 + i as f64 * 0.013, 0.3)).collect(), ..spot(0.0, 0.0, 0.003) };
        assert_eq!(plan(&wire, &s, None, 6000, 4000, FREE).unwrap_err(), TOO_LARGE);
        assert_eq!(TOO_LARGE, "The model can't take an area this large. Please select a smaller area.");
    }

    #[test]
    fn a_rotated_photo_places_the_hole_on_the_source() {
        let s = DevelopSettings { orientation: lcc_geom::Orientation::Rotate90, ..Default::default() };
        // top-left of the rotated view = bottom-left of the stored image
        let p = plan(&spot(0.1, 0.1, 0.01), &s, None, 6000, 4000, FREE).unwrap();
        let (x, y, _) = p.dabs[0];
        assert!(x < 1000.0 && y > 3000.0, "{x} {y}");
    }

    #[test]
    fn hostile_spots_are_errors() {
        let s = DevelopSettings::default();
        for sp in [
            spot(f64::NAN, 0.5, 0.02),
            spot(5.0, 5.0, 0.02),
            spot(0.5, 0.5, 0.0),
            spot(0.5, 0.5, f64::INFINITY),
            Spot { points: vec![], ..spot(0.5, 0.5, 0.02) },
        ] {
            assert!(plan(&sp, &s, None, 6000, 4000, OPENAI).is_err(), "{sp:?}");
        }
        assert!(plan(&spot(0.5, 0.5, 0.02), &s, None, 8, 8, OPENAI).is_err());
        // a photo smaller than the provider's size is sent whole
        let p = plan(&spot(0.5, 0.5, 0.05), &s, None, 640, 480, OPENAI).unwrap();
        assert_eq!(p.tile.crop, [0, 0, 640, 480]);
    }

    #[test]
    fn ids_are_hex_and_differ() {
        let (a, b) = (new_id(1), new_id(1));
        assert!(GenFill::valid_id(&a) && GenFill::valid_id(&b) && a != b);
    }
}
