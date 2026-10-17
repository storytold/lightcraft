//! Export end to end: output sizes, original + XMP, DNG.

use serde_json::json;

use crate::Session;
use crate::export::{ExportFormat, ExportOptions, export_photo};
use crate::tests_xmp::{synthetic_dng_with, temp_dir};

#[test]
fn full_size_export_of_a_cropped_photo_is_not_upscaled() {
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    let (pw, ph) = {
        let p = s.catalog.photo(id).unwrap();
        (p.width as usize, p.height as usize)
    };
    s.execute("crop.set", &json!({"rect": [0.25, 0.25, 0.75, 0.75]})).unwrap();
    let e = export_photo(&mut s, id, &ExportOptions::default(), 1).unwrap();
    assert!(e.width.abs_diff(pw / 2) <= 1 && e.height.abs_diff(ph / 2) <= 1, "{}×{} from {pw}×{ph}", e.width, e.height);
    // and the size modes apply to the cropped size
    let o = ExportOptions::from_json(&json!({"percent": 50}));
    let e = export_photo(&mut s, id, &o, 1).unwrap();
    assert!(e.width.abs_diff(pw / 4) <= 1, "{} vs {}", e.width, pw / 4);
    let o = ExportOptions::from_json(&json!({"width": 200, "height": 200, "format": "png"}));
    let e = export_photo(&mut s, id, &o, 1).unwrap();
    assert_eq!(e.width.max(e.height), 200);
    // the print resolution lands in the file
    let i = e.bytes.windows(4).position(|w| w == b"pHYs").expect("pHYs");
    assert_eq!(u32::from_be_bytes(e.bytes[i + 4..i + 8].try_into().unwrap()), 9449, "240 ppi");
}

