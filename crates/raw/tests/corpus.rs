//! Corpus test over `corpus/raw/**` (git-ignored CC0 samples from raw.pixls.us, fetched with
//! `cargo xtask corpus --download`; `LIGHTCRAFT_CORPUS` overrides the corpus root). Skips cleanly when absent.
//!
//! Every file must be recognised, carry an embedded JPEG preview (DNG: optional), and either decode to a valid image or report
//! `Unsupported` for one of the variants we know we don't decode yet. Prints decode times
//! (`cargo test -p lightcraft-raw --release --test corpus -- --nocapture`).

use lightcraft_raw::{RawError, RawFormat, decode, embedded_preview, probe, probe_info};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn corpus_root() -> PathBuf {
    std::env::var_os("LIGHTCRAFT_CORPUS").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
}

/// Variants known not to decode yet (see the crate docs): matched against the lower-case file name.
const KNOWN_UNSUPPORTED: &[&str] = &[
    "cr3-",                     // CR3 / CRX (M11.1)
    "raf-fuji-xt20-compressed", // Fujifilm compressed RAF
    "orf-olympus-em",           // Olympus compressed ORF
    "sraw",                     // Canon sRAW / mRAW
];

#[test]
fn corpus_raw_decodes() {
    let dir = corpus_root().join("raw");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        eprintln!("skip: {} absent", dir.display());
        return;
    };
    let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    paths.sort();
    let (mut ok, mut unsupported) = (0, 0);
    for p in paths {
        let name = p.file_name().unwrap().to_string_lossy().to_lowercase();
        if name.ends_with(".part") || name.ends_with(".txt") || name.ends_with(".md") {
            continue;
        }
        let bytes = std::fs::read(&p).unwrap();
        let fmt = probe(&bytes).unwrap_or_else(|| panic!("{name}: not recognised"));
        let t0 = Instant::now();
        let preview = embedded_preview(&bytes);
        let tp = t0.elapsed().as_secs_f64() * 1e3;
        // DNG previews are optional (and some carry only an uncompressed RGB thumbnail); vendor raws embed a JPEG,
        // except the Panasonic `.RAW` files of 2005–2007
        if let Some(p) = &preview {
            let jpeg = p.starts_with(&[0xff, 0xd8]) && p.ends_with(&[0xff, 0xd9]);
            let jxl = p.starts_with(&[0xff, 0x0a]) || p.starts_with(b"\0\0\0\x0cJXL ");
            assert!(jpeg || jxl, "{name}: preview is neither a JPEG nor a JPEG XL file");
        } else {
            assert!(fmt == RawFormat::Dng || name.starts_with("raw-panasonic-"), "{name}: no embedded preview");
        }
        let preview_kb = preview.as_ref().map_or(0, |p| p.len() / 1024);
        let t1 = Instant::now();
        let decoded = decode(&bytes);
        let dt = t1.elapsed().as_secs_f64() * 1e3;
        // the header-only probe agrees with the full decode, faster
        let t2 = Instant::now();
        let info = probe_info(&bytes);
        let di = t2.elapsed().as_secs_f64() * 1e3;
        match (&decoded, &info) {
            (Ok(img), Ok(info)) => assert_eq!(&img.info(), info, "{name}: probe_info differs from decode"),
            (Err(RawError::Unsupported(_)), Err(RawError::Unsupported(_))) => {}
            (d, i) => panic!("{name}: decode {:?} but probe_info {:?}", d.as_ref().err(), i.as_ref().err()),
        }
        eprintln!("{name:44} probe_info {di:.1} ms");
        match decoded {
            Ok(img) => {
                img.validate().unwrap();
                assert!(img.white_at(0) > img.black.mean(), "{name}: white {} <= black {}", img.white_at(0), img.black.mean());
                let mp = (img.width * img.height) as f64 / 1e6;
                eprintln!(
                    "{name:44} {fmt:?} {}x{} {}-bit {:?}: decode {dt:.0} ms ({:.0} MP/s), preview {} KB in {tp:.1} ms",
                    img.width,
                    img.height,
                    img.bits,
                    img.cfa.as_ref().map(|c| c.name()),
                    mp / (dt / 1e3),
                    preview_kb
                );
                ok += 1;
            }
            Err(RawError::Unsupported(why)) => {
                assert!(KNOWN_UNSUPPORTED.iter().any(|k| name.contains(k)), "{name}: unexpectedly unsupported: {why}");
                eprintln!("{name:44} {fmt:?} unsupported ({why}); preview {preview_kb} KB in {tp:.1} ms");
                unsupported += 1;
            }
            Err(e) => panic!("{name}: {e}"),
        }
    }
    eprintln!("corpus/raw: {ok} decoded, {unsupported} known-unsupported (preview only)");
}

