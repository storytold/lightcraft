//! AI Defringe, the session's side (docs/ai-defringe.md): the model the user installed, the correction each photo
//! gets from it, and making the missing ones in the background.
//!
//! A photo's correction is *cached data*: the model reads the picture at the Defringe stage (the full-size source with
//! the default develop settings, so the edit never makes it again), and what it changes is kept as a per-pixel,
//! per-channel gain on the source picture, only for the pixels it changes. It is kept in
//! `<library>/ai-defringe/<key>.lcadf`, named by a hash of the photo's content, the model file and this algorithm's
//! version. A render multiplies the source picture by it, scaled by the Amount, before the develop pipeline; a
//! render smaller than the source takes the mean gain over each of its pixels.
//!
//! What runs where: the session thread decides (which photo, which model) and keeps the corrections in memory. One
//! background thread at a time decodes a photo and runs the model ([`Recipe::make`]). An export that finds its
//! photo's correction missing makes it itself, on its own thread, so it never comes out without the correction it
//! was asked for.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex, PoisonError};

use lightcraft_catalog::{Photo, PhotoId};
use lightcraft_develop::DevelopSettings;
use lightcraft_pipeline::SourceInfo;
use lightcraft_preview::Hasher128;
use lightcraft_raster::Rgb32f;
use serde::{Deserialize, Serialize};

use crate::Session;
use crate::media::{SourceLevel, SourceRef};

/// The composition and the correction's form; a change makes every correction again.
const VERSION: &str = "ai-defringe-2";
/// The manifest that describes a defringe model, beside its ONNX file.
pub const MANIFEST: &str = "defringe-model.json";
/// What a model must say it does: [`lightcraft_denoise::defringe`]'s planes in, (Δa, Δb, gate) out.
pub const CONTRACT: &str = "lightcraftDefringe1";
/// The folder corrections are kept in, inside the library's folder.
const DIR: &str = "ai-defringe";
/// Corrections kept in memory (each is a few megabytes: only the pixels it changes).
const IN_MEMORY: usize = 24;
/// The largest model file accepted.
const MAX_MODEL_BYTES: u64 = 256 << 20;

/// What a defringe model is and whose it is, from the `defringe-model.json` beside its file (untrusted).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    /// The licence the weights are under, as the model's author states it.
    pub licence: String,
    /// The ONNX file, beside the manifest.
    pub file: String,
    pub sha256: String,
    pub contract: String,
    /// Anything the user should know before using it (training data, terms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest, String> {
        if text.len() > 64 * 1024 {
            return Err(format!("{MANIFEST} is too large"));
        }
        let m: Manifest = serde_json::from_str(text).map_err(|e| format!("{MANIFEST}: {e}"))?;
        let short = |name: &str, v: &str, max: usize| -> Result<(), String> {
            if v.trim().is_empty() || v.len() > max || v.chars().any(char::is_control) {
                return Err(format!("{MANIFEST}: `{name}` must be 1 to {max} characters of text"));
            }
            Ok(())
        };
        short("id", &m.id, 64)?;
        if !m.id.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) {
            return Err(format!("{MANIFEST}: `id` may only use letters, digits, '-', '_' and '.'"));
        }
        short("name", &m.name, 120)?;
        short("version", &m.version, 40)?;
        short("licence", &m.licence, 200)?;
        short("file", &m.file, 120)?;
        if m.file.contains(['/', '\\']) || m.file.starts_with('.') || !m.file.to_ascii_lowercase().ends_with(".onnx") {
            return Err(format!("{MANIFEST}: `file` must be the name of an .onnx file beside it"));
        }
        if m.sha256.len() != 64 || !m.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("{MANIFEST}: `sha256` must be 64 hexadecimal digits"));
        }
        if m.contract != CONTRACT {
            return Err(format!("{MANIFEST}: this LightCraft runs defringe models of contract `{CONTRACT}`, not `{}`", m.contract));
        }
        if let Some(n) = &m.notice
            && n.len() > 4000
        {
            return Err(format!("{MANIFEST}: `notice` is too long"));
        }
        if let Some(u) = &m.url {
            short("url", u, 400)?;
        }
        Ok(Manifest { sha256: m.sha256.to_ascii_lowercase(), ..m })
    }
}

/// Whether this build can run defringe models.
pub fn runtime_available() -> bool {
    cfg!(feature = "denoise")
}

/// The folder the installed model is kept in: `$LIGHTCRAFT_DEFRINGE_MODELS` if set, else `<config>/defringe-models`.
pub fn models_dir() -> Option<PathBuf> {
    std::env::var_os("LIGHTCRAFT_DEFRINGE_MODELS")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::config::config_dir().map(|d| d.join("defringe-models")))
}

/// The folder inside the models folder that LightCraft owns, and the only thing there it writes or deletes: the
/// models folder may be one the user keeps other files in. It holds the installed model, and during an install the
/// new one being written and the previous one, kept until the new one is in place.
const OWN: &str = "installed";
/// The installed model, inside [`OWN`].
const CURRENT: &str = "model";
/// The previous model while a new one replaces it, inside [`OWN`].
const PREVIOUS: &str = "previous";

/// The model installed in models folder `dir`: its manifest and file, if there is one.
pub fn installed(dir: &Path) -> Option<(Manifest, PathBuf)> {
    let cur = dir.join(OWN).join(CURRENT);
    let m = Manifest::parse(&std::fs::read_to_string(cur.join(MANIFEST)).ok()?).ok()?;
    let file = cur.join(&m.file);
    file.is_file().then_some((m, file))
}

