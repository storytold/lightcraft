/// Keywords from a file keep their hierarchy: the paths of `lr:hierarchicalSubject`, not the
/// flat names of `dc:subject` that LightCraft's exports (and Lightroom Classic) write besides
/// them; an exported `travel|Italy|Rome` comes back as itself.
#[test]
fn a_lens_without_a_name_is_shown_by_its_range() {
    // a Nikon DSLR raw: no LensModel, the range from the maker note
    let m = lightcraft_meta::Metadata { lens_spec: Some([18.0, 135.0, 3.5, 5.6]), ..Default::default() };
    assert_eq!(super::meta_of(&m).0.lens, "18.0-135.0 mm f/3.5-5.6");
    let named = lightcraft_meta::Metadata {
        lens_model: Some("NIKKOR Z 24-70mm f/4 S".into()),
        lens_spec: Some([24.0, 70.0, 4.0, 4.0]),
        ..Default::default()
    };
    assert_eq!(super::meta_of(&named).0.lens, "NIKKOR Z 24-70mm f/4 S");
    assert_eq!(super::meta_of(&lightcraft_meta::Metadata::default()).0.lens, "");
}

#[test]
fn imported_keywords_keep_their_hierarchy() {
    let m = lightcraft_meta::Metadata {
        keywords: vec!["Rome".into(), "Italy".into(), "travel".into(), "beach".into()],
        hierarchical_keywords: vec!["travel|Italy|Rome".into()],
        ..Default::default()
    };
    assert_eq!(super::meta_of(&m).0.keywords, ["travel|Italy|Rome", "beach"]);
}

#[test]
fn psd_probe_reads_embedded_capture_time_gps_and_title() {
    let capture = lightcraft_meta::DateTime::parse_iso("2023-05-14T14:40:39").unwrap();
    let metadata = lightcraft_meta::Metadata {
        capture_time: Some(capture),
        gps: Some(lightcraft_meta::Gps { latitude: 45.0, longitude: 7.0, altitude: None }),
        title: Some("PSD metadata".into()),
        ..Default::default()
    };
    let bytes = crate::tests_xmp::synthetic_psd_with_metadata(&metadata);
    let probe = probe_bytes("metadata.psd", &bytes).unwrap();
    assert_eq!(probe.captured.as_deref(), Some("2023-05-14T14:40:39"));
    assert_eq!(probe.meta.gps, Some((45.0, 7.0)));
    assert_eq!(probe.meta.title, "PSD metadata");
}

use super::*;
use lightcraft_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};

/// Which white-balance scale a raw gets (issue #730): a file with matrices of its own is absolute from them; a
/// fitted-look file is absolute through its camera's measured matrices, which only read the as-shot
/// multipliers as Kelvin; any other raw stays relative (6500 / 0 = as shot). The catalog's decision from the
/// format and camera string agrees.
#[test]
fn white_balance_scale_follows_the_files_matrices_then_the_cameras() {
    use lightcraft_raw::{ColorData, RawFormat};
    let meta = |make: &str, model: &str| lightcraft_meta::Metadata { make: Some(make.into()), model: Some(model.into()), ..Default::default() };
    let mul = Some([2.2f32, 1.0, 1.6]);
    let d65 = lightcraft_color::D65;
    // a covered camera: Kelvin through the spectral matrices, near the daylight these multipliers describe
    let a7m3 = WbScaleOf::new(RawFormat::Arw, &ColorData::default(), mul, &meta("SONY", "ILCE-7M3"));
    assert!(!a7m3.own_matrix && !a7m3.relative && a7m3.legacy_relative && a7m3.scale_white.is_some());
    let (t, tint) = a7m3.as_shot(d65);
    assert!((3500.0..7000.0).contains(&t) && t != 6500.0 && tint.abs() < 30.0 && t.fract() == 0.0 && tint.fract() == 0.0, "{t} {tint}");
    assert!(!lightcraft_catalog::relative_wb_camera("ARW", "SONY ILCE-7M3"));
    // the same camera without multipliers: the scale is Kelvin still, the white falls back to the tags' (D65)
    let bare = WbScaleOf::new(RawFormat::Arw, &ColorData::default(), None, &meta("SONY", "ILCE-7M3"));
    assert!(bare.legacy_relative && bare.scale_white.is_none());
    assert_eq!(bare.as_shot(d65), {
        let (t, tint) = xy_to_temp_tint(d65);
        (t.round(), tint.round())
    });
    // a camera without measured matrices: relative, whatever the multipliers
    let a7m2 = WbScaleOf::new(RawFormat::Arw, &ColorData::default(), mul, &meta("SONY", "ILCE-7M2"));
    assert!(a7m2.relative && !a7m2.legacy_relative && a7m2.scale_white.is_none());
    assert_eq!(a7m2.as_shot(d65), (6500.0, 0.0));
    assert!(lightcraft_catalog::relative_wb_camera("ARW", "SONY ILCE-7M2"));
    // a camera without matrices whose presets give the scale: Kelvin through them, Sony's daylight preset as
    // 5500 K or so; without presets in the file the scale is Kelvin still, the white the tags' (D65)
    let g = |r: f32, b: f32| [r / 1024.0, 1.0, b / 1024.0];
    let presets = lightcraft_raw::kelvin_presets::KelvinPresets::new([
        (2500.0, g(1336.0, 3672.0)),
        (3200.0, g(1720.0, 2620.0)),
        (4500.0, g(2256.0, 1880.0)),
        (6000.0, g(2680.0, 1544.0)),
        (8500.0, g(3136.0, 1300.0)),
    ]);
    let with = ColorData { kelvin_presets: presets, ..Default::default() };
    let a99m2 = WbScaleOf::new(RawFormat::Arw, &with, Some(g(2500.0, 1668.0)), &meta("SONY", "ILCA-99M2"));
    assert!(!a99m2.own_matrix && !a99m2.relative && a99m2.legacy_relative && a99m2.scale_white.is_some());
    let (t, tint) = a99m2.as_shot(d65);
    assert!((5000.0..6000.0).contains(&t) && tint.abs() < 10.0 && t.fract() == 0.0, "{t} {tint}");
    assert!(!lightcraft_catalog::relative_wb_camera("ARW", "SONY ILCA-99M2"));
    // a Canon body without matrices, through the presets its ColorData lists (PowerShot G1 X Mark III, a
    // Daylight shot with the camera's white balance shifted towards blue: below Daylight's 5651 K)
    let canon = lightcraft_raw::kelvin_presets::KelvinPresets::from_unordered([
        (5651.0, g(1418.0, 1357.0)),
        (7505.0, g(1653.0, 1171.0)),
        (6594.0, g(1533.0, 1259.0)),
        (3174.0, g(963.0, 2016.0)),
        (8529.0, g(1807.0, 1080.0)),
    ]);
    let with_canon = ColorData { kelvin_presets: canon, ..Default::default() };
    let g1x3 = WbScaleOf::new(RawFormat::Cr2, &with_canon, Some(g(1264.0, 1537.0)), &meta("Canon", "Canon PowerShot G1 X Mark III"));
    assert!(!g1x3.relative && g1x3.scale_white.is_some());
    let (t, tint) = g1x3.as_shot(d65);
    assert!((4300.0..5200.0).contains(&t) && tint.abs() < 10.0, "{t} {tint}");
    assert!(!lightcraft_catalog::relative_wb_camera("CR2", "Canon Canon PowerShot G1 X Mark III"));
    let bare = WbScaleOf::new(RawFormat::Arw, &ColorData::default(), mul, &meta("SONY", "ILCA-99M2"));
    assert!(bare.legacy_relative && bare.scale_white.is_none());
    // presets in a file of a camera not listed: relative, as the catalog decides from the camera alone
    let other = WbScaleOf::new(RawFormat::Arw, &with, mul, &meta("SONY", "ILCE-1"));
    assert!(other.relative && other.scale_white.is_none());
    let unknown = WbScaleOf::new(RawFormat::Nef, &ColorData::default(), mul, &lightcraft_meta::Metadata::default());
    assert!(unknown.relative && unknown.as_shot(d65) == (6500.0, 0.0));
    // a file with matrices of its own (DNG): absolute from them, no matter whose camera
    let own = ColorData { illuminant: [21, 0], color_matrix: [Some(lightcraft_color::Mat3::IDENTITY), None], ..Default::default() };
    let dng = WbScaleOf::new(RawFormat::Dng, &own, mul, &meta("SONY", "ILCE-7M3"));
    assert!(dng.own_matrix && !dng.relative && !dng.legacy_relative && dng.scale_white.is_none());
    let xy = lightcraft_color::cct::temp_tint_to_xy(4200.0, 12.0);
    assert_eq!(dng.as_shot(xy), (4200.0, 12.0));
    assert!(!lightcraft_catalog::relative_wb_camera("DNG", "SONY ILCE-7M3"));
}

/// A Fujifilm X-Trans III body reads Kelvin through the neutral pair its RAF stores (issue #730); without the pair
/// in the file, or on a body with matrices (X-T3), nothing changes.
#[test]
fn a_fujifilm_x_trans_iii_raf_reads_kelvin_through_its_neutral_pair() {
    use lightcraft_raw::{ColorData, RawFormat};
    let meta = |make: &str, model: &str| lightcraft_meta::Metadata { make: Some(make.into()), model: Some(model.into()), ..Default::default() };
    let a = [366.0f32 / 302.0, 1.0, 866.0 / 302.0];
    let d65 = [646.0f32 / 302.0, 1.0, 498.0 / 302.0];
    let pair = lightcraft_raw::kelvin_presets::KelvinPresets::from_neutral_pair(a, d65);
    assert!(pair.is_some());
    let with = ColorData { kelvin_presets: pair, ..Default::default() };
    let xt20 = WbScaleOf::new(RawFormat::Raf, &with, Some(d65), &meta("FUJIFILM", "X-T20"));
    assert!(!xt20.relative && xt20.scale_white.is_some());
    let (t, tint) = xt20.as_shot(lightcraft_color::cct::temp_tint_to_xy(6500.0, 0.0));
    assert!((6000.0..7000.0).contains(&t) && tint.abs() < 25.0, "{t} {tint}");
    assert!(!lightcraft_catalog::relative_wb_camera("RAF", "FUJIFILM X-T20"));
    let bare = WbScaleOf::new(RawFormat::Raf, &ColorData::default(), Some(d65), &meta("FUJIFILM", "X-T20"));
    assert!(bare.scale_white.is_none());
}

/// A merged DNG (Photo Merge) of a raw without colour matrices shows the As Shot temperature and tint its source
/// shows, on every scale: through the measured matrices (ILCE-7M3), through the presets (ILCA-99M2), the relative
/// scale's 6500 K / 0 (ILCE-7M2) and a file's own white (an unfitted format). Merges of real ILCE-7M3 brackets
/// opened at 6113–7307 K / +115 where the ARWs show 3932–5268 K / +7.
#[test]
fn a_merged_dng_shows_the_white_balance_its_source_shows() {
    use lightcraft_raw::{ColorData, RawFormat};
    let meta = |make: &str, model: &str| lightcraft_meta::Metadata { make: Some(make.into()), model: Some(model.into()), ..Default::default() };
    let g = |r: f32, b: f32| [r / 1024.0, 1.0, b / 1024.0];
    let presets = lightcraft_raw::kelvin_presets::KelvinPresets::new([
        (2500.0, g(1336.0, 3672.0)),
        (3200.0, g(1720.0, 2620.0)),
        (4500.0, g(2256.0, 1880.0)),
        (6000.0, g(2680.0, 1544.0)),
        (8500.0, g(3136.0, 1300.0)),
    ]);
    let with_presets = ColorData { kelvin_presets: presets, ..Default::default() };
    let cases = [
        (RawFormat::Arw, ColorData::default(), [2.5039f32, 1.0, 1.5078], meta("SONY", "ILCE-7M3")),
        (RawFormat::Arw, with_presets, g(2500.0, 1668.0), meta("SONY", "ILCA-99M2")),
        (RawFormat::Arw, ColorData::default(), [2.2, 1.0, 1.6], meta("SONY", "ILCE-7M2")),
        (RawFormat::Rw2, ColorData::default(), [2.0, 1.0, 1.4], meta("Panasonic", "DC-G9")),
    ];
    for (format, color, mul, meta) in cases {
        let own = lightcraft_raw::color::as_shot_white_of(&color, Some(mul));
        let source = WbScaleOf::new(format, &color, Some(mul), &meta);
        let t = lightcraft_raw::color::camera_transform_of(&color, own);
        let dng = crate::camera_preview::dng_colour(&color, &source.dng_white(&color, own), &t, None).unwrap();
        let merged = WbScaleOf::new(RawFormat::Dng, &dng, None, &meta);
        let shown = source.as_shot(own);
        assert_eq!(merged.as_shot(lightcraft_raw::color::as_shot_white_of(&dng, None)), shown, "{:?}", meta.model);
    }
}

/// A RAF whose preview pointer is in its header, then a large raw tail the reader must not need.
fn raf_with_embedded_jpeg(jpeg: &[u8], tail: usize) -> Vec<u8> {
    let head = 160;
    let mut f = vec![0u8; head];
    f.get_mut(..16).expect("head").copy_from_slice(b"FUJIFILMCCD-RAW ");
    f.get_mut(84..88).expect("jpeg offset").copy_from_slice(&(head as u32).to_be_bytes());
    f.get_mut(88..92).expect("jpeg length").copy_from_slice(&(jpeg.len() as u32).to_be_bytes());
    f.extend_from_slice(jpeg);
    f.extend(std::iter::repeat_n(0x33u8, tail));
    f
}