/// Green-channel means of 32×32 blocks.
fn block_means(img: &lightcraft_raw::RawImage) -> Vec<f64> {
    let lightcraft_raw::RawData::U16(d) = &img.data else { panic!("float data") };
    let (w, h, b) = (img.width, img.height, 32);
    let mut out = Vec::new();
    for by in 0..h / b {
        for bx in 0..w / b {
            let (mut s, mut n) = (0f64, 0f64);
            for y in by * b..by * b + b {
                for x in (bx * b..bx * b + b).step_by(2) {
                    s += d[y * w + x + (y & 1)] as f64;
                    n += 1.0;
                }
            }
            out.push(s / n);
        }
    }
    out
}

fn correlation(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let cov: f64 = a.iter().zip(b).map(|(x, y)| (x - ma) * (y - mb)).sum();
    let va: f64 = a.iter().map(|x| (x - ma).powi(2)).sum();
    let vb: f64 = b.iter().map(|y| (y - mb).powi(2)).sum();
    cov / (va * vb).sqrt()
}

/// Which diagonal of each 2×2 cell (anchored at raw pixel (0, 0)) holds the green sites: the two greens of a Bayer
/// cell see nearly the same light, so their mean absolute difference is far smaller than across the other diagonal.
/// Returns `true` when green sits at (0, 0)/(1, 1) (GBRG/GRBG), `false` for (1, 0)/(0, 1) (RGGB/BGGR).
fn green_on_main_diagonal(img: &lightcraft_raw::RawImage) -> bool {
    let lightcraft_raw::RawData::U16(d) = &img.data else { panic!("float data") };
    let (w, a) = (img.width, img.active_area);
    let (mut main, mut anti) = (0f64, 0f64);
    for y in ((a.y + 2) & !1..a.y + a.height - 2).step_by(2) {
        for x in ((a.x + 2) & !1..a.x + a.width - 2).step_by(2) {
            let v = |dx: usize, dy: usize| d[(y + dy) * w + x + dx] as f64;
            main += (v(0, 0) - v(1, 1)).abs();
            anti += (v(1, 0) - v(0, 1)).abs();
        }
    }
    main < anti
}

/// Canon CR2 colour-filter layouts differ by model (issue #85); the decoder reads them from the `CR2CFAPattern` tag.
/// The expected layouts were checked visually (natural colours vs. the embedded preview) and agree with the
/// green-diagonal statistic of the mosaic itself.
#[test]
fn corpus_cr2_cfa_patterns() {
    let dir = corpus_root().join("raw");
    let cases = [
        ("cr2-canon-40d.cr2", "RGGB"),
        ("cr2-canon-550d.cr2", "GBRG"),
        ("cr2-canon-5d2.cr2", "GBRG"),
        ("cr2-canon-5d3.cr2", "RGGB"),
        ("cr2-canon-5dsr.cr2", "RGGB"),
        ("cr2-canon-6d.cr2", "RGGB"),
        ("cr2-canon-7d.cr2", "GBRG"),
        ("cr2-canon-80d.cr2", "RGGB"),
    ];
    let mut seen = 0;
    for (name, want) in cases {
        let Ok(bytes) = std::fs::read(dir.join(name)) else {
            eprintln!("skip: {name} absent");
            continue;
        };
        let img = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        let got = img.cfa.as_ref().map(|c| c.name()).unwrap_or_default();
        assert_eq!(got, want, "{name}: CFA layout");
        assert_eq!(green_on_main_diagonal(&img), want.starts_with('G'), "{name}: mosaic statistics disagree with {want}");
        seen += 1;
    }
    eprintln!("CR2 CFA layouts checked on {seen} files");
}

