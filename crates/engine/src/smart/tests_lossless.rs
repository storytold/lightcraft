use super::*;
use lightcraft_develop::DevelopSettings;
use lightcraft_pipeline::output::{DeepSamples, OutputDepth};
use lightcraft_pipeline::{RenderRequest, SourceInfo};

fn scene() -> Rgb32f {
    let mut img = Rgb32f::new(1024, 64);
    for (i, p) in img.data.iter_mut().enumerate() {
        let x = (i % 1024) as f32 / 1023.0;
        *p = [0.01 + x * 0.6, 0.012 + x * 0.7, 0.018 + x * 0.8];
    }
    // Fewer than 0.05% of pixels, so the compact format's scale clips them.
    for (i, p) in img.data.iter_mut().take(16).enumerate() {
        *p = [2.0 + i as f32 / 4.0, 1.5 + i as f32 / 3.0, -0.01];
    }
    img
}

fn rendered(img: &Rgb32f, settings: &DevelopSettings) -> Vec<f32> {
    let info = SourceInfo { raw: true, ..Default::default() };
    let req = RenderRequest { depth: OutputDepth::F32Linear, ..RenderRequest::fit(img.width, img.height) };
    let r = lightcraft_pipeline::render(img, &info, settings, &req);
    match r.deep.unwrap().samples {
        DeepSamples::F32(v) => v,
        _ => panic!("expected float render"),
    }
}

/// CIE76 in D65 Lab, from the renderer's linear sRGB output.
fn lab(rgb: &[f32]) -> [f64; 3] {
    let [r, g, b] = [rgb[0] as f64, rgb[1] as f64, rgb[2] as f64];
    let xyz = [
        (0.4124564 * r + 0.3575761 * g + 0.1804375 * b) / 0.95047,
        0.2126729 * r + 0.7151522 * g + 0.0721750 * b,
        (0.0193339 * r + 0.1191920 * g + 0.9503041 * b) / 1.08883,
    ];
    let [x, y, z] = xyz.map(|v| if v > 216.0 / 24389.0 { v.cbrt() } else { v * 841.0 / 108.0 + 4.0 / 29.0 });
    [116.0 * y - 16.0, 500.0 * (x - y), 200.0 * (y - z)]
}

fn delta_e(a: &[f32], b: &[f32]) -> Vec<f64> {
    a.as_chunks::<3>()
        .0
        .iter()
        .zip(b.as_chunks::<3>().0.iter())
        .map(|(a, b)| {
            let a = lab(a);
            let b = lab(b);
            a.iter().zip(b).map(|(a, b)| (a - b).powi(2)).sum::<f64>().sqrt()
        })
        .collect()
}

#[test]
fn lossless_preserves_float_bits_and_recovered_highlights() {
    let src = scene();
    let bytes = encode_format(&src, None, Format::Lossless).unwrap();
    let restored = decode(&bytes).unwrap().0;
    assert_eq!(src.data, restored.data);
    let jpeg = decode(&encode(&src, None).unwrap()).unwrap().0;
    for (exposure, highlights) in [(0.0, 0.0), (-2.0, -100.0), (2.0, 0.0)] {
        let mut s = DevelopSettings::default();
        s.light.exposure = exposure;
        s.light.highlights = highlights;
        let reference = rendered(&src, &s);
        let lossless = rendered(&restored, &s);
        let compact = rendered(&jpeg, &s);
        if exposure == 0.0 {
            let repeats = |v: &[f32]| v[1024 * 3..2048 * 3].as_chunks::<3>().0.windows(2).filter(|p| p[0] == p[1]).count();
            assert_eq!(repeats(&reference), repeats(&lossless));
            assert!(repeats(&compact) > repeats(&reference) + 100);
            println!("rendered gradient repeats: reference={} JPEG={} lossless={}", repeats(&reference), repeats(&compact), repeats(&lossless));
        }
        let lossless_e = delta_e(&reference, &lossless);
        let jpeg_e = delta_e(&reference, &compact);
        assert!(lossless_e.iter().all(|v| *v == 0.0));
        let mean = jpeg_e.iter().sum::<f64>() / jpeg_e.len() as f64;
        assert!(mean > 0.01, "JPEG must fail exact render parity: {mean}");
        if exposure == -2.0 {
            let highlights = jpeg_e.iter().take(16).sum::<f64>() / 16.0;
            assert!(highlights > 1.0, "clipped JPEG highlights must remain different: {highlights}");
        }
        println!("exposure={exposure} highlights={highlights}: mean deltaE76 JPEG={mean:.6} lossless=0");
    }
    let repeats = |img: &Rgb32f| img.data[1024..2048].windows(2).filter(|p| p[0] == p[1]).count();
    assert_eq!(repeats(&src), 0);
    assert_eq!(repeats(&restored), 0);
    assert!(repeats(&jpeg) > 100, "the 8-bit gradient has repeated adjacent values");
    println!("gradient repeated adjacent samples: JPEG={} lossless=0", repeats(&jpeg));
}

