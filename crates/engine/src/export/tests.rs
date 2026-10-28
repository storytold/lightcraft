use super::*;
use serde_json::json;

fn test_image() -> Rgba8 {
    let mut img = Rgba8::new(96, 64);
    for (i, p) in img.data.iter_mut().enumerate() {
        let (x, y) = (i % 96, i / 96);
        *p = [(x * 2) as u8, (y * 3) as u8, ((x * y) % 255) as u8, 255];
    }
    img
}

/// Issue #1670: removing a redundant alpha channel preserves samples and metadata.
#[test]
fn opaque_eight_bit_exports_are_rgb() {
    let img = test_image();
    let metadata = Metadata { copyright: Some("Synthetic export test".into()), ..Default::default() };
    for format in [ExportFormat::Png, ExportFormat::Tiff, ExportFormat::Webp] {
        for compression in [TiffCompression::None, TiffCompression::Lzw, TiffCompression::Deflate, TiffCompression::PackBits] {
            let options = ExportOptions { format, tiff_compression: compression, ..Default::default() };
            let bytes = encode_with_metadata(&img, &options, Some(&metadata)).unwrap();
            let decoded = lightcraft_codecs::decode(&bytes, Default::default()).unwrap();
            assert!(!decoded.has_alpha, "{format:?}/{compression:?} must be RGB");
            assert_eq!((decoded.width, decoded.height, decoded.bit_depth), (96, 64, 8));
            assert_eq!(decoded.to_srgb8().data, img.data, "{format:?}/{compression:?} pixels");
            assert_eq!(decoded.icc.as_deref(), Some(icc::write_named(NamedSpace::Srgb).as_slice()));
            assert!(decoded.xmp.as_deref().unwrap_or_default().contains("Synthetic export test"));
            if format == ExportFormat::Png {
                assert_eq!(bytes[25], 2, "PNG IHDR declares RGB");
            }
            if format != ExportFormat::Tiff {
                break;
            }
        }
    }
}

#[test]
fn eight_bit_exports_preserve_real_alpha() {
    let mut img = test_image();
    img.data[0][3] = 128;
    for format in [ExportFormat::Png, ExportFormat::Tiff, ExportFormat::Webp] {
        let bytes = encode_image(&img, &ExportOptions { format, ..Default::default() }).unwrap();
        let decoded = lightcraft_codecs::decode(&bytes, Default::default()).unwrap();
        assert!(decoded.has_alpha, "{format:?} must retain real alpha");
        assert_eq!(decoded.to_srgb8().data, img.data, "{format:?} samples");
    }
}

#[test]
fn formats_have_magic() {
    let img = test_image();
    for (f, magic) in
        [(ExportFormat::Jpeg, &b"\xFF\xD8"[..]), (ExportFormat::Png, b"\x89PNG"), (ExportFormat::Tiff, b"II"), (ExportFormat::Webp, b"RIFF")]
    {
        let b = encode_image(&img, &ExportOptions { format: f, ..Default::default() }).unwrap();
        assert!(b.starts_with(magic), "{f:?}");
    }
}

#[test]
fn size_limit_respected() {
    let img = test_image();
    let full = encode_image(&img, &ExportOptions { quality: 100, ..Default::default() }).unwrap();
    let kb = (full.len() / 1024 / 2).max(2) as u32;
    let b = encode_image(&img, &ExportOptions { quality: 100, limit_kb: Some(kb), ..Default::default() }).unwrap();
    assert!(b.len() <= kb as usize * 1024);
}

#[test]
fn hdr_jpeg_size_limit_respected() {
    let img = Rgba8::from_fn(256, 192, |x, y| {
        let n = (x as u32).wrapping_mul(73856093) ^ (y as u32).wrapping_mul(19349663);
        [n as u8, (n >> 8) as u8, (n >> 16) as u8, 255]
    });
    let hdr = DeepImage {
        width: img.width,
        height: img.height,
        space: OutputSpace::Srgb,
        samples: DeepSamples::F32(img.data.iter().flat_map(|p| p[..3].iter().map(|v| lightcraft_color::transfer::decode_srgb8(*v) * 4.0)).collect()),
    };
    let opts = ExportOptions { hdr: true, quality: 100, limit_kb: Some(u32::MAX), ..Default::default() };
    let full = encode_gain_map_jpeg(&img, &hdr, &opts, None).unwrap();
    let kb = (full.len() / 2048) as u32;
    let limited = encode_gain_map_jpeg(&img, &hdr, &ExportOptions { limit_kb: Some(kb), ..opts.clone() }, None).unwrap();
    assert!(limited.len() <= kb as usize * 1024, "{} bytes exceeds {kb} KB", limited.len());
    let decoded = lightcraft_codecs::gainmap::read_jpeg(&limited).unwrap();
    assert!(decoded.meta.alternate_headroom > 1.0, "the gain map must survive size limiting");
    let impossible = encode_gain_map_jpeg(&img, &hdr, &ExportOptions { limit_kb: Some(1), ..opts }, None);
    assert!(impossible.is_err_and(|e| e.contains("cannot fit within 1 KB")));
}