/// A raw's preview is read from its header and its JPEG only. Every preview used to cost a
/// whole-file read, which on a 56 MB RAF over a network share is ten times the bytes and ten
/// times the wait (measured: 2.5 s local against 25.6 s over SMB, DSCF9492.RAF). The pixels the
/// quick path produces must equal the whole-file path's, exactly.
#[test]
fn preview_loading_reads_only_the_header_and_the_jpeg() {
    let (w, h) = (48u32, 32u32);
    let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 13 % 251) as u8).collect();
    let img = EncodeImage::new(w, h, 3, Samples::U8(&rgb));
    let jpeg = encode_jpeg(&img, 90, ChromaSubsampling::S420, &EncodeMeta::default()).unwrap();
    let file = raf_with_embedded_jpeg(&jpeg, 200_000);
    let dir = crate::tests_xmp::temp_dir("preview-window");
    let path = dir.join("dscf0001.raf");
    std::fs::write(&path, &file).unwrap();
    let path = path.to_string_lossy().into_owned();
    let max_edge = 512;

    let (header, window, read) = preview_window(&path, max_edge).expect("the file reads").expect("it has a preview");
    assert_eq!(header.len(), lightcraft_raw::PREVIEW_PREFIX, "the header prefix is bounded");
    assert!(read.header_path, "the header path is what answered");
    assert!(!read.planned, "a RAF pointer in the header needs no planner");
    assert_eq!(read.total, file.len() as u64);
    assert!(read.fraction() < 0.5, "the read is a fraction of the file: {read:?}");
    assert!(window.len() < file.len() / 2, "the raw tail must not be read: {} of {} bytes", window.len(), file.len());

    // The quick path and the whole-file path agree byte for byte.
    let quick = quick_image_srgb(&header, &window, read.adobe_rgb, max_edge).expect("the window holds a decodable preview");
    let whole = embedded_preview_srgb(&file, max_edge).expect("the whole file decodes too");
    assert_eq!((quick.width, quick.height), (whole.width, whole.height), "same preview size");
    assert_eq!(quick.data, whole.data, "byte for byte");

    // A file with no locatable preview declines (the caller then reads it whole), a missing file errors.
    let plain = dir.join("plain.jpg");
    std::fs::write(&plain, &jpeg).unwrap();
    assert!(preview_window(&plain.to_string_lossy(), max_edge).expect("a plain file reads").is_none());
    assert!(preview_window(&dir.join("nope.raf").to_string_lossy(), max_edge).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// What a fixture's IFD0 says about itself: the tag that makes `probe` call the file a raw, the
/// file's own orientation, and the EXIF colour space the container declares for its preview.
#[derive(Clone, Copy, Default)]
struct Fixture {
    dng: bool,
    sony: bool,
    orientation: Option<u16>,
    /// EXIF `ColorSpace` (0xA001): 1 = sRGB, 0xFFFF with an "R03" InteropIndex = Adobe RGB.
    color_space: Option<u16>,
}

/// A TIFF-family raw no header field points into: IFD0 is the JPEG-compressed preview strip (the
/// shape a DNG's `PreviewImage`, a CR2's or an ARW's preview takes), IFD1 a CFA "sensor" strip of
/// `tail` bytes. The writer puts each IFD's table after its own chunk, so a `strip` bigger than
/// the plan's bounded header puts IFD0 (and any orientation tag in it) out of that header's reach.
fn tiff_with_preview_strip(strip: &[u8], tail: usize, what: Fixture) -> Vec<u8> {
    tiff_with_preview_strips(strip, &[], tail, what)
}

/// [`tiff_with_preview_strip`] that also keeps the `smaller` JPEGs in sub-IFDs of IFD0, as an ARW keeps its
/// 1616 px preview beside the full-size one.
fn tiff_with_preview_strips(strip: &[u8], smaller: &[&[u8]], tail: usize, what: Fixture) -> Vec<u8> {
    use lightcraft_tiff::{IfdBuilder, ImageData, TiffWriter, Value, tags as t};
    let mut preview = IfdBuilder::new();
    if what.dng {
        preview.set(t::DNG_VERSION, Value::Byte(vec![1, 4, 0, 0]));
    }
    if what.sony {
        preview.set(t::MAKE, Value::Ascii("SONY".into()));
    }
    if let Some(o) = what.orientation {
        preview.set(t::ORIENTATION, Value::Short(vec![o]));
    }
    if let Some(space) = what.color_space {
        let mut exif = IfdBuilder::new();
        exif.set(0xA001, Value::Short(vec![space]));
        if space == 0xFFFF {
            let mut interop = IfdBuilder::new();
            interop.set(0x0001, Value::Undefined(b"R03".to_vec()));
            exif.set_child(t::INTEROP_IFD, interop);
        }
        preview.set_child(t::EXIF_IFD, exif);
    }
    preview.set(t::COMPRESSION, Value::Short(vec![7]));
    preview.set(t::PHOTOMETRIC, Value::Short(vec![t::photometric::RGB]));
    preview.set(t::IMAGE_WIDTH, Value::Long(vec![48]));
    preview.set(t::IMAGE_LENGTH, Value::Long(vec![32]));
    preview.set_image(ImageData::Strips { rows_per_strip: 32, strips: vec![strip.to_vec()] });
    for jpeg in smaller {
        let mut sub = IfdBuilder::new();
        sub.set(t::COMPRESSION, Value::Short(vec![7]));
        sub.set(t::PHOTOMETRIC, Value::Short(vec![t::photometric::RGB]));
        sub.set(t::IMAGE_WIDTH, Value::Long(vec![48]));
        sub.set(t::IMAGE_LENGTH, Value::Long(vec![32]));
        sub.set_image(ImageData::Strips { rows_per_strip: 32, strips: vec![jpeg.to_vec()] });
        preview.add_sub_ifd(sub);
    }
    let mut raw = IfdBuilder::new();
    raw.set(t::COMPRESSION, Value::Short(vec![1]));
    raw.set(t::PHOTOMETRIC, Value::Short(vec![t::photometric::CFA]));
    raw.set(t::IMAGE_WIDTH, Value::Long(vec![6000]));
    raw.set(t::IMAGE_LENGTH, Value::Long(vec![4000]));
    raw.set_image(ImageData::Strips { rows_per_strip: 4000, strips: vec![vec![0x33u8; tail]] });
    TiffWriter::default().write(&[preview, raw]).expect("the fixture writes")
}

/// A raw whose preview only its tags locate — a Canon CR3's JPEG track, a TIFF raw whose preview
/// tag sits past the prefix — is read in part too: `sparse::preview_plan` names the ranges
/// `embedded_preview` reads, so the preview strip is read and the sensor strip is not. The pixels
/// must equal the whole-file path's, exactly; a file the planner cannot describe must decline
/// rather than guess, so the caller keeps reading it whole.
#[test]
fn preview_loading_plans_the_ranges_it_needs() {
    let (w, h) = (48u32, 32u32);
    let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 29 % 241) as u8).collect();
    let img = EncodeImage::new(w, h, 3, Samples::U8(&rgb));
    let jpeg = encode_jpeg(&img, 90, ChromaSubsampling::S420, &EncodeMeta::default()).unwrap();
    let file = tiff_with_preview_strip(&jpeg, 400_000, Fixture { dng: true, ..Default::default() });
    let dir = crate::tests_xmp::temp_dir("preview-plan");
    let path = dir.join("preview.dng");
    std::fs::write(&path, &file).unwrap();
    let path = path.to_string_lossy().into_owned();
    let max_edge = 512;

    let (header, window, read) = preview_window(&path, max_edge).expect("the file reads").expect("the plan locates its preview");
    assert_eq!(header.len(), PLAN_PREFIX as usize, "the plan's header is bounded");
    assert!(read.planned, "the planner is what answered");
    assert!(read.header_path, "and it is still a bounded read");
    assert_eq!(read.total, file.len() as u64);
    assert!(read.fraction() < 0.5, "the read is a fraction of the file: {read:?} of {} bytes", file.len());
    assert!(window.len() < file.len() / 2, "the sensor strip must not be read: {} of {} bytes", window.len(), file.len());

    // The plan path and the whole-file path agree byte for byte.
    let quick = quick_image_srgb(&header, &window, read.adobe_rgb, max_edge).expect("the window holds a decodable preview");
    let whole = embedded_preview_srgb(&file, max_edge).expect("the whole file decodes too");
    assert_eq!((quick.width, quick.height), (whole.width, whole.height), "same preview size");
    assert_eq!(quick.data, whole.data, "byte for byte");

    // The same structure without the tag that makes it a raw: the planner declines, and the
    // caller's whole-file read is what answers (no guessing at what a file might hold).
    let not_a_raw = tiff_with_preview_strip(&jpeg, 400_000, Fixture::default());
    std::fs::write(&path, &not_a_raw).unwrap();
    assert!(preview_window(&path, max_edge).expect("the file reads").is_none(), "an unknown container falls back");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A raw that keeps several JPEGs is shown, on the plan route as on the whole-file path, from the smallest one that
/// covers the box (`embedded_preview_covering`). The plan route used to hand on the largest whatever the size: an
/// A7R VI ARW's 9984 × 6656 JPEG for a 512 px thumbnail instead of its 1616 × 1080 one.
#[test]
fn preview_loading_plans_the_smallest_covering_jpeg() {
    let jpeg = |w: u32, h: u32| {
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 31 % 247) as u8).collect();
        encode_jpeg(&EncodeImage::new(w, h, 3, Samples::U8(&rgb)), 90, ChromaSubsampling::S420, &EncodeMeta::default()).unwrap()
    };
    let (big, small) = (jpeg(96, 64), jpeg(48, 32));
    assert!(big.len() > small.len(), "the full-size JPEG is the largest");
    let file = tiff_with_preview_strips(&big, &[&small], 400_000, Fixture { dng: true, ..Default::default() });
    let dir = crate::tests_xmp::temp_dir("preview-plan-covering");
    let path = dir.join("two-previews.dng");
    std::fs::write(&path, &file).unwrap();
    let path = path.to_string_lossy().into_owned();

    for (edge, want, which) in [(48, &small, "small"), (49, &big, "full-size"), (usize::MAX, &big, "full-size")] {
        let (header, window, read) = preview_window(&path, edge).expect("the file reads").expect("the plan locates its preview");
        assert!(read.planned, "the planner is what answered");
        assert!(&window == want, "a {edge} px box takes the {which} JPEG");
        let quick = quick_image_srgb(&header, &window, read.adobe_rgb, edge).expect("the window holds a decodable preview");
        let whole = embedded_preview_srgb(&file, edge).expect("the whole file decodes too");
        assert_eq!((quick.width, quick.height, quick.data), (whole.width, whole.height, whole.data), "{edge} px: the plan and the whole file agree");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file whose preview the quick path could not show the way the whole-file path does is left to
/// the whole file. Here the orientation tag sits in an IFD0 past the bounded header (its chunk is
/// bigger than that header) and the preview JPEG carries none of its own, so a window read would
/// come out unrotated where the whole-file path rotates it — the plan must decline instead of
/// handing on a wrong picture.
#[test]
fn preview_loading_declines_an_orientation_only_the_whole_file_shows() {
    let (w, h) = (48u32, 32u32);
    let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 17 % 239) as u8).collect();
    let img = EncodeImage::new(w, h, 3, Samples::U8(&rgb));
    let jpeg = encode_jpeg(&img, 90, ChromaSubsampling::S420, &EncodeMeta::default()).unwrap();
    // the strip (what the IFD's chunk covers) is padded past the plan's header, which is what puts
    // IFD0 out of its reach; `embedded_preview` trims back to the JPEG's end-of-image
    let mut strip = jpeg.clone();
    strip.extend(std::iter::repeat_n(0u8, PLAN_PREFIX as usize));
    let file = tiff_with_preview_strip(&strip, 400_000, Fixture { dng: true, orientation: Some(6), ..Default::default() });
    let dir = crate::tests_xmp::temp_dir("preview-plan-orientation");
    let path = dir.join("rotated.dng");
    std::fs::write(&path, &file).unwrap();
    let path = path.to_string_lossy().into_owned();

    assert_eq!(lightcraft_meta::extract(&file).orientation, Some(Orientation::from_exif(6)), "the fixture states an orientation");
    assert!(lightcraft_meta::extract(&file[..PLAN_PREFIX as usize]).orientation.is_none(), "which the bounded header cannot see");
    assert!(lightcraft_meta::extract(&jpeg).orientation.is_none(), "and the preview JPEG carries none of its own");
    assert!(preview_window(&path, usize::MAX).expect("the file reads").is_none(), "the whole-file path keeps it");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A container can declare what its preview JPEG is in, and the whole-file path applies that
/// declaration (`decode_raw_preview`). The planned path reads it too and the quick decode applies
/// it, so a container that declares Adobe RGB (a Sony α1's ARW) is read in part like any other and
/// shows the same pixels as the whole-file path (issue #1440; it used to be read whole) — and one
/// that declares sRGB (what most bodies write) keeps the fast path as before.
#[test]
fn preview_loading_applies_a_container_colour_space() {
    use lightcraft_raw::PreviewColorSpace;
    let (w, h) = (48u32, 32u32);
    let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 23 % 233) as u8).collect();
    let img = EncodeImage::new(w, h, 3, Samples::U8(&rgb));
    let jpeg = encode_jpeg(&img, 90, ChromaSubsampling::S420, &EncodeMeta::default()).unwrap();
    let dir = crate::tests_xmp::temp_dir("preview-plan-space");
    let path = dir.join("space.arw");
    let path = path.to_string_lossy().into_owned();

    let adobe = tiff_with_preview_strip(&jpeg, 400_000, Fixture { sony: true, color_space: Some(0xFFFF), ..Default::default() });
    assert_eq!(lightcraft_raw::embedded_preview_color_space(&adobe), Some(PreviewColorSpace::AdobeRgb), "the fixture declares Adobe RGB");
    std::fs::write(&path, &adobe).unwrap();
    let (header, window, read) = preview_window(&path, usize::MAX).expect("the file reads").expect("the plan answers for Adobe RGB too");
    assert!(read.planned && read.adobe_rgb, "{read:?}");
    assert!(read.fraction() < 0.5, "the sensor strip is not read: {read:?}");
    let quick = quick_image_srgb(&header, &window, read.adobe_rgb, 512).expect("the window decodes");
    let whole = embedded_preview_srgb(&adobe, 512).expect("the whole file decodes too");
    assert_eq!(quick.data, whole.data, "the space is applied as the whole-file path applies it");
    let plain = quick_image_srgb(&header, &window, false, 512).expect("the window decodes");
    assert_ne!(plain.data, whole.data, "and it changes the pixels (the fixture's colours are saturated enough)");

    let srgb = tiff_with_preview_strip(&jpeg, 400_000, Fixture { sony: true, color_space: Some(1), ..Default::default() });
    assert_eq!(lightcraft_raw::embedded_preview_color_space(&srgb), Some(PreviewColorSpace::Srgb), "the fixture declares sRGB");
    std::fs::write(&path, &srgb).unwrap();
    let (_, window, read) = preview_window(&path, usize::MAX).expect("the file reads").expect("sRGB is what the JPEG-only decode assumes");
    assert!(read.planned && !read.adobe_rgb, "the plan still answers");
    assert!(window.starts_with(&[0xff, 0xd8]), "and hands on the JPEG");
}

