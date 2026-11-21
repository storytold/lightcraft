//! Nikon source corrections across the decoder, optics controls, export and existing libraries.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use craft_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value, tags};
use lightcraft_catalog::Op;
use lightcraft_develop::DevelopSettings;
use lightcraft_geom::Orientation;
use lightcraft_pipeline::{RenderRequest, SourceInfo};
use lightcraft_raster::{Rgb32f, Rgba8};
use serde_json::json;

use crate::{Session, files};

/// Original procedural sensor data and NikonNEFInfo records, with no external media fixture.
fn nef(model: &str, orientation: Orientation) -> Vec<u8> {
    fn rational(value: f64, denominator: i32) -> Vec<u8> {
        let numerator = (value * f64::from(denominator)).round() as i32;
        [numerator.to_le_bytes(), denominator.to_le_bytes()].concat()
    }
    let mut distortion = b"0100\x03\x01\0\0".to_vec();
    distortion.extend(rational(21.6, 4096));
    distortion.extend(4u32.to_le_bytes());
    for value in [0.039895, 0.019211, -0.127218, 0.0, 0.0] {
        distortion.extend(rational(value, 1 << 20));
    }
    distortion.extend([0u8; 4]);
    distortion.extend(rational(-0.025043, 1 << 20));
    distortion.resize(84, 0);
    let mut center = b"0100".to_vec();
    center.extend(rational(1.5, 256));
    center.extend(rational(-0.75, 256));
    center.resize(24, 0);
    let mut info = IfdBuilder::new();
    info.set(5, Value::Undefined(distortion));
    info.set(8, Value::Undefined(center));
    let mut blob = b"Nikon\0\x01\x03\0\0".to_vec();
    blob.extend(TiffWriter::default().write(&[info]).unwrap());
    let (w, h) = (192u32, 128u32);
    let mut samples = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let rgb = [2200 + x * 25, 3000 + y * 30, 2600 + (x + y) * 10];
            let channel = [[0, 1], [1, 2]][y as usize % 2][x as usize % 2];
            samples.extend((rgb[channel] as u16).to_le_bytes());
        }
    }
    let mut raw = IfdBuilder::new();
    raw.set(tags::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
    raw.set(tags::IMAGE_WIDTH, Value::Long(vec![w]));
    raw.set(tags::IMAGE_LENGTH, Value::Long(vec![h]));
    raw.set(tags::BITS_PER_SAMPLE, Value::Short(vec![14]));
    raw.set(tags::COMPRESSION, Value::Short(vec![1]));
    raw.set(tags::PHOTOMETRIC, Value::Short(vec![tags::photometric::CFA]));
    raw.set(tags::CFA_REPEAT_PATTERN_DIM, Value::Short(vec![2, 2]));
    raw.set(tags::CFA_PATTERN_EP, Value::Byte(vec![0, 1, 1, 2]));
    raw.set(0xc7d5, Value::Undefined(blob));
    raw.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![samples] });
    let mut root = IfdBuilder::new();
    root.set(tags::MAKE, Value::Ascii("NIKON CORPORATION".into()));
    root.set(tags::MODEL, Value::Ascii(model.into()));
    root.set(tags::ORIENTATION, Value::Short(vec![orientation.to_exif()]));
    root.add_sub_ifd(raw);
    TiffWriter::new(ByteOrder::Little, false).write(&[root]).unwrap()
}

fn settings(info: &SourceInfo) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.wb.temp = info.as_shot_temp;
    s.wb.tint = info.as_shot_tint;
    s.optics.lens_profile = true;
    s
}

fn render(source: &Rgb32f, info: &SourceInfo, settings: &DevelopSettings, edge: usize) -> Rgba8 {
    lightcraft_pipeline::render(source, info, settings, &RenderRequest::fit(edge, edge)).image
}

fn difference(a: &Rgba8, b: &Rgba8) -> (f64, u8) {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let values: Vec<_> = a.data.iter().zip(&b.data).flat_map(|(a, b)| (0..3).map(move |c| a[c].abs_diff(b[c]))).collect();
    (values.iter().map(|&v| f64::from(v)).sum::<f64>() / values.len() as f64, values.into_iter().max().unwrap())
}

