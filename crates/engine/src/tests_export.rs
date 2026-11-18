//! Export end to end: output sizes, original + XMP, DNG.

use serde_json::json;

use crate::Session;
use crate::export::{ExportFormat, ExportOptions, export_photo};
use crate::tests_xmp::{synthetic_dng_with, synthetic_psd_with_metadata, temp_dir};

fn jpeg_with_xmp(metadata: &lcc_meta::Metadata) -> Vec<u8> {
    let pixels = [96u8; 12];
    let image = lcc_codecs::EncodeImage::new(2, 2, 3, lcc_codecs::Samples::U8(&pixels));
    let xmp = lcc_meta::write_xmp(metadata, None);
    let encoded = lcc_codecs::EncodeMeta { xmp: Some(&xmp), ..Default::default() };
    lcc_codecs::encode_jpeg(&image, 90, lcc_codecs::ChromaSubsampling::S444, &encoded).unwrap()
}

#[test]
fn full_size_export_of_a_cropped_photo_is_not_upscaled() {
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    let (pw, ph) = {
        let p = s.catalog.photo(id).unwrap();
        (p.width as usize, p.height as usize)
    };
    // (a quarter of each side: a full-size export of it is a sixteenth of the photo to render)
    s.execute("crop.set", &json!({"rect": [0.375, 0.375, 0.625, 0.625]})).unwrap();
    let e = export_photo(&mut s, id, &ExportOptions::default(), 1).unwrap();
    assert!(e.width.abs_diff(pw / 4) <= 1 && e.height.abs_diff(ph / 4) <= 1, "{}×{} from {pw}×{ph}", e.width, e.height);
    // and the size modes apply to the cropped size
    let o = ExportOptions::from_json(&json!({"percent": 50}));
    let e = export_photo(&mut s, id, &o, 1).unwrap();
    assert!(e.width.abs_diff(pw / 8) <= 1, "{} vs {}", e.width, pw / 8);
    let o = ExportOptions::from_json(&json!({"width": 200, "height": 200, "format": "png"}));
    let e = export_photo(&mut s, id, &o, 1).unwrap();
    assert_eq!(e.width.max(e.height), 200);
    // the print resolution lands in the file
    let i = e.bytes.windows(4).position(|w| w == b"pHYs").expect("pHYs");
    assert_eq!(u32::from_be_bytes(e.bytes[i + 4..i + 8].try_into().unwrap()), 9449, "240 ppi");
}

#[test]
fn original_export_copies_the_file_and_writes_its_edits_beside_it() {
    {
        let dir = temp_dir("export-orig");
        let src = dir.join("Shot.dng");
        let dng = synthetic_dng_with(None, Default::default());
        std::fs::write(&src, &dng).unwrap();
        let mut s = Session::new().with_fs();
        s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
        let id = s.catalog.photos().next().unwrap().id;
        s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
        s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.7})).unwrap();

        let o = ExportOptions::from_json(&json!({"format": "original", "naming": "{name}-{seq}"}));
        let e = export_photo(&mut s, id, &o, 3).unwrap();
        assert_eq!(e.file_name, "Shot-003.dng");
        assert_eq!(e.bytes, dng, "the original, byte for byte");
        assert_eq!(e.sidecars.len(), 1);
        let (ext, xmp) = &e.sidecars[0];
        assert_eq!(*ext, "xmp");
        let name = "Shot-003.xmp";
        let copy = dir.join("copy");
        std::fs::create_dir_all(&copy).unwrap();
        std::fs::write(copy.join(&e.file_name), &e.bytes).unwrap();
        std::fs::write(copy.join(name), xmp).unwrap();
        let mut s2 = Session::new().with_fs();
        s2.execute("library.import", &json!({"paths": [copy.join(&e.file_name).to_string_lossy()]})).unwrap();
        assert_eq!(s2.catalog.photos().next().unwrap().develop.light.exposure, 0.7, "edits travel in the sidecar");

        // DNG: re-encoded raw data with the edits embedded
        let o = ExportOptions { format: ExportFormat::Dng, ..Default::default() };
        let e = export_photo(&mut s, id, &o, 1).unwrap();
        assert_eq!(e.file_name, "Shot.dng");
        assert!(e.sidecars.is_empty());
        let back = lcc_raw::decode(&e.bytes).expect("our DNG decodes");
        let orig = lcc_raw::decode(&dng).unwrap();
        assert_eq!((back.width, back.height), (orig.width, orig.height));
        assert_eq!(back.data, orig.data, "lossless");
        let out = dir.join("out.dng");
        std::fs::write(&out, &e.bytes).unwrap();
        let mut s2 = Session::new().with_fs();
        s2.execute("library.import", &json!({"paths": [out.to_string_lossy()]})).unwrap();
        assert_eq!(s2.catalog.photos().next().unwrap().develop.light.exposure, 0.7, "edits embedded in the DNG are read back");
        let _ = std::fs::remove_dir_all(&dir);
    }
    {
        let mut s = Session::with_demo();
        let id = s.active().unwrap();
        for f in ["original", "dng"] {
            let err = export_photo(&mut s, id, &ExportOptions::from_json(&json!({"format": f})), 1).err().unwrap();
            assert!(err.contains("no original file"), "{err}");
        }
    }
    {
        let dir = temp_dir("export-dng-jpeg");
        let src = dir.join("a.png");
        let img = lcc_raster::Rgba8::from_fn(32, 24, |x, y| [(x * 8) as u8, (y * 10) as u8, 90, 255]);
        std::fs::write(&src, crate::export::encode_image(&img, &ExportOptions { format: ExportFormat::Png, ..Default::default() }).unwrap()).unwrap();
        let mut s = Session::new().with_fs();
        s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
        let id = s.catalog.photos().next().unwrap().id;
        let err = export_photo(&mut s, id, &ExportOptions { format: ExportFormat::Dng, ..Default::default() }, 1).err().unwrap();
        assert!(err.contains("needs a raw photo"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Review of #1966: a preset without `format` keeps its `bitDepth` until the format written is
/// known. With `path: a.png` it writes the 16-bit PNG it asks for; a format without that depth
/// gets its own, as stored settings do, while a depth the call itself passes is still refused.
#[test]
fn a_preset_depth_is_judged_against_the_format_written() {
    {
        use crate::export::ExportPreset;
        let mut s = Session::with_demo();
        let id = s.active().unwrap();
        s.export_presets.push(ExportPreset { name: "Deep".into(), params: json!({"bitDepth": 16, "longEdge": 64}), ..Default::default() });
        let p = s.export_params(&json!({"preset": "deep", "path": "/out/a.png"})).unwrap();
        let o = ExportOptions::from_params(&p).unwrap();
        assert_eq!((o.format, o.bit_depth), (ExportFormat::Png, Some(16)));
        assert_eq!(export_photo(&mut s, id, &o, 1).unwrap().bytes[24], 16, "a 16-bit PNG");
        let p = s.export_params(&json!({"preset": "deep", "path": "/out/a.jpg"})).unwrap();
        assert_eq!(ExportOptions::from_params(&p).unwrap().bit_depth, None);
        let p = s.export_params(&json!({"preset": "deep", "path": "/out/a.jpg", "bitDepth": 16})).unwrap();
        assert!(ExportOptions::from_params(&p).unwrap_err().to_string().contains("`bitDepth` must be 8 for jpeg"));
        // such a preset can be saved: its depth is judged when it exports; with a format, at once
        s.execute("export.savePreset", &json!({"name": "Deep 2", "params": {"bitDepth": 16}})).unwrap();
        assert!(s.execute("export.savePreset", &json!({"name": "Deep 3", "params": {"format": "jpeg", "bitDepth": 16}})).is_err());
    }
    // Review of #1966: savePreset without `params` saves the previous export, a stored setting. A
    // depth written before #1952 at the format's own depth is dropped, as Export with Previous does,
    // instead of refusing the save; without `format`, the depth waits for the format written.
    {
        let mut s = Session::with_demo();
        s.last_export = Some(json!({"format": "jpeg", "bitDepth": 16, "quality": 80, "dir": "/out"}));
        s.execute("export.savePreset", &json!({"name": "Old"})).unwrap();
        assert_eq!(
            s.export_presets[0].params,
            json!({"format": "jpeg", "quality": 80, "dir": "/out"}),
            "the folder is a setting, as in Lightroom's presets"
        );
        s.last_export = Some(json!({"bitDepth": 16, "dir": "/out"}));
        s.execute("export.savePreset", &json!({"name": "Deep"})).unwrap();
        let p = s.export_params(&json!({"preset": "Deep", "path": "/out/a.png"})).unwrap();
        assert_eq!(ExportOptions::from_params(&p).unwrap().bit_depth, Some(16));
    }
}

#[test]
fn presets_conflicts_and_subfolders() {
    {
        use crate::export::{Destination, export_batch};
        let mut s = Session::with_demo();
        let ids: Vec<_> = s.visible().iter().copied().take(2).collect();
        // built-ins and user presets; the call's own params override the preset's
        let names: Vec<String> =
            s.execute("export.presets", &json!({})).unwrap().as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap().to_string()).collect();
        assert!(names.contains(&"JPEG (Small)".to_string()) && names.contains(&"Original + Settings".to_string()), "{names:?}");
        let p = s.export_params(&json!({"preset": "jpeg (small)", "quality": 50})).unwrap();
        assert_eq!((p["longEdge"].as_u64(), p["quality"].as_u64(), p.get("preset")), (Some(2048), Some(50), None));
        assert!(s.export_params(&json!({"preset": "nope"})).unwrap_err().contains("unknown export preset"));
        assert!(s.execute("export.savePreset", &json!({"name": "JPEG (Large)", "params": {}})).is_err(), "built-in names are reserved");
        s.execute("export.savePreset", &json!({"name": "Web", "params": {"format": "png", "width": 64, "ids": [1], "dir": "/x"}})).unwrap();
        let web = s.export_params(&json!({"preset": "Web"})).unwrap();
        // the photos are not part of a preset; the destination folder is (as in Lightroom's presets)
        assert_eq!(web, json!({"format": "png", "width": 64, "dir": "/x"}), "ids are not part of a preset");
        s.execute("export.savePreset", &json!({"name": "web", "params": {"format": "webp"}})).unwrap();
        assert_eq!(s.export_presets.len(), 1, "same name (any case) replaces");
        s.execute("export.deletePreset", &json!({"name": "WEB"})).unwrap();
        assert!(s.export_presets.is_empty());

        // a fake file system: what exists, what was written
        let existing = ["out/Sub/LC01347.png".to_string()];
        let to = Destination { dir: "out".into(), exact: None };
        let batch = |s: &mut Session, ids: &[_], o: &ExportOptions| {
            let mut written: Vec<String> = Vec::new();
            let mut write = |p: &str, _: &[u8]| {
                written.push(p.to_string());
                Ok(())
            };
            let files = export_batch(s, ids, o, &to, &mut write, &|p| existing.contains(&p.to_string())).unwrap();
            (files, written)
        };
        let o = ExportOptions::from_json(&json!({"format": "png", "width": 32, "subfolder": "Sub", "naming": "LC01347"}));
        // both photos get the same name from the template: the first is renamed past the existing
        // file, the second past the first
        let (files, written) = batch(&mut s, &ids, &o);
        assert_eq!(written, ["out/Sub/LC01347-2.png", "out/Sub/LC01347-3.png"]);
        assert_eq!(files[0]["width"], 32);
        let (files, written) = batch(&mut s, &ids, &ExportOptions { conflict: crate::export::Conflict::Skip, ..o.clone() });
        assert_eq!(files.iter().filter(|f| f["skipped"] == "out/Sub/LC01347.png").count(), 2, "{files:?}");
        assert!(written.is_empty());
        let (_, written) = batch(&mut s, &ids[..1], &ExportOptions { conflict: crate::export::Conflict::Overwrite, ..o });
        assert_eq!(written, ["out/Sub/LC01347.png"]);
    }
    {
        let mut s = Session::with_demo();
        // the issue's four named built-ins are listed, marked builtin
        let builtin: Vec<String> = s
            .execute("export.presets", &json!({}))
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["builtin"].as_bool().unwrap())
            .map(|p| p["name"].as_str().unwrap().to_string())
            .collect();
        for name in ["Instagram Feed", "Instagram Story / Reel", "Web / Portfolio", "Full Resolution Master"] {
            assert!(builtin.contains(&name.to_string()), "{builtin:?}");
        }
        // expanding one yields exactly the issue's settings, and they validate as app.export params
        let feed = s.export_params(&json!({"preset": "Instagram Feed"})).unwrap();
        assert_eq!(feed, json!({"format": "jpeg", "quality": 85, "longEdge": 1080, "colorSpace": "srgb", "sharpen": "screen"}));
        ExportOptions::validate("app.export", &feed).unwrap();
        // round trip: save as a user preset → expand again → identical ExportOptions
        s.execute("export.savePreset", &json!({"name": "My Feed", "params": feed})).unwrap();
        let mine = s.export_params(&json!({"preset": "My Feed"})).unwrap();
        assert_eq!(ExportOptions::from_json(&feed), ExportOptions::from_json(&mine));
        // built-in names (the new ones too) are reserved
        assert!(s.execute("export.savePreset", &json!({"name": "web / portfolio", "params": {}})).is_err());
    }
}

#[test]
fn prepared_exports_run_on_another_thread() {
    fn send<T: Send>(_: &T) {}
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    let o = ExportOptions::from_json(&json!({"format": "png", "width": 48}));
    let items: Vec<_> = (1..=2).map(|seq| crate::export::prepare_export(&mut s, id, &o, seq).unwrap()).collect();
    send(&items);
    let to = crate::export::Destination::default();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    let files = std::thread::spawn(move || {
        let mut written = Vec::new();
        let mut write = |p: &str, b: &[u8]| {
            written.push((p.to_string(), b.len()));
            Ok(())
        };
        // cancel after the first photo
        crate::export::run_batch(items, &o, &to, &mut write, &|_| false, false, &mut |done, name| {
            seen2.lock().unwrap().push((done, name.to_string()));
            done < 1
        })
        .unwrap()
    })
    .join()
    .unwrap();
    assert_eq!(files.len(), 1, "cancelled before the second: {files:?}");
    assert_eq!(files[0]["width"], 48);
    assert_eq!(seen.lock().unwrap().len(), 2);
}

/// Issue #78: a GPU render whose work never ran (a driver that drops a submission without an
/// error) gave an all-black export. The export must come from the CPU instead — the same
/// image — with the reason recorded on the export itself.
///
/// Issue #675: this test runs beside every other engine test, many of them rendering on the GPU.
/// It asks only about its own export (`Exported::gpu_fallback`), never the process-wide
/// `lcc_gpu::last_fallback` another thread's render overwrites, and the simulated failure
/// switches the GPU off for this thread alone, so neither the fault nor `reset_failures` reaches
/// the other tests.
#[test]
fn export_falls_back_to_the_cpu_when_gpu_work_is_lost() {
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.4})).unwrap();
    s.execute("develop.set", &json!({"control": "effects.clarity", "value": 20})).unwrap();
    let o = ExportOptions::from_json(&json!({"format": "png", "longEdge": 640}));
    let mean = |b: &[u8]| {
        let d = lcc_codecs::decode(b, Default::default()).unwrap().image;
        d.data.iter().map(|p| (p[0] + p[1] + p[2]) as f64).sum::<f64>() / (3 * d.data.len()) as f64
    };
    let gpu = lcc_gpu::available();
    let healthy = mean(&export_photo(&mut s, id, &o, 1).unwrap().bytes);
    lcc_gpu::inject_fault(lcc_gpu::Fault::DropWork);
    let faulted = export_photo(&mut s, id, &o, 1).unwrap();
    if gpu {
        let why = faulted.gpu_fallback.clone().unwrap_or_default();
        assert!(why.contains("incomplete"), "{why}");
        // the GPU is off for this thread now: the next export is not even tried on it
        assert!(!lcc_gpu::available());
        assert!(lcc_gpu::unavailable_reason().unwrap_or_default().contains("incomplete"), "{:?}", lcc_gpu::unavailable_reason());
        // the thread's own `inject_fault` stopped only its own GPU use: the others still have it
        let elsewhere = std::thread::spawn(|| (lcc_gpu::available(), lcc_gpu::unavailable_reason())).join().unwrap();
        assert_eq!(elsewhere, (true, None), "a simulated failure must not switch the GPU off for the other threads");
    } else {
        assert_eq!(faulted.gpu_fallback, None);
    }
    let cpu = export_photo(&mut s, id, &o, 1).unwrap();
    assert_eq!(cpu.gpu_fallback, None, "not tried on the GPU: no fallback to report");
    lcc_gpu::reset_failures();
    assert_eq!(lcc_gpu::available(), gpu, "reset_failures gives this thread the GPU back");
    let px = |b: &[u8]| lcc_codecs::decode(b, Default::default()).unwrap().image.data;
    assert!(px(&faulted.bytes) == px(&cpu.bytes), "the fallback is the CPU render");
    let m = mean(&faulted.bytes);
    assert!(m > 0.02, "not black: mean {m}");
    assert!((m - healthy).abs() < 0.01, "GPU {healthy} vs CPU {m}");
}