/// A JPEG with no date in its metadata (an export saved without it) takes the file's date, as
/// in Lightroom, instead of grouping under "Unknown Date"; a dated one keeps its capture time.
#[test]
fn undated_files_take_the_file_date() {
    let dir = std::env::temp_dir().join(format!("lightcraft-undated-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rgb = vec![128u8; 16 * 16 * 3];
    let img = EncodeImage::new(16, 16, 3, Samples::U8(&rgb));
    let jpeg = encode_jpeg(&img, 90, ChromaSubsampling::S420, &EncodeMeta::default()).unwrap();
    let path = dir.join("export.jpg");
    std::fs::write(&path, &jpeg).unwrap();
    let path = path.to_string_lossy().to_string();
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let want = crate::import::civil(mtime as i64);
    let (_, probe) = fs_hooks();
    let info = probe(&path).unwrap();
    assert_eq!(info.captured, None, "the file itself has no date");
    assert_eq!(info.file_modified.as_deref(), Some(want.as_str()));
    let mut s = crate::Session::new().with_fs();
    let r = crate::import::import(&mut s, &[path], crate::import::ImportMode::Add).unwrap();
    let id = lightcraft_catalog::PhotoId(r.imported[0]);
    assert_eq!(s.catalog.photo(id).unwrap().captured.as_deref(), Some(want.as_str()));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The probe opens the file once and says the same thing as before about one that can't be read:
/// its path, then the system's words (a missing file, a folder).
#[test]
fn a_probe_of_a_file_that_cannot_be_read_names_the_path_and_the_cause() {
    let dir = std::env::temp_dir().join(format!("lightcraft-probe-errors-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (_, probe) = fs_hooks();
    for path in [dir.join("no-such-file.jpg"), dir.clone()] {
        let path = path.to_string_lossy().to_string();
        let e = probe(&path).unwrap_err();
        assert_eq!(e, format!("{path}: {}", std::fs::read(&path).unwrap_err()));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #367: probes read headers, not pixels, and report what the decode-based probe did:
/// oriented dimensions (EXIF orientation 6 swaps them), metadata, the error for a truncated
/// file or one that isn't an image.
#[test]
fn image_probe_reads_headers_like_the_decode_did() {
    let (w, h) = (300u32, 200u32);
    let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 7 % 251) as u8).collect();
    let img = EncodeImage::new(w, h, 3, Samples::U8(&rgb));
    for o in [None, Some(1), Some(6), Some(3), Some(8)] {
        let exif = o.map(lightcraft_codecs::exif::minimal_exif);
        let meta = EncodeMeta { exif: exif.as_deref(), ..Default::default() };
        let jpeg = encode_jpeg(&img, 90, ChromaSubsampling::S420, &meta).unwrap();
        let png = lightcraft_codecs::encode_png(&img, &meta).unwrap();
        for (name, bytes) in [("a.jpg", &jpeg), ("a.png", &png)] {
            let p = probe_bytes(name, bytes).unwrap();
            // what the probe computed from a decode before
            let d = lightcraft_codecs::decode(bytes, lightcraft_codecs::DecodeOptions::fit(64, 64)).unwrap();
            let swap = Orientation::from_exif(d.orientation).swaps_axes();
            let want = if swap { (d.source_height, d.source_width) } else { (d.source_width, d.source_height) };
            assert_eq!((p.width, p.height), want, "{name} {o:?}");
            assert_eq!((p.width, p.height), if matches!(o, Some(6 | 8)) { (h, w) } else { (w, h) }, "{name} {o:?}");
            assert_eq!(p.kind, MediaKind::Image);
            assert_eq!(p.format, if name == "a.jpg" { "JPEG" } else { "PNG" });
            assert_eq!(p.file_size, bytes.len() as u64);
            assert!(p.content_hash.is_some());
            assert!(probe_bytes(name, &bytes[..bytes.len() / 2]).is_err(), "{name}: truncated file accepted");
        }
    }
    assert!(probe_bytes("x.jpg", b"not an image").is_err());
}

/// Issue #1212: a transparent PNG develops on white (what renders, previews, exports and contact sheets
/// start from), not on the black its transparent pixels store.
#[test]
fn transparent_png_develops_on_white() {
    let rgba = [200, 40, 40, 255, 0, 0, 0, 0];
    let png = lightcraft_codecs::encode_png(&EncodeImage::new(2, 1, 4, Samples::U8(&rgba)), &EncodeMeta::default()).unwrap();
    let (img, _) = load_bytes(&png, 64).unwrap();
    assert!(img.get(0, 0)[0] > 0.3 && img.get(0, 0)[1] < 0.1, "{:?}", img.get(0, 0));
    assert!(img.get(1, 0).iter().all(|&c| (c - 1.0).abs() < 1e-4), "{:?}", img.get(1, 0));
}

/// Issue #548: a small preview bins the mosaic by 8; specular points that clip a single green sample
/// of a block on a grey surface must not become green speckles (the block's green used to take the
/// clip level, which highlight reconstruction then kept).
#[test]
fn coarse_binning_keeps_specular_points_neutral() {
    let (w, h) = (256usize, 192usize);
    // grey after the file's white balance (as-shot neutral 0.5 : 1 : 0.7), brighter to the right
    let neutral = [0.5f32, 1.0, 0.7];
    let cfa = lightcraft_raw::Cfa::bayer("RGGB").unwrap();
    let mut data: Vec<u16> = (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let v = (0.3 + 0.2 * x as f32 / w as f32) * neutral[cfa.color_at(x, y).min(2) as usize];
            (256.0 + v * (16000.0 - 256.0)).round() as u16
        })
        .collect();
    // one clipped green sample (RGGB: even row, odd column) in every other 8 × 8 block
    for by in 0..h / 8 {
        for bx in (by % 2..w / 8).step_by(2) {
            data[(by * 8 + 4) * w + bx * 8 + 5] = 16000;
        }
    }
    let bytes = crate::tests_xmp::synthetic_dng_of(w, h, "RGGB", data, None, Default::default());
    let raw = lightcraft_raw::decode(&bytes).unwrap();
    assert_eq!(bin_factor(&raw, 32), Some(8));
    let (img, _) = load_bytes(&bytes, 32).unwrap();
    assert_eq!((img.width, img.height), (32, 24));
    let worst = img.data.iter().map(|p| p[1] / ((p[0] + p[2]) / 2.0).max(1e-6)).fold(0.0f32, f32::max);
    assert!(worst < 1.12, "green speckles: green {worst:.2}× the other channels");
}

#[test]
fn cached_denoise_uses_the_same_white_balance_profile_and_orientation_as_plain_raw() {
    use lightcraft_denoise::product;
    use lightcraft_raw::{DngWriteOptions, Method, Orientation, profile::HsvTable, write_dng};
    let dir = crate::tests_xmp::temp_dir("denoise-color-finish");
    let fixture = crate::tests_xmp::synthetic_dng_with(None, Default::default());
    let mut raw = lightcraft_raw::decode(&fixture).unwrap();
    raw.orientation = Orientation::Rotate90;
    raw.color.baseline_exposure = 0.5;
    raw.color.profile.look_table =
        Some(HsvTable { hue_divisions: 1, sat_divisions: 2, val_divisions: 1, data: vec![[23.0, 0.65, 0.8]; 2], srgb_value: false });
    let bytes = write_dng(&raw, &DngWriteOptions::default()).unwrap();
    let decoded = lightcraft_raw::decode(&bytes).unwrap();
    assert!(decoded.color.profile.look_table.is_some());
    // A cache containing the ordinary demosaic must undergo the identical downstream finish.
    let camera_rgb = decoded.develop(Method::Ahd).unwrap();
    let key = "synthetic-color-finish";
    let path = dir.join("same.lcdn");
    product::write(&path, &camera_rgb, key).unwrap();
    let spec = DenoiseSpec { product: path, key: key.into(), make: None };
    let (ordinary, _) = load_bytes(&bytes, 2048).unwrap();
    let (plain, cached, _) = load_vec_pair(bytes, 2048, &spec).unwrap();
    assert_eq!(plain.data, ordinary.data, "requesting a twin must not change the ordinary path");
    let cached = cached.unwrap();
    assert_eq!((plain.width, plain.height), (24, 32));
    assert_eq!((cached.width, cached.height), (plain.width, plain.height));
    let worst = plain.data.iter().zip(&cached.data).flat_map(|(a, b)| a.iter().zip(b).map(|(a, b)| (a - b).abs())).fold(0.0f32, f32::max);
    assert!(worst < 0.002, "color finish differs beyond cache half-float rounding: {worst}");
    let _ = std::fs::remove_dir_all(dir);
}

/// Luminance (from linear RGB) minus its 9 × 9 local mean: tone differences between a render and a camera JPEG
/// mostly cancel, edges remain.
fn detail(w: usize, h: usize, lum: impl Fn(usize, usize) -> f32) -> Vec<f32> {
    let l: Vec<f32> = (0..w * h).map(|i| lum(i % w, i / w)).collect();
    let mut out = vec![0.0; w * h];
    for y in 0..h {
        for x in 0..w {
            let (x0, x1, y0, y1) = (x.saturating_sub(4), (x + 5).min(w), y.saturating_sub(4), (y + 5).min(h));
            let m = (y0..y1).flat_map(|yy| (x0..x1).map(move |xx| (xx, yy))).map(|(xx, yy)| l[yy * w + xx]).sum::<f32>();
            out[y * w + x] = l[y * w + x] - m / ((x1 - x0) * (y1 - y0)) as f32;
        }
    }
    out
}

/// The integer shift (within ±`r` px) of the `p × p` patch of `a` at `(x, y)` that best matches `b`, with its
/// normalised cross-correlation, or `None` when the patch has too little detail to tell.
fn patch_shift(a: &[f32], b: &[f32], w: usize, (x, y): (usize, usize), p: usize, r: i64) -> Option<((i64, i64), f32)> {
    let pa: Vec<f32> = (0..p * p).map(|i| a[(y + i / p) * w + x + i % p]).collect();
    let na = pa.iter().map(|v| v * v).sum::<f32>().sqrt();
    if na < 1e-3 * p as f32 {
        return None;
    }
    let mut best = (f32::MIN, (0, 0));
    for dy in -r..=r {
        for dx in -r..=r {
            let (bx, by) = (x as i64 + dx, y as i64 + dy);
            let (mut dot, mut nb) = (0.0f32, 0.0f32);
            for i in 0..p {
                for j in 0..p {
                    let v = b[(by as usize + i) * w + bx as usize + j];
                    dot += pa[i * p + j] * v;
                    nb += v * v;
                }
            }
            let c = dot / (na * nb.sqrt()).max(1e-12);
            if c > best.0 {
                best = (c, (dx, dy));
            }
        }
    }
    Some((best.1, best.0))
}

#[test]
fn corpus_a7cr_warp_keeps_coordinates_after_orientation_and_resizing() {
    use lightcraft_geom::{Orientation, Point};
    use lightcraft_pipeline::optics::Warp;
    let root = std::env::var_os("LIGHTCRAFT_CORPUS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
    for name in ["arw-sony-a7cr-compressed.arw", "arw-sony-a7cr-lossless-l.arw"] {
        let Ok(mut bytes) = std::fs::read(root.join("raw").join(name)) else { continue };
        let off = lightcraft_raw::probe_info(&bytes).unwrap();
        assert!(embedded_lens(&off).is_none());
        let tiff = lightcraft_tiff::Tiff::parse(&bytes).unwrap();
        let at = tiff.find(0x7036).unwrap().offset as usize;
        let mut enabled = Vec::new();
        tiff.order.put_u16(&mut enabled, 17);
        bytes[at..at + 2].copy_from_slice(&enabled);
        let mut raw = lightcraft_raw::probe_info(&bytes).unwrap();
        raw.orientation = Orientation::Normal;
        let mut settings = lightcraft_develop::DevelopSettings::default();
        settings.optics.lens_profile = true;
        let base_lens = embedded_lens(&raw).unwrap();
        let base = Warp::from_settings(9504.0, 6336.0, &settings, Some(&base_lens));
        for orientation in 1..=8 {
            let o = Orientation::from_exif(orientation);
            raw.orientation = o;
            let lens = embedded_lens(&raw).unwrap();
            for edge in [1200.0, 2560.0, 9504.0] {
                let scale = edge / 9504.0;
                let (w, h) = if o.swaps_axes() { (6336.0 * scale, edge) } else { (edge, 6336.0 * scale) };
                let oriented = Warp::from_settings(w, h, &settings, Some(&lens));
                for (x, y) in [(0.05, 0.1), (0.5, 0.5), (0.9, 0.8)] {
                    let point = Point::new(x * 9504.0, y * 6336.0);
                    let source = base.corrected_to_source(point, 1);
                    let (px, py) = o.map(point.x, point.y, 9504.0, 6336.0);
                    let (sx, sy) = o.map(source.x, source.y, 9504.0, 6336.0);
                    let got = oriented.corrected_to_source(Point::new(px * scale, py * scale), 1);
                    assert!((got.x - sx * scale).hypot(got.y - sy * scale) < 1e-8, "{name} orientation {orientation} edge {edge}");
                }
            }
        }
    }
}

/// Issue #256: public Panasonic / Leica raws (skipped without the corpus) are corrected for distortion the way the
/// camera corrected its own JPEG (tag 0x0119): at the corners of the frame, the render with "Enable Profile
/// Corrections" lines up with the embedded JPEG within 2 px at 640 px, while the uncorrected render of the wide
/// lenses is far off. A file shot with the camera's correction off (DMC-GH1) carries no correction.
#[test]
fn corpus_rw2_distortion_matches_the_camera_jpeg() {
    let dir = std::env::var_os("LIGHTCRAFT_CORPUS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
        .join("raw");
    let Ok(gh1) = std::fs::read(dir.join("rw2-panasonic-gh1.rw2")) else {
        lightcraft_testkit::skip!(Corpus, "{} absent", dir.display());
        return;
    };
    assert_eq!(probe_bytes("gh1.rw2", &gh1).unwrap().embedded_lens, None, "correction off: nothing to apply");
    assert_eq!(load_bytes(&gh1, 320).unwrap().1.lens, None);

    // (file, how far off (px) the uncorrected corners are at the least, if they match within ±R at all)
    let files = [
        ("rw2-panasonic-fz1000m2-4x3.rw2", 8), // 9.1 mm, ~13 % at the corners
        ("rwl-leica-dlux7.rwl", 8),            // 10.9 mm, ~13 %
        ("rw2-panasonic-gx80.rw2", 8),         // 14 mm, ~8 %
        ("rw2-panasonic-gh6.rw2", 3),          // 20 mm, ~1.6 %
        ("rw2-panasonic-s9.rw2", 3),           // 45 mm, 3 % zoom after the correction
        ("rw2-panasonic-g9.rw2", 0),           // 40 mm, ~0.5 %
    ];
    const EDGE: usize = 640;
    const P: usize = 64;
    const R: i64 = 12;
    for (name, off_at_least) in files {
        let bytes = std::fs::read(dir.join(name)).unwrap();
        let probed = probe_bytes(name, &bytes).unwrap();
        assert!(probed.embedded_lens.is_some_and(|l| l.warp.is_some()), "{name}: no distortion correction read");
        let (src, info) = load_bytes(&bytes, 2 * EDGE).unwrap();
        assert_eq!(info.lens, probed.embedded_lens, "{name}: probe and decode disagree");
        // Convert to DNG keeps it: the same correction, as the DNG's own `WarpRectilinear`
        let dng = lightcraft_raw::write_dng(&lightcraft_raw::decode(&bytes).unwrap(), &Default::default()).unwrap();
        assert_eq!(probe_bytes("x.dng", &dng).unwrap().embedded_lens, probed.embedded_lens, "{name}: lost in the DNG");
        let mut s = lightcraft_develop::DevelopSettings::default();
        let render = |s: &lightcraft_develop::DevelopSettings| {
            lightcraft_pipeline::render(&src, &info, s, &lightcraft_pipeline::RenderRequest::fit(EDGE, EDGE)).image
        };
        let before = render(&s);
        s.optics.lens_profile = true;
        let after = render(&s);
        let (w, h) = (after.width, after.height);
        assert_eq!((before.width, before.height), (w, h));
        // the camera JPEG shows the whole sensor; a render in an in-camera aspect ratio is its centre
        let (cam, _) = load_embedded_preview(&bytes, 4 * EDGE).unwrap();
        let (cw, ch) = (cam.width as f64, cam.height as f64);
        let (sw, sh) = if cw / ch > w as f64 / h as f64 { (ch * w as f64 / h as f64, ch) } else { (cw, cw * h as f64 / w as f64) };
        let cam = cam.crop(((cw - sw) / 2.0).round() as usize, ((ch - sh) / 2.0).round() as usize, sw.round() as usize, sh.round() as usize);
        let cam = lightcraft_raster::resample::resize(&cam, w, h, Filter::Mitchell);
        let lin = |v: u8| (v as f32 / 255.0).powf(2.2);
        let lum8 = |im: &lightcraft_raster::Rgba8, x: usize, y: usize| {
            let p = im.data[y * im.width + x];
            0.3 * lin(p[0]) + 0.6 * lin(p[1]) + 0.1 * lin(p[2])
        };
        let reference = detail(w, h, |x, y| {
            let p = cam.data[y * w + x];
            0.3 * p[0] + 0.6 * p[1] + 0.1 * p[2]
        });
        let (db, da) = (detail(w, h, |x, y| lum8(&before, x, y)), detail(w, h, |x, y| lum8(&after, x, y)));
        let m = R as usize + 4;
        let corners = [(m, m), (w - m - P, m), (m, h - m - P), (w - m - P, h - m - P)];
        let mut measured = 0;
        for c in corners {
            let Some(((dx, dy), ca)) = patch_shift(&da, &reference, w, c, P, R) else { continue };
            measured += 1;
            assert!(dx.abs() <= 2 && dy.abs() <= 2 && ca >= 0.5, "{name}: corrected corner {c:?} off by ({dx}, {dy}) px, ncc {ca}");
            if off_at_least > 0
                && let Some(((bx, by), cb)) = patch_shift(&db, &reference, w, c, P, R)
            {
                let off = bx.abs().max(by.abs()) >= off_at_least || cb < 0.3;
                assert!(off, "{name}: uncorrected corner {c:?} already within ({bx}, {by}) px, ncc {cb}");
            }
        }
        assert!(measured >= 2, "{name}: only {measured} corners with detail");
    }
}

/// A CR3-shaped file (not decodable yet) whose only content is a `PRVW` preview box.
pub(crate) fn cr3_with_preview(w: u32, h: u32) -> Vec<u8> {
    let px: Vec<u8> = (0..w * h).flat_map(|i| [(i % 251) as u8, 128, 200]).collect();
    let jpeg = encode_jpeg(&EncodeImage::new(w, h, 3, Samples::U8(&px)), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
    let mut b = b"\0\0\0\x18ftypcrx \0\0\0\x01crx isom".to_vec();
    b.extend_from_slice(&((24 + jpeg.len()) as u32).to_be_bytes());
    b.extend_from_slice(b"PRVW\0\0\0\0\0\x01");
    b.extend_from_slice(&(w as u16).to_be_bytes());
    b.extend_from_slice(&(h as u16).to_be_bytes());
    b.extend_from_slice(b"\0\x01");
    b.extend_from_slice(&(jpeg.len() as u32).to_be_bytes());
    b.extend_from_slice(&jpeg);
    b
}

/// [`cr3_with_preview`] plus a full-size CRX raw track whose `CMP1` coding header is garbage:
/// the container parses, the raw data doesn't (`RawError::Corrupt`, not `Unsupported`).
fn cr3_with_corrupt_raw(w: u32, h: u32) -> Vec<u8> {
    let bx = |kind: &[u8; 4], body: &[u8]| [&((body.len() + 8) as u32).to_be_bytes()[..], kind, body].concat();
    let mut craw = vec![0u8; 82];
    craw[24..26].copy_from_slice(&(w as u16).to_be_bytes());
    craw[26..28].copy_from_slice(&(h as u16).to_be_bytes());
    craw.extend(bx(b"CMP1", &[0xff; 4]));
    let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
    stsd.extend(bx(b"CRAW", &craw));
    let stsz = [0u32, 16, 1].map(u32::to_be_bytes).concat();
    let co64 = [&[0u8, 0, 0, 0, 0, 0, 0, 1][..], &0u64.to_be_bytes()].concat();
    let stbl = [bx(b"stsd", &stsd), bx(b"stsz", &stsz), bx(b"co64", &co64)].concat();
    let trak = bx(b"trak", &bx(b"mdia", &bx(b"minf", &bx(b"stbl", &stbl))));
    let preview = cr3_with_preview(w, h);
    [&preview[..24], &bx(b"moov", &trak), &preview[24..]].concat()
}

/// A CR3 whose raw data fails to decode, for any reason, still opens from its embedded JPEG:
/// CR3s did before the CRX decoder, which is verified on few bodies (PR #279 review).
#[test]
fn corrupt_cr3_falls_back_to_embedded_preview() {
    let b = cr3_with_corrupt_raw(48, 32);
    let err = lightcraft_raw::decode(&b).expect_err("the fixture's raw data is corrupt");
    assert!(!matches!(err, lightcraft_raw::RawError::Unsupported(_)), "{err}");
    let p = probe_bytes("x.cr3", &b).unwrap();
    assert_eq!((p.width, p.height, p.kind, p.format.as_str()), (48, 32, MediaKind::Raw, "CR3"));
    assert!(p.preview_only.is_some_and(|why| why.contains("CR3")));
    let (img, src) = load_bytes(&b, 24).unwrap();
    assert_eq!((img.width, img.height), (24, 16));
    assert!(!src.raw);
    // other formats still report a corrupt file as an error
    let mut dng = crate::tests_xmp::synthetic_dng_with(None, Default::default());
    dng.truncate(dng.len() / 2);
    assert!(load_bytes(&dng, 24).is_err());
}

/// Issue #138: a DNG's own profile look (hue/saturation map, look table, tone curve) is
/// applied when it's developed — Lightroom-converted DNGs rendered flat and muted without it.
#[test]
fn dng_profile_look_is_applied() {
    use lightcraft_raw::profile::{HsvTable, ProfileLook, ToneCurve};
    let plain = crate::tests_xmp::synthetic_dng_with(None, Default::default());
    let mut raw = lightcraft_raw::decode(&plain).unwrap();
    let sat = |img: &Rgb32f| {
        img.data.iter().map(|p| (p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2])) / p[0].max(p[1]).max(p[2]).max(1e-6)).sum::<f32>()
            / img.data.len() as f32
    };
    let (before, info) = load_bytes(&plain, 64).unwrap();
    assert!(sat(&before) > 0.05, "the synthetic scene is coloured");
    assert!(info.camera_tone.is_none());
    // a map that removes all saturation, and a tone curve
    let grey = HsvTable { hue_divisions: 4, sat_divisions: 2, val_divisions: 1, data: vec![[0.0, 0.0, 1.0]; 8], srgb_value: false };
    raw.color.profile = ProfileLook {
        hue_sat_map: [Some(grey), None],
        look_table: None,
        tone_curve: ToneCurve::from_tag(&[0.0, 0.0, 0.18, 0.3, 1.0, 1.0]),
        gain_table_map: None,
    };
    let with = lightcraft_raw::write_dng(&raw, &Default::default()).unwrap();
    let (after, info) = load_bytes(&with, 64).unwrap();
    assert!(sat(&after) < 1e-3, "saturation {} → {}", sat(&before), sat(&after));
    // a saturation-only map keeps brightness roughly (HSV value is kept in ProPhoto RGB)
    let mean = |img: &Rgb32f| img.data.iter().map(|p| p[0].max(p[1]).max(p[2])).sum::<f32>() / img.data.len() as f32;
    assert!((0.8..1.1).contains(&(mean(&after) / mean(&before))), "{} vs {}", mean(&after), mean(&before));
    let tone = info.camera_tone.expect("the DNG tone curve becomes the camera tone");
    assert!((tone.apply(0.18) - 0.3).abs() < 0.01, "{}", tone.apply(0.18));
    // a look table alone changes the render too (applied after exposure)
    raw.color.profile = ProfileLook {
        look_table: Some(HsvTable { hue_divisions: 1, sat_divisions: 2, val_divisions: 1, data: vec![[0.0, 1.0, 0.5]; 2], srgb_value: false }),
        ..Default::default()
    };
    let (dim, _) = load_bytes(&lightcraft_raw::write_dng(&raw, &Default::default()).unwrap(), 64).unwrap();
    assert!((mean(&dim) / mean(&before) - 0.5).abs() < 0.02, "{} vs {}", mean(&dim), mean(&before));
}

/// Corrections used while fitting a DNG's preview must not be baked into its editable source.
/// The pipeline applies them once from SourceInfo, and a file-defined tone stays authoritative.
#[test]
fn dng_preview_corrections_leave_the_render_source_uncorrected() {
    let plain = crate::tests_xmp::synthetic_dng_with(None, Default::default());
    let mut raw = lightcraft_raw::decode(&plain).unwrap();
    raw.color.profile.tone_curve = lightcraft_raw::profile::ToneCurve::from_tag(&[0.0, 0.0, 0.18, 0.4, 1.0, 1.0]);
    let before = lightcraft_raw::write_dng(&raw, &Default::default()).unwrap();
    raw.opcodes.list3 = vec![lightcraft_raw::Opcode::FixVignetteRadial { k: [1.0, 0.0, 0.0, 0.0, 0.0], center: [0.5, 0.5] }];
    let corrected = lightcraft_raw::write_dng(&raw, &Default::default()).unwrap();
    let (before, expected) = load_bytes(&before, 64).unwrap();
    let (after, info) = load_bytes(&corrected, 64).unwrap();
    assert_eq!(before.data, after.data, "lens correction belongs to the pipeline");
    assert!(info.lens.as_ref().is_some_and(|l| l.vignette.is_some()));
    assert_eq!(info.camera_tone, expected.camera_tone, "the file's own tone is preserved");
}

/// A camera JPEG made from the corrected scene teaches one tone to the DNG's existing fit.
/// The source pixels stay uncorrected, including when the file is rotated.
#[test]
fn dng_lens_corrected_preview_teaches_one_tone_without_baking_in_the_lens() {
    use lightcraft_color::{REC2020, SRGB, luminance_2020, transfer::linear_to_srgb};
    use lightcraft_raw::{Opcode, Orientation};
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
    let mut raw = lightcraft_raw::decode(&bytes).unwrap();
    raw.color = lightcraft_raw::ColorData {
        illuminant: [21, 0],
        color_matrix: [Some(REC2020.from_xyz()), None],
        as_shot_neutral: Some([1.0; 3]),
        ..Default::default()
    };
    let plain = lightcraft_raw::write_dng(&raw, &Default::default()).unwrap();
    raw.opcodes.list3 = vec![
        Opcode::FixVignetteRadial { k: [0.8, 0.0, 0.0, 0.0, 0.0], center: [0.5, 0.5] },
        Opcode::WarpRectilinear { planes: vec![[0.7, 0.2, 0.0, 0.0, 0.0, 0.0]], center: [0.5, 0.5] },
    ];
    let t = lightcraft_raw::color::camera_transform(&raw, lightcraft_raw::color::as_shot_white_xy(&raw));
    let corrected = raw.develop(lightcraft_raw::Method::Bilinear).unwrap();
    let to_srgb = REC2020.to_space(&SRGB);
    let pixels: Vec<u8> = corrected
        .data
        .iter()
        .flat_map(|p| {
            let p = t.matrix.apply_f32(std::array::from_fn(|i| p[i] * t.wb[i]));
            let y = luminance_2020(p).max(1e-6);
            let gain = (1.0 - (-3.0 * y).exp()) / y;
            to_srgb.apply_f32(p.map(|v| v * gain)).map(|v| (255.0 * linear_to_srgb(v).clamp(0.0, 1.0)).round() as u8)
        })
        .collect();
    let image = lightcraft_codecs::EncodeImage::new(w as u32, h as u32, 3, lightcraft_codecs::Samples::U8(&pixels));
    let jpeg = lightcraft_codecs::encode_jpeg(&image, 98, lightcraft_codecs::ChromaSubsampling::S444, &Default::default()).unwrap();
    for orientation in [Orientation::Normal, Orientation::Rotate90] {
        raw.orientation = orientation;
        let mut file = lightcraft_raw::write_dng(&raw, &Default::default()).unwrap();
        // Append a preview IFD to the little-endian DNG written above.
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
        let (source, info) = load_bytes(&file, 96).unwrap();
        let tone = info.camera_tone.expect("one DNG look fitted to the lens-corrected preview");
        let target = 1.0 - (-3.0f32 * 0.18).exp();
        assert!((tone.apply(0.18) - target).abs() < 0.04, "one tone: {} vs {target}", tone.apply(0.18));
        let expected = load_bytes(&plain, 96).unwrap().0.into_oriented(orientation);
        assert_eq!(source.data, expected.data, "the editable source is not lens-corrected");
    }
}

/// A look fitted to a camera's JPEG, as `camera_preview` makes them: its rows don't sum to one, so the camera's
/// own white-balanced neutral comes out yellow (blue 12 % low).
fn yellowing_look() -> lightcraft_color::Mat3 {
    lightcraft_color::Mat3([[0.42, 0.548, 0.04], [0.246, 0.728, 0.033], [-0.247, 0.679, 0.447]])
}

/// A fitted camera look applied in lanes, on rows that aren't a whole number of them, gives the bits of the
/// pixel-by-pixel path, awkward pixels (black, negative, huge, NaN, infinite) included.
#[test]
fn camera_look_in_lanes_matches_pixel_by_pixel() {
    let data = (0..72 * 5 * 5).map(|i| [((i * 37) % 61) as f32 - 30.0, 0.6 + ((i * 11) % 13) as f32 / 13.0, 1.0]).collect();
    let table = lightcraft_raw::profile::HsvTable { hue_divisions: 72, sat_divisions: 5, val_divisions: 5, data, srgb_value: true };
    let hue_sat = crate::camera_preview::HueSat::new(&table).unwrap();
    let colour = CameraColourModel { wb: [2.1, 1.0, 1.5], matrix: yellowing_look(), gain: 1.3, tables: None, hue_sat: Some(&hue_sat) };
    let special = [0.0, -0.0, -0.01, 1e-30, 0.3, 1.0, 5.0, 1e30, f32::INFINITY, f32::NAN];
    let img = Rgb32f::from_fn(37, 5, |x, y| {
        let k = y * 37 + x;
        if k % 5 == 0 {
            [special[k % 10], special[k / 10 % 10], special[k / 100 % 10]]
        } else {
            [(x as f32 * 0.37).sin().abs(), ((x + y) as f32 * 0.11).cos().abs(), y as f32 / 4.0 - 0.05]
        }
    });
    let mut lanes = img.clone();
    colour.apply(&mut lanes);
    let m = colour.matrix.to_f32();
    for (p, got) in img.data.iter().zip(&lanes.data) {
        assert_eq!(got.map(f32::to_bits), colour.pixel(&m, *p).map(f32::to_bits), "{p:?}");
    }
}

#[test]
fn clipped_white_renders_neutral_through_the_colour_model() {
    use lightcraft_color::{LUMA_2020, Mat3};
    let look = yellowing_look();
    let white = clipped_white(&look);
    let out = look.apply(white.map(f64::from));
    assert!(out.iter().all(|v| (v - out[1]).abs() < 1e-5), "{white:?} renders {out:?}");
    let luma = |c: [f64; 3]| (0..3).map(|i| c[i] * f64::from(LUMA_2020[i])).sum::<f64>();
    assert!((luma(out) - luma(look.apply([1.0; 3]))).abs() < 1e-5, "as bright as the camera's neutral");
    assert_eq!(clipped_white(&Mat3::IDENTITY), [1.0; 3]);
    // a model that can't be inverted, or only just, keeps the camera's neutral
    assert_eq!(clipped_white(&Mat3([[1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]])), [1.0; 3]);
    assert_eq!(clipped_white(&Mat3([[1.0, 1.0, 0.0], [1.0, 1.0 + 1e-8, 0.0], [0.0, 0.0, 1.0]])), [1.0; 3]);
    // so does one whose white would overflow the rebuilt highlights (2.5e38 in red here)
    assert_eq!(clipped_white(&Mat3([[3e-39, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]])), [1.0; 3]);
    // or move the camera's neutral implausibly far
    assert_eq!(clipped_white(&Mat3([[0.1, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]])), [1.0; 3]);
    // what it answers renders neutral through the f32 matrix, and fully clipped highlights stay finite
    for model in [look, Mat3([[1.9, -0.7, -0.2], [-0.3, 1.4, -0.1], [0.0, -0.4, 1.4]])] {
        let w = clipped_white(&model);
        let m = model.to_f32();
        let out: [f32; 3] = std::array::from_fn(|i| m[i][0] * w[0] + m[i][1] * w[1] + m[i][2] * w[2]);
        assert!(out.iter().all(|v| (v - out[1]).abs() <= 1e-3 * out[1]), "{w:?} renders {out:?}");
        let colour = CameraColourModel { wb: [2.5, 1.0, 1.6], matrix: model, gain: 1.0, tables: None, hue_sat: None };
        let mut img = Rgb32f::filled(4, 4, [1.0; 3]);
        colour.rebuild_highlights(&mut img);
        colour.apply(&mut img);
        assert!(img.data.iter().flatten().all(|v| v.is_finite()), "{:?}", img.get(0, 0));
    }
}

/// Issue #523: fully clipped highlights are neutral in the white-balanced camera RGB, but a colour model that
/// doesn't map that neutral to neutral tinted them. The tone map's roll-off to white hid the tint at Neutral;
/// Highlights -100, or the issue's edit (Exposure -0.88, Highlights -75…), showed it.
#[test]
fn clipped_highlights_stay_neutral_when_darkened() {
    use lightcraft_pipeline::{RenderRequest, render};
    let wb = [2.4f32, 1.0, 1.6];
    let colour = CameraColourModel { wb, matrix: yellowing_look(), gain: 1.0, tables: None, hue_sat: None };
    // a grey wall (camera neutral) with a fully clipped light and a light clipped in green only
    let mut img = Rgb32f::from_fn(96, 64, |x, y| {
        let d = |cx: f32| ((x as f32 - cx).powi(2) + (y as f32 - 32.0).powi(2)).sqrt();
        if d(28.0) < 14.0 {
            [1.0; 3]
        } else if d(70.0) < 12.0 {
            [0.6 / wb[0], 1.0, 0.7 / wb[2]]
        } else {
            [0.3 / wb[0], 0.3, 0.3 / wb[2]]
        }
    });
    colour.rebuild_highlights(&mut img);
    colour.apply(&mut img);
    let info = SourceInfo { raw: true, ..Default::default() };
    let mut issue = lightcraft_develop::DevelopSettings::default();
    let l = &mut issue.light;
    (l.exposure, l.contrast, l.highlights, l.shadows, l.whites, l.blacks) = (-0.88, 5.0, -75.0, 52.0, 6.0, -37.0);
    let mut h100 = lightcraft_develop::DevelopSettings::default();
    h100.light.highlights = -100.0;
    for (name, s) in [("Highlights -100", h100), ("#523's edit", issue)] {
        let out = render(&img, &info, &s, &RenderRequest::fit(96, 64)).image;
        let p = out.get(28, 32);
        assert!(p[1] < 250, "{name}: the clipped light is darkened ({p:?})");
        let spread = p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]);
        assert!(spread <= 1, "{name}: clipped light {p:?} is not neutral");
    }
}

/// A smooth pink gradient (`[0.9k, k, 0.8k]` in the sensor, k from 0.8 to 1.35, white balance `[2.5, 1, 1.6]`)
/// through the point where green clips, 1600 px long, in `rows` of a 1600 × `height` picture that is otherwise
/// near black, rebuilt and rendered at Exposure -2. Returns the rendered colour of the middle row's last
/// unclipped pixel and the next two, and the largest step between neighbouring pixels along that row.
fn darkened_ramp(rows: std::ops::Range<usize>, height: usize) -> ([[u8; 4]; 3], u8) {
    use lightcraft_pipeline::{RenderRequest, render};
    let wb = [2.5f32, 1.0, 1.6];
    let colour = CameraColourModel { wb, matrix: lightcraft_color::Mat3::IDENTITY, gain: 1.0, tables: None, hue_sat: None };
    let w = 1600usize;
    let k = |x: usize| 0.8 + 0.55 * x as f32 / (w - 1) as f32;
    let sensor = Rgb32f::from_fn(w, height, |x, y| if rows.contains(&y) { [0.9 * k(x), k(x), 0.8 * k(x)].map(|v| v.min(1.0)) } else { [0.01; 3] });
    let y = (rows.start + rows.end) / 2;
    let first = (0..w).find(|&x| sensor.get(x, y)[1] >= HIGHLIGHT_CLIP).unwrap();
    let mut img = sensor.clone();
    colour.rebuild_highlights(&mut img);
    colour.apply(&mut img);
    let mut s = lightcraft_develop::DevelopSettings::default();
    s.light.exposure = -2.0;
    let out = render(&img, &SourceInfo { raw: true, ..Default::default() }, &s, &RenderRequest::fit(w, height)).image;
    let step = |x: usize| {
        let (a, b) = (out.get(x - 1, y), out.get(x, y));
        (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0)
    };
    ([out.get(first - 1, y), out.get(first, y), out.get(first + 1, y)], (1..w).map(step).max().unwrap_or(0))
}

/// The darkened pink gradient carries on across the point where green clips: before the fix the fade towards
/// the clipped neutral, driven by how close the other channels were to their clip, made a step of 37 levels
/// there.
#[test]
fn rendered_colour_is_continuous_where_the_first_channel_clips() {
    let ([a, b, c], worst) = darkened_ramp(0..16, 16);
    let step = |p: [u8; 4], q: [u8; 4]| (0..3).map(|i| p[i].abs_diff(q[i])).max().unwrap_or(0);
    assert!(step(a, b) <= 1 && step(b, c) <= 1, "at the first clip: {a:?} {b:?} {c:?}");
    assert!(worst <= 3, "largest step between neighbouring pixels: {worst} levels");
}

/// The same gradient as a thin bright line on near black: every pixel of it is next to a strong edge, so there
/// is no reliable colour around it. It still carries on across the first clip, however thin the line and however
/// much dark background there is (a 4-row line on 32 rows used to jump from [225, 126, 153] to [198, 177, 163];
/// on 512 rows, from [225, 126, 153] to [218, 145, 151]).
#[test]
fn rendered_thin_line_is_continuous_where_the_first_channel_clips() {
    let step = |p: [u8; 4], q: [u8; 4]| (0..3).map(|i| p[i].abs_diff(q[i])).max().unwrap_or(0);
    for height in [32usize, 128, 512, 1024] {
        for width in [1usize, 2, 4, 16] {
            let top = (height - width) / 2;
            let ([a, b, c], worst) = darkened_ramp(top..top + width, height);
            let case = format!("{width} rows of {height}");
            assert!(step(a, b) <= 1 && step(b, c) <= 1, "{case}, at the first clip: {a:?} {b:?} {c:?}");
            assert!(worst <= 3, "{case}: largest step between neighbouring pixels {worst} levels");
        }
    }
}

/// A band clipped in green whose red rises through its own clip from left to right (sensor `[0.985 + 0.01 t, 1,
/// 0.8]`, t from 0 to 1 across 320 px), `band` rows at the bottom of the picture, below `strip` rows of a pale
/// surface just under the clip (`[0.4, 0.95, 0.8]`: the band's rim, of another colour) and above those `top` rows
/// of a strongly coloured reliable surface (`[0.8, 0.35, 0.3]`). Rebuilt and rendered at Exposure -2: the
/// rendered colour on either side of where red clips on the band's middle row, and the largest step between
/// neighbouring pixels along that row.
fn second_clip(top: usize, strip: usize, band: usize) -> ([[u8; 4]; 2], u8) {
    use lightcraft_pipeline::{RenderRequest, render};
    let wb = [2.5f32, 1.0, 1.6];
    let colour = CameraColourModel { wb, matrix: lightcraft_color::Mat3::IDENTITY, gain: 1.0, tables: None, hue_sat: None };
    let (w, h) = (320usize, top + strip + band);
    let sensor = Rgb32f::from_fn(w, h, |x, y| {
        if y < top {
            [0.8, 0.35, 0.3]
        } else if y < top + strip {
            [0.4, 0.95, 0.8]
        } else {
            [0.985 + 0.01 * x as f32 / (w - 1) as f32, 1.0, 0.8]
        }
    });
    let y = top + strip + band / 2;
    let at = (0..w).find(|&x| sensor.get(x, y)[0] >= HIGHLIGHT_CLIP).unwrap();
    let mut img = sensor.clone();
    colour.rebuild_highlights(&mut img);
    colour.apply(&mut img);
    let mut s = lightcraft_develop::DevelopSettings::default();
    s.light.exposure = -2.0;
    let out = render(&img, &SourceInfo { raw: true, ..Default::default() }, &s, &RenderRequest::fit(w, h)).image;
    let step = |x: usize| {
        let (a, b) = (out.get(x - 1, y), out.get(x, y));
        (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0)
    };
    ([out.get(at - 1, y), out.get(at, y)], (1..w).map(step).max().unwrap_or(0))
}

/// Where a second channel clips within reach of the surface's own rim, the rebuilt colour carries on too. The
/// rim's comparison with the pixel used to drop each clipped channel, so a rim far from the pixel in red suddenly
/// matched it once red clipped (80 rows of colour, 16 of rim and 32 of band: [237, 124, 153] then
/// [205, 181, 168]); and a channel about to clip counted as an uncapped floor that was capped once it clipped
/// (without the coloured surface: [198, 195, 178] then [205, 181, 168]).
#[test]
fn rendered_colour_is_continuous_where_a_second_channel_clips() {
    for (top, strip, band) in [(80, 16, 32), (0, 16, 32), (80, 16, 4), (200, 4, 8), (16, 64, 16), (8, 32, 24)] {
        let ([a, b], worst) = second_clip(top, strip, band);
        let case = format!("{top} rows of colour, {strip} of rim, {band} of band");
        let step = (0..3).map(|i| a[i].abs_diff(b[i])).max().unwrap_or(0);
        assert!(step <= 1, "{case}: {a:?} then {b:?} where red clips");
        assert!(worst <= 3, "{case}: largest step between neighbouring pixels {worst} levels");
    }
}

/// Issue #523 on a public ILCE-7RM4 raw (raw.pixls.us, CC0; skipped without the corpus): a dusk sky clipped in
/// green behind a poplar. Its rebuilt colour used to come from the tree's foliage and lens fringes, a green halo
/// on one side of the tree and a magenta one on the other (Highlights -100 or the issue's edit show them).
#[test]
fn corpus_clipped_sky_beside_a_tree_has_no_coloured_halo() {
    let dir = std::env::var_os("LIGHTCRAFT_CORPUS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
        .join("raw");
    let Ok(bytes) = std::fs::read(dir.join("arw-sony-a7rm4-14bit-compressed.arw")) else {
        lightcraft_testkit::skip!(Corpus, "{} absent", dir.display());
        return;
    };
    let (img, _) = load_bytes(&bytes, 1600).unwrap();
    let (w, h) = (img.width, img.height);
    let lum = |p: [f32; 3]| lightcraft_color::luminance_2020(p);
    let chroma = |p: [f32; 3]| {
        let s = (p[0] + p[1] + p[2]).max(1e-6);
        [p[0] / s, p[2] / s]
    };
    // the poplar stands at about 30 % of the width, its top 25 to 34 % down the frame
    let px = |f: f32, n: usize| (f * n as f32) as usize;
    let (mut near, mut far) = ([[0f64; 2]; 2], [[0f64; 2]; 2]);
    let mut rows = 0;
    for y in px(0.24, h)..px(0.34, h) {
        let row = &img.data[y * w..(y + 1) * w];
        let sky = lum(row[px(0.2, w)]);
        let dark: Vec<usize> = (px(0.25, w)..px(0.35, w)).filter(|&x| lum(row[x]) < 0.5 * sky).collect();
        let (Some(&left), Some(&right)) = (dark.first(), dark.last()) else { continue };
        let k = w as f32 / 6000.0;
        let mean = |a: usize, b: usize| {
            let n = (b - a) as f64;
            (a..b).map(|x| chroma(row[x])).fold([0f64; 2], |s, c| [s[0] + c[0] as f64 / n, s[1] + c[1] as f64 / n])
        };
        let (n0, n1, f0, f1) = (px(8.0 / 6000.0, w).max(2), px(40.0 / 6000.0, w), (200.0 * k) as usize, (320.0 * k) as usize);
        for (side, (n, f)) in [(mean(left - n1, left - n0), mean(left - f1, left - f0)), (mean(right + n0, right + n1), mean(right + f0, right + f1))]
            .into_iter()
            .enumerate()
        {
            for c in 0..2 {
                near[side][c] += n[c];
                far[side][c] += f[c];
            }
        }
        rows += 1;
    }
    assert!(rows > 50, "the tree was not found ({rows} rows)");
    for side in 0..2 {
        let d = (0..2).map(|c| ((near[side][c] - far[side][c]) / rows as f64).abs()).fold(0.0, f64::max);
        assert!(d < 0.004, "{} of the tree the clipped sky's chromaticity is off by {d:.4}", ["left", "right"][side]);
    }
}

/// A Lightroom merge (a LinearRaw DNG keeping a reduced copy of its raw image): its grid thumbnail develops the
/// copy, but an export never does, whatever its size, and not from the thumbnail's source in memory either. The
/// copy's samples are much brighter than the image's here, so a picture says which one it came from.
#[test]
fn exports_never_develop_a_dngs_reduced_copy() {
    use lightcraft_tiff::tags::{self as t, photometric};
    use lightcraft_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value};
    use serde_json::json;
    let linear = |subfile: u32, w: u32, h: u32, sample: u16| {
        let mut ifd = IfdBuilder::new();
        ifd.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![subfile]));
        ifd.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
        ifd.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
        ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![16, 16, 16]));
        ifd.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![3]));
        ifd.set(t::PHOTOMETRIC, Value::Short(vec![photometric::LINEAR_RAW]));
        ifd.set(t::COMPRESSION, Value::Short(vec![1]));
        ifd.set(t::WHITE_LEVEL, Value::Long(vec![32768]));
        ifd.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![sample.to_le_bytes().repeat((w * h * 3) as usize)] });
        ifd
    };
    // (the copy must cover a thumbnail source's 512 px)
    let (w, h) = (1200u32, 600u32);
    let mut main = linear(0, w, h, 1500);
    main.set(t::DEFAULT_CROP_ORIGIN, Value::Long(vec![4, 2]));
    main.set(t::DEFAULT_CROP_SIZE, Value::Long(vec![w - 8, h - 4]));
    let mut ifd0 = IfdBuilder::new();
    ifd0.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![1]));
    ifd0.set(t::DNG_VERSION, Value::Byte(vec![1, 7, 0, 0]));
    ifd0.set(t::MAKE, Value::Ascii("DJI".into()));
    ifd0.set(t::COLOR_MATRIX_1, Value::SRational(vec![(1, 1), (0, 1), (0, 1), (0, 1), (1, 1), (0, 1), (0, 1), (0, 1), (1, 1)]));
    ifd0.set(t::CALIBRATION_ILLUMINANT_1, Value::Short(vec![21]));
    ifd0.set(t::PHOTOMETRIC, Value::Short(vec![photometric::RGB]));
    ifd0.set(t::IMAGE_WIDTH, Value::Long(vec![4]));
    ifd0.set(t::IMAGE_LENGTH, Value::Long(vec![2]));
    ifd0.set(t::BITS_PER_SAMPLE, Value::Short(vec![8, 8, 8]));
    ifd0.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![3]));
    ifd0.set(t::COMPRESSION, Value::Short(vec![1]));
    ifd0.set_image(ImageData::Strips { rows_per_strip: 2, strips: vec![vec![128; 24]] });
    ifd0.add_sub_ifd(main);
    ifd0.add_sub_ifd(linear(1, w / 2, h / 2, 24000));
    let bytes = TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap();
    let g = |img: &Rgb32f| img.data.iter().map(|p| p[1] as f64).sum::<f64>() / img.data.len().max(1) as f64;
    let mean = |img: &lightcraft_raster::Rgba8| img.data.iter().map(|p| p[1] as f64).sum::<f64>() / img.data.len().max(1) as f64;

    // the loaders: only the thumbnail's develops the copy, and only when it covers the size
    let (copy, _, used) = load_vec_reduced(bytes.clone(), 512).unwrap();
    assert!(used, "a thumbnail develops the copy");
    assert!(!load_vec_reduced(bytes.clone(), 1024).unwrap().2, "no copy covers 1024 px");
    let (full, _) = load_vec(bytes.clone(), 512).unwrap();
    assert!(g(&copy) > 4.0 * g(&full), "the fixture's copy is much brighter: {} vs {}", g(&copy), g(&full));

    let dir = std::env::temp_dir().join(format!("lc-reduced-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("merge-pano.dng");
    std::fs::write(&path, &bytes).unwrap();
    let mut s = crate::Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    let id = lightcraft_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
    let (space, depth) = (lightcraft_pipeline::OutputSpace::Srgb, lightcraft_pipeline::OutputDepth::U8);
    // an export at a thumbnail's size, before any thumbnail
    assert_eq!(s.export_job(id, 64, 64, space, depth).unwrap().level, crate::media::SourceLevel::Thumb);
    let export = mean(&s.render_export(id, 64, 64, space, depth).unwrap().image);
    // the thumbnail develops the copy and its source stays in memory…
    s.media.clear_sources();
    let thumb = s.thumb_job(id, 64).unwrap().run();
    assert!(thumb.loaded.as_ref().is_some_and(|l| l.reduced));
    let thumb_mean = mean(&thumb.rendered.as_ref().unwrap().image);
    s.accept(&thumb);
    assert!(thumb_mean > export + 20.0, "the thumbnail shows the copy: {thumb_mean} vs {export}");
    // …and an export after it still reads the full image
    let job = s.export_job(id, 64, 64, space, depth).unwrap();
    assert!(matches!(&job.source, crate::media::SourceRef::File { reduced: None, .. }), "the file, not the copy");
    let again = mean(&s.render_export(id, 64, 64, space, depth).unwrap().image);
    assert!((again - export).abs() < 1.0, "{again} vs {export}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// iPhone ProRAW-style semantic masks drive the Sky mask: Apple's sky matte (here the right
/// half of the sensor, i.e. the bottom of the photo once turned 90° clockwise — where the sky
/// heuristic would never look) is used; a matte with an unknown name is ignored.
#[test]
fn dng_sky_matte_drives_the_sky_mask() {
    use lightcraft_develop::{DevelopSettings, LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape};
    use lightcraft_pipeline::{RenderRequest, render};
    use lightcraft_tiff::tags::{self as t, photometric};
    use lightcraft_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value};
    let (w, h) = (16u32, 8u32);
    let image = |ifd: &mut IfdBuilder, w: u32, h: u32, cpp: u16, bits: u16, data: Vec<u8>| {
        ifd.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
        ifd.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
        ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![bits; cpp as usize]));
        ifd.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![cpp]));
        ifd.set(t::COMPRESSION, Value::Short(vec![1]));
        ifd.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![data] });
    };
    let mut raw = IfdBuilder::new();
    raw.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
    raw.set(t::PHOTOMETRIC, Value::Short(vec![photometric::LINEAR_RAW]));
    image(&mut raw, w, h, 3, 16, 12000u16.to_le_bytes().repeat((w * h * 3) as usize));
    let matte = |name: &str, right: bool| {
        let mut m = IfdBuilder::new();
        m.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![t::SUBFILE_SEMANTIC_MASK]));
        m.set(t::PHOTOMETRIC, Value::Short(vec![photometric::MASK]));
        m.set(t::SEMANTIC_NAME, Value::Ascii(name.into()));
        image(&mut m, w / 2, h / 2, 1, 8, (0..w / 2 * h / 2).map(|i| if (i % (w / 2) >= w / 4) == right { 255 } else { 0 }).collect());
        m
    };
    let mut ifd0 = IfdBuilder::new();
    ifd0.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![1]));
    ifd0.set(t::DNG_VERSION, Value::Byte(vec![1, 6, 0, 0]));
    ifd0.set(t::MAKE, Value::Ascii("Apple".into()));
    ifd0.set(t::ORIENTATION, Value::Short(vec![6]));
    ifd0.set(t::COLOR_MATRIX_1, Value::SRational(vec![(1, 1), (0, 1), (0, 1), (0, 1), (1, 1), (0, 1), (0, 1), (0, 1), (1, 1)]));
    ifd0.set(t::CALIBRATION_ILLUMINANT_1, Value::Short(vec![21]));
    ifd0.set(t::PHOTOMETRIC, Value::Short(vec![photometric::RGB]));
    image(&mut ifd0, 4, 2, 3, 8, vec![128; 24]);
    ifd0.add_sub_ifd(raw);
    ifd0.add_sub_ifd(matte("urn:com:apple:photo:2020:aux:semanticskymatte", true));
    ifd0.add_sub_ifd(matte("urn:com:apple:photo:2020:aux:semanticsomethingelse", false));
    let bytes = TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap();

    let (img, info) = load_bytes(&bytes, 64).unwrap();
    assert_eq!((img.width, img.height), (8, 16));
    assert!(info.mattes.is_some());
    let sky = Mask {
        components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape: MaskShape::Sky }],
        adjust: LocalAdjustments { exposure: -3.0, ..Default::default() },
        ..Default::default()
    };
    let s = DevelopSettings { masks: vec![sky], ..Default::default() };
    let req = RenderRequest::fit(8, 16);
    let green = |s: &DevelopSettings, info: &SourceInfo, y: usize| render(&img, info, s, &req).image.get(4, y)[1] as i32;
    let plain = DevelopSettings::default();
    assert_eq!(green(&s, &info, 2), green(&plain, &info, 2), "no sky at the top");
    assert!(green(&s, &info, 13) + 40 < green(&plain, &info, 13), "the sky at the bottom is darkened");
    // without the matte the heuristic looks at the top of the frame instead
    let heuristic = SourceInfo { mattes: None, ..info.clone() };
    assert!(green(&s, &heuristic, 2) < green(&plain, &heuristic, 2));
    assert_eq!(green(&s, &heuristic, 13), green(&plain, &heuristic, 13));
}