/// New body recognition must reach both public decode paths and the existing toggle/strength controls.
#[test]
fn nikon_z6_z7_probe_decode_toggle_amount_and_dng_agree() {
    for model in ["NIKON Z 6", "NIKON Z 7"] {
        let bytes = nef(model, Orientation::Normal);
        let header = lightcraft_raw::probe_info(&bytes).unwrap();
        let raw = lightcraft_raw::decode(&bytes).unwrap();
        assert_eq!(header, raw.info());
        assert!(matches!(&raw.data,lightcraft_raw::RawData::U16(samples) if samples.iter().all(|&v|v<16384)), "the fixture is valid 14-bit data");
        assert_eq!(raw.opcodes.list3.len(), 1);
        let lens = files::embedded_lens(&header).unwrap();
        assert!(lens.warp.is_some() && lens.vignette.is_none());
        let dng = lightcraft_raw::write_dng(&raw, &Default::default()).unwrap();
        let back = lightcraft_raw::decode(&dng).unwrap();
        assert_eq!(back.opcodes.list3, raw.opcodes.list3);
        assert_eq!(back.data, raw.data);
        assert_eq!(files::embedded_lens(&back.info()), Some(lens));
        let (source, info) = files::load_bytes(&bytes, 192).unwrap();
        assert_eq!(info.lens, Some(lens));
        let mut s = settings(&info);
        let on = render(&source, &info, &s, 192);
        s.optics.lens_profile = false;
        let off = render(&source, &info, &s, 192);
        assert_ne!(on.data, off.data);
        s.optics.lens_profile = true;
        s.optics.profile_distortion = 0.0;
        assert_eq!(render(&source, &info, &s, 192).data, off.data);
        s.optics.profile_distortion = 50.0;
        let half = render(&source, &info, &s, 192);
        assert_ne!(half.data, off.data);
        assert_ne!(half.data, on.data);
        s.disabled_sections.push("optics".into());
        assert_eq!(render(&source, &info, &s, 192).data, off.data);
    }
}

/// A sensor-shifted warp must rotate with the sensor, and reduced development must retain its geometry.
#[test]
fn nikon_orientation_and_preview_export_geometry_agree() {
    for model in ["NIKON Z 6", "NIKON Z 7"] {
        let (source, info) = files::load_bytes(&nef(model, Orientation::Normal), 192).unwrap();
        let s = settings(&info);
        let full = render(&source, &info, &s, 192);
        for exif in 1..=8 {
            let orientation = Orientation::from_exif(exif);
            let (source, info) = files::load_bytes(&nef(model, orientation), 192).unwrap();
            let actual = render(&source, &info, &s, 192);
            let expected = full.oriented(orientation);
            let (mean, max) = difference(&actual, &expected);
            assert!(mean <= 0.5 && max <= 3, "{model} {orientation:?}: mean {mean}, max {max}");
        }
        let (preview, preview_info) = files::load_bytes(&nef(model, Orientation::Normal), 96).unwrap();
        let actual = render(&preview, &preview_info, &s, 96);
        let expected = lightcraft_raster::resample::resize(
            &full.map(|p| p.map(f32::from)),
            actual.width,
            actual.height,
            lightcraft_raster::resample::Filter::Lanczos3,
        )
        .map(|p| p.map(|v| v.round().clamp(0.0, 255.0) as u8));
        let (mean, max) = difference(&actual, &expected);
        eprintln!("{model} preview/full: mean {mean}, max {max}");
        assert!(mean <= 1.0 && max <= 8, "{model} preview/full: mean {mean}, max {max}");
        let full_warp = lightcraft_pipeline::optics::Warp::from_settings(source.width as f64, source.height as f64, &s, info.lens.as_ref());
        let preview_warp =
            lightcraft_pipeline::optics::Warp::from_settings(preview.width as f64, preview.height as f64, &s, preview_info.lens.as_ref());
        for x in [0.025, 0.5, 0.975] {
            for y in [0.025, 0.5, 0.975] {
                let a = full_warp.to_source(lightcraft_geom::Point::new(x * full_warp.w, y * full_warp.h), 1);
                let b = preview_warp.to_source(lightcraft_geom::Point::new(x * preview_warp.w, y * preview_warp.h), 1);
                assert!((a.x / full_warp.w - b.x / preview_warp.w).abs() < 1e-12);
                assert!((a.y / full_warp.h - b.y / preview_warp.h).abs() < 1e-12);
            }
        }
    }
}

