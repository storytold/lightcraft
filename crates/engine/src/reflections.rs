//! Local reflection correction: a small, source-bound linear Rec.2020 delta grid.
//! The original is never rewritten. Rendering, history, thumbnails and exports use the same delta.
use lightcraft_develop::Reflection;
use lightcraft_preview::Hash128;
use lightcraft_raster::Rgb32f;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

const MAGIC: &[u8; 8] = b"LCREFL01";
const MAX_EDGE: usize = 1024;
type Mix = (Reflection, Weak<Rgb32f>, Arc<Rgb32f>);
#[derive(Default)]
struct Cache {
    grids: HashMap<String, Arc<Rgb32f>>,
    mix: Option<Mix>,
}
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

pub fn identity(key: Option<Hash128>) -> String {
    key.map(|k| format!("{k:?}")).unwrap_or_default()
}

/// Zero strength is exactly the unchanged input, including HDR values above white.
pub fn apply(
    src: &Arc<Rgb32f>,
    r: &Reflection,
    key: Option<Hash128>,
    mattes: Option<&lightcraft_pipeline::masks::Mattes>,
) -> Result<Arc<Rgb32f>, String> {
    if !r.enabled || r.amount == 0.0 || r.correction.is_empty() {
        return Ok(src.clone());
    }
    if src.width == 0 || src.height == 0 || src.width.checked_mul(src.height) != Some(src.data.len()) {
        return Err("Invalid reflection source dimensions".into());
    }
    if !r.amount.is_finite() || !(0.0..=100.0).contains(&r.amount) {
        return Err("Invalid reflection strength".into());
    }
    if r.source_key.is_empty() || r.source_key != identity(key) {
        return Err("The photo changed. Recalculate or reset reflection removal.".into());
    }
    let mut cache = CACHE.get_or_init(Default::default).lock().map_err(|_| "Reflection cache unavailable")?;
    if ![r.left, r.top, r.right, r.bottom, r.detail_protection].iter().all(|v| v.is_finite() && (0.0..=100.0).contains(v))
        || (r.limit_region && (r.left >= r.right || r.top >= r.bottom))
    {
        return Err("Invalid reflection protection area".into());
    }
    if let Some((settings, input, output)) = &cache.mix
        && settings == r
        && input.upgrade().is_some_and(|x| Arc::ptr_eq(&x, src))
    {
        return Ok(output.clone());
    }
    let grid = match cache.grids.get(&r.correction) {
        Some(g) => g.clone(),
        None => {
            let g = Arc::new(read_grid(&r.correction)?);
            if cache.grids.len() >= 8 {
                cache.grids.clear();
            }
            cache.grids.insert(r.correction.clone(), g.clone());
            g
        }
    };
    let resized = lightcraft_raster::resample::resize(&grid, src.width, src.height, lightcraft_raster::resample::Filter::Bilinear);
    let sky = if r.protect_sky {
        let small = lightcraft_raster::resample::fit(src, 384, 384, lightcraft_raster::resample::Filter::Lanczos3);
        let frame = lightcraft_pipeline::frame_for(&small, &Default::default(), &Default::default(), false);
        let alpha = lightcraft_pipeline::masks::shape_alpha(
            &lightcraft_develop::MaskShape::Sky,
            &frame,
            small.width,
            small.height,
            &small,
            &small.map(lightcraft_pipeline::local::log_lum),
            0.0,
            mattes,
        );
        Some(lightcraft_raster::resample::resize(&alpha, src.width, src.height, lightcraft_raster::resample::Filter::Bilinear))
    } else {
        None
    };
    let mut image = (**src).clone();
    let amount = r.amount as f32 / 100.0;
    for (i, (pixel, delta)) in image.data.iter_mut().zip(&resized.data).enumerate() {
        let x = (i % src.width) as f64 + 0.5;
        let y = (i / src.width) as f64 + 0.5;
        if r.limit_region
            && (x * 100.0 / (src.width as f64) < r.left
                || x * 100.0 / (src.width as f64) > r.right
                || y * 100.0 / (src.height as f64) < r.top
                || y * 100.0 / (src.height as f64) > r.bottom)
        {
            continue;
        }
        let mut a = amount * (1.0 - sky.as_ref().and_then(|s| s.data.get(i)).copied().unwrap_or(0.0).clamp(0.0, 1.0));
        if r.detail_protection > 0.0 {
            let brightness = pixel.iter().copied().fold(0.0f32, f32::max);
            let largest = delta.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
            let cap = brightness * (1.0 - r.detail_protection as f32 / 100.0);
            if largest > cap && largest > 0.0 {
                a *= cap / largest;
            }
        }
        if a <= 0.0 {
            continue;
        }
        for (v, d) in pixel.iter_mut().zip(delta) {
            *v = (*v - a * d).max(0.0);
        }
    }
    let image = Arc::new(image);
    cache.mix = Some((r.clone(), Arc::downgrade(src), image.clone()));
    Ok(image)
}

