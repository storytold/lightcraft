//! Running SAM 3's backbone on the Apple Neural Engine, next to the candle path it replaces when it is better.
//!
//! The backbone is the one slow, once-per-photo step. [`NeuralEngine`] turns it into a Core ML program
//! ([`crate::coreml_vit`]), has Core ML compile and load it (about a minute and a half the first time, the compiled
//! model is kept; later loads take 20 s), and then [`crate::Sam3::encode_pixels`] may use it. Nothing here blocks a
//! photo: the set-up runs on its own thread while candle keeps working, and any problem leaves candle in charge.
//!
//! The Neural Engine computes in 16 bit, so it is checked before it is trusted: the first photo encoded after the
//! set-up goes through both, and the Neural Engine is used from then on only when its features agree with candle's
//! ([`MIN_COSINE`]) and it is not much slower ([`MAX_SLOWDOWN`]; it draws about a third of the energy of the graphics
//! card for the same photo, so a little slower is still better). The verdict is kept beside the model, so later runs
//! do not repeat the check. A tile that fails or gives numbers that are not finite is encoded by candle, and after
//! [`MAX_FALLBACKS`] of them the Neural Engine is left alone.
//!
//! A set-up that crashes the process leaves a marker with this version's number; the next run of the same version does
//! not try again (a crash loop would otherwise make every launch fail), a newer version does.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use lightcraft_coreml::{ComputeUnits, Model, compile};

use crate::weights::Weights;

/// The writer's version: part of the name the compiled model is kept under, so a better writer makes a new one.
const WRITER_VERSION: u32 = 1;
/// Agreement with candle's features (cosine similarity of the whole backbone output) needed to use the Neural Engine.
/// Measured on real photos: 0.9994 to 0.9999.
pub const MIN_COSINE: f64 = 0.995;
/// How much slower than candle the Neural Engine may be and still be used (for its energy).
pub const MAX_SLOWDOWN: f64 = 1.25;
/// Tiles it may get wrong before it is left alone.
pub const MAX_FALLBACKS: usize = 3;
/// The set-up's time limit (a first compile of the whole backbone on a slow computer).
const SETUP_LIMIT: std::time::Duration = std::time::Duration::from_secs(20 * 60);

/// Where the Neural Engine path stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Not started.
    Idle,
    /// Being made, compiled or loaded.
    Building,
    /// Loaded; the first photo it meets is also run by candle to check it.
    Checking,
    /// Checked and used. Times are one backbone on the Neural Engine and on candle (ms), and their agreement.
    Using { ane_ms: f64, candle_ms: f64, cosine: f64 },
    /// Not used, and why (an English sentence: a crash, no Neural Engine, a mismatch, too slow).
    Off(String),
}

enum Slot {
    Idle,
    Building,
    Ready(Arc<Ready>),
    Off(String),
}

pub(crate) struct Ready {
    model: Model,
    dir: PathBuf,
    verdict: Mutex<Verdict>,
}

#[derive(Clone, Debug)]
enum Verdict {
    Unchecked,
    Good { ane_ms: f64, candle_ms: f64, cosine: f64 },
    Bad(String),
}

/// The Neural Engine path of one [`crate::Sam3`].
pub struct NeuralEngine {
    slot: Mutex<Slot>,
    fallbacks: AtomicUsize,
}

impl Default for NeuralEngine {
    fn default() -> Self {
        NeuralEngine { slot: Mutex::new(Slot::Idle), fallbacks: AtomicUsize::new(0) }
    }
}

/// What [`NeuralEngine::plan`] says to do with the next backbone.
pub(crate) enum Plan {
    /// Candle alone.
    Candle,
    /// The Neural Engine, with candle when it fails.
    Ane(Arc<Ready>),
    /// Both, to check the Neural Engine against candle (candle's answer is the one used).
    Check(Arc<Ready>),
}

/// Whether this build and computer can have a Neural Engine at all.
pub const fn possible() -> bool {
    cfg!(all(target_os = "macos", target_arch = "aarch64"))
}

