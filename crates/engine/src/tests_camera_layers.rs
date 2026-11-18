//! A camera profile and a look are two layers (ported from OneSlips, LR-PROF-LAYERS): a camera profile with a look of
//! its own (tone curve, look table) develops with that look under the default look ("As profile"); any other look
//! sits on its calibration only, so two looks are never stacked. Profiles are built in code (none is ever a fixture).

use lcc_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::Session;
use crate::media::LoadOptions;

const MAKE: &str = "TESTMAKE";
const MODEL: &str = "LAYERS1";
const WITH_LOOK: &str = "dcp:layers-test-with-look";
const PLAIN: &str = "dcp:layers-test-plain";
const TABLE: &str = "dcp:layers-test-look-table";

fn profile(name: &str, look: bool) -> lcc_raw::dcp::Dcp {
    let m = lcc_raw::Mat3([[0.7, -0.1, -0.05], [-0.5, 1.3, 0.2], [-0.1, 0.25, 0.65]]);
    let mut color = lcc_raw::ColorData { illuminant: [21, 0], color_matrix: [Some(m), None], ..Default::default() };
    if look {
        color.profile.tone_curve = lcc_raw::profile::ToneCurve::from_tag(&[0.0, 0.0, 0.25, 0.42, 1.0, 1.0]);
    }
    lcc_raw::dcp::Dcp { model: format!("{MAKE} {MODEL}"), name: name.into(), copyright: String::new(), color }
}

/// The two test profiles in the decoder's list of imported ones (beside whatever other tests put there).
fn install() -> std::sync::MutexGuard<'static, ()> {
    let lock = crate::camera_profiles::test_library_lock();
    let mut list: Vec<(String, lcc_raw::dcp::Dcp)> =
        lcc_raw::dcp::library().into_iter().filter(|(k, _)| k != WITH_LOOK && k != PLAIN && k != TABLE).map(|(k, d)| (k, (*d).clone())).collect();
    list.push((WITH_LOOK.into(), profile("With Look", true)));
    list.push((PLAIN.into(), profile("Plain", false)));
    list.push((TABLE.into(), table_profile()));
    lcc_raw::dcp::install_library(list);
    lock
}

/// A tiny RGGB DNG of the test camera.
fn dng() -> Vec<u8> {
    let (w, h) = (64usize, 48usize);
    let mut raw = lcc_raw::RawImage {
        format: lcc_raw::RawFormat::Dng,
        width: w,
        height: h,
        cpp: 1,
        data: lcc_raw::RawData::U16((0..w * h).map(|i| 1000 + (i % 97) as u16 * 40).collect()),
        cfa: lcc_raw::Cfa::bayer("RGGB"),
        bits: 14,
        black: lcc_raw::BlackLevel::uniform(512.0),
        white: vec![16000.0],
        active_area: lcc_raw::Rect::new(0, 0, w, h),
        crop: lcc_raw::Rect::new(0, 0, w, h),
        orientation: lcc_raw::Orientation::Normal,
        color: lcc_raw::ColorData {
            illuminant: [21, 0],
            color_matrix: [Some(lcc_raw::Mat3([[0.8, -0.2, -0.05], [-0.45, 1.3, 0.15], [-0.08, 0.22, 0.62]])), None],
            as_shot_neutral: Some([0.5, 1.0, 0.7]),
            ..Default::default()
        },
        wb_multipliers: None,
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: Default::default(),
        metadata: Default::default(),
        lens_metadata: Default::default(),
    };
    raw.metadata.make = Some(MAKE.into());
    raw.metadata.model = Some(MODEL.into());
    lcc_raw::write_dng(&raw, &Default::default()).unwrap()
}

fn raw_photo(s: &mut Session) -> PhotoId {
    let id = s.catalog.alloc_photo_id();
    let mut p = Photo::new(id, Source::File { path: "/nonexistent/LAYERS.DNG".into() }, "LAYERS.DNG", "DNG", 64, 48, "2026-01-01");
    p.kind = MediaKind::Raw;
    p.meta.camera = format!("{MAKE} {MODEL}");
    s.commit("Add", Op::AddPhoto { photo: Box::new(p) }).unwrap();
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    id
}

