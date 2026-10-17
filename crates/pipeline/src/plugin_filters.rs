//! Pixel-filter plug-ins (PhotoCraft ABI, [`lcc_filters`]) in the develop pipeline.
//!
//! A photo's stack ([`DevelopSettings::plugin_filters`]) runs at two points:
//!
//! - **Source** filters run on the resampled source (after geometry, before white balance, local
//!   tone and every other step) in linear light with sRGB primaries (`"encoding":"linear"`), so
//!   e.g. a negative conversion sees the scan and every develop slider acts on its result. The
//!   result is cached with the stage cache, keyed by the stack, so other sliders don't re-run it.
//! - **Display** filters run on the finished, output-encoded `f32` image (sRGB curve, output
//!   primaries, `"encoding":"srgb"`) before quantisation; on an HDR render, on its linear samples
//!   (`"encoding":"linear"`).
//!
//! The pipeline doesn't own the installed filters: it asks a [`FilterHost`] (from
//! [`crate::SourceInfo::filters`], else the process default set with [`set_default_host`]). A
//! filter that isn't installed or fails is skipped and named in [`crate::Rendered::warnings`];
//! a render never fails or panics because of one.
//!
//! Positions handed to a filter (`_image.x/y`, `canvasWidth/Height`) are in the whole output
//! frame's pixels, so a zoomed window matches the full render at the same size.

use std::borrow::Cow;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, PoisonError, RwLock};

use lcc_develop::{DevelopSettings, FilterStage, PluginFilter};
use lcc_raster::Rgb32f;
use serde_json::Value;

/// Resolves a stack entry to an installed filter.
pub trait FilterHost: Send + Sync {
    /// The installed filter with manifest id `plugin`, or why there is none. `module_hash` is the
    /// build the entry was made with; a host may return another build of the same plug-in (the
    /// pipeline then warns).
    fn resolve(&self, plugin: &str, module_hash: &str) -> Result<Arc<lcc_filters::Filter>, String>;
}

/// A shared [`FilterHost`], compared by identity (so [`crate::SourceInfo`] stays `PartialEq`).
#[derive(Clone)]
pub struct FilterHostRef(pub Arc<dyn FilterHost>);

impl std::fmt::Debug for FilterHostRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FilterHostRef")
    }
}

impl PartialEq for FilterHostRef {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

static DEFAULT_HOST: RwLock<Option<Arc<dyn FilterHost>>> = RwLock::new(None);

/// Sets the host renders use when their [`crate::SourceInfo::filters`] is `None` (the app's
/// installed plug-ins). `None` clears it.
pub fn set_default_host(host: Option<Arc<dyn FilterHost>>) {
    *DEFAULT_HOST.write().unwrap_or_else(PoisonError::into_inner) = host;
}

fn host_for(info: &crate::SourceInfo) -> Option<Arc<dyn FilterHost>> {
    info.filters.as_ref().map(|h| h.0.clone()).or_else(|| DEFAULT_HOST.read().unwrap_or_else(PoisonError::into_inner).clone())
}

/// Any enabled plug-in filter at either stage (the GPU renderer leaves these to the CPU).
pub fn any_active(s: &DevelopSettings) -> bool {
    s.active_plugin_filters(FilterStage::Source).next().is_some() || s.active_plugin_filters(FilterStage::Display).next().is_some()
}

/// One stage's stack, resolved against the host.
pub struct Stack {
    entries: Vec<(PluginFilter, Result<Arc<lcc_filters::Filter>, String>)>,
    /// Identifies what the stack computes (ids, the builds that run, params, analysis).
    pub key: u64,
}

impl Stack {
    /// The enabled filters of `stage` in `s`, resolved through `info`'s host.
    pub fn resolve(info: &crate::SourceInfo, s: &DevelopSettings, stage: FilterStage) -> Stack {
        let host = host_for(info);
        let entries: Vec<_> = s
            .active_plugin_filters(stage)
            .map(|f| {
                let r = match &host {
                    Some(h) => h.resolve(&f.plugin, &f.module_hash),
                    None => Err(format!("plug-in {:?} is not installed", f.plugin)),
                };
                (f.clone(), r)
            })
            .collect();
        let key = crate::hash_of(
            entries
                .iter()
                .map(|(f, r)| {
                    let build = r.as_ref().map(|x| x.module_hash().to_string()).unwrap_or_default();
                    let analysis = f.analysis.as_ref().map(Value::to_string).unwrap_or_default();
                    (f.plugin.clone(), build, Value::Object(f.params.clone()).to_string(), analysis)
                })
                .collect::<Vec<_>>(),
        );
        Stack { entries, key }
    }