#[test]
fn future_header_fields_and_tone_survive_conversion() {
    let tone = CameraTone::new(std::array::from_fn(|i| {
        let x = 0.004 * 1.17f32.powi(i as i32);
        [x, x / (1.0 + x)]
    }))
    .unwrap()
    .with_chroma([0.8, 0.9, 1.0, 1.1, 1.2, 1.0, 0.8, 0.5])
    .unwrap();
    let src = scene();
    for storage in [Format::Compact, Format::Lossless] {
        let bytes = encode_format(&src, Some(&lightcraft_pipeline::SourceInfo { camera_tone: Some(tone), ..Default::default() }), storage).unwrap();
        let (_, mut previous, _) = format::split(&bytes).unwrap();
        previous["future"] = serde_json::json!({"some": "metadata"});
        previous["tone"]["white_stat"] = serde_json::json!(1.2345);
        let rewritten = preserve_header(&bytes, &previous).unwrap();
        let (_, head, pixels) = format::split(&rewritten).unwrap();
        assert_eq!(head["tone"]["white_stat"], previous["tone"]["white_stat"]);
        assert_eq!(head["future"], previous["future"]);
        assert_eq!(pixels, format::split(&bytes).unwrap().2);
        assert_eq!(decode(&rewritten).unwrap().1, Some(tone));
        // A rebuild without a fitted curve still retains metadata this writer does not know.
        let without_tone = encode_format(&src, None, storage).unwrap();
        let rewritten = preserve_header(&without_tone, &previous).unwrap();
        assert_eq!(format::split(&rewritten).unwrap().1["tone"], serde_json::json!({"white_stat": 1.2345}));
        assert_eq!(decode(&rewritten).unwrap().1, None);
    }
}

#[test]
fn bounded_readers_reject_truncation_corruption_and_hostile_headers() {
    let mut src = Rgb32f::new(2, 2);
    src.data.fill([0.25, -0.5, 8.0]);
    let good = encode_format(&src, None, Format::Lossless).unwrap();
    for cut in 0..good.len() {
        assert!(decode(&good[..cut]).is_err(), "cut={cut}");
    }
    let (_, head, body) = format::split(&good).unwrap();
    for i in 0..body.len() {
        let mut changed = body.to_vec();
        changed[i] ^= 0x40;
        assert!(decode(&format::pack(Format::Lossless, &head, &changed).unwrap()).is_err(), "byte={i}");
    }
    for header in [
        serde_json::json!({"w": u64::MAX, "h": u64::MAX}),
        serde_json::json!({"w": 0, "h": 1}),
        serde_json::json!({"w": 8192, "h": 8192}),
        serde_json::json!({"w": 2, "h": 2, "scale": -2}),
    ] {
        let legacy = format::pack(Format::Compact, &header, b"broken pixels").unwrap();
        assert!(decode(&legacy).is_err());
    }
    let mut oversized = MAGIC.to_vec();
    oversized.extend(vec![b' '; HEADER_MAX as usize]);
    oversized.push(b'\n');
    assert!(decode(&oversized).is_err());
    src.data[0][0] = f32::NAN;
    assert!(encode_format(&src, None, Format::Lossless).is_err());
}