fn read_grid(path: &str) -> Result<Rgb32f, String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| format!("Reflection correction missing: {e}. Recalculate or reset reflection removal."))?;
    let mut bytes = Vec::new();
    file.by_ref().take((16 + MAX_EDGE * MAX_EDGE * 12 + 1) as u64).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.get(..8) != Some(MAGIC.as_slice()) {
        return Err("Invalid reflection correction".into());
    }
    let u32_at = |n| -> Result<usize, String> {
        let b: [u8; 4] = bytes.get(n..n + 4).ok_or("Truncated correction")?.try_into().map_err(|_| "Truncated correction")?;
        Ok(u32::from_le_bytes(b) as usize)
    };
    let (w, h) = (u32_at(8)?, u32_at(12)?);
    if w == 0 || h == 0 || w > MAX_EDGE || h > MAX_EDGE || bytes.len() != 16 + w * h * 12 {
        return Err("Invalid reflection dimensions".into());
    }
    let mut data = Vec::with_capacity(w * h);
    for b in bytes.get(16..).ok_or("Truncated correction")?.as_chunks::<12>().0 {
        let mut p = [0.0; 3];
        for (v, b) in p.iter_mut().zip(b.as_chunks::<4>().0.iter()) {
            *v = f32::from_le_bytes(*b);
            if !v.is_finite() || v.abs() > 4.0 {
                return Err("Invalid reflection values".into());
            }
        }
        data.push(p);
    }
    Ok(Rgb32f { width: w, height: h, data })
}

/// Models live under the platform's application support directory, or an explicit override.
pub fn models_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("LIGHTCRAFT_REFLECTION_MODELS")
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| crate::config::config_dir().map(|p| p.join("reflection-models")))
}
pub fn valid_model(model: &str) -> bool {
    matches!(model, "dsrnet-s" | "dsrnet-l" | "dsrnet-l-nature")
}
#[cfg(not(target_arch = "wasm32"))]
pub fn available_model(model: &str) -> bool {
    valid_model(model) && models_dir().is_some_and(|p| lightcraft_reflections::installed(&p.join(model)))
}
#[cfg(target_arch = "wasm32")]
pub fn available_model(_model: &str) -> bool {
    false
}
pub fn available() -> bool {
    available_model("dsrnet-s")
}