/// A DNG's white balance is re-evaluated through its own colour model: the decoder hands the
/// pipeline the file's colour tags (without the profile look, applied at load) and the white
/// the pixels were developed for, and a custom white balance renders a neutral differently
/// from the Bradford adaptation it replaces.
#[test]
fn dng_white_balance_uses_the_camera_colour_model() {
    use lightcraft_pipeline::{RenderRequest, render};
    let dng = crate::tests_xmp::synthetic_dng_with(None, Default::default());
    let raw = lightcraft_raw::decode(&dng).unwrap();
    let (img, info) = load_bytes(&dng, 64).unwrap();
    let cc = info.camera_color.as_ref().expect("a DNG with a colour matrix");
    assert_eq!(cc.developed_for, lightcraft_raw::color::as_shot_white_xy(&raw));
    assert!(cc.tags.profile.is_empty());
    assert_eq!(cc.tags.color_matrix, raw.color.color_matrix);
    let mut s = lightcraft_develop::DevelopSettings::default();
    (s.wb.mode, s.wb.temp, s.wb.tint) = (lightcraft_develop::WbMode::Custom, 3000.0, 20.0);
    let req = RenderRequest::fit(32, 32);
    let camera = render(&img, &info, &s, &req).image;
    let adapted = render(&img, &SourceInfo { camera_color: None, ..info.clone() }, &s, &req).image;
    assert_ne!(camera.data, adapted.data);
}