impl NeuralEngine {
    fn lock(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Where the path stands.
    pub fn status(&self) -> Status {
        match &*self.lock() {
            Slot::Idle => Status::Idle,
            Slot::Building => Status::Building,
            Slot::Off(why) => Status::Off(why.clone()),
            Slot::Ready(r) => match r.verdict.lock().unwrap_or_else(PoisonError::into_inner).clone() {
                Verdict::Unchecked => Status::Checking,
                Verdict::Good { ane_ms, candle_ms, cosine } => Status::Using { ane_ms, candle_ms, cosine },
                Verdict::Bad(why) => Status::Off(why),
            },
        }
    }

    /// Sets the status by hand, for the tests of code that reads it (never used by the app).
    #[doc(hidden)]
    pub fn pretend(&self, status: Status) {
        *self.lock() = match status {
            Status::Idle => Slot::Idle,
            Status::Building | Status::Checking => Slot::Building,
            Status::Using { .. } => Slot::Idle,
            Status::Off(why) => Slot::Off(why),
        };
    }

    /// Starts making and loading the Neural Engine's model in `cache` on its own thread (once: later calls do
    /// nothing). `weights` are the checkpoint's. Never blocks and never fails: the outcome is in [`Self::status`].
    pub(crate) fn start(self: &Arc<Self>, weights: Arc<Weights>, cache: PathBuf) {
        {
            let mut slot = self.lock();
            if !matches!(*slot, Slot::Idle) {
                return;
            }
            if !possible() {
                *slot = Slot::Off("this computer has no Neural Engine".into());
                return;
            }
            *slot = Slot::Building;
        }
        let me = Arc::clone(self);
        let spawned = std::thread::Builder::new().name("sam3-neural-engine".into()).spawn(move || {
            let started = std::time::Instant::now();
            let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| set_up(&weights, &cache)))
                .unwrap_or_else(|_| Err("setting up the Neural Engine failed unexpectedly".into()));
            let late = started.elapsed() > SETUP_LIMIT;
            let mut slot = me.lock();
            *slot = match made {
                Ok(_) if late => Slot::Off(format!("setting it up took longer than {} minutes", SETUP_LIMIT.as_secs() / 60)),
                Ok(r) => {
                    log::info!("SAM 3: the Neural Engine's model is ready after {:?}", started.elapsed());
                    Slot::Ready(Arc::new(r))
                }
                Err(why) => {
                    log::info!("SAM 3: not using the Neural Engine ({why})");
                    Slot::Off(why)
                }
            };
        });
        if let Err(e) = spawned {
            *self.lock() = Slot::Off(format!("could not start its thread: {e}"));
        }
    }

    /// What to do with the next backbone, given whether candle ran one before (its time then is a warm one).
    pub(crate) fn plan(&self, candle_warm: bool) -> Plan {
        let slot = self.lock();
        let Slot::Ready(r) = &*slot else {
            return Plan::Candle;
        };
        if self.fallbacks.load(Ordering::Relaxed) >= MAX_FALLBACKS {
            return Plan::Candle;
        }
        match &*r.verdict.lock().unwrap_or_else(PoisonError::into_inner) {
            Verdict::Good { .. } => Plan::Ane(Arc::clone(r)),
            Verdict::Unchecked if candle_warm => Plan::Check(Arc::clone(r)),
            _ => Plan::Candle,
        }
    }

    /// Counts a backbone the Neural Engine got wrong (an error, numbers that are not finite).
    pub(crate) fn fell_back(&self, why: &str) {
        let n = self.fallbacks.fetch_add(1, Ordering::Relaxed) + 1;
        if n == 1 {
            log::warn!("SAM 3: the Neural Engine failed on a photo ({why}); candle does it");
        } else if n == MAX_FALLBACKS {
            log::warn!("SAM 3: the Neural Engine failed {n} times; candle does the rest");
        }
    }
}

impl Ready {
    /// The backbone on the Neural Engine: the first layer's input `[1, tokens, hidden]` in, the last layer's out.
    pub(crate) fn run(&self, shape: &[usize], input: &[f32]) -> Result<Vec<f32>, String> {
        let out = self.model.predict(shape, input).map_err(|e| e.to_string())?;
        if out.shape != shape {
            return Err(format!("its answer is shaped {:?}", out.shape));
        }
        if !out.data.iter().all(|v| v.is_finite()) {
            return Err("its answer has numbers that are not finite".into());
        }
        Ok(out.data)
    }

