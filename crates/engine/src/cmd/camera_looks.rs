//! Camera-matching looks: `profile.fitCameraLook` fits the camera's own JPEG rendering from a
//! person's raw + JPEG pairs ([`lcc_camprof::jpeg_look`]) and keeps it as a profile in the
//! *Camera Matching* group of the profile browser (an imported `.cube` look, Amount 0–200 %).
//!
//! The raw side is the app's render of the raw with its current settings (import defaults give the
//! look that a fresh import should get); the JPEG side is the camera's JPEG as decoded. Only the
//! fitted table is kept: nothing of the photos is written anywhere.

use std::collections::HashMap;
use std::path::Path;

use lcc_catalog::{MediaKind, PhotoId, Source};
use serde_json::{Value, json};

use super::{CommandSpec, bad, cmd, has_selection, str_param};
use crate::{Result, Session};

const C: &str = "profile.fitCameraLook";

/// The group fitted looks go to.
pub const GROUP: &str = "Camera Matching";

/// Long side of the renders the samples come from: enough for a smooth fit, cheap to render.
const SAMPLE_SIDE: usize = 640;

/// Most pairs one fit reads.
const MAX_PAIRS: usize = 200;

/// Pair raws with the JPEGs of the same name in the same folder among `ids`.
fn auto_pairs(s: &Session, ids: &[PhotoId]) -> Vec<(PhotoId, PhotoId)> {
    let key = |id: PhotoId| -> Option<(String, MediaKind)> {
        let p = s.catalog.photo(id)?;
        let Source::File { path } = &p.source else { return None };
        let path = Path::new(path);
        let stem = path.with_extension("").to_string_lossy().to_lowercase();
        let jpeg = path.extension().is_some_and(|x| ["jpg", "jpeg"].iter().any(|e| x.eq_ignore_ascii_case(e)));
        match p.kind {
            MediaKind::Raw => Some((stem, MediaKind::Raw)),
            MediaKind::Image if jpeg => Some((stem, MediaKind::Image)),
            _ => None,
        }
    };
    let mut jpegs: HashMap<String, PhotoId> = HashMap::new();
    for &id in ids {
        if let Some((k, MediaKind::Image)) = key(id) {
            jpegs.entry(k).or_insert(id);
        }
    }
    ids.iter()
        .filter_map(|&id| match key(id) {
            Some((k, MediaKind::Raw)) => jpegs.get(&k).map(|&j| (id, j)),
            _ => None,
        })
        .collect()
}

/// The `pairs` param (`[[raw, jpeg], …]`), else pairs found among the targets.
fn pairs(s: &Session, p: &Value) -> Result<Vec<(PhotoId, PhotoId)>> {
    let Some(v) = p.get("pairs").filter(|v| !v.is_null()) else {
        let ids = s.targets(p).map_err(|e| bad(C, e.to_string()))?;
        return Ok(auto_pairs(s, &ids));
    };
    let a = v.as_array().ok_or_else(|| bad(C, "`pairs` must be a list of [raw, jpeg] photo ids"))?;
    a.iter()
        .enumerate()
        .map(|(i, x)| {
            let two = x.as_array().filter(|t| t.len() == 2).and_then(|t| Some((t.first()?.as_u64()?, t.get(1)?.as_u64()?)));
            two.map(|(r, j)| (PhotoId(r), PhotoId(j))).ok_or_else(|| bad(C, format!("`pairs[{i}]` is {x}, not [raw id, jpeg id]")))
        })
        .collect()
}