/// The manifest beside model file `onnx`, checked, without running the model. A file with no `defringe-model.json`
/// describing it (any ONNX the user has) gets one made up from the file: its name, its SHA-256, and a licence that
/// says none was given. Installing it then checks that it really is a defringe model by running it.
pub fn inspect(onnx: &Path) -> Result<Manifest, String> {
    let dir = onnx.parent().ok_or("the model file has no folder")?;
    let name = onnx.file_name().and_then(|n| n.to_str()).ok_or("the model file has no name")?;
    if let Ok(text) = std::fs::read_to_string(dir.join(MANIFEST)) {
        let m = Manifest::parse(&text)?;
        if m.file == name {
            return Ok(m);
        }
    }
    bare(onnx, name)
}

/// The manifest of an ONNX file that came without one.
fn bare(onnx: &Path, name: &str) -> Result<Manifest, String> {
    let len = std::fs::metadata(onnx).map_err(|e| e.to_string())?.len();
    if len > MAX_MODEL_BYTES {
        return Err("the model file is too large".into());
    }
    let sha256 = lightcraft_denoise_core::hash::sha256_file(onnx).map_err(|e| e.to_string())?;
    let stem = onnx.file_stem().and_then(|n| n.to_str()).unwrap_or("model");
    let id: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '-' }).take(64).collect();
    let plain = |v: &str| v.chars().filter(|c| !c.is_control()).take(120).collect::<String>();
    let file = if name.len() <= 120 && !name.starts_with('.') && !name.contains(['/', '\\']) && name.to_ascii_lowercase().ends_with(".onnx") {
        name.to_string()
    } else {
        "model.onnx".into()
    };
    let m = Manifest {
        id: if id.trim_matches(['-', '.', '_']).is_empty() { "model".into() } else { id },
        name: Some(plain(stem)).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Defringe model".into()),
        version: sha256.get(..8).unwrap_or("1").to_string(),
        licence: "Not stated".into(),
        file,
        sha256,
        contract: CONTRACT.into(),
        notice: Some(format!(
            "This model came without a {MANIFEST}, so LightCraft doesn't know who made it or under which terms. \
             Use it only if you are allowed to. LightCraft checks that it runs as a defringe model before installing it."
        )),
        url: None,
    };
    // the made-up manifest must read back as a valid one once installed
    Manifest::parse(&serde_json::to_string(&m).map_err(|e| e.to_string())?)
}

fn read_model_file(path: &Path, m: &Manifest) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    // read at most one byte past the limit, whatever the file says its size is
    file.take(MAX_MODEL_BYTES + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_MODEL_BYTES {
        return Err("the model file is too large".into());
    }
    if lightcraft_denoise_core::hash::sha256_hex(&bytes) != m.sha256 {
        return Err("the model file's SHA-256 is not the one its manifest gives".into());
    }
    Ok(bytes)
}

/// Install the model `onnx` (with its manifest beside it, or without one: [`inspect`]) in models folder `dir`, after
/// checking its hash and running it once: it replaces the installed one. `accepted` is the manifest the user was shown
/// and accepted ([`inspect`]'s answer); if the file or its manifest changed since, nothing is installed.
pub fn install(onnx: &Path, dir: &Path, accepted: &Manifest) -> Result<Manifest, String> {
    let m = inspect(onnx)?;
    if &m != accepted {
        return Err("the model or its description changed after you accepted it: install it again to see its terms".into());
    }
    // the bytes read here are the ones checked, run and installed
    let bytes = read_model_file(onnx, &m)?;
    self_test(&bytes)?;
    let own = dir.join(OWN);
    std::fs::create_dir_all(&own).map_err(|e| e.to_string())?;
    let (cur, prev) = (own.join(CURRENT), own.join(PREVIOUS));
    // a previous installation left behind by an install that could not put it back is restored first, never deleted
    if !cur.exists() && prev.exists() {
        std::fs::rename(&prev, &cur).map_err(|e| format!("couldn't restore the previous model from {}: {e}", prev.display()))?;
    }
    let staging = own.join(format!("installing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let write = || -> std::io::Result<()> {
        std::fs::write(staging.join(&m.file), &bytes)?;
        std::fs::write(staging.join(MANIFEST), serde_json::to_vec_pretty(&m).unwrap_or_default())
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e.to_string());
    }
    if prev.exists() {
        std::fs::remove_dir_all(&prev).map_err(|e| e.to_string())?;
    }
    if cur.exists() {
        std::fs::rename(&cur, &prev).map_err(|e| e.to_string())?;
    }
    if let Err(e) = std::fs::rename(&staging, &cur) {
        let _ = std::fs::remove_dir_all(&staging);
        if prev.exists()
            && let Err(back) = std::fs::rename(&prev, &cur)
        {
            return Err(format!("couldn't install the model ({e}), nor put the previous one back ({back}); it is kept in {}", prev.display()));
        }
        return Err(e.to_string());
    }
    let _ = std::fs::remove_dir_all(&prev);
    Ok(m)
}

/// Delete the model installed in models folder `dir` (only LightCraft's own folder in it; corrections already made
/// stay until the cache is cleared).
pub fn remove(dir: &Path) -> Result<bool, String> {
    let own = dir.join(OWN);
    let had = own.join(CURRENT).exists();
    if own.exists() {
        std::fs::remove_dir_all(&own).map_err(|e| e.to_string())?;
    }
    Ok(had)
}