/// Apple ProRAW: like Lightroom Classic, the default render ignores the file's
/// `ProfileGainTableMap` (Apple's local tone mapping); a map that would double every pixel
/// changes nothing.
#[test]
fn dng_gain_table_map_is_not_rendered() {
    use lightcraft_raw::gaintable::GainTableMap;
    let plain = crate::tests_xmp::synthetic_dng_with(None, Default::default());
    let mut raw = lightcraft_raw::decode(&plain).unwrap();
    let (before, _) = load_bytes(&lightcraft_raw::write_dng(&raw, &Default::default()).unwrap(), 64).unwrap();
    raw.color.profile.gain_table_map = Some(GainTableMap {
        points_v: 1,
        points_h: 1,
        points_n: 1,
        spacing_v: 1.0,
        spacing_h: 1.0,
        origin_v: 0.0,
        origin_h: 0.0,
        weights: [0.2, 0.2, 0.2, 0.2, 0.2],
        gamma: 1.0,
        gains: vec![2.0],
    });
    let with_map = lightcraft_raw::write_dng(&raw, &Default::default()).unwrap();
    assert!(lightcraft_raw::decode(&with_map).unwrap().color.profile.gain_table_map.is_some(), "the map is kept");
    let (after, _) = load_bytes(&with_map, 64).unwrap();
    assert_eq!(before.data, after.data);
}

