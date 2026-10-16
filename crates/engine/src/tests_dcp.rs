//! Imported camera profiles (`.dcp`): import, choosing one for a raw photo, decoding with it, the
//! source cache, persistence and deletion. Profiles are built in memory (no profile file is committed).

use std::path::PathBuf;

use lightcraft_catalog::PhotoId;
use lightcraft_raw::dcp::{DcpProfile, write_dcp};
use lightcraft_raw::{ColorData, Mat3};
use serde_json::json;

use crate::Session;
use crate::media::SourceRef;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-dcp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A profile whose matrix swaps the camera's red and blue (our own numbers): photos developed with it
/// come out with red and blue exchanged, which is easy to see in a test.
fn swapping_profile(name: &str) -> DcpProfile {
    let m = Mat3([[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]]);
    let xyz_to_cam = m.mul(&Mat3([[3.2406, -1.5372, -0.4986], [-0.9689, 1.8758, 0.0415], [0.0557, -0.2040, 1.0570]]));
    DcpProfile {
        name: name.into(),
        camera_model: "LightCraft Test Camera".into(),
        color: ColorData { illuminant: [21, 0], color_matrix: [Some(xyz_to_cam), None], ..Default::default() },
    }
}

fn mean_rgb(img: &lightcraft_raster::Rgb32f) -> [f32; 3] {
    let n = img.data.len().max(1) as f32;
    let s = img.data.iter().fold([0.0f32; 3], |a, p| [a[0] + p[0], a[1] + p[1], a[2] + p[2]]);
    [s[0] / n, s[1] / n, s[2] / n]
}

#[test]
fn a_camera_profile_replaces_the_raw_colour_model_when_decoding() {
    let dng = crate::tests_xmp::synthetic_dng_with(None, Default::default());
    let (plain, plain_info) = crate::files::load_bytes(&dng, 64).unwrap();
    let p = swapping_profile("Swap");
    let (with, info) = crate::files::load_bytes_with(&dng, 64, Some(&p)).unwrap();
    let (a, b) = (mean_rgb(&plain), mean_rgb(&with));
    assert!((a[0] - b[0]).abs() > 1e-3 || (a[2] - b[2]).abs() > 1e-3, "the profile changes the colour: {a:?} vs {b:?}");
    // white balance at render time works from the profile's matrices
    let tags = &info.camera_color.as_ref().expect("a matrix colour model").tags;
    assert_eq!(tags.color_matrix, p.color.color_matrix);
    assert_ne!(plain_info.camera_color.as_ref().map(|c| c.tags.color_matrix), Some(p.color.color_matrix));
}

#[test]
fn a_profile_without_a_tone_curve_keeps_the_photos_contrast() {
    use lightcraft_raw::profile::ToneCurve;
    // a DNG with a tone curve of its own (our numbers: 18 % grey → 30 %)
    let plain = crate::tests_xmp::synthetic_dng_with(None, Default::default());
    let mut raw = lightcraft_raw::decode(&plain).unwrap();
    raw.color.profile.tone_curve = ToneCurve::from_tag(&[0.0, 0.0, 0.18, 0.3, 1.0, 1.0]);
    let dng = lightcraft_raw::write_dng(&raw, &Default::default()).unwrap();
    // a chart-style profile (no curve) keeps the file's curve
    let (_, info) = crate::files::load_bytes_with(&dng, 64, Some(&swapping_profile("Swap"))).unwrap();
    let tone = info.camera_tone.expect("the file's tone curve stays");
    assert!((tone.apply(0.18) - 0.3).abs() < 0.01, "{}", tone.apply(0.18));
    // a profile with a curve of its own uses that one (18 % → 45 %)
    let mut curved = swapping_profile("Curved");
    curved.color.profile.tone_curve = ToneCurve::from_tag(&[0.0, 0.0, 0.18, 0.45, 1.0, 1.0]);
    let (_, info) = crate::files::load_bytes_with(&dng, 64, Some(&curved)).unwrap();
    let tone = info.camera_tone.expect("the profile's tone curve");
    assert!((tone.apply(0.18) - 0.45).abs() < 0.01, "{}", tone.apply(0.18));
}