#[test]
fn a_look_over_a_camera_profile_with_its_own_look_decodes_with_its_calibration_only() {
    let _lock = install();
    let bytes = dng();
    let decode = |look: bool| {
        let options = LoadOptions { camera: WITH_LOOK.into(), camera_look: look, ..Default::default() };
        crate::files::load_bytes_with(&bytes, 64, options).unwrap().1
    };
    assert!(decode(true).camera_tone.is_some(), "As profile: the profile's own tone curve");
    assert!(decode(false).camera_tone.is_none(), "under another look: the calibration only, no tone curve stacked under it");
    assert!(profile("x", true).has_look() && !profile("x", false).has_look());
    let mut identity = profile("x", false);
    identity.color.profile.tone_curve = lcc_raw::profile::ToneCurve::from_tag(&[0.0, 0.0, 1.0, 1.0]);
    assert!(!identity.has_look(), "an identity tone curve is no look");
}

#[test]
fn choosing_a_camera_profile_with_a_look_is_as_profile_and_a_look_keeps_its_calibration() {
    let _lock = install();
    let mut s = Session::new();
    let id = raw_photo(&mut s);
    s.execute("develop.profile", &json!({"id": "lc.vivid"})).unwrap();
    // a camera profile with a look of its own comes with it: the look goes back to the default
    s.execute("develop.profile", &json!({"id": WITH_LOOK})).unwrap();
    let p = s.catalog.photo(id).unwrap().clone();
    assert_eq!((p.develop.profile.camera.as_str(), p.develop.profile.id.as_str()), (WITH_LOOK, lcc_develop::DEFAULT_LOOK));
    assert!(crate::camera_profiles::as_profile(&p));
    assert!(LoadOptions::for_photo(&p).camera_look, "decoded with the profile's look");
    // a look of the app's keeps the camera profile as the calibration, without its look
    s.execute("develop.profile", &json!({"id": "lc.vivid"})).unwrap();
    let p = s.catalog.photo(id).unwrap().clone();
    assert_eq!((p.develop.profile.camera.as_str(), p.develop.profile.id.as_str()), (WITH_LOOK, "lc.vivid"));
    assert!(!crate::camera_profiles::as_profile(&p));
    assert!(!LoadOptions::for_photo(&p).camera_look, "the calibration only");
    // a camera profile without a look keeps the look, and a change of look never decodes it again
    s.execute("develop.profile", &json!({"id": PLAIN})).unwrap();
    let p = s.catalog.photo(id).unwrap().clone();
    assert_eq!((p.develop.profile.camera.as_str(), p.develop.profile.id.as_str()), (PLAIN, "lc.vivid"));
    assert!(LoadOptions::for_photo(&p).camera_look);
    assert!(s.execute("develop.profile", &json!({"id": "dcp:no-such-profile"})).is_err());

    // settings from before the two layers keep their look on the default: an old edit, the camera
    // profile chosen with develop.cameraProfile, the look left at the default
    let mut s = Session::new();
    let id = raw_photo(&mut s);
    s.execute("develop.cameraProfile", &json!({"id": WITH_LOOK})).unwrap();
    let p = s.catalog.photo(id).unwrap().clone();
    assert_eq!(p.develop.profile.id, lcc_develop::DEFAULT_LOOK);
    assert!(crate::camera_profiles::as_profile(&p), "renders as it did: with the profile's own look");
    assert!(LoadOptions::for_photo(&p).camera_look);
}

/// An Adobe Standard-style profile: a matrix and a look table, no tone curve.
fn table_profile() -> lcc_raw::dcp::Dcp {
    let mut d = profile("Look Table", false);
    d.color.profile.look_table = lcc_raw::profile::HsvTable::from_tags(&[1, 2, 1], &[0.0, 1.0, 1.0, 0.0, 1.0, 1.0], 0);
    assert!(d.color.profile.look_table.is_some() && d.has_look());
    d
}