    /// Records what comparing it with candle showed, and keeps it beside the model.
    pub(crate) fn judge(&self, ane_ms: f64, candle_ms: f64, cosine: f64) {
        let verdict = if cosine.is_nan() || cosine < MIN_COSINE {
            Verdict::Bad(format!("its features differ from candle's (agreement {cosine:.4}, {MIN_COSINE} needed)"))
        } else if ane_ms > candle_ms * MAX_SLOWDOWN {
            Verdict::Bad(format!("it is slower than candle on this computer ({ane_ms:.0} ms against {candle_ms:.0} ms)"))
        } else {
            Verdict::Good { ane_ms, candle_ms, cosine }
        };
        log::info!("SAM 3: Neural Engine {ane_ms:.0} ms, candle {candle_ms:.0} ms, agreement {cosine:.5}: {verdict:?}");
        let text = match &verdict {
            Verdict::Good { ane_ms, candle_ms, cosine } => {
                format!("{{\"use\":true,\"aneMs\":{ane_ms},\"candleMs\":{candle_ms},\"cosine\":{cosine}}}")
            }
            Verdict::Bad(why) => serde_json::json!({"use": false, "why": why}).to_string(),
            Verdict::Unchecked => return,
        };
        let _ = std::fs::write(self.dir.join(VERDICT), text);
        *self.verdict.lock().unwrap_or_else(PoisonError::into_inner) = verdict;
    }
}

const VERDICT: &str = "verdict.json";
const OUTPUT: &str = "output";
const MARKER: &str = "setup.marker";

/// The verdict kept beside a model, if there is one.
fn read_verdict(dir: &Path) -> Verdict {
    let Some(v) = std::fs::read_to_string(dir.join(VERDICT)).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) else {
        return Verdict::Unchecked;
    };
    match (v["use"].as_bool(), v["aneMs"].as_f64(), v["candleMs"].as_f64(), v["cosine"].as_f64()) {
        (Some(true), Some(a), Some(c), Some(cos)) if cos >= MIN_COSINE && a <= c * MAX_SLOWDOWN => {
            Verdict::Good { ane_ms: a, candle_ms: c, cosine: cos }
        }
        (Some(false), ..) => Verdict::Bad(v["why"].as_str().unwrap_or("it was left out earlier").to_string()),
        _ => Verdict::Unchecked,
    }
}

/// The folder a model is kept in under `cache`: it names the writer and the checkpoint (its size identifies the release).
fn model_dir(cache: &Path, weights: &Weights) -> PathBuf {
    cache.join(format!("v{WRITER_VERSION}-{:x}", weights.file_len()))
}

/// Folders of a set-up that never finished, older than this, are removed.
const STALE: std::time::Duration = std::time::Duration::from_secs(3600);