/// raw.pixls.us has the same D5100 scene as 14-bit lossless compressed and uncompressed NEF: the Huffman decode must
/// match the uncompressed image (up to the small differences between two exposures).
#[test]
fn corpus_nef_compressed_matches_uncompressed() {
    let dir = corpus_root().join("raw");
    let (Ok(a), Ok(b)) = (std::fs::read(dir.join("nef-nikon-d5100-lossless.nef")), std::fs::read(dir.join("nef-nikon-d5100-uncompressed.nef")))
    else {
        eprintln!("skip: D5100 NEF pair absent");
        return;
    };
    let (a, b) = (decode(&a).unwrap(), decode(&b).unwrap());
    assert_eq!((a.width, a.height, a.bits), (b.width, b.height, b.bits));
    let r = correlation(&block_means(&a), &block_means(&b));
    eprintln!("D5100 lossless vs uncompressed: block correlation {r:.4}");
    assert!(r > 0.98, "correlation {r}");
    // the lossy 12-bit D7000 file decodes into the curve's range and isn't flat
    if let Ok(c) = std::fs::read(dir.join("nef-nikon-d7000-lossy12.nef")) {
        let c = decode(&c).unwrap();
        let m = block_means(&c);
        let (lo, hi) = m.iter().fold((f64::MAX, 0f64), |(l, h), &v| (l.min(v), h.max(v)));
        assert!(hi <= 4095.0 && hi - lo > 500.0, "D7000 block means {lo}..{hi}");
    }
}

/// raw.pixls.us has the same D7500 scene as 12- and 14-bit lossless compressed NEF. Both store black level 400 in
/// maker note 0x003d (14-bit units): after subtracting the black level and scaling to white, the two must agree.
#[test]
fn corpus_nef_12_bit_black_level_matches_14_bit() {
    let dir = corpus_root().join("raw");
    let (Ok(a), Ok(b)) = (std::fs::read(dir.join("nef-nikon-d7500-lossless12.nef")), std::fs::read(dir.join("nef-nikon-d7500-lossless14.nef")))
    else {
        eprintln!("skip: D7500 NEF pair absent");
        return;
    };
    let (a, b) = (decode(&a).unwrap(), decode(&b).unwrap());
    assert_eq!((a.bits, b.bits), (12, 14));
    assert!((a.black.values[0] - 100.0).abs() < 1.0 && (b.black.values[0] - 400.0).abs() < 1.0, "{:?} {:?}", a.black, b.black);
    let normalized = |img: &lightcraft_raw::RawImage| {
        let (black, white) = (img.black.values[0] as f64, img.white[0] as f64);
        block_means(img).into_iter().map(|v| (v - black) / (white - black)).collect::<Vec<_>>()
    };
    // the two shots aren't pixel-aligned (handheld): compare the distributions, not block by block
    let (mut na, mut nb) = (normalized(&a), normalized(&b));
    na.sort_by(f64::total_cmp);
    nb.sort_by(f64::total_cmp);
    for q in [0.1, 0.5, 0.9] {
        let (x, y) = (na[(na.len() as f64 * q) as usize], nb[(nb.len() as f64 * q) as usize]);
        eprintln!("D7500 12- vs 14-bit: normalized quantile {q}: {x:.4} vs {y:.4}");
        // with the tag read as 12-bit units, the 12-bit values would sit below black (negative)
        assert!(x > 0.0 && (0.8..1.25).contains(&(x / y)), "quantile {q}: {x} vs {y}");
    }
}

/// Issue #138: DNGs converted by Adobe software carry their camera profile's hue/saturation map and
/// look table; we read them (and render with them). Apple ProRAW carries a tone curve and a gain
/// table map (its local tone mapping, read and kept but not rendered, as in Lightroom Classic).
/// Other camera-written DNGs here carry none.
#[test]
fn corpus_dngs_carry_profile_looks() {
    let dir = corpus_root().join("raw");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        eprintln!("skip: {} absent", dir.display());
        return;
    };
    let mut seen = 0;
    for p in rd.flatten().map(|e| e.path()) {
        let name = p.file_name().unwrap().to_string_lossy().to_lowercase();
        if !name.starts_with("dng-") || !name.ends_with(".dng") {
            continue;
        }
        let info = probe_info(&std::fs::read(&p).unwrap()).unwrap_or_else(|e| panic!("{name}: {e}"));
        let look = &info.color.profile;
        if name.starts_with("dng-adobe-") {
            seen += 1;
            let hsm = look.hue_sat_map[0].as_ref().unwrap_or_else(|| panic!("{name}: no hue/sat map"));
            assert!(hsm.hue_divisions > 1 && hsm.sat_divisions > 1, "{name}");
            assert!(look.look_table.is_some(), "{name}: no look table");
            // a profile applied to a mid grey keeps it (close to) neutral
            let t = lightcraft_raw::profile::ProfileTables::new(look, 0.5).unwrap();
            let g = t.apply([0.18; 3], 1.0);
            assert!(g.iter().all(|v| (v - g[0]).abs() < 0.01 * g[0].max(0.01)), "{name}: grey → {g:?}");
        }
        if name.starts_with("dng-apple-") {
            seen += 1;
            let map = look.gain_table_map.as_ref().unwrap_or_else(|| panic!("{name}: no gain table map"));
            assert!(map.points_v > 1 && map.points_h > 1 && map.points_n > 1, "{name}");
            assert!(look.tone_curve.is_some(), "{name}: no tone curve");
            // the map lifts dark tones (Apple's local tone mapping), most at the darkest input
            let first = &map.gains[..map.points_n];
            assert!(first[0] > 1.5 && first[0] >= first[map.points_n - 1], "{name}: first table {first:?}");
        }
        eprintln!(
            "{name:44} profile look: hsm {} look {} tone {} gain map {}",
            look.hue_sat_map[0].is_some(),
            look.look_table.is_some(),
            look.tone_curve.is_some(),
            look.gain_table_map.is_some()
        );
    }
    eprintln!("{seen} DNGs with profile looks checked");
}

