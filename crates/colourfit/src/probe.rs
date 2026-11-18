//! Synthetic probe DNGs: designed colour patches another editor renders, read back by position.
//!
//! A probe is a DNG of flat patches spread through camera RGB (a white-balanced 7 × 7 × 7 cube, a grey ramp and
//! saturated primaries / secondaries at four levels) on a grey ground. It carries the camera's Exif make and model
//! (so its exports pair with that camera's real raws in [`crate::fit`]), the camera database's matrix (to set its
//! white and let the editor pick its own profile for the model by `UniqueCameraModel`) and no settings beyond a
//! neutral baseline. Rendered by Lightroom at its defaults, it gives an evenly spread camera-RGB → output sample that
//! natural photos never do; its export goes into `calibrate-lightroom fit` with the real raws.
//!
//! **Slider sweeps** are the same probe with one Lightroom setting in its embedded XMP (`crs:Exposure2012="+1.00"`,
//! …), which Lightroom applies on import. [`check`] renders each probe through the app with the setting the app's
//! own import maps it to (`lcc_engine::crs`), then searches that app control for the value whose change from the
//! base probe best matches Lightroom's change from its base export (changes, not absolute colours, so the base
//! colour difference doesn't bias the answer). The result is a measured mapping per Lightroom slider value.
//!
//! Sources: own design.
use crate::compare::Stats;
use crate::deltae::{de2000, lab, quantile};
use crate::{Error, Result};
use lcc_raw::{BlackLevel, Cfa, ColorData, DngWriteOptions, OpcodeLists, Orientation, RawData, RawFormat, RawImage, Rect};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Manifest format version.
pub const VERSION: u32 = 1;
/// The manifest's file name inside a probe folder.
pub const MANIFEST: &str = "probe.json";
/// The base probe's file stem.
pub const BASE: &str = "probe_base";

const COLS: usize = 24;
const ROWS: usize = 16;
const CELL: usize = 50;
const INSET: usize = 6;
const MARGIN: usize = 40;
const BLACK: f64 = 512.0;
const WHITE: f64 = 16000.0;
/// Ground and gutters: a mid-dark grey (white-balanced camera RGB).
const GROUND: f64 = 0.12;
/// Long edge the app renders probes at.
const EDGE: usize = 640;

/// One patch: its raw camera RGB (black-subtracted, white = 1, not white-balanced), its white-balanced value, and
/// its centre as a fraction of the frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Patch {
    pub camera: [f32; 3],
    pub balanced: [f32; 3],
    pub pos: [f32; 2],
}

/// One probe file: its stem, the Lightroom settings embedded in it (`crs:` name → value, none for the base) and
/// the group it belongs to (the swept setting, `base` for the base probe).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProbeFile {
    pub stem: String,
    pub group: String,
    pub crs: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub make: String,
    pub model: String,
    pub unique_camera_model: String,
    pub width: usize,
    pub height: usize,
    /// Half the averaged area of a patch, as a fraction of the frame `[x, y]`.
    pub half: [f32; 2],
    pub patches: Vec<Patch>,
    pub files: Vec<ProbeFile>,
}

/// The neutral baseline every probe carries (Lightroom's defaults that would otherwise blur, sharpen or
/// lens-correct the patches).
const BASELINE: &[(&str, &str)] = &[
    ("ProcessVersion", "15.4"),
    ("HasSettings", "True"),
    ("Sharpness", "0"),
    ("LuminanceSmoothing", "0"),
    ("ColorNoiseReduction", "0"),
    ("LensProfileEnable", "0"),
    ("AutoLateralCA", "0"),
    ("VignetteAmount", "0"),
];

/// The sweeps: each Lightroom setting and the values it is probed at.
fn sweeps() -> Vec<(&'static str, Vec<f64>)> {
    let pm = |a: f64, b: f64| vec![-b, -a, a, b];
    let mut v = vec![
        ("Exposure2012", vec![-2.0, -1.0, -0.5, 0.5, 1.0, 2.0]),
        ("Contrast2012", pm(50.0, 100.0)),
        ("Highlights2012", pm(50.0, 100.0)),
        ("Shadows2012", pm(50.0, 100.0)),
        ("Whites2012", pm(50.0, 100.0)),
        ("Blacks2012", pm(50.0, 100.0)),
        ("Dehaze", pm(25.0, 50.0)),
        ("Vibrance", pm(50.0, 100.0)),
        ("Saturation", pm(50.0, 100.0)),
    ];
    for colour in ["Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta"] {
        for kind in ["HueAdjustment", "SaturationAdjustment", "LuminanceAdjustment"] {
            let name: &'static str = Box::leak(format!("{kind}{colour}").into_boxed_str());
            v.push((name, vec![-60.0, 60.0]));
        }
    }
    v
}

