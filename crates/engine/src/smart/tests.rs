use super::*;

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-smart-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn writable_check_does_not_create_missing_parents() {
    let base = temp("writable");
    assert!(check_writable(&base.join("new")).is_ok(), "one missing level is created");
    assert!(base.join("new").is_dir());
    let err = check_writable(&base.join("gone/deeper")).unwrap_err();
    assert!(err.contains("not available"), "{err}");
    assert!(!base.join("gone").exists(), "no fallback onto the current drive");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn migrate_moves_leaves_or_discards() {
    let base = temp("migrate");
    let (a, b) = (base.join("a"), base.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(a.join("1.lcsp"), b"one").unwrap();
    std::fs::write(a.join("note.txt"), b"keep").unwrap();
    assert_eq!(stats(&a), (1, 3));
    assert_eq!(migrate(&a, &b, Existing::Leave).0, 0);
    assert_eq!(stats(&a).0, 1);
    assert_eq!(migrate(&a, &b, Existing::Move).0, 1);
    assert_eq!((stats(&a).0, stats(&b).0), (0, 1));
    assert!(a.join("note.txt").exists(), "only smart previews are touched");
    assert_eq!(migrate(&b, &a, Existing::Discard).0, 1);
    assert_eq!(stats(&b).0, 0);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn sharded_and_legacy_proxies_are_counted_and_moved() {
    let base = temp("shards");
    let from = base.join("from");
    let to = base.join("to");
    std::fs::create_dir_all(from.join("a1")).unwrap();
    std::fs::create_dir(&to).unwrap();
    let old = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2.lcsp";
    let new = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1.lcsp";
    std::fs::write(from.join(old), b"legacy").unwrap();
    std::fs::write(from.join("a1").join(new), b"sharded").unwrap();
    std::fs::write(from.join("a1").join("notes.txt"), b"user file").unwrap();
    std::fs::write(from.join("notes.lcsp.txt"), b"keep").unwrap();
    assert_eq!(stats(&from), (2, 13));
    let (moved, failed) = migrate(&from, &to, Existing::Move);
    assert_eq!((moved, failed), (2, Vec::<String>::new()));
    assert_eq!(stats(&from), (0, 0));
    assert_eq!(stats(&to), (2, 13));
    assert_eq!(std::fs::read(to.join("b2").join(old)).unwrap(), b"legacy");
    assert_eq!(std::fs::read(to.join("a1").join(new)).unwrap(), b"sharded");
    assert!(from.join("a1").join("notes.txt").exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn damaged_proxies_are_invalid_and_never_kept_over_good_ones() {
    let base = temp("valid");
    let (a, b) = (base.join("a"), base.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let img = lightcraft_scenes::demo_library()[0].render(64, 40);
    let good = encode(&img, None).unwrap();
    std::fs::write(a.join("1.lcsp"), &good).unwrap();
    assert!(is_valid(&a.join("1.lcsp")));
    for cut in [good.len() - 1, good.len() / 2, 10, 0] {
        std::fs::write(b.join("1.lcsp"), &good[..cut]).unwrap();
        assert!(!is_valid(&b.join("1.lcsp")), "cut at {cut}");
    }
    assert!(!is_valid(&b.join("missing.lcsp")));
    // moving onto a damaged copy replaces it instead of deleting the good one
    assert_eq!(migrate(&a, &b, Existing::Move).0, 1);
    assert_eq!(std::fs::read(b.join("1.lcsp")).unwrap(), good);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn camera_tone_travels_with_the_proxy() {
    let tone = CameraTone::new(std::array::from_fn(|i| {
        let x = 0.004 * 1.17f32.powi(i as i32);
        [x, 0.95 * (1.0 - (-2.7 * x).exp())]
    }))
    .unwrap();
    let img = lightcraft_scenes::demo_library()[0].render(64, 40);
    let info = lightcraft_pipeline::SourceInfo { raw: true, camera_tone: Some(tone), ..Default::default() };
    let bytes = encode(&img, Some(&info)).unwrap();
    assert_eq!(decode(&bytes).unwrap().1, Some(tone));
    let dir = temp("tone");
    let path = dir.join("t.lcsp");
    std::fs::write(&path, &bytes).unwrap();
    // the longer header is still read by the validity check, and the curve by `load`
    assert!(is_valid(&path));
    let loaded = load(&path).unwrap();
    assert_eq!(loaded.camera_tone, Some(tone));
    let header = lightcraft_pipeline::SourceInfo { raw: true, ..Default::default() };
    assert_eq!(loaded.info_or(header).camera_tone, Some(tone));
    // a hostile curve (reversing knots) is ignored, not trusted
    let nl = MAGIC.len() + bytes[MAGIC.len()..].iter().position(|b| *b == b'\n').unwrap();
    let mut head: serde_json::Value = serde_json::from_slice(&bytes[MAGIC.len()..nl]).unwrap();
    let mut knots: Vec<[f32; 2]> = serde_json::from_value(head["tone"]["knots"].clone()).unwrap();
    knots.swap(3, 4);
    head["tone"]["knots"] = serde_json::json!(knots);
    let mut hostile = MAGIC.to_vec();
    hostile.extend_from_slice(head.to_string().as_bytes());
    hostile.extend_from_slice(&bytes[nl..]);
    let (pixels, tone, _) = decode(&hostile).unwrap();
    assert_eq!((pixels.width, tone), (img.width, None));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn facts_and_mattes_travel_with_the_proxy() {
    let img = lightcraft_scenes::demo_library()[0].render(32, 20);
    let mut mattes = Mattes::default();
    mattes.push(MatteKind::Sky, Image { width: 16, height: 10, data: vec![255; 160] });
    let info = SourceInfo {
        raw: true,
        as_shot_temp: 4200.0,
        as_shot_tint: -5.0,
        legacy_relative_wb: true,
        mattes: Some(std::sync::Arc::new(mattes)),
        ..Default::default()
    };
    let bytes = encode(&img, Some(&info)).unwrap();
    let (_, _, back) = decode(&bytes).unwrap();
    let back = back.expect("the facts are read back");
    assert_eq!((back.as_shot_temp, back.as_shot_tint, back.legacy_relative_wb), (4200.0, -5.0, true));
    assert_eq!(back.mattes.expect("the mattes are read back").bytes(), 160);
    // a proxy cut inside its tail is incomplete (rebuilt, not taken for a source of facts)
    let dir = temp("facts");
    let path = dir.join("f.lcsp");
    std::fs::write(&path, &bytes[..bytes.len() - 8]).unwrap();
    assert!(!is_valid(&path));
    let _ = std::fs::remove_dir_all(&dir);
    // an old proxy (no tail) has no facts, and neither has one with a malformed tail
    assert!(decode(&encode(&img, None).unwrap()).unwrap().2.is_none());
    let nl = MAGIC.len() + bytes[MAGIC.len()..].iter().position(|b| *b == b'\n').unwrap();
    let mut head: serde_json::Value = serde_json::from_slice(&bytes[MAGIC.len()..nl]).unwrap();
    head["tail"] = serde_json::json!(bytes.len());
    let mut lying = MAGIC.to_vec();
    lying.extend_from_slice(head.to_string().as_bytes());
    lying.extend_from_slice(&bytes[nl..]);
    assert!(decode(&lying).map(|(_, _, i)| i.is_none()).unwrap_or(true), "an over-large tail is not trusted");
}

#[test]
fn roundtrip_keeps_the_picture() {
    let img = lightcraft_scenes::demo_library()[0].render(160, 100);
    let (back, tone, _) = decode(&encode(&img, None).unwrap()).unwrap();
    assert_eq!((back.width, back.height), (img.width, img.height));
    assert!(tone.is_none());
    let mut err = 0.0f64;
    for (a, b) in img.data.iter().zip(&back.data) {
        for k in 0..3 {
            let (x, y) = (linear_to_srgb(a[k].clamp(0.0, 1.0)), linear_to_srgb(b[k].clamp(0.0, 1.0)));
            err += ((x - y) as f64).abs();
        }
    }
    let mean = err / (img.data.len() * 3) as f64;
    assert!(mean < 0.02, "mean encoded error {mean}");
    assert!(decode(b"nope").is_err());
}

/// A smooth, deterministic stand-in for a photo: a bright, near-neutral "sky" crossing over into
/// a saturated colour field, every value inside 0..1 so the proxy's 0.05 % highlight clip never
/// enters what is measured. Built from a rendered demo scene so the dimensions and the flat
/// row-major layout are the real ones.
fn smooth_scene(w: usize, h: usize) -> Rgb32f {
    let mut img = lightcraft_scenes::demo_library()[0].render(w, h);
    let (w, h) = (img.width.max(1), img.height.max(1));
    for (i, px) in img.data.iter_mut().enumerate() {
        let u = (i % w) as f32 / (w - 1).max(1) as f32;
        let v = (i / w) as f32 / (h - 1).max(1) as f32;
        let t = (v * v * (3.0 - 2.0 * v)).clamp(0.0, 1.0); // smoothstep: a fade, not a step
        let sky = [0.10 + 0.70 * v, 0.14 + 0.72 * v, 0.30 + 0.62 * v];
        let field = [0.86 - 0.26 * u, 0.18 + 0.62 * u, 0.12 + 0.30 * (1.0 - u)];
        *px = std::array::from_fn(|k| (sky[k] * (1.0 - t) + field[k] * t).clamp(0.0, 1.0));
    }
    img
}

/// CIE L*a*b* (D65) of a scene-linear colour: the perceptual yardstick a proxy is held to. The
/// input is clamped into 0..1, the only range a proxy ever stores.
fn lab(c: [f32; 3]) -> [f64; 3] {
    let (r, g, b) = (c[0].clamp(0.0, 1.0) as f64, c[1].clamp(0.0, 1.0) as f64, c[2].clamp(0.0, 1.0) as f64);
    let (x, y, z) =
        ((0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047, 0.2126 * r + 0.7152 * g + 0.0722 * b, (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883);
    let f = |t: f64| if t > 216.0 / 24389.0 { t.cbrt() } else { (841.0 / 108.0) * t + 4.0 / 29.0 };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// The mean CIE76 ΔE between two same-sized scene-linear images: monotone in CIEDE2000 for a
/// low-contrast, near-neutral difference like a proxy's, and enough to rank two encoders.
fn mean_delta_e(a: &Rgb32f, b: &Rgb32f) -> f64 {
    let n = a.data.len().min(b.data.len());
    if n == 0 {
        return 0.0;
    }
    (0..n)
        .map(|i| {
            let (p, q) = (lab(a.data[i]), lab(b.data[i]));
            ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt()
        })
        .sum::<f64>()
        / n as f64
}

/// A deliberately frozen copy of the pixel mapping and container this module used before the
/// quality raise: the 99.95 % highlight scale, the sRGB 8-bit mapping, JPEG quality 92, and the
/// version-1 header (width, height, scale and camera-look fit — no encode version). A copy on
/// purpose: it must not follow `encode` as that changes, so the fidelity test measures what the
/// quality raise bought instead of trusting a written-down number.
fn encode_at_q92(img: &Rgb32f) -> Vec<u8> {
    let mut lum: Vec<f32> = img.data.iter().map(|c| c[0].max(c[1]).max(c[2])).filter(|v| v.is_finite()).collect();
    let scale = if lum.is_empty() {
        1.0
    } else {
        let k = ((lum.len() as f64) * 0.9995) as usize;
        let k = k.min(lum.len() - 1);
        let (_, v, _) = lum.select_nth_unstable_by(k, f32::total_cmp);
        v.max(1.0)
    };
    let data: Vec<[u8; 4]> = img
        .data
        .iter()
        .map(|c| {
            let e = |v: f32| (linear_to_srgb((v / scale).clamp(0.0, 1.0)) * 255.0).round() as u8;
            [e(c[0]), e(c[1]), e(c[2]), 255]
        })
        .collect();
    let rgba = Rgba8 { width: img.width, height: img.height, data };
    let jpg = lightcraft_codecs::encode_jpeg(
        &lightcraft_codecs::EncodeImage::rgba8(&rgba),
        92,
        lightcraft_codecs::ChromaSubsampling::S444,
        &Default::default(),
    )
    .unwrap();
    // the version-1 container the decoder needs: magic, header, JPEG — and no `enc` field
    let mut out = MAGIC.to_vec();
    let head = serde_json::json!({"w": img.width, "h": img.height, "scale": scale, LOOK_FIELD: crate::camera_preview::LOOK_VERSION});
    out.extend_from_slice(head.to_string().as_bytes());
    out.push(b'\n');
    out.extend_from_slice(&jpg);
    out
}

/// The mean CIE76 ΔE between two same-sized scene-linear images, each reduced first to one
/// sample per 8×8 block — the block's mean colour. JPEG stores a block's DC coefficient, its
/// mean, and rebuilds the whole block from it, so a proxy differs from the original by a
/// constant per block: the "8×8 block pattern" a preview shows. Averaging a block's 64 pixels
/// keeps that constant while shrinking the 8-bit sRGB rounding the two encoders share
/// eightfold, which is what a per-pixel ΔE drowns in.
fn mean_block_delta_e(a: &Rgb32f, b: &Rgb32f) -> f64 {
    let w = a.width.min(b.width).max(1);
    let h = a.height.min(b.height).max(1);
    let (mut sum, mut n) = (0.0f64, 0usize);
    for by in (0..h).step_by(8) {
        for bx in (0..w).step_by(8) {
            let (mut sa, mut sb, mut count) = ([0.0f64; 3], [0.0f64; 3], 0u32);
            for y in by..(by + 8).min(h) {
                for x in bx..(bx + 8).min(w) {
                    if let (Some(ca), Some(cb)) = (a.data.get(y * a.width + x), b.data.get(y * b.width + x)) {
                        for k in 0..3 {
                            sa[k] += f64::from(ca[k]);
                            sb[k] += f64::from(cb[k]);
                        }
                        count += 1;
                    }
                }
            }
            let d = f64::from(count.max(1));
            let (pa, pb) = (
                lab([(sa[0] / d) as f32, (sa[1] / d) as f32, (sa[2] / d) as f32]),
                lab([(sb[0] / d) as f32, (sb[1] / d) as f32, (sb[2] / d) as f32]),
            );
            sum += ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt();
            n += 1;
        }
    }
    sum / n.max(1) as f64
}

/// for [`a_smooth_proxy_keeps_the_scene_closer_than_the_old_quality`] to pass. The shared 8-bit
/// sRGB quantization floor both encoders share caps the achievable ratio at 0.632 on this scene,
/// a no-op encoder scores 1.000, and this is their geometric midpoint: it fails the moment the
/// quality raise is reverted and never trips on the honest result (measured 0.683).
const PROXY_BLOCK_GAIN: f64 = 0.8;

#[test]
fn a_smooth_proxy_keeps_the_scene_closer_than_the_old_quality() {
    let img = smooth_scene(256, 160);
    let ours = decode(&encode(&img, None).unwrap()).unwrap().0;
    let before = decode(&encode_at_q92(&img)).unwrap().0;
    let block = (mean_block_delta_e(&img, &ours), mean_block_delta_e(&img, &before));
    let pixel = (mean_delta_e(&img, &ours), mean_delta_e(&img, &before));
    println!(
        "smooth-scene proxy round-trip: block-mean ΔE76 {:.3} at q{JPEG_QUALITY} vs {:.3} at q92 \
             (per-pixel {:.3} vs {:.3})",
        block.0, block.1, pixel.0, pixel.1
    );
    assert!(block.1 > 0.0, "the old encoder must measure something to compare against");
    assert!(
        block.0 <= PROXY_BLOCK_GAIN * block.1,
        "q{JPEG_QUALITY} leaves a block-mean ΔE of {:.3}, not the ≤{:.3} the quality raise should buy over q92's {:.3} \
             (the 8-bit sRGB floor caps the achievable ratio at ~0.63 on this scene)",
        block.0,
        PROXY_BLOCK_GAIN * block.1,
        block.1
    );
}

#[test]
fn a_proxy_from_an_older_encode_is_stale() {
    let dir = temp("encode-version");
    let img = lightcraft_scenes::demo_library()[0].render(64, 40);
    let bytes = encode(&img, None).unwrap();
    let path = dir.join("p.lcsp");
    std::fs::write(&path, &bytes).unwrap();
    assert!(!is_stale(&path), "this build's own proxy is up to date");
    // one written before the encode version was recorded is not
    let nl = MAGIC.len() + bytes[MAGIC.len()..].iter().position(|b| *b == b'\n').unwrap();
    let head: serde_json::Value = serde_json::from_slice(&bytes[MAGIC.len()..nl]).unwrap();
    assert_eq!(head[ENC_FIELD].as_u64(), Some(ENCODE_VERSION));
    let mut older = head.as_object().cloned().unwrap_or_default();
    older.remove(ENC_FIELD);
    let mut old = MAGIC.to_vec();
    old.extend_from_slice(serde_json::Value::Object(older).to_string().as_bytes());
    old.extend_from_slice(&bytes[nl..]);
    std::fs::write(&path, &old).unwrap();
    assert!(is_valid(&path), "losing the field keeps the file readable");
    assert!(is_stale(&path), "an older encoding is rebuilt");
    // the marker records both versions, and one from before the encode version forces a scan
    assert!(!look_checked(&dir));
    mark_look_checked(&dir).unwrap();
    assert!(look_checked(&dir));
    std::fs::write(dir.join(LOOK_MARKER), "99\n").unwrap();
    assert!(!look_checked(&dir), "a look-only marker is not enough to skip the rebuild");
    let _ = std::fs::remove_dir_all(&dir);
}