/// iPhone ProRAW (`corpus/apple/IMG_1361.DNG`, iPhone 12 Pro, CC0 from raw.pixls.us) carries a
/// lossy-JPEG sky matte at half resolution as a DNG semantic mask: it decodes, and it is a sky. The
/// matte is stored like the raw (landscape; the photo is shown rotated 90° clockwise), so the sky
/// is on its left and the lake and shore on its right.
#[test]
fn corpus_proraw_sky_matte() {
    let path = corpus_root().join("apple/IMG_1361.DNG");
    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!("skip: {} absent", path.display());
        return;
    };
    let t0 = Instant::now();
    let masks = lightcraft_raw::semantic_masks(&bytes);
    eprintln!("semantic masks read in {:.1} ms", t0.elapsed().as_secs_f64() * 1e3);
    let names: Vec<&str> = masks.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["urn:com:apple:photo:2020:aux:semanticskymatte"]);
    let m = &masks[0];
    assert_eq!((m.width, m.height, m.sub_area), (2016, 1512, None));
    let cols = |x0: usize, x1: usize| {
        let sum: f64 = (0..m.height).flat_map(|y| m.data[y * m.width + x0..y * m.width + x1].iter()).map(|&v| v as f64 / 65535.0).sum();
        sum / ((x1 - x0) * m.height) as f64
    };
    let (left, right) = (cols(0, 100), cols(1916, 2016));
    eprintln!("sky matte: left columns {left:.3}, right columns {right:.3}");
    assert!(left > 0.5 && right < 0.02, "left {left}, right {right}");
}

/// Issue #148: Sony ARWs from before ~2017 carry no plain white-balance, black-level or crop tags in the raw IFD.
/// White balance comes from the maker note's enciphered `Tag2010`, the black level from the encrypted `SR2SubIFD`
/// and the crop from `FullImageSize`; without them the RX100 III opened bright green. Expected values: the black
/// levels agree with each sensor's dark-pixel floor, the gains with the neutral sky of the camera JPEG.
#[test]
fn corpus_sony_pre2017_colour_metadata() {
    let dir = corpus_root().join("raw");
    // (file, black, approximate R and B gains, crop width × height)
    let cases = [
        ("arw-sony-rx100m3.arw", 800.0, [2.61, 1.72], (5472, 3648)),
        ("arw-sony-rx100.arw", 800.0, [2.23, 2.00], (5472, 3648)),
        ("arw-sony-a7rm2-12bit-uncompressed.arw", 512.0, [2.58, 1.46], (7952, 5304)),
    ];
    let mut seen = 0;
    for (name, black, [r, b], (cw, ch)) in cases {
        let Ok(bytes) = std::fs::read(dir.join(name)) else {
            eprintln!("skip: {name} absent");
            continue;
        };
        let img = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(img.black.mean(), black, "{name}: black level");
        let wb = img.wb_multipliers.unwrap_or_else(|| panic!("{name}: no as-shot white balance"));
        assert!((wb[0] - r).abs() < 0.01 && wb[1] == 1.0 && (wb[2] - b).abs() < 0.01, "{name}: white balance {wb:?}");
        assert_eq!((img.crop.width, img.crop.height), (cw, ch), "{name}: crop");
        assert!(img.white_at(0) > 16000.0, "{name}: white {} (14-bit scale)", img.white_at(0));
        seen += 1;
    }
    eprintln!("pre-2017 Sony ARW colour metadata checked on {seen} files");
}