#[cfg(feature = "denoise")]
pub type Model = lightcraft_denoise::defringe::Model;

/// A stand-in in builds without the model runtime: nothing can be loaded.
#[cfg(not(feature = "denoise"))]
pub struct Model;

#[cfg(feature = "denoise")]
fn load_model(bytes: &[u8]) -> Result<Model, String> {
    let net = lightcraft_denoise::onnx::read_bytes(bytes).map_err(|e| e.to_string())?;
    Model::new(&net).map_err(|e| e.to_string())
}

#[cfg(not(feature = "denoise"))]
fn load_model(_: &[u8]) -> Result<Model, String> {
    Err("this build of LightCraft can't run AI models".into())
}

/// Load the model and check it on a synthetic fringe: it runs, gives finite colours, gives the same answer twice
/// and keeps lightness.
#[cfg(feature = "denoise")]
fn self_test(bytes: &[u8]) -> Result<(), String> {
    let model = load_model(bytes)?;
    let (w, h) = (300, 260);
    let rgb: Vec<[f32; 3]> = (0..w * h)
        .map(|i| match i % w {
            0..120 => [1.0, 1.0, 1.0],
            120..122 => [0.45, 0.08, 0.6],
            _ => [0.18, 0.18, 0.18],
        })
        .collect();
    let clip = vec![false; w * h];
    let run = || model.correct(&rgb, &clip, w, h, &|_, _| {}, &AtomicBool::new(false)).map_err(|e| e.to_string());
    let (a, b) = (run()?, run()?);
    if a != b {
        return Err("the model gives different answers to the same picture".into());
    }
    for (i, p) in &a {
        let old = rgb.get(*i as usize).copied().unwrap_or_default();
        let (l0, l1) = (lightcraft_denoise::defringe::oklab(old)[0], lightcraft_denoise::defringe::oklab(*p)[0]);
        if !p.iter().all(|v| v.is_finite()) || (l0 - l1).abs() > 1e-3 {
            return Err("the model's correction changes lightness or isn't finite".into());
        }
    }
    Ok(())
}

#[cfg(not(feature = "denoise"))]
fn self_test(bytes: &[u8]) -> Result<(), String> {
    load_model(bytes).map(|_| ())
}

/// A photo's correction: for the pixels the model changed, how much each channel of the source picture is
/// multiplied by (minus 1, as half floats).
pub struct Correction {
    pub key: u64,
    w: usize,
    h: usize,
    index: Vec<u32>,
    gain: Vec<[u16; 3]>,
    /// The last source it was applied to, at an amount (thousandths), and the result: renders of one view reuse the
    /// corrected picture, so the stage cache keeps working while sliders move. Both are weak: the memo keeps no picture
    /// alive (each would be a whole picture), and a source freed and replaced at the same address never matches.
    memo: Mutex<Option<Memo>>,
}

struct Memo {
    source: std::sync::Weak<Rgb32f>,
    thousandths: u32,
    result: std::sync::Weak<Rgb32f>,
}

impl Correction {
    pub fn changed(&self) -> usize {
        self.index.len()
    }

    /// `src` with the correction applied at `amount` (0..1). A source smaller than the correction gets, for each of
    /// its pixels, the mean gain over the pixels of the correction it covers.
    pub fn apply(&self, src: &Arc<Rgb32f>, amount: f32) -> Arc<Rgb32f> {
        if amount.is_nan() || amount <= 0.0 || self.index.is_empty() || src.width == 0 || src.height == 0 {
            return src.clone();
        }
        let k = amount.min(1.0);
        let thousandths = (k * 1000.0).round() as u32;
        let mut memo = self.memo.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(m) = memo.as_ref()
            && m.thousandths == thousandths
            && m.source.upgrade().is_some_and(|s| Arc::ptr_eq(&s, src))
            && let Some(img) = m.result.upgrade()
        {
            return img;
        }
        let (w, h) = (src.width, src.height);
        let mut out = (**src).clone();
        if (w, h) == (self.w, self.h) {
            for (&i, g) in self.index.iter().zip(&self.gain) {
                if let Some(p) = out.data.get_mut(i as usize) {
                    for (v, &d) in p.iter_mut().zip(g) {
                        *v *= 1.0 + f16_value(d) * k;
                    }
                }
            }
        } else {
            // each pixel of the render takes the mean gain over the pixels of the correction that map to it (how many
            // depends on where it is when the sizes don't divide)
            let covers = |big: usize, small: usize| -> Vec<u32> {
                let mut n = vec![0u32; small];
                for x in 0..big {
                    if let Some(c) = n.get_mut((x * small / big).min(small - 1)) {
                        *c += 1;
                    }
                }
                n
            };
            let (nx, ny) = (covers(self.w, w), covers(self.h, h));
            let mut acc: HashMap<usize, [f32; 3]> = HashMap::new();
            for (&i, g) in self.index.iter().zip(&self.gain) {
                let (x, y) = (i as usize % self.w, i as usize / self.w);
                let (dx, dy) = ((x * w / self.w).min(w - 1), (y * h / self.h).min(h - 1));
                let a = acc.entry(dy * w + dx).or_default();
                for (s, &d) in a.iter_mut().zip(g) {
                    *s += f16_value(d);
                }
            }
            for (i, s) in acc {
                let area = (nx.get(i % w).copied().unwrap_or(1) * ny.get(i / w).copied().unwrap_or(1)).max(1) as f32;
                if let Some(p) = out.data.get_mut(i) {
                    for (v, d) in p.iter_mut().zip(s) {
                        *v *= 1.0 + d / area * k;
                    }
                }
            }
        }
        let out = Arc::new(out);
        *memo = Some(Memo { source: Arc::downgrade(src), thousandths, result: Arc::downgrade(&out) });
        out
    }