#[test]
fn graphic_watermark_honours_exif_orientation() {
    let dir = std::env::temp_dir().join(format!("lc-wm-orientation-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let logo = Rgba8::from_fn(40, 20, |x, _| if x < 20 { [255, 0, 0, 255] } else { [0, 0, 255, 255] });
    for orientation in [2, 6] {
        let meta = Metadata { orientation: Some(lightcraft_geom::Orientation::from_exif(orientation)), ..Default::default() };
        let exif = lightcraft_meta::write_exif(&meta);
        let bytes = lightcraft_codecs::encode_jpeg(
            &EncodeImage::rgba8(&logo),
            100,
            ChromaSubsampling::S444,
            &EncodeMeta { exif: Some(&exif), ..Default::default() },
        )
        .unwrap();
        let path = dir.join(format!("oriented-{orientation}.jpg"));
        std::fs::write(&path, bytes).unwrap();
        let wm = Watermark {
            image: path.to_string_lossy().into(),
            image_width: if orientation == 6 { 0.25 } else { 0.5 },
            opacity: 1.0,
            inset_x: 0.0,
            inset_y: 0.0,
            ..Default::default()
        };
        let mut img = Rgba8::from_fn(80, 80, |_, _| [0, 0, 0, 255]);
        draw_watermark(&mut img, &wm);
        if orientation == 6 {
            assert!(img.get(70, 45)[0] > 240 && img.get(70, 75)[2] > 240, "EXIF 6: red above blue");
        } else {
            assert!(img.get(45, 70)[2] > 240 && img.get(75, 70)[0] > 240, "EXIF 2: blue left of red");
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn graphic_watermark_keeps_aspect_when_fitted_and_rotated() {
    let dir = std::env::temp_dir().join(format!("lc-wm-aspect-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tall.png");
    let logo = Rgba8::from_fn(40, 160, |_, _| [255, 0, 0, 255]);
    std::fs::write(&path, lightcraft_codecs::encode_png(&EncodeImage::rgba8(&logo), &EncodeMeta::default()).unwrap()).unwrap();
    for (rotation, expected) in [(0, (20, 80)), (90, (120, 30)), (180, (20, 80)), (270, (120, 30))] {
        let wm = Watermark {
            image: path.to_string_lossy().into(),
            image_width: 0.5,
            opacity: 1.0,
            inset_x: 0.0,
            inset_y: 0.0,
            rotation,
            ..Default::default()
        };
        let mut img = Rgba8::from_fn(120, 80, |_, _| [0, 0, 0, 255]);
        draw_watermark(&mut img, &wm);
        let lit: Vec<_> = img.data.iter().enumerate().filter(|(_, p)| p[0] > 200).map(|(i, _)| (i % 120, i / 120)).collect();
        let bounds = (
            lit.iter().map(|p| p.0).max().unwrap() - lit.iter().map(|p| p.0).min().unwrap() + 1,
            lit.iter().map(|p| p.1).max().unwrap() - lit.iter().map(|p| p.1).min().unwrap() + 1,
        );
        assert_eq!(bounds, expected, "{rotation} degrees: fitting must preserve the logo's aspect ratio");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn graphic_watermark_fit_preserves_anchor_insets() {
    let dir = std::env::temp_dir().join(format!("lc-wm-insets-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tall.png");
    let logo = Rgba8::from_fn(40, 160, |_, _| [255, 0, 0, 255]);
    std::fs::write(&path, lightcraft_codecs::encode_png(&EncodeImage::rgba8(&logo), &EncodeMeta::default()).unwrap()).unwrap();
    use Anchor::*;
    for (anchor, rotation, expected) in [
        (BottomRight, 90, (0, 44, 111, 71)),
        (TopLeft, 270, (8, 8, 119, 35)),
        (Right, 90, (0, 26, 111, 53)),
        (BottomRight, 0, (94, 0, 111, 71)),
        (TopLeft, 180, (8, 8, 25, 79)),
        (Bottom, 0, (51, 0, 68, 71)),
        // Centered axes ignore their inset rather than shrinking the image unnecessarily.
        (Center, 90, (0, 25, 119, 54)),
        (Top, 90, (0, 8, 119, 37)),
    ] {
        let wm = Watermark {
            image: path.to_string_lossy().into(),
            image_width: 0.5,
            opacity: 1.0,
            inset_x: 0.1,
            inset_y: 0.1,
            rotation,
            anchor,
            ..Default::default()
        };
        let mut img = Rgba8::from_fn(120, 80, |_, _| [0, 0, 0, 255]);
        draw_watermark(&mut img, &wm);
        let lit: Vec<_> = img.data.iter().enumerate().filter(|(_, p)| p[0] > 200).map(|(i, _)| (i % 120, i / 120)).collect();
        let bounds = (
            lit.iter().map(|p| p.0).min().unwrap(),
            lit.iter().map(|p| p.1).min().unwrap(),
            lit.iter().map(|p| p.0).max().unwrap(),
            lit.iter().map(|p| p.1).max().unwrap(),
        );
        assert_eq!(bounds, expected, "{anchor:?}, {rotation} degrees");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn sharpen_increases_edge_contrast() {
    let mut img = Rgba8::new(8, 8);
    for (i, p) in img.data.iter_mut().enumerate() {
        let v = if i % 8 < 4 { 80 } else { 170 };
        *p = [v, v, v, 255];
    }
    output_sharpen(&mut img, SharpenFor::Matte, SharpenAmount::High);
    assert!(img.data[3][0] < 80 && img.data[4][0] > 170);
}

/// Output sharpening's pixels for every target and amount, pinned (its box blurs run row-parallel, which must not
/// change a bit), on 8- and 16-bit images; with no target the image is left as it is.
#[test]
fn sharpened_pixels_are_pinned() {
    let fnv = |b: &[u8]| b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &x| (h ^ u64::from(x)).wrapping_mul(0x100_0000_01b3));
    let mut got = Vec::new();
    for target in [SharpenFor::Screen, SharpenFor::Matte, SharpenFor::Glossy] {
        for amount in [SharpenAmount::Low, SharpenAmount::High] {
            let mut img = test_image();
            output_sharpen(&mut img, target, amount);
            got.push(fnv(img.data.as_flattened()));
        }
        let src = test_image();
        let samples = src.data.iter().flat_map(|p| [p[0], p[1], p[2]].map(|v| u16::from(v) * 257)).collect();
        let mut deep = DeepImage { width: src.width, height: src.height, samples: DeepSamples::U16(samples), space: OutputSpace::Srgb };
        output_sharpen_deep(&mut deep, target, SharpenAmount::Standard);
        let DeepSamples::U16(v) = &deep.samples else { unreachable!() };
        got.push(fnv(&v.iter().flat_map(|s| s.to_le_bytes()).collect::<Vec<u8>>()));
    }
    assert_eq!(got, SHARPENED, "{got:#x?}");
    let mut img = test_image();
    output_sharpen(&mut img, SharpenFor::None, SharpenAmount::High);
    assert_eq!(img, test_image());
}
const SHARPENED: [u64; 9] = [
    0x1494_273e_0bdc_a61c,
    0x3fbb_40c4_74f4_b0c3,
    0xa27f_8ff5_82a0_99f9,
    0x6550_6571_a38d_4d9f,
    0x5e28_f4f3_fc8a_9685,
    0xdc2b_6f65_9138_3b1d,
    0x8ce6_f0b8_d8ed_8026,
    0x92d8_12e7_4b2e_81fe,
    0x139f_6d78_b1f2_a910,
];

/// Exported files carry keywords as Lightroom Classic writes them: names flat in `dc:subject`
/// (with the keywords containing them), full paths in `lr:hierarchicalSubject`, and nothing of
/// a keyword left out of export. They used to carry the `a|b` paths in `dc:subject`.
#[test]
fn exported_keywords_follow_their_options() {
    use lightcraft_catalog::{Catalog, Op, Photo, PhotoId, Source, keywords::KeywordInfo};
    let mut c = Catalog::new();
    c.apply(Op::SetKeyword { path: "Places".into(), info: Some(KeywordInfo { include_on_export: false, ..KeywordInfo::default() }) }).unwrap();
    c.apply(Op::SetKeyword { path: "draft".into(), info: Some(KeywordInfo { include_on_export: false, ..KeywordInfo::default() }) }).unwrap();
    let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, "a.jpg", "jpeg", 10, 10, "2026-09-30T00:00:00");
    p.meta.keywords = vec!["Places|Lisbon".into(), "travel|Italy".into(), "draft".into()];
    let m = export_metadata(&p, &c, &ExportOptions::default()).unwrap();
    assert_eq!(m.keywords, ["Lisbon", "Italy", "travel"]);
    assert_eq!(m.hierarchical_keywords, ["Places|Lisbon", "travel|Italy"]);
    let jpg = encode_with_metadata(&test_image(), &ExportOptions::default(), Some(&m)).unwrap();
    let back = lightcraft_meta::extract(&jpg);
    assert_eq!((back.keywords, back.hierarchical_keywords), (m.keywords.clone(), m.hierarchical_keywords.clone()));
}

#[test]
fn metadata_policies() {
    use lightcraft_catalog::{Catalog, Photo, PhotoId, Source};
    let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, "a.jpg", "jpeg", 10, 10, "2026-09-30T00:00:00");
    p.meta.camera = "Synthetic X2".into();
    p.meta.copyright = "(c) Me".into();
    p.meta.copyright_status = lightcraft_catalog::CopyrightStatus::Copyrighted;
    p.meta.usage_terms = "Editorial use only".into();
    p.meta.copyright_url = "https://example.com/rights".into();
    p.meta.shutter = "1/250".into();
    p.meta.gps = Some((43.0, -110.0));
    let all = export_metadata(&p, &Catalog::new(), &ExportOptions::default()).unwrap();
    assert_eq!(all.model.as_deref(), Some("Synthetic X2"));
    assert!((all.exposure_time.unwrap() - 0.004).abs() < 1e-9);
    assert!(all.gps.is_some());
    let o = ExportOptions { metadata: MetadataPolicy::AllExceptCamera, remove_location: true, ..Default::default() };
    let m = export_metadata(&p, &Catalog::new(), &o).unwrap();
    assert!(m.model.is_none() && m.gps.is_none() && m.copyright.is_some());
    let c = export_metadata(&p, &Catalog::new(), &ExportOptions { metadata: MetadataPolicy::Copyright, ..Default::default() }).unwrap();
    assert!(c.model.is_none() && c.gps.is_none() && c.copyright.as_deref() == Some("(c) Me"));
    // "copyright only" keeps all the copyright info: status, usage terms, info URL
    assert_eq!(
        (c.copyright_marked, c.usage_terms.as_deref(), c.copyright_url.as_deref()),
        (Some(true), Some("Editorial use only"), Some("https://example.com/rights"))
    );
    assert!(export_metadata(&p, &Catalog::new(), &ExportOptions { metadata: MetadataPolicy::None, ..Default::default() }).is_none());
    // embedded and readable back from the JPEG
    let jpg = encode_with_metadata(&test_image(), &ExportOptions::default(), Some(&all)).unwrap();
    let back = lightcraft_meta::extract(&jpg);
    assert_eq!(back.model.as_deref(), Some("Synthetic X2"));
    assert_eq!(back.copyright.as_deref(), Some("(c) Me"));
    assert_eq!((back.copyright_marked, back.usage_terms.as_deref()), (Some(true), Some("Editorial use only")));
    assert_eq!(back.copyright_url.as_deref(), Some("https://example.com/rights"));
    assert!(back.gps.is_some());
}

#[test]
fn watermark_draws_in_the_anchored_corner_only() {
    let mut img = Rgba8::new(400, 300);
    for p in img.data.iter_mut() {
        *p = [0, 0, 0, 255];
    }
    let o = ExportOptions::from_json(&serde_json::json!({"watermark": {"text": "LightCraft", "size": 0.08, "opacity": 1.0, "shadow": false}}));
    let wm = o.watermark.clone().unwrap();
    draw_watermark(&mut img, &wm);
    let lit = |x0: usize, x1: usize, y0: usize, y1: usize| {
        (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| img.get(x, y)[0] > 128).count()
    };
    assert!(lit(200, 400, 225, 300) > 100, "bottom-right has text");
    assert!(lit(0, 200, 0, 150) == 0, "top-left untouched");
    // string shorthand
    assert_eq!(ExportOptions::from_json(&serde_json::json!({"watermark": "© Me"})).watermark.unwrap().text, "© Me");
    assert!(ExportOptions::from_json(&serde_json::json!({"watermark": ""})).watermark.is_none());
}

/// Horizontal and vertical margins move a text watermark separately; settings saved with one
/// `inset` (before the two margins) read as both, and either new key overrides it.
#[test]
fn watermark_insets_are_horizontal_and_vertical() {
    // gaps between the drawn text and the right and bottom edges of a 400 × 300 image
    let gaps = |wm: &Watermark| {
        let mut img = Rgba8::new(400, 300);
        draw_watermark(&mut img, wm);
        let lit: Vec<(usize, usize)> = (0..300).flat_map(|y| (0..400).map(move |x| (x, y))).filter(|&(x, y)| img.get(x, y)[0] > 128).collect();
        let right = lit.iter().map(|p| p.0).max().unwrap();
        let bottom = lit.iter().map(|p| p.1).max().unwrap();
        (399 - right as i64, 299 - bottom as i64)
    };
    let base = Watermark { text: "LC".into(), size: 0.1, opacity: 1.0, shadow: false, inset_x: 0.0, inset_y: 0.0, ..Default::default() };
    let (r0, b0) = gaps(&base);
    // 0.2 and 0.1 of the 300 px short edge
    let (r1, b1) = gaps(&Watermark { inset_x: 0.2, ..base.clone() });
    assert!((r1 - r0 - 60).abs() <= 1 && (b1 - b0).abs() <= 1, "horizontal: {r0},{b0} → {r1},{b1}");
    let (r2, b2) = gaps(&Watermark { inset_y: 0.1, ..base.clone() });
    assert!((r2 - r0).abs() <= 1 && (b2 - b0 - 30).abs() <= 1, "vertical: {r0},{b0} → {r2},{b2}");
    // the old single margin
    let wm = |w: serde_json::Value| ExportOptions::from_json(&json!({"watermark": w})).watermark.unwrap();
    let old = wm(json!({"text": "LC", "inset": 0.1}));
    assert_eq!((old.inset_x, old.inset_y), (0.1, 0.1));
    let mixed = wm(json!({"text": "LC", "inset": 0.1, "insetY": 0.3}));
    assert_eq!((mixed.inset_x, mixed.inset_y), (0.1, 0.3));
    assert!(ExportOptions::from_params(&json!({"watermark": {"text": "LC", "inset": 0.1}})).is_ok());
    // saved and read back as the two margins
    let o = ExportOptions { watermark: Some(Watermark { inset_x: 0.05, inset_y: 0.15, ..base }), ..Default::default() };
    let saved = o.to_json();
    assert!(saved["watermark"].get("inset").is_none(), "{saved}");
    assert_eq!(ExportOptions::from_json(&saved).watermark.map(|w| (w.inset_x, w.inset_y)), Some((0.05, 0.15)));
}

/// A turned text watermark is placed by its turned box: taller than wide at 90° and 270°, the
/// same corner and insets, and an unknown angle draws it unturned.
#[test]
fn watermark_rotation_turns_the_text_in_its_corner() {
    let lit = |wm: &Watermark| {
        let mut img = Rgba8::new(400, 300);
        draw_watermark(&mut img, wm);
        let on: Vec<(usize, usize)> = (0..300).flat_map(|y| (0..400).map(move |x| (x, y))).filter(|&(x, y)| img.get(x, y)[0] > 128).collect();
        let (x0, x1) = (on.iter().map(|p| p.0).min().unwrap(), on.iter().map(|p| p.0).max().unwrap());
        let (y0, y1) = (on.iter().map(|p| p.1).min().unwrap(), on.iter().map(|p| p.1).max().unwrap());
        (x0, x1, y0, y1, img)
    };
    let base = Watermark { text: "LightCraft".into(), size: 0.08, opacity: 1.0, shadow: false, inset_x: 0.0, inset_y: 0.0, ..Default::default() };
    let (a0, a1, b0, b1, flat) = lit(&base);
    assert!(a1 - a0 > 2 * (b1 - b0), "unturned text is wide");
    for rotation in [90, 270] {
        let (x0, x1, y0, y1, _) = lit(&Watermark { rotation, ..base.clone() });
        assert!(y1 - y0 > 2 * (x1 - x0), "{rotation}°: tall");
        assert!(x1 >= 390 && y1 >= 290, "{rotation}°: still in the bottom-right corner ({x1}, {y1})");
        // 0.1 × 300 px further from the right edge
        let (_, moved, ..) = lit(&Watermark { rotation, inset_x: 0.1, ..base.clone() });
        assert!((x1 as i64 - moved as i64 - 30).abs() <= 1, "{rotation}°: {x1} → {moved}");
    }
    let (x0, x1, y0, y1, _) = lit(&Watermark { rotation: 180, ..base.clone() });
    assert!(((x1 - x0) as i64 - (a1 - a0) as i64).abs() <= 2 && ((y1 - y0) as i64 - (b1 - b0) as i64).abs() <= 2, "180°: same size");
    assert!(x1 >= 390 && y1 >= 290, "180°: same corner");
    let (.., odd) = lit(&Watermark { rotation: 45, ..base.clone() });
    assert_eq!(odd.data, flat.data, "an angle that isn't a quarter turn draws unturned");
    // saved and read back; older settings have none
    let o = ExportOptions { watermark: Some(Watermark { rotation: 270, ..base }), ..Default::default() };
    assert_eq!(ExportOptions::from_json(&o.to_json()).watermark.map(|w| w.rotation), Some(270));
    assert_eq!(ExportOptions::from_json(&json!({"watermark": {"text": "x"}})).watermark.map(|w| w.rotation), Some(0));
}

/// A preview keeps the export's 6 px minimum text size as a share of the photo: a watermark
/// clamped to 6 px in a 150 px export covers the same share of a 600 px preview.
#[test]
fn watermark_preview_scales_the_minimum_text_size() {
    let wm = Watermark { text: "LightCraft".into(), size: 0.01, opacity: 1.0, shadow: false, ..Default::default() };
    let rows = |img: &Rgba8| (0..img.height).filter(|&y| (0..img.width).any(|x| img.get(x, y)[0] > 64)).count() as f32;
    let mut export = Rgba8::new(200, 150);
    draw_watermark(&mut export, &wm);
    let mut preview = Rgba8::new(800, 600);
    draw_watermark_preview(&mut preview, &wm, 150);
    let mut unscaled = Rgba8::new(800, 600);
    draw_watermark(&mut unscaled, &wm);
    let (export_share, preview_share) = (rows(&export) / 150.0, rows(&preview) / 600.0);
    assert!((preview_share - export_share).abs() < 0.02, "export {export_share}, preview {preview_share}");
    assert!(rows(&unscaled) < rows(&preview) / 2.0, "the unscaled 6 px floor would draw it too small");
    // drawn at the export's own size, the preview is the export
    let mut same = Rgba8::new(200, 150);
    draw_watermark_preview(&mut same, &wm, 150);
    assert_eq!(same.data, export.data);
}

fn vertical_japanese() -> Watermark {
    let options = ExportOptions::from_json(&json!({"watermark": {"text": "日本語", "vertical": true, "size": 0.1, "shadow": false, "opacity": 1.0}}));
    options.watermark.unwrap()
}

#[test]
fn japanese_watermark_options_and_legacy_defaults() {
    let old = ExportOptions::from_json(&json!({"watermark": {"text": "日本語"}}));
    assert!(!old.watermark.unwrap().vertical);
    let wm = vertical_japanese();
    assert!(wm.vertical);
    let round: Watermark = serde_json::from_value(serde_json::to_value(&wm).unwrap()).unwrap();
    assert!(round.vertical);
}

/// Built without craft-fonts, a Japanese watermark still lays out and draws (Inter's
/// missing-glyph boxes), and Latin text is unaffected.
#[test]
fn watermarks_work_without_craft_fonts() {
    assert_eq!(watermark_fonts(&[]).len(), 1, "Inter only");
    for wm in [vertical_japanese(), Watermark { text: "LightCraft 日本語".into(), ..Watermark::default() }] {
        let mut covered = 0usize;
        watermark_coverage(400, 300, &wm, &[], 1.0, |_, _, k, _| covered += usize::from(k > 0.0));
        assert!(covered > 0, "{:?} draws something", wm.text);
    }
}

#[test]
fn biz_ud_vertical_watermarks_place_punctuation_at_the_top_right() {
    use ab_glyph::Font;
    let Some(entry) = crate::fonts::CRAFT_FONTS.iter().find(|f| f.family == "BIZ UDMincho") else {
        lightcraft_testkit::skip!(Fonts, "built without BIZ UDMincho from craft-fonts");
        return;
    };
    let font = ab_glyph::FontRef::try_from_slice(entry.bytes).unwrap();
    // BIZ UDMincho has no Unicode presentation-form characters: the old fallback drew 、。.
    assert_eq!(font.glyph_id('︑').0, 0);
    assert_eq!(font.glyph_id('︒').0, 0);
    let data = harfrust::ShaperData::new(&harfrust::FontRef::new(entry.bytes).unwrap());
    for ch in ['、', '。'] {
        let glyph = watermark_cell_glyphs(&font, &data, &ch.to_string(), true, 100.0, 0.0, 0.0).unwrap().remove(0);
        assert_ne!(glyph.id, font.glyph_id(ch), "the font's vertical alternate for {ch}");
        let rect = font.outline_glyph(glyph).unwrap().px_bounds();
        assert!(rect.min.x > 50.0 && rect.max.y < 50.0, "{ch} in the upper right: {rect:?}");
    }
    let craft = Box::leak(
        vec![crate::fonts::CraftFont { family: entry.family, style: entry.style, scripts: entry.scripts, bytes: entry.bytes }].into_boxed_slice(),
    );
    let wm = Watermark {
        text: "、。".into(),
        vertical: true,
        size: 0.1,
        anchor: Anchor::TopLeft,
        inset_x: 0.0,
        inset_y: 0.0,
        shadow: false,
        opacity: 1.0,
        ..Default::default()
    };
    let mut pixels = Vec::new();
    watermark_coverage(400, 300, &wm, craft, 1.0, |x, y, k, _| {
        if k > 0.5 {
            pixels.push((x, y));
        }
    });
    assert!(!pixels.is_empty());
    for (x, y) in pixels {
        assert!(x > 15 && y % 30 < 15, "actual export coverage at the upper right of the 30 px cell: {x}, {y}");
    }
}

#[test]
fn vertical_watermarks_keep_combining_marks_in_one_cell() {
    let coverage = |text: &str, craft| {
        let wm = Watermark {
            text: text.into(),
            vertical: true,
            size: 0.1,
            anchor: Anchor::Center,
            inset_x: 0.0,
            inset_y: 0.0,
            shadow: false,
            opacity: 1.0,
            ..Default::default()
        };
        let mut pixels = vec![0.0f32; 400 * 300];
        watermark_coverage(400, 300, &wm, craft, 1.0, |x, y, k, _| pixels[y * 400 + x] = k);
        assert!(pixels.iter().any(|&k| k > 0.0));
        pixels
    };
    let compare = |a: &str, b: &str, craft| {
        let (a_pixels, b_pixels) = (coverage(a, craft), coverage(b, craft));
        let difference = a_pixels.iter().zip(&b_pixels).position(|(a, b)| a != b);
        assert!(difference.is_none(), "{a:?} vs {b:?}: first differing pixel {difference:?}");
    };
    for (composed, decomposed) in [("éA", "e\u{301}A"), ("Å\nA", "A\u{30a}\nA"), ("A\nB", "A\r\nB"), ("日A", "日\u{e0100}A")] {
        compare(composed, decomposed, &[]);
    }
    if crate::fonts::CRAFT_FONTS.iter().any(|f| f.family == "BIZ UDMincho") {
        for (composed, decomposed) in [("が日", "か\u{3099}日"), ("ぱ\n日", "は\u{309a}\n日")] {
            compare(composed, decomposed, crate::fonts::CRAFT_FONTS);
        }
    } else {
        lightcraft_testkit::skip!(Fonts, "Japanese comparisons: built without BIZ UDMincho from craft-fonts");
    }
}

#[test]
fn upright_watermark_cell_retains_multiple_glyphs() {
    use ab_glyph::{Font, ScaleFont};
    let font = ab_glyph::FontRef::try_from_slice(WATERMARK_FONT).unwrap();
    let data = harfrust::ShaperData::new(&harfrust::FontRef::new(WATERMARK_FONT).unwrap());
    // There is no precomposed A with a combining long solidus overlay.
    let glyphs = watermark_cell_glyphs(&font, &data, "A\u{338}", false, 30.0, 0.0, 0.0).unwrap();
    assert_eq!(glyphs.len(), 2, "retain the base and the separately positioned combining glyph");
    let baseline = font.as_scaled(30.0).ascent();
    for glyph in glyphs {
        assert!((glyph.position.y - baseline).abs() < 1.0, "no extra vertical cell for the mark");
        assert!(font.outline_glyph(glyph).is_some(), "both glyphs have ink");
    }
}

#[test]
fn japanese_watermarks_support_vertical_columns() {
    use ab_glyph::Font;
    let fonts = watermark_fonts(crate::fonts::CRAFT_FONTS);
    if fonts.len() < 2 {
        lightcraft_testkit::skip!(Fonts, "built without CRAFT_FONTS_DIR, so there is no Japanese watermark face");
        return;
    }
    for c in "日本語の文字".chars() {
        assert!(fonts[1..].iter().any(|f| f.glyph_id(c).0 != 0), "a craft-fonts face has {c}");
    }
    let wm = vertical_japanese();
    let mut img = Rgba8::new(400, 300);
    img.data.fill([0, 0, 0, 255]);
    draw_watermark(&mut img, &wm);
    let lit: Vec<_> = (0..300).flat_map(|y| (0..400).map(move |x| (x, y))).filter(|&(x, y)| img.get(x, y)[0] > 64).collect();
    assert!(lit.len() > 100);
    let w = lit.iter().map(|p| p.0).max().unwrap() - lit.iter().map(|p| p.0).min().unwrap();
    let h = lit.iter().map(|p| p.1).max().unwrap() - lit.iter().map(|p| p.1).min().unwrap();
    assert!(h > w * 2, "vertical Japanese must extend down the column");
    let two = Watermark { text: "日\n本".into(), anchor: Anchor::TopLeft, inset_x: 0.0, inset_y: 0.0, ..wm };
    img.data.fill([0, 0, 0, 255]);
    draw_watermark(&mut img, &two);
    for (text, dx) in [("本", 0), ("日", 30)] {
        let mut single = Rgba8::new(400, 300);
        single.data.fill([0, 0, 0, 255]);
        draw_watermark(&mut single, &Watermark { text: text.into(), ..two.clone() });
        for y in 0..30 {
            for x in 0..30 {
                assert_eq!(img.get(x + dx, y), single.get(x, y), "newlines move columns left");
            }
        }
    }
}

#[test]
fn graphic_watermark_is_placed_scaled_and_blended() {
    let dir = std::env::temp_dir().join(format!("lc-wm-logo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let logo_path = dir.join("logo.png");
    // 40×20: left half opaque red, right half fully transparent
    let logo = Rgba8::from_fn(40, 20, |x, _| if x < 20 { [255, 0, 0, 255] } else { [0, 255, 0, 0] });
    let png = lightcraft_codecs::encode_png(&EncodeImage::rgba8(&logo), &EncodeMeta::default()).unwrap();
    std::fs::write(&logo_path, png).unwrap();
    let mut img = Rgba8::new(400, 300);
    img.data.iter_mut().for_each(|p| *p = [0, 0, 0, 255]);
    let wm =
        Watermark { image: logo_path.to_string_lossy().into(), image_width: 0.25, opacity: 1.0, inset_x: 0.0, inset_y: 0.0, ..Default::default() };
    draw_watermark(&mut img, &wm);
    // 100 px wide × 50 px high in the bottom-right corner
    assert!(img.get(310, 280)[0] > 200, "opaque half: {:?}", img.get(310, 280));
    assert_eq!(img.get(390, 280), [0, 0, 0, 255], "transparent half leaves the photo");
    assert_eq!(img.get(310, 240), [0, 0, 0, 255], "above the logo");
    assert_eq!(img.get(10, 10), [0, 0, 0, 255]);
    // 30 px from the right edge and 15 px from the bottom: x 270..370, y 235..285
    let mut img = Rgba8::new(400, 300);
    img.data.iter_mut().for_each(|p| *p = [0, 0, 0, 255]);
    draw_watermark(&mut img, &Watermark { inset_x: 0.1, inset_y: 0.05, ..wm.clone() });
    assert!(img.get(272, 283)[0] > 200, "inside the moved logo: {:?}", img.get(272, 283));
    assert_eq!(img.get(267, 283), [0, 0, 0, 255], "left of the logo");
    assert_eq!(img.get(272, 288), [0, 0, 0, 255], "below the logo");
    // turned: 180° puts the opaque half on the right; 90° clockwise puts it on top of a 50 × 100 box
    let turned = |rotation: u16| {
        let mut img = Rgba8::new(400, 300);
        img.data.iter_mut().for_each(|p| *p = [0, 0, 0, 255]);
        draw_watermark(&mut img, &Watermark { rotation, ..wm.clone() });
        img
    };
    let half = turned(180);
    assert!(half.get(390, 280)[0] > 200 && half.get(310, 280) == [0, 0, 0, 255]);
    let quarter = turned(90);
    assert!(quarter.get(375, 220)[0] > 200, "top half of the turned logo: {:?}", quarter.get(375, 220));
    assert_eq!(quarter.get(375, 280), [0, 0, 0, 255], "its transparent half below");
    assert_eq!(quarter.get(340, 220), [0, 0, 0, 255], "left of the 50 px wide box");
    assert!(quarter.get(352, 202)[0] > 200 && quarter.get(398, 248)[0] > 200, "the whole box is covered");
    // three quarter turns: the opaque half at the bottom
    let three = turned(270);
    assert!(three.get(375, 280)[0] > 200 && three.get(375, 220) == [0, 0, 0, 255]);
    // an image-only watermark survives the params (no text needed)
    let o = ExportOptions::from_json(&json!({"watermark": {"image": logo_path.to_string_lossy(), "imageWidth": 0.1}}));
    assert_eq!(o.watermark.as_ref().map(|w| w.image_width), Some(0.1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn naming_and_params() {
    let o = ExportOptions::from_json(&serde_json::json!({"format": "jpg", "quality": 150, "longEdge": 2048, "naming": "{name}-{seq}"}));
    assert_eq!(o.format, ExportFormat::Jpeg);
    assert_eq!(o.quality, 100);
    assert_eq!(o.resize, Some(Resize { dont_enlarge: true, height: 0, ..Resize::long_edge(2048) }));
    assert_eq!(o.file_name_for(&named("IMG/1.png"), 7), "IMG_1-007.jpg");
    assert_eq!(o.ppi, 240);
    let orig = ExportOptions { format: ExportFormat::Original, naming: "{name}-{seq}".into(), ..Default::default() };
    assert_eq!(orig.file_name_for(&named("DSC_1.NEF"), 2), "DSC_1-002.NEF");
    assert_eq!(orig.file_name_for(&named("noext"), 1), "noext-001");
    assert_eq!(ExportOptions { format: ExportFormat::Dng, ..Default::default() }.file_name_for(&named("a.cr2"), 1), "a.dng");
}

fn named(file_name: &str) -> lightcraft_catalog::Photo {
    use lightcraft_catalog::{Photo, PhotoId, Source};
    Photo::new(PhotoId(1), Source::Demo { scene: 0 }, file_name, "", 1, 1, "2026-01-01T00:00:00")
}

/// Export naming takes the same tokens as Rename Photos and import renaming.
#[test]
fn naming_uses_the_rename_tokens() {
    let mut p = named("DSC_0815.NEF");
    p.captured = Some("2026-09-30T14:05:09".into());
    p.meta.camera = "Model X/2".into();
    p.meta.title = "Harbour".into();
    p.rating = 5;
    let o = |naming: &str| ExportOptions { naming: naming.into(), start_number: 9, ..Default::default() };
    assert_eq!(o("{date:%Y-%m-%d}_{title}_{seq:2}").file_name_for(&p, 1), "2026-09-30_Harbour_09.jpg");
    assert_eq!(o("{camera}-{num}-{rating}").file_name_for(&p, 1), "Model X_2-0815-5.jpg", "path separators become _");
    assert_eq!(o("{name}-{seq}").file_name_for(&p, 3), "DSC_0815-011.jpg", "a bare {{seq}} keeps its 3 digits");
    assert_eq!(o("{ext}_{name}").file_name_for(&p, 1), "NEF_DSC_0815.jpg");
    // nothing left after expanding: the original name
    p.meta.title.clear();
    assert_eq!(o("{title}").file_name_for(&p, 1), "DSC_0815.jpg");
}

#[test]
fn params_round_trip() {
    let o = ExportOptions {
        format: ExportFormat::Tiff,
        quality: 70,
        resize: Some(Resize { mode: ResizeMode::Megapixels, value: 12.5, height: 0, dont_enlarge: false }),
        ppi: 300,
        limit_kb: Some(500),
        sharpen: SharpenFor::Glossy,
        naming: "{date}-{name}-{seq}".into(),
        start_number: 42,
        subfolder: "Client".into(),
        conflict: Conflict::Skip,
        tiff_compression: TiffCompression::Lzw,
        metadata: MetadataPolicy::Copyright,
        remove_location: true,
        watermark: Some(Watermark { text: "© LC".into(), ..Default::default() }),
        color_space: OutputSpace::ProPhoto,
        bit_depth: Some(16),
        ..Default::default()
    };
    assert_eq!(ExportOptions::from_json(&o.to_json()), o);
    let full = ExportOptions::default();
    assert_eq!(ExportOptions::from_json(&full.to_json()), full);
    assert!(ExportOptions::has_size_param(&full.to_json()), "full size is explicit");
    let mut p = named("IMG.png");
    p.captured = Some("2026-09-30T10:00:00".into());
    assert_eq!(o.file_name_for(&p, 2), "20260930-IMG-043.tif");
}

#[test]
fn resize_modes() {
    let size = |p: serde_json::Value, w: f64, h: f64| ExportOptions::from_json(&p).resize.map(|r| r.apply(w, h));
    // a 6000 × 4000 landscape and a 4000 × 6000 portrait
    assert_eq!(size(json!({"longEdge": 1500}), 6000.0, 4000.0), Some((1500, 1000)));
    assert_eq!(size(json!({"longEdge": 1500}), 4000.0, 6000.0), Some((1000, 1500)));
    assert_eq!(size(json!({"shortEdge": 1080}), 6000.0, 4000.0), Some((1620, 1080)));
    assert_eq!(size(json!({"width": 3000}), 4000.0, 6000.0), Some((3000, 4500)));
    assert_eq!(size(json!({"height": 1000}), 6000.0, 4000.0), Some((1500, 1000)));
    // W × H fits either orientation: the long edge gets the larger number
    assert_eq!(size(json!({"width": 1000, "height": 800}), 6000.0, 4000.0), Some((1000, 667)));
    assert_eq!(size(json!({"width": 1000, "height": 800}), 4000.0, 6000.0), Some((667, 1000)));
    assert_eq!(size(json!({"width": 1000, "height": 500}), 6000.0, 4000.0), Some((750, 500)));
    assert_eq!(size(json!({"width": 270, "height": 337, "fit": "dimensions"}), 480.0, 320.0), Some((337, 225)));
    // Width & Height (`fit: widthHeight`) binds each edge to its own number: W limits the width, H the height (#1620)
    assert_eq!(size(json!({"width": 270, "height": 337, "fit": "widthHeight"}), 480.0, 320.0), Some((270, 180)));
    assert_eq!(size(json!({"width": 270, "height": 337, "fit": "widthHeight"}), 320.0, 480.0), Some((225, 337)));
    assert_eq!(size(json!({"width": 240, "height": 300, "fit": "widthHeight"}), 480.0, 320.0), Some((240, 160)));
    // `fit` only matters with both numbers
    assert_eq!(size(json!({"width": 1000, "fit": "widthHeight"}), 6000.0, 4000.0), Some((1000, 667)));
    let (w, h) = size(json!({"megapixels": 6}), 6000.0, 4000.0).unwrap();
    assert!(((w * h) as f64 - 6e6).abs() < 6e6 * 0.002, "{w}×{h}");
    assert_eq!(size(json!({"percent": 25}), 6000.0, 4000.0), Some((1500, 1000)));
    // don't enlarge (default) vs enlarge
    assert_eq!(size(json!({"longEdge": 8000}), 6000.0, 4000.0), Some((6000, 4000)));
    assert_eq!(size(json!({"longEdge": 9000, "dontEnlarge": false}), 6000.0, 4000.0), Some((9000, 6000)));
    // full size / absent
    assert_eq!(size(json!({"longEdge": 0}), 6000.0, 4000.0), None);
    assert_eq!(size(json!({}), 6000.0, 4000.0), None);
    assert!(ExportOptions::has_size_param(&json!({"longEdge": 0})) && !ExportOptions::has_size_param(&json!({"quality": 3})));
    // the `resize` object form round-trips (what the dialog and prefs store)
    let r = Resize { mode: ResizeMode::ShortEdge, value: 720.0, height: 0, dont_enlarge: false };
    let back = ExportOptions::from_json(&ExportOptions::resize_json(Some(&r))).resize;
    assert_eq!(back, Some(r));
    let r = Resize { mode: ResizeMode::WidthHeight, value: 270.0, height: 337, dont_enlarge: true };
    assert_eq!(ExportOptions::from_json(&ExportOptions::resize_json(Some(&r))).resize, Some(r));
    assert_eq!(ExportOptions::from_json(&ExportOptions::resize_json(None)).resize, None);
    assert_eq!(ExportOptions::from_json(&json!({"ppi": 300})).ppi, 300);
    // never larger than the encoders' limit, never empty
    assert_eq!(size(json!({"percent": 10000, "dontEnlarge": false}), 6000.0, 4000.0), Some((27713, 18475)), "512 MP at most");
    assert_eq!(size(json!({"width": 70000, "dontEnlarge": false}), 60000.0, 100.0), Some((65535, 109)), "65535 px an edge");
    assert_eq!(size(json!({"width": 1}), 6000.0, 40.0).map(|s| s.1), Some(1));
}

/// Typing 20000 for 2000 (or percent 10000) with Don't Enlarge off asked for gigapixels: the
/// render's first plane alone (34 GB at 65535 × 43690) failed to allocate, which aborts the
/// process. An enlargement stops at 512 megapixels; full-size and smaller exports of photos
/// larger than that are untouched.
#[test]
fn enlargements_stop_at_512_megapixels() {
    let size = |p: serde_json::Value, w: f64, h: f64| ExportOptions::from_json(&p).resize.map(|r| r.apply(w, h)).unwrap();
    for p in [json!({"longEdge": 60000, "dontEnlarge": false}), json!({"megapixels": 5000}), json!({"percent": 10000, "dontEnlarge": false})] {
        let (w, h) = size(p.clone(), 6000.0, 4000.0);
        assert!((w * h) as f64 <= MAX_ENLARGED_PIXELS * 1.001, "{p}: {w}×{h}");
        assert!((w as f64 / h as f64 - 1.5).abs() < 0.001, "{p}: keeps the shape");
    }
    // below the ceiling an enlargement is as asked
    assert_eq!(size(json!({"longEdge": 12000, "dontEnlarge": false}), 6000.0, 4000.0), (12000, 8000));
    // a 600 MP panorama exports at full size, and smaller, as before; it is never enlarged
    assert_eq!(size(json!({"percent": 100, "dontEnlarge": false}), 30000.0, 20000.0), (30000, 20000));
    assert_eq!(size(json!({"percent": 50}), 30000.0, 20000.0), (15000, 10000));
    assert_eq!(size(json!({"percent": 200, "dontEnlarge": false}), 30000.0, 20000.0), (30000, 20000));
}

/// An enlarged export that needs more memory than the computer has is refused with a message,
/// before it starts; everything else goes ahead.
#[test]
fn enlarged_exports_must_fit_the_computers_memory() {
    let gb = 1usize << 30;
    let why = too_big_for_memory((27713, 18475), 24_000_000, 20 * gb, Some(16 * gb)).unwrap();
    assert!(why.contains("27713 × 18475") && why.contains("20 GB") && why.contains("16 GB") && why.contains("smaller size"), "{why}");
    assert_eq!(too_big_for_memory((27713, 18475), 24_000_000, 20 * gb, Some(32 * gb)), None, "fits");
    assert_eq!(too_big_for_memory((6000, 4000), 24_000_000, 20 * gb, Some(gb)), None, "full size is never refused");
    assert_eq!(too_big_for_memory((3000, 2000), 24_000_000, 20 * gb, Some(gb)), None, "nor a smaller export");
    assert_eq!(too_big_for_memory((27713, 18475), 24_000_000, 20 * gb, None), None, "unknown memory");
}

#[test]
fn resize_size_survives_the_render() {
    // the render fits the photo into the size `apply` gives; that must not cost a pixel (#1312)
    let cropped = |r: Resize, w: u32, h: u32, x1: f64| {
        let mut p = named("IMG.ARW");
        (p.width, p.height) = (w, h);
        std::sync::Arc::make_mut(&mut p.develop).crop.geometry.rect = lightcraft_geom::Rect::new(0.0, 0.0, x1, 1.0);
        let (bw, bh) = output_size(&p, &ExportOptions { resize: Some(r), ..Default::default() });
        lightcraft_pipeline::output_size(w as usize, h as usize, &p.develop, &lightcraft_pipeline::RenderRequest::fit(bw, bh))
    };
    let rendered = |r: Resize, w: u32, h: u32| cropped(r, w, h, 1.0);
    let mode = |mode: ResizeMode, value: f32, height: u32| Resize { mode, value, height, dont_enlarge: true };
    // a frame a hair narrower than 3:2 (a raw's default crop in floats) puts 1333 × 1.5 just under 1999.5
    let hair = 1.0 - 1e-9;
    assert_eq!(cropped(Resize::long_edge(2000), 6000, 4000, hair), (2000, 1333));
    assert_eq!(cropped(Resize::long_edge(2048), 6000, 4000, hair), (2048, 1365));
    assert_eq!(cropped(mode(ResizeMode::Width, 2000.0, 0), 6000, 4000, hair), (2000, 1333));
    assert_eq!(cropped(mode(ResizeMode::ShortEdge, 1333.0, 0), 6000, 4000, hair), (2000, 1333));
    assert_eq!(cropped(mode(ResizeMode::Height, 1333.0, 0), 6000, 4000, hair), (2000, 1333));
    assert_eq!(rendered(Resize::long_edge(2000), 4000, 6000), (1333, 2000));
    // 3:2, 4:3, 16:9, 1:1 and their portraits, every mode: the render keeps what `apply` says
    for (w, h) in [(6000u32, 4000u32), (6240, 4160), (4032, 3024), (5184, 3888), (3840, 2160), (6000, 3375), (4000, 4000)] {
        for (w, h) in [(w, h), (h, w)] {
            for value in [1000.0, 1080.0, 1333.0, 2000.0, 2048.0, 2500.0, 3000.0] {
                for r in [
                    Resize::long_edge(value as u32),
                    mode(ResizeMode::ShortEdge, value, 0),
                    mode(ResizeMode::Width, value, 0),
                    mode(ResizeMode::Height, value, 0),
                    mode(ResizeMode::Dimensions, value, 1365),
                    mode(ResizeMode::WidthHeight, value, 1365),
                    mode(ResizeMode::Megapixels, value / 1000.0, 0),
                    mode(ResizeMode::Percent, value / 100.0, 0),
                ] {
                    let want = r.apply(w.into(), h.into());
                    assert_eq!(rendered(r, w, h), want, "{r:?} on {w}×{h}");
                }
            }
        }
    }
}

/// A photo more than 65535 times as long as it is short (a 70000 × 1 strip) made `scale`
/// clamp with its lower bound above its upper one, which panics, in the export and in the
/// Export dialog's preview on the UI thread. The encoders' limit wins, the short edge keeps a
/// pixel.
#[test]
fn resize_survives_photos_too_long_for_the_encoders() {
    let modes = [
        json!({"longEdge": 2000}),
        json!({"shortEdge": 1}),
        json!({"width": 500, "height": 500}),
        json!({"megapixels": 1}),
        json!({"percent": 50}),
        json!({"percent": 1000, "dontEnlarge": false}),
    ];
    for p in modes {
        for (w, h) in [(70_000.0, 1.0), (1.0, 70_000.0), (1e9, 3.0), (65_536.0, 1.0)] {
            let r = ExportOptions::from_json(&p).resize.unwrap();
            let (ow, oh) = r.apply(w, h);
            assert!((1..=65_535).contains(&ow) && (1..=65_535).contains(&oh), "{p} on {w}×{h}: {ow}×{oh}");
            assert!(r.scale(w, h).is_finite());
        }
    }
    // (a short edge under a pixel would be scaled up to one, which the limit then caps)
    assert_eq!(Resize::long_edge(2000).apply(70_000.0, 1.0), (65_535, 1));
    assert_eq!(Resize::long_edge(2000).apply(1.0, 70_000.0), (1, 65_535));
}

/// A flat field of a saturated green inside Display P3 but outside sRGB, rendered into `space`.
fn p3_green(space: OutputSpace) -> Rgba8 {
    use lightcraft_color::{DISPLAY_P3, REC2020};
    let c = DISPLAY_P3.to_space(&REC2020).apply_f32([0.04, 0.45, 0.04]);
    let src = lightcraft_raster::Rgb32f::filled(16, 16, c);
    let req = lightcraft_pipeline::RenderRequest { space, ..lightcraft_pipeline::RenderRequest::fit(16, 16) };
    lightcraft_pipeline::render(&src, &Default::default(), &Default::default(), &req).image
}

/// Decode `bytes` and return the centre pixel in linear sRGB primaries (unclamped), plus the
/// recognised space of the embedded profile.
fn decoded_in_srgb(bytes: &[u8]) -> ([f32; 3], Option<NamedSpace>) {
    let d = lightcraft_codecs::decode(bytes, Default::default()).expect("decodes");
    let m = d.space.to_space(&lightcraft_color::SRGB);
    (m.apply_f32(d.image.get(8, 8)), d.space.named)
}

#[test]
fn p3_colour_survives_a_p3_export_and_is_clipped_in_srgb() {
    let o = |space| ExportOptions { format: ExportFormat::Png, color_space: space, ..Default::default() };
    let (p3, named) = decoded_in_srgb(&encode_image(&p3_green(OutputSpace::DisplayP3), &o(OutputSpace::DisplayP3)).unwrap());
    assert_eq!(named, Some(NamedSpace::DisplayP3));
    assert!(p3[0] < -0.03 || p3[2] < -0.03, "outside sRGB after a P3 round trip: {p3:?}");
    let (s, named) = decoded_in_srgb(&encode_image(&p3_green(OutputSpace::Srgb), &o(OutputSpace::Srgb)).unwrap());
    assert_eq!(named, Some(NamedSpace::Srgb));
    assert!(s.iter().all(|v| *v > -0.002), "{s:?}");
    // the green itself is about the same brightness either way
    assert!((p3[1] - s[1]).abs() < 0.15, "{p3:?} {s:?}");
}

#[test]
fn every_space_embeds_its_own_profile_and_round_trips() {
    let grey = {
        let src = lightcraft_raster::Rgb32f::filled(16, 16, [0.18; 3]);
        let base = lightcraft_pipeline::RenderRequest::fit(16, 16);
        move |space| {
            lightcraft_pipeline::render(&src, &Default::default(), &Default::default(), &lightcraft_pipeline::RenderRequest { space, ..base }).image
        }
    };
    let reference =
        decoded_in_srgb(&encode_image(&grey(OutputSpace::Srgb), &ExportOptions { format: ExportFormat::Tiff, ..Default::default() }).unwrap()).0;
    for space in OutputSpace::ALL {
        for format in [ExportFormat::Jpeg, ExportFormat::Png, ExportFormat::Tiff, ExportFormat::Webp] {
            let o = ExportOptions { format, color_space: space, ..Default::default() };
            let bytes = encode_image(&grey(space), &o).unwrap();
            let (px, named) = decoded_in_srgb(&bytes);
            assert_eq!(named, Some(named_space(space)), "{space:?} {format:?}");
            // a neutral grey decodes to the same linear value whatever the output space
            for c in 0..3 {
                assert!((px[c] - reference[c]).abs() < 0.01, "{space:?} {format:?}: {px:?} vs {reference:?}");
            }
        }
        let icc = icc::write_named(named_space(space));
        let info = icc::parse(&icc).expect("our profile parses");
        assert_eq!(info.named, Some(named_space(space)));
        assert!(info.description.as_deref().is_some_and(|d| !d.is_empty()));
    }
    let o = ExportOptions { format: ExportFormat::Avif, color_space: OutputSpace::ProPhoto, ..Default::default() };
    assert_eq!(o.effective_space(), OutputSpace::Srgb);
    let o = ExportOptions::from_json(&serde_json::json!({"colorSpace": "displayP3"}));
    assert_eq!(o.color_space, OutputSpace::DisplayP3);
    assert_eq!(ExportOptions::from_json(&serde_json::json!({"colorSpace": "Adobe RGB (1998) compatible"})).color_space, OutputSpace::AdobeRgb);
}

#[test]
fn srgb_watermark_colour_is_converted() {
    assert_eq!(srgb8_in(OutputSpace::Srgb, [10, 200, 30]), [10, 200, 30]);
    for s in OutputSpace::ALL {
        assert_eq!(srgb8_in(s, [255, 255, 255]), [255, 255, 255], "{s:?}");
        assert_eq!(srgb8_in(s, [0, 0, 0]), [0, 0, 0], "{s:?}");
    }
    let p3 = srgb8_in(OutputSpace::DisplayP3, [255, 0, 0]);
    assert!(p3[0] < 255 && p3[1] > 0, "{p3:?}");
}

/// A shallow horizontal grey ramp (few 8-bit levels), rendered at `depth`.
fn ramp(depth: OutputDepth, space: OutputSpace) -> lightcraft_pipeline::Rendered {
    let src = lightcraft_raster::Rgb32f::from_fn(1024, 4, |x, _| [0.10 + 0.03 * x as f32 / 1023.0; 3]);
    let req = lightcraft_pipeline::RenderRequest { depth, space, ..lightcraft_pipeline::RenderRequest::fit(1024, 4) };
    lightcraft_pipeline::render(&src, &Default::default(), &Default::default(), &req)
}

fn distinct_levels(bytes: &[u8]) -> (usize, lightcraft_codecs::Decoded) {
    let d = lightcraft_codecs::decode(bytes, Default::default()).expect("decodes");
    let mut v: Vec<u32> = (0..d.image.width).map(|x| d.image.get(x, 1)[1].to_bits()).collect();
    v.dedup();
    (v.len(), d)
}

#[test]
fn sixteen_bit_tiff_and_png_have_no_banding() {
    let tiff = |bit_depth| ExportOptions { format: ExportFormat::Tiff, bit_depth, ..Default::default() };
    assert_eq!(tiff(None).effective_depth(), OutputDepth::U16, "TIFF defaults to 16-bit");
    let r16 = ramp(OutputDepth::U16, OutputSpace::Srgb);
    assert!(matches!(r16.deep.as_ref().unwrap().samples, DeepSamples::U16(_)));
    let (levels16, d16) = distinct_levels(&encode_rendered(&r16, &tiff(None), None).unwrap());
    let r8 = ramp(OutputDepth::U8, OutputSpace::Srgb);
    let (levels8, _) = distinct_levels(&encode_rendered(&r8, &tiff(Some(8)), None).unwrap());
    eprintln!("gradient levels: 16-bit TIFF {levels16}, 8-bit TIFF {levels8}");
    assert!(levels8 < 40, "{levels8}");
    assert!(levels16 > 10 * levels8, "16-bit: {levels16} levels vs 8-bit: {levels8}");
    assert_eq!(d16.space.named, Some(NamedSpace::Srgb));
    // the 8-bit preview of a deep render matches the 8-bit render
    for (a, b) in r16.image.data.iter().zip(&r8.image.data) {
        assert!(a[1].abs_diff(b[1]) <= 1, "{a:?} {b:?}");
    }
    let png = ExportOptions { format: ExportFormat::Png, bit_depth: Some(16), color_space: OutputSpace::DisplayP3, ..Default::default() };
    let rp = ramp(OutputDepth::U16, OutputSpace::DisplayP3);
    let bytes = encode_rendered(&rp, &png, None).unwrap();
    assert_eq!(bytes[24], 16, "PNG IHDR bit depth");
    let (levels, d) = distinct_levels(&bytes);
    assert!(levels > 10 * levels8, "{levels}");
    assert_eq!(d.space.named, Some(NamedSpace::DisplayP3));
    // sharpening and watermarking work on deep images too
    let o =
        ExportOptions { sharpen: SharpenFor::Matte, watermark: Some(Watermark { text: "LC".into(), size: 0.5, ..Default::default() }), ..tiff(None) };
    assert!(encode_rendered(&r16, &o, None).is_ok());
}

#[test]
fn float_tiff_is_linear_with_a_linear_profile() {
    let o = ExportOptions { format: ExportFormat::Tiff, bit_depth: Some(32), color_space: OutputSpace::ProPhoto, ..Default::default() };
    assert_eq!(o.effective_depth(), OutputDepth::F32Linear);
    let rf = ramp(OutputDepth::F32Linear, OutputSpace::ProPhoto);
    let (levels, df) = distinct_levels(&encode_rendered(&rf, &o, None).unwrap());
    assert!(levels > 500, "{levels}");
    assert_eq!(df.space.named, Some(NamedSpace::ProPhoto));
    assert!(df.space.trc.as_ref().is_some_and(|t| t[0].is_linear()), "{:?}", df.space.trc);
    // decodes to the same light as the 16-bit gamma-encoded export
    let o16 = ExportOptions { bit_depth: Some(16), ..o.clone() };
    let (_, d16) = distinct_levels(&encode_rendered(&ramp(OutputDepth::U16, OutputSpace::ProPhoto), &o16, None).unwrap());
    for x in [0, 300, 700, 1023] {
        let (a, b) = (df.image.get(x, 1)[1], d16.image.get(x, 1)[1]);
        assert!((a - b).abs() < 2e-4, "{x}: {a} vs {b}");
    }
}

#[test]
fn jxl_options() {
    assert_eq!(ExportFormat::parse("JPEG-XL"), Some(ExportFormat::Jxl));
    let o = ExportOptions::from_params(&json!({"format": "jxl", "jxlLossless": true, "jxlEffort": "fast", "jxlProgressive": true})).unwrap();
    assert_eq!((o.format, o.jxl_lossless, o.jxl_effort, o.jxl_progressive), (ExportFormat::Jxl, true, JxlEffort::Fast, true));
    assert_eq!(ExportOptions::from_json(&o.to_json()), o, "saved presets keep the JPEG XL settings");
    assert!(ExportOptions::from_params(&json!({"jxlEffort": "best"})).is_err());
    assert!(ExportOptions::from_params(&json!({"jxlLossless": "yes"})).is_err());
    let space = |lossless, color_space, hdr| {
        ExportOptions { format: ExportFormat::Jxl, jxl_lossless: lossless, color_space, hdr, ..Default::default() }.effective_space()
    };
    assert_eq!(space(false, OutputSpace::DisplayP3, false), OutputSpace::DisplayP3);
    assert_eq!(space(false, OutputSpace::ProPhoto, false), OutputSpace::Rec2020, "lossy has no ProPhoto enum");
    assert_eq!(space(true, OutputSpace::ProPhoto, false), OutputSpace::ProPhoto, "lossless keeps the profile");
    assert_eq!(space(true, OutputSpace::Srgb, true), OutputSpace::Rec2020);
    let o = ExportOptions { format: ExportFormat::Jxl, hdr: true, ..Default::default() };
    assert!(o.hdr_output());
    assert_eq!(o.effective_depth(), OutputDepth::F32Hdr);
    assert_eq!(ExportFormat::Jxl.extension(), "jxl");
}

#[test]
fn jxl_every_space_round_trips() {
    // wide-gamut green stays outside sRGB in P3, Rec. 2020 and (lossless) ProPhoto files
    for (lossless, space) in [
        (false, OutputSpace::Srgb),
        (false, OutputSpace::DisplayP3),
        (false, OutputSpace::Rec2020),
        (true, OutputSpace::DisplayP3),
        (true, OutputSpace::AdobeRgb),
        (true, OutputSpace::ProPhoto),
    ] {
        let o = ExportOptions { format: ExportFormat::Jxl, color_space: space, jxl_lossless: lossless, quality: 95, ..Default::default() };
        let png = ExportOptions { format: ExportFormat::Png, color_space: space, ..Default::default() };
        let img = p3_green(space);
        let bytes = encode_image(&img, &o).unwrap();
        assert_eq!(lightcraft_codecs::sniff(&bytes), Some(lightcraft_codecs::Format::Jxl));
        let (got, _) = decoded_in_srgb(&bytes);
        let (want, _) = decoded_in_srgb(&encode_image(&img, &png).unwrap());
        for c in 0..3 {
            assert!((got[c] - want[c]).abs() < 0.01, "{space:?} lossless={lossless}: {got:?} vs PNG {want:?}");
        }
        if space != OutputSpace::Srgb {
            assert!(got[0] < -0.03 || got[2] < -0.03, "{space:?}: outside sRGB after the round trip: {got:?}");
        }
    }
}

#[test]
fn jxl_sixteen_bit() {
    let o = ExportOptions {
        format: ExportFormat::Jxl,
        bit_depth: Some(16),
        jxl_lossless: true,
        color_space: OutputSpace::DisplayP3,
        ..Default::default()
    };
    assert_eq!(o.effective_depth(), OutputDepth::U16);
    let (levels8, _) = distinct_levels(
        &encode_rendered(&ramp(OutputDepth::U8, OutputSpace::DisplayP3), &ExportOptions { bit_depth: Some(8), ..o.clone() }, None).unwrap(),
    );
    let (levels, d) = distinct_levels(&encode_rendered(&ramp(OutputDepth::U16, OutputSpace::DisplayP3), &o, None).unwrap());
    assert_eq!(d.bit_depth, 16);
    assert!(levels > 10 * levels8, "{levels} vs {levels8}");
    // lossy 16-bit encodes too
    let lossy = ExportOptions { jxl_lossless: false, ..o };
    assert!(encode_rendered(&ramp(OutputDepth::U16, OutputSpace::DisplayP3), &lossy, None).is_ok());
}

#[test]
fn bit_depth_options_per_format() {
    let d = |format, bit_depth| ExportOptions { format, bit_depth, ..Default::default() }.effective_depth();
    assert_eq!(d(ExportFormat::Jpeg, Some(16)), OutputDepth::U8);
    assert_eq!(d(ExportFormat::Webp, None), OutputDepth::U8);
    assert_eq!(d(ExportFormat::Png, None), OutputDepth::U8);
    assert_eq!(d(ExportFormat::Png, Some(16)), OutputDepth::U16);
    assert_eq!(d(ExportFormat::Avif, Some(10)), OutputDepth::U16);
    assert_eq!(ExportOptions::from_json(&serde_json::json!({"bitDepth": 16})).bit_depth, Some(16));
    assert_eq!(ExportOptions::from_json(&serde_json::json!({"bitDepth": 12})).bit_depth, None);
    assert_eq!(d(ExportFormat::Jxl, Some(16)), OutputDepth::U16);
    assert_eq!(d(ExportFormat::Jxl, None), OutputDepth::U8);
    for f in [ExportFormat::Jpeg, ExportFormat::Png, ExportFormat::Tiff, ExportFormat::Webp, ExportFormat::Avif, ExportFormat::Jxl] {
        let first = ExportOptions::bit_depths(f)[0].0;
        let def = d(f, None);
        assert_eq!(d(f, Some(first)), def, "{f:?}: the first choice is the default");
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn avif_ten_bit() {
    let o = ExportOptions { format: ExportFormat::Avif, bit_depth: Some(10), quality: 60, ..Default::default() };
    let r = ramp(OutputDepth::U16, OutputSpace::Srgb);
    match encode_rendered(&r, &o, None) {
        Ok(b) => assert_eq!(&b[4..8], b"ftyp"),
        Err(e) => assert!(e.contains("not available"), "{e}"),
    }
}

/// Issue #181: a command about to export refuses what `from_json` would silently default —
/// unknown keys (naming the nearest known one) and values of the wrong kind or range.
#[test]
fn export_params_are_validated() {
    assert_invalid_export_params();
    assert_export_options_round_trip();
}

fn assert_invalid_export_params() {
    use serde_json::json;
    let err = |p: serde_json::Value| ExportOptions::from_params(&p).err().map(|e| e.to_string()).unwrap_or_default();
    let e = err(json!({"longEdgee": 400}));
    assert!(e.starts_with("invalid parameters for `app.export`: unknown parameter `longEdgee` (did you mean `longEdge`?)"), "{e}");
    assert!(err(json!({"qualty": 5})).contains("did you mean `quality`"));
    assert!(err(json!({"longEdge": "banana"})).contains("`longEdge` must be a number"));
    assert!(err(json!({"quality": 0})).contains("`quality` must be an integer 1..100"));
    assert!(err(json!({"quality": "5"})).contains("`quality` must be an integer"));
    assert!(err(json!({"quality": 92.5})).contains("`quality` must be an integer"));
    assert!(err(json!({"sharpen": "lots"})).contains("`sharpen` must be one of none|screen|matte|glossy"));
    assert!(err(json!({"format": "bmp"})).contains("`format` must be one of"));
    assert!(err(json!({"bitDepth": 12})).contains("`bitDepth` must be 8, 10, 16 or 32"));
    assert!(err(json!({"colorSpace": "cmyk"})).contains("`colorSpace` must be one of"));
    assert!(err(json!({"dontEnlarge": "no"})).contains("`dontEnlarge` must be true or false"));
    assert!(err(json!({"fit": "sideways"})).contains("`fit` must be one of dimensions|widthHeight"));
    assert!(err(json!({"resize": {"mode": "longEdge", "valeu": 10}})).contains("did you mean `value`"));
    assert!(err(json!({"resize": {"mode": "diagonal", "value": 10}})).contains("`resize`:"));
    assert!(err(json!({"ids": [1, "x"]})).contains("`ids` must be an array of photo ids"));
    assert!(err(json!("jpeg")).contains("params must be an object"));
    assert!(
        ExportOptions::validate("export.savePreset", &json!({"qualty": 5}))
            .unwrap_err()
            .to_string()
            .starts_with("invalid parameters for `export.savePreset`")
    );
}

fn assert_export_options_round_trip() {
    use serde_json::json;
    let err = |p: serde_json::Value| ExportOptions::from_params(&p).err().map(|e| e.to_string()).unwrap_or_default();
    // everything the presets, the dialog, Export with Previous and `render --opt` send passes
    for p in builtin_presets() {
        ExportOptions::from_params(&p.params).unwrap_or_else(|e| panic!("{}: {e}", p.name));
    }
    let full = ExportOptions {
        resize: Some(Resize::default()),
        limit_kb: Some(500),
        watermark: Some(Watermark { text: "©".into(), ..Default::default() }),
        format: ExportFormat::Png,
        bit_depth: Some(16),
        ..Default::default()
    };
    let mut j = full.to_json();
    for (k, v) in [
        ("dir", json!("/tmp/out")),
        ("background", json!(true)),
        ("preset", json!("x")),
        ("ids", json!([1, 2])),
        ("id", json!(1)),
        ("path", json!("a.jpg")),
    ] {
        j[k] = v;
    }
    assert_eq!(ExportOptions::from_params(&j).unwrap(), full);
    // Add to Apple Photos (issue #236): a switch and an album name; on only where Photos is
    assert!(err(json!({"addToPhotos": "yes"})).contains("`addToPhotos` must be true or false"));
    assert!(err(json!({"photosAlbum": 3})).contains("`photosAlbum` must be a string"));
    assert!(err(json!({"photosAlbum": "a\0b"})).contains("NUL"));
    assert!(err(json!({"photosAlbm": "x"})).contains("did you mean `photosAlbum`"));
    let album = ExportOptions { photos_album: "Trip “2026” \\ \"x\"".into(), ..Default::default() };
    assert_eq!(ExportOptions::from_params(&album.to_json()).unwrap(), album);
    let on = ExportOptions { add_to_photos: true, ..album };
    if cfg!(target_os = "macos") {
        assert_eq!(ExportOptions::from_params(&on.to_json()).unwrap(), on);
    } else {
        assert!(err(on.to_json()).contains("Apple Photos is only available on macOS"));
        // stored settings (a preset or Export with Previous made on a Mac) still read
        assert!(ExportOptions::from_json(&on.to_json()).add_to_photos);
    }
    assert_eq!(ExportOptions::from_json(&json!({"photosAlbum": "  Trip  "})).photos_album, "Trip");
    assert_eq!(ExportOptions::from_params(&ExportOptions::default().to_json()).unwrap(), ExportOptions::default());
    // null is "not given", as a dropped key is
    ExportOptions::from_params(&json!({"quality": 92, "longEdge": null, "watermark": null})).unwrap();
}

/// Issue #1952: a `bitDepth` the format doesn't have is refused (the Export dialog never offers
/// it), not written at the format's own depth; the error names the depths the format takes.
#[test]
fn bit_depth_must_suit_the_format() {
    use serde_json::json;
    let err = |p: serde_json::Value| ExportOptions::from_params(&p).err().map(|e| e.to_string()).unwrap_or_default();
    assert!(err(json!({"format": "jpeg", "bitDepth": 16})).contains("`bitDepth` must be 8 for jpeg"));
    assert!(err(json!({"format": "png", "bitDepth": 10})).contains("`bitDepth` must be 8 or 16 for png"));
    assert!(err(json!({"format": "tiff", "bitDepth": 10})).contains("`bitDepth` must be 8, 16 or 32 for tiff"));
    // no `format`: the one `path` names, else the default (JPEG)
    assert!(err(json!({"path": "/out/a.png", "bitDepth": 32})).contains("for png"));
    assert!(err(json!({"bitDepth": 16})).contains("for jpeg"));
    assert!(ExportOptions::from_params(&json!({"path": "/out/a.tif", "bitDepth": 32})).is_ok());
    // a path naming no format, or a `null` format, exports the default too
    assert!(err(json!({"path": "/out/output", "bitDepth": 10})).contains("for jpeg"));
    assert!(err(json!({"path": "/out/output.xyz", "bitDepth": 10})).contains("for jpeg"));
    assert!(err(json!({"format": null, "path": "/out/a.png", "bitDepth": 16})).contains("for jpeg"));
    // or the caller's own format for such a path, which is the one returned
    let png = |p: serde_json::Value| ExportOptions::from_params_or(&p, ExportFormat::Png);
    assert!(png(json!({"path": "/out/output", "bitDepth": 10})).unwrap_err().to_string().contains("for png"));
    assert_eq!(png(json!({"path": "/out/output.xyz", "bitDepth": 16})).unwrap().format, ExportFormat::Png);
    assert_eq!(png(json!({"path": "/out/a.tif", "bitDepth": 32})).unwrap().format, ExportFormat::Tiff);
    // stored settings (a preset, Export with Previous) lose such a depth instead of failing
    let stored = ExportOptions::known_keys_only(&json!({"format": "jpeg", "bitDepth": 16}));
    assert_eq!(stored.get("bitDepth"), None);
    assert_eq!(ExportOptions::known_keys_only(&json!({"format": "tiff", "bitDepth": 32}))["bitDepth"], 32);
    for f in [
        ExportFormat::Jpeg,
        ExportFormat::Png,
        ExportFormat::Tiff,
        ExportFormat::Psd,
        ExportFormat::Webp,
        ExportFormat::Avif,
        ExportFormat::Jxl,
        ExportFormat::Original,
        ExportFormat::Dng,
        ExportFormat::RenderedDng,
    ] {
        for &(b, _) in ExportOptions::bit_depths(f) {
            let o = ExportOptions { format: f, bit_depth: Some(b), ..Default::default() };
            assert_eq!(ExportOptions::from_params(&o.to_json()).ok(), Some(o), "{f:?} {b}");
        }
    }
}

/// Issue #956: `destinationMode` picks a specific folder (the default) or each photo's own
/// folder; it is checked like the other params and kept in presets and Export with Previous.
#[test]
fn destination_mode_param() {
    use serde_json::json;
    assert_eq!(ExportOptions::default().destination_mode, DestinationMode::SpecificFolder);
    for (v, want) in [
        ("sameAsOriginal", DestinationMode::SameAsOriginal),
        ("SameAsOriginal", DestinationMode::SameAsOriginal),
        ("specificFolder", DestinationMode::SpecificFolder),
    ] {
        assert_eq!(ExportOptions::from_params(&json!({"destinationMode": v})).unwrap().destination_mode, want, "{v}");
    }
    let e = ExportOptions::from_params(&json!({"destinationMode": "nextToIt"})).unwrap_err().to_string();
    assert!(e.contains("`destinationMode` must be one of specificFolder|sameAsOriginal"), "{e}");
    assert!(ExportOptions::from_params(&json!({"destinationMode": 1})).is_err());
    assert!(
        ExportOptions::from_params(&json!({"destinationMod": "sameAsOriginal"})).unwrap_err().to_string().contains("did you mean `destinationMode`")
    );
    // saved (a preset, `lastExport`) and read back
    let o = ExportOptions { destination_mode: DestinationMode::SameAsOriginal, subfolder: "Exports".into(), ..Default::default() };
    let j = o.to_json();
    assert_eq!(j["destinationMode"], "sameAsOriginal");
    assert_eq!(ExportOptions::from_params(&ExportOptions::known_keys_only(&j)).unwrap(), o);
}

/// A choice the validation accepts is the one exported: `metadata: "NONE"` passed the
/// case-insensitive check, then failed the exact-case read and fell back to `all`, so GPS and
/// camera data went out in a file meant to carry none. `sharpen`, `sharpenAmount` and
/// `conflict` had the same gap.
#[test]
fn accepted_choices_are_the_ones_exported() {
    use serde_json::json;
    let read = |k: &str, v: &str| ExportOptions::from_params(&json!({ k: v })).map_err(|e| e.to_string());
    let metadata = [
        ("all", MetadataPolicy::All),
        ("allExceptCamera", MetadataPolicy::AllExceptCamera),
        ("copyright", MetadataPolicy::Copyright),
        ("none", MetadataPolicy::None),
    ];
    for (name, want) in metadata {
        for v in [name.to_string(), name.to_ascii_uppercase(), name.to_ascii_lowercase()] {
            assert_eq!(read("metadata", &v).unwrap().metadata, want, "metadata: {v}");
        }
    }
    for (name, want) in [("none", SharpenFor::None), ("screen", SharpenFor::Screen), ("matte", SharpenFor::Matte), ("glossy", SharpenFor::Glossy)] {
        assert_eq!(read("sharpen", &name.to_ascii_uppercase()).unwrap().sharpen, want);
    }
    for (name, want) in [("low", SharpenAmount::Low), ("standard", SharpenAmount::Standard), ("high", SharpenAmount::High)] {
        assert_eq!(read("sharpenAmount", &name.to_ascii_uppercase()).unwrap().sharpen_amount, want);
    }
    for (name, want) in [("unique", Conflict::Unique), ("overwrite", Conflict::Overwrite), ("skip", Conflict::Skip)] {
        assert_eq!(read("conflict", &name.to_ascii_uppercase()).unwrap().conflict, want);
    }
    // an explicit value nothing reads as is an error, never the permissive default
    for v in ["", " none", "nothing", "all-except-camera"] {
        let e = read("metadata", v).unwrap_err();
        assert!(e.contains("`metadata` must be one of all|allExceptCamera|copyright|none"), "{v:?}: {e}");
    }
    assert!(read("sharpenAmount", "max").unwrap_err().contains("`sharpenAmount` must be one of low|standard|high"));
    assert!(read("conflict", "replace").unwrap_err().contains("`conflict` must be one of unique|overwrite|skip"));
    // absent (or null) keeps the defaults
    let d = ExportOptions::from_params(&json!({"metadata": null})).unwrap();
    assert_eq!(
        (d.metadata, d.sharpen, d.sharpen_amount, d.conflict),
        (MetadataPolicy::All, SharpenFor::None, SharpenAmount::Standard, Conflict::Unique)
    );
    // `removeLocation` stays independent of the policy's spelling
    let o = ExportOptions::from_params(&json!({"metadata": "AllExceptCamera", "removeLocation": true})).unwrap();
    assert_eq!((o.metadata, o.remove_location), (MetadataPolicy::AllExceptCamera, true));
    // stored settings (lenient `from_json`) read the same spellings
    assert_eq!(ExportOptions::from_json(&json!({"metadata": "None"})).metadata, MetadataPolicy::None);
    // and `to_json` still writes what the MCP schema lists
    let j = ExportOptions { metadata: MetadataPolicy::AllExceptCamera, conflict: Conflict::Skip, ..Default::default() }.to_json();
    assert_eq!((j["metadata"].as_str(), j["conflict"].as_str()), (Some("allExceptCamera"), Some("skip")));
}

/// Issue #183: the `watermark` object's keys, kinds and ranges are checked (and documented in
/// the `app.export` param string), instead of unknown keys passing and sizes clamping silently.
#[test]
fn watermark_params_are_validated() {
    use serde_json::json;
    let err = |w: serde_json::Value| ExportOptions::from_params(&json!({"watermark": w})).err().map(|e| e.to_string()).unwrap_or_default();
    assert!(err(json!(5)).contains(
        "`watermark` must be text or an object {text, vertical, size, opacity, anchor, insetX, insetY, inset, rotation, color, shadow, image, imageWidth}"
    ));
    assert!(err(json!({"text": "x", "bogus": 1})).contains(
        "unknown watermark key `bogus` (one of text, vertical, size, opacity, anchor, insetX, insetY, inset, rotation, color, shadow, image, imageWidth)"
    ));
    assert!(err(json!({"text": "x", "sizee": 0.1})).contains("did you mean `size`"));
    for size in [json!(3), json!(0.5001), json!(0), json!("big")] {
        let e = err(json!({"text": "x", "size": size}));
        assert!(e.contains("`watermark.size` must be a number 0.005..0.5 (fraction of the short edge)"), "{size}: {e}");
    }
    assert!(err(json!({"text": "x", "opacity": 1.5})).contains("`watermark.opacity` must be a number 0..1"));
    assert!(err(json!({"text": "x", "inset": -0.1})).contains("`watermark.inset` must be a number 0..0.4"));
    assert!(err(json!({"text": "x", "insetX": 0.5})).contains("`watermark.insetX` must be a number 0..0.4"));
    assert!(err(json!({"text": "x", "insetY": "far"})).contains("`watermark.insetY` must be a number 0..0.4"));
    for rotation in [json!(45), json!(-90), json!(360), json!("90"), json!(90.5)] {
        let e = err(json!({"text": "x", "rotation": rotation}));
        assert!(e.contains("`watermark.rotation` must be 0, 90, 180 or 270 (degrees clockwise)"), "{rotation}: {e}");
    }
    assert!(ExportOptions::from_params(&json!({"watermark": {"text": "x", "rotation": 270}})).is_ok());
    assert!(err(json!({"image": "logo.png", "imageWidth": 2})).contains("`watermark.imageWidth` must be a number 0.01..1"));
    assert!(err(json!({"text": "x", "anchor": "middle"})).contains("`watermark.anchor` must be one of"));
    assert!(err(json!({"text": "x", "color": [255, 255]})).contains("`watermark.color` must be [r, g, b]"));
    assert!(err(json!({"text": "x", "color": [255, 255, 256]})).contains("`watermark.color`"));
    assert!(err(json!({"text": "x", "vertical": "yes"})).contains("`watermark.vertical` must be true or false"));
    assert!(err(json!({"text": 7})).contains("`watermark.text` must be a string"));
    // the documented shapes pass and read as before
    let o = ExportOptions::from_params(&json!({"watermark": "© me"})).unwrap();
    assert_eq!(o.watermark.map(|w| w.text).as_deref(), Some("© me"));
    let w = json!({"text": "日本語", "vertical": true, "size": 0.1, "opacity": 0.5, "anchor": "topLeft", "inset": 0.05, "color": [0, 0, 0], "shadow": false, "image": "", "imageWidth": 0.3});
    let wm = ExportOptions::from_params(&json!({"watermark": w})).unwrap().watermark.unwrap();
    assert!((wm.size - 0.1).abs() < 1e-6 && wm.vertical && wm.anchor == Anchor::TopLeft && wm.color == [0, 0, 0] && !wm.shadow);
    assert_eq!(ExportOptions::from_params(&json!({"watermark": {"text": "x", "size": 0.5}})).unwrap().watermark.map(|w| w.size), Some(0.5));
    // the Export dialog's sliders (1–15 % size, 5–100 % opacity, 2–100 % width) stay inside the accepted ranges
    assert!(WATERMARK_SIZE_RANGE.0 <= 0.01 && WATERMARK_SIZE_RANGE.1 >= 0.15);
    assert!(WATERMARK_IMAGE_WIDTH_RANGE.0 <= 0.02 && WATERMARK_IMAGE_WIDTH_RANGE.1 >= 1.0);
}

/// A batch with `lanes` photos in flight into a fake folder `out` (nothing exists there):
/// results, writes (path, bytes) and progress calls; `progress` returns false at `cancel_at`.
#[allow(clippy::type_complexity)]
fn lanes_batch(
    s: &mut crate::Session,
    ids: &[lightcraft_catalog::PhotoId],
    o: &ExportOptions,
    lanes: usize,
    stop_on_error: bool,
    cancel_at: usize,
) -> (Result<Vec<serde_json::Value>, String>, Vec<(String, Vec<u8>)>, Vec<(usize, String)>) {
    let items = prepare_batch(s, ids, o).unwrap();
    let to = Destination { dir: "out".into(), exact: None };
    let mut written = Vec::new();
    let mut write = |p: &str, b: &[u8]| {
        written.push((p.to_string(), b.to_vec()));
        Ok(())
    };
    let mut seen = Vec::new();
    let mut progress = |i: usize, name: &str| {
        seen.push((i, name.to_string()));
        i < cancel_at
    };
    let files = run_batch_with(items, &mut Placer::new(o, &to, &mut write, &|_| false, stop_on_error), &mut progress, lanes);
    (files, written, seen)
}

/// The files of a batch without their byte counts: a GPU render (when another test turns the
/// GPU off mid-run) may differ from a CPU one by an LSB, never in name, size or order.
fn shape(files: &[serde_json::Value]) -> Vec<serde_json::Value> {
    files.iter().map(|f| json!({"path": f["path"], "width": f["width"], "height": f["height"], "error": f["error"]})).collect()
}

/// Issue #675: a batch reports a render that fell back from the GPU to the CPU on that file's
/// entry (`gpuFallback`, the render's own reason) and says nothing on the others.
#[test]
fn a_batch_reports_a_gpu_fallback_on_the_file_it_hit() {
    if !lightcraft_gpu::available() {
        lightcraft_testkit::skip!(Gpu, "no GPU adapter ({})", lightcraft_gpu::unavailable_reason().unwrap_or_default());
        return;
    }
    let mut s = crate::Session::with_demo();
    let ids: Vec<_> = s.visible().iter().copied().take(2).collect();
    let o = ExportOptions::from_json(&json!({"format": "png", "width": 64}));
    // one lane: both photos render on this thread, and the fault hits the first render here
    lightcraft_gpu::inject_fault(lightcraft_gpu::Fault::Limit(1 << 10));
    let (files, _, _) = lanes_batch(&mut s, &ids, &o, 1, true, usize::MAX);
    let files = files.unwrap();
    assert_eq!(files.len(), 2, "{files:?}");
    let why: Vec<_> = files.iter().filter_map(|f| f.get("gpuFallback").and_then(serde_json::Value::as_str)).collect();
    assert_eq!(why.len(), 1, "exactly one file fell back: {files:?}");
    assert!(why[0].contains("limit"), "{files:?}");
    assert!(files[1].get("gpuFallback").is_none(), "the second render was fine: {files:?}");
    assert!(lightcraft_gpu::available(), "a limit is no device failure");
}

// Issue #496: photos rendered side by side give the same files, names, order and progress as
// Render to DNG: the rendered photo as a linear float DNG that reads back as the same picture
#[test]
fn rendered_dng_matches_the_rendered_tiff() {
    let mut s = crate::Session::with_demo();
    let id = s.visible()[0];
    let dng = export_photo(&mut s, id, &ExportOptions::from_json(&json!({"format": "rendereddng", "width": 64})), 1).unwrap();
    assert!(dng.file_name.ends_with(".dng"), "{}", dng.file_name);
    let png = export_photo(&mut s, id, &ExportOptions::from_json(&json!({"format": "png", "width": 64})), 1).unwrap();
    let raw = lightcraft_raw::decode(&dng.bytes).unwrap();
    assert_eq!((raw.width, raw.height, raw.cpp), (dng.width, dng.height, 3));
    assert!(raw.color.profile.tone_curve.is_some(), "identity tone curve keeps converters from adding one");
    let back = raw.develop(lightcraft_raw::Method::Bilinear).unwrap();
    let reference = lightcraft_codecs::decode_thumbnail(&png.bytes, 4096).unwrap().image;
    assert_eq!((reference.width, reference.height), (back.width, back.height));
    // mean brightness: the PNG holds 8-bit sRGB, the DNG linear sRGB
    let mean_dng: f32 = back.data.iter().map(|p| lightcraft_color::transfer::linear_to_srgb(p[1].min(1.0))).sum::<f32>() / back.data.len() as f32;
    let mean_tif: f32 = reference.data.iter().map(|p| p[1] as f32 / 255.0).sum::<f32>() / reference.data.len() as f32;
    assert!((mean_dng - mean_tif).abs() < 0.02, "{mean_dng} vs {mean_tif}");
}

// one at a time, and a cancel stops at the same photo
#[test]
fn side_by_side_batches_match_one_at_a_time() {
    let mut s = crate::Session::with_demo();
    let ids: Vec<_> = s.visible().iter().copied().take(5).collect();
    // one name for all: the Unique numbering follows the order the files are placed in
    let o = ExportOptions::from_json(&json!({"format": "png", "width": 40, "naming": "same"}));
    let (files, written, seen) = lanes_batch(&mut s, &ids, &o, 1, true, usize::MAX);
    let files = files.unwrap();
    let paths: Vec<_> = written.iter().map(|(p, _)| p.clone()).collect();
    assert_eq!(paths, ["out/same.png", "out/same-2.png", "out/same-3.png", "out/same-4.png", "out/same-5.png"]);
    let heights: std::collections::HashSet<_> = files.iter().map(|f| f["height"].as_u64()).collect();
    assert!(heights.len() > 1, "photos of different shapes show the order: {files:?}");
    for lanes in [2, 3, 8] {
        let (f, w, sn) = lanes_batch(&mut s, &ids, &o, lanes, true, usize::MAX);
        assert_eq!(shape(&f.unwrap()), shape(&files), "{lanes} lanes");
        assert_eq!(w.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(), paths, "{lanes} lanes");
        assert_eq!(sn, seen, "{lanes} lanes");
    }
    let (f, w, sn) = lanes_batch(&mut s, &ids, &o, 3, true, 2);
    assert_eq!(shape(&f.unwrap()), shape(&files[..2]));
    assert_eq!((w.len(), sn.len()), (2, 3), "photos past the cancel are never written");
}

// A panic in `progress` while lanes run must end the batch (the lanes stop waiting), not hang it
#[test]
fn a_panic_in_progress_does_not_hang_the_lanes() {
    let mut s = crate::Session::with_demo();
    let ids: Vec<_> = s.visible().iter().copied().take(5).collect();
    let o = ExportOptions::from_json(&json!({"format": "png", "width": 40}));
    let items = prepare_batch(&mut s, &ids, &o).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let to = Destination { dir: "out".into(), exact: None };
        let mut write = |_: &str, _: &[u8]| Ok(());
        let mut calls = 0;
        let mut progress = |_: usize, _: &str| {
            calls += 1;
            assert!(calls < 2, "progress panics on its second call");
            true
        };
        let r =
            crate::guard::catch("test batch", || run_batch_with(items, &mut Placer::new(&o, &to, &mut write, &|_| false, true), &mut progress, 2));
        tx.send(r.and_then(|r| r)).ok();
    });
    let r = rx.recv_timeout(std::time::Duration::from_secs(60));
    assert!(matches!(r, Ok(Err(_))), "the batch returns an error instead of hanging: {r:?}");
}

// A photo that fails is reported in its place while the others are exported; with
// stop_on_error the batch ends there and nothing after it is written
#[test]
fn side_by_side_batches_report_failures_in_order() {
    use lightcraft_catalog::{Op, Photo, PhotoId, Source};
    let mut s = crate::Session::with_demo();
    let mut ids: Vec<_> = s.visible().iter().copied().take(3).collect();
    let gone = Photo::new(PhotoId(9_496), Source::File { path: "no/such/dir/gone.jpg".into() }, "gone.jpg", "JPEG", 600, 400, "2026-10-09T00:00:00");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(gone) }).unwrap();
    ids.insert(1, PhotoId(9_496));
    let o = ExportOptions::from_json(&json!({"format": "jpeg", "width": 40}));
    let (files, written, _) = lanes_batch(&mut s, &ids, &o, 3, false, usize::MAX);
    let files = files.unwrap();
    assert_eq!(files.len(), 4);
    assert_eq!((files[1]["photo"].as_u64(), files[1]["file"].as_str()), (Some(9_496), Some("gone.jpg")), "{files:?}");
    assert!(files[1]["error"].is_string());
    assert!([0, 2, 3].iter().all(|&i| files[i]["path"].is_string()), "{files:?}");
    assert_eq!(written.len(), 3);
    let one = lanes_batch(&mut s, &ids, &o, 1, false, usize::MAX).0.unwrap();
    assert_eq!(shape(&files), shape(&one));

    let (r, written, _) = lanes_batch(&mut s, &ids, &o, 3, true, usize::MAX);
    assert!(r.is_err());
    assert_eq!(written.len(), 1, "only the photo before the failure");
}

#[test]
fn export_weights_follow_the_source_and_output_sizes() {
    use crate::media::SourceLevel;
    let mb = |b: usize| b >> 20;
    // a full-size 24 MP export (measured peak ~1.2 GB)
    assert_eq!(mb(render_weight(6000 * 4000, 6000, SourceLevel::Full, 6000 * 4000)), 1098);
    // 2048 px from the 2560 px preview (measured ~150 MB)
    assert_eq!(mb(render_weight(6000 * 4000, 6000, SourceLevel::Preview, 2048 * 1365)), 145);
    // a photo smaller than the preview level is its own source
    assert_eq!(render_weight(1000 * 500, 1000, SourceLevel::Preview, 0), 1000 * 500 * 12);
    assert_eq!(render_weight(0, 0, SourceLevel::Full, 0), 0);
    assert!(render_weight(usize::MAX, 1, SourceLevel::Full, usize::MAX) > 0, "no overflow");
}