/// The embedded JPEG as linear RGB, reduced to `gw × gh` cells.
fn jpeg_cells(jpeg: &[u8], gw: usize, gh: usize) -> Vec<[f64; 3]> {
    use zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(jpeg), opts);
    let px = d.decode().unwrap();
    let info = d.info().unwrap();
    let (w, h) = (info.width as usize, info.height as usize);
    let (mut sum, mut n) = (vec![[0f64; 3]; gw * gh], vec![0f64; gw * gh]);
    for y in 0..h {
        for x in 0..w {
            let i = (y * gh / h) * gw + x * gw / w;
            for c in 0..3 {
                let v = px[(y * w + x) * 3 + c] as f64 / 255.0;
                sum[i][c] += if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
            }
            n[i] += 1.0;
        }
    }
    sum.iter().zip(&n).map(|(s, n)| s.map(|v| v / n)).collect()
}

/// Black-subtracted means of the four sites of the 2×2 cells (anchored at sample (0, 0)) of the active area,
/// reduced to `gw × gh` cells.
fn raw_sites(img: &lightcraft_raw::RawImage, gw: usize, gh: usize) -> Vec<[f64; 4]> {
    let lightcraft_raw::RawData::U16(d) = &img.data else { panic!("float data") };
    let (w, a, black) = (img.width, img.active_area, img.black.mean() as f64);
    let (x0, y0) = ((a.x + 1) & !1, (a.y + 1) & !1);
    let (cw, ch) = ((a.x + a.width - x0) / 2, (a.y + a.height - y0) / 2);
    let (mut sum, mut n) = (vec![[0f64; 4]; gw * gh], vec![0f64; gw * gh]);
    for cy in 0..ch {
        for cx in 0..cw {
            let i = (cy * gh / ch) * gw + cx * gw / cw;
            for site in 0..4 {
                sum[i][site] += d[(y0 + 2 * cy + site / 2) * w + x0 + 2 * cx + site % 2] as f64 - black;
            }
            n[i] += 1.0;
        }
    }
    sum.iter().zip(&n).map(|(s, n)| s.map(|v| v / n)).collect()
}

fn ranks(v: &[f64]) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..v.len()).collect();
    idx.sort_by(|&a, &b| v[a].total_cmp(&v[b]));
    let mut r = vec![0f64; v.len()];
    for (rank, &i) in idx.iter().enumerate() {
        r[i] = rank as f64;
    }
    r
}

/// How well the raw's chromaticities (log r/g, log b/g per cell) follow the JPEG's when the mosaic is read with
/// `layout` (colour of each 2×2 site at sample (0, 0)).
fn chroma_agreement(sites: &[[f64; 4]], jpeg: &[[f64; 3]], layout: &[u8]) -> f64 {
    let (mut x, mut y) = ([vec![], vec![]], [vec![], vec![]]);
    for (s, j) in sites.iter().zip(jpeg) {
        let (mut rgb, mut n) = ([0f64; 3], [0f64; 3]);
        for (site, &c) in layout.iter().enumerate() {
            rgb[c as usize] += s[site];
            n[c as usize] += 1.0;
        }
        let rgb = [rgb[0] / n[0], rgb[1] / n[1], rgb[2] / n[2]];
        if rgb.iter().all(|v| *v > 1.0) && j.iter().all(|v| (0.002..0.9).contains(v)) {
            for (k, c) in [0, 2].into_iter().enumerate() {
                x[k].push((rgb[c] / rgb[1]).ln());
                y[k].push((j[c] / j[1]).ln());
            }
        }
    }
    (correlation(&x[0], &y[0]) + correlation(&x[1], &y[1])) / 2.0
}