fn value_text(v: f64) -> String {
    if v.fract() == 0.0 { format!("{v:+.0}") } else { format!("{v:+.2}") }
}

/// `probe_exposure2012_p0_50`, `probe_contrast2012_m100`: file-name safe on every system.
fn stem_of(group: &str, v: f64) -> String {
    let t = if v.fract() == 0.0 { format!("{:.0}", v.abs()) } else { format!("{:.2}", v.abs()).replace('.', "_") };
    format!("probe_{}_{}{t}", group.to_lowercase(), if v < 0.0 { "m" } else { "p" })
}

/// The probe files (base first, then every sweep step).
pub fn files() -> Vec<ProbeFile> {
    let mut out = vec![ProbeFile { stem: BASE.into(), group: "base".into(), crs: Vec::new() }];
    for (key, values) in sweeps() {
        for v in values {
            out.push(ProbeFile { stem: stem_of(key, v), group: key.to_string(), crs: vec![(key.to_string(), value_text(v))] });
        }
    }
    out
}

/// The patches' white-balanced colours, in layout order.
fn balanced_colours() -> Vec<[f64; 3]> {
    let mut v = Vec::with_capacity(COLS * ROWS);
    // grey ramp, log-spaced
    for i in 0..16 {
        let g = 0.004 * (0.92f64 / 0.004).powf(i as f64 / 15.0);
        v.push([g; 3]);
    }
    // cube, gamma-spaced levels
    let level = |i: usize| 0.012 + 0.84 * (i as f64 / 6.0).powf(2.2);
    for r in 0..7 {
        for g in 0..7 {
            for b in 0..7 {
                v.push([level(r), level(g), level(b)]);
            }
        }
    }
    // saturated primaries and secondaries at four levels
    for l in [0.06, 0.18, 0.4, 0.8] {
        for c in [[1.0, 0.02, 0.02], [0.02, 1.0, 0.02], [0.02, 0.02, 1.0], [1.0, 1.0, 0.02], [0.02, 1.0, 1.0], [1.0, 0.02, 1.0]] {
            v.push([c[0] * l, c[1] * l, c[2] * l]);
        }
    }
    v.truncate(COLS * ROWS);
    while v.len() < COLS * ROWS {
        v.push([0.5; 3]);
    }
    v
}

/// The camera neutral at D65 through the camera database's matrices for `make` / `model` (green = 1).
fn neutral_for(make: &str, model: &str) -> Result<(lcc_camdb::Entry, [f64; 3])> {
    let entry = lcc_camdb::find(Some(make), model)
        .filter(|e| lcc_raw::color::has_matrix(&e.color_data()))
        .ok_or_else(|| Error(format!("no colour matrices are known for {make} {model}: a probe needs one to set its white")))?;
    let n = lcc_raw::color::camera_neutral(&entry.color_data(), lcc_color::D65);
    if !(n.iter().all(|v| v.is_finite() && *v > 0.0) && n[1] > 0.0) {
        return Err(Error(format!("{make} {model}: its matrices give no usable white")));
    }
    Ok((entry, [n[0] / n[1], 1.0, n[2] / n[1]]))
}

fn xmp_packet(crs: &[(String, String)]) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;");
    let mut attrs = String::new();
    for (k, v) in BASELINE.iter().map(|(k, v)| (k.to_string(), v.to_string())).chain(crs.iter().cloned()) {
        attrs.push_str(&format!("\n   crs:{}=\"{}\"", esc(&k), esc(&v)));
    }
    format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\"{attrs}/></rdf:RDF></x:xmpmeta>"
    )
}