#[test]
fn original_export_copies_the_file_and_writes_its_edits_beside_it() {
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
    let back = lightcraft_raw::decode(&e.bytes).expect("our DNG decodes");
    let orig = lightcraft_raw::decode(&dng).unwrap();
    assert_eq!((back.width, back.height), (orig.width, orig.height));
    assert_eq!(back.data, orig.data, "lossless");
    let out = dir.join("out.dng");
    std::fs::write(&out, &e.bytes).unwrap();
    let mut s2 = Session::new().with_fs();
    s2.execute("library.import", &json!({"paths": [out.to_string_lossy()]})).unwrap();
    assert_eq!(s2.catalog.photos().next().unwrap().develop.light.exposure, 0.7, "edits embedded in the DNG are read back");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn original_and_dng_need_a_file() {
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    for f in ["original", "dng"] {
        let err = export_photo(&mut s, id, &ExportOptions::from_json(&json!({"format": f})), 1).err().unwrap();
        assert!(err.contains("no original file"), "{err}");
    }
}

#[test]
fn dng_export_rejects_non_raw_photos() {
    let dir = temp_dir("export-dng-jpeg");
    let src = dir.join("a.png");
    let img = lightcraft_raster::Rgba8::from_fn(32, 24, |x, y| [(x * 8) as u8, (y * 10) as u8, 90, 255]);
    std::fs::write(&src, crate::export::encode_image(&img, &ExportOptions { format: ExportFormat::Png, ..Default::default() }).unwrap()).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    let err = export_photo(&mut s, id, &ExportOptions { format: ExportFormat::Dng, ..Default::default() }, 1).err().unwrap();
    assert!(err.contains("needs a raw photo"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn presets_conflicts_and_subfolders() {
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
    assert_eq!(web, json!({"format": "png", "width": 64}), "targets are not part of a preset");
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
/// `lightcraft_gpu::last_fallback` another thread's render overwrites, and the simulated failure
/// switches the GPU off for this thread alone, so neither the fault nor `reset_failures` reaches
/// the other tests.
#[test]
fn export_falls_back_to_the_cpu_when_gpu_work_is_lost() {
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.4})).unwrap();
    s.execute("develop.set", &json!({"control": "effects.clarity", "value": 20})).unwrap();
    let o = ExportOptions::from_json(&json!({"format": "png"}));
    let mean = |b: &[u8]| {
        let d = lightcraft_codecs::decode(b, Default::default()).unwrap().image;
        d.data.iter().map(|p| (p[0] + p[1] + p[2]) as f64).sum::<f64>() / (3 * d.data.len()) as f64
    };
    let gpu = lightcraft_gpu::available();
    let healthy = mean(&export_photo(&mut s, id, &o, 1).unwrap().bytes);
    lightcraft_gpu::inject_fault(lightcraft_gpu::Fault::DropWork);
    let faulted = export_photo(&mut s, id, &o, 1).unwrap();
    if gpu {
        let why = faulted.gpu_fallback.clone().unwrap_or_default();
        assert!(why.contains("incomplete"), "{why}");
        // the GPU is off for this thread now: the next export is not even tried on it
        assert!(!lightcraft_gpu::available());
        assert!(lightcraft_gpu::unavailable_reason().unwrap_or_default().contains("incomplete"), "{:?}", lightcraft_gpu::unavailable_reason());
        // the thread's own `inject_fault` stopped only its own GPU use: the others still have it
        let elsewhere = std::thread::spawn(|| (lightcraft_gpu::available(), lightcraft_gpu::unavailable_reason())).join().unwrap();
        assert_eq!(elsewhere, (true, None), "a simulated failure must not switch the GPU off for the other threads");
    } else {
        assert_eq!(faulted.gpu_fallback, None);
    }
    let cpu = export_photo(&mut s, id, &o, 1).unwrap();
    assert_eq!(cpu.gpu_fallback, None, "not tried on the GPU: no fallback to report");
    lightcraft_gpu::reset_failures();
    assert_eq!(lightcraft_gpu::available(), gpu, "reset_failures gives this thread the GPU back");
    let px = |b: &[u8]| lightcraft_codecs::decode(b, Default::default()).unwrap().image.data;
    assert!(px(&faulted.bytes) == px(&cpu.bytes), "the fallback is the CPU render");
    let m = mean(&faulted.bytes);
    assert!(m > 0.02, "not black: mean {m}");
    assert!((m - healthy).abs() < 0.01, "GPU {healthy} vs CPU {m}");
}

/// A JPEG original `dir/IMG_1.jpg` imported into a library on disk.
fn library_with_jpeg(tag: &str) -> (Session, lightcraft_catalog::PhotoId, std::path::PathBuf, std::path::PathBuf, Vec<u8>) {
    let dir = temp_dir(tag);
    let src = dir.join("IMG_1.jpg");
    let img = lightcraft_raster::Rgba8::from_fn(48, 32, |x, y| [(x * 5) as u8, (y * 7) as u8, 120, 255]);
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

#[test]
fn preset_export_never_overwrites_originals_or_sidecars() {
    preset_export_preserves_originals("preset.export", json!({"ids": ["lc.warm-glow"]}), "preset-export-guard");
}

#[test]
fn curve_preset_export_never_overwrites_originals_or_sidecars() {
    preset_export_preserves_originals("curve.exportPresets", json!({"names": ["Linear"]}), "curve-export-guard");
}

/// Issue #1944: Export Keywords wrote its list over whatever path it was given, a library
/// original included.
#[test]
fn keyword_export_never_overwrites_originals_or_sidecars() {
    preset_export_preserves_originals("keyword.export", json!({}), "keyword-export-guard");
}

#[test]
fn preset_exports_preserve_previous_files_on_write_failure() {
    let dir = temp_dir("preset-export-failure");
    let mut s = Session::new();
    for (command, selection, file) in
        [("preset.export", json!({"ids": ["lc.warm-glow"]}), "look.lcpreset"), ("curve.exportPresets", json!({"names": ["Linear"]}), "curve.lccurve")]
    {
        let path = dir.join(file);
        let mut params = selection;
        params["path"] = json!(path.to_string_lossy());
        s.execute(command, &params).unwrap();
        let previous = std::fs::read(&path).unwrap();
        {
            let _fault = lightcraft_catalog::safe_file::fail_writes_after(4);
            assert!(s.execute(command, &params).is_err(), "{command} must report the failed write");
        }
        assert_eq!(std::fs::read(&path).unwrap(), previous, "{command} preserves the prior export");
        assert_eq!(s.execute(command, &params).unwrap()["count"], 1, "replacement still succeeds");
    }
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "no temporary files remain");
    let _ = std::fs::remove_dir_all(dir);
}

fn disk_batch(
    s: &mut Session,
    id: lightcraft_catalog::PhotoId,
    o: &ExportOptions,
    to: &crate::export::Destination,
) -> Result<Vec<serde_json::Value>, String> {
    crate::export::export_batch(s, &[id], o, to, &mut crate::export::write_file, &|p| std::path::Path::new(p).exists())
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

/// Issue #93: the XMP sidecar of an "Original" export ignored the conflict policy.
#[test]
fn export_sidecars_follow_the_conflict_policy() {
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
        let _fault = lightcraft_catalog::safe_file::fail_writes_after(100);
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
    use lightcraft_catalog::safe_file::syncs_on_this_thread;
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
fn sunset(s: &mut Session) -> lightcraft_catalog::PhotoId {
    let id = s.catalog.photos().find(|p| p.meta.title == "Golden horizon").map(|p| p.id).unwrap();
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    id
}

#[test]
fn hdr_jpeg_export_writes_a_gain_map_and_sdr_edits_stay_plain() {
    use lightcraft_codecs::gainmap;
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
    // the base is the SDR rendition: the same pixels as the SDR export (the decoders agree)
    let a = lightcraft_codecs::decode(&e.bytes, Default::default()).unwrap();
    let b = lightcraft_codecs::decode(&plain.bytes, Default::default()).unwrap();
    assert_eq!((a.width, a.height), (b.width, b.height));
    let diff = a.image.data.iter().zip(&b.image.data).map(|(p, q)| (p[1] - q[1]).abs()).fold(0.0f32, f32::max);
    assert!(diff < 0.02, "base differs from the SDR export by {diff}");
    // HDR off again: the gain map goes away
    s.execute("develop.hdr", &json!({"enabled": false})).unwrap();
    assert!(!gainmap::is_gain_map_jpeg(&export_photo(&mut s, id, &o, 1).unwrap().bytes));
}

#[test]
fn hdr_float_tiff_keeps_highlights_above_white() {
    let mut s = Session::with_demo();
    let id = sunset(&mut s);
    s.execute("develop.hdr", &json!({"enabled": true, "maxEv": 2})).unwrap();
    let max_of = |s: &mut Session, hdr: bool| {
        let o = ExportOptions::from_json(&json!({"longEdge": 240, "format": "tiff", "bitDepth": 32, "hdr": hdr}));
        let e = export_photo(s, id, &o, 1).unwrap();
        let d = lightcraft_codecs::decode(&e.bytes, Default::default()).unwrap();
        d.image.data.iter().flat_map(|p| p.iter().copied()).fold(0.0f32, f32::max)
    };
    let hdr = max_of(&mut s, true);
    let sdr = max_of(&mut s, false);
    assert!(sdr <= 1.0 + 1e-3, "SDR float TIFF stays in 0..1 ({sdr})");
    assert!(hdr > 1.2 && hdr <= 4.0 + 1e-3, "HDR float TIFF max {hdr}");
}

#[test]
fn hdr_avif_export_is_pq_rec2020() {
    let mut s = Session::with_demo();
    let id = sunset(&mut s);
    let o = ExportOptions::from_json(&json!({"longEdge": 160, "format": "avif", "hdr": true, "quality": 70}));
    assert!(o.hdr_output());
    let sdr = export_photo(&mut s, id, &o, 1).unwrap();
    s.execute("develop.hdr", &json!({"enabled": true, "maxEv": 2})).unwrap();
    let hdr = export_photo(&mut s, id, &o, 1).unwrap();
    // the `colr` nclx box: BT.2020 primaries (9), PQ (16), BT.2020 NCL (9), full range
    let nclx = |b: &[u8]| b.windows(4).position(|w| w == b"nclx").map(|i| b[i + 4..i + 11].to_vec());
    let c = nclx(&hdr.bytes).expect("colr nclx box");
    assert_eq!(c, vec![0, 9, 0, 16, 0, 9, 0x80]);
    assert!(hdr.bytes.windows(4).any(|w| w == b"clli"), "content light level box");
    // a photo without an HDR edit exports as an ordinary (sRGB) AVIF
    assert!(nclx(&sdr.bytes).is_none_or(|c| c[3] != 16));
}

#[test]
fn hdr_avif_export_hlg() {
    let mut s = Session::with_demo();
    let id = sunset(&mut s);
    let o = ExportOptions::from_json(&json!({"longEdge": 160, "format": "avif", "hdr": true, "hdrTransfer": "hlg", "quality": 70}));
    assert_eq!(o.hdr_transfer, crate::export::HdrCurve::Hlg);
    assert_eq!(o.to_json()["hdrTransfer"], "hlg");
    s.execute("develop.hdr", &json!({"enabled": true, "maxEv": 2})).unwrap();
    let hdr = export_photo(&mut s, id, &o, 1).unwrap();
    // nclx: BT.2020 primaries (9), HLG (18), BT.2020 NCL (9), full range; no content light level box
    let i = hdr.bytes.windows(4).position(|w| w == b"nclx").expect("colr nclx box");
    assert_eq!(hdr.bytes[i + 4..i + 11].to_vec(), vec![0, 9, 0, 18, 0, 9, 0x80]);
    assert!(!hdr.bytes.windows(4).any(|w| w == b"clli"));
}

/// "Remove location info" holds for the file-copying formats too: the sidecar and the DNG's
/// packet carry no location, and an original that embeds GPS is refused rather than copied.
#[test]
fn remove_location_holds_for_original_and_dng_exports() {
    let dir = temp_dir("export-noloc");
    let gps =
        lightcraft_meta::Metadata { gps: Some(lightcraft_meta::Gps { latitude: 48.85, longitude: 2.35, altitude: None }), ..Default::default() };
    // a raw with no embedded GPS, located in the catalog
    let raw = dir.join("Raw.dng");
    std::fs::write(&raw, synthetic_dng_with(None, Default::default())).unwrap();
    // a JPEG with GPS in its EXIF
    let jpg = dir.join("Geo.jpg");
    let img = lightcraft_raster::Rgba8::from_fn(32, 24, |x, y| [(x * 8) as u8, (y * 10) as u8, 90, 255]);
    let exif = lightcraft_meta::write_exif(&gps);
    let meta = lightcraft_codecs::EncodeMeta { exif: Some(&exif), ..Default::default() };
    std::fs::write(&jpg, lightcraft_codecs::encode_jpeg(&lightcraft_codecs::EncodeImage::rgba8(&img), 90, Default::default(), &meta).unwrap())
        .unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [raw.to_string_lossy(), jpg.to_string_lossy()]})).unwrap();
    let id = |s: &Session, n: &str| s.catalog.photos().find(|p| p.file_name == n).unwrap().id;
    let (rid, jid) = (id(&s, "Raw.dng"), id(&s, "Geo.jpg"));
    s.execute("photo.setMeta", &json!({"ids": [rid.0, jid.0], "gps": [48.85, 2.35], "city": "Paris"})).unwrap();
    let opts = |f: &str, remove: bool| ExportOptions::from_json(&json!({"format": f, "removeLocation": remove}));

    // original raw: the sidecar keeps the location unless it is removed
    let side = |e: &crate::export::Exported| lightcraft_meta::parse_xmp(std::str::from_utf8(&e.sidecars[0].1).unwrap()).unwrap().metadata;
    let kept = side(&export_photo(&mut s, rid, &opts("original", false), 1).unwrap());
    assert!(kept.gps.is_some() && kept.city.is_some());
    let gone = side(&export_photo(&mut s, rid, &opts("original", true), 1).unwrap());
    assert!(gone.gps.is_none() && gone.city.is_none(), "{gone:?}");

    // dng: the embedded packet
    let e = export_photo(&mut s, rid, &opts("dng", false), 1).unwrap();
    assert!(lightcraft_meta::extract(&e.bytes).gps.is_some());
    let e = export_photo(&mut s, rid, &opts("dng", true), 1).unwrap();
    let m = lightcraft_meta::extract(&e.bytes);
    assert!(m.gps.is_none() && m.city.is_none(), "{m:?}");

    // original JPEG embedding GPS: copied as is normally, refused when the location must go
    let e = export_photo(&mut s, jid, &opts("original", false), 1).unwrap();
    assert!(lightcraft_meta::extract(&e.bytes).gps.is_some());
    let err = export_photo(&mut s, jid, &opts("original", true), 1).err().unwrap();
    assert!(err.contains("can't remove the location") && err.contains("export as JPEG"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
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
        lightcraft_meta::extract(&export_photo(s, id, &ExportOptions::from_params(&p).unwrap(), 1).unwrap().bytes)
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

/// An original raw whose own metadata holds GPS is refused under "Remove location info".
#[test]
fn remove_location_refuses_a_raw_that_embeds_gps() {
    let dir = temp_dir("export-noloc-raw");
    let gps = lightcraft_meta::Metadata { gps: Some(lightcraft_meta::Gps { latitude: 10.0, longitude: 20.0, altitude: None }), ..Default::default() };
    let raw = dir.join("Geo.dng");
    let bytes = synthetic_dng_with(Some(&lightcraft_meta::write_xmp(&gps, None)), Default::default());
    assert!(lightcraft_meta::extract(&bytes).gps.is_some(), "fixture embeds GPS");
    std::fs::write(&raw, bytes).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [raw.to_string_lossy()]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    let err = export_photo(&mut s, id, &ExportOptions::from_json(&json!({"format": "original", "removeLocation": true})), 1).err().unwrap();
    assert!(err.contains("can't remove the location"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hdr_png_export_is_16_bit_pq_rec2020() {
    let mut s = Session::with_demo();
    let id = sunset(&mut s);
    let o = ExportOptions::from_json(&json!({"longEdge": 160, "format": "png", "hdr": true}));
    assert!(o.hdr_output());
    assert_eq!(o.effective_space(), lightcraft_pipeline::OutputSpace::Rec2020);
    // a photo without an HDR edit exports as an ordinary (sRGB, 8-bit) PNG
    let sdr = export_photo(&mut s, id, &o, 1).unwrap();
    assert!(!sdr.bytes.windows(4).any(|w| w == b"cICP"));
    assert_eq!(sdr.bytes[24], 8, "8-bit");
    s.execute("develop.hdr", &json!({"enabled": true, "maxEv": 2})).unwrap();
    let hdr = export_photo(&mut s, id, &o, 1).unwrap();
    let at = hdr.bytes.windows(4).position(|w| w == b"cICP").expect("cICP chunk");
    assert_eq!(&hdr.bytes[at + 4..at + 8], &[9, 16, 0, 1], "BT.2020, PQ, RGB, full range");
    assert_eq!(hdr.bytes[24], 16, "16-bit");
    // the highlights above SDR white are there: MaxCLL beyond the 203 cd/m² of SDR white
    let at = hdr.bytes.windows(4).position(|w| w == b"cLLI").expect("cLLI chunk");
    let max_cll = u32::from_be_bytes(hdr.bytes[at + 4..at + 8].try_into().unwrap()) as f32 / 10_000.0;
    assert!(max_cll > 250.0 && max_cll <= 4.0 * 203.0 + 1.0, "MaxCLL {max_cll}");
    // HDR output off: the HDR edit exports its SDR rendition
    let plain = export_photo(&mut s, id, &ExportOptions::from_json(&json!({"longEdge": 160, "format": "png"})), 1).unwrap();
    assert!(!plain.bytes.windows(4).any(|w| w == b"cICP"));
}

#[test]
fn hdr_jxl_export_is_pq_rec2020() {
    let mut s = Session::with_demo();
    let id = sunset(&mut s);
    let o = ExportOptions::from_json(&json!({"longEdge": 160, "format": "jxl", "hdr": true, "quality": 90}));
    assert!(o.hdr_output());
    let sdr = export_photo(&mut s, id, &o, 1).unwrap();
    s.execute("develop.hdr", &json!({"enabled": true, "maxEv": 2})).unwrap();
    let hdr = export_photo(&mut s, id, &o, 1).unwrap();
    let encoding = |b: &[u8]| {
        let img = jxl_oxide::JxlImage::builder().read(std::io::Cursor::new(b)).unwrap();
        let m = &img.image_header().metadata;
        (format!("{:?}", m.colour_encoding), m.bit_depth.bits_per_sample())
    };
    let (c, bits) = encoding(&hdr.bytes);
    assert!(c.contains("Bt2100") && c.contains("Pq"), "{c}");
    assert_eq!(bits, 16);
    // a photo without an HDR edit exports as an ordinary SDR file
    let (c, _) = encoding(&sdr.bytes);
    assert!(!c.contains("Pq"), "{c}");
}

#[test]
fn jxl_export_writes_metadata() {
    let mut s = Session::with_demo();
    let id = sunset(&mut s);
    let o = ExportOptions::from_json(&json!({"longEdge": 120, "format": "jxl", "quality": 80}));
    let e = export_photo(&mut s, id, &o, 1).unwrap();
    let d = lightcraft_codecs::decode(&e.bytes, Default::default()).unwrap();
    assert_eq!(d.width.max(d.height), 120);
    assert!(d.xmp.as_deref().is_some_and(|x| x.contains("Golden horizon")), "title in the XMP box");
    assert!(d.exif.is_some(), "EXIF box");
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

    fn export_two(s: &mut Session, tag: &str) -> Vec<Value> {
        let dir = temp_dir(tag);
        let ids: Vec<_> = s.visible().iter().copied().take(2).collect();
        let o = ExportOptions::from_json(&json!({"longEdge": 48, "naming": "{name}-{seq}"}));
        let to = Destination { dir: dir.to_string_lossy().into(), exact: None };
        let mut files = Vec::new();
        for id in ids {
            files.extend(disk_batch(s, id, &o, &to).unwrap());
        }
        files
    }

    #[test]
    fn exported_files_go_to_photos_in_order() {
        let mut s = Session::with_demo();
        let files = export_two(&mut s, "photos-flow");
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
                let files = export_two(&mut s, "photos-session");
                assert_eq!(after.run(&files, false).unwrap()["imported"], 2);
                assert_eq!(calls.lock().unwrap().len(), 1);
            }
            Err(e) => assert!(e.contains("only available on macOS"), "{e}"),
        }
    }

    #[test]
    fn the_add_to_photos_command_needs_a_runner_and_checks_its_input() {
        let mut s = Session::with_demo();
        let files = export_two(&mut s, "photos-cmd");
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
        let files = export_two(&mut s, "photos-slow");
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
        let mut s = Session::with_demo();
        let (runner, calls) = fake(usize::MAX);
        s.apple_photos = Some(runner.clone());
        let elsewhere = export_two(&mut s, "photos-hold-other");
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
        let files = export_two(&mut s, "photos-hold");
        let out = after.run(&files, false).unwrap();
        assert_eq!((out["job"].as_u64(), out["imported"].as_u64(), out["album"].as_str()), (Some(1), Some(2), Some("Held")), "{out}");
        // then the slot is free
        assert_eq!(s.execute("export.addToPhotos", &json!({"paths": other})).unwrap()["job"], 2);
        // an export that fails (its AfterExport dropped) frees it too
        drop(AfterExport::apple_photos(fake(usize::MAX).0, "", &s.apple_photos_imports).unwrap());
        assert!(s.apple_photos_imports.running().is_none());
        assert_eq!(s.execute("export.addToPhotos", &json!({"paths": other})).unwrap()["job"], 4);
    }

    /// Review of #236: a runner that panics while the command or an export waits for it is that
    /// import's error; the slot is freed, so the next import isn't refused as "still running"
    /// and a process ending doesn't wait for it.
    #[test]
    fn a_panicking_runner_never_holds_photos() {
        let mut s = Session::with_demo();
        let files = export_two(&mut s, "photos-panic");
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

    /// The desktop app, lightcraft-cli and the MCP server get the real runner from `with_fs`, on
    /// macOS only. (Installed, never run here.)
    #[test]
    fn with_fs_installs_the_runner_on_macos_only() {
        let s = Session::new().with_fs();
        assert_eq!(s.apple_photos.is_some(), cfg!(target_os = "macos"));
        assert_eq!(s.commands().into_iter().find(|c| c.id == "export.addToPhotos").unwrap().enabled, cfg!(target_os = "macos"));
    }
}

/// Issue #956: "Same folder as original photo" puts each photo's export next to its own original
/// (plus the subfolder), so one batch from two folders lands in two folders. The conflict policy
/// and the original guard work per folder as they do for a specific folder.
#[test]
fn same_folder_as_original_exports_next_to_each_photo() {
    use crate::export::{Conflict, Destination, DestinationMode};
    let dir = temp_dir("export-same-folder");
    let (a, b) = (dir.join("A"), dir.join("B"));
    // two different photos (the same bytes would be imported once)
    let jpeg = |blue: u8| {
        let img = lightcraft_raster::Rgba8::from_fn(48, 32, |x, y| [(x * 5) as u8, (y * 7) as u8, blue, 255]);
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
    let mut want = vec![a.join("Exports/IMG_1.jpg").to_string_lossy().to_string(), b.join("Exports/IMG_1.jpg").to_string_lossy().to_string()];
    let mut got: Vec<String> = written.iter().map(|p| p.to_string()).collect();
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

/// Issue #956: a photo with no folder on disk (a demo photo) or whose folder is gone gets its own
/// error in "Same folder as original photo" mode instead of a file written somewhere else.
#[test]
fn same_folder_as_original_refuses_photos_without_a_folder() {
    use crate::export::{Destination, DestinationMode};
    let o = ExportOptions { destination_mode: DestinationMode::SameAsOriginal, subfolder: "Exports".into(), ..Default::default() };
    let to = Destination { dir: "out".into(), exact: None };
    let mut s = Session::with_demo();
    let ids: Vec<_> = s.catalog.photos().take(2).map(|p| p.id).collect();
    let mut written = Vec::new();
    let mut write = |p: &str, _: &[u8]| -> Result<(), String> {
        written.push(p.to_string());
        Ok(())
    };
    let err = crate::export::export_batch(&mut s, &ids, &o, &to, &mut write, &|_| false).unwrap_err();
    assert!(err.contains("no folder on disk"), "{err}");
    // without stopping: one error per photo, nothing written
    let items = crate::export::prepare_batch(&mut s, &ids, &o).unwrap();
    let files = crate::export::run_batch(items, &o, &to, &mut write, &|_| false, false, &mut |_, _| true).unwrap();
    assert_eq!(files.len(), 2);
    for (f, id) in files.iter().zip(&ids) {
        assert_eq!(f["photo"], id.0, "{f}");
        assert!(f["error"].as_str().unwrap().contains("no folder on disk"), "{f}");
    }
    drop(write);
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
