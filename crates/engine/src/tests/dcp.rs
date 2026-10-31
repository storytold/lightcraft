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
        // a single .dcp file is a camera profile only, not also a LUT that "couldn't be read"
        assert_eq!(again["failed"], json!([]), "{again}");
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
            SourceRef::File { camera_profile: Some((pid, _)), .. } => assert!(pid.starts_with(&format!("{dcp_id}#")), "{pid}"),
            _ => panic!("the photo is decoded again with its camera profile"),
        }
        let after = job.run();
        assert_eq!(
            after.loaded.as_ref().and_then(|l| l.camera_profile.clone()),
            s.media.camera_profile_of(s.catalog.photo(id).unwrap()).map(|c| c.0)
        );
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
    // the profile's file replaced under the same id: the hover's key changes with it, so the old picture isn't
    // taken for current (CodeRabbit on #1063)
    let before = s.preview_job(id, 64, 64, true, &hovered).unwrap().key;
    s.media.camera_profiles.get_mut(&dcp_id).unwrap().fingerprint ^= 0x10;
    assert_ne!(s.preview_job(id, 64, 64, true, &hovered).unwrap().key, before);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A library with one raw photo and the `Swap` profile imported from `files/<sub>/Swap.dcp` and selected for it.
fn library_with_profile(dir: &std::path::Path) -> (Session, PhotoId, String) {
    let (lib, files) = (dir.join("lib"), dir.join("files"));
    std::fs::create_dir_all(files.join("a")).unwrap();
    let raw = files.join("IMG_0003.dng");
    std::fs::write(&raw, crate::tests_xmp::synthetic_dng_with(None, Default::default())).unwrap();
    std::fs::write(files.join("a").join("Swap.dcp"), write_dcp(&swapping_profile("Swap")).unwrap()).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let r = s.execute("library.import", &json!({"paths": [raw.to_string_lossy()]})).unwrap();
    let id = PhotoId(r["imported"][0].as_u64().unwrap());
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    let r = s.execute("profile.import", &json!({"paths": [files.join("a").join("Swap.dcp").to_string_lossy()]})).unwrap();
    let dcp_id = r["imported"][0]["id"].as_str().unwrap().to_string();
    s.execute("develop.profile", &json!({"id": dcp_id})).unwrap();
    (s, id, dcp_id)
}

/// The thumbnail, browser-thumbnail and loupe cache keys of photo `id` (as it is now), and its source tag.
fn render_keys(s: &mut Session, id: PhotoId) -> [lightcraft_preview::Hash128; 3] {
    let settings = (*s.develop_of(id).unwrap()).clone();
    [
        s.thumb_job(id, 256).unwrap().cache.unwrap().1,
        s.variant_job(id, &settings, 64).unwrap().cache.unwrap().1,
        s.loupe_job(id, 200, 200, true).unwrap().view_cache.unwrap().1,
    ]
}