#[cfg(not(target_arch = "wasm32"))]
pub fn make(source: crate::media::SourceRef, key: Option<Hash128>, edge: u32, model: &str) -> Result<Reflection, String> {
    use std::io::{BufWriter, Write};
    if !matches!(edge, 384 | 768 | 1024) || !valid_model(model) {
        return Err("Invalid reflection model or quality".into());
    }
    let dir = models_dir().ok_or("No reflection model directory")?.join(model);
    let net = lightcraft_reflections::Net::load(&dir, false)?;
    let src = source.load()?;
    let small = lightcraft_raster::resample::fit(&src, edge as usize, edge as usize, lightcraft_raster::resample::Filter::Lanczos3);
    let matrix = lightcraft_color::REC2020.to_space(&lightcraft_color::SRGB);
    let bytes: Vec<u8> = small.data.iter().flat_map(|p| matrix.apply_f32(*p).map(lightcraft_color::transfer::encode_srgb8)).collect();
    let clean = net.infer(&bytes, small.width, small.height)?;
    let root = crate::config::config_dir().ok_or("No local configuration directory")?.join("reflection-cache");
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
    let path = root.join(format!("{}-{nonce}.lcr", std::process::id()));
    let file = std::fs::OpenOptions::new().create_new(true).write(true).open(&path).map_err(|e| e.to_string())?;
    let result = (|| {
        let mut f = BufWriter::new(file);
        f.write_all(MAGIC)
            .and_then(|_| f.write_all(&(small.width as u32).to_le_bytes()))
            .and_then(|_| f.write_all(&(small.height as u32).to_le_bytes()))
            .map_err(|e| e.to_string())?;
        let to_working = lightcraft_color::SRGB.to_space(&lightcraft_color::REC2020);
        for (a, b) in bytes.as_chunks::<3>().0.iter().zip(clean) {
            let a = *a;
            let a = to_working.apply_f32(a.map(lightcraft_color::transfer::decode_srgb8));
            let b = to_working.apply_f32(b.map(lightcraft_color::transfer::srgb_to_linear));
            for (a, b) in a.iter().zip(b) {
                f.write_all(&(a - b).to_le_bytes()).map_err(|e| e.to_string())?;
            }
        }
        f.flush().map_err(|e| e.to_string())?;
        f.get_ref().sync_all().map_err(|e| e.to_string())?;
        Ok(Reflection {
            enabled: true,
            correction: path.to_string_lossy().into(),
            source_key: identity(key),
            amount: 35.0,
            quality: edge,
            model: model.into(),
            protect_sky: true,
            detail_protection: 60.0,
            ..Default::default()
        })
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&path);
    }
    result
}