/// A DNG of the test camera with a camera JPEG: a smooth scene developed through the file's own matrix and a bright,
/// contrasty camera curve (`1 - e^(-3y)`), so the DNG tone fit learns a tone well away from the default one.
fn dng_with_camera_jpeg() -> Vec<u8> {
    use lcc_color::{REC2020, SRGB, luminance_2020, transfer::linear_to_srgb};
    let (w, h) = (384usize, 256usize);
    let data = (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as f32, (i / w) as f32);
            let v = 0.02 + 0.2 * (0.5 + 0.5 * (x * 0.045).sin() * (y * 0.06).cos()) + 0.1 * x / w as f32;
            let colour = match (i / w % 2, i % w % 2) {
                (0, 0) => 1.2,
                (1, 1) => 0.8,
                _ => 1.0,
            };
            256 + (v * colour * 15744.0) as u16
        })
        .collect();
    let bytes = crate::tests_xmp::synthetic_dng_of(w, h, "RGGB", data, None, Default::default());
    let mut raw = lcc_raw::decode(&bytes).unwrap();
    raw.metadata.make = Some(MAKE.into());
    raw.metadata.model = Some(MODEL.into());
    raw.color = lcc_raw::ColorData {
        illuminant: [21, 0],
        color_matrix: [Some(REC2020.from_xyz()), None],
        as_shot_neutral: Some([1.0; 3]),
        ..Default::default()
    };
    let t = lcc_raw::color::camera_transform(&raw, lcc_raw::color::as_shot_white_xy(&raw));
    let developed = raw.develop(lcc_raw::Method::Bilinear).unwrap();
    let to_srgb = REC2020.to_space(&SRGB);
    let pixels: Vec<u8> = developed
        .data
        .iter()
        .flat_map(|p| {
            let p = t.matrix.apply_f32(std::array::from_fn(|i| p[i] * t.wb[i]));
            let y = luminance_2020(p).max(1e-6);
            let gain = (1.0 - (-3.0 * y).exp()) / y;
            to_srgb.apply_f32(p.map(|v| v * gain)).map(|v| (255.0 * linear_to_srgb(v).clamp(0.0, 1.0)).round() as u8)
        })
        .collect();
    let image = lcc_codecs::EncodeImage::new(w as u32, h as u32, 3, lcc_codecs::Samples::U8(&pixels));
    let jpeg = lcc_codecs::encode_jpeg(&image, 98, lcc_codecs::ChromaSubsampling::S444, &Default::default()).unwrap();
    let mut file = lcc_raw::write_dng(&raw, &Default::default()).unwrap();
    // a preview IFD after IFD0 of the little-endian DNG written above
    let root = u32::from_le_bytes(file[4..8].try_into().unwrap()) as usize;
    let count = u16::from_le_bytes(file[root..root + 2].try_into().unwrap()) as usize;
    let next = root + 2 + 12 * count;
    let start = file.len() as u32;
    file[next..next + 4].copy_from_slice(&start.to_le_bytes());
    let jpeg_at = start + 2 + 4 * 12 + 4;
    file.extend(4u16.to_le_bytes());
    for (tag, value) in [(256u16, w as u32), (257, h as u32), (0x201, jpeg_at), (0x202, jpeg.len() as u32)] {
        file.extend(tag.to_le_bytes());
        file.extend(4u16.to_le_bytes());
        file.extend(1u32.to_le_bytes());
        file.extend(value.to_le_bytes());
    }
    file.extend(0u32.to_le_bytes());
    file.extend(&jpeg);
    file
}

fn camera_tone_with(bytes: &[u8], camera: &str, look: bool) -> Option<lcc_pipeline::tone::CameraTone> {
    let options = LoadOptions { camera: camera.into(), camera_look: look, ..Default::default() };
    crate::files::load_bytes_with(bytes, 96, options).unwrap().1.camera_tone
}