/// The Profile option "Camera local tone mapping" renders the map by position (in the raw's
/// active area, whatever the photo's orientation), and only when it's on.
#[test]
fn dng_gain_table_map_renders_when_asked() {
    use lightcraft_pipeline::{RenderRequest, render};
    use lightcraft_raw::gaintable::GainTableMap;
    let plain = crate::tests_xmp::synthetic_dng_with(None, Default::default());
    let mut raw = lightcraft_raw::decode(&plain).unwrap();
    // ×1 at the active area's left edge rising to ×4 at its right edge, whatever the colour
    raw.color.profile.gain_table_map = Some(GainTableMap {
        points_v: 1,
        points_h: 2,
        points_n: 1,
        spacing_v: 1.0,
        spacing_h: 1.0,
        origin_v: 0.0,
        origin_h: 0.0,
        weights: [1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0, 0.0, 0.0],
        gamma: 1.0,
        gains: vec![1.0, 4.0],
    });
    let with_map = lightcraft_raw::write_dng(&raw, &Default::default()).unwrap();
    let (img, info) = load_bytes(&with_map, 64).unwrap();
    let (_, plain_info) = load_bytes(&plain, 64).unwrap();
    assert!(info.local_tone.is_some() && plain_info.local_tone.is_none());
    let req = RenderRequest::fit(64, 64);
    let mut s = lightcraft_develop::DevelopSettings::default();
    // (darker, so that ×4 never reaches white)
    s.light.exposure = -3.0;
    let off = render(&img, &info, &s, &req).image;
    assert_eq!(off.data, render(&img, &plain_info, &s, &req).image.data, "off: the map changes nothing");
    s.profile.camera_local_tone = true;
    assert!(s.to_json().to_string().contains("camera_local_tone"), "the option is part of the settings (and their hash)");
    let on = render(&img, &info, &s, &req).image;
    // brightness gained, per column: none at the left edge, most at the right
    let column = |im: &lightcraft_raster::Rgba8, x: usize| (0..im.height).map(|y| im.data[y * im.width + x][1] as f32).sum::<f32>();
    let gain = |x: usize| column(&on, x) / column(&off, x).max(1.0);
    let last = on.width - 1;
    assert!((gain(0) - 1.0).abs() < 0.08, "left ×{}", gain(0));
    assert!(gain(last) > 1.4 && gain(last) > gain(last / 2) && gain(last / 2) > gain(0), "{} {} {}", gain(0), gain(last / 2), gain(last));
    // the photo turned upside down: the map follows the raw, so the brightened side swaps
    s.orientation = lightcraft_geom::Orientation::Rotate180;
    s.profile.camera_local_tone = false;
    let off = render(&img, &info, &s, &req).image;
    s.profile.camera_local_tone = true;
    let on = render(&img, &info, &s, &req).image;
    let gain = |x: usize| column(&on, x) / column(&off, x).max(1.0);
    assert!(gain(0) > 1.4 && (gain(last) - 1.0).abs() < 0.08, "rotated: {} … {}", gain(0), gain(last));
    // a rendered (non-raw) source never gets it
    let jpeg = SourceInfo { raw: false, ..info.clone() };
    assert!(!lightcraft_pipeline::local_tone::enabled(&jpeg, &s));
}