#[test]
fn import_choose_cache_persist_and_delete() {
    let dir = temp_dir("session");
    let (lib, files) = (dir.join("lib"), dir.join("files"));
    std::fs::create_dir_all(files.join("profiles")).unwrap();
    let raw = files.join("IMG_0001.dng");
    std::fs::write(&raw, crate::tests_xmp::synthetic_dng_with(None, Default::default())).unwrap();
    std::fs::write(files.join("profiles").join("Swap.dcp"), write_dcp(&swapping_profile("Swap")).unwrap()).unwrap();
    std::fs::write(files.join("profiles").join("Broken.dcp"), b"IIRC\x08\0\0\0junk").unwrap();
    let id;
    let dcp_id;
    {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        let r = s.execute("library.import", &json!({"paths": [raw.to_string_lossy()]})).unwrap();
        id = PhotoId(r["imported"][0].as_u64().unwrap());
        s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
        // a folder: the good profile comes in, the broken one is reported
        let r = s.execute("profile.import", &json!({"paths": [files.join("profiles").to_string_lossy()]})).unwrap();
        assert_eq!(r["imported"].as_array().unwrap().len(), 1, "{r}");
        assert_eq!(r["failed"].as_array().unwrap().len(), 1, "{r}");
        dcp_id = r["imported"][0]["id"].as_str().unwrap().to_string();
        assert!(dcp_id.starts_with("dcp:"));
        assert_eq!(r["imported"][0]["cameraModel"], "LightCraft Test Camera");
        // importing it again adds nothing
        let again = s.execute("profile.import", &json!({"paths": [files.join("profiles").join("Swap.dcp").to_string_lossy()]})).unwrap();
        assert_eq!(again["imported"][0]["existing"], true);
        assert_eq!(s.dcp_profiles.len(), 1);
        // the library keeps its own copy
        assert!(std::path::Path::new(&s.dcp_profiles[0].file).starts_with(&lib));
        // listed in the profile menu under Camera Profiles
        let menu = s.profile_menu();
        let group = menu["groups"].as_array().unwrap().iter().find(|g| g["name"] == "Camera Profiles").expect("a Camera Profiles group");
        assert_eq!(group["profiles"][0]["id"], dcp_id.as_str());
        // develop the photo with it: a decode made without it doesn't stand in
        let before = s.render_job(id, 48, 48, false, true).unwrap().run();
        let loaded = before.loaded.clone().expect("decoded from the file");
        assert!(loaded.camera_profile.is_none());
        s.media.insert(id, before.level, loaded.image.clone());
        assert!(matches!(s.render_job(id, 48, 48, false, true).unwrap().source, SourceRef::Loaded(_)), "cached while nothing changed");
        s.execute("develop.profile", &json!({"id": dcp_id})).unwrap();
        let job = s.render_job(id, 48, 48, false, true).unwrap();
        match &job.source {
            SourceRef::File { camera_profile: Some((pid, _)), .. } => assert_eq!(pid, &dcp_id),
            _ => panic!("the photo is decoded again with its camera profile"),
        }
        let after = job.run();
        assert_eq!(after.loaded.as_ref().and_then(|l| l.camera_profile.clone()).as_deref(), Some(dcp_id.as_str()));
        assert!(after.rendered.is_ok());
        assert_ne!(mean_rgb(&before.loaded.unwrap().image), mean_rgb(&after.loaded.unwrap().image));
        s.persist().unwrap();
        s.close_library().unwrap();
    }
    // reopened: the profile is there and decodes again
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert_eq!(s.dcp_profiles.len(), 1);
    assert!(s.media.camera_profiles.contains_key(&dcp_id));
    assert_eq!(s.catalog.photo(id).unwrap().develop.profile.id, dcp_id);
    assert_eq!(s.profile_info(&dcp_id).map(|(_, g)| g), Some("Camera Profiles"));
    // deleted: photos using it develop with their file's own colour, the library's copy goes
    let file = s.dcp_profiles[0].file.clone();
    s.execute("profile.deleteImported", &json!({"id": dcp_id})).unwrap();
    assert!(s.dcp_profiles.is_empty() && !std::path::Path::new(&file).exists());
    let p = s.catalog.photo(id).unwrap().clone();
    assert!(s.media.camera_profile_of(&p).is_none());
    assert!(s.execute("develop.profile", &json!({"id": dcp_id})).is_err(), "no longer a known profile");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Hovering a camera profile (the loupe's preview, the browser's thumbnails) develops the photo with
/// it, and hovering a built-in profile while one is applied develops without it. Before, both used
/// the photo's own decode: a camera profile's colour never showed in a hover.
#[test]
fn hover_previews_and_browser_thumbnails_decode_with_the_hovered_camera_profile() {
    let dir = temp_dir("hover");
    let (lib, files) = (dir.join("lib"), dir.join("files"));
    std::fs::create_dir_all(&files).unwrap();
    let raw = files.join("IMG_0002.dng");
    std::fs::write(&raw, crate::tests_xmp::synthetic_dng_with(None, Default::default())).unwrap();
    let dcp_file = files.join("Swap.dcp");
    std::fs::write(&dcp_file, write_dcp(&swapping_profile("Swap")).unwrap()).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let r = s.execute("library.import", &json!({"paths": [raw.to_string_lossy()]})).unwrap();
    let id = PhotoId(r["imported"][0].as_u64().unwrap());
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    let dcp_id =
        s.execute("profile.import", &json!({"paths": [dcp_file.to_string_lossy()]})).unwrap()["imported"][0]["id"].as_str().unwrap().to_string();
    let mean = |j: crate::media::RenderJob| {
        let img = j.run().rendered.unwrap().image;
        let n = img.data.len().max(1) as f64;
        img.data.iter().fold([0.0f64; 3], |a, p| [a[0] + f64::from(p[0]) / n, a[1] + f64::from(p[1]) / n, a[2] + f64::from(p[2]) / n])
    };
    let differ = |a: [f64; 3], b: [f64; 3]| a.iter().zip(b).any(|(x, y)| (x - y).abs() > 2.0);
    let close = |a: [f64; 3], b: [f64; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.6);
    let own = (*s.develop_of(id).unwrap()).clone();
    let mut hovered = own.clone();
    hovered.profile.id = dcp_id.clone();
    let builtin = mean(s.render_job(id, 64, 64, false, true).unwrap());
    let hover = mean(s.preview_job(id, 64, 64, true, &hovered).unwrap());
    assert!(differ(hover, builtin), "the hover shows the camera profile: {hover:?} vs {builtin:?}");
    // the browser's thumbnail too, under its own cache key
    let (va, vb) = (s.variant_job(id, &own, 64).unwrap(), s.variant_job(id, &hovered, 64).unwrap());
    assert_ne!(va.cache.as_ref().unwrap().1, vb.cache.as_ref().unwrap().1);
    assert!(matches!(&vb.source, SourceRef::File { camera_profile: Some(_), .. }), "decoded with the camera profile");
    assert!(differ(mean(va), mean(vb)));
    // selected: the same picture as the hover; hovering the built-in profile then: without it
    s.execute("develop.profile", &json!({"id": dcp_id})).unwrap();
    let selected = mean(s.render_job(id, 64, 64, false, true).unwrap());
    assert!(close(hover, selected), "{hover:?} vs {selected:?}");
    let back = mean(s.preview_job(id, 64, 64, true, &own).unwrap());
    assert!(close(back, builtin), "{back:?} vs {builtin:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