fn remove_stale_parts(cache: &Path) {
    let Ok(entries) = std::fs::read_dir(cache) else { return };
    for e in entries.flatten() {
        let name = e.file_name();
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > STALE);
        if name.to_string_lossy().starts_with(".part-") && old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

static PARTS: AtomicUsize = AtomicUsize::new(0);

/// Makes (when it is not kept), compiles and loads the model.
fn set_up(weights: &Arc<Weights>, cache: &Path) -> Result<Ready, String> {
    std::fs::create_dir_all(cache).map_err(|e| format!("{}: {e}", cache.display()))?;
    let dir = model_dir(cache, weights);
    let marker = cache.join(MARKER);
    let version = env!("CARGO_PKG_VERSION");
    if std::fs::read_to_string(&marker).is_ok_and(|v| v.trim() == version) {
        return Err("setting it up closed LightCraft last time".into());
    }
    std::fs::write(&marker, version).map_err(|e| format!("{}: {e}", marker.display()))?;
    let result = (|| {
        if let Some(r) = load_kept(&dir) {
            return Ok(r);
        }
        remove_stale_parts(cache);
        let _ = std::fs::remove_dir_all(&dir);
        let part = cache.join(format!(".part-{}-{}", std::process::id(), PARTS.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&part);
        let made = (|| {
            let vb = crate::Sam3::vb(weights, &candle_core::Device::Cpu).pp("detector_model.vision_encoder.backbone");
            let (package, output) = crate::coreml_vit::write_backbone(&vb, &crate::coreml_vit::Dims::SAM3, &part.join("backbone.mlpackage"))
                .map_err(|e| e.to_string())?;
            let compiled = compile(&package, &part).map_err(|e| e.to_string())?;
            // the package is as large as the compiled model: only that is kept
            let _ = std::fs::remove_dir_all(&package);
            std::fs::write(part.join(OUTPUT), &output).map_err(|e| e.to_string())?;
            std::fs::rename(&compiled, part.join("net.mlmodelc")).map_err(|e| e.to_string())?;
            std::fs::rename(&part, &dir).map_err(|e| format!("{}: {e}", dir.display()))
        })();
        if let Err(why) = made {
            let _ = std::fs::remove_dir_all(&part);
            return Err(why);
        }
        load_kept(&dir).ok_or_else(|| "the Core ML model made for it does not load".to_string())
    })();
    // reaching here means the compile and the load did not take the process down
    let _ = std::fs::remove_file(&marker);
    result
}

/// The model kept in `dir`, loaded for the Neural Engine; `None` (and the folder removed) when it does not load.
fn load_kept(dir: &Path) -> Option<Ready> {
    let output = std::fs::read_to_string(dir.join(OUTPUT)).ok()?;
    let compiled = dir.join("net.mlmodelc");
    if !compiled.exists() {
        return None;
    }
    match Model::load(&compiled, ComputeUnits::CpuAndNeuralEngine, "x", output.trim()) {
        Ok(model) => Some(Ready { model, dir: dir.to_path_buf(), verdict: Mutex::new(read_verdict(dir)) }),
        Err(e) => {
            log::warn!("SAM 3: the kept Neural Engine model does not load ({e}); it is made again");
            let _ = std::fs::remove_dir_all(dir);
            None
        }
    }
}

/// Cosine similarity of two equally long arrays (`NaN` when either has no length).
pub(crate) fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let (mut dot, mut na, mut nb) = (0f64, 0f64, 0f64);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (f64::from(*x), f64::from(*y));
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    dot / (na.sqrt() * nb.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_of_equal_and_opposite_arrays() {
        let a = [1.0f32, 2.0, 3.0];
        assert!((cosine(&a, &a) - 1.0).abs() < 1e-12);
        assert!((cosine(&a, &[-1.0, -2.0, -3.0]) + 1.0).abs() < 1e-12);
        assert!(cosine(&[], &[]).is_nan());
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lc-sam-ane-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_kept_verdict_is_read_back_and_checked_again() {
        let d = scratch("verdict");
        assert!(matches!(read_verdict(&d), Verdict::Unchecked));
        std::fs::write(d.join(VERDICT), r#"{"use":true,"aneMs":3000,"candleMs":4000,"cosine":0.9995}"#).unwrap();
        assert!(matches!(read_verdict(&d), Verdict::Good { .. }));
        // a verdict that would not pass today's limits is not trusted
        std::fs::write(d.join(VERDICT), r#"{"use":true,"aneMs":3000,"candleMs":4000,"cosine":0.9}"#).unwrap();
        assert!(matches!(read_verdict(&d), Verdict::Unchecked));
        std::fs::write(d.join(VERDICT), r#"{"use":true,"aneMs":9000,"candleMs":4000,"cosine":0.9999}"#).unwrap();
        assert!(matches!(read_verdict(&d), Verdict::Unchecked));
        std::fs::write(d.join(VERDICT), r#"{"use":false,"why":"slower"}"#).unwrap();
        assert!(matches!(read_verdict(&d), Verdict::Bad(w) if w == "slower"));
        std::fs::write(d.join(VERDICT), "not json").unwrap();
        assert!(matches!(read_verdict(&d), Verdict::Unchecked));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn stale_parts_go_and_fresh_ones_stay() {
        let d = scratch("parts");
        let (old, fresh, other) = (d.join(".part-1-0"), d.join(".part-1-1"), d.join("v1-abc"));
        for p in [&old, &fresh, &other] {
            std::fs::create_dir_all(p).unwrap();
        }
        let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 3600);
        std::fs::File::open(&old).unwrap().set_modified(hour_ago).unwrap();
        remove_stale_parts(&d);
        assert!(!old.exists() && fresh.exists() && other.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_kept_model_that_does_not_load_is_removed_not_trusted() {
        let d = scratch("broken");
        let model = d.join("v1-1");
        std::fs::create_dir_all(model.join("net.mlmodelc")).unwrap();
        std::fs::write(model.join(OUTPUT), "out").unwrap();
        assert!(load_kept(&model).is_none());
        assert!(!model.exists() || !possible_to_load(), "a model that cannot load is removed");
        assert!(load_kept(&d.join("nothing")).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Core ML says no to a folder that is not a model on macOS; elsewhere nothing is loaded and nothing removed.
    fn possible_to_load() -> bool {
        lightcraft_coreml::supported()
    }

    #[test]
    fn the_status_follows_the_slot() {
        let ne = Arc::new(NeuralEngine::default());
        assert_eq!(ne.status(), Status::Idle);
        assert!(matches!(ne.plan(true), Plan::Candle));
        *ne.lock() = Slot::Off("because".into());
        assert_eq!(ne.status(), Status::Off("because".into()));
        assert!(matches!(ne.plan(true), Plan::Candle));
    }
}