    /// Write the correction to `path` through a temporary file of this writer's own, if `publish` still allows it
    /// (the cache was not cleared since the work started). Another writer that published the same correction first
    /// is a success.
    #[cfg_attr(not(feature = "denoise"), allow(dead_code))]
    fn write(&self, path: &Path, publish: &Publish) -> Result<(), String> {
        let mut body = Vec::with_capacity(self.index.len() * 10);
        for (i, g) in self.index.iter().zip(&self.gain) {
            body.extend_from_slice(&i.to_le_bytes());
            for d in g {
                body.extend_from_slice(&d.to_le_bytes());
            }
        }
        let packed = miniz_oxide::deflate::compress_to_vec(&body, 6);
        let mut b = Vec::with_capacity(packed.len() + 48);
        b.extend_from_slice(b"LCADF1\0\0");
        b.extend_from_slice(&self.key.to_le_bytes());
        for v in [self.w as u64, self.h as u64, self.index.len() as u64, body.len() as u64, fnv(&body)] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&packed);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let tmp = path.with_extension(format!("{}-{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::write(&tmp, &b).map_err(|e| e.to_string())?;
        let epoch = publish.epoch.lock().unwrap_or_else(PoisonError::into_inner);
        let done = if *epoch != publish.at {
            Err("the AI Defringe cache was cleared while this correction was made".into())
        } else {
            match std::fs::rename(&tmp, path) {
                Ok(()) => Ok(()),
                Err(_) if Correction::read(path, self.key).is_some() => Ok(()),
                Err(e) => Err(e.to_string()),
            }
        };
        drop(epoch);
        let _ = std::fs::remove_file(&tmp);
        done
    }

    /// The correction at `path`, if it is one, for `key`, and intact.
    fn read(path: &Path, key: u64) -> Option<Correction> {
        let b = std::fs::read(path).ok()?;
        let word = |i: usize| b.get(8 + i * 8..16 + i * 8).and_then(|s| s.try_into().ok()).map(u64::from_le_bytes);
        if b.get(..8)? != b"LCADF1\0\0" || word(0)? != key {
            return None;
        }
        let (w, h, n, len, sum) = (word(1)? as usize, word(2)? as usize, word(3)? as usize, word(4)? as usize, word(5)?);
        if w == 0 || h == 0 || w.checked_mul(h)? > u32::MAX as usize || n > w * h || len != n * 10 {
            return None;
        }
        let body = miniz_oxide::inflate::decompress_to_vec_with_limit(b.get(56..)?, len).ok()?;
        if body.len() != len || fnv(&body) != sum {
            return None;
        }
        let (mut index, mut gain) = (Vec::with_capacity(n), Vec::with_capacity(n));
        for r in body.as_chunks::<10>().0 {
            let i = u32::from_le_bytes(r.get(..4)?.try_into().ok()?);
            if i as usize >= w * h || index.last().is_some_and(|&l| l >= i) {
                return None;
            }
            index.push(i);
            let d = |k: usize| r.get(4 + 2 * k..6 + 2 * k).and_then(|s| s.try_into().ok()).map(u16::from_le_bytes);
            gain.push([d(0)?, d(1)?, d(2)?]);
        }
        Some(Correction { key, w, h, index, gain, memo: Mutex::new(None) })
    }
}

fn fnv(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &v| (h ^ u64::from(v)).wrapping_mul(0x0100_0000_01b3))
}

#[cfg_attr(not(feature = "denoise"), allow(dead_code))]
fn f16_bits(v: f32) -> u16 {
    let b = v.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let e = ((b >> 23) & 0xff) as i32 - 127 + 15;
    let m = b & 0x7f_ffff;
    if v.is_nan() || e >= 31 {
        return sign | 0x7bff; // the largest finite value
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = (m | 0x80_0000) >> (1 - e);
        return sign | ((m + 0x1000) >> 13) as u16;
    }
    let r = (((e as u32) << 10) | (m >> 13)) + ((m >> 12) & 1);
    sign | r.min(0x7bff) as u16
}

fn f16_value(b: u16) -> f32 {
    let (sign, e, m) = (b >> 15, i32::from((b >> 10) & 31), f32::from(b & 1023));
    let v = if e == 0 { m / 1024.0 * 2f32.powi(-14) } else { (1.0 + m / 1024.0) * 2f32.powi(e - 15) };
    if sign == 1 { -v } else { v }
}

/// Whether a correction may still be published: the cache's epoch when its work started, and the cache's epoch now
/// (bumped, under the lock, when the cache is cleared).
#[derive(Clone)]
pub struct Publish {
    epoch: Arc<Mutex<u64>>,
    at: u64,
}

/// The name of photo `p`'s correction by the model with SHA-256 `model`.
pub fn key(p: &Photo, model: &str) -> u64 {
    let mut h = Hasher128::new();
    h.str(VERSION).str(model).str(&crate::media::content_key(p));
    h.finish().0 as u64 | 1
}

/// Everything needed to make one photo's correction, on any thread.
#[derive(Clone)]
pub struct Recipe {
    pub photo: PhotoId,
    pub key: u64,
    source: SourceRef,
    info: SourceInfo,
    model: Arc<Model>,
    path: Option<PathBuf>,
    publish: Publish,
}