/// A JPEG original `dir/IMG_1.jpg` imported into a library on disk.
fn library_with_jpeg(tag: &str) -> (Session, lcc_catalog::PhotoId, lcc_testkit::TempDir, std::path::PathBuf, Vec<u8>) {
    let dir = temp_dir(tag);
    let src = dir.join("IMG_1.jpg");
    let img = lcc_raster::Rgba8::from_fn(48, 32, |x, y| [(x * 5) as u8, (y * 7) as u8, 120, 255]);
    let bytes = crate::export::encode_image(&img, &ExportOptions::default()).unwrap();
    std::fs::write(&src, &bytes).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    s.execute("develop.set", &json!({"ids": [id.0], "control": "light.exposure", "value": 1.0})).unwrap();
    (s, id, dir, src, bytes)
}

fn preset_export_preserves_originals(command: &str, selection: serde_json::Value, tag: &str) {
    let (mut s, _, dir, src, original) = library_with_jpeg(tag);
    // something for mask.exportEffects to write (the other commands ignore it)
    s.mask_effects.push(crate::cmd::mask_effects::MaskEffect { name: "Glow".into(), ..Default::default() });
    let sidecar = src.with_extension("xmp");
    let appended_sidecar = dir.join("IMG_1.jpg.xmp");
    std::fs::write(&sidecar, b"existing sidecar").unwrap();
    std::fs::write(&appended_sidecar, b"appended sidecar").unwrap();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let mut targets = vec![src.clone(), dir.join("sub/../IMG_1.jpg"), sidecar.clone()];
    targets.push(appended_sidecar.clone());
    #[cfg(unix)]
    {
        let alias = dir.join("alias.jpg");
        std::os::unix::fs::symlink(&src, &alias).unwrap();
        targets.push(alias);
        let alias = dir.join("alias.xmp");
        std::os::unix::fs::symlink(&sidecar, &alias).unwrap();
        targets.push(alias);
    }
    for path in targets {
        let mut params = selection.clone();
        params["path"] = json!(path.to_string_lossy());
        let error = s.execute(command, &params).unwrap_err().to_string();
        assert!(error.contains("never writes over an original"), "{error}");
        assert_eq!(std::fs::read(&src).unwrap(), original);
        assert_eq!(std::fs::read(&sidecar).unwrap(), b"existing sidecar");
        assert_eq!(std::fs::read(&appended_sidecar).unwrap(), b"appended sidecar");
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// Exporting presets, curve presets, keywords (#1944: straight to `std::fs::write`) or mask effects (issue #2564)
/// never writes over a library original or its sidecar, however the path is spelled.
#[test]
fn preset_curve_keyword_and_mask_effect_exports_never_overwrite_originals_or_sidecars() {
    for (command, selection, tag) in [
        ("preset.export", json!({"ids": ["lc.warm-glow"]}), "preset-export-guard"),
        ("curve.exportPresets", json!({"names": ["Linear"]}), "curve-export-guard"),
        ("keyword.export", json!({}), "keyword-export-guard"),
        ("mask.exportEffects", json!({"names": ["Glow"]}), "effect-export-guard"),
    ] {
        preset_export_preserves_originals(command, selection, tag);
    }
}

/// A new file named like a photo's sidecar, in the photo's folder, would be read back as that
/// photo's edits: refused even though nothing is there yet.
#[test]
fn preset_export_never_creates_a_photos_sidecar() {
    let (mut s, _, dir, _, _) = library_with_jpeg("preset-export-new-sidecar");
    std::fs::create_dir_all(dir.join("presets")).unwrap();
    for name in ["IMG_1.xmp", "IMG_1.jpg.xmp", "presets/../IMG_1.xmp"] {
        let path = dir.join(name);
        assert!(!path.exists(), "{name} isn't there before the export");
        let error = s.execute("preset.export", &json!({"ids": ["lc.warm-glow"], "path": path.to_string_lossy()})).unwrap_err().to_string();
        assert!(error.contains("XMP sidecar of IMG_1.jpg"), "{error}");
        assert!(!path.exists(), "{name} was not created");
    }
    // the same name in another folder is an ordinary preset file
    let elsewhere = dir.join("presets/IMG_1.xmp");
    s.execute("preset.export", &json!({"ids": ["lc.warm-glow"], "path": elsewhere.to_string_lossy()})).unwrap();
    assert!(elsewhere.exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn preset_exports_preserve_previous_files_on_write_failure() {
    {
        let dir = temp_dir("preset-export-failure");
        let mut s = Session::new();
        s.mask_effects.push(crate::cmd::mask_effects::MaskEffect { name: "Glow".into(), ..Default::default() });
        for (command, selection, file) in [
            ("preset.export", json!({"ids": ["lc.warm-glow"]}), "look.lcpreset"),
            ("curve.exportPresets", json!({"names": ["Linear"]}), "curve.lccurve"),
            ("mask.exportEffects", json!({"names": ["Glow"]}), "effects.lceffect"),
        ] {
            let path = dir.join(file);
            let mut params = selection;
            params["path"] = json!(path.to_string_lossy());
            s.execute(command, &params).unwrap();
            let previous = std::fs::read(&path).unwrap();
            {
                let _fault = lcc_catalog::safe_file::fail_writes_after(4);
                assert!(s.execute(command, &params).is_err(), "{command} must report the failed write");
            }
            assert_eq!(std::fs::read(&path).unwrap(), previous, "{command} preserves the prior export");
            assert_eq!(s.execute(command, &params).unwrap()["count"], 1, "replacement still succeeds");
        }
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 3, "no temporary files remain");
        let _ = std::fs::remove_dir_all(dir);
    }
    {
        let dir = temp_dir("keyword-export-failure");
        let mut s = Session::with_demo();
        let path = dir.join("keywords.txt");
        let params = json!({"path": path.to_string_lossy()});
        s.execute("keyword.export", &params).unwrap();
        let previous = std::fs::read(&path).unwrap();
        {
            let _fault = lcc_catalog::safe_file::fail_writes_after(4);
            assert!(s.execute("keyword.export", &params).is_err(), "keyword.export must report the failed write");
        }
        assert_eq!(std::fs::read(&path).unwrap(), previous, "the prior export is kept");
        s.execute("keyword.export", &params).unwrap();
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temporary files remain");
        let _ = std::fs::remove_dir_all(dir);
    }
}

fn disk_batch(
    s: &mut Session,
    id: lcc_catalog::PhotoId,
    o: &ExportOptions,
    to: &crate::export::Destination,
) -> Result<Vec<serde_json::Value>, String> {
    crate::export::export_batch(s, &[id], o, to, &mut crate::export::write_file, &|p| std::path::Path::new(p).exists())
}

/// Upstream #1405: explicit paths honour the same conflict policy as folder exports, sidecars included, and an
/// original or a folder named as the path is refused under every policy.
#[test]
fn explicit_export_paths_follow_the_conflict_policy() {
    {
        use crate::export::{Conflict, Destination};
        let (mut s, id, dir, src, original) = library_with_jpeg("export-exact-conflict");
        let target = dir.join("chosen.name.jpg");
        std::fs::write(&target, b"precious existing file").unwrap();
        std::fs::write(dir.join("chosen.name-2.jpg"), b"previous numbered export").unwrap();
        let to = Destination { dir: dir.join("unused").to_string_lossy().into(), exact: Some(target.to_string_lossy().into()) };
        let opts = ExportOptions::default();
        let skipped = disk_batch(&mut s, id, &ExportOptions { conflict: Conflict::Skip, ..opts.clone() }, &to).unwrap();
        assert_eq!(skipped[0]["skipped"], target.to_string_lossy().as_ref());
        assert_eq!(std::fs::read(&target).unwrap(), b"precious existing file");
        let unique = disk_batch(&mut s, id, &ExportOptions { conflict: Conflict::Unique, ..opts.clone() }, &to).unwrap();
        assert_eq!(std::path::Path::new(unique[0]["path"].as_str().unwrap()), dir.join("chosen.name-3.jpg"));
        assert_eq!(std::fs::read(&target).unwrap(), b"precious existing file");
        disk_batch(&mut s, id, &ExportOptions { conflict: Conflict::Overwrite, ..opts.clone() }, &to).unwrap();
        assert_ne!(std::fs::read(&target).unwrap(), b"precious existing file");
        // an original or a folder is refused whatever the policy
        for exact in [src.to_string_lossy().to_string(), dir.to_string_lossy().to_string()] {
            for conflict in [Conflict::Unique, Conflict::Skip, Conflict::Overwrite] {
                let to = Destination { dir: String::new(), exact: Some(exact.clone()) };
                assert!(disk_batch(&mut s, id, &ExportOptions { conflict, ..opts.clone() }, &to).is_err(), "{exact} {conflict:?}");
            }
        }
        assert_eq!(std::fs::read(&src).unwrap(), original);
        let _ = std::fs::remove_dir_all(dir);
    }
    // Issue #93: the XMP sidecar of an "Original" export ignored the conflict policy.
    {
        use crate::export::{Conflict, Destination};
        let (mut s, id, dir, _src, original) = library_with_jpeg("export-sidecar");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let theirs = out.join("IMG_1.xmp");
        std::fs::write(&theirs, b"someone else's sidecar").unwrap();
        let to = Destination { dir: out.to_string_lossy().to_string(), exact: None };
        let o = ExportOptions { format: ExportFormat::Original, naming: "{name}".into(), ..Default::default() };

        // Unique: the photo and its sidecar both move to the next free name
        let files = disk_batch(&mut s, id, &ExportOptions { conflict: Conflict::Unique, ..o.clone() }, &to).unwrap();
        assert!(files[0]["path"].as_str().unwrap().ends_with("IMG_1-2.jpg"), "{files:?}");
        assert!(files[0]["sidecars"][0].as_str().unwrap().ends_with("IMG_1-2.xmp"), "{files:?}");
        assert_eq!(std::fs::read(&theirs).unwrap(), b"someone else's sidecar");
        assert_eq!(std::fs::read(out.join("IMG_1-2.jpg")).unwrap(), original);
        // Skip: a taken sidecar name skips the photo
        std::fs::remove_file(out.join("IMG_1-2.jpg")).unwrap();
        std::fs::remove_file(out.join("IMG_1-2.xmp")).unwrap();
        let files = disk_batch(&mut s, id, &ExportOptions { conflict: Conflict::Skip, ..o.clone() }, &to).unwrap();
        assert!(files[0]["skipped"].as_str().is_some(), "{files:?}");
        assert!(!out.join("IMG_1.jpg").exists());
        assert_eq!(std::fs::read(&theirs).unwrap(), b"someone else's sidecar");
        // Overwrite: replaced, as asked
        disk_batch(&mut s, id, &ExportOptions { conflict: Conflict::Overwrite, ..o }, &to).unwrap();
        assert!(std::fs::read_to_string(&theirs).unwrap().contains("xmpmeta"));
        let _ = std::fs::remove_dir_all(&dir);
    }
    {
        use crate::export::{sidecar_path_for_tests, split_file_name_for_tests};
        assert_eq!(sidecar_path_for_tests(r"C:\album.v1\chosen-2", "xmp"), r"C:\album.v1\chosen-2.xmp");
        assert_eq!(sidecar_path_for_tests("album.v1/chosen.name-3.jpg", "xmp"), "album.v1/chosen.name-3.xmp");
        assert_eq!(sidecar_path_for_tests("/a/.hidden", "xmp"), "/a/.hidden.xmp");
        assert_eq!(split_file_name_for_tests("a/b.tar.gz"), ("a/".to_string(), "b.tar".to_string(), Some("gz".to_string())));
    }
}

/// Issue #93: exporting into the photo's own folder as `{name}` with "Overwrite" replaced the
/// original with the re-encoded render. It is refused now, whatever the policy or exact path.
#[test]
fn export_never_overwrites_an_original() {
    use crate::export::{Conflict, Destination};
    let (mut s, id, dir, src, original) = library_with_jpeg("export-guard");
    let folder = Destination { dir: dir.to_string_lossy().to_string(), exact: None };
    let o = ExportOptions { naming: "{name}".into(), conflict: Conflict::Overwrite, ..Default::default() };
    let err = disk_batch(&mut s, id, &o, &folder).unwrap_err();
    assert!(err.contains("original of IMG_1.jpg"), "{err}");
    assert_eq!(std::fs::read(&src).unwrap(), original, "the original is byte-identical");

    // the exact path of the control channel / MCP, also spelled another way
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    for exact in [src.to_string_lossy().to_string(), dir.join("sub/../IMG_1.jpg").to_string_lossy().to_string()] {
        let to = Destination { dir: String::new(), exact: Some(exact) };
        assert!(disk_batch(&mut s, id, &o, &to).unwrap_err().contains("never writes over an original"));
    }
    // the "Original" format re-writing the file onto itself (and its sidecar)
    let orig = ExportOptions { format: ExportFormat::Original, ..o.clone() };
    assert!(disk_batch(&mut s, id, &orig, &folder).is_err());
    assert_eq!(std::fs::read(&src).unwrap(), original);
    assert!(s.execute("export.checkTarget", &json!({"path": src.to_string_lossy()})).is_err());
    assert!(s.execute("export.checkTarget", &json!({"path": dir.join("free.jpg").to_string_lossy()})).is_ok());

    // Unique moves past it; Overwrite still replaces an ordinary earlier export
    let unique = ExportOptions { conflict: Conflict::Unique, ..o.clone() };
    let files = disk_batch(&mut s, id, &unique, &folder).unwrap();
    assert!(files[0]["path"].as_str().unwrap().ends_with("IMG_1-2.jpg"), "{files:?}");
    let earlier = dir.join("IMG_1-2.jpg");
    std::fs::write(&earlier, b"an earlier export").unwrap();
    let named = ExportOptions { naming: "{name}-2".into(), ..o };
    disk_batch(&mut s, id, &named, &folder).unwrap();
    assert_ne!(std::fs::read(&earlier).unwrap(), b"an earlier export", "an ordinary file is overwritten");
    assert_eq!(std::fs::read(&src).unwrap(), original);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #2965: an export that waited its turn is checked against the library as it is when it starts. A
/// file catalogued at its destination while it waited is an original by then, and is not written over.
#[test]
fn a_waiting_export_checks_the_originals_catalogued_while_it_waited() {
    use crate::export::{Conflict, Destination, prepare_export, run_batch};
    let (mut s, id, dir, _src, _) = library_with_jpeg("export-waited");
    let later = dir.join("LATER.jpg");
    let img = lcc_raster::Rgba8::from_fn(16, 16, |x, _| [(x * 9) as u8, 40, 200, 255]);
    let theirs = crate::export::encode_image(&img, &ExportOptions::default()).unwrap();
    std::fs::write(&later, &theirs).unwrap();
    let o = ExportOptions { conflict: Conflict::Overwrite, ..Default::default() };
    let to = Destination { dir: String::new(), exact: Some(later.to_string_lossy().to_string()) };
    // prepared while LATER.jpg is still an ordinary file, which is imported while the export waits
    let mut waiting = prepare_export(&mut s, id, &o, 1).unwrap();
    s.execute("library.import", &json!({"paths": [later.to_string_lossy()]})).unwrap();
    waiting.use_guard(std::sync::Arc::new(s.original_guard()));
    let mut write = |path: &str, bytes: &[u8]| crate::export::write_file(path, bytes);
    let files = run_batch(vec![waiting], &o, &to, &mut write, &|p| std::path::Path::new(p).exists(), false, &mut |_, _| true).unwrap();
    assert!(files[0]["error"].as_str().is_some_and(|e| e.contains("original")), "{files:?}");
    assert_eq!(std::fs::read(&later).unwrap(), theirs, "the photo catalogued while the export waited is untouched");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #93: the native writer truncated the target first; a failure mid-write (full disk,
/// unplugged drive) now leaves the previous file intact and no partial one.
#[test]
fn a_failed_export_write_keeps_the_previous_file() {
    use crate::export::{Conflict, Destination};
    let (mut s, id, dir, _src, _) = library_with_jpeg("export-midwrite");
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let prev = out.join("IMG_1.jpg");
    std::fs::write(&prev, b"last week's export").unwrap();
    let to = Destination { dir: out.to_string_lossy().to_string(), exact: None };
    let o = ExportOptions { conflict: Conflict::Overwrite, ..Default::default() };
    {
        let _fault = lcc_catalog::safe_file::fail_writes_after(100);
        let err = disk_batch(&mut s, id, &o, &to).unwrap_err();
        assert!(err.contains("injected"), "{err}");
    }
    assert_eq!(std::fs::read(&prev).unwrap(), b"last week's export");
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1, "no temp file left behind");
    disk_batch(&mut s, id, &o, &to).unwrap();
    assert_ne!(std::fs::read(&prev).unwrap(), b"last week's export");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn export_protects_the_target_of_an_imported_symlink() {
    use crate::export::{Conflict, Destination};
    let (original_session, _, dir, src, original) = library_with_jpeg("export-symlink-original");
    drop(original_session);
    let alias = dir.join("alias.jpg");
    std::os::unix::fs::symlink(&src, &alias).unwrap();
    let sidecar = dir.join("metadata.xmp");
    std::fs::write(&sidecar, b"<x/>").unwrap();
    std::os::unix::fs::symlink(&sidecar, alias.with_extension("xmp")).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [alias.to_string_lossy()]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    s.execute("develop.set", &json!({"ids": [id.0], "control": "light.exposure", "value": 1.0})).unwrap();
    let o = ExportOptions { conflict: Conflict::Overwrite, ..Default::default() };
    let to = Destination { dir: String::new(), exact: Some(src.to_string_lossy().to_string()) };
    let result = disk_batch(&mut s, id, &o, &to);
    assert!(result.is_err(), "export replaced the original through its imported alias: {result:?}");
    assert_eq!(std::fs::read(&src).unwrap(), original);
    assert!(s.execute("export.checkTarget", &json!({"path": src.to_string_lossy()})).is_err());
    assert!(s.execute("export.checkTarget", &json!({"path": sidecar.to_string_lossy()})).is_err());
    assert_eq!(std::fs::read(&sidecar).unwrap(), b"<x/>");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #134: exports can always be made again, so they are written atomically but not synced to
/// disk file by file; an edit copy that becomes a library photo still is.
#[test]
fn exports_are_atomic_without_a_sync() {
    use crate::export::{Conflict, Destination};
    use lcc_catalog::safe_file::syncs_on_this_thread;
    let (mut s, id, dir, _src, _) = library_with_jpeg("export-nosync");
    let out = dir.join("out");
    let to = Destination { dir: out.to_string_lossy().to_string(), exact: None };
    let o = ExportOptions { conflict: Conflict::Unique, ..Default::default() };
    let before = syncs_on_this_thread();
    for _ in 0..3 {
        disk_batch(&mut s, id, &o, &to).unwrap();
    }
    assert_eq!(syncs_on_this_thread(), before, "no per-file sync for exports");
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 3, "three exports, no temp files");
    crate::export::write_file_durable(&out.join("edit.tif").to_string_lossy(), b"tiff").unwrap();
    assert!(syncs_on_this_thread() > before, "the durable writer syncs");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The demo library's ocean sunset (a sun far above SDR white), selected and active.
fn sunset(s: &mut Session) -> lcc_catalog::PhotoId {
    let id = s.catalog.photos().find(|p| p.meta.title == "Golden horizon").map(|p| p.id).unwrap();
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    id
}

#[test]
fn hdr_jpeg_export_writes_a_gain_map_and_sdr_edits_stay_plain() {
    use lcc_codecs::gainmap;
    let mut s = Session::with_demo();
    let id = sunset(&mut s);
    let o = ExportOptions::from_json(&json!({"longEdge": 320, "hdr": true}));
    assert!(o.hdr_output());
    // no HDR edit: an ordinary JPEG even with HDR output asked for
    let plain = export_photo(&mut s, id, &o, 1).unwrap();
    assert!(!gainmap::is_gain_map_jpeg(&plain.bytes));

    let r = s.execute("develop.hdr", &json!({"enabled": true, "maxEv": 3})).unwrap();
    assert_eq!(r["enabled"], true);
    let e = export_photo(&mut s, id, &o, 1).unwrap();
    assert!(gainmap::is_gain_map_jpeg(&e.bytes));
    let gm = gainmap::read_jpeg(&e.bytes).unwrap();
    assert!(gm.meta.alternate_headroom > 0.5 && gm.meta.alternate_headroom <= 3.05, "headroom {}", gm.meta.alternate_headroom);
    // the base is the edit's SDR rendition: the same pixels as an SDR export of the same HDR edit (the decoders
    // agree). Not the HDR-off export: the SDR rendition rolls highlights off instead of clipping them (#69).
    let sdr = export_photo(&mut s, id, &ExportOptions::from_json(&json!({"longEdge": 320, "hdr": false})), 1).unwrap();
    assert!(!gainmap::is_gain_map_jpeg(&sdr.bytes), "no HDR output asked for: a plain JPEG");
    let a = lcc_codecs::decode(&e.bytes, Default::default()).unwrap();
    let b = lcc_codecs::decode(&sdr.bytes, Default::default()).unwrap();
    assert_eq!((a.width, a.height), (b.width, b.height));
    let diff = a.image.data.iter().zip(&b.image.data).map(|(p, q)| (p[1] - q[1]).abs()).fold(0.0f32, f32::max);
    assert!(diff < 0.02, "base differs from the SDR export by {diff}");
    // HDR off again: the gain map goes away
    s.execute("develop.hdr", &json!({"enabled": false})).unwrap();
    assert!(!gainmap::is_gain_map_jpeg(&export_photo(&mut s, id, &o, 1).unwrap().bytes));
}

/// An HDR edit exported with HDR output on: AVIF (PQ, or HLG when asked), 16-bit PNG and JPEG XL are Rec. 2020 with
/// that transfer curve, and a float TIFF keeps the highlights above white. Without the HDR edit, or with HDR output
/// off, the same photo exports as an ordinary SDR file. A JPEG XL carries its metadata.
#[test]
fn hdr_exports_are_pq_or_hlg_rec2020_and_sdr_edits_stay_plain() {
    let mut s = Session::with_demo();
    let id = sunset(&mut s);
    let avif = ExportOptions::from_json(&json!({"longEdge": 160, "format": "avif", "hdr": true, "quality": 70}));
    let hlg = ExportOptions::from_json(&json!({"longEdge": 160, "format": "avif", "hdr": true, "hdrTransfer": "hlg", "quality": 70}));
    let png = ExportOptions::from_json(&json!({"longEdge": 160, "format": "png", "hdr": true}));
    let jxl = ExportOptions::from_json(&json!({"longEdge": 160, "format": "jxl", "hdr": true, "quality": 90}));
    assert!(avif.hdr_output() && png.hdr_output() && jxl.hdr_output());
    assert_eq!(png.effective_space(), lcc_pipeline::OutputSpace::Rec2020);
    assert_eq!(hlg.hdr_transfer, crate::export::HdrCurve::Hlg);
    assert_eq!(hlg.to_json()["hdrTransfer"], "hlg");
    // the `colr` nclx box of an AVIF: primaries, transfer, matrix, range
    let nclx = |b: &[u8]| b.windows(4).position(|w| w == b"nclx").map(|i| b[i + 4..i + 11].to_vec());
    let has = |b: &[u8], kind: &[u8; 4]| b.windows(4).any(|w| w == kind);
    let jxl_encoding = |b: &[u8]| {
        let img = jxl_oxide::JxlImage::builder().read(std::io::Cursor::new(b)).unwrap();
        let m = &img.image_header().metadata;
        (format!("{:?}", m.colour_encoding), m.bit_depth.bits_per_sample())
    };

    // a photo without an HDR edit exports as ordinary SDR files, even with HDR output asked for
    let sdr = export_photo(&mut s, id, &avif, 1).unwrap();
    assert!(nclx(&sdr.bytes).is_none_or(|c| c[3] != 16), "an ordinary (sRGB) AVIF");
    let sdr = export_photo(&mut s, id, &png, 1).unwrap();
    assert!(!has(&sdr.bytes, b"cICP"), "an ordinary PNG");
    assert_eq!(sdr.bytes[24], 8, "8-bit");
    let sdr = export_photo(&mut s, id, &jxl, 1).unwrap();
    let (c, _) = jxl_encoding(&sdr.bytes);
    assert!(!c.contains("Pq"), "{c}");
    // JPEG XL metadata: the title in the XMP box, an EXIF box
    let e = export_photo(&mut s, id, &ExportOptions::from_json(&json!({"longEdge": 120, "format": "jxl", "quality": 80})), 1).unwrap();
    let d = lcc_codecs::decode(&e.bytes, Default::default()).unwrap();
    assert_eq!(d.width.max(d.height), 120);
    assert!(d.xmp.as_deref().is_some_and(|x| x.contains("Golden horizon")), "title in the XMP box");
    assert!(d.exif.is_some(), "EXIF box");

    s.execute("develop.hdr", &json!({"enabled": true, "maxEv": 2})).unwrap();
    // AVIF: BT.2020 primaries (9), PQ (16), BT.2020 NCL (9), full range, with a content light level box
    let e = export_photo(&mut s, id, &avif, 1).unwrap();
    assert_eq!(nclx(&e.bytes).expect("colr nclx box"), vec![0, 9, 0, 16, 0, 9, 0x80]);
    assert!(has(&e.bytes, b"clli"), "content light level box");
    // HLG (18): no content light level box
    let e = export_photo(&mut s, id, &hlg, 1).unwrap();
    assert_eq!(nclx(&e.bytes).expect("colr nclx box"), vec![0, 9, 0, 18, 0, 9, 0x80]);
    assert!(!has(&e.bytes, b"clli"));
    // PNG: 16-bit with a cICP chunk
    let e = export_photo(&mut s, id, &png, 1).unwrap();
    let at = e.bytes.windows(4).position(|w| w == b"cICP").expect("cICP chunk");
    assert_eq!(&e.bytes[at + 4..at + 8], &[9, 16, 0, 1], "BT.2020, PQ, RGB, full range");
    assert_eq!(e.bytes[24], 16, "16-bit");
    // the highlights above SDR white are there: MaxCLL beyond the 203 cd/m² of SDR white
    let at = e.bytes.windows(4).position(|w| w == b"cLLI").expect("cLLI chunk");
    let max_cll = u32::from_be_bytes(e.bytes[at + 4..at + 8].try_into().unwrap()) as f32 / 10_000.0;
    assert!(max_cll > 250.0 && max_cll <= 4.0 * 203.0 + 1.0, "MaxCLL {max_cll}");
    // HDR output off: the HDR edit exports its SDR rendition
    let plain = export_photo(&mut s, id, &ExportOptions::from_json(&json!({"longEdge": 160, "format": "png"})), 1).unwrap();
    assert!(!has(&plain.bytes, b"cICP"));
    // JPEG XL
    let e = export_photo(&mut s, id, &jxl, 1).unwrap();
    let (c, bits) = jxl_encoding(&e.bytes);
    assert!(c.contains("Bt2100") && c.contains("Pq"), "{c}");
    assert_eq!(bits, 16);
    // float TIFF
    let max_of = |s: &mut Session, hdr: bool| {
        let o = ExportOptions::from_json(&json!({"longEdge": 240, "format": "tiff", "bitDepth": 32, "hdr": hdr}));
        let e = export_photo(s, id, &o, 1).unwrap();
        let d = lcc_codecs::decode(&e.bytes, Default::default()).unwrap();
        d.image.data.iter().flat_map(|p| p.iter().copied()).fold(0.0f32, f32::max)
    };
    let hdr = max_of(&mut s, true);
    let sdr = max_of(&mut s, false);
    assert!(sdr <= 1.0 + 1e-3, "SDR float TIFF stays in 0..1 ({sdr})");
    assert!(hdr > 1.2 && hdr <= 4.0 + 1e-3, "HDR float TIFF max {hdr}");
}

/// An exported AVIF's metadata reads back (issue #693: HEIF-family files had no metadata reader).
/// avif-serialize writes the EXIF as an `Exif` item in two extents, its offset field and the TIFF.
#[test]
fn exported_avif_metadata_reads_back() {
    use lcc_catalog::{Catalog, Photo, PhotoId, Source};
    let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, "a.jpg", "jpeg", 10, 10, "2026-09-30T00:00:00");
    p.meta.camera = "Synthetic X2".into();
    p.captured = Some("2026-10-12T12:34:56".into());
    let m = crate::export::export_metadata(&p, &Catalog::new(), &ExportOptions::default()).unwrap();
    let img = lcc_raster::Rgba8::new(48, 32);
    let avif = crate::export::encode_with_metadata(&img, &ExportOptions::from_json(&json!({"format": "avif", "quality": 60})), Some(&m)).unwrap();
    assert_eq!(lcc_codecs::sniff(&avif), Some(lcc_codecs::Format::Avif));
    let back = lcc_meta::extract(&avif);
    assert_eq!(back.model.as_deref(), Some("Synthetic X2"));
    assert_eq!(back.capture_time.map(|d| d.to_iso()).as_deref(), Some("2026-10-12T12:34:56"));
}

/// Import → `photo.inspect` → export keeps a long exposure time (issue #1619): a 0.8 s photo reads as 0.8
/// and leaves the export as 0.8 s, instead of being rounded to a whole stop on the way in (`1/1` → 1.0 s).
#[test]
fn an_imported_long_exposure_survives_the_trip_to_the_export() {
    let dir = temp_dir("export-shutter-step");
    let src = dir.join("lc_exposure_0.8s.jpg");
    let in_meta = lcc_meta::Metadata { exposure_time: Some(0.8), ..Default::default() };
    let img = lcc_raster::Rgba8::from_fn(64, 48, |x, y| [(x * 4) as u8, (y * 5) as u8, 90, 255]);
    let o = ExportOptions::from_json(&json!({"format": "jpeg"}));
    std::fs::write(&src, crate::export::encode_with_metadata(&img, &o, Some(&in_meta)).unwrap()).unwrap();

    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    assert_eq!(s.catalog.photo(id).unwrap().meta.shutter, "0.8", "the catalog shows the file's own step");

    let e = export_photo(&mut s, id, &o, 1).unwrap();
    let back = lcc_meta::extract(&e.bytes);
    let t = back.exposure_time.expect("the export kept an exposure time");
    assert!((t - 0.8).abs() < 0.01, "0.8 s in, {t} s out");
    let _ = std::fs::remove_dir_all(&dir);
}

/// "Remove location info" holds for the file-copying formats too: the sidecar and the DNG's
/// packet carry no location, and an original that embeds GPS is refused rather than copied.
#[test]
fn remove_location_holds_for_original_and_dng_exports() {
    let dir = temp_dir("export-noloc");
    let gps = lcc_meta::Metadata { gps: Some(lcc_meta::Gps { latitude: 48.85, longitude: 2.35, altitude: None }), ..Default::default() };
    // a raw with no embedded GPS, located in the catalog
    let raw = dir.join("Raw.dng");
    std::fs::write(&raw, synthetic_dng_with(None, Default::default())).unwrap();
    // a JPEG with GPS in its EXIF
    let jpg = dir.join("Geo.jpg");
    let img = lcc_raster::Rgba8::from_fn(32, 24, |x, y| [(x * 8) as u8, (y * 10) as u8, 90, 255]);
    let exif = lcc_meta::write_exif(&gps);
    let meta = lcc_codecs::EncodeMeta { exif: Some(&exif), ..Default::default() };
    std::fs::write(&jpg, lcc_codecs::encode_jpeg(&lcc_codecs::EncodeImage::rgba8(&img), 90, Default::default(), &meta).unwrap()).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [raw.to_string_lossy(), jpg.to_string_lossy()]})).unwrap();
    let id = |s: &Session, n: &str| s.catalog.photos().find(|p| p.file_name == n).unwrap().id;
    let (rid, jid) = (id(&s, "Raw.dng"), id(&s, "Geo.jpg"));
    s.execute("photo.setMeta", &json!({"ids": [rid.0, jid.0], "gps": [48.85, 2.35], "city": "Paris"})).unwrap();
    let opts = |f: &str, remove: bool| ExportOptions::from_json(&json!({"format": f, "removeLocation": remove}));

    // original raw: the sidecar keeps the location unless it is removed
    let side = |e: &crate::export::Exported| lcc_meta::parse_xmp(std::str::from_utf8(&e.sidecars[0].1).unwrap()).unwrap().metadata;
    let kept = side(&export_photo(&mut s, rid, &opts("original", false), 1).unwrap());
    assert!(kept.gps.is_some() && kept.city.is_some());
    let gone = side(&export_photo(&mut s, rid, &opts("original", true), 1).unwrap());
    assert!(gone.gps.is_none() && gone.city.is_none(), "{gone:?}");

    // dng: the embedded packet
    let e = export_photo(&mut s, rid, &opts("dng", false), 1).unwrap();
    assert!(lcc_meta::extract(&e.bytes).gps.is_some());
    let e = export_photo(&mut s, rid, &opts("dng", true), 1).unwrap();
    let m = lcc_meta::extract(&e.bytes);
    assert!(m.gps.is_none() && m.city.is_none(), "{m:?}");

    // original JPEG embedding GPS: copied as is normally, refused when the location must go
    let e = export_photo(&mut s, jid, &opts("original", false), 1).unwrap();
    assert!(lcc_meta::extract(&e.bytes).gps.is_some());
    let err = export_photo(&mut s, jid, &opts("original", true), 1).err().unwrap();
    assert!(err.contains("can't remove the location") && err.contains("export as JPEG"), "{err}");
    assert!(!err.contains("DNG"), "a JPEG can't be exported as DNG: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// "Remove location info" refuses to copy an original whose own metadata holds a location, or metadata it can't read
/// to rule one out, whatever the container: place names in a JPEG's XMP, an unreadable XMP packet, a GPS property
/// that can't be parsed (upstream #3054), a position only in the Extended XMP (issue #2581), a raw's own GPS (it can
/// go out as DNG), and a PSD's GPS, unreadable XMP or unreadable resources.
#[test]
fn remove_location_refuses_originals_that_embed_or_may_embed_a_location() {
    let dir = temp_dir("export-noloc-refused");
    let gps = lcc_meta::Metadata { gps: Some(lcc_meta::Gps { latitude: 45.0, longitude: 7.0, altitude: None }), ..Default::default() };
    let jpeg_with_raw_xmp = |xmp: &str| {
        let pixels = [96u8; 12];
        let image = lcc_codecs::EncodeImage::new(2, 2, 3, lcc_codecs::Samples::U8(&pixels));
        let encoded = lcc_codecs::EncodeMeta { xmp: Some(xmp), ..Default::default() };
        lcc_codecs::encode_jpeg(&image, 90, lcc_codecs::ChromaSubsampling::S444, &encoded).unwrap()
    };
    let psd_with_raw_xmp = |xmp: &str| {
        let pixels = [40, 80, 120];
        let image = lcc_codecs::EncodeImage::new(1, 1, 3, lcc_codecs::Samples::U8(&pixels));
        let embedded = lcc_codecs::EncodeMeta { xmp: Some(xmp), ..Default::default() };
        lcc_codecs::encode_psd(&image, &embedded).unwrap()
    };
    // (file name, its bytes, what the refusal says)
    let mut cases: Vec<(String, Vec<u8>, &[&str])> = Vec::new();

    // place names, one per file and no GPS
    let place = "Embedded Place";
    for (field, metadata) in [
        ("sublocation", lcc_meta::Metadata { sublocation: Some(place.into()), ..Default::default() }),
        ("city", lcc_meta::Metadata { city: Some(place.into()), ..Default::default() }),
        ("state", lcc_meta::Metadata { state: Some(place.into()), ..Default::default() }),
        ("country", lcc_meta::Metadata { country: Some(place.into()), ..Default::default() }),
    ] {
        let bytes = jpeg_with_xmp(&metadata);
        let embedded = lcc_meta::try_extract(&bytes).unwrap();
        assert!(embedded.gps.is_none(), "{field} fixture should isolate place names");
        let read = [embedded.sublocation.as_deref(), embedded.city.as_deref(), embedded.state.as_deref(), embedded.country.as_deref()];
        assert_eq!(read.iter().filter(|v| **v == Some(place)).count(), 1, "{field} fixture should be parsed: {read:?}");
        cases.push((format!("{field}.jpg"), bytes, &["place names", "export as JPEG or TIFF"]));
    }

    // a JPEG whose XMP can't be read
    let broken = "<x:xmpmeta><broken";
    assert!(lcc_meta::parse_xmp(broken).is_err(), "fixture XMP should be malformed");
    let bytes = jpeg_with_raw_xmp(broken);
    let metadata = lcc_meta::try_extract(&bytes).unwrap();
    assert!(metadata.gps.is_none() && metadata.city.is_none(), "best-effort extraction omits malformed XMP");
    cases.push(("BrokenXmp.jpg".into(), bytes, &["can't verify location metadata", "malformed embedded XMP"]));

    // a GPS property whose value is malformed: not projected into `Metadata`, still in the file's XMP
    let bad_gps = r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:exif="http://ns.adobe.com/exif/1.0/" exif:GPSLatitude="45,60N" exif:GPSLongitude="7,0E"/></rdf:RDF>"#;
    cases.push(("MalformedGps.jpg".into(), jpeg_with_raw_xmp(bad_gps), &["can't", "location"]));

    // a position that lives only in the Extended XMP
    let bytes = jpeg_with_extended_gps();
    assert!(lcc_meta::embedded(&bytes).xmp.as_deref().is_some_and(|x| !x.contains("GPSLatitude")), "the main packet holds no position");
    cases.push(("Extended.jpg".into(), bytes, &["can't remove the location"]));

    // a raw whose own metadata holds GPS
    let raw_gps = lcc_meta::Metadata { gps: Some(lcc_meta::Gps { latitude: 10.0, longitude: 20.0, altitude: None }), ..Default::default() };
    let bytes = synthetic_dng_with(Some(&lcc_meta::write_xmp(&raw_gps, None)), Default::default());
    assert!(lcc_meta::extract(&bytes).gps.is_some(), "fixture embeds GPS");
    cases.push(("RawGps.dng".into(), bytes, &["can't remove the location", "DNG"]));

    // PSD: EXIF GPS
    let geo_psd = synthetic_psd_with_metadata(&gps);
    assert_eq!(lcc_meta::extract(&geo_psd).gps, gps.gps, "the fixture embeds EXIF GPS");
    cases.push(("ExifGps.psd".into(), geo_psd.clone(), &["can't remove the location", "export as JPEG"]));
    // PSD: unreadable XMP
    let bytes = psd_with_raw_xmp(broken);
    assert_eq!(lcc_meta::extract(&bytes), lcc_meta::Metadata::default(), "ordinary extraction stays best-effort");
    cases.push(("BrokenXmpPsd.psd".into(), bytes, &["can't verify location metadata", "malformed PSD/PSB"]));
    // PSD: XMP that is not UTF-8
    let mut bytes = psd_with_raw_xmp(&lcc_meta::write_xmp(&gps, None));
    assert_eq!(lcc_meta::extract(&bytes).gps, gps.gps);
    let latitude = bytes.windows(2).position(|bytes| bytes == b"45").unwrap();
    bytes[latitude] = 0xff;
    assert!(lcc_meta::extract(&bytes).gps.is_none());
    cases.push(("InvalidUtf8.psd".into(), bytes, &["can't verify location metadata", "malformed PSD/PSB"]));
    // PSD: a malformed resource after the readable ones
    let mut bytes = geo_psd;
    let colour_length = usize::try_from(u32::from_be_bytes(bytes[26..30].try_into().unwrap())).unwrap();
    let resource_length_at = 30 + colour_length;
    let resource_length = usize::try_from(u32::from_be_bytes(bytes[resource_length_at..resource_length_at + 4].try_into().unwrap())).unwrap();
    let resource_end = resource_length_at + 4 + resource_length;
    bytes.splice(resource_end..resource_end, *b"BAD!");
    bytes[resource_length_at..resource_length_at + 4].copy_from_slice(&u32::try_from(resource_length + 4).unwrap().to_be_bytes());
    assert!(lcc_meta::extract(&bytes).gps.is_none(), "the malformed trailing resource reproduces the metadata gap");
    cases.push(("Resources.psd".into(), bytes, &["can't verify location metadata", "malformed PSD/PSB"]));

    let mut paths = Vec::new();
    for (name, bytes, _) in &cases {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        paths.push(path.to_string_lossy().to_string());
    }
    let mut session = Session::new().with_fs();
    session.execute("library.import", &json!({"paths": paths})).unwrap();
    let options = ExportOptions { format: ExportFormat::Original, remove_location: true, ..Default::default() };
    for (name, _, says) in &cases {
        let Some(id) = session.catalog.photos().find(|p| p.file_name == *name).map(|p| p.id) else { panic!("{name} was not imported") };
        let Err(error) = export_photo(&mut session, id, &options, 1) else { panic!("{name}: the Original export must be refused") };
        for phrase in *says {
            assert!(error.contains(*phrase), "{name}: {error}");
        }
    }
    // All Metadata lists the properties a JPEG keeps in its Extended XMP (issue #2581)
    let Some(id) = session.catalog.photos().find(|p| p.file_name == "Extended.jpg").map(|p| p.id) else { panic!("Extended.jpg") };
    let all = session.execute("photo.allMetadata", &json!({"id": id.0})).unwrap();
    let names: Vec<&str> = all["xmp"].as_array().unwrap().iter().filter_map(|r| r["name"].as_str()).collect();
    assert!(names.contains(&"exif:GPSLatitude") && names.contains(&"exif:GPSLongitude"), "{names:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn remove_location_allows_original_without_embedded_location() {
    {
        let dir = temp_dir("export-noloc-place-controls");
        let embedded_path = dir.join("Embedded.jpg");
        let no_location_path = dir.join("NoLocation.jpg");
        let blank_path = dir.join("Blank.jpg");
        let embedded = jpeg_with_xmp(&lcc_meta::Metadata { city: Some("Embedded Place".into()), ..Default::default() });
        let no_location = jpeg_with_xmp(&Default::default());
        let blank = jpeg_with_xmp(&lcc_meta::Metadata { city: Some("  \t".into()), ..Default::default() });
        std::fs::write(&embedded_path, &embedded).unwrap();
        std::fs::write(&no_location_path, &no_location).unwrap();
        std::fs::write(&blank_path, &blank).unwrap();
        assert!(lcc_meta::try_extract(&blank).unwrap().city.is_none(), "whitespace-only XMP place names are blank");

        let mut session = Session::new().with_fs();
        session
            .execute(
                "library.import",
                &json!({"paths": [embedded_path.to_string_lossy(), no_location_path.to_string_lossy(), blank_path.to_string_lossy()]}),
            )
            .unwrap();
        let photo_id = |name: &str| session.catalog.photos().find(|p| p.file_name == name).unwrap().id;
        let embedded_id = photo_id("Embedded.jpg");
        let no_location_id = photo_id("NoLocation.jpg");
        let blank_id = photo_id("Blank.jpg");

        let keep = ExportOptions { format: ExportFormat::Original, ..Default::default() };
        let kept = export_photo(&mut session, embedded_id, &keep, 1).unwrap();
        assert_eq!(kept.bytes, embedded, "removeLocation=false still copies the source byte for byte");
        assert_eq!(lcc_meta::try_extract(&kept.bytes).unwrap().city.as_deref(), Some("Embedded Place"));

        let remove = ExportOptions { format: ExportFormat::Original, remove_location: true, ..Default::default() };
        for (id, source) in [(no_location_id, &no_location), (blank_id, &blank)] {
            let exported = export_photo(&mut session, id, &remove, 1).unwrap();
            assert_eq!(&exported.bytes, source);
        }

        session.execute("photo.setMeta", &json!({"ids": [no_location_id.0], "city": "Catalog-only Place"})).unwrap();
        let catalog_only = export_photo(&mut session, no_location_id, &remove, 1).unwrap();
        let sidecar = catalog_only.sidecars.first().unwrap().1.as_slice();
        let packet = std::str::from_utf8(sidecar).unwrap();
        assert!(lcc_meta::parse_xmp(packet).unwrap().metadata.city.is_none(), "catalog-only place name is removed from the sidecar");

        let _ = std::fs::remove_dir_all(&dir);
    }
    {
        use crate::export::{Destination, prepare_batch, run_batch};

        let dir = temp_dir("export-noloc-place-no-write");
        let source = dir.join("City.jpg");
        let original = jpeg_with_xmp(&lcc_meta::Metadata { city: Some("Embedded Place".into()), ..Default::default() });
        std::fs::write(&source, &original).unwrap();
        let output = dir.join("out");
        std::fs::create_dir_all(&output).unwrap();

        let mut session = Session::new().with_fs();
        session.execute("library.import", &json!({"paths": [source.to_string_lossy()]})).unwrap();
        let id = session.catalog.photos().next().unwrap().id;
        let options = ExportOptions { format: ExportFormat::Original, remove_location: true, ..Default::default() };
        let items = prepare_batch(&mut session, &[id], &options).unwrap();
        let destination = Destination { dir: output.to_string_lossy().to_string(), exact: None };
        let mut writes = Vec::new();
        let mut write = |path: &str, bytes: &[u8]| -> Result<(), String> {
            writes.push(path.to_string());
            std::fs::write(path, bytes).map_err(|error| error.to_string())
        };
        let exists = |path: &str| std::path::Path::new(path).exists();
        let result = run_batch(items, &options, &destination, &mut write, &exists, true, &mut |_, _| true);

        let error = result.unwrap_err();
        assert!(error.contains("place names"), "{error}");
        assert!(writes.is_empty(), "refused export called the writer: {writes:?}");
        assert!(!output.join("City.jpg").exists());
        assert!(!output.join("City.xmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
    {
        let dir = temp_dir("export-noloc-place-catalog-clear");
        let source = dir.join("City.jpg");
        let original = jpeg_with_xmp(&lcc_meta::Metadata { city: Some("Embedded Place".into()), ..Default::default() });
        std::fs::write(&source, &original).unwrap();

        let mut session = Session::new().with_fs();
        session.execute("library.import", &json!({"paths": [source.to_string_lossy()]})).unwrap();
        let id = session.catalog.photos().next().unwrap().id;
        assert_eq!(session.catalog.photo(id).unwrap().meta.city, "Embedded Place");
        session.execute("photo.setMeta", &json!({"ids": [id.0], "city": ""})).unwrap();
        assert!(session.catalog.photo(id).unwrap().meta.city.is_empty());
        assert_eq!(lcc_meta::try_extract(&std::fs::read(&source).unwrap()).unwrap().city.as_deref(), Some("Embedded Place"));

        let options = ExportOptions { format: ExportFormat::Original, remove_location: true, ..Default::default() };
        let error = export_photo(&mut session, id, &options, 1).err().expect("embedded place name should refuse Original export");
        assert!(error.contains("place names"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// `metadata: "NONE"` as `app.export` sends it: the JPEG carries no location or camera data
/// (it used to be read as `all` and carry both).
#[test]
fn metadata_none_in_any_case_exports_no_metadata() {
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    s.execute("photo.setMeta", &json!({"ids": [id.0], "gps": [48.85, 2.35], "city": "Paris"})).unwrap();
    let export = |s: &mut Session, extra: serde_json::Value| {
        let mut p = json!({"format": "jpeg", "longEdge": 64});
        for (k, v) in extra.as_object().unwrap() {
            p[k] = v.clone();
        }
        lcc_meta::extract(&export_photo(s, id, &ExportOptions::from_params(&p).unwrap(), 1).unwrap().bytes)
    };
    let all = export(&mut s, json!({}));
    assert!(all.gps.is_some() && all.city.is_some(), "the default keeps everything: {all:?}");
    for v in ["none", "NONE", "None"] {
        let m = export(&mut s, json!({"metadata": v}));
        assert!(m.gps.is_none() && m.city.is_none() && m.model.is_none(), "{v}: {m:?}");
    }
    let m = export(&mut s, json!({"metadata": "ALL", "removeLocation": true}));
    assert!(m.gps.is_none() && m.city.is_none(), "{m:?}");
}

/// A HEIC whose only GPS is in its thumbnail's EXIF (the pictures have no data: an original's
/// export never decodes it). Item 1 is the photo, 2 its thumbnail, 3 the thumbnail's EXIF, kept in
/// the `meta` box's `idat`.
fn heic_with_gps_in_its_thumbnail() -> Vec<u8> {
    let gps = lcc_meta::Gps { latitude: 48.85, longitude: 2.35, altitude: None };
    let exif = [&[0u8, 0, 0, 0][..], &lcc_meta::write_exif(&lcc_meta::Metadata { gps: Some(gps), ..Default::default() })].concat();
    heic_with_metadata_in_its_thumbnail(b"Exif", &exif)
}

fn heic_with_metadata_in_its_thumbnail(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let bx = |kind: &[u8], body: &[u8]| [&((body.len() + 8) as u32).to_be_bytes()[..], kind, body].concat();
    let full = |kind: &[u8], body: &[u8]| bx(kind, &[&[0u8, 0, 0, 0][..], body].concat());
    let infe = |id: u8, kind: &[u8]| {
        let fields = if kind == b"mime" { b"\0application/rdf+xml\0".as_slice() } else { b"\0" };
        bx(b"infe", &[&[2, 0, 0, 0, 0, id, 0, 0][..], kind, fields].concat())
    };
    let iinf = full(b"iinf", &[&[0, 3][..], &infe(1, b"hvc1"), &infe(2, b"hvc1"), &infe(3, kind)].concat());
    let iref = full(b"iref", &[bx(b"thmb", &[0, 2, 0, 1, 0, 1]), bx(b"cdsc", &[0, 3, 0, 1, 0, 2])].concat());
    // `iloc` version 1, 4-byte offsets and lengths: item 3 from `idat` (construction method 1)
    let mut iloc = vec![1, 0, 0, 0, 0x44, 0, 0, 1, 0, 3, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0];
    iloc.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    let meta = full(b"meta", &[full(b"pitm", &[0, 1]), iinf, iref, bx(b"iloc", &iloc), bx(b"idat", payload)].concat());
    [bx(b"ftyp", b"heic\0\0\0\0mif1heic"), meta].concat()
}

/// "Remove location info" refuses an original that embeds GPS anywhere it can be read, here only in a HEIC's
/// thumbnail (#693 review), and one whose `iloc` or item catalog is too damaged to rule it out.
#[test]
fn remove_location_refuses_a_heic_whose_gps_cannot_be_ruled_out() {
    // A listed metadata item with an unreadable placement is not proof that an original has no GPS.
    // Exercise the real file-copy path; these damaged containers do not need a working decoder.
    {
        use lcc_catalog::{Op, Photo, Source};
        let dir = temp_dir("export-noloc-heic-iloc");
        let path = dir.join("Placement.HEIC");
        let mut s = Session::new().with_fs();
        let id = s.catalog.alloc_photo_id();
        let p = Photo::new(id, Source::File { path: path.to_string_lossy().into() }, "Placement.HEIC", "HEIC", 32, 32, "2026-10-12T00:00:00");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        let opts = |remove: bool| ExportOptions::from_json(&json!({"format": "original", "removeLocation": remove}));

        let original = heic_with_gps_in_its_thumbnail();
        assert!(lcc_meta::embeds_location(&original), "the intact fixture has GPS");
        let kind_at = original.windows(4).position(|w| w == b"iloc").unwrap();
        let body = kind_at + 4;
        for damage in ["none", "missing", "version", "field size", "truncated entry"] {
            let mut bytes = original.clone();
            match damage {
                "missing" => bytes[kind_at..body].copy_from_slice(b"free"),
                "version" => bytes[body] = 3,
                "field size" => bytes[body + 4] = 0x24,
                "truncated entry" => bytes[body + 15] = 2, // two extents declared, only one present
                _ => {}
            }
            std::fs::write(&path, &bytes).unwrap();
            assert_eq!(export_photo(&mut s, id, &opts(false), 1).unwrap().bytes, bytes, "location may stay: {damage}");
            let err = export_photo(&mut s, id, &opts(true), 1).err().expect("unresolved GPS must not be copied");
            assert!(err.contains("can't remove the location") && err.contains("export as JPEG or TIFF"), "{damage}: {err}");
        }

        // Ordinary files without GPS still export, with and without an EXIF item.
        for exif in [None, Some(lcc_meta::write_exif(&lcc_meta::Metadata { model: Some("Synthetic".into()), ..Default::default() }))] {
            let mut spec = lcc_heif::testdata::Spec::new(32, 32, &|_, _| [0, 0, 0]);
            spec.exif = exif;
            let bytes = lcc_heif::testdata::build(&spec);
            std::fs::write(&path, &bytes).unwrap();
            assert_eq!(export_photo(&mut s, id, &opts(true), 1).unwrap().bytes, bytes);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    // A damaged item catalog cannot authorize copying the GPS bytes that `iloc` still references.
    {
        let original = heic_with_gps_in_its_thumbnail();
        let idat = original.windows(4).position(|w| w == b"idat").unwrap();
        assert!(lcc_meta::try_read_exif(&original[idat + 8..]).unwrap().gps.is_some(), "real EXIF payload");
        assert_incomplete_catalog_refused(&original, "exif");
    }
    {
        let gps = lcc_meta::Metadata { gps: Some(lcc_meta::Gps { latitude: 48.85, longitude: 2.35, altitude: None }), ..Default::default() };
        let xmp = lcc_meta::write_xmp(&gps, None);
        assert!(lcc_meta::parse_xmp(&xmp).unwrap().metadata.gps.is_some(), "real XMP payload");
        assert_incomplete_catalog_refused(&heic_with_metadata_in_its_thumbnail(b"mime", xmp.as_bytes()), "xmp");
    }
}

fn assert_incomplete_catalog_refused(original: &[u8], label: &str) {
    use lcc_catalog::{Op, Photo, Source};
    let dir = temp_dir(&format!("export-noloc-heic-catalog-{label}"));
    let path = dir.join("Catalog.HEIC");
    let mut s = Session::new().with_fs();
    let id = s.catalog.alloc_photo_id();
    let p = Photo::new(id, Source::File { path: path.to_string_lossy().into() }, "Catalog.HEIC", "HEIC", 32, 32, "2026-10-12T00:00:00");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let opts = |remove: bool| ExportOptions::from_json(&json!({"format": "original", "removeLocation": remove}));
    let iinf = original.windows(4).position(|w| w == b"iinf").unwrap();
    let infe = original.windows(4).rposition(|w| w == b"infe").unwrap();
    let idat = original.windows(4).position(|w| w == b"idat").unwrap();
    assert!(lcc_meta::embeds_location(original), "intact GPS container");
    for damage in ["truncated type", "missing catalog", "unsupported table", "unsupported entry", "unknown entry", "truncated catalog"] {
        let mut bytes = original.to_vec();
        match damage {
            "truncated type" => bytes[infe - 4..infe].copy_from_slice(&18u32.to_be_bytes()),
            "missing catalog" => bytes[iinf..iinf + 4].copy_from_slice(b"free"),
            "unsupported table" => bytes[iinf + 4] = 2,
            "unsupported entry" => bytes[infe + 4] = 4,
            "unknown entry" => bytes[infe..infe + 4].copy_from_slice(b"free"),
            "truncated catalog" => bytes[iinf - 4..iinf].copy_from_slice(&u32::MAX.to_be_bytes()),
            _ => panic!("unknown case"),
        }
        assert_eq!(&bytes[idat..], &original[idat..], "GPS bytes retained");
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(export_photo(&mut s, id, &opts(false), 1).unwrap().bytes, bytes, "location may stay: {damage}");
        let result = export_photo(&mut s, id, &opts(true), 1);
        assert!(result.is_err(), "Remove location copied the original with {damage}");
        let err = result.err().unwrap();
        assert!(err.contains("can't remove the location") && err.contains("export as JPEG or TIFF"), "{damage}: {err}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #236: Add to Apple Photos after an export. A fake runner stands in for osascript: tests
/// never start it, nor Photos.
#[cfg(not(target_arch = "wasm32"))]
mod apple_photos {
    use std::sync::{Arc, Mutex};

    use serde_json::{Value, json};

    use super::disk_batch;
    use crate::Session;
    use crate::apple_photos::{Imports, MARKER, Output, Runner};
    use crate::export::{AfterExport, Destination, ExportOptions};
    use crate::tests_xmp::temp_dir;

    type Calls = Arc<Mutex<Vec<Vec<String>>>>;

    /// Records the arguments; Photos "imports" the first `keep` files (all: `usize::MAX`).
    fn fake(keep: usize) -> (Runner, Calls) {
        let calls: Calls = Arc::default();
        let c = calls.clone();
        let r: Runner = Arc::new(move |args: &[String], _| {
            c.lock().unwrap().push(args.to_vec());
            let files = args.len() - args.iter().position(|a| a == MARKER).unwrap() - 2;
            let ids: Vec<String> = (0..files.min(keep)).map(|i| format!("ID-{i}/L0/001")).collect();
            Ok(Output { status: Some(0), stdout: ids.join("\n") + "\n", stderr: String::new() })
        });
        (r, calls)
    }

    /// The paths after the album: the files Photos was asked to import.
    fn imported_paths(args: &[String]) -> Vec<String> {
        args[args.iter().position(|a| a == MARKER).unwrap() + 2..].to_vec()
    }

    /// Two photos exported to a Temp folder of their own, kept until the guard (first) is dropped.
    fn export_two(s: &mut Session, tag: &str) -> (lcc_testkit::TempDir, Vec<Value>) {
        let dir = temp_dir(tag);
        let ids: Vec<_> = s.visible().iter().copied().take(2).collect();
        let o = ExportOptions::from_json(&json!({"longEdge": 48, "naming": "{name}-{seq}"}));
        let to = Destination { dir: dir.to_string_lossy().into(), exact: None };
        let mut files = Vec::new();
        for id in ids {
            files.extend(disk_batch(s, id, &o, &to).unwrap());
        }
        (dir, files)
    }

    #[test]
    fn exported_files_go_to_photos_in_order() {
        let mut s = Session::with_demo();
        let (_out, files) = export_two(&mut s, "photos-flow");
        let written: Vec<String> = files.iter().map(|f| f["path"].as_str().unwrap().to_string()).collect();
        assert_eq!(written.len(), 2);
        let (runner, calls) = fake(usize::MAX);
        let imports = Imports::default();
        let after = AfterExport::apple_photos(runner.clone(), "  Trip \"2026\"\n ", &imports).unwrap();
        // a skipped file and a failed photo are not sent
        let mut results = files.clone();
        results.insert(1, json!({"skipped": "/elsewhere/x.jpg"}));
        results.push(json!({"photo": 9, "file": "y", "error": "no source"}));
        let out = after.run(&results, false).unwrap();
        assert_eq!(out["imported"], 2, "{out}");
        assert_eq!(out["requested"], 2);
        assert_eq!(out["ids"], json!(["ID-0/L0/001", "ID-1/L0/001"]));
        assert_eq!(out["album"], "Trip \"2026\"");
        assert!(out.get("warning").is_none());
        let args = calls.lock().unwrap()[0].clone();
        assert_eq!(imported_paths(&args), written, "the files written, in order, as whole arguments");
        assert!(written.iter().all(|p| std::path::Path::new(p).is_absolute()));
        assert_eq!(args[args.iter().position(|a| a == MARKER).unwrap() + 1], "Trip \"2026\"");
        // a cancelled export adds nothing
        let out = AfterExport::apple_photos(runner.clone(), "", &imports).unwrap().run(&files, true).unwrap();
        assert!(out["skipped"].as_str().unwrap().contains("cancelled"));
        assert_eq!(calls.lock().unwrap().len(), 1);
        // Photos took one of the two: said so, not an error
        let (runner, _) = fake(1);
        let out = AfterExport::apple_photos(runner, "", &Imports::default()).unwrap().run(&files, false).unwrap();
        assert_eq!((out["imported"].as_u64(), out["requested"].as_u64()), (Some(1), Some(2)));
        assert!(out["warning"].as_str().unwrap().contains("1 of 2"), "{out}");
        assert!(out["album"].is_null());
        // permission refused: the outcome says where to allow it; the files stay written
        let denied: Runner = Arc::new(|_: &[String], _| {
            Ok(Output {
                status: Some(1),
                stdout: String::new(),
                stderr: "execution error: Not authorized to send Apple events to Photos. (-1743)".into(),
            })
        });
        let out = AfterExport::apple_photos(denied, "", &Imports::default()).unwrap().run(&files, false).unwrap();
        assert!(out["error"].as_str().unwrap().contains("Privacy & Security › Automation"), "{out}");
        assert!(written.iter().all(|p| std::path::Path::new(p).is_file()));
        // nothing asked: nothing done
        assert!(AfterExport::default().is_none());
        assert_eq!(AfterExport::default().run(&files, false), None);
    }

    #[test]
    fn photos_is_refused_before_anything_is_written_when_it_cannot_be_reached() {
        {
            let mut s = Session::with_demo();
            assert!(s.apple_photos.is_none(), "only with_fs installs the runner");
            let on = ExportOptions { add_to_photos: true, ..Default::default() };
            let e = s.after_export(&on).err().unwrap();
            if cfg!(target_os = "macos") {
                assert!(e.contains("isn't connected in this session"), "{e}");
            } else {
                assert_eq!(e, "Apple Photos is only available on macOS");
            }
            assert!(s.after_export(&ExportOptions::default()).unwrap().is_none());
            // with a runner: macOS adds, other platforms still refuse
            let (runner, calls) = fake(usize::MAX);
            s.apple_photos = Some(runner);
            let after = s.after_export(&on);
            assert_eq!(after.is_ok(), cfg!(target_os = "macos"), "{:?}", after.as_ref().err());
            match after {
                Ok(after) => {
                    let (_out, files) = export_two(&mut s, "photos-session");
                    assert_eq!(after.run(&files, false).unwrap()["imported"], 2);
                    assert_eq!(calls.lock().unwrap().len(), 1);
                }
                Err(e) => assert!(e.contains("only available on macOS"), "{e}"),
            }
        }
        // The desktop app, lcc-cli and the MCP server get the real runner from `with_fs`, on
        // macOS only. (Installed, never run here.)
        {
            let s = Session::new().with_fs();
            assert_eq!(s.apple_photos.is_some(), cfg!(target_os = "macos"));
            assert_eq!(s.commands().into_iter().find(|c| c.id == "export.addToPhotos").unwrap().enabled, cfg!(target_os = "macos"));
        }
    }

    #[test]
    fn the_add_to_photos_command_needs_a_runner_and_checks_its_input() {
        let mut s = Session::with_demo();
        let (_out, files) = export_two(&mut s, "photos-cmd");
        let paths: Vec<String> = files.iter().map(|f| f["path"].as_str().unwrap().to_string()).collect();
        let info = |s: &Session| s.commands().into_iter().find(|c| c.id == "export.addToPhotos").unwrap();
        // no runner: listed, disabled, with the reason
        let i = info(&s);
        assert!(!i.enabled);
        let why = i.disabled_reason.unwrap();
        assert!(why.contains(if cfg!(target_os = "macos") { "isn't connected" } else { "only available on macOS" }), "{why}");
        let e = s.execute("export.addToPhotos", &json!({"paths": paths})).unwrap_err().to_string();
        assert!(e.contains("Apple Photos"), "{e}");
        let (runner, calls) = fake(usize::MAX);
        s.apple_photos = Some(runner);
        assert!(info(&s).enabled);
        // no window: it waits for Photos by default
        let r = s.execute("export.addToPhotos", &json!({"paths": paths, "album": "Läufe/日本 \\ \"q\""})).unwrap();
        assert_eq!((r["imported"].as_u64(), r["running"].as_bool(), r["job"].as_u64()), (Some(2), Some(false), Some(1)), "{r}");
        assert_eq!(r["album"], "Läufe/日本 \\ \"q\"");
        assert_eq!(imported_paths(&calls.lock().unwrap()[0]), paths);
        // bad input never reaches the runner (paths from the temp folder: absolute on every platform)
        let missing = std::env::temp_dir().join("lc-photos-nonexistent").join("a.jpg").to_string_lossy().into_owned();
        for (p, msg) in [
            (json!({}), "missing `paths`"),
            (json!({"paths": "a.jpg"}), "missing `paths`"),
            (json!({"paths": [1]}), "array of file paths"),
            (json!({"paths": []}), "no files"),
            (json!({"paths": ["relative.jpg"]}), "absolute"),
            (json!({"paths": [missing]}), "no such file"),
            (json!({"paths": paths, "album": 3}), "`album` must be a string"),
            (json!({"paths": paths, "wait": "yes"}), "`wait` must be true or false"),
            (json!({"paths": paths, "albm": "x"}), "unknown parameter `albm`"),
        ] {
            let e = s.execute("export.addToPhotos", &p).unwrap_err().to_string();
            assert!(e.contains(msg), "{p}: {e}");
        }
        assert_eq!(calls.lock().unwrap().len(), 1);
        // not journaled: replaying actions must not import again
        assert!(!s.journal.iter().any(|(id, _)| id == "export.addToPhotos"));
    }

    /// The desktop app answers control requests on its UI thread: `export.addToPhotos` must not
    /// wait there for Photos (minutes, or a permission prompt). It returns the running job; the
    /// outcome comes from `export.photosImports`; a second import, or an export asking for Photos,
    /// is refused until it ends, so a retry never adds the files twice.
    #[test]
    fn a_slow_photos_import_runs_in_the_background() {
        let mut s = Session::with_demo();
        let (_out, files) = export_two(&mut s, "photos-slow");
        let paths: Vec<String> = files.iter().map(|f| f["path"].as_str().unwrap().to_string()).collect();
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let wait = Mutex::new(wait);
        let runner: Runner = Arc::new(move |args: &[String], _| {
            // a regression (waiting on the caller's thread) fails after 20 s instead of hanging
            let _ = wait.lock().unwrap().recv_timeout(std::time::Duration::from_secs(20));
            let n = args.len() - args.iter().position(|a| a == MARKER).unwrap() - 2;
            Ok(Output { status: Some(0), stdout: (0..n).map(|i| format!("ID-{i}\n")).collect(), stderr: String::new() })
        });
        s.apple_photos = Some(runner);
        s.apple_photos_background = true; // as in the desktop app
        let t0 = std::time::Instant::now();
        let r = s.execute("export.addToPhotos", &json!({"paths": paths, "album": "Slow"})).unwrap();
        assert!(t0.elapsed() < std::time::Duration::from_secs(5), "answered without waiting for Photos");
        assert_eq!(r, json!({"job": 1, "running": true, "requested": 2, "album": "Slow"}));
        let status = |s: &mut Session| s.execute("export.photosImports", &json!({"job": 1})).unwrap();
        assert_eq!(status(&mut s)["running"], true);
        // a retry is refused, and says why
        let e = s.execute("export.addToPhotos", &json!({"paths": paths})).unwrap_err().to_string();
        assert!(e.contains("still adding 2 file(s) from an earlier request (import 1)"), "{e}");
        if cfg!(target_os = "macos") {
            let on = ExportOptions { add_to_photos: true, ..Default::default() };
            assert!(s.after_export(&on).err().unwrap().contains("still adding"), "refused before writing anything");
        }
        go.send(()).unwrap();
        while status(&mut s)["running"] == true {
            assert!(t0.elapsed() < std::time::Duration::from_secs(20), "the import finishes");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let done = status(&mut s);
        assert_eq!((done["imported"].as_u64(), done["album"].as_str()), (Some(2), Some("Slow")), "{done}");
        let all = s.execute("export.photosImports", &json!({})).unwrap();
        assert_eq!(all["imports"].as_array().map(Vec::len), Some(1));
        assert!(s.execute("export.photosImports", &json!({"job": 7})).unwrap_err().to_string().contains("no import 7"));
        // the window's thread never waits for Photos: `wait: true` is refused here, before any import
        let e = s.execute("export.addToPhotos", &json!({"paths": paths, "wait": true})).unwrap_err().to_string();
        assert!(e.contains("isn't available in the desktop app"), "{e}");
        // finished: the next one may start
        go.send(()).unwrap();
        assert_eq!(s.execute("export.addToPhotos", &json!({"paths": paths})).unwrap()["job"], 2);
    }

    /// Review of #236: an export adding to Photos reserves the import before it writes its files.
    /// While it renders, `export.addToPhotos` is refused, naming the export's job, instead of
    /// taking Photos and leaving the export to fail its Photos step after writing.
    #[test]
    fn an_export_holds_photos_from_before_it_writes() {
        {
            let mut s = Session::with_demo();
            let (runner, calls) = fake(usize::MAX);
            s.apple_photos = Some(runner.clone());
            let (_elsewhere_dir, elsewhere) = export_two(&mut s, "photos-hold-other");
            let other: Vec<String> = elsewhere.iter().map(|f| f["path"].as_str().unwrap().to_string()).collect();
            // what Session::after_export does on macOS (it refuses other platforms first)
            let after = AfterExport::apple_photos(runner, "Held", &s.apple_photos_imports).unwrap();
            let job = s.execute("export.photosImports", &json!({"job": 1})).unwrap();
            assert_eq!((job["running"].as_bool(), job["exporting"].as_bool()), (Some(true), Some(true)), "{job}");
            let e = s.execute("export.addToPhotos", &json!({"paths": other})).unwrap_err().to_string();
            assert!(e.contains("An export is about to add its files to Apple Photos (import 1)"), "{e}");
            if cfg!(target_os = "macos") {
                let on = ExportOptions { add_to_photos: true, ..Default::default() };
                assert!(s.after_export(&on).err().unwrap().contains("(import 1)"), "a second export is refused before writing");
            }
            assert!(calls.lock().unwrap().is_empty());
            // the export writes its files and hands them over: imported under its own job
            let (_out, files) = export_two(&mut s, "photos-hold");
            let out = after.run(&files, false).unwrap();
            assert_eq!((out["job"].as_u64(), out["imported"].as_u64(), out["album"].as_str()), (Some(1), Some(2), Some("Held")), "{out}");
            // then the slot is free
            assert_eq!(s.execute("export.addToPhotos", &json!({"paths": other})).unwrap()["job"], 2);
            // an export that fails (its AfterExport dropped) frees it too
            drop(AfterExport::apple_photos(fake(usize::MAX).0, "", &s.apple_photos_imports).unwrap());
            assert!(s.apple_photos_imports.running().is_none());
            assert_eq!(s.execute("export.addToPhotos", &json!({"paths": other})).unwrap()["job"], 4);
        }
        // Review of #236: a runner that panics while the command or an export waits for it is that
        // import's error; the slot is freed, so the next import isn't refused as "still running"
        // and a process ending doesn't wait for it.
        {
            let mut s = Session::with_demo();
            let (_out, files) = export_two(&mut s, "photos-panic");
            let paths: Vec<String> = files.iter().map(|f| f["path"].as_str().unwrap().to_string()).collect();
            s.apple_photos = Some(Arc::new(|_: &[String], _| panic!("runner exploded")));
            // the command (no window: it waits)
            let e = s.execute("export.addToPhotos", &json!({"paths": paths})).unwrap_err().to_string();
            assert!(e.contains("stopped unexpectedly"), "{e}");
            assert!(s.apple_photos_imports.running().is_none());
            assert_eq!(s.execute("export.photosImports", &json!({"job": 1})).unwrap()["running"], false);
            // an export's reservation
            let runner = s.apple_photos.clone().unwrap();
            let out = AfterExport::apple_photos(runner, "", &s.apple_photos_imports).unwrap().run(&files, false).unwrap();
            assert!(out["error"].as_str().unwrap().contains("stopped unexpectedly"), "{out}");
            assert!(s.apple_photos_imports.running().is_none());
            assert!(!s.wait_for_photos_imports(), "nothing left to wait for");
            // and the next import runs
            let (good, _) = fake(usize::MAX);
            s.apple_photos = Some(good);
            assert_eq!(s.execute("export.addToPhotos", &json!({"paths": paths})).unwrap()["imported"], 2);
        }
    }
}

/// Issue #956: "Same folder as original photo" puts each photo's export next to its own original
/// (plus the subfolder), so one batch from two folders lands in two folders. The conflict policy
/// and the original guard work per folder as they do for a specific folder.
#[test]
fn same_folder_as_original_exports_next_to_each_photo() {
    {
        use crate::export::{Conflict, Destination, DestinationMode};
        let dir = temp_dir("export-same-folder");
        let (a, b) = (dir.join("A"), dir.join("B"));
        // two different photos (the same bytes would be imported once)
        let jpeg = |blue: u8| {
            let img = lcc_raster::Rgba8::from_fn(48, 32, |x, y| [(x * 5) as u8, (y * 7) as u8, blue, 255]);
            crate::export::encode_image(&img, &ExportOptions::default()).unwrap()
        };
        let originals = [(&a, jpeg(120)), (&b, jpeg(30))];
        for (d, bytes) in &originals {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("IMG_1.jpg"), bytes).unwrap();
        }
        let mut s = Session::new().with_fs();
        let paths = [a.join("IMG_1.jpg").to_string_lossy().to_string(), b.join("IMG_1.jpg").to_string_lossy().to_string()];
        s.execute("library.import", &json!({"paths": paths})).unwrap();
        let ids: Vec<_> = s.catalog.photos().map(|p| p.id).collect();
        assert_eq!(ids.len(), 2);
        // the folder of a specific-folder export is not used
        let to = Destination { dir: dir.join("unused").to_string_lossy().to_string(), exact: None };
        let o = ExportOptions { destination_mode: DestinationMode::SameAsOriginal, subfolder: "Exports".into(), ..Default::default() };
        let exists = |p: &str| std::path::Path::new(p).exists();
        let files = crate::export::export_batch(&mut s, &ids, &o, &to, &mut crate::export::write_file, &exists).unwrap();
        let written: Vec<&str> = files.iter().map(|f| f["path"].as_str().unwrap()).collect();
        // compared as paths: on Windows `A\\Exports/IMG_1.jpg` and `A/Exports/IMG_1.jpg` are one file
        let mut want = vec![a.join("Exports").join("IMG_1.jpg"), b.join("Exports").join("IMG_1.jpg")];
        let mut got: Vec<std::path::PathBuf> = written.iter().map(std::path::PathBuf::from).collect();
        want.sort();
        got.sort();
        assert_eq!(got, want);
        assert!(!dir.join("unused").exists(), "nothing written to the specific folder");
        // again: each folder's own collision gets the next free name
        let files = crate::export::export_batch(&mut s, &ids, &o, &to, &mut crate::export::write_file, &exists).unwrap();
        assert!(files.iter().all(|f| f["path"].as_str().unwrap().ends_with("Exports/IMG_1-2.jpg")), "{files:?}");
        // no subfolder: next to the original, which is never written over
        let beside = ExportOptions { subfolder: String::new(), conflict: Conflict::Overwrite, ..o.clone() };
        let err = crate::export::export_batch(&mut s, &ids, &beside, &to, &mut crate::export::write_file, &exists).unwrap_err();
        assert!(err.contains("original of IMG_1.jpg"), "{err}");
        let unique = ExportOptions { conflict: Conflict::Unique, ..beside };
        let files = crate::export::export_batch(&mut s, &ids, &unique, &to, &mut crate::export::write_file, &exists).unwrap();
        assert!(files.iter().all(|f| f["path"].as_str().unwrap().ends_with("IMG_1-2.jpg")), "{files:?}");
        for (d, bytes) in &originals {
            assert_eq!(&std::fs::read(d.join("IMG_1.jpg")).unwrap(), bytes, "the original is untouched");
            assert!(d.join("IMG_1-2.jpg").exists());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    // Issue #956: a photo with no folder on disk (a demo photo) or whose folder is gone gets its own
    // error in "Same folder as original photo" mode instead of a file written somewhere else.
    {
        use crate::export::{Destination, DestinationMode};
        let small = ExportOptions::from_json(&json!({"longEdge": 32}));
        let o = ExportOptions { destination_mode: DestinationMode::SameAsOriginal, subfolder: "Exports".into(), ..small };
        let to = Destination { dir: "out".into(), exact: None };
        let mut s = Session::with_demo();
        let ids: Vec<_> = s.catalog.photos().take(2).map(|p| p.id).collect();
        let mut written = Vec::new();
        // the writer lives in a block of its own, so its borrow of `written` has ended by the time
        // the assertion below reads it
        let files = {
            let mut write = |p: &str, _: &[u8]| -> Result<(), String> {
                written.push(p.to_string());
                Ok(())
            };
            let err = crate::export::export_batch(&mut s, &ids, &o, &to, &mut write, &|_| false).unwrap_err();
            assert!(err.contains("no folder on disk"), "{err}");
            // without stopping: one error per photo, nothing written
            let items = crate::export::prepare_batch(&mut s, &ids, &o).unwrap();
            crate::export::run_batch(items, &o, &to, &mut write, &|_| false, false, &mut |_, _| true).unwrap()
        };
        assert_eq!(files.len(), 2);
        for (f, id) in files.iter().zip(&ids) {
            assert_eq!(f["photo"], id.0, "{f}");
            assert!(f["error"].as_str().unwrap().contains("no folder on disk"), "{f}");
        }
        // (`write` borrows `written` until its last use above; a closure has no `Drop` to wait for)
        assert!(written.is_empty(), "{written:?}");

        // a photo on disk whose folder is gone (moved, an unplugged drive)
        let (mut s, id, dir, _src, _) = library_with_jpeg("export-same-folder-gone");
        let items = crate::export::prepare_batch(&mut s, &[id], &o).unwrap();
        let gone = dir.to_string_lossy().to_string();
        let exists = |p: &str| p != gone && !p.starts_with(&format!("{gone}/"));
        let mut write = |p: &str, _: &[u8]| -> Result<(), String> { Err(format!("wrote {p}")) };
        let files = crate::export::run_batch(items, &o, &to, &mut write, &exists, false, &mut |_, _| true).unwrap();
        let e = files[0]["error"].as_str().unwrap();
        assert!(e.contains("folder is missing") && e.contains(&gone), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Issue #80: "Custom Name - Sequence" numbers the batch in selection order from the start number,
/// with no gaps (a skipped or failed photo doesn't renumber the rest), and a name already taken
/// in the folder gets the conflict policy's free name rather than a reused number.
#[test]
fn custom_name_sequence_follows_selection_order() {
    use crate::export::Destination;
    let mut s = Session::with_demo();
    let mut ids: Vec<_> = s.catalog.photos().take(3).map(|p| p.id).collect();
    ids.reverse(); // selection order, not library order
    let o = ExportOptions::from_params(&json!({"naming": "{custom} ({seq:1})", "customText": "Trip/Day", "startNumber": 7, "longEdge": 32})).unwrap();
    let items = crate::export::prepare_batch(&mut s, &ids, &o).unwrap();
    let names: Vec<_> = items.iter().map(|i| i.file_name.clone()).collect();
    assert_eq!(ExportOptions::from_params(&ExportOptions::known_keys_only(&o.to_json())).unwrap(), o, "kept in presets");
    assert_eq!(names, ["Trip_Day (7).jpg", "Trip_Day (8).jpg", "Trip_Day (9).jpg"], "separators in the custom text never make a path");
    let to = Destination { dir: "out".into(), exact: None };
    let mut written = Vec::new();
    let mut write = |p: &str, _: &[u8]| -> Result<(), String> {
        written.push(p.to_string());
        Ok(())
    };
    let taken = |p: &str| p == "out/Trip_Day (8).jpg";
    let files = crate::export::run_batch(items, &o, &to, &mut write, &taken, true, &mut |_, _| true).unwrap();
    let paths: Vec<_> = files.iter().map(|f| f["path"].as_str().unwrap().to_string()).collect();
    assert_eq!(paths, ["out/Trip_Day (7).jpg", "out/Trip_Day (8)-2.jpg", "out/Trip_Day (9).jpg"]);
}

/// Issue #80: a subfolder that would leave the destination folder (`..`, an absolute path, a
/// separator) is refused by the command, and an option set built in code with one writes nothing.
#[test]
fn hostile_subfolders_are_refused() {
    use crate::export::{Destination, DestinationMode};
    for bad in ["..", ".", "../up", "/etc", "a/b", "a\\b", "C:x", "x\u{0}y", "CON", "nul.txt", "COM1", "a*b", "what?", "a<b>", "q\"x", "p|q", "dots."]
    {
        let e = ExportOptions::from_params(&json!({"subfolder": bad})).unwrap_err().to_string();
        assert!(e.contains("subfolder"), "{bad}: {e}");
        assert!(ExportOptions::from_json(&json!({"subfolder": bad})).subfolder.chars().all(|c| !matches!(c, '/' | '\\')), "{bad}");
    }
    assert_eq!(ExportOptions::from_params(&json!({"subfolder": " Edited "})).unwrap().subfolder, "Edited");
    // a name that merely looks like a device is fine
    for ok in ["Console", "COM10", "Trip 2026"] {
        assert_eq!(ExportOptions::from_params(&json!({"subfolder": ok})).unwrap().subfolder, ok);
    }
    let mut s = Session::with_demo();
    let ids: Vec<_> = s.catalog.photos().take(1).map(|p| p.id).collect();
    for mode in [DestinationMode::SpecificFolder, DestinationMode::SameAsOriginal] {
        let small = ExportOptions::from_json(&json!({"longEdge": 32}));
        let o = ExportOptions { destination_mode: mode, subfolder: "../../x".into(), ..small };
        let items = crate::export::prepare_batch(&mut s, &ids, &o).unwrap();
        let mut write = |p: &str, _: &[u8]| -> Result<(), String> { Err(format!("wrote {p}")) };
        let to = Destination { dir: "out".into(), exact: None };
        let files = crate::export::run_batch(items, &o, &to, &mut write, &|_| true, false, &mut |_, _| true).unwrap();
        assert!(files[0]["error"].as_str().unwrap().contains("subfolder"), "{files:?}");
    }
}

/// "Overwrite" replaces a file from an earlier run, but two photos of one batch that render to
/// the same name no longer replace each other: the second gets the next free name.
#[test]
fn overwrite_never_clobbers_a_file_of_the_same_batch() {
    use crate::export::{Conflict, Destination};
    let (mut s, _, dir, _src, _) = library_with_jpeg("export-collide");
    let other = dir.join("b").join("IMG_1.jpg");
    std::fs::create_dir_all(other.parent().unwrap()).unwrap();
    let img = lcc_raster::Rgba8::from_fn(48, 32, |x, _| [200, (x * 3) as u8, 10, 255]);
    std::fs::write(&other, crate::export::encode_image(&img, &ExportOptions::default()).unwrap()).unwrap();
    s.execute("library.import", &json!({"paths": [other.to_string_lossy()]})).unwrap();
    let ids: Vec<_> = s.catalog.photos().map(|p| p.id).collect();
    assert_eq!(ids.len(), 2);
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("IMG_1.jpg"), b"an earlier run").unwrap();
    let to = Destination { dir: out.to_string_lossy().to_string(), exact: None };
    let o = ExportOptions { naming: "{name}".into(), conflict: Conflict::Overwrite, ..Default::default() };
    let files = crate::export::export_batch(&mut s, &ids, &o, &to, &mut crate::export::write_file, &|p| std::path::Path::new(p).exists()).unwrap();
    let paths: Vec<_> = files.iter().map(|f| f["path"].as_str().unwrap_or_default().to_string()).collect();
    assert!(paths[0].ends_with("IMG_1.jpg"), "{files:?}");
    assert!(paths[1].ends_with("IMG_1-2.jpg"), "{files:?}");
    assert_ne!(std::fs::read(out.join("IMG_1.jpg")).unwrap(), b"an earlier run", "the earlier run's file is replaced");
    assert!(out.join("IMG_1-2.jpg").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A library with one 300 × 200 PNG: transparent, with an opaque red 150 × 100 rectangle in the middle.
fn library_with_transparent_png(tag: &str) -> (Session, lcc_catalog::PhotoId, lcc_testkit::TempDir) {
    let dir = temp_dir(tag);
    let src = dir.join("logo.png");
    let img =
        lcc_raster::Rgba8::from_fn(300, 200, |x, y| if (75..225).contains(&x) && (50..150).contains(&y) { [200, 40, 30, 255] } else { [0, 0, 0, 0] });
    let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &Default::default()).unwrap();
    std::fs::write(&src, png).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    (s, id, dir)
}

/// Issue #2976: PNG, TIFF and WebP exports of a transparent source keep its alpha, at 8 and 16 bits,
/// with the colour of the opaque part as it was (no white mixed in); TIFF stores it associated.
#[test]
fn transparent_sources_keep_their_alpha_in_png_tiff_and_webp() {
    {
        let (mut s, id, dir) = library_with_transparent_png("export-alpha");
        for (format, depth) in [("png", 8), ("png", 16), ("tiff", 8), ("tiff", 16), ("tiff", 32), ("webp", 8)] {
            let o = ExportOptions::from_json(&json!({"format": format, "bitDepth": depth}));
            let e = export_photo(&mut s, id, &o, 1).unwrap();
            let d = lcc_codecs::decode(&e.bytes, Default::default()).unwrap();
            assert!(d.has_alpha, "{format} {depth}-bit keeps alpha");
            assert_eq!((d.width, d.height), (300, 200), "{format} {depth}-bit");
            let a = d.alpha.as_ref().unwrap();
            for (x, y) in [(2, 2), (297, 197), (74, 100), (150, 49)] {
                assert!(a.get(x, y) < 1e-3, "{format} {depth}-bit: transparent at ({x}, {y})");
            }
            for (x, y) in [(75, 100), (150, 100), (224, 149)] {
                assert!(a.get(x, y) > 0.999, "{format} {depth}-bit: opaque at ({x}, {y})");
            }
            let edge = d.to_srgb8().get(75, 100);
            assert!(edge[0].abs_diff(200) <= 2 && edge[1].abs_diff(40) <= 2 && edge[2].abs_diff(30) <= 2, "{format} {depth}-bit edge {edge:?}");
            if format == "tiff" {
                // ExtraSamples (338), SHORT, count 1, value 1: associated alpha
                assert!(e.bytes.windows(10).any(|w| w == [0x52, 0x01, 3, 0, 1, 0, 0, 0, 1, 0]), "{depth}-bit TIFF alpha is associated");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    // JPEG (and the other formats without alpha in this export) composites a transparent source onto
    // white as before; an opaque PNG source still exports as RGB (#1670).
    {
        let (mut s, id, dir) = library_with_transparent_png("export-alpha-white");
        // (AVIF is left out: it composites onto white too, but this build can't decode one to check)
        for format in ["jpeg", "psd", "jxl"] {
            let e = export_photo(&mut s, id, &ExportOptions::from_json(&json!({"format": format})), 1).unwrap();
            let d = lcc_codecs::decode(&e.bytes, Default::default()).unwrap();
            assert!(!d.has_alpha, "{format}");
            let corner = d.to_srgb8().get(2, 2);
            assert!(corner.iter().take(3).all(|&c| c >= 250), "{format} corner on white: {corner:?}");
        }
        let src = dir.join("opaque.png");
        let img = lcc_raster::Rgba8::from_fn(64, 48, |x, y| [(x * 3) as u8, (y * 5) as u8, 90, 255]);
        std::fs::write(&src, lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &Default::default()).unwrap()).unwrap();
        s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
        let opaque = s.catalog.photos().map(|p| p.id).find(|&p| p != id).unwrap();
        let e = export_photo(&mut s, opaque, &ExportOptions::from_json(&json!({"format": "png"})), 1).unwrap();
        assert_eq!(e.bytes[25], 2, "PNG IHDR declares RGB");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The alpha follows the pixels through rotation, crop and resize.
#[test]
fn exported_alpha_follows_rotate_crop_and_resize() {
    let (mut s, id, dir) = library_with_transparent_png("export-alpha-geometry");
    // turned right: 200 × 300 with the rectangle at x 50..150, y 75..225; its left half: 100 × 300;
    // fitted to 150 px: 50 × 150, the rectangle at x 25..50, y 37.5..112.5
    s.execute("photo.rotateRight", &json!({})).unwrap();
    s.execute("crop.set", &json!({"rect": [0.0, 0.0, 0.5, 1.0]})).unwrap();
    let e = export_photo(&mut s, id, &ExportOptions::from_json(&json!({"format": "png", "longEdge": 150})), 1).unwrap();
    let d = lcc_codecs::decode(&e.bytes, Default::default()).unwrap();
    assert_eq!((d.width, d.height), (50, 150));
    let px = d.to_srgb8();
    for (x, y) in [(30, 75), (45, 40), (45, 110)] {
        let p = px.get(x, y);
        assert_eq!(p[3], 255, "opaque at ({x}, {y})");
        assert!(p[0] > 150 && p[1] < 80, "red at ({x}, {y}): {p:?}");
    }
    for (x, y) in [(10, 75), (30, 20), (30, 130)] {
        assert_eq!(px.get(x, y)[3], 0, "transparent at ({x}, {y})");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #2976: a source without transparency (no alpha channel, or one opaque everywhere) exports
/// exactly as before, byte for byte, through the PNG/TIFF/WebP path too: the grey a Transform brings
/// in around the photo stays grey and no alpha is written.
#[test]
fn opaque_sources_export_exactly_as_before() {
    let dir = temp_dir("export-alpha-opaque");
    let mut s = Session::new().with_fs();
    let img = lcc_raster::Rgba8::from_fn(96, 64, |x, y| [(x * 2) as u8, (y * 3) as u8, 90, 255]);
    let rgba = dir.join("rgba.png");
    std::fs::write(&rgba, lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &Default::default()).unwrap()).unwrap();
    let rgb = dir.join("rgb.png");
    let flat: Vec<u8> = img.data.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let rgb_png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::new(96, 64, 3, lcc_codecs::Samples::U8(&flat)), &Default::default());
    std::fs::write(&rgb, rgb_png.unwrap()).unwrap();
    // (unconstrained, so the keystone brings in the grey outside the photo)
    s.execute("library.preferences", &json!({"import": {"constrainCrop": false}})).unwrap();
    s.execute("library.import", &json!({"paths": [rgba.to_string_lossy(), rgb.to_string_lossy()]})).unwrap();
    let ids: Vec<_> = s.catalog.photos().map(|p| p.id).collect();
    assert_eq!(ids.len(), 2);
    for id in ids {
        s.execute("develop.set", &json!({"ids": [id.0], "control": "geometry.vertical", "value": -60})).unwrap();
        for (format, depth) in [("png", 8), ("png", 16), ("tiff", 8), ("webp", 8)] {
            let o = ExportOptions::from_json(&json!({"format": format, "bitDepth": depth}));
            let e = export_photo(&mut s, id, &o, 1).unwrap();
            // the same render without the transparency path, as before #2976
            let (w, h) = crate::export::output_size(s.catalog.photo(id).unwrap(), &o);
            let meta = crate::export::export_metadata(s.catalog.photo(id).unwrap(), &s.catalog, &o);
            let job = s.export_job(id, w, h, o.effective_space(), o.effective_depth()).unwrap();
            let before = crate::export::encode_rendered(&job.run().rendered.unwrap(), &o, meta.as_ref()).unwrap();
            assert!(e.bytes == before, "{format} {depth}-bit: identical to the render without transparency");
            let d = lcc_codecs::decode(&e.bytes, Default::default()).unwrap();
            assert!(!d.has_alpha, "{format} {depth}-bit: no alpha");
            if (format, depth) == ("png", 8) {
                // the keystone brings in the light grey outside the photo at the bottom corners
                let corner = d.to_srgb8().get(1, d.height as usize - 2);
                assert!(corner[0] == corner[1] && corner[1] == corner[2] && corner[0] > 150, "grey corner: {corner:?}");
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #2976, end to end: a logo with soft, semi-transparent edges keeps them through a crop, a
/// 10° straighten and an exposure change, with the colour of its opaque part (no white fringe) and
/// transparent corners, in PNG, TIFF and WebP.
#[test]
fn soft_edges_keep_their_colour_through_crop_straighten_and_exposure() {
    let dir = temp_dir("export-alpha-soft");
    let src = dir.join("soft.png");
    // a blue disc of radius 90 with a 24 px soft edge
    let img = lcc_raster::Rgba8::from_fn(400, 300, |x, y| {
        let d = ((x as f32 + 0.5 - 200.0).powi(2) + (y as f32 + 0.5 - 150.0).powi(2)).sqrt();
        let a = ((90.0 - d) / 24.0 + 0.5).clamp(0.0, 1.0);
        if a > 0.0 { [30, 120, 200, (a * 255.0).round() as u8] } else { [0, 0, 0, 0] }
    });
    std::fs::write(&src, lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &Default::default()).unwrap()).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    s.execute("crop.set", &json!({"rect": [0.1, 0.1, 0.9, 0.9]})).unwrap();
    s.execute("crop.set", &json!({"angle": 10.0})).unwrap();
    s.execute("develop.set", &json!({"ids": [id.0], "control": "light.exposure", "value": 0.7})).unwrap();
    for format in ["png", "tiff", "webp"] {
        let e = export_photo(&mut s, id, &ExportOptions::from_json(&json!({"format": format})), 1).unwrap();
        let d = lcc_codecs::decode(&e.bytes, Default::default()).unwrap();
        assert!(d.has_alpha, "{format}");
        let px = d.to_srgb8();
        let (w, h) = (px.width, px.height);
        assert_eq!(px.get(1, 1)[3], 0, "{format}: transparent corner");
        let centre = px.get(w / 2, h / 2);
        assert_eq!(centre[3], 255, "{format}: opaque centre");
        assert!(centre[2] > 200, "{format}: the exposure brightened the disc: {centre:?}");
        // every soft-edge pixel has the colour of the opaque part
        let soft: Vec<[u8; 4]> = px.data.iter().copied().filter(|p| (24..=232).contains(&p[3])).collect();
        assert!(soft.len() > 500, "{format}: the soft edge survives ({} pixels)", soft.len());
        let tolerance = if format == "tiff" { 6 } else { 3 };
        for p in &soft {
            assert!((0..3).all(|c| p[c].abs_diff(centre[c]) <= tolerance), "{format}: soft-edge colour {p:?} vs the disc's {centre:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A gain map's HDR rendition starts from the SDR render's decoded original and stages (as
/// `PreparedExport::run` hands them over) and comes out exactly as rendered on its own, noise
/// reduction and clarity included (the GPU stages too, when the renders run there).
#[test]
fn the_hdr_rendition_from_the_sdr_renders_stages_is_the_same() {
    use crate::export::{OutputDepth, OutputSpace};
    let mut s = Session::with_demo();
    let id = sunset(&mut s);
    s.execute("develop.hdr", &json!({"enabled": true, "maxEv": 3})).unwrap();
    s.execute("develop.set", &json!({"control": "detail.nrLuminance", "value": 30})).unwrap();
    s.execute("develop.set", &json!({"control": "effects.clarity", "value": 20})).unwrap();
    let (w, h, space) = (400, 400, OutputSpace::DisplayP3);
    let fresh = s.export_job(id, w, h, space, OutputDepth::F32Hdr).unwrap().run().rendered.unwrap().deep.unwrap();
    let stages = std::sync::Arc::new(lcc_pipeline::StageCache::default());
    let mut sdr = s.export_job(id, w, h, space, OutputDepth::U8).unwrap();
    sdr.stages = Some(stages.clone());
    let mut r = sdr.run();
    assert!(r.rendered.is_ok());
    let mut hdr = s.export_job(id, w, h, space, OutputDepth::F32Hdr).unwrap();
    if let Some(src) = r.loaded.take() {
        hdr.source = crate::media::SourceRef::Loaded(Box::new(src));
    }
    hdr.stages = Some(stages);
    let shared = hdr.run().rendered.unwrap().deep.unwrap();
    assert_eq!((shared.width, shared.height), (fresh.width, fresh.height));
    let (crate::export::DeepSamples::F32(a), crate::export::DeepSamples::F32(b)) = (&shared.samples, &fresh.samples) else {
        panic!("float samples expected")
    };
    assert!(a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits()), "the shared render differs");
}

/// Settings like a real HDR edit (tone, clarity, noise reduction, a radial mask) for the export
/// turn tests.
fn hdr_export_settings(s: &mut Session, id: lcc_catalog::PhotoId) {
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.execute("develop.hdr", &json!({"enabled": true, "maxEv": 3})).unwrap();
    for (c, v) in [("light.exposure", 0.3), ("effects.clarity", 15.0), ("detail.nrLuminance", 20.0), ("light.highlights", -40.0)] {
        s.execute("develop.set", &json!({"control": c, "value": v})).unwrap();
    }
}

/// Photos side by side took turns at an idle GPU because a photo's turn covered its decode and
/// its encode too. A gain map export decodes before its renders and encodes after them, never
/// inside them.
#[test]
fn an_export_encodes_outside_its_renders() {
    use std::sync::atomic::Ordering;
    let mut s = Session::with_demo();
    let ids: Vec<_> = s.catalog.photos().map(|p| p.id).take(3).collect();
    for &id in &ids {
        hdr_export_settings(&mut s, id);
    }
    let o = ExportOptions::from_json(&json!({"longEdge": 256, "hdr": true, "quality": 90}));
    let before = crate::export::ENCODES_IN_TURN.load(Ordering::Relaxed);
    let files = crate::export::export_batch_lanes(&mut s, &ids, &o, 2, false).unwrap();
    assert_eq!(files.len(), 3, "{files:?}");
    assert!(files.iter().all(|f| f.get("error").is_none()), "{files:?}");
    assert_eq!(crate::export::ENCODES_IN_TURN.load(Ordering::Relaxed), before, "no encode inside a photo's renders");
}

/// A cancelled export stayed "cancelling" for minutes and kept exporting. It returns
/// within about a second of the cancel and writes nothing more (its lanes stop at their next step).
#[test]
fn a_cancelled_export_stops_within_a_second() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut s = Session::with_demo();
    let ids: Vec<_> = s.catalog.photos().map(|p| p.id).take(4).collect();
    for &id in &ids {
        hdr_export_settings(&mut s, id);
    }
    let batch: Vec<_> = ids.iter().copied().cycle().take(12).collect();
    let o = ExportOptions::from_json(&json!({"longEdge": 1600, "hdr": true}));
    let items = crate::export::prepare_batch(&mut s, &batch, &o).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let cancelled_at = Arc::new(std::sync::Mutex::new(None));
    let at = cancelled_at.clone();
    // cancel while the first photos are still rendering
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        *at.lock().unwrap() = Some(std::time::Instant::now());
        flag.store(true, Ordering::Relaxed);
    });
    let mut written = 0;
    let mut write = |_: &str, _: &[u8]| {
        written += 1;
        Ok(())
    };
    let to = crate::export::Destination::default();
    let c = cancel.clone();
    let files = crate::export::run_batch_cancellable(
        items,
        &o,
        &to,
        &mut write,
        &|_| false,
        false,
        &mut |_, _| !c.load(Ordering::Relaxed),
        Some(cancel.clone()),
    )
    .unwrap();
    let after = cancelled_at.lock().unwrap().expect("cancelled").elapsed();
    assert!(after < std::time::Duration::from_millis(1500), "returned {after:?} after the cancel");
    assert!(files.len() < 12 && written == files.len(), "{} files, {written} written", files.len());
}

/// The GPU and the processor at once: with one export at a time on the card and the processor
/// rendering the photos it has no room for, every photo comes out and none fails.
#[test]
fn exports_render_on_the_gpu_and_the_processor_at_once() {
    if !lcc_gpu::available() {
        lcc_testkit::skip!(Gpu, "no GPU adapter ({})", lcc_gpu::unavailable_reason().unwrap_or_default());
        return;
    }
    let mut s = Session::with_demo();
    let ids: Vec<_> = s.catalog.photos().map(|p| p.id).take(4).collect();
    for &id in &ids {
        hdr_export_settings(&mut s, id);
    }
    let o = ExportOptions::from_json(&json!({"longEdge": 512, "hdr": true, "quality": 90}));
    lcc_gpu::set_batch_limits(None, Some(1));
    let files = crate::export::export_batch_lanes(&mut s, &ids, &o, 4, true);
    lcc_gpu::set_batch_limits(None, None);
    let files = files.unwrap();
    assert_eq!(files.len(), 4);
    assert!(files.iter().all(|f| f.get("error").is_none()), "{files:?}");
}

/// Review of upstream #3116: a decode the decoder fits down keeps a straight 0..1 alpha plane for a
/// transparent file, and none for an RGBA file that is opaque everywhere (the fit brings 1.0 back
/// as 0.99999994: that must not send an opaque photo down the transparent export path).
#[test]
fn a_fitted_decode_keeps_alpha_only_for_real_transparency() {
    let png = |img: &lcc_raster::Rgba8| lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(img), &Default::default()).unwrap();
    let opaque = lcc_raster::Rgba8::from_fn(600, 400, |x, y| [(x % 256) as u8, (y % 256) as u8, 90, 255]);
    let (_, info) = crate::files::load_bytes(&png(&opaque), 300).unwrap();
    assert!(info.alpha.is_none(), "an opaque RGBA file has no transparency");
    let clear =
        lcc_raster::Rgba8::from_fn(
            600,
            400,
            |x, y| if (150..450).contains(&x) && (100..300).contains(&y) { [200, 40, 30, 255] } else { [0, 0, 0, 0] },
        );
    let (img, info) = crate::files::load_bytes(&png(&clear), 300).unwrap();
    let a = info.alpha.expect("a transparent file keeps its alpha");
    assert_eq!((a.width, a.height), (img.width, img.height));
    assert!(a.data.iter().all(|&v| (0.0..=1.0).contains(&v)), "straight alpha stays in 0..1");
}

/// A JPEG whose main XMP packet names an Extended XMP packet holding a GPS position and a caption
/// (written by apps that move long properties out of the main packet).
fn jpeg_with_extended_gps() -> Vec<u8> {
    const GUID: &str = "0123456789ABCDEF0123456789ABCDEF";
    let pixels = [96u8; 12];
    let image = lcc_codecs::EncodeImage::new(2, 2, 3, lcc_codecs::Samples::U8(&pixels));
    let jpeg = lcc_codecs::encode_jpeg(&image, 90, lcc_codecs::ChromaSubsampling::S444, &Default::default()).unwrap();
    let main = format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:xmpNote=\"http://ns.adobe.com/xmp/note/\" xmpNote:HasExtendedXMP=\"{GUID}\"/></rdf:RDF></x:xmpmeta>"
    );
    let ext = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:exif=\"http://ns.adobe.com/exif/1.0/\" exif:GPSLatitude=\"51,30.0N\" exif:GPSLongitude=\"0,7.5W\"/></rdf:RDF></x:xmpmeta>";
    let segment = |payload: Vec<u8>| {
        let mut s = vec![0xff, 0xe1];
        s.extend(((payload.len() + 2) as u16).to_be_bytes());
        s.extend(payload);
        s
    };
    let mut p = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
    p.extend(main.as_bytes());
    let mut e = b"http://ns.adobe.com/xmp/extension/\0".to_vec();
    e.extend(GUID.as_bytes());
    e.extend((ext.len() as u32).to_be_bytes());
    e.extend(0u32.to_be_bytes());
    e.extend(ext.as_bytes());
    let mut out = jpeg[..2].to_vec();
    out.extend(segment(p));
    out.extend(segment(e));
    out.extend(&jpeg[2..]);
    out
}