/// Exercise the exact Nikon-derived profile on the GPU as well as the CPU reference.
#[test]
fn nikon_distortion_cpu_gpu_agree() {
    if !lightcraft_gpu::available() {
        let reason = lightcraft_gpu::unavailable_reason().unwrap_or_default();
        assert!(!reason.contains("kernels failed to build"), "{reason}");
        lightcraft_testkit::skip!(Gpu, "no GPU adapter for Nikon optics comparison: {reason}");
        return;
    }
    eprintln!("Nikon optics GPU: {}", lightcraft_gpu::adapter_name().unwrap_or_default());
    for model in ["NIKON Z 6", "NIKON Z 7"] {
        let (source, info) = files::load_bytes(&nef(model, Orientation::Rotate90), 192).unwrap();
        let source = Arc::new(source);
        let mut s = settings(&info);
        for amount in [0.0, 50.0, 100.0] {
            s.optics.profile_distortion = amount;
            let request = RenderRequest::fit(192, 192);
            let cpu = lightcraft_pipeline::render(&source, &info, &s, &request);
            let gpu = lightcraft_gpu::render(&source, &info, &s, &request, None).expect("available GPU must render Nikon optics");
            let (mean, max) = difference(&cpu.image, &gpu.image);
            assert!(mean <= 0.5 && max <= 3, "{model} {amount}: mean {mean}, max {max}");
        }
    }
}