fn fit_look(s: &mut Session, p: &Value) -> Result<Value> {
    let pairs = pairs(s, p)?;
    if pairs.is_empty() {
        return Err(bad(C, "no raw + JPEG pairs: select raws together with the camera's JPEGs of the same names, or give `pairs`"));
    }
    if pairs.len() > MAX_PAIRS {
        return Err(bad(C, format!("{} pairs; at most {MAX_PAIRS} per look", pairs.len())));
    }
    for &(r, j) in &pairs {
        for id in [r, j] {
            if s.catalog.photo(id).is_none() {
                return Err(bad(C, format!("no photo {}", id.0)));
            }
        }
        if r == j {
            return Err(bad(C, format!("photo {} is paired with itself", r.0)));
        }
    }
    let camera = pairs.first().and_then(|f| s.catalog.photo(f.0)).map(|ph| ph.meta.camera.clone()).unwrap_or_default();
    let name = match str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => n.chars().take(120).collect::<String>(),
        None if !camera.is_empty() => format!("{camera} camera look"),
        None => "Camera look".to_string(),
    };
    let n = pairs.len();
    let task = s.activity.start("cameraLook", "Fitting camera look", crate::activity::Cancel::Yes);
    let mut samples = Vec::new();
    let mut failed = Vec::new();
    for (i, &(raw, jpeg)) in pairs.iter().enumerate() {
        task.progress(i as u64, n as u64);
        if task.is_cancelled() {
            return Err(bad(C, "cancelled"));
        }
        let a = s.render_now(raw, SAMPLE_SIDE, SAMPLE_SIDE);
        let b = s.render_now(jpeg, SAMPLE_SIDE, SAMPLE_SIDE);
        match (a, b) {
            (Ok(a), Ok(b)) => {
                let (aw, ah, bw, bh) = (a.image.width, a.image.height, b.image.width, b.image.height);
                // the same frame: a JPEG of another shape (cropped in camera, rotated) is not this raw's
                let (ra, rb) = (aw as f64 / ah.max(1) as f64, bw as f64 / bh.max(1) as f64);
                if (ra / rb.max(1e-6) - 1.0).abs() > 0.03 {
                    failed.push(json!([[raw.0, jpeg.0], "the JPEG's shape differs from the raw's"]));
                    continue;
                }
                match lcc_camprof::jpeg_look::samples_from_images(&a.image.data, aw, ah, &b.image.data, bw, bh) {
                    Some(v) => samples.extend(v),
                    None => failed.push(json!([[raw.0, jpeg.0], "empty render"])),
                }
            }
            (Err(e), _) | (_, Err(e)) => failed.push(json!([[raw.0, jpeg.0], e])),
        }
    }
    task.progress(n as u64, n as u64);
    let look = lcc_camprof::jpeg_look::fit(&samples).map_err(|e| bad(C, e.to_string()))?;
    drop(samples);
    let id = super::lut_profiles::add_generated(s, C, &name, GROUP, &look.to_cube(&name))?;
    Ok(json!({
        "id": id,
        "name": name,
        "group": GROUP,
        "pairs": n - failed.len(),
        "failed": failed,
        "samples": look.samples,
        "errorBefore": look.error_before,
        "errorAfter": look.error_after,
    }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "profile.fitCameraLook",
        "Create Camera Matching Look from Raw + JPEG",
        ["Photo", "Develop Settings"],
        None,
        "{pairs?: [[raw id, jpeg id]…] (default: the selected raws paired with the selected JPEGs of the same name and folder), name?}: fits the camera's own JPEG rendering (tone curve + colour table) to the app's render of each raw and keeps it as a profile in the profile browser's Camera Matching group → {id, name, group, pairs, failed, samples, errorBefore, errorAfter} (mean RGB difference, 0–255)",
        has_selection,
        fit_look
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A made-up camera rendering: more contrast, warmer, greens pulled towards cyan.
    fn camera(c: [u8; 3]) -> [u8; 3] {
        let f = |v: u8| f32::from(v) / 255.0;
        let s = |v: f32| v * v * (3.0 - 2.0 * v) * 0.6 + v * 0.4;
        let (r, g, b) = (s(f(c[0])), s(f(c[1])), s(f(c[2])));
        let o = [(r * 1.06).min(1.0), g * 0.92 + b * 0.08, b * 0.9];
        o.map(|v| (v * 255.0).round().clamp(0.0, 254.0) as u8)
    }

    fn write_jpeg(path: &Path, w: usize, h: usize, px: impl Fn(usize, usize) -> [u8; 3]) {
        let mut data = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                data.extend(px(x, y));
            }
        }
        let img = lcc_codecs::EncodeImage::new(w as u32, h as u32, 3, lcc_codecs::Samples::U8(&data));
        let b = lcc_codecs::encode_jpeg(&img, 98, lcc_codecs::ChromaSubsampling::S444, &Default::default()).unwrap();
        std::fs::write(path, b).unwrap();
    }

    fn mean_diff(a: &lcc_raster::Rgba8, b: &lcc_raster::Rgba8) -> f64 {
        assert_eq!((a.width, a.height), (b.width, b.height));
        let t: u64 = a.data.iter().zip(&b.data).map(|(p, q)| (0..3).map(|c| u64::from(p[c].abs_diff(q[c]))).sum::<u64>()).sum();
        t as f64 / (a.data.len() * 3) as f64
    }

    /// The look fitted from a synthetic pair brings the "raw" side's render to the "camera JPEG".
    #[test]
    fn a_fitted_look_makes_the_render_match_the_camera_jpeg() {
        let dir = std::env::temp_dir().join(format!("lc-camlook-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (w, h) = (192, 128);
        let src = |x: usize, y: usize| {
            let r = (x * 250 / w) as u8;
            let g = (y * 250 / h) as u8;
            let b = (((x + y) * 7) % 250) as u8;
            [r, g, b]
        };
        let (a, b) = (dir.join("scene.jpg"), dir.join("scene-camera.jpg"));
        write_jpeg(&a, w, h, src);
        write_jpeg(&b, w, h, |x, y| camera(src(x, y)));
        let mut s = Session::new().with_fs();
        let r = s.execute("library.import", &json!({"paths": [a.to_string_lossy(), b.to_string_lossy()]})).unwrap();
        let ids: Vec<u64> = r["imported"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
        let (ra, rb) = (PhotoId(ids[0]), PhotoId(ids[1]));
        let target = s.render_now(rb, 192, 192).unwrap().image;
        let plain = s.render_now(ra, 192, 192).unwrap().image;
        let out = s.execute("profile.fitCameraLook", &json!({"pairs": [[ids[0], ids[1]]], "name": "Test cam"})).unwrap();
        assert_eq!(out["group"], GROUP);
        let id = out["id"].as_str().unwrap().to_string();
        assert!(s.lut_profiles.iter().any(|p| p.id == id && p.group == GROUP), "listed with the imported profiles");
        s.execute("library.select", &json!({"ids": [ids[0]], "active": ids[0]})).unwrap();
        s.execute("develop.profile", &json!({"id": id})).unwrap();
        let looked = s.render_now(ra, 192, 192).unwrap().image;
        let (before, after) = (mean_diff(&plain, &target), mean_diff(&looked, &target));
        assert!(after < before * 0.35, "before {before:.2} after {after:.2}");
        lcc_pipeline::lut::unregister(&id);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_pairs_are_actionable_errors() {
        let mut s = Session::with_demo();
        let id = s.active().unwrap().0;
        for p in [
            json!({"pairs": []}),
            json!({"pairs": "x"}),
            json!({"pairs": [[1]]}),
            json!({"pairs": [[-1, 2.5]]}),
            json!({"pairs": [[id, id]]}),
            json!({"pairs": [[id, 999_999]]}),
            json!({"ids": [id]}),
        ] {
            assert!(s.execute(C, &p).is_err(), "{p}");
        }
    }
}