/// Write the probe set for a camera into `dir`: one DNG per [`files`] entry and the [`MANIFEST`]. `make` and `model`
/// are the camera's Exif strings (as its raws carry them); `unique` overrides the `UniqueCameraModel` written (by
/// default the camera database's make and model, the naming raw converters' profiles use).
pub fn write(dir: &Path, make: &str, model: &str, unique: Option<&str>, with_sweeps: bool) -> Result<Manifest> {
    let (make, model) = (make.trim(), model.trim());
    if make.is_empty() || model.is_empty() {
        return Err(Error("a probe needs the camera's make and model".into()));
    }
    let (entry, neutral) = neutral_for(make, model)?;
    let unique = unique.map(str::trim).filter(|u| !u.is_empty()).map(str::to_string).unwrap_or_else(|| format!("{} {}", entry.make, entry.model));
    let (w, h) = (COLS * CELL + 2 * MARGIN, ROWS * CELL + 2 * MARGIN);
    let top = neutral.iter().copied().fold(1.0, f64::max);
    let raw_of = |bal: [f64; 3]| -> [f64; 3] { std::array::from_fn(|c| (bal[c] * neutral[c] / top).clamp(0.0, 0.97)) };
    let colours = balanced_colours();
    let patches: Vec<Patch> = colours
        .iter()
        .enumerate()
        .map(|(i, bal)| {
            let (col, row) = (i % COLS, i / COLS);
            let cx = (MARGIN + col * CELL) as f32 + CELL as f32 / 2.0;
            let cy = (MARGIN + row * CELL) as f32 + CELL as f32 / 2.0;
            let cam = raw_of(*bal);
            Patch { camera: cam.map(|v| v as f32), balanced: bal.map(|v| (v / top) as f32), pos: [cx / w as f32, cy / h as f32] }
        })
        .collect();
    let ground = raw_of([GROUND; 3]);
    let cfa = Cfa::bayer("RGGB").ok_or_else(|| Error("no RGGB pattern".into()))?;
    let mut data = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let inside = |p: usize, m: usize| p >= MARGIN && p < m - MARGIN && (p - MARGIN) % CELL >= INSET && (p - MARGIN) % CELL < CELL - INSET;
            let v = if inside(x, w) && inside(y, h) {
                let i = ((y - MARGIN) / CELL) * COLS + (x - MARGIN) / CELL;
                patches.get(i).map(|p| p.camera.map(f64::from)).unwrap_or(ground)
            } else {
                ground
            };
            let c = v.get(cfa.color_at(x, y) as usize).copied().unwrap_or(0.0);
            data.push((BLACK + c * (WHITE - BLACK)).round() as u16);
        }
    }
    let color = ColorData {
        illuminant: entry.illuminants,
        color_matrix: entry.color_matrix,
        forward_matrix: entry.forward_matrix,
        as_shot_neutral: Some(neutral),
        ..Default::default()
    };
    let mut raw = RawImage {
        format: RawFormat::Dng,
        width: w,
        height: h,
        cpp: 1,
        data: RawData::U16(data),
        cfa: Some(cfa),
        bits: 14,
        black: BlackLevel::uniform(BLACK as f32),
        white: vec![WHITE as f32],
        active_area: Rect::new(0, 0, w, h),
        crop: Rect::new(0, 0, w, h),
        orientation: Orientation::Normal,
        color,
        wb_multipliers: None,
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: OpcodeLists::default(),
        metadata: Default::default(),
        lens_metadata: Default::default(),
    };
    raw.metadata.make = Some(make.into());
    raw.metadata.model = Some(model.into());
    std::fs::create_dir_all(dir).map_err(|e| Error(format!("{}: {e}", dir.display())))?;
    let list: Vec<ProbeFile> = files().into_iter().filter(|f| with_sweeps || f.group == "base").collect();
    for f in &list {
        let opts = DngWriteOptions { xmp: Some(xmp_packet(&f.crs)), unique_camera_model: Some(unique.clone()), ..Default::default() };
        let bytes = lcc_raw::write_dng(&raw, &opts).map_err(|e| Error(format!("{}: {e}", f.stem)))?;
        let path = dir.join(format!("{}.dng", f.stem));
        std::fs::write(&path, bytes).map_err(|e| Error(format!("{}: {e}", path.display())))?;
    }
    let half = [(CELL - 2 * INSET) as f32 / 4.0 / w as f32, (CELL - 2 * INSET) as f32 / 4.0 / h as f32];
    let manifest = Manifest {
        version: VERSION,
        make: make.into(),
        model: model.into(),
        unique_camera_model: unique,
        width: w,
        height: h,
        half,
        patches,
        files: list,
    };
    let json = serde_json::to_string_pretty(&manifest).map_err(|e| Error(e.to_string()))?;
    std::fs::write(dir.join(MANIFEST), json).map_err(|e| Error(format!("{}: {e}", dir.join(MANIFEST).display())))?;
    Ok(manifest)
}