#[test]
fn unsupported_raw_falls_back_to_embedded_preview() {
    let b = cr3_with_preview(48, 32);
    let p = probe_bytes("x.cr3", &b).unwrap();
    assert_eq!((p.width, p.height, p.kind, p.format.as_str()), (48, 32, MediaKind::Raw, "CR3"));
    let (img, src) = load_bytes(&b, 24).unwrap();
    assert_eq!((img.width, img.height), (24, 16));
    assert!(!src.raw);
    // no preview at all: a clear error, not a panic
    assert!(load_bytes(b"\0\0\0\x18ftypcrx \0\0\0\x01", 24).is_err());
}

/// Issue #10: a raw variant we can't decode imports as "preview only" with the decoder's
/// reason, is rendered as the rendered JPEG it is (not as raw), says so to agents, survives a
/// library reopen, and Reload / Relink clear it once the file decodes (undoably).
#[test]
fn unsupported_raw_imports_as_preview_only_and_survives_reopen() {
    use serde_json::json;
    let dir = std::env::temp_dir().join(format!("lc-preview-only-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (lib, files) = (dir.join("lib"), dir.join("files"));
    std::fs::create_dir_all(&files).unwrap();
    let path = files.join("DSC_0001.cr3");
    std::fs::write(&path, cr3_with_preview(96, 64)).unwrap();
    let mut s = crate::Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    // the import review already says so
    let scan = s.execute("library.importPreview", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    assert!(scan["candidates"][0]["previewOnly"].as_str().is_some_and(|w| !w.is_empty()), "{scan}");
    let r = s.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    let id = lightcraft_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
    let p = s.catalog.photo(id).unwrap().clone();
    let why = p.preview_only.clone().expect("marked preview only");
    assert!(why.to_uppercase().contains("CR3"), "the decoder's reason: {why}");
    // stamped with the version that decided it, so a later version rechecks it (#662)
    assert_eq!(p.preview_checked, lightcraft_catalog::APP_VERSION);
    assert!(!p.preview_only_stale());
    assert_eq!(p.kind, MediaKind::Raw, "still a raw file (filters, Convert to DNG…)");
    assert!(!p.develops_raw());
    // rendered like the JPEG it is: relative white balance, display tone curve, no raw defaults
    assert_eq!(SourceInfo { native_long: None, ..crate::media::source_info(&p) }, SourceInfo::default());
    assert_eq!(*p.develop, lightcraft_develop::DevelopSettings::default());
    assert!(s.render_job(id, 48, 48, false, true).unwrap().run().rendered.is_ok());
    // agents see it
    let q = s.execute("catalog.query", &json!({"filter": {}})).unwrap();
    assert_eq!(q["photos"][0]["previewOnly"], json!(why), "{q}");
    let i = s.execute("photo.inspect", &json!({"id": id.0})).unwrap();
    assert_eq!(i["preview_only"], json!(why), "{i}");
    // a library reopen keeps it
    s.persist().unwrap();
    drop(s);
    let mut s = crate::Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().preview_only.as_deref(), Some(why.as_str()));
    assert_eq!(s.catalog.photo(id).unwrap().preview_checked, lightcraft_catalog::APP_VERSION);
    // the file decodes now (here: replaced by a DNG): Reload clears it, undo brings it back
    std::fs::write(&path, crate::tests_xmp::synthetic_dng_with(None, lightcraft_meta::Metadata::default())).unwrap();
    let r = s.execute("photo.reload", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(r["reloaded"], json!([id.0]), "{r}");
    assert!(s.catalog.photo(id).unwrap().develops_raw());
    assert_eq!(s.catalog.photo(id).unwrap().preview_checked, "", "the stamp goes with the flag");
    assert!(crate::media::source_info(s.catalog.photo(id).unwrap()).raw);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().preview_only.as_deref(), Some(why.as_str()));
    assert_eq!(s.catalog.photo(id).unwrap().preview_checked, lightcraft_catalog::APP_VERSION);
    // relinking to a file that decodes clears it too
    let dng = files.join("DSC_0001.dng");
    std::fs::write(&dng, crate::tests_xmp::synthetic_dng_with(None, lightcraft_meta::Metadata::default())).unwrap();
    s.execute("photo.relink", &json!({"id": id.0, "path": dng.to_string_lossy()})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().preview_only, None);
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A raw whose TIFF shell holds only a small RGB image (IFD0) next to a private block, as Exif
/// declares a picture 25x its size.
fn tiff_shell(w: u32, h: u32, shell: bool) -> Vec<u8> {
    use lightcraft_tiff::{IfdBuilder, ImageData, TiffWriter, Value, tags as t};
    let mut ifd0 = IfdBuilder::new();
    ifd0.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
    ifd0.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
    ifd0.set(t::BITS_PER_SAMPLE, Value::Short(vec![8, 8, 8]));
    ifd0.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![3]));
    ifd0.set(t::PHOTOMETRIC, Value::Short(vec![2]));
    ifd0.set(t::COMPRESSION, Value::Short(vec![1]));
    let px: Vec<u8> = (0..w * h).flat_map(|i| [(i % 251) as u8, 128, 200]).collect();
    ifd0.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![px] });
    ifd0.set_child(
        t::EXIF_IFD,
        IfdBuilder::new().with(t::PIXEL_X_DIMENSION, Value::Long(vec![w * 25])).with(t::PIXEL_Y_DIMENSION, Value::Long(vec![h * 25])),
    );
    let mut b = TiffWriter::default().write(&[ifd0]).unwrap();
    if shell {
        b.extend(std::iter::repeat_n(0x5au8, (w * h * 625 / 8) as usize + 100));
    }
    b
}

/// A TIFF raw whose only image is a small thumbnail used to open as a plain image of that size
/// ("a 150 MP file renders 296x220"), with nothing saying so. Now it is recognised as a raw we can't
/// decode: preview only, with the reason, still showing that image.
#[test]
fn thumbnail_only_tiff_raw_is_preview_only_not_a_plain_image() {
    let b = tiff_shell(24, 16, true);
    let p = probe_bytes("IMG_0001.IIQ", &b).unwrap();
    assert_eq!((p.kind, p.format.as_str(), p.width, p.height), (MediaKind::Raw, "IIQ", 24, 16));
    let why = p.preview_only.expect("marked preview only");
    assert!(why.contains("600x400") && why.contains("24x16"), "{why}");
    let (img, src) = load_bytes(&b, 12).unwrap();
    assert_eq!((img.width, img.height, src.raw), (12, 8, false));
    assert!(embedded_preview_srgb(&b, 12).is_some());
    // the same pixels without the private block are a small ordinary image
    let plain = probe_bytes("small.tif", &tiff_shell(24, 16, false)).unwrap();
    assert_eq!((plain.kind, plain.preview_only, plain.width, plain.height), (MediaKind::Image, None, 24, 16));
    assert!(lightcraft_raw::probe(&tiff_shell(24, 16, false)).is_none());

    // TIFFs preserving camera Make tags (SONY, NIKON, etc.) are ordinary images, not raws (#281)
    for make in ["SONY", "NIKON CORPORATION", "PENTAX", "RICOH", "SAMSUNG"] {
        use lightcraft_tiff::{IfdBuilder, ImageData, TiffWriter, Value, tags as t};
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::IMAGE_WIDTH, Value::Long(vec![24]));
        ifd0.set(t::IMAGE_LENGTH, Value::Long(vec![16]));
        ifd0.set(t::BITS_PER_SAMPLE, Value::Short(vec![8, 8, 8]));
        ifd0.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![3]));
        ifd0.set(t::PHOTOMETRIC, Value::Short(vec![2]));
        ifd0.set(t::COMPRESSION, Value::Short(vec![1]));
        ifd0.set(t::MAKE, Value::Ascii(make.into()));
        let px: Vec<u8> = (0..24 * 16).flat_map(|i| [(i % 251) as u8, 128, 200]).collect();
        ifd0.set_image(ImageData::Strips { rows_per_strip: 16, strips: vec![px] });
        let bytes = TiffWriter::default().write(&[ifd0]).unwrap();
        let p = probe_bytes("photo.tif", &bytes).unwrap();
        assert_eq!((p.kind, p.preview_only, p.width, p.height), (MediaKind::Image, None, 24, 16), "make: {make}");
        let (img, src) = load_bytes(&bytes, 24).unwrap();
        assert_eq!((img.width, img.height, src.raw), (24, 16, false), "make: {make}");
    }
}