struct Scratch(PathBuf);
impl Scratch {
    fn new(model: &str) -> Self {
        let p = std::env::temp_dir().join(format!("lc-nikon-lens-{}-{}", std::process::id(), model.replace(' ', "_")));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Give a synthetic old preview its actual pre-change look version, preserving the JPEG and facts tail.
fn old_header(bytes: &[u8]) -> Vec<u8> {
    let start = b"LCSP1\n".len();
    let end = start + bytes[start..].iter().position(|&b| b == b'\n').unwrap();
    let mut header: serde_json::Value = serde_json::from_slice(&bytes[start..end]).unwrap();
    header["look_version"] = json!(13);
    let mut out = bytes[..start].to_vec();
    out.extend(serde_json::to_vec(&header).unwrap());
    out.extend(&bytes[end..]);
    out
}

/// Reopen a pre-correction library, preserve edited settings, and migrate stored pixels and lens facts only online.
#[test]
fn nikon_existing_library_and_smart_preview_refresh_preserve_edits() {
    const { assert!(crate::media::RENDER_CACHE_VERSION > 75, "the source change must invalidate old renders") };
    for model in ["NIKON Z 6", "NIKON Z 7"] {
        let scratch = Scratch::new(model);
        let original = scratch.0.join("photo.nef");
        let library = scratch.0.join("library");
        let bytes = nef(model, Orientation::Normal);
        std::fs::write(&original, &bytes).unwrap();
        let mut s = Session::new().with_fs();
        s.open_library(&library, false).unwrap();
        s.execute("library.import", &json!({"paths":[original]})).unwrap();
        let id = s.active().unwrap();
        let mut edited = (*s.catalog.photo(id).unwrap().develop).clone();
        edited.light.exposure = 0.7;
        edited.optics.lens_profile = false;
        s.commit(
            "Old edit",
            Op::Batch {
                ops: vec![
                    Op::SetEmbeddedLens { id, lens: None },
                    Op::SetDevelop { id, settings: Arc::new(edited.clone()), label: "Old edit".into(), edited: Some("2026-10-12T00:00:00".into()) },
                ],
            },
        )
        .unwrap();
        s.persist_if_dirty();
        s.flush_log().unwrap();
        drop(s);
        let mut s = Session::new().with_fs();
        s.open_library(&library, false).unwrap();
        assert_eq!(*s.catalog.photo(id).unwrap().develop, edited);
        assert!(s.catalog.photo(id).unwrap().embedded_lens.is_none());
        let (source, mut info) = files::load_bytes(&bytes, 192).unwrap();
        let new_lens = info.lens;
        assert!(new_lens.is_some());
        info.lens = None;
        let old_tone = lightcraft_pipeline::tone::CameraTone::new(std::array::from_fn(|i| {
            let x = 0.004 * 1.17f32.powi(i as i32);
            [x, 0.95 * (1.0 - (-1.5 * x).exp())]
        }))
        .unwrap();
        info.camera_tone = Some(old_tone);
        let old_source = source.map(|p| p.map(|v| v * 0.5));
        let old = old_header(&crate::smart::encode(&old_source, Some(&info)).unwrap());
        let preview = crate::smart::sharded_file_path(s.media.smart_dir.as_ref().unwrap(), s.catalog.photo(id).unwrap());
        std::fs::create_dir_all(preview.parent().unwrap()).unwrap();
        std::fs::write(&preview, &old).unwrap();
        assert!(crate::smart::is_stale(&preview), "look version 13 must be older than this correction");
        let offline = scratch.0.join("offline.nef");
        std::fs::rename(&original, &offline).unwrap();
        s.media.forget(id);
        let result = s.execute("library.smartPreviews", &json!({"ids":[id.0]})).unwrap();
        assert_eq!(result["staleKept"], 1, "{result}");
        assert_eq!(std::fs::read(&preview).unwrap(), old);
        let (_, tone, stored) = crate::smart::decode(&old).unwrap();
        assert_eq!(tone, Some(old_tone));
        assert!(stored.unwrap().lens.is_none());
        let offline_render = s.render_now(id, 600, 600).unwrap();
        assert!(!offline_render.image.data.is_empty());
        let cached = s.media.source_ref(s.catalog.photo(id).unwrap(), crate::media::SourceLevel::Preview);
        assert!(matches!(cached, crate::media::SourceRef::Loaded(source) if source.from_proxy), "the offline loupe cached the proxy");
        std::fs::rename(&offline, &original).unwrap();
        let result = s.execute("library.smartPreviews", &json!({"ids":[id.0]})).unwrap();
        assert_eq!(result["refreshed"], 1, "{result}");
        assert!(!crate::smart::is_stale(&preview));
        let fresh = std::fs::read(&preview).unwrap();
        let (fresh_pixels, tone, stored) = crate::smart::decode(&fresh).unwrap();
        let (old_pixels, _, _) = crate::smart::decode(&old).unwrap();
        let error = |image: &Rgb32f| {
            image.data.iter().zip(&source.data).flat_map(|(a, b)| (0..3).map(move |c| (a[c] - b[c]).abs() as f64)).sum::<f64>()
                / (source.data.len() * 3) as f64
        };
        assert!(error(&fresh_pixels) < error(&old_pixels) / 4.0, "online rebuild replaces fitted pixels as well as header facts");
        assert_ne!(tone, Some(old_tone));
        assert_eq!(stored.unwrap().lens, new_lens);
        s.execute("photo.reload", &json!({"ids":[id.0]})).unwrap();
        assert_eq!(s.catalog.photo(id).unwrap().embedded_lens, new_lens);
        assert_eq!(*s.catalog.photo(id).unwrap().develop, edited, "reloading lens data preserves the entire edited settings value");
        s.persist_if_dirty();
        s.flush_log().unwrap();
        drop(s);
        let mut s = Session::new().with_fs();
        s.open_library(&library, false).unwrap();
        assert_eq!(s.catalog.photo(id).unwrap().embedded_lens, new_lens);
        assert_eq!(*s.catalog.photo(id).unwrap().develop, edited);
        let options = crate::export::ExportOptions { format: crate::export::ExportFormat::Dng, ..Default::default() };
        let exported = crate::export::export_photo(&mut s, id, &options, 1).unwrap();
        let dng = lightcraft_raw::decode(&exported.bytes).unwrap();
        assert_eq!(files::embedded_lens(&dng.info()), new_lens);
    }
}

/// The public real files agree with their header probes and contain no shipped vignette opcode.
#[test]
fn nikon_z6_z7_corpus_headers_and_full_decodes_agree() {
    let corpus =
        std::env::var_os("LIGHTCRAFT_CORPUS").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
    for id in [3584, 3581, 3582, 2792, 2789, 2790] {
        let path = corpus.join(format!("raw/coverage-{id}.nef"));
        if !path.exists() {
            lightcraft_testkit::skip!(Corpus, "Nikon coverage sample {id} absent");
            continue;
        }
        let bytes = std::fs::read(path).unwrap();
        let raw = lightcraft_raw::decode(&bytes).unwrap();
        assert_eq!(lightcraft_raw::probe_info(&bytes).unwrap(), raw.info());
        assert!(matches!(raw.opcodes.list3.as_slice(), [lightcraft_raw::Opcode::WarpRectilinear { .. }]));
    }
}