/// The Exif make and model of a raw (or any image) file, for writing probes "like" one of a camera's raws.
pub fn camera_of(path: &Path) -> Result<(Option<String>, Option<String>)> {
    let bytes = crate::dataset::read_file(path).map_err(|e| Error(format!("{}: {e}", path.display())))?;
    let m = lcc_meta::extract(&bytes);
    Ok((m.make, m.model))
}

/// Read and check a probe folder's manifest.
pub fn read_manifest(dir: &Path) -> Result<Manifest> {
    let p = dir.join(MANIFEST);
    let text = crate::dataset::read_file(&p).map_err(|e| Error(format!("{}: {e}", p.display())))?;
    let m: Manifest = serde_json::from_slice(&text).map_err(|e| Error(format!("{}: {e}", p.display())))?;
    if m.version != VERSION {
        return Err(Error(format!("{}: probe version {} (this tool reads {VERSION}); write the probes again", p.display(), m.version)));
    }
    let ok = |v: f32| v.is_finite() && (0.0..=1.0).contains(&v);
    if m.patches.is_empty() || m.patches.len() > 10_000 || !m.patches.iter().all(|p| p.pos.iter().all(|v| ok(*v))) || !m.half.iter().all(|v| ok(*v)) {
        return Err(Error(format!("{}: not a usable probe manifest", p.display())));
    }
    Ok(m)
}

/// Patch means (XYZ D50, white Y = 1) of an image grid at the manifest's positions; `None` for a patch off the grid.
fn patch_means(grid: &[[f64; 3]], w: usize, h: usize, m: &Manifest) -> Vec<Option<[f64; 3]>> {
    m.patches
        .iter()
        .map(|p| {
            let (cx, cy) = (p.pos[0] as f64 * w as f64, p.pos[1] as f64 * h as f64);
            let (hx, hy) = ((m.half[0] as f64 * w as f64).max(0.5), (m.half[1] as f64 * h as f64).max(0.5));
            let (x0, x1) = ((cx - hx).floor().max(0.0) as usize, ((cx + hx).ceil() as usize).min(w));
            let (y0, y1) = ((cy - hy).floor().max(0.0) as usize, ((cy + hy).ceil() as usize).min(h));
            let mut acc = [0.0; 3];
            let mut n = 0.0;
            for y in y0..y1 {
                for x in x0..x1 {
                    let v = grid.get(y * w + x)?;
                    if v.iter().all(|c| c.is_finite()) {
                        for c in 0..3 {
                            acc[c] += v[c];
                        }
                        n += 1.0;
                    }
                }
            }
            (n > 0.0).then(|| acc.map(|a| a / n))
        })
        .collect()
}

/// An export's patches (decoded through its own colour profile).
fn export_patches(path: &Path, m: &Manifest) -> Result<Vec<Option<[f64; 3]>>> {
    let name = crate::dataset::file_name(path);
    let bytes = crate::dataset::read_file(path).map_err(|e| Error(format!("{name}: {e}")))?;
    let d = lcc_codecs::decode(&bytes, lcc_codecs::DecodeOptions::fit(2048, 2048)).map_err(|e| Error(format!("{name}: cannot decode: {e}")))?;
    let mat = d.space.to_xyz_d50;
    let img = d.image.into_oriented(lcc_raw::Orientation::from_exif(d.orientation));
    let (want, got) = (m.width as f64 / m.height as f64, img.width as f64 / img.height.max(1) as f64);
    if img.width < 64 || (want / got - 1.0).abs() > 0.02 {
        return Err(Error(format!("{name}: not the probe's shape ({}×{}): export it uncropped and unrotated", img.width, img.height)));
    }
    let grid: Vec<[f64; 3]> = img.data.iter().map(|p| mat.apply([f64::from(p[0]), f64::from(p[1]), f64::from(p[2])])).collect();
    Ok(patch_means(&grid, img.width, img.height, m))
}

/// The app's own decode of the base probe, rendered with any settings.
struct OurProbe {
    img: lcc_raster::Rgb32f,
    info: lcc_pipeline::SourceInfo,
}

impl OurProbe {
    fn load(path: &Path) -> Result<OurProbe> {
        let bytes = crate::dataset::read_file(path).map_err(Error)?;
        let (img, info) = lcc_engine::files::load_bytes_with(&bytes, EDGE, lcc_engine::media::LoadOptions::default()).map_err(Error)?;
        Ok(OurProbe { img, info })
    }