/// Recognised raw containers we can't decode (an MRW whose block header is damaged here; its preview's first
/// byte is overwritten, as in real MRW files) import as preview only with the JPEG's size, instead of failing.
#[test]
fn undecodable_container_with_a_preview_imports_as_preview_only() {
    let px: Vec<u8> = (0..40 * 30).flat_map(|i| [(i % 251) as u8, 128, 200]).collect();
    let mut jpeg = encode_jpeg(&EncodeImage::new(40, 30, 3, Samples::U8(&px)), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
    jpeg[0] = 0x02;
    let mut f = b"\0MRM\0\x01\0\0".to_vec();
    f.extend(std::iter::repeat_n(0x11u8, 300));
    f.extend_from_slice(&jpeg);
    f.extend(std::iter::repeat_n(0x22u8, 300));
    let p = probe_bytes("A.MRW", &f).unwrap();
    assert_eq!((p.kind, p.format.as_str(), p.width, p.height), (MediaKind::Raw, "MRW", 40, 30));
    assert!(p.preview_only.is_some_and(|w| w.contains("Mrw")));
    let (img, src) = load_bytes(&f, 20).unwrap();
    assert_eq!((img.width, img.height, src.raw), (20, 15, false));
    // without a JPEG: a clear error
    let e = probe_bytes("A.MRW", b"\0MRM\0\x01\0\0 nothing here").unwrap_err();
    assert!(e.contains("without an embedded preview"), "{e}");
}

/// Issue #1275: an uncompressed Leaf MOS (IFD0 is the 16-bit mosaic, no Make tag; Leaf's data block `0x8606`
/// holds the preview) imports as a raw shown from its preview, not as a greyscale image of the mosaic.
#[test]
fn uncompressed_leaf_mos_imports_as_a_preview_only_raw() {
    use lightcraft_tiff::{IfdBuilder, ImageData, TiffWriter, Value, tags as t};
    let px: Vec<u8> = (0..40 * 30).flat_map(|i| [(i % 251) as u8, 128, 200]).collect();
    let jpeg = encode_jpeg(&EncodeImage::new(40, 30, 3, Samples::U8(&px)), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
    let mos = |leaf_data: bool| {
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::IMAGE_WIDTH, Value::Long(vec![16]));
        ifd0.set(t::IMAGE_LENGTH, Value::Long(vec![12]));
        ifd0.set(t::BITS_PER_SAMPLE, Value::Short(vec![16]));
        ifd0.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![1]));
        ifd0.set(t::PHOTOMETRIC, Value::Short(vec![1]));
        ifd0.set(t::COMPRESSION, Value::Short(vec![1]));
        ifd0.set_image(ImageData::Tiles { tile_width: 16, tile_height: 12, tiles: vec![vec![0x30; 16 * 12 * 2]] });
        if leaf_data {
            ifd0.set(0x8606, Value::Undefined([&b"PKTS\0\0\0\x01"[..], &jpeg].concat()));
        }
        TiffWriter::default().write(&[ifd0]).unwrap()
    };
    let p = probe_bytes("aptus22.mos", &mos(true)).unwrap();
    assert_eq!((p.kind, p.format.as_str(), p.width, p.height), (MediaKind::Raw, "MOS", 40, 30));
    assert!(p.preview_only.is_some_and(|why| !why.is_empty()));
    let (img, src) = load_bytes(&mos(true), 20).unwrap();
    assert_eq!((img.width, img.height, src.raw), (20, 15, false));
    // the same grey image without Leaf's block is an ordinary 16-bit TIFF
    let plain = probe_bytes("grey.tif", &mos(false)).unwrap();
    assert_eq!((plain.kind, plain.preview_only, plain.width, plain.height), (MediaKind::Image, None, 16, 12));
}

/// A GoPro GPR (DNG with VC-5 coded data, no preview anywhere) still imports, described from its
/// headers; loading it says what is wrong instead of naming a bare TIFF compression number.
#[test]
fn gopro_vc5_dng_imports_and_fails_to_load_with_a_clear_reason() {
    use lightcraft_tiff::{IfdBuilder, ImageData, TiffWriter, Value, tags as t};
    let mut ifd = IfdBuilder::new();
    ifd.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
    ifd.set(t::IMAGE_WIDTH, Value::Long(vec![32]));
    ifd.set(t::IMAGE_LENGTH, Value::Long(vec![16]));
    ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![16]));
    ifd.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![1]));
    ifd.set(t::PHOTOMETRIC, Value::Short(vec![32803]));
    ifd.set(t::COMPRESSION, Value::Short(vec![9]));
    ifd.set(t::DNG_VERSION, Value::Byte(vec![1, 4, 0, 0]));
    ifd.set(t::CFA_REPEAT_PATTERN_DIM, Value::Short(vec![2, 2]));
    ifd.set(t::CFA_PATTERN_EP, Value::Byte(vec![0, 1, 1, 2]));
    ifd.set_image(ImageData::Tiles { tile_width: 32, tile_height: 16, tiles: vec![vec![0x5au8; 400]] });
    let f = TiffWriter::default().write(&[ifd]).unwrap();
    let p = probe_bytes("GOPR0001.GPR", &f).unwrap();
    assert_eq!((p.kind, p.preview_only, p.width, p.height), (MediaKind::Raw, None, 32, 16));
    let e = load_bytes(&f, 16).unwrap_err();
    assert!(e.contains("GoPro VC-5") && e.contains("not decoded yet"), "{e}");
}

/// Any failure of a recognised raw (not only an unsupported variant) can fall back to its preview;
/// CR3 keeps its own wording.
#[test]
fn decode_failures_of_recognised_raws_may_fall_back_to_the_preview() {
    use lightcraft_raw::RawError;
    let cr3 = b"\0\0\0\x18ftypcrx \0\0\0\x01";
    assert_eq!(preview_reason(b"x", RawError::Unsupported("x".into())), Ok("x".to_string()));
    assert!(preview_reason(b"x", RawError::Corrupt("bad strip".into())).is_ok_and(|w| w.contains("bad strip")));
    assert!(preview_reason(b"x", RawError::Limit("too big")).is_ok());
    assert!(preview_reason(b"x", RawError::Tiff(lightcraft_tiff::TiffError::MissingTag(256))).is_ok());
    assert!(preview_reason(b"x", RawError::NotRaw).is_err());
    assert!(preview_reason(cr3, RawError::Corrupt("bad".into())).is_ok_and(|w| w.starts_with("CR3 ")));
}

#[test]
fn quick_jobs_show_embedded_previews_then_cached_renders() {
    use crate::media::QuickSource;
    let dir = std::env::temp_dir().join(format!("lc-quick-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("a.cr3");
    std::fs::write(&path, cr3_with_preview(96, 64)).unwrap();
    let mut s = crate::Session::new().with_fs();
    let r = s.execute("library.import", &serde_json::json!({"paths": [path.to_string_lossy()]})).unwrap();
    let id = lightcraft_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
    // loupe: an unedited raw opens on its embedded preview…
    let q = s.quick_view_job(id, 1600, true).unwrap().run();
    assert_eq!(q.quick, Some(QuickSource::Embedded));
    let img = q.rendered.unwrap().image;
    assert_eq!((img.width, img.height), (96, 64));
    // …and once the loupe has rendered it, on that render (any size)
    let full = s.loupe_job(id, 48, 32, true).unwrap().run();
    assert!(full.rendered.is_ok() && full.quick.is_none());
    let q = s.quick_view_job(id, 1600, true).unwrap().run();
    assert_eq!(q.quick, Some(QuickSource::Cached));
    assert_eq!(q.rendered.unwrap().image.width, 48);
    // grid: embedded first, then the real thumbnail (final, with the thumbnail job's key)
    let job = s.thumb_job(id, 128).unwrap();
    let q = s.quick_thumb_job(&job).unwrap().run();
    assert_eq!((q.quick, q.key), (Some(QuickSource::Embedded), job.key));
    drop(job.clone().run());
    assert_eq!(s.quick_thumb_job(&job).unwrap().run().quick, Some(QuickSource::Cached));
    // an edited raw: no embedded stand-in (it wouldn't show the edit)
    s.execute("library.select", &serde_json::json!({"ids": [id.0]})).unwrap();
    s.execute("develop.set", &serde_json::json!({"control": "light.exposure", "value": 1.0})).unwrap();
    let job = s.thumb_job(id, 128).unwrap();
    assert!(s.quick_thumb_job(&job).is_none());
    assert_eq!(s.quick_view_job(id, 1600, true).unwrap().run().quick, Some(QuickSource::Small));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #1448: the grid renders nothing behind an unedited raw's quick thumbnail, so when the
/// embedded preview can't be read (and nothing is cached) the quick job itself must render the
/// raw, or the cell stays empty for good ("no quick preview").
#[test]
fn a_quick_thumbnail_without_an_embedded_preview_renders_the_raw() {
    use crate::media::QuickSource;
    let dir = std::env::temp_dir().join(format!("lc-quick-none-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("a.cr3");
    std::fs::write(&path, cr3_with_preview(96, 64)).unwrap();
    let mut s = crate::Session::new().with_fs();
    let r = s.execute("library.import", &serde_json::json!({"paths": [path.to_string_lossy()]})).unwrap();
    let id = lightcraft_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
    s.media.preview_loader = Some(std::sync::Arc::new(|_: &str, _| None));
    let job = s.thumb_job(id, 128).unwrap();
    let q = s.quick_thumb_job(&job).unwrap().run();
    assert_eq!((q.quick, q.key), (Some(QuickSource::Small), job.key));
    assert!(q.rendered.is_ok(), "{:?}", q.rendered.err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A Sony ARW of `model`: a 64 × 48 uncompressed 14-bit CFA strip in a SubIFD (as `crates/raw/tests/it/vendor_robust.rs`
/// lays it out) whose IFD carries the full-frame 16–35 mm distortion table (tag 0x7037).
fn sony_arw(model: &str) -> Vec<u8> {
    use lightcraft_tiff::tags::{self as t, photometric};
    use lightcraft_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value};
    // 3:2, the aspect Sony's lens opcodes are limited to
    let (w, h) = (72u32, 48u32);
    let mut raw = IfdBuilder::new();
    raw.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
    raw.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
    raw.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
    raw.set(t::BITS_PER_SAMPLE, Value::Short(vec![14]));
    raw.set(t::COMPRESSION, Value::Short(vec![1]));
    raw.set(t::PHOTOMETRIC, Value::Short(vec![photometric::CFA]));
    raw.set(t::CFA_REPEAT_PATTERN_DIM, Value::Short(vec![2, 2]));
    raw.set(t::CFA_PATTERN_EP, Value::Byte(vec![0, 1, 1, 2]));
    raw.set(0x7037, Value::SShort(vec![16, 0, 2, 7, 16, 28, 43, 61, 83, 109, 139, 175, 217, 267, 325, 395, 479]));
    raw.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![2000u16.to_le_bytes().repeat((w * h) as usize)] });
    let mut ifd0 = IfdBuilder::new();
    ifd0.set(t::MAKE, Value::Ascii("SONY".into()));
    ifd0.set(t::MODEL, Value::Ascii(model.into()));
    ifd0.add_sub_ifd(raw);
    TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap()
}

/// Sony's embedded distortion correction is on by default: an ARW of a verified body imports with "Enable Lens
/// Corrections" on and still counts as unedited; another body's tables aren't read, so its correction is off.
#[test]
fn arw_with_lens_data_imports_with_profile_corrections_on() {
    let dir = std::env::temp_dir().join(format!("lc-arw-lens-default-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = crate::Session::new().with_fs();
    for (model, on) in [("ILCE-7RM6", true), ("ILCE-7M4", false)] {
        let path = dir.join(format!("{model}.arw"));
        std::fs::write(&path, sony_arw(model)).unwrap();
        let r = s.execute("library.import", &serde_json::json!({"paths": [path.to_string_lossy()]})).unwrap();
        let p = s.catalog.photo(lightcraft_catalog::PhotoId(r["imported"][0].as_u64().unwrap())).unwrap();
        assert_eq!((p.format.as_str(), p.embedded_lens.is_some(), p.develop.optics.lens_profile), ("ARW", on, on), "{model}");
        assert!(!p.is_edited(), "{model}: as imported");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// RW2 and DNG keep their default: the correction is on when the file carries one (RW2 distortion, a DNG's
/// opcodes), off when it doesn't.
#[test]
fn rw2_and_dng_defaults_unchanged() {
    use lightcraft_catalog::{Photo, PhotoId, Source};
    let s = crate::Session::new();
    let lens = lightcraft_develop::EmbeddedLens { warp: Some(Default::default()), ..Default::default() };
    for format in ["RW2", "DNG"] {
        for embedded in [None, Some(lens)] {
            let name = format!("x.{format}");
            let mut p = Photo::new(PhotoId(1), Source::File { path: format!("/{name}") }, &name, format, 64, 48, "");
            p.kind = MediaKind::Raw;
            p.embedded_lens = embedded;
            crate::import::apply_import_defaults(&s, &mut p);
            assert_eq!(p.develop.optics.lens_profile, embedded.is_some(), "{format} {embedded:?}");
        }
    }
}

/// Stored settings stay as they are when the library opens again: an ARW catalogued before its lens data was read
/// (none, correction off) keeps the correction off and counts as unedited, and one whose user switched the
/// correction off keeps it off.
#[test]
fn existing_photo_settings_are_not_rewritten() {
    use lightcraft_catalog::{Op, PhotoId};
    let dir = std::env::temp_dir().join(format!("lc-arw-lens-existing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (lib, files) = (dir.join("lib"), dir.join("files"));
    std::fs::create_dir_all(&files).unwrap();
    let mut s = crate::Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let import = |s: &mut crate::Session, name: &str| {
        let path = files.join(name);
        // trailing bytes of its own: not a duplicate of the other file
        std::fs::write(&path, [sony_arw("ILCE-7RM6"), name.as_bytes().to_vec()].concat()).unwrap();
        let r = s.execute("library.import", &serde_json::json!({"paths": [path.to_string_lossy()]})).unwrap();
        PhotoId(r["imported"][0].as_u64().unwrap())
    };
    let old = import(&mut s, "old.arw");
    // as a release that didn't read the Sony tables catalogued it
    let mut d = (*s.catalog.photo(old).unwrap().develop).clone();
    d.optics.lens_profile = false;
    let ops = vec![
        Op::SetEmbeddedLens { id: old, lens: None },
        Op::SetDevelop { id: old, settings: std::sync::Arc::new(d), label: "x".into(), edited: None },
    ];
    s.commit("x", Op::Batch { ops }).unwrap();
    let off = import(&mut s, "off.arw");
    s.execute("develop.merge", &serde_json::json!({"settings": {"optics": {"lens_profile": false}}})).unwrap();
    let before: Vec<_> = [old, off].iter().map(|id| s.catalog.photo(*id).unwrap().clone()).collect();
    assert!(before.iter().all(|p| !p.develop.optics.lens_profile));
    s.persist().unwrap();
    drop(s);
    let mut s = crate::Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    for b in &before {
        let p = s.catalog.photo(b.id).unwrap();
        assert_eq!((&p.develop, &p.embedded_lens), (&b.develop, &b.embedded_lens), "{}", p.file_name);
    }
    assert!(!s.catalog.photo(old).unwrap().is_edited(), "still as imported");
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}