impl Recipe {
    /// Decode the photo at full size, run the model on its Defringe-stage picture, keep the result on disk.
    #[cfg(feature = "denoise")]
    pub fn make(&self, progress: &(dyn Fn(usize, usize) + Sync), cancel: &AtomicBool) -> Result<Correction, String> {
        let source = self.source.load_source()?;
        let src = source.image.clone();
        let info = source.info_or(self.info.clone());
        let req = lightcraft_pipeline::RenderRequest { apply_crop: false, ..lightcraft_pipeline::RenderRequest::fit(src.width, src.height) };
        let (tap, frame) = lightcraft_pipeline::defringe_tap(&src, &info, &DevelopSettings::default(), &req);
        let map = frame.source_map(src.width, src.height, tap.width, tap.height);
        let Some(map) = map.filter(|_| (tap.width, tap.height) == (src.width, src.height)) else {
            return Err("the Defringe stage's picture doesn't line up with the photo".into());
        };
        let (w, h) = (tap.width, tap.height);
        let top = lightcraft_denoise::defringe::top(&tap.data);
        let rgb: Vec<[f32; 3]> = tap.data.iter().map(|p| p.map(|v| (v / top).max(0.0))).collect();
        drop(tap);
        let clip = lightcraft_denoise::defringe::clip_mask(&rgb, w, h, 1.0);
        let changed = self.model.correct(&rgb, &clip, w, h, progress, cancel).map_err(|e| e.to_string())?;
        // as a gain on the source picture: the Defringe stage is the source after per-channel gains (white balance,
        // exposure), so the ratio the model made carries over
        let ratios = changed.into_iter().filter_map(|(i, p)| {
            let old = rgb.get(i as usize)?;
            Some((i, [0, 1, 2].map(|c| if old[c] > 1e-4 { (p[c] / old[c]).clamp(0.2, 5.0) - 1.0 } else { 0.0 })))
        });
        let (index, gain) = to_source(ratios, &map, w, h);
        let c = Correction { key: self.key, w, h, index, gain, memo: Mutex::new(None) };
        if let Some(path) = &self.path {
            c.write(path, &self.publish)?;
        }
        Ok(c)
    }

    #[cfg(not(feature = "denoise"))]
    pub fn make(&self, _: &(dyn Fn(usize, usize) + Sync), _: &AtomicBool) -> Result<Correction, String> {
        let _ = (&self.source, &self.info, &self.model, &self.path, &self.publish);
        Err("this build of LightCraft can't run AI models".into())
    }
}

/// The gains (minus 1) the model made at Defringe-stage pixels, moved to the source pixels each plane of those
/// pixels was read from (`map`): lateral chromatic aberration correction and lens corrections read the planes of
/// a pixel from slightly different places, and the correction is applied to the source. Each plane's gain is
/// spread over the four source pixels around where it was read, with bilinear weights, and each source pixel gets
/// the weighted mean of what reached it. The unchanged pixels around the changed ones count too, with no gain, so
/// a gain that is the same over an area stays the same and the edge of a corrected area doesn't jump to full gain.
#[cfg_attr(not(feature = "denoise"), allow(dead_code))]
fn to_source(
    changed: impl Iterator<Item = (u32, [f32; 3])>,
    map: &lightcraft_pipeline::geometry::SourceMap,
    w: usize,
    h: usize,
) -> (Vec<u32>, Vec<[u16; 3]>) {
    let changed: HashMap<u32, [f32; 3]> = changed.filter(|e| e.1 != [0.0; 3]).collect();
    let mut out: Vec<(u32, [u16; 3])> = if map.is_identity() {
        changed.into_iter().map(|(i, g)| (i, g.map(f16_bits))).collect()
    } else {
        // the changed pixels and the unchanged ones within 2 px of them, each visited once, in a fixed order so
        // the sums come out the same every time
        let mut keys: Vec<u32> = changed.keys().copied().collect();
        keys.sort_unstable();
        let mut seen: HashSet<u32> = HashSet::with_capacity(changed.len());
        let around = keys.into_iter().flat_map(|i| {
            let (x, y) = (i as usize % w, i as usize / w);
            (y.saturating_sub(2)..(y + 3).min(h)).flat_map(move |ny| (x.saturating_sub(2)..(x + 3).min(w)).map(move |nx| (ny * w + nx) as u32))
        });
        // per source pixel and plane: the sum of weight × gain, and of weight
        let mut sums: HashMap<u32, [[f32; 2]; 3]> = HashMap::new();
        for i in around {
            if !seen.insert(i) {
                continue;
            }
            let g = changed.get(&i).copied().unwrap_or([0.0; 3]);
            let (x, y) = (i as usize % w, i as usize / w);
            for (c, &gc) in g.iter().enumerate() {
                let Some(p) = map.at(x, y, c) else { continue };
                let (fx, fy) = (p.x - 0.5, p.y - 0.5);
                let (x0, y0) = (fx.floor(), fy.floor());
                let (tx, ty) = ((fx - x0) as f32, (fy - y0) as f32);
                for (dx, dy, wgt) in [(0, 0, (1.0 - tx) * (1.0 - ty)), (1, 0, tx * (1.0 - ty)), (0, 1, (1.0 - tx) * ty), (1, 1, tx * ty)] {
                    let (sx, sy) = (x0 as i64 + dx, y0 as i64 + dy);
                    if wgt <= 0.0 || sx < 0 || sy < 0 || sx >= w as i64 || sy >= h as i64 {
                        continue;
                    }
                    let e = sums.entry((sy as usize * w + sx as usize) as u32).or_insert([[0.0; 2]; 3]);
                    e[c][0] += gc * wgt;
                    e[c][1] += wgt;
                }
            }
        }
        sums.into_iter().map(|(i, s)| (i, s.map(|[gw, wt]| f16_bits(if wt > 1e-6 { gw / wt } else { 0.0 })))).collect()
    };
    out.retain(|e| e.1 != [0; 3]);
    out.sort_unstable_by_key(|e| e.0);
    out.into_iter().unzip()
}