/// A camera profile without a tone curve of its own (a chart-made matrix; also one with a look table, as
/// OneSlips decides) keeps the tone the photo has without it: here the tone fitted to the file's camera JPEG, not
/// the flat default tone. A profile with a tone curve of its own still renders with that curve.
#[test]
fn a_camera_profile_without_a_tone_curve_keeps_the_photos_own_tone() {
    let _lock = install();
    let bytes = dng_with_camera_jpeg();
    let own = camera_tone_with(&bytes, "lc", true).expect("without a profile: the tone fitted to the camera JPEG");
    let camera = 1.0 - (-3.0f32 * 0.18).exp();
    assert!((own.apply(0.18) - camera).abs() < 0.05, "the fit learnt the camera's curve: {} vs {camera}", own.apply(0.18));
    let default = lcc_pipeline::tone::CameraTone::default_raw().unwrap();
    assert!((own.apply(0.18) - default.apply(0.18)).abs() > 0.02, "the camera's tone is not the default one");
    assert_eq!(camera_tone_with(&bytes, PLAIN, true), Some(own), "a matrix-only profile keeps the photo's tone");
    assert_eq!(camera_tone_with(&bytes, TABLE, true), Some(own), "a look table is colour, not tone");
    assert_eq!(camera_tone_with(&bytes, TABLE, false), Some(own), "under another look: the calibration and the photo's tone");
    // a profile with its own curve keeps it (As profile), and under another look its calibration only
    let curve = profile("x", true).color.profile.tone_curve.unwrap();
    let with_look = camera_tone_with(&bytes, WITH_LOOK, true).expect("the profile's own curve");
    assert_eq!(Some(with_look), crate::files::dng_tone_curve(&curve));
    assert_eq!(camera_tone_with(&bytes, WITH_LOOK, false), None);
    // the colour still comes from the profile
    let options = LoadOptions { camera: PLAIN.into(), ..Default::default() };
    let colour = crate::files::load_bytes_with(&bytes, 96, options).unwrap().1.camera_color.map(|c| c.tags.color_matrix);
    assert_eq!(colour, Some(profile("x", false).color.color_matrix));
    // `photo.sourceReport` names the kept tone
    let report = crate::source_report::report(&bytes, PLAIN).unwrap();
    assert_eq!((report["tone"].as_str(), report["keptTone"].as_str()), (Some("withoutProfile"), Some("dngToneFit")), "{report}");
    // the render's contrast (luma standard deviation, 8-bit): the photo's tone against the default tone the profile
    // got before
    let options = LoadOptions { camera: PLAIN.into(), ..Default::default() };
    let (img, info) = crate::files::load_bytes_with(&bytes, 384, options).unwrap();
    let spread = |info: &lcc_pipeline::SourceInfo| {
        let s = lcc_develop::DevelopSettings::for_raw(info.as_shot_temp, info.as_shot_tint);
        let out = lcc_pipeline::render(&img, info, &s, &lcc_pipeline::RenderRequest::fit(384, 256)).image;
        let luma: Vec<f64> = out.data.iter().map(|p| 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2])).collect();
        let mean = luma.iter().sum::<f64>() / luma.len() as f64;
        (luma.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / luma.len() as f64).sqrt()
    };
    let kept = spread(&info);
    let flat = spread(&lcc_pipeline::SourceInfo { camera_tone: None, ..info.clone() });
    eprintln!("luma standard deviation under the matrix-only profile: {kept:.2} (default tone before: {flat:.2})");
    assert!(kept > flat * 1.05, "the photo's tone keeps more contrast: {kept} vs {flat}");

    // An imported photo carries its camera profile in its settings (`develop.profile.camera`, from the import's
    // camera-profile choice); its grid and loupe renders decode with the photo's own tone under a matrix-only profile.
    let dir = std::env::temp_dir().join(format!("lc-camera-layers-import-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("CHART0001.DNG");
    std::fs::write(&path, &bytes).unwrap();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [path.to_string_lossy()], "cameraProfile": {"all": PLAIN}})).unwrap();
    let id = PhotoId(r["imported"][0].as_u64().unwrap());
    let p = s.catalog.photo(id).unwrap().clone();
    assert_eq!(p.develop.profile.camera, PLAIN, "the profile is in the photo's settings");
    assert_eq!(LoadOptions::for_photo(&p).camera, PLAIN);
    for (what, job) in [("grid", s.thumb_job(id, 128)), ("loupe", s.render_job(id, 400, 300, false, true))] {
        let source = job.unwrap().source.load_source().unwrap();
        let tone = source.info.as_ref().and_then(|i| i.camera_tone).or(source.camera_tone);
        assert_eq!(tone, Some(own), "{what}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