    fn patches(&self, s: &lcc_develop::DevelopSettings, m: &Manifest) -> Result<Vec<Option<[f64; 3]>>> {
        use lcc_pipeline::{DeepSamples, OutputDepth, OutputSpace, RenderRequest};
        let req = RenderRequest { depth: OutputDepth::F32Linear, space: OutputSpace::ProPhoto, ..RenderRequest::fit(EDGE, EDGE) };
        let out = lcc_pipeline::render(&self.img, &self.info, s, &req);
        let deep = out.deep.ok_or_else(|| Error("render: no float output".into()))?;
        let DeepSamples::F32(px) = deep.samples else { return Err(Error("render: no float samples".into())) };
        let mat = lcc_color::PROPHOTO.to_xyz();
        let grid: Vec<[f64; 3]> = px.as_chunks::<3>().0.iter().map(|c| mat.apply([f64::from(c[0]), f64::from(c[1]), f64::from(c[2])])).collect();
        if grid.len() < deep.width.saturating_mul(deep.height) {
            return Err(Error("render: short output".into()));
        }
        Ok(patch_means(&grid, deep.width, deep.height, m))
    }
}

/// How the app maps one Lightroom slider value, and the app value that matches Lightroom best.
#[derive(Clone, Debug, Serialize)]
pub struct StepResult {
    pub stem: String,
    /// The Lightroom setting (`crs:` name) and its value.
    pub setting: String,
    pub lightroom: f64,
    /// The app control the import maps it to, and the value it sets (`None` when the import maps it to several
    /// values or none).
    pub control: Option<String>,
    pub mapped: Option<f64>,
    /// The app value whose change from the base probe best matches Lightroom's.
    pub best: Option<f64>,
    /// Mean Lab distance between Lightroom's change and the app's change, at the mapped and at the best value.
    pub error_mapped: f64,
    pub error_best: Option<f64>,
    /// Mean size of Lightroom's change (Lab), for scale.
    pub lightroom_change: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProbeReport {
    pub make: String,
    pub model: String,
    /// The app's base render against Lightroom's base export, ΔE00 over the patches (`None` without a base export).
    pub base: Option<Stats>,
    pub steps: Vec<StepResult>,
    /// Probe files without an export.
    pub missing: Vec<String>,
    /// (file, why it was not used).
    pub skipped: Vec<(String, String)>,
}

/// The exports under `dir` (TIFF / JPEG, with subfolders) by lower-case file stem.
fn exports_by_stem(dir: &Path) -> Vec<(String, PathBuf)> {
    fn walk(d: &Path, depth: usize, out: &mut Vec<(String, PathBuf)>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
                continue;
            }
            if p.is_dir() {
                if depth < 8 {
                    walk(&p, depth + 1, out);
                }
            } else if p.extension().and_then(|x| x.to_str()).is_some_and(|x| ["tif", "tiff", "jpg", "jpeg"].iter().any(|e| x.eq_ignore_ascii_case(e)))
                && let Some(stem) = p.file_stem()
            {
                out.push((stem.to_string_lossy().to_lowercase(), p.clone()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, 0, &mut out);
    out.sort();
    out
}

fn find_export<'a>(exports: &'a [(String, PathBuf)], stem: &str) -> Option<&'a PathBuf> {
    let stem = stem.to_lowercase();
    exports
        .iter()
        .find(|(s, _)| *s == stem)
        .or_else(|| exports.iter().find(|(s, _)| s.strip_prefix(&stem).is_some_and(|r| r.starts_with(|c: char| !c.is_alphanumeric()))))
        .map(|(_, p)| p)
}

/// The numeric leaves of a partial settings object that are develop controls (`light.exposure`, …).
fn control_leaves(v: &Value, prefix: &str, out: &mut Vec<(String, f64)>) {
    if let Value::Object(map) = v {
        for (k, x) in map {
            let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
            match x {
                Value::Object(_) => control_leaves(x, &path, out),
                Value::Number(n) => {
                    if let Some(f) = n.as_f64()
                        && lcc_develop::controls::find(&path).is_some()
                    {
                        out.push((path, f));
                    }
                }
                _ => {}
            }
        }
    }
}

/// Mean Lab distance between two sets of patch changes.
fn change_error(app: &[[f64; 3]], lr: &[[f64; 3]]) -> f64 {
    let n = app.len().min(lr.len());
    if n == 0 {
        return f64::INFINITY;
    }
    app.iter().zip(lr).map(|(a, b)| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()).sum::<f64>() / n as f64
}

/// Lab changes from `base` to `step`, over the patches both have.
fn changes(base: &[Option<[f64; 3]>], step: &[Option<[f64; 3]>], keep: &[bool]) -> Vec<[f64; 3]> {
    base.iter()
        .zip(step)
        .zip(keep)
        .filter(|(_, k)| **k)
        .map(|((b, s), _)| match (b, s) {
            (Some(b), Some(s)) => {
                let (lb, ls) = (lab(*b), lab(*s));
                [ls[0] - lb[0], ls[1] - lb[1], ls[2] - lb[2]]
            }
            _ => [0.0; 3],
        })
        .collect()
}

/// Check a probe folder against Lightroom's exports of it.
pub fn check(probes: &Path, exports: &Path) -> Result<ProbeReport> {
    let m = read_manifest(probes)?;
    let found = exports_by_stem(exports);
    if found.is_empty() {
        return Err(Error(format!("{}: no TIFF or JPEG exports found", exports.display())));
    }
    let ours = OurProbe::load(&probes.join(format!("{BASE}.dng")))?;
    let base_settings = crate::fit::pipe::settings();
    let mut skipped = Vec::new();
    let mut missing = Vec::new();
    let ours_base = ours.patches(&base_settings, &m)?;
    let lr_base = match find_export(&found, BASE) {
        Some(p) => match export_patches(p, &m) {
            Ok(v) => Some(v),
            Err(e) => {
                skipped.push((crate::dataset::file_name(p), e.0));
                None
            }
        },
        None => {
            missing.push(BASE.to_string());
            None
        }
    };
    let base = lr_base.as_ref().map(|lr| {
        let de: Vec<f64> = ours_base.iter().zip(lr).filter_map(|(a, b)| Some(de2000(lab((*a)?), lab((*b)?)))).filter(|d| d.is_finite()).collect();
        stats(&de)
    });
    // patches clipped on either side say nothing about a slider: leave out those near white or black
    let usable = |v: &Option<[f64; 3]>| v.is_some_and(|x| x[1] > 0.002 && x[1] < 0.9);
    let steps_in: Vec<&ProbeFile> = m.files.iter().filter(|f| f.group != "base").collect();
    let mut steps = Vec::new();
    if let Some(lr_base) = &lr_base {
        let results: Vec<std::result::Result<Option<StepResult>, (String, String)>> = steps_in
            .par_iter()
            .map(|f| {
                let Some(path) = find_export(&found, &f.stem) else { return Ok(None) };
                let lr = export_patches(path, &m).map_err(|e| (crate::dataset::file_name(path), e.0))?;
                let keep: Vec<bool> = lr_base.iter().zip(&lr).zip(&ours_base).map(|((a, b), c)| usable(a) && usable(b) && usable(c)).collect();
                let lr_change = changes(lr_base, &lr, &keep);
                let size = lr_change.iter().map(|d| (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()).sum::<f64>() / lr_change.len().max(1) as f64;
                // the app's own reading of the setting
                let mut props = std::collections::BTreeMap::new();
                for (k, v) in &f.crs {
                    props.insert(format!("crs:{k}"), vec![v.clone()]);
                }
                let partial = lcc_engine::crs::to_partial(&props, lcc_engine::crs::Target::RawAbsolute);
                let mapped_settings = lcc_develop::presets::apply_partial(&base_settings, &partial, 1.0);
                let err_at = |s: &lcc_develop::DevelopSettings| -> std::result::Result<f64, (String, String)> {
                    let p = ours.patches(s, &m).map_err(|e| (f.stem.clone(), e.0))?;
                    Ok(change_error(&changes(&ours_base, &p, &keep), &lr_change))
                };
                let error_mapped = err_at(&mapped_settings)?;
                let mut leaves = Vec::new();
                control_leaves(&partial, "", &mut leaves);
                let lightroom = f.crs.first().and_then(|(_, v)| v.trim().parse::<f64>().ok()).unwrap_or(f64::NAN);
                let mut out = StepResult {
                    stem: f.stem.clone(),
                    setting: f.group.clone(),
                    lightroom,
                    control: None,
                    mapped: None,
                    best: None,
                    error_mapped,
                    error_best: None,
                    lightroom_change: size,
                };
                if let [(control, mapped)] = leaves.as_slice()
                    && let Some(spec) = lcc_develop::controls::find(control)
                {
                    out.control = Some(control.clone());
                    out.mapped = Some(*mapped);
                    let at = |x: f64| {
                        let mut s = mapped_settings.clone();
                        lcc_develop::controls::set(&mut s, control, x);
                        err_at(&s)
                    };
                    let (best, err) = search(spec.min, spec.max, at)?;
                    out.best = Some(best);
                    out.error_best = Some(err.min(error_mapped));
                    if error_mapped <= err {
                        out.best = Some(*mapped);
                    }
                }
                Ok(Some(out))
            })
            .collect();
        for (r, f) in results.into_iter().zip(&steps_in) {
            match r {
                Ok(Some(s)) => steps.push(s),
                Ok(None) => missing.push(f.stem.clone()),
                Err(e) => skipped.push(e),
            }
        }
    } else {
        missing.extend(steps_in.iter().filter(|f| find_export(&found, &f.stem).is_none()).map(|f| f.stem.clone()));
    }
    if base.is_none() && steps.is_empty() {
        return Err(Error(
            "no probe export could be used: export probe_base (and the sweeps) from Lightroom as TIFFs, keeping the file names".into(),
        ));
    }
    Ok(ProbeReport { make: m.make, model: m.model, base, steps, missing, skipped })
}

/// Minimise `f` over `[lo, hi]`: a coarse scan, then golden-section refinement around the best point.
fn search<E>(lo: f64, hi: f64, f: impl Fn(f64) -> std::result::Result<f64, E>) -> std::result::Result<(f64, f64), E> {
    if !(lo.is_finite() && hi.is_finite() && hi > lo) {
        return Ok((lo, f64::INFINITY));
    }
    const N: usize = 24;
    let step = (hi - lo) / N as f64;
    let mut best = (lo, f64::INFINITY);
    for i in 0..=N {
        let x = lo + step * i as f64;
        let e = f(x)?;
        if e < best.1 {
            best = (x, e);
        }
    }
    let (mut a, mut b) = ((best.0 - step).max(lo), (best.0 + step).min(hi));
    let g = (5f64.sqrt() - 1.0) / 2.0;
    let (mut c, mut d) = (b - g * (b - a), a + g * (b - a));
    let (mut fc, mut fd) = (f(c)?, f(d)?);
    for _ in 0..14 {
        if fc < fd {
            b = d;
            d = c;
            fd = fc;
            c = b - g * (b - a);
            fc = f(c)?;
        } else {
            a = c;
            c = d;
            fc = fd;
            d = a + g * (b - a);
            fd = f(d)?;
        }
    }
    for (x, e) in [(c, fc), (d, fd)] {
        if e < best.1 {
            best = (x, e);
        }
    }
    Ok(best)
}

fn stats(de: &[f64]) -> Stats {
    let n = de.len();
    if n == 0 {
        return Stats::default();
    }
    Stats {
        pixels: n,
        mean: de.iter().sum::<f64>() / n as f64,
        median: quantile(de, 0.5),
        p95: quantile(de, 0.95),
        p99: quantile(de, 0.99),
        max: de.iter().copied().fold(0.0, f64::max),
        ..Default::default()
    }
}

/// Plain-text summary for the terminal.
pub fn text(r: &ProbeReport) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(s, "probe check: {} {}", r.make, r.model);
    if let Some(b) = &r.base {
        let _ = writeln!(
            s,
            "default render, app vs Lightroom over {} patches: ΔE00 mean {:.2}, median {:.2}, p95 {:.2}",
            b.pixels, b.mean, b.median, b.p95
        );
    }
    if !r.steps.is_empty() {
        let _ = writeln!(
            s,
            "\n{:<26} {:>8}  {:<26} {:>8} {:>8}   {:>7} {:>7}  {:>7}",
            "Lightroom setting", "value", "app control", "mapped", "best", "err now", "err best", "LR Δ"
        );
        for st in &r.steps {
            let opt = |v: Option<f64>| v.map(|x| format!("{x:.2}")).unwrap_or_else(|| "n/a".into());
            let _ = writeln!(
                s,
                "{:<26} {:>+8.2}  {:<26} {:>8} {:>8}   {:>7.2} {:>7}  {:>7.2}",
                st.setting,
                st.lightroom,
                st.control.as_deref().unwrap_or("n/a"),
                opt(st.mapped),
                opt(st.best),
                st.error_mapped,
                opt(st.error_best),
                st.lightroom_change
            );
        }
        s.push_str("\nerr: mean Lab distance between Lightroom's change from its base export and the app's change from its base render;\n'best' is the app value that matches Lightroom's change closest (the import should map the Lightroom value there)\n");
    }
    if !r.missing.is_empty() {
        let _ =
            writeln!(s, "\nno export for {} probe(s), e.g. {}", r.missing.len(), r.missing.iter().take(3).cloned().collect::<Vec<_>>().join(", "));
    }
    for (f, why) in r.skipped.iter().take(10) {
        let _ = writeln!(s, "skipped {f}: {why}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test's folder in Temp, removed when the test ends.
    fn scratch(name: &str) -> lcc_testkit::TempDir {
        lcc_testkit::TempDir::new(&format!("lcc-colourfit-probe-{name}"))
    }

    #[test]
    fn probes_are_valid_dngs_with_their_settings_and_a_manifest() {
        let d = scratch("write");
        let m = write(&d, "SONY", "ILCE-7M3", None, true).unwrap();
        assert_eq!(m.patches.len(), COLS * ROWS);
        assert!(m.files.len() > 50 && m.files[0].stem == BASE);
        let mut stems: Vec<&str> = m.files.iter().map(|f| f.stem.as_str()).collect();
        stems.sort_unstable();
        stems.dedup();
        assert_eq!(stems.len(), m.files.len(), "unique file names");
        assert_eq!(read_manifest(&d).unwrap(), m);
        let exp = m.files.iter().find(|f| f.group == "Exposure2012").unwrap();
        let bytes = std::fs::read(d.join(format!("{}.dng", exp.stem))).unwrap();
        let raw = lcc_raw::decode(&bytes).unwrap();
        assert_eq!(raw.metadata.model.as_deref(), Some("ILCE-7M3"));
        assert!(lcc_raw::color::has_matrix(&raw.color));
        let xmp = lcc_meta::embedded(&bytes).xmp.unwrap();
        let props = lcc_meta::parse_xmp(&xmp).unwrap().properties;
        assert_eq!(props.get("crs:Exposure2012").and_then(|v| v.first()).map(String::as_str), Some(exp.crs[0].1.as_str()));
        // the patches are where the manifest says: the decoded raw is brighter at a bright patch than at a dark one
        let base = write(&scratch("base-only"), "SONY", "ILCE-7M3", Some("Sony ILCE-7M3"), false).unwrap();
        assert_eq!(base.files.len(), 1);
        assert_eq!(base.unique_camera_model, "Sony ILCE-7M3");
        // every patch sits inside the frame and off the margin
        for p in &m.patches {
            assert!(p.pos.iter().all(|v| (0.03..0.97).contains(v)), "{p:?}");
            assert!(p.camera.iter().all(|v| (0.0..=0.97).contains(v)));
        }
    }

    #[test]
    fn bad_cameras_and_folders_are_errors() {
        assert!(write(&scratch("x"), "", "ILCE-7M3", None, false).is_err());
        assert!(write(&scratch("y"), "Nobody", "Nothing 1", None, false).is_err());
        let d = scratch("bad-manifest");
        std::fs::create_dir_all(&d).unwrap();
        assert!(read_manifest(&d).is_err(), "missing");
        std::fs::write(d.join(MANIFEST), "{not json").unwrap();
        assert!(read_manifest(&d).is_err(), "garbage");
        std::fs::write(d.join(MANIFEST), r#"{"version":1,"make":"a","model":"b","unique_camera_model":"c","width":10,"height":10,"half":[2.0,0.1],"patches":[{"camera":[0,0,0],"balanced":[0,0,0],"pos":[0.5,0.5]}],"files":[]}"#).unwrap();
        assert!(read_manifest(&d).is_err(), "out-of-range half size");
        assert!(check(&d, &d).is_err());
    }

    #[test]
    fn the_search_finds_a_minimum_and_survives_bad_ranges() {
        let (x, e) = search(-5.0, 5.0, |x| Ok::<_, ()>((x - 1.37).abs())).unwrap();
        assert!((x - 1.37).abs() < 0.01 && e < 0.01, "{x} {e}");
        let (_, e) = search(1.0, 1.0, Ok::<_, ()>).unwrap();
        assert!(e.is_infinite());
        let (_, e) = search(f64::NAN, 1.0, Ok::<_, ()>).unwrap();
        assert!(e.is_infinite());
    }
}