/// Panasonic RW2 / RWL / RAW (`crates/raw/src/vendor/rw2.rs`): one file per raw encoding (formats 2, 4, 5, 6, 7 and
/// 8 — at 12, 14 and 16 bits — and the 16-bit words of the oldest bodies) decodes to the image of the camera's own
/// JPEG, with the black levels, crops and colour-filter layouts the module docs derive and no defect markers left.
#[test]
fn corpus_panasonic_encodings() {
    let dir = corpus_root().join("raw");
    // (file, bits, mean black level, crop width × height, CFA layout)
    let cases = [
        ("raw-panasonic-fz50.raw", 12, 15.0, (3648, 2736), "BGGR"),
        ("raw-panasonic-fz8.raw", 12, 15.0, (3072, 2304), "RGGB"),
        ("rw2-panasonic-fz1000m2-4x3.rw2", 12, 143.0, (4864, 3648), "GBRG"),
        ("rw2-panasonic-g9-b.rw2", 12, 143.0, (5184, 3888), "RGGB"),
        ("rw2-panasonic-g9.rw2", 12, 127.5, (5184, 3888), "RGGB"),
        ("rw2-panasonic-gh1.rw2", 12, 15.0, (4000, 3000), "GBRG"),
        ("rw2-panasonic-gh5.rw2", 12, 143.5, (5184, 3888), "RGGB"),
        ("rw2-panasonic-gh5m2.rw2", 12, 144.5, (5184, 3888), "RGGB"),
        ("rw2-panasonic-gh5s.rw2", 14, 509.5, (3680, 2760), "RGGB"),
        ("rw2-panasonic-gh6.rw2", 16, 2048.0, (5776, 4336), "RGGB"),
        ("rw2-panasonic-gx80.rw2", 12, 143.0, (4592, 3448), "BGGR"),
        ("rw2-panasonic-s1.rw2", 14, 526.0, (6000, 4000), "RGGB"),
        ("rw2-panasonic-s5-format7.rw2", 14, 512.0, (6000, 4000), "RGGB"),
        ("rw2-panasonic-s5m2.rw2", 14, 512.0, (6000, 4000), "RGGB"),
        ("rw2-panasonic-s9.rw2", 12, 128.0, (6000, 4000), "RGGB"),
        ("rwl-leica-dlux7.rwl", 12, 143.0, (4736, 3552), "BGGR"),
    ];
    let mut seen = 0;
    for (name, bits, black, (cw, ch), cfa) in cases {
        let Ok(bytes) = std::fs::read(dir.join(name)) else {
            eprintln!("skip: {name} absent");
            continue;
        };
        let img = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(img.bits, bits, "{name}: bits");
        assert_eq!(img.black.mean(), black, "{name}: black level");
        assert_eq!((img.crop.width, img.crop.height), (cw, ch), "{name}: crop");
        let layout = img.cfa.clone().unwrap_or_else(|| panic!("{name}: no CFA"));
        assert_eq!(layout.name(), cfa, "{name}: CFA layout");
        let lightcraft_raw::RawData::U16(d) = &img.data else { panic!("float data") };
        let a = img.active_area;
        let zeros =
            (a.y..a.y + a.height).map(|y| d[y * img.width + a.x..y * img.width + a.x + a.width].iter().filter(|&&v| v == 0).count()).sum::<usize>();
        assert_eq!(zeros, 0, "{name}: defect markers left in the active area");
        let Some(jpeg) = embedded_preview(&bytes) else {
            // the 2005–2007 `.RAW` files have no preview; their flat scenes make the green diagonal unambiguous
            assert_eq!(green_on_main_diagonal(&img), cfa.starts_with('G'), "{name}: mosaic statistics disagree with {cfa}");
            seen += 1;
            continue;
        };
        // the camera's JPEG shows the whole active area (whatever the crop): the raw's green must rank like it, and
        // its colours must follow the JPEG's best with the decoded colour-filter layout
        let (gw, gh) = (24, 18);
        let (sites, jpeg) = (raw_sites(&img, gw, gh), jpeg_cells(&jpeg, gw, gh));
        let green: Vec<f64> = sites.iter().map(|s| (0..4).filter(|&i| layout.pattern[i] == 1).map(|i| s[i]).sum::<f64>()).collect();
        let jg: Vec<f64> = jpeg.iter().map(|j| j[1]).collect();
        let rho = correlation(&ranks(&green), &ranks(&jg));
        let agree = |l: &str| chroma_agreement(&sites, &jpeg, &lightcraft_raw::Cfa::bayer(l).unwrap().pattern);
        let best_other = ["RGGB", "GRBG", "GBRG", "BGGR"].into_iter().filter(|l| *l != cfa).map(agree).fold(f64::MIN, f64::max);
        let own = agree(cfa);
        eprintln!(
            "{name:32} {}x{} {bits}-bit: green rank correlation {rho:.3}, colour agreement {own:.3} (others ≤ {best_other:.3})",
            img.width, img.height
        );
        assert!(rho > 0.9, "{name}: rank correlation with the camera JPEG {rho}");
        assert!(own > 0.5 && own > best_other, "{name}: colours follow the JPEG better with another layout ({own} vs {best_other})");
        seen += 1;
    }
    eprintln!("Panasonic encodings checked on {seen} files");
}