    /// No filters.
    pub fn empty() -> Stack {
        Stack { entries: Vec::new(), key: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether a filter of the stack reads neighbouring pixels (manifest `overlap` > 0): its look
    /// depends on the pixel scale, so the pipeline runs it at the frame's native resolution
    /// ([`crate::source_stage`]).
    pub fn pixel_scale(&self) -> bool {
        self.entries.iter().any(|(_, r)| r.as_ref().is_ok_and(|f| f.manifest().overlap > 0))
    }

    /// Runs the stack on `img` (interleaved RGB as `encoding` says), in order; each filter that is
    /// missing or fails leaves the image as it was and adds a warning.
    pub fn apply(&self, img: &mut [[f32; 3]], w: usize, h: usize, frame: lcc_filters::FrameInfo, encoding: &str, warnings: &mut Vec<String>) {
        let cancel = AtomicBool::new(false);
        for (f, r) in &self.entries {
            let filter = match r {
                Ok(x) => x,
                Err(e) => {
                    warnings.push(format!("{}skipped: {e}", warning_prefix(f)));
                    continue;
                }
            };
            if !f.module_hash.is_empty() && f.module_hash != filter.module_hash() {
                warnings.push(format!("{}the installed build differs from the one this edit was made with", warning_prefix(f)));
            }
            let (Ok(w32), Ok(h32)) = (u32::try_from(w), u32::try_from(h)) else {
                warnings.push(format!("{}skipped: image too large", warning_prefix(f)));
                continue;
            };
            let params = filter_params(f, encoding);
            let r = crate::timed(&format!("plug-in {}", f.plugin), || filter.apply_rgb(img.as_flattened_mut(), w32, h32, frame, &params, &cancel));
            if let Err(e) = r {
                warnings.push(format!("{}skipped: {e}", warning_prefix(f)));
            }
        }
    }
}

/// How a warning about stack entry `f` starts (`plug-in filter #3 "Negative" `): it names the entry
/// for people and, by its uid, for [`warning_message`].
pub fn warning_prefix(f: &PluginFilter) -> String {
    format!("plug-in filter #{} {:?} ", f.uid, f.name)
}

/// The message of a render warning about entry `f` (without [`warning_prefix`]), or `None` when
/// `warning` is about something else.
pub fn warning_message<'a>(warning: &'a str, f: &PluginFilter) -> Option<&'a str> {
    warning.strip_prefix(warning_prefix(f).as_str())
}

/// An entry's parameters as the module gets them: its values, `encoding`, and `_analysis`.
pub fn filter_params(f: &PluginFilter, encoding: &str) -> Value {
    let mut m = f.params.clone();
    m.insert("encoding".into(), Value::from(encoding));
    if let Some(a) = &f.analysis {
        m.insert("_analysis".into(), a.clone());
    }
    Value::Object(m)
}

/// Linear Rec.2020 → linear sRGB primaries, and back.
fn matrices() -> ([[f32; 3]; 3], [[f32; 3]; 3]) {
    (lcc_color::REC2020.to_space(&lcc_color::SRGB).to_f32(), lcc_color::SRGB.to_space(&lcc_color::REC2020).to_f32())
}

fn mul(m: &[[f32; 3]; 3], c: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| m[i][0] * c[0] + m[i][1] * c[1] + m[i][2] * c[2])
}

/// Most pixels a draft runs Source filters on when it can't reuse a cached result: a drag of a
/// filter's own parameter stays interactive, the full render after it is exact.
pub const DRAFT_SOURCE_PIXELS: usize = 500_000;