/// What a render does about AI Defringe.
#[derive(Clone)]
pub enum ForRender {
    /// Apply this correction at this amount.
    Ready(Arc<Correction>, f32),
    /// Make the correction first (an export whose photo has none yet).
    Make(Box<Recipe>, f32),
}

impl ForRender {
    /// `src` corrected, making the correction first if needed.
    pub fn apply(&self, src: &Arc<Rgb32f>) -> Result<Arc<Rgb32f>, String> {
        match self {
            ForRender::Ready(c, a) => Ok(c.apply(src, *a)),
            ForRender::Make(r, a) => Ok(r.make(&|_, _| {}, &AtomicBool::new(false))?.apply(src, *a)),
        }
    }

    pub fn key(&self) -> u64 {
        match self {
            ForRender::Ready(c, _) => c.key,
            ForRender::Make(r, _) => r.key,
        }
    }
}

struct Job {
    photo: PhotoId,
    key: u64,
    rx: Receiver<Result<Correction, String>>,
    done: Arc<AtomicUsize>,
    total: Arc<AtomicUsize>,
    cancel: Arc<AtomicBool>,
    /// Set when the pump cancelled it because it was no longer wanted (not the user, from Activity).
    unwanted: AtomicBool,
    /// The cache's epoch when it started: a result from before a clear is dropped.
    epoch: u64,
    _task: crate::activity::TaskGuard,
}

/// The session's AI Defringe state.
#[derive(Default)]
pub struct State {
    /// The installed model, loaded: its SHA-256 and the model.
    model: Option<(String, Arc<Model>)>,
    /// The installed model's manifest, and when it was last read.
    installed: Option<(Manifest, PathBuf)>,
    seen: Option<web_time::Instant>,
    job: Option<Job>,
    /// Corrections that failed, by key, and why (not tried again until asked).
    failed: HashMap<u64, String>,
    /// Corrections the user cancelled, by key (not started again until asked: Try Again, or switching it on).
    cancelled: HashSet<u64>,
    /// The cache's epoch, bumped (under this lock) when it is cleared ([`Publish`]).
    epoch: Arc<Mutex<u64>>,
    /// Corrections in memory, most recently used last.
    ready: Vec<(PhotoId, Arc<Correction>)>,
    /// Bumped whenever a correction arrives (renders made before it are stale).
    pub generation: u64,
    /// The models folder in place of [`models_dir`] (tests, hosts with their own).
    pub(crate) models_dir: Option<PathBuf>,
}

/// Where a photo's correction stands.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum PhotoState {
    /// AI Defringe is off for it.
    Off,
    /// No model is installed (or this build can't run one).
    NoModel,
    Ready {
        changed: usize,
    },
    Running {
        done: usize,
        total: usize,
    },
    Failed {
        error: String,
    },
    Waiting,
}

impl Session {
    /// Where the defringe model is installed.
    pub fn aidefringe_models_dir(&self) -> Option<PathBuf> {
        self.aidefringe.models_dir.clone().or_else(models_dir)
    }

    /// Use `dir` for the defringe model instead of the default folder.
    pub fn set_aidefringe_models_dir(&mut self, dir: Option<PathBuf>) {
        self.aidefringe.models_dir = dir;
        self.aidefringe_model_changed();
    }