#[cfg(not(target_arch = "wasm32"))]
pub fn install_check(path: &std::path::Path) -> Result<(String, Vec<u8>), String> {
    let (manifest, bytes) = lightcraft_reflections::verified_weights(path)?;
    Ok((manifest.architecture, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn grid(path: &std::path::Path, value: f32) {
        let mut b = MAGIC.to_vec();
        b.extend(1u32.to_le_bytes());
        b.extend(1u32.to_le_bytes());
        for _ in 0..3 {
            b.extend(value.to_le_bytes());
        }
        std::fs::write(path, b).unwrap();
    }
    #[test]
    fn reflection_is_source_bound_zero_is_exact_and_strength_is_linear() {
        let path = std::env::temp_dir().join(format!("lc-reflection-grid-{}", std::process::id()));
        grid(&path, 0.2);
        let key = Some(Hash128(123));
        let mut r = Reflection {
            enabled: true,
            correction: path.to_string_lossy().into(),
            source_key: identity(key),
            amount: 50.0,
            quality: 384,
            ..Default::default()
        };
        let src = Arc::new(Rgb32f::filled(2, 2, [0.5, 0.5, 2.0]));
        let out = apply(&src, &r, key, None).unwrap();
        assert!((out.data[0][0] - 0.4).abs() < 1e-5);
        assert!((out.data[0][2] - 1.9).abs() < 1e-5);
        assert!(Arc::ptr_eq(&out, &apply(&src, &r, key, None).unwrap()));
        assert!(apply(&src, &r, Some(Hash128(456)), None).is_err());
        r.amount = 0.0;
        assert!(Arc::ptr_eq(&src, &apply(&src, &r, key, None).unwrap()));
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn reflection_survives_settings_roundtrip_and_affects_view_and_export() {
        let mut session = crate::Session::with_demo();
        let id = session.active().unwrap();
        let plain = session.render_job(id, 128, 128, false, true).unwrap().run().rendered.unwrap().image;
        let job = session.render_job(id, 128, 128, false, true).unwrap();
        let path = std::env::temp_dir().join(format!("lc-reflection-render-{}", std::process::id()));
        grid(&path, 0.15);
        let mut d = (*session.develop_of(id).unwrap()).clone();
        d.reflection = Reflection {
            enabled: true,
            correction: path.to_string_lossy().into(),
            source_key: identity(job.source_key),
            amount: 100.0,
            quality: 384,
            ..Default::default()
        };
        let json = serde_json::to_vec(&d).unwrap();
        let restored = serde_json::from_slice(&json).unwrap();
        session.set_develop(id, restored, "Reflections").unwrap();
        let view = session.render_job(id, 128, 128, false, true).unwrap().run().rendered.unwrap().image;
        assert_ne!(plain.data, view.data);
        let export = session
            .export_job(id, view.width, view.height, lightcraft_pipeline::OutputSpace::Srgb, lightcraft_pipeline::OutputDepth::U8)
            .unwrap()
            .run()
            .rendered
            .unwrap()
            .image;
        assert_eq!(view, export);
        session.execute("develop.set", &serde_json::json!({"control":"reflection.amount","value":0})).unwrap();
        let zero = session.render_job(id, 128, 128, false, true).unwrap().run().rendered.unwrap().image;
        assert_eq!(plain, zero);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn region_and_detail_protection_preserve_clouds_and_cache_keys() {
        let path = std::env::temp_dir().join(format!("lc-reflection-protection-{}", std::process::id()));
        grid(&path, 1.0);
        let key = Some(Hash128(888));
        let src = Arc::new(Rgb32f::filled(4, 4, [0.8, 0.9, 1.0]));
        let mut r = Reflection {
            enabled: true,
            correction: path.to_string_lossy().into(),
            source_key: identity(key),
            amount: 100.0,
            quality: 384,
            ..Default::default()
        };
        let unprotected = apply(&src, &r, key, None).unwrap();
        assert_eq!(unprotected.data[0], [0.0, 0.0, 0.0]);
        r.detail_protection = 80.0;
        let gentle = apply(&src, &r, key, None).unwrap();
        assert!(gentle.data[0][0] > 0.59, "clouds must not disappear");
        r.limit_region = true;
        r.top = 50.0;
        let limited = apply(&src, &r, key, None).unwrap();
        assert_eq!(&limited.data[..8], &src.data[..8]);
        assert_ne!(&limited.data[8..], &src.data[8..]);
        r.detail_protection = 100.0;
        assert_eq!(*apply(&src, &r, key, None).unwrap(), *src);
        let small = Arc::new(lightcraft_raster::resample::resize(&src, 2, 2, lightcraft_raster::resample::Filter::Bilinear));
        r.detail_protection = 80.0;
        let scaled = apply(&small, &r, key, None).unwrap();
        assert_eq!(&scaled.data[..2], &small.data[..2]);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn default_settings_stay_compatible_and_area_controls_do_not_cross() {
        let mut d = lightcraft_develop::DevelopSettings::default();
        let json = serde_json::to_value(&d).unwrap();
        assert!(json.get("reflection").is_none());
        assert!(lightcraft_develop::controls::set(&mut d, "reflection.left", 100.0));
        assert!(d.reflection.left < d.reflection.right);
        assert!(lightcraft_develop::controls::set(&mut d, "reflection.bottom", 0.0));
        assert!(d.reflection.top < d.reflection.bottom);
    }
    #[test]
    fn corrupt_reflection_file_is_an_error_not_a_panic() {
        let path = std::env::temp_dir().join(format!("lc-reflection-corrupt-{}", std::process::id()));
        std::fs::write(&path, b"LCREFL01").unwrap();
        assert!(read_grid(&path.to_string_lossy()).is_err());
        grid(&path, f32::NAN);
        assert!(read_grid(&path.to_string_lossy()).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