/// The Source stack on `img` (linear Rec.2020, in place): converted to linear sRGB primaries,
/// filtered with `"encoding":"linear"`, converted back. `draft`: run on a reduced copy (at most
/// [`DRAFT_SOURCE_PIXELS`]) and scale the result back up.
pub fn apply_source(stack: &Stack, img: &mut Rgb32f, frame: lcc_filters::FrameInfo, draft: bool, warnings: &mut Vec<String>) {
    if stack.is_empty() {
        return;
    }
    let (to_srgb, to_2020) = matrices();
    let n = img.width.saturating_mul(img.height);
    if draft && n > DRAFT_SOURCE_PIXELS {
        let k = (DRAFT_SOURCE_PIXELS as f64 / n as f64).sqrt();
        let (sw, sh) = (((img.width as f64 * k) as usize).max(1), ((img.height as f64 * k) as usize).max(1));
        let mut small = lcc_raster::resample::resize(img, sw, sh, lcc_raster::resample::Filter::Box);
        small.data.iter_mut().for_each(|c| *c = mul(&to_srgb, *c));
        let sframe = scale_frame(frame, k);
        stack.apply(&mut small.data, sw, sh, sframe, "linear", warnings);
        small.data.iter_mut().for_each(|c| *c = mul(&to_2020, *c));
        *img = lcc_raster::resample::resize(&small, img.width, img.height, lcc_raster::resample::Filter::Bilinear);
        return;
    }
    crate::for_rows(&mut img.data, img.width.max(1), |_, row| row.iter_mut().for_each(|c| *c = mul(&to_srgb, *c)));
    let (w, h) = (img.width, img.height);
    stack.apply(&mut img.data, w, h, frame, "linear", warnings);
    crate::for_rows(&mut img.data, img.width.max(1), |_, row| row.iter_mut().for_each(|c| *c = mul(&to_2020, *c)));
}

fn scale_frame(f: lcc_filters::FrameInfo, k: f64) -> lcc_filters::FrameInfo {
    let s = |v: u32| ((v as f64 * k).round() as u32).max(1);
    lcc_filters::FrameInfo { x: (f.x as f64 * k).round() as u32, y: (f.y as f64 * k).round() as u32, full_w: s(f.full_w), full_h: s(f.full_h) }
}

/// The Display stack on finished samples (`linear`: an HDR render's linear values; otherwise
/// sRGB-curve encoded).
pub fn apply_display(
    stack: &Stack,
    img: &mut [[f32; 3]],
    w: usize,
    h: usize,
    frame: lcc_filters::FrameInfo,
    linear: bool,
    warnings: &mut Vec<String>,
) {
    if !stack.is_empty() {
        stack.apply(img, w, h, frame, if linear { "linear" } else { "srgb" }, warnings);
    }
}

/// `src` as the Source filters of `s` turn it (whole frame, downsized to at most `max_edge`), for
/// whole-image helpers (auto tone and the like) that would otherwise read the unfiltered source.
/// Borrowed unchanged when no Source filter is active.
pub fn filtered_source<'a>(src: &'a Rgb32f, info: &crate::SourceInfo, s: &DevelopSettings, max_edge: usize) -> Cow<'a, Rgb32f> {
    let stack = Stack::resolve(info, s, FilterStage::Source);
    if stack.is_empty() {
        return Cow::Borrowed(src);
    }
    let mut img = lcc_raster::resample::fit(src, max_edge.max(1), max_edge.max(1), lcc_raster::resample::Filter::Box);
    let frame = lcc_filters::FrameInfo::whole(img.width as u32, img.height as u32);
    let mut warnings = Vec::new();
    apply_source(&stack, &mut img, frame, false, &mut warnings);
    Cow::Owned(img)
}

/// A downsized whole frame (long edge at most `max_edge`) in linear sRGB primaries, interleaved,
/// for a filter's `pc_analyze`: `(samples, width, height)`.
pub fn analysis_input(src: &Rgb32f, max_edge: usize) -> (Vec<f32>, usize, usize) {
    let img = lcc_raster::resample::fit(src, max_edge.max(1), max_edge.max(1), lcc_raster::resample::Filter::Box);
    let (to_srgb, _) = matrices();
    let data = img.data.iter().flat_map(|c| mul(&to_srgb, *c)).collect();
    (data, img.width, img.height)
}