    fn aidefringe_dir(&self) -> Option<PathBuf> {
        self.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.join(DIR))
    }

    /// Read which model is installed (at most every couple of seconds unless `force`).
    pub fn aidefringe_refresh(&mut self, force: bool) {
        let st = &mut self.aidefringe;
        if !force && st.seen.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(2)) {
            return;
        }
        st.seen = Some(web_time::Instant::now());
        st.installed = st.models_dir.clone().or_else(models_dir).and_then(|d| installed(&d));
        if st.model.as_ref().map(|m| &m.0) != st.installed.as_ref().map(|i| &i.0.sha256) {
            st.model = None;
        }
    }

    /// The installed model's manifest.
    pub fn aidefringe_model(&mut self) -> Option<Manifest> {
        self.aidefringe_refresh(false);
        self.aidefringe.installed.as_ref().map(|i| i.0.clone())
    }

    fn aidefringe_loaded(&mut self) -> Result<(String, Arc<Model>), String> {
        self.aidefringe_refresh(false);
        let (m, file) = self.aidefringe.installed.clone().ok_or("no defringe model is installed: install one in Settings > AI Defringe")?;
        if let Some(l) = &self.aidefringe.model {
            return Ok(l.clone());
        }
        let model = Arc::new(load_model(&read_model_file(&file, &m)?)?);
        self.aidefringe.model = Some((m.sha256.clone(), model.clone()));
        Ok((m.sha256, model))
    }

    fn aidefringe_recipe(&mut self, id: PhotoId) -> Result<Recipe, String> {
        let (sha, model) = self.aidefringe_loaded()?;
        let p = self.catalog.photo(id).ok_or("no such photo")?.clone();
        let key = key(&p, &sha);
        let path = self.aidefringe_dir().map(|d| d.join(format!("{key:016x}.lcadf")));
        let source = self.media.source_ref_uncached(&p, SourceLevel::Full);
        let publish = Publish { epoch: self.aidefringe.epoch.clone(), at: self.aidefringe_epoch() };
        Ok(Recipe { photo: id, key, source, info: self.source_info(id), model, path, publish })
    }

    fn aidefringe_epoch(&self) -> u64 {
        *self.aidefringe.epoch.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The installed model's key for photo `p`, if a model is installed.
    fn aidefringe_key(&mut self, p: &Photo) -> Option<u64> {
        self.aidefringe_refresh(false);
        self.aidefringe.installed.as_ref().map(|i| key(p, &i.0.sha256))
    }

    /// Photo `id`'s correction, from memory or from disk.
    fn aidefringe_get(&mut self, id: PhotoId, key: u64) -> Option<Arc<Correction>> {
        let st = &mut self.aidefringe;
        if let Some(at) = st.ready.iter().position(|(p, c)| *p == id && c.key == key) {
            let e = st.ready.remove(at);
            let c = e.1.clone();
            st.ready.push(e);
            return Some(c);
        }
        let path = self.aidefringe_dir()?.join(format!("{key:016x}.lcadf"));
        let c = Arc::new(Correction::read(&path, key)?);
        self.aidefringe_keep(id, c.clone());
        Some(c)
    }

    fn aidefringe_keep(&mut self, id: PhotoId, c: Arc<Correction>) {
        let st = &mut self.aidefringe;
        st.ready.retain(|(p, _)| *p != id);
        st.ready.push((id, c));
        if st.ready.len() > IN_MEMORY {
            st.ready.remove(0);
        }
        st.generation += 1;
    }

    /// The key of photo `p`'s correction when a render of it uses one already in memory, else 0.
    pub(crate) fn aidefringe_salt(&self, p: &Photo) -> u64 {
        if p.develop.ai_defringe_amount() <= 0.0 {
            return 0;
        }
        let Some((m, _)) = &self.aidefringe.installed else { return 0 };
        let k = key(p, &m.sha256);
        if self.aidefringe.ready.iter().any(|(id, c)| *id == p.id && c.key == k) { k } else { 0 }
    }

    /// What a render of photo `p` with settings `s` does about AI Defringe (`None`: nothing, as it is off, has no
    /// model or no correction yet). With `make`, a missing correction is made by the render (an export).
    pub(crate) fn aidefringe_for_render(&mut self, p: &Photo, s: &DevelopSettings, make: bool) -> Result<Option<ForRender>, String> {
        let amount = s.ai_defringe_amount();
        if amount <= 0.0 {
            return Ok(None);
        }
        let Some(key) = self.aidefringe_key(p) else {
            return if make {
                Err(format!("{}: AI Defringe is on for this photo but no defringe model is installed", p.file_name))
            } else {
                Ok(None)
            };
        };
        if let Some(c) = self.aidefringe_get(p.id, key) {
            return Ok(Some(ForRender::Ready(c, amount)));
        }
        if !make {
            return Ok(None);
        }
        let r = self.aidefringe_recipe(p.id).map_err(|e| format!("{}: {e}", p.file_name))?;
        Ok(Some(ForRender::Make(Box::new(r), amount)))
    }

    /// Where photo `id`'s correction stands.
    pub fn aidefringe_state(&mut self, id: PhotoId) -> PhotoState {
        let Some(p) = self.catalog.photo(id).cloned() else { return PhotoState::Off };
        if p.develop.ai_defringe_amount() <= 0.0 && !p.develop.optics.ai_defringe_enabled() {
            return PhotoState::Off;
        }
        let Some(key) = self.aidefringe_key(&p).filter(|_| runtime_available()) else { return PhotoState::NoModel };
        if let Some(c) = self.aidefringe_get(id, key) {
            return PhotoState::Ready { changed: c.changed() };
        }
        if let Some(j) = self.aidefringe.job.as_ref().filter(|j| j.photo == id && j.key == key) {
            return PhotoState::Running { done: j.done.load(Ordering::Relaxed), total: j.total.load(Ordering::Relaxed) };
        }
        match self.aidefringe.failed.get(&key) {
            Some(e) => PhotoState::Failed { error: e.clone() },
            None => PhotoState::Waiting,
        }
    }

    /// Forget why photo `id`'s correction failed or that it was cancelled, so it is tried again.
    pub fn aidefringe_retry(&mut self, id: PhotoId) {
        if let Some(p) = self.catalog.photo(id).cloned()
            && let Some(key) = self.aidefringe_key(&p)
        {
            self.aidefringe.failed.remove(&key);
            self.aidefringe.cancelled.remove(&key);
        }
    }

    /// Called often (every frame by the app): take in a finished correction, stop one no longer wanted, and start the
    /// active photo's when it is on and missing. True when a correction arrived.
    pub fn aidefringe_pump(&mut self) -> bool {
        let mut arrived = false;
        if let Some(job) = &self.aidefringe.job {
            let still = self.catalog.photo(job.photo).cloned().is_some_and(|p| {
                p.develop.ai_defringe_amount() > 0.0 && self.aidefringe.installed.as_ref().is_some_and(|i| key(&p, &i.0.sha256) == job.key)
            });
            if !still {
                job.unwanted.store(true, Ordering::Relaxed);
                job.cancel.store(true, Ordering::Relaxed);
            }
            match job.rx.try_recv() {
                Err(TryRecvError::Empty) => {
                    if let Some(j) = &self.aidefringe.job {
                        j._task.progress(j.done.load(Ordering::Relaxed) as u64, j.total.load(Ordering::Relaxed).max(1) as u64);
                    }
                    return false;
                }
                r => {
                    let Some(job) = self.aidefringe.job.take() else { return false };
                    let stale = job.epoch != self.aidefringe_epoch();
                    match r {
                        _ if stale => {}
                        Ok(Ok(c)) => {
                            self.aidefringe_keep(job.photo, Arc::new(c));
                            arrived = true;
                        }
                        Ok(Err(e)) if !job.cancel.load(Ordering::Relaxed) => {
                            self.aidefringe.failed.insert(job.key, e);
                        }
                        // cancelled by the user (not by the pump because it was no longer wanted): not started again
                        // by itself. One the pump stopped that is wanted again (switched off and on quickly) starts
                        // again below.
                        Ok(Err(_)) if still && !job.unwanted.load(Ordering::Relaxed) => {
                            self.aidefringe.cancelled.insert(job.key);
                        }
                        Ok(Err(_)) => {}
                        Err(_) => {
                            self.aidefringe.failed.insert(job.key, "the defringe job stopped".into());
                        }
                    }
                }
            }
        }
        if !runtime_available() {
            return arrived;
        }
        let Some(id) = self.active() else { return arrived };
        let Some(p) = self.catalog.photo(id).cloned() else { return arrived };
        if p.develop.ai_defringe_amount() <= 0.0 {
            return arrived;
        }
        let Some(key) = self.aidefringe_key(&p) else { return arrived };
        if self.aidefringe.failed.contains_key(&key) || self.aidefringe.cancelled.contains(&key) || self.aidefringe_get(id, key).is_some() {
            return arrived;
        }
        match self.aidefringe_recipe(id) {
            Ok(r) => self.aidefringe_start(r, &p.file_name),
            Err(e) => {
                self.aidefringe.failed.insert(key, e);
            }
        }
        arrived
    }

    fn aidefringe_start(&mut self, r: Recipe, name: &str) {
        let (done, total, cancel) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)));
        let task = self.activity.start("aidefringe", "AI Defringe", crate::activity::Cancel::Flag(cancel.clone()));
        task.detail(name);
        let (tx, rx) = channel();
        let (d, t, c, key, photo, epoch) = (done.clone(), total.clone(), cancel.clone(), r.key, r.photo, r.publish.at);
        std::thread::Builder::new()
            .name("ai-defringe".into())
            .spawn(move || {
                let progress = |a: usize, b: usize| {
                    d.store(a, Ordering::Relaxed);
                    t.store(b, Ordering::Relaxed);
                };
                let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.make(&progress, &c)))
                    .unwrap_or_else(|_| Err("AI Defringe stopped unexpectedly".into()));
                let _ = tx.send(made);
            })
            .ok();
        self.aidefringe.job = Some(Job { photo, key, rx, done, total, cancel, unwanted: AtomicBool::new(false), epoch, _task: task });
    }

    /// Make photo `id`'s correction now, on this thread (for scripts and tests) → how many pixels it changes.
    pub fn aidefringe_make_now(&mut self, id: PhotoId) -> Result<usize, String> {
        let r = self.aidefringe_recipe(id)?;
        let c = Arc::new(r.make(&|_, _| {}, &AtomicBool::new(false))?);
        let n = c.changed();
        self.aidefringe.failed.remove(&r.key);
        self.aidefringe.cancelled.remove(&r.key);
        self.aidefringe_keep(id, c);
        Ok(n)
    }

    /// Delete every cached correction of this library → (files, bytes). Work started before is not published: the
    /// epoch is bumped under the lock writers publish under, so none can write after the files are deleted.
    pub fn aidefringe_clear(&mut self) -> (usize, u64) {
        let epoch = self.aidefringe.epoch.clone();
        let mut epoch = epoch.lock().unwrap_or_else(PoisonError::into_inner);
        *epoch += 1;
        if let Some(j) = &self.aidefringe.job {
            j.cancel.store(true, Ordering::Relaxed);
        }
        self.aidefringe.ready.clear();
        self.aidefringe.failed.clear();
        self.aidefringe.cancelled.clear();
        self.aidefringe.generation += 1;
        let Some(dir) = self.aidefringe_dir() else { return (0, 0) };
        let (mut files, mut bytes) = (0, 0);
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = e.path();
            if path.extension().is_some_and(|x| x == "lcadf") {
                let len = e.metadata().map_or(0, |m| m.len());
                if std::fs::remove_file(&path).is_ok() {
                    files += 1;
                    bytes += len;
                }
            }
        }
        (files, bytes)
    }

    /// The installed model changed: forget the loaded one and what failed with it.
    pub(crate) fn aidefringe_model_changed(&mut self) {
        if let Some(j) = &self.aidefringe.job {
            j.cancel.store(true, Ordering::Relaxed);
        }
        self.aidefringe.model = None;
        self.aidefringe.failed.clear();
        self.aidefringe.cancelled.clear();
        self.aidefringe.generation += 1;
        self.aidefringe_refresh(true);
    }
}

#[cfg(test)]
mod tests;