#[test]
fn lossless_completeness_and_mixed_format_byte_accounting() {
    let dir = super::tests::temp("lossless-stats");
    let mut src = Rgb32f::new(2, 2);
    src.data.fill([0.2, 0.3, 0.4]);
    let compact = encode(&src, None).unwrap();
    let lossless = encode_format(&src, None, Format::Lossless).unwrap();
    std::fs::write(dir.join("old.lcsp"), &compact).unwrap();
    std::fs::create_dir(dir.join("ab")).unwrap();
    let path = dir.join("ab/ab000000000000000000000000000000.lcsp");
    std::fs::write(&path, &lossless).unwrap();
    assert!(is_complete(&path));
    assert_eq!(stats(&dir), (2, (compact.len() + lossless.len()) as u64));
    for cut in [lossless.len() - 1, lossless.len() / 2, 0] {
        std::fs::write(&path, &lossless[..cut]).unwrap();
        assert!(!is_complete(&path));
    }
    for bytes in [&compact, &lossless] {
        let (storage, mut head, body) = format::split(bytes).unwrap();
        head["future"] = "".into();
        let available = HEADER_MAX as usize - MAGIC.len() - 1 - serde_json::to_vec(&head).unwrap().len();
        head["future"] = "x".repeat(available).into();
        let full = format::pack(storage, &head, body).unwrap();
        assert_eq!(full.len() - body.len(), HEADER_MAX as usize);
        std::fs::write(&path, &full).unwrap();
        assert!(is_complete(&path), "a header at the limit still permits the pixel marker to be read");
        assert!(decode(&full).is_ok());
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn folder_migration_keeps_a_complete_proxy_over_damaged_or_lower_precision_pixels() {
    let dir = super::tests::temp("lossless-migrate");
    let from = dir.join("from");
    let to = dir.join("to");
    std::fs::create_dir(&from).unwrap();
    std::fs::create_dir(&to).unwrap();
    let src = Rgb32f::new(2, 2);
    let compact = encode(&src, None).unwrap();
    let lossless = encode_format(&src, None, Format::Lossless).unwrap();
    for (source, destination, expected) in [
        (&lossless[..], &compact[..], &lossless[..]),
        (&compact[..], &lossless[..], &lossless[..]),
        (&lossless[..lossless.len() - 1], &compact[..], &compact[..]),
    ] {
        std::fs::write(from.join("one.lcsp"), source).unwrap();
        std::fs::write(to.join("one.lcsp"), destination).unwrap();
        let (moved, errors) = migrate(&from, &to, Existing::Move);
        assert_eq!(moved, 1);
        assert!(errors.is_empty());
        assert_eq!(std::fs::read(to.join("one.lcsp")).unwrap(), expected);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn regression709_migration_replaces_checksum_damaged_destination() {
    let dir = super::tests::temp("checksum-migrate");
    let from = dir.join("from");
    let to = dir.join("to");
    std::fs::create_dir(&from).unwrap();
    std::fs::create_dir(&to).unwrap();
    let image = Rgb32f::new(4, 3);
    let good = encode_format(&image, None, Format::Lossless).unwrap();
    let mut corrupt = good.clone();
    let byte = corrupt.len() - 9; // Last payload byte, before the intact end marker.
    corrupt[byte] ^= 0x40;
    assert!(decode(&corrupt).is_err());
    for source in [encode(&image, None).unwrap(), good] {
        std::fs::write(from.join("one.lcsp"), &source).unwrap();
        std::fs::write(to.join("one.lcsp"), &corrupt).unwrap();
        assert!(is_complete(&to.join("one.lcsp")), "structural hints cannot detect payload damage");
        assert!(!is_usable(&to.join("one.lcsp")));
        let (moved, errors) = migrate(&from, &to, Existing::Move);
        assert_eq!(moved, 1, "{errors:?}");
        assert!(errors.is_empty());
        assert!(load(&to.join("one.lcsp")).is_ok(), "migration deleted the last usable proxy");
        assert_eq!(std::fs::read(to.join("one.lcsp")).unwrap(), source);
        assert!(!from.join("one.lcsp").exists());
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn failed_verified_migration_retains_the_source() {
    let dir = super::tests::temp("verified-migrate-failure");
    let from = dir.join("from");
    let to = dir.join("to");
    std::fs::create_dir(&from).unwrap();
    std::fs::create_dir(&to).unwrap();
    let good = encode_format(&Rgb32f::new(4, 3), None, Format::Lossless).unwrap();
    std::fs::write(from.join("one.lcsp"), &good).unwrap();
    {
        let _fault = lightcraft_catalog::safe_file::fail_writes_after(64);
        let (moved, errors) = migrate(&from, &to, Existing::Move);
        assert_eq!(moved, 0);
        assert_eq!(errors.len(), 1);
        assert_eq!(std::fs::read(from.join("one.lcsp")).unwrap(), good);
        assert!(!to.join("one.lcsp").exists());
    }
    std::fs::write(from.join("one.lcsp"), b"damaged source").unwrap();
    let (moved, errors) = migrate(&from, &to, Existing::Move);
    assert_eq!(moved, 0);
    assert_eq!(errors.len(), 1);
    assert_eq!(std::fs::read(from.join("one.lcsp")).unwrap(), b"damaged source");
    std::fs::remove_dir_all(dir).unwrap();
}

/// Main's decoder-facts tail (issue #1534) rides after a Lossless proxy's end marker too, so an
/// offline edit of a Lossless proxy reads the same facts as a Compact one; a proxy cut inside the
/// tail is incomplete, and the payload checksum still covers the pixels alone.
#[test]
fn lossless_proxies_keep_the_decoder_facts() {
    use lightcraft_pipeline::masks::{MatteKind, Mattes};
    let src = Rgb32f::from_fn(32, 20, |x, y| [x as f32 / 32.0, y as f32 / 20.0, 0.5]);
    let mut mattes = Mattes::default();
    mattes.push(MatteKind::Sky, lightcraft_raster::Image { width: 16, height: 10, data: vec![255; 160] });
    let info = SourceInfo { raw: true, as_shot_temp: 4200.0, as_shot_tint: -5.0, mattes: Some(Arc::new(mattes)), ..Default::default() };
    let bytes = encode_format(&src, Some(&info), Format::Lossless).unwrap();
    let (pixels, _, back) = decode(&bytes).unwrap();
    assert_eq!(pixels.data, src.data, "the pixels stay bit-exact");
    let back = back.expect("the facts are read back");
    assert_eq!((back.raw, back.as_shot_temp, back.as_shot_tint), (true, 4200.0, -5.0));
    assert_eq!(back.mattes.expect("the mattes are read back").bytes(), 160);
    let dir = super::tests::temp("lossless-facts");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("f.lcsp");
    std::fs::write(&path, &bytes).unwrap();
    assert!(is_complete(&path) && is_usable(&path));
    std::fs::write(&path, &bytes[..bytes.len() - 8]).unwrap();
    assert!(!is_complete(&path), "a proxy cut inside its tail is incomplete");
    // a proxy written without facts (or before the tail existed) still reads, with none
    assert!(decode(&encode_format(&src, None, Format::Lossless).unwrap()).unwrap().2.is_none());
    std::fs::remove_dir_all(dir).unwrap();
}