/// A camera profile deleted, or replaced by a different file under the same id, never has renders or decoded
/// sources made with the old one stand in: before, the cache keys followed only the profile id, which the photo keeps.
#[test]
fn a_deleted_or_replaced_camera_profile_never_reuses_renders_made_with_it() {
    let dir = temp_dir("replace");
    let (mut s, id, dcp_id) = library_with_profile(&dir);
    let with = render_keys(&mut s, id);
    let level = s.render_job(id, 48, 48, false, true).unwrap().level;
    let old_tag = s.media.camera_profile_of(s.catalog.photo(id).unwrap()).map(|c| c.0);
    assert!(old_tag.is_some());
    // deleted: the photo keeps the id, renders without the profile, under other keys
    s.execute("profile.deleteImported", &json!({"id": dcp_id})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().develop.profile.id, dcp_id);
    let without = render_keys(&mut s, id);
    for (a, b) in with.iter().zip(&without) {
        assert_ne!(a, b, "a render made with the deleted profile would stand in");
    }
    // another profile under the same file name gets the same id back, and other keys again
    let other_dir = dir.join("files").join("b");
    std::fs::create_dir_all(&other_dir).unwrap();
    let mut other = swapping_profile("Swap");
    other.color.color_matrix = [Some(Mat3([[3.2406, -1.5372, -0.4986], [-0.9689, 1.8758, 0.0415], [0.0557, -0.2040, 1.0570]])), None];
    std::fs::write(other_dir.join("Swap.dcp"), write_dcp(&other).unwrap()).unwrap();
    let r = s.execute("profile.import", &json!({"paths": [other_dir.join("Swap.dcp").to_string_lossy()]})).unwrap();
    assert_eq!(r["imported"][0]["id"], dcp_id.as_str(), "{r}");
    let replaced = render_keys(&mut s, id);
    for ((a, b), c) in with.iter().zip(&without).zip(&replaced) {
        assert_ne!(a, c, "a render made with the replaced profile would stand in");
        assert_ne!(b, c);
    }
    // nor does a source decoded with the old file
    let img = std::sync::Arc::new(lightcraft_raster::Rgb32f::new(4, 4));
    s.media.insert_source(id, level, crate::media::DecodedSource { camera_profile: old_tag, ..crate::media::DecodedSource::new(img, None) });
    assert!(!matches!(s.render_job(id, 48, 48, false, true).unwrap().source, SourceRef::Loaded(_)), "decoded again with the new file");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A grid thumbnail is cut from the photo's decoded preview only when that preview was decoded with the camera profile
/// the photo uses now; before, one made with another profile (or none) was fitted and cached under the new key.
#[test]
fn a_thumbnail_is_not_cut_from_a_preview_decoded_with_another_camera_profile() {
    let dir = temp_dir("fit");
    let (mut s, id, _) = library_with_profile(&dir);
    let p = (**s.catalog.photo(id).unwrap()).clone();
    let tag = s.media.camera_profile_of(&p).map(|c| c.0);
    let img = std::sync::Arc::new(lightcraft_raster::Rgb32f::new(1600, 1000));
    let preview = |camera_profile| crate::media::DecodedSource { camera_profile, ..crate::media::DecodedSource::new(img.clone(), None) };
    use crate::media::SourceLevel;
    s.media.insert_source(id, SourceLevel::Preview, preview(None));
    assert!(!matches!(s.media.source_ref(&p, SourceLevel::Thumb), SourceRef::Fit { .. }), "decoded without the profile");
    s.media.insert_source(id, SourceLevel::Preview, preview(tag));
    assert!(matches!(s.media.source_ref(&p, SourceLevel::Thumb), SourceRef::Fit { .. }), "decoded with it: still cut from it");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A library moved (or restored) elsewhere finds its camera profiles in its own `Profiles/` folder, and a camera
/// profile can be a favourite, which survives reopening like the recent list.
#[test]
fn a_moved_library_keeps_its_camera_profiles_and_their_favourites() {
    let dir = temp_dir("moved");
    let (mut s, id, dcp_id) = library_with_profile(&dir);
    assert_eq!(s.execute("profile.favorite", &json!({"id": dcp_id})).unwrap()["favorite"], true);
    assert!(s.profile_recent.contains(&dcp_id));
    s.persist().unwrap();
    s.close_library().unwrap();
    drop(s);
    let moved = dir.join("moved");
    std::fs::rename(dir.join("lib"), &moved).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&moved, false).unwrap();
    assert!(s.media.camera_profiles.contains_key(&dcp_id), "the profile is found in the moved library");
    assert!(std::path::Path::new(&s.dcp_profiles[0].file).starts_with(&moved), "{}", s.dcp_profiles[0].file);
    assert!(s.media.camera_profile_of(s.catalog.photo(id).unwrap()).is_some());
    assert!(s.profile_favorites.contains(&dcp_id) && s.profile_recent.contains(&dcp_id));
    s.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|i| (a[i] - b[i]).abs()).sum()
}

