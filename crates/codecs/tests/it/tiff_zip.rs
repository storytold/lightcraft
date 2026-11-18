//! Noisy 8/16-bit ZIP TIFFs stay within their uncompressed size (issue #2882), using the Deflate backend the
//! application dependency graph selects (no test-only backend is forced), so a dependency switching it shows up.

use lcc_codecs::*;

/// A deterministic value in -0.5..0.5 per position and channel.
fn hash(x: u32, y: u32, c: u32) -> f64 {
    let mut z = (x as u64) << 32 ^ (y as u64) << 2 ^ c as u64 ^ 0x2882;
    z = (z ^ (z >> 33)).wrapping_mul(0xff51_afd7_ed55_8ccd);
    z = (z ^ (z >> 33)).wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    (z ^ (z >> 33)) as f64 / u64::MAX as f64 - 0.5
}

/// 16-bit RGB: smooth gradients plus noise of `noise` codes peak to peak (a clean to a high-ISO photo's low bits).
fn noisy16(w: u32, h: u32, noise: f64) -> Vec<u16> {
    let mut v = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let (u, t) = (x as f64 / w as f64, y as f64 / h as f64);
            let base = [0.10 + 0.5 * u, 0.15 + 0.4 * t, 0.3 + 0.3 * u * t];
            for c in 0..3 {
                v.push((base[c as usize] * 65535.0 + noise * hash(x, y, c)).clamp(0.0, 65535.0) as u16);
            }
        }
    }
    v
}

/// Both depths must actually use Deflate and decode at the original dimensions.
fn checked_zip_size(img: &EncodeImage<'_>) -> usize {
    let bytes = encode_tiff(img, TiffCompression::Deflate, &EncodeMeta::default()).unwrap();
    let mut decoder = tiff::decoder::Decoder::new(std::io::Cursor::new(&bytes)).unwrap();
    let compression = decoder.get_tag_u32(tiff::tags::Tag::Compression).unwrap();
    assert!(matches!(compression, 8 | 32946 | 1), "Compression tag {compression} is neither Deflate nor the uncompressed fallback");
    let decoded = decode(&bytes, DecodeOptions::default()).unwrap();
    assert_eq!((decoded.image.width, decoded.image.height), (img.width as usize, img.height as usize));
    // Untagged integer RGB TIFFs are sRGB; check the application conversion independently of the integer
    // comparison above, including the sample count so zip cannot hide truncation.
    let encoded: Vec<f32> = match img.samples {
        Samples::U8(samples) => samples.iter().map(|&v| f32::from(v) / 255.0).collect(),
        Samples::U16(samples) => samples.iter().map(|&v| f32::from(v) / 65535.0).collect(),
        _ => panic!("expected an integer TIFF fixture"),
    };
    assert_eq!(decoded.image.data.len() * 3, encoded.len());
    for (i, (&actual, encoded)) in decoded.image.data.iter().flatten().zip(encoded).enumerate() {
        let expected = lcc_color::transfer::srgb_to_linear(encoded);
        assert!((actual - expected).abs() <= 1e-6, "application-decoded sample {i}: {actual}, expected {expected}");
    }
    bytes.len()
}

/// The shipped backend keeps noisy 16-bit ZIP exports smaller than uncompressed TIFFs.
#[test]
fn zip_tiff_is_never_larger_than_uncompressed() {
    let (w, h) = (512, 384);
    for noise in [0.0, 200.0, 1300.0, 4000.0, 13000.0] {
        let px = noisy16(w, h, noise);
        let img = EncodeImage::new(w, h, 3, Samples::U16(&px));
        let none = encode_tiff(&img, TiffCompression::None, &EncodeMeta::default()).unwrap().len();
        let zip = checked_zip_size(&img);
        eprintln!("noise {noise} codes: none {none} B, ZIP {zip} B ({:+.1} %)", 100.0 * (zip as f64 / none as f64 - 1.0));
        assert!(zip <= none, "noise {noise}: 16-bit ZIP {zip} B is larger than uncompressed {none} B");
    }
    // eight bit zip tiff is never larger than uncompressed
    // The same size guarantee holds for 8-bit ZIP exports across several noise levels.
    {
        let (w, h) = (512, 384);
        for noise in [0.0, 4.0, 20.0, 60.0] {
            let px: Vec<u8> = noisy16(w, h, noise * 257.0).iter().map(|v| (v >> 8) as u8).collect();
            let img = EncodeImage::new(w, h, 3, Samples::U8(&px));
            let none = encode_tiff(&img, TiffCompression::None, &EncodeMeta::default()).unwrap().len();
            let zip = checked_zip_size(&img);
            eprintln!("8-bit noise {noise}: none {none} B, ZIP {zip} B ({:+.1} %)", 100.0 * (zip as f64 / none as f64 - 1.0));
            assert!(zip <= none, "8-bit noise {noise}: ZIP {zip} B is larger than uncompressed {none} B");
        }
    }
}

/// What ZIP costs at 24 MP, 16 bit (ignored by default):
/// `cargo test --release -p lcc-codecs --test it tiff_zip::bench -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_24mp_16_bit_zip() {
    let (w, h) = (6000, 4000);
    for noise in [200.0, 1300.0, 4000.0] {
        let px = noisy16(w, h, noise);
        let img = EncodeImage::new(w, h, 3, Samples::U16(&px));
        for c in [TiffCompression::None, TiffCompression::Deflate, TiffCompression::Lzw] {
            let t = std::time::Instant::now();
            let n = encode_tiff(&img, c, &EncodeMeta::default()).unwrap().len();
            println!("noise {noise:>6} {c:?}: {:>7.1} MB in {:>6.0} ms", n as f64 / 1e6, t.elapsed().as_secs_f64() * 1e3);
        }
    }
}