/// A smart preview keeps the file's own colour: Build Smart Previews never develops it with the photo's imported
/// camera profile, nor takes a cached source decoded with it. Before, only the stale-proxy refresh left the profile out.
#[test]
fn smart_previews_are_built_without_the_imported_camera_profile() {
    use crate::media::{DecodedSource, SourceLevel};
    let dir = temp_dir("smart");
    let (mut s, id, _) = library_with_profile(&dir);
    let p = (**s.catalog.photo(id).unwrap()).clone();
    s.execute("library.smartPreviews", &json!({"ids": [id.0], "discard": true})).unwrap();
    // a cached preview decoded with the profile (flat magenta, so it shows if the proxy is cut from it)
    let tag = s.media.camera_profile_of(&p).map(|c| c.0);
    let mut magenta = lightcraft_raster::Rgb32f::new(8, 8);
    magenta.data.iter_mut().for_each(|px| *px = [1.0, 0.0, 1.0]);
    s.media.insert_source(id, SourceLevel::Preview, DecodedSource { camera_profile: tag, ..DecodedSource::new(std::sync::Arc::new(magenta), None) });
    let r = s.execute("library.smartPreviews", &json!({"ids": [id.0]})).unwrap();
    assert_eq!((r["built"].as_u64(), r["repaired"].as_u64()), (Some(1), Some(0)), "{r}");
    let smart_dir = s.media.smart_dir.clone().unwrap();
    let proxy = crate::smart::load(&crate::smart::existing_file_path(&smart_dir, crate::smart::sharded_file_path(&smart_dir, &p))).unwrap();
    let got = mean_rgb(&proxy.image);
    let dng = std::fs::read(dir.join("files").join("IMG_0003.dng")).unwrap();
    let edge = SourceLevel::Preview.max_edge();
    let plain = mean_rgb(&crate::files::load_bytes(&dng, edge).unwrap().0);
    let with = mean_rgb(&crate::files::load_bytes_with(&dng, edge, Some(&swapping_profile("Swap"))).unwrap().0);
    assert!(distance(got, plain) < distance(got, with), "the proxy has the file's own colour: {got:?}, plain {plain:?}, profiled {with:?}");
    // nor from the file developed with the profile
    s.media.forget(id);
    s.execute("library.smartPreviews", &json!({"ids": [id.0], "discard": true})).unwrap();
    s.execute("library.smartPreviews", &json!({"ids": [id.0]})).unwrap();
    let proxy = crate::smart::load(&crate::smart::existing_file_path(&smart_dir, crate::smart::sharded_file_path(&smart_dir, &p))).unwrap();
    let got = mean_rgb(&proxy.image);
    assert!(distance(got, plain) < distance(got, with), "the proxy has the file's own colour: {got:?}, plain {plain:?}, profiled {with:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// While the original is offline, a cached smart preview stands in for a photo developed with an imported camera
/// profile: it has no profile and decoding it again gives the same picture. Before, it was decoded again for every
/// render. Once the original is back, the photo is decoded from it with its profile.
#[test]
fn a_cached_smart_preview_stands_in_while_the_original_is_offline() {
    use crate::media::{DecodedSource, SourceLevel, SourceRef};
    let dir = temp_dir("proxy-offline");
    let (mut s, id, _) = library_with_profile(&dir);
    let p = (**s.catalog.photo(id).unwrap()).clone();
    assert!(s.media.camera_profile_of(&p).is_some());
    let proxy = DecodedSource { from_proxy: true, ..DecodedSource::new(std::sync::Arc::new(lightcraft_raster::Rgb32f::new(8, 8)), None) };
    s.media.insert_source(id, SourceLevel::Preview, proxy);
    let offline = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let probe_offline = offline.clone();
    s.media.availability.set_probe(std::sync::Arc::new(move |path: &str| {
        !(path.ends_with("IMG_0003.dng") && probe_offline.load(std::sync::atomic::Ordering::Relaxed)) && std::path::Path::new(path).exists()
    }));
    assert!(matches!(s.media.source_ref(&p, SourceLevel::Preview), SourceRef::Loaded(a) if a.from_proxy), "the proxy stands in");
    assert!(matches!(s.media.source_ref(&p, SourceLevel::Thumb), SourceRef::Fit { .. }), "the thumbnail is cut from it");
    offline.store(false, std::sync::atomic::Ordering::Relaxed);
    assert!(!matches!(s.media.source_ref(&p, SourceLevel::Preview), SourceRef::Loaded(_)), "the original is back: decoded with the profile");
    assert!(!matches!(s.media.source_ref(&p, SourceLevel::Thumb), SourceRef::Fit { .. }));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The decoder facts reported for a photo come from a cached source developed with the camera profile it uses now;
/// before, a thumbnail decoded with another profile (or none) answered first.
#[test]
fn source_facts_come_from_a_source_developed_with_the_current_camera_profile() {
    use crate::media::{DecodedSource, SourceLevel};
    let dir = temp_dir("facts");
    let (mut s, id, _) = library_with_profile(&dir);
    let tag = s.media.camera_profile_of(s.catalog.photo(id).unwrap()).map(|c| c.0);
    let img = std::sync::Arc::new(lightcraft_raster::Rgb32f::new(4, 4));
    let facts = |temp: f64, camera_profile| DecodedSource {
        camera_profile,
        ..DecodedSource::new(img.clone(), Some(lightcraft_pipeline::SourceInfo { as_shot_temp: temp, ..Default::default() }))
    };
    s.media.insert_source(id, SourceLevel::Thumb, facts(3100.0, None));
    s.media.insert_source(id, SourceLevel::Preview, facts(6200.0, tag));
    assert_eq!(s.source_info(id).as_shot_temp, 6200.0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The Before side (and its region renders) decodes with the Before settings' camera profile — here none, the
/// import defaults — not with the edit's: before, it reused or decoded a source developed with the edit's profile.
#[test]
fn the_before_side_decodes_with_its_own_camera_profile() {
    use crate::media::{DecodedSource, SourceLevel};
    let dir = temp_dir("before");
    let (mut s, id, _) = library_with_profile(&dir);
    let camera = |job: &crate::media::RenderJob| match &job.source {
        SourceRef::File { camera_profile, .. } => camera_profile.as_ref().map(|c| c.0.clone()),
        SourceRef::Loaded(a) => a.camera_profile.clone(),
        _ => None,
    };
    s.media.forget(id);
    let after = s.render_job(id, 256, 256, false, true).unwrap();
    let tag = camera(&after);
    assert!(tag.is_some(), "the edit develops with the imported profile");
    let before = s.render_job(id, 256, 256, true, true).unwrap();
    assert_eq!(camera(&before), None, "the Before side develops with the file's own colour");
    // nor does a cached source developed with the edit's profile stand in for it
    for level in [SourceLevel::Thumb, SourceLevel::Preview, SourceLevel::Full] {
        let img = std::sync::Arc::new(lightcraft_raster::Rgb32f::new(4, 4));
        s.media.insert_source(id, level, DecodedSource { camera_profile: tag.clone(), ..DecodedSource::new(img, None) });
    }
    let before = s.render_job(id, 256, 256, true, true).unwrap();
    assert!(!matches!(before.source, SourceRef::Loaded(_)), "the Before side is decoded afresh");
    assert_eq!(camera(&before), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A profile whose copy into the library fails is reported as failed on its own: the profiles imported with
/// it stay imported (and are saved), and the command does not fail.
#[test]
fn a_profile_that_cannot_be_copied_fails_on_its_own() {
    let dir = temp_dir("copy-fails");
    let (lib, files) = (dir.join("lib"), dir.join("files"));
    std::fs::create_dir_all(&files).unwrap();
    for name in ["A", "B"] {
        std::fs::write(files.join(format!("{name}.dcp")), write_dcp(&swapping_profile(name)).unwrap()).unwrap();
    }
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    // a folder where B's copy goes: writing it fails
    let blocked = lib.join("Profiles").join(format!("{}.dcp", crate::presets::slug("LightCraft Test Camera-B")));
    std::fs::create_dir_all(&blocked).unwrap();
    let r = s.execute("profile.import", &json!({"paths": [files.to_string_lossy()]})).unwrap();
    let imported: Vec<&str> = r["imported"].as_array().unwrap().iter().filter_map(|p| p["name"].as_str()).collect();
    assert_eq!(imported, ["A"], "{r}");
    let failed = r["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 1, "{r}");
    assert!(failed[0][0].as_str().is_some_and(|f| f.ends_with("B.dcp")), "{r}");
    assert_eq!(s.dcp_profiles.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["A"]);
    assert!(blocked.is_dir(), "the folder in the way is left alone");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file that is neither a `.cube`, a `.dcp` nor a `.zip` is reported as failed instead of being passed over.
#[test]
fn importing_an_unsupported_profile_file_reports_it() {
    let dir = temp_dir("unsupported");
    let notes = dir.join("notes.txt");
    std::fs::write(&notes, "not a profile").unwrap();
    let mut s = Session::new();
    let r = s.execute("profile.import", &json!({"paths": [notes.to_string_lossy()]})).unwrap();
    assert_eq!(r["imported"].as_array().map(Vec::len), Some(0), "{r}");
    assert_eq!(r["failed"], json!([["notes", "not a .cube, .dcp or .zip file"]]), "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A library copy that can't be removed keeps its profile: the command fails and nothing is forgotten, so the
/// library never holds a profile file it no longer lists (CodeRabbit on #1063).
#[test]
fn a_profile_whose_copy_cannot_be_deleted_stays() {
    let dir = temp_dir("delete-fails");
    let (mut s, _, dcp_id) = library_with_profile(&dir);
    let file = s.dcp_profiles.iter().find(|p| p.id == dcp_id).map(|p| PathBuf::from(&p.file)).unwrap();
    // a folder in the copy's place: removing it as a file fails (as a file in use does)
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir_all(&file).unwrap();
    let r = s.execute("profile.deleteCamera", &json!({"id": dcp_id}));
    assert!(r.is_err(), "{r:?}");
    assert!(s.dcp_profiles.iter().any(|p| p.id == dcp_id), "the profile is still listed");
    // gone already: deleting just forgets it
    std::fs::remove_dir_all(&file).unwrap();
    s.execute("profile.deleteCamera", &json!({"id": dcp_id})).unwrap();
    assert!(s.dcp_profiles.iter().all(|p| p.id != dcp_id));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A listed path that climbs out of the profiles folder with `..` is not the library's copy: deleting the profile
/// leaves that file alone (CodeRabbit on #1063).
#[test]
fn deleting_a_profile_never_follows_dot_dot_out_of_the_profiles_folder() {
    let dir = temp_dir("delete-dotdot");
    let (mut s, _, dcp_id) = library_with_profile(&dir);
    let entry = s.dcp_profiles.iter_mut().find(|p| p.id == dcp_id).unwrap();
    let profiles = PathBuf::from(&entry.file).parent().unwrap().to_path_buf();
    let outside = profiles.parent().unwrap().join("keep.txt");
    std::fs::write(&outside, b"the user's").unwrap();
    entry.file = profiles.join("..").join("keep.txt").to_string_lossy().into_owned();
    s.execute("profile.deleteCamera", &json!({"id": dcp_id})).unwrap();
    assert!(s.dcp_profiles.iter().all(|p| p.id != dcp_id), "forgotten");
    assert!(outside.exists(), "the file outside the profiles folder stays");
    let _ = std::fs::remove_dir_all(&dir);
}

/// CodeRabbit on #1063: a camera profile whose library copy is missing or damaged when the library opens stayed in the
/// profile browser and `develop.profile` accepted it, and the photo then silently developed with the raw's own
/// colour. It is no longer offered or accepted; its entry stays, and once the file is back it is offered again.
#[test]
fn a_damaged_camera_profile_is_not_offered_until_its_file_is_back() {
    let dir = temp_dir("damaged");
    let (mut s, _id, dcp_id) = library_with_profile(&dir);
    let file = std::path::PathBuf::from(&s.dcp_profiles[0].file);
    s.persist().unwrap();
    s.close_library().unwrap();
    drop(s);
    let good = std::fs::read(&file).unwrap();
    std::fs::write(&file, b"not a profile").unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(dir.join("lib"), false).unwrap();
    assert_eq!(s.dcp_profiles.len(), 1, "the entry stays");
    assert!(!s.imported_profiles().iter().any(|p| p.0 == dcp_id), "not offered");
    let menu = s.profile_menu().to_string();
    assert!(!menu.contains(&dcp_id), "not in the profile menu: {menu}");
    assert!(s.execute("develop.profile", &json!({"id": dcp_id})).is_err(), "not accepted");
    s.close_library().unwrap();
    drop(s);
    std::fs::write(&file, good).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(dir.join("lib"), false).unwrap();
    assert!(s.imported_profiles().iter().any(|p| p.0 == dcp_id), "offered again");
    s.execute("develop.profile", &json!({"id": dcp_id})).unwrap();
    s.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// CodeRabbit on #1063: a LUT profile group named like the camera profiles' ("Camera Profiles") that doesn't sort last
/// made the profile menu list that group twice, each with the same profiles. It is one group, last.
#[test]
fn a_lut_group_named_camera_profiles_joins_the_camera_profiles_group() {
    let dir = temp_dir("group");
    let (mut s, _id, dcp_id) = library_with_profile(&dir);
    for (id, group) in [("lut:a", crate::cmd::dcp_profiles::DCP_GROUP), ("lut:z", "Zebra")] {
        s.lut_profiles.push(crate::cmd::lut_profiles::LutProfile { id: id.into(), name: id.into(), group: group.into(), file: String::new() });
    }
    let menu = s.profile_menu();
    let groups: Vec<&str> = menu["groups"].as_array().unwrap().iter().filter_map(|g| g["name"].as_str()).collect();
    assert_eq!(groups.iter().filter(|g| **g == crate::cmd::dcp_profiles::DCP_GROUP).count(), 1, "{groups:?}");
    assert_eq!(groups.last(), Some(&crate::cmd::dcp_profiles::DCP_GROUP), "{groups:?}");
    let ids: Vec<&str> = menu["groups"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|g| g["name"] == crate::cmd::dcp_profiles::DCP_GROUP)
        .flat_map(|g| g["profiles"].as_array().unwrap().iter().filter_map(|p| p["id"].as_str()))
        .collect();
    assert!(ids.contains(&"lut:a") && ids.contains(&dcp_id.as_str()), "{ids:?}");
    s.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `.dcp` file larger than the parser accepts is refused while it is read, before the whole file is loaded into
/// memory (CodeRabbit on #1063): the import reports it as failed with its own reason.
#[test]
fn an_oversized_camera_profile_is_refused_while_it_is_read() {
    let dir = temp_dir("oversized");
    let big = dir.join("Big.dcp");
    let mut bytes = write_dcp(&swapping_profile("Big")).unwrap();
    bytes.truncate(4);
    std::fs::write(&big, &bytes).unwrap();
    std::fs::OpenOptions::new().write(true).open(&big).unwrap().set_len(lightcraft_raw::dcp::MAX_DCP_BYTES + 1).unwrap();
    let mut s = Session::new();
    let r = s.execute("profile.import", &json!({"paths": [big.to_string_lossy()]})).unwrap();
    assert_eq!(r["imported"], json!([]), "{r}");
    assert_eq!(r["failed"][0][1], json!("the camera profile is larger than 64 MiB"), "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}
