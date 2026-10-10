//! Corpus test over `corpus/raw/**` (git-ignored CC0 samples from raw.pixls.us, fetched with
//! `cargo xtask corpus --download`; `LIGHTCRAFT_CORPUS` overrides the corpus root). Skips cleanly when absent.
//!
//! Every file must be recognised, carry an embedded JPEG preview (optional for DNG, older Panasonic RAW and HEVC-preview CR3), and either decode to a valid image or report
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
    "cr3-",           // Unverified CRX coding variants; exact supported cases live in cr3_corpus.rs.
    "orf-olympus-em", // Olympus compressed ORF
    "-he.nef",        // Nikon High Efficiency (HE / HE★) NEF, issue #193
    "sraw",           // Canon sRAW / mRAW
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
        // except the Panasonic `.RAW` files of 2005–2007 and Canon's explicit HEVC preview tracks.
        if let Some(p) = &preview {
            let jpeg = p.starts_with(&[0xff, 0xd8]) && p.ends_with(&[0xff, 0xd9]);
            let jxl = p.starts_with(&[0xff, 0x0a]) || p.starts_with(b"\0\0\0\x0cJXL ");
            assert!(jpeg || jxl, "{name}: preview is neither a JPEG nor a JPEG XL file");
        } else {
            let hevc_preview = fmt == RawFormat::Cr3
                && lightcraft_meta::cr3::parse_cr3(&bytes)
                    .is_some_and(|c| c.tracks.iter().any(|t| t.kind == lightcraft_meta::cr3::Cr3TrackKind::Other(*b"HEVC") && t.data.is_some()));
            assert!(fmt == RawFormat::Dng || name.starts_with("raw-panasonic-") || hevc_preview, "{name}: no embedded preview");
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
    eprintln!("corpus/raw: {ok} decoded, {unsupported} known-unsupported (embedded JPEG when available)");
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

/// The as-shot white balance of Canon CR2s whose ColorData is stored as SHORT words stays readable (the PowerShot /
/// EOS M files that store it as UNDEFINED bytes are covered by the unit test in `vendor/cr2.rs`; none is in this corpus).
/// Daylight-ish CR2s have R/G above 1 and B/G between 1 and 2.5 (observed 1.4-2.7 on these nine bodies).
#[test]
fn corpus_cr2_as_shot_white_balance() {
    let dir = corpus_root().join("raw");
    let mut seen = 0;
    for name in [
        "cr2-canon-40d.cr2",
        "cr2-canon-550d.cr2",
        "cr2-canon-5d2.cr2",
        "cr2-canon-5d3.cr2",
        "cr2-canon-5dsr.cr2",
        "cr2-canon-6d.cr2",
        "cr2-canon-7d.cr2",
        "cr2-canon-80d.cr2",
    ] {
        let Ok(bytes) = std::fs::read(dir.join(name)) else {
            eprintln!("skip: {name} absent");
            continue;
        };
        let wb = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}")).wb_multipliers.unwrap_or_else(|| panic!("{name}: no as-shot WB"));
        assert!(wb[1] == 1.0 && wb[0] > 1.0 && wb[2] > 0.5 && wb[0] < 4.0 && wb[2] < 4.0, "{name}: WB {wb:?}");
        seen += 1;
    }
    eprintln!("CR2 as-shot WB checked on {seen} files");
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
/// Issue #193: the Z f's High Efficiency NEF (no `0x0096` table, a JPEG XS codestream) is named as such and keeps a
/// full-size embedded JPEG to edit, while the same body's Lossless compressed NEF decodes.
#[test]
fn corpus_nef_high_efficiency_is_named() {
    let dir = corpus_root().join("raw");
    let (Ok(he), Ok(lossless)) = (std::fs::read(dir.join("nef-nikon-zf-he.nef")), std::fs::read(dir.join("nef-nikon-zf-lossless.nef"))) else {
        eprintln!("skip: Z f samples absent");
        return;
    };
    for result in [decode(&he).map(|_| ()), probe_info(&he).map(|_| ())] {
        let Err(RawError::Unsupported(why)) = result else { panic!("Z f HE: expected unsupported") };
        assert!(why.starts_with("Nikon High Efficiency NEF ("), "{why}");
    }
    assert!(embedded_preview(&he).is_some_and(|p| p.len() > 1 << 20), "Z f HE: no full-size preview");
    let img = decode(&lossless).unwrap();
    assert_eq!((img.width, img.height, img.bits), (6064, 4040, 14));
}

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

/// iPhone ProRAW (`dng-apple-iphone12pro-proraw.dng`, iPhone 12 Pro, CC0 from raw.pixls.us, see
/// `cargo xtask corpus`) carries a lossy-JPEG sky matte at half resolution as a DNG semantic mask:
/// it decodes, and it is a sky. The matte is stored like the raw (landscape; the photo is shown
/// rotated 90° clockwise), so the sky is on its left and the lake and shore on its right.
#[test]
fn corpus_proraw_sky_matte() {
    let path = corpus_root().join("raw/dng-apple-iphone12pro-proraw.dng");
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
/// White balance and black level come from the encrypted `SR2SubIFD`, the crop from `FullImageSize`; without them
/// the RX100 III opened bright green. Expected values: the black levels agree with each sensor's dark-pixel floor;
/// the gains are the `SR2SubIFD`'s `WB_RGGBLevels`, within 4 % of the gains that make the camera JPEG's neutral
/// pixels neutral (RX100 III 2.59 / 1.69, RX100 2.21 / 2.01, ILCE-7RM2 2.67 / 1.47). Until issue #535 they came
/// from the maker note's `Tag2010` (2.61 / 1.72, 2.23 / 2.00, 2.58 / 1.46), which differs from what the camera
/// applied on other files (see `corpus_sony_sr2_white_balance`).
#[test]
fn corpus_sony_pre2017_colour_metadata() {
    let dir = corpus_root().join("raw");
    // (file, black, approximate R and B gains, crop width × height)
    let cases = [
        ("arw-sony-rx100m3.arw", 800.0, [2.55, 1.72], (5472, 3648)),
        ("arw-sony-rx100.arw", 800.0, [2.21, 1.99], (5472, 3648)),
        ("arw-sony-a7rm2-12bit-uncompressed.arw", 512.0, [2.58, 1.45], (7952, 5304)),
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

/// Sony ILCE-7CR (61 MP), three of the camera's raw codings of one scene on raw.pixls.us: lossless compressed L
/// (LJ92 tiles of 2×2 cells), lossless compressed M (subsampled YCbCr, already white-balanced) and compressed
/// (ARW2). Expected values: the files' own black level and default crop, and the as-shot gains of the camera's
/// colour-temperature setting.
#[test]
fn corpus_sony_a7cr_codings() {
    let dir = corpus_root().join("raw");
    // (file, colour-filter layout, as-shot R and B gains, default crop x, y, width, height)
    let cases = [
        ("arw-sony-a7cr-lossless-l.arw", Some("RGGB"), [2.625, 1.598], (32, 20, 9504, 6336)),
        ("arw-sony-a7cr-compressed.arw", Some("RGGB"), [2.625, 1.598], (32, 20, 9504, 6336)),
        ("arw-sony-a7cr-lossless-m.arw", None, [1.0, 1.0], (20, 12, 6240, 4160)),
    ];
    let mut decoded = Vec::new();
    for (name, cfa, [r, b], crop) in cases {
        let bytes = match std::fs::read(dir.join(name)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("skip: {name} absent");
                continue;
            }
            Err(e) => panic!("{name}: {e}"),
        };
        let img = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(img.metadata.model.as_deref(), Some("ILCE-7CR"), "{name}: model");
        assert_eq!(img.cfa.as_ref().map(|c| c.name()).as_deref(), cfa, "{name}: colour-filter layout");
        // Bayer codings use the file's black level; the YCbCr M coding's pedestal is handled separately (#535, item 3).
        if cfa.is_some() {
            assert_eq!(img.black.mean(), 512.0, "{name}: black level");
        }
        assert!(img.white_at(0) > 15000.0, "{name}: white {} (14-bit scale)", img.white_at(0));
        let wb = img.wb_multipliers.unwrap_or_else(|| panic!("{name}: no as-shot white balance"));
        assert!((wb[0] - r).abs() < 0.01 && wb[1] == 1.0 && (wb[2] - b).abs() < 0.01, "{name}: white balance {wb:?}");
        assert_eq!((img.crop.x, img.crop.y, img.crop.width, img.crop.height), crop, "{name}: default crop");
        decoded.push((name, img));
    }
    // The lossless and compressed codings are two exposures of the same scene with the same framing: inside the
    // default crop their mosaics must agree (up to the small differences between two exposures).
    let crop_means = |img: &lightcraft_raw::RawImage| {
        let lightcraft_raw::RawData::U16(d) = &img.data else { panic!("float data") };
        let (w, c, b) = (img.width, img.crop, 32);
        let mut out = Vec::new();
        for by in 0..c.height / b {
            for bx in 0..c.width / b {
                let (x0, y0) = ((c.x + bx * b) & !1, (c.y + by * b) & !1);
                let (mut s, mut n) = (0f64, 0f64);
                for y in y0..y0 + b {
                    for x in (x0..x0 + b).step_by(2) {
                        s += d[y * w + x + 1 - (y & 1)] as f64;
                        n += 1.0;
                    }
                }
                out.push(s / n);
            }
        }
        out
    };
    let bayer: Vec<_> = decoded.iter().filter(|(_, img)| img.cfa.is_some()).collect();
    if let [(a_name, a), (b_name, b)] = bayer.as_slice() {
        let r = correlation(&crop_means(a), &crop_means(b));
        eprintln!("ILCE-7CR {a_name} vs {b_name}: green block correlation {r:.4}");
        assert!(r > 0.98, "correlation {r}");
    }
    eprintln!("ILCE-7CR codings checked on {} files", decoded.len());
}

/// Sony ILCE-7CM2 (33 MP, issue #697), two of the camera's codings on raw.pixls.us: lossless compressed L (LJ92 tiles
/// of 2×2 cells) and compressed "M-size" (ARW2). Only its lossless M and S codings are the downsized linear YCbCr
/// whose 1024 pedestal 0.5.0 missed (rendering the issue's file washed out: the darkest half of a dusk scene
/// +11 L* and blue); the compressed and uncompressed M-size files are Bayer at 4736×3132 with the recorded black
/// 512, so a size-based rule would break them. Expected values: the files' own colour-filter layout, black and
/// white levels, default crop and the as-shot gains of the camera's colour-temperature setting.
#[test]
fn corpus_sony_a7cm2_codings() {
    let dir = corpus_root().join("raw");
    // (file, sensor width and height, as-shot R and B gains, default crop x, y, width, height)
    let cases = [
        ("arw-sony-a7cm2-lossless-l.arw", (7168, 5120), [2.313, 1.654], (12, 8, 7008, 4672)),
        ("arw-sony-a7cm2-compressed-m.arw", (4736, 3132), [2.309, 1.657], (44, 30, 4608, 3072)),
    ];
    let mut checked = 0;
    for (name, (width, height), [r, b], crop) in cases {
        let bytes = match std::fs::read(dir.join(name)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("skip: {name} absent");
                continue;
            }
            Err(e) => panic!("{name}: {e}"),
        };
        let img = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(probe_info(&bytes).unwrap(), img.info(), "{name}: header and full decode agree");
        assert_eq!(img.metadata.model.as_deref(), Some("ILCE-7CM2"), "{name}: model");
        assert_eq!((img.width, img.height, img.cpp), (width, height, 1), "{name}: sensor size");
        assert_eq!(img.cfa.as_ref().map(|c| c.name()).as_deref(), Some("RGGB"), "{name}: colour-filter layout");
        assert_eq!((img.black.mean(), img.white_at(0)), (512.0, 16383.0), "{name}: black and white levels");
        let wb = img.wb_multipliers.unwrap_or_else(|| panic!("{name}: no as-shot white balance"));
        assert!((wb[0] - r).abs() < 0.01 && wb[1] == 1.0 && (wb[2] - b).abs() < 0.01, "{name}: white balance {wb:?}");
        assert_eq!((img.crop.x, img.crop.y, img.crop.width, img.crop.height), crop, "{name}: default crop");
        checked += 1;
    }
    eprintln!("ILCE-7CM2 codings checked on {checked} files");
}

/// Issue #535: Sony black levels against the data. The SR2SubIFD keeps the black level at a position that depends
/// on its layout (the DSLR-A500 and A700 read 365 and 975 from a fixed position, where ExifTool and the data say
/// 512), and the downsized lossless M and S codings (linear YCbCr) sit 512 above the recorded level (1024, with
/// white moved by the same 512). Each file's darkest 0.1 % per channel must lie just above its black level: a black
/// level that is too low leaves a pedestal, one that is too high clips the shadows.
#[test]
fn corpus_sony_black_levels_sit_at_the_data_floor() {
    use lightcraft_raw::RawData;
    let dir = corpus_root().join("raw");
    // (file, black, white): SR2SubIFD black at three layouts, a plain 0x7310, and the lossless L / M / S codings
    let cases = [
        ("arw-sony-a500.arw", 512.0, None),
        ("arw-sony-a700.arw", 512.0, None),
        ("arw-sony-rx100m3.arw", 800.0, None),
        ("arw-sony-a7m4-lossless-l.arw", 512.0, Some(16383.0)),
        ("arw-sony-a7m4-lossless-m.arw", 1024.0, Some(16895.0)),
        ("arw-sony-a7m4-lossless-s.arw", 1024.0, Some(16895.0)),
        // ILCE-7CM2 (#697): its compressed "M-size" coding is Bayer at the recorded 512, unlike its lossless M
        ("arw-sony-a7cm2-lossless-l.arw", 512.0, Some(16383.0)),
        ("arw-sony-a7cm2-compressed-m.arw", 512.0, Some(16383.0)),
    ];
    let mut seen = 0;
    for (name, black, white) in cases {
        let bytes = match std::fs::read(dir.join(name)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("skip: {name} absent");
                continue;
            }
            Err(e) => panic!("{name}: {e}"),
        };
        let img = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(img.black.mean(), black, "{name}: black level");
        if let Some(white) = white {
            assert_eq!(img.white_at(0), white, "{name}: white level");
        }
        let RawData::U16(ref data) = img.data else { panic!("{name}: integer samples expected") };
        // per CFA position (2×2) or per colour sample, inside the default crop, about a million samples each
        let channels = if img.cpp == 3 { 3 } else { 4 };
        let (a, c) = (img.active_area, img.crop);
        let step = (c.width * c.height / 1_000_000).max(1);
        let mut samples = vec![Vec::new(); channels];
        for (k, (x, y)) in (c.y..c.y + c.height).flat_map(|y| (c.x..c.x + c.width).map(move |x| (x + a.x, y + a.y))).enumerate() {
            if (k / 2) % step != 0 {
                continue;
            }
            if img.cpp == 3 {
                for (s, v) in samples.iter_mut().enumerate() {
                    v.push(data[(y * img.width + x) * 3 + s]);
                }
            } else {
                samples[(y % 2) * 2 + x % 2].push(data[y * img.width + x]);
            }
        }
        for (i, mut v) in samples.into_iter().enumerate() {
            v.sort_unstable();
            let floor = f32::from(v[v.len() / 1000]);
            assert!((black - 64.0..=black + 128.0).contains(&floor), "{name}: channel {i}: darkest 0.1 % at {floor}, black level {black}");
        }
        seen += 1;
    }
    eprintln!("Sony black levels checked against the data floor on {seen} files");
}

/// Issue #535: the white balance a Sony ARW records as applied, `WB_RGGBLevels`, read from the encrypted
/// `SR2SubIFD` when the raw IFD has no plain copy. Before, the first three opened with no white balance (green) and
/// the last two with the maker note's `Tag2010` gains, which there follow the scene rather than the preset used.
/// Expected levels: ExifTool's decrypted `WB_RGGBLevels` (green 1024). In brackets, the gains that make the
/// camera JPEG's neutral pixels neutral.
#[test]
fn corpus_sony_sr2_white_balance() {
    let dir = corpus_root().join("raw");
    // (file, R and B levels)
    let cases = [
        ("arw-sony-a500.arw", [2212, 1416]),          // 27152-byte SR2SubIFD layout (2.04 / 1.35)
        ("arw-sony-a33.arw", [2344, 1508]),           // 29000-byte layout (2.29 / 1.48)
        ("arw-sony-a700.arw", [2128, 1564]),          // 62112-byte layout (too few neutral pixels)
        ("arw-sony-a3500-5600k.arw", [2932, 1576]),   // 5600 K (2.76 / 1.53; Tag2010 1.89 / 3.08)
        ("arw-sony-a7s-shade.arw", [2932, 1372]),     // Shade (2.90 / 1.32; Tag2010 2.63 / 1.52)
        ("arw-sony-a900-packed12.arw", [2688, 1516]), // packed 12-bit (the maker note's plain 0x0020 block agrees)
    ];
    let mut seen = 0;
    for (name, [r, b]) in cases {
        let Ok(bytes) = std::fs::read(dir.join(name)) else {
            eprintln!("skip: {name} absent");
            continue;
        };
        let img = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        let wb = img.wb_multipliers.unwrap_or_else(|| panic!("{name}: no as-shot white balance"));
        let (er, eb) = (r as f32 / 1024.0, b as f32 / 1024.0);
        assert!((wb[0] - er).abs() < 1e-4 && wb[1] == 1.0 && (wb[2] - eb).abs() < 1e-4, "{name}: white balance {wb:?}, expected [{er}, 1, {eb}]");
        seen += 1;
    }
    eprintln!("Sony SR2SubIFD white balance checked on {seen} files");
}

/// The DSLR-A900's packed 12-bit strip decodes (issue #535, item 6): 12 bits on a black of 128, and the camera
/// JPEG's `FullImageSize` window centred in the frame, which has no padding at either edge.
#[test]
fn corpus_sony_a900_packed12() {
    let name = "arw-sony-a900-packed12.arw";
    let Ok(bytes) = std::fs::read(corpus_root().join("raw").join(name)) else {
        eprintln!("skip: {name} absent");
        return;
    };
    let img = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!((img.width, img.height, img.bits), (6080, 4048, 12));
    assert_eq!(img.black.values, vec![128.0]);
    let c = img.crop;
    assert_eq!((c.x, c.y, c.width, c.height), (16, 8, 6048, 4032));
}

/// Issue #535: pre-2017 Sony bodies without crop tags shot in the camera's 16:9 mode. The ILCE-7SM2 records the
/// 16:9 size in `FullImageSize`, the DSLR-A580 only in the Exif image size; both crops are centred vertically (each
/// camera JPEG registered on its raw) and drop the right-edge padding, so the image matches its JPEG's aspect ratio
/// and gets a camera look.
#[test]
fn corpus_sony_16x9_crops() {
    let dir = corpus_root().join("raw");
    let mut seen = 0;
    for (name, (x, y, w, h)) in [("arw-sony-a7sm2-16x9.arw", (0, 232, 4240, 2384)), ("arw-sony-a580-16x9.arw", (0, 260, 4912, 2760))] {
        let bytes = match std::fs::read(dir.join(name)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("skip: {name} absent");
                continue;
            }
            Err(e) => panic!("{name}: {e}"),
        };
        let img = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        let c = img.crop;
        assert_eq!((c.x, c.y, c.width, c.height), (x, y, w, h), "{name}: crop");
        let jpeg = embedded_preview(&bytes).unwrap_or_else(|| panic!("{name}: no preview"));
        let mut d = zune_jpeg::JpegDecoder::new(zune_core::bytestream::ZCursor::new(&jpeg));
        d.decode_headers().unwrap();
        let info = d.info().unwrap();
        let (aspect, preview) = (c.width as f64 / c.height as f64, info.width as f64 / info.height as f64);
        assert!((aspect / preview - 1.0).abs() < 0.02, "{name}: crop aspect {aspect} vs camera JPEG {preview}");
        seen += 1;
    }
    eprintln!("Sony 16:9 crops checked on {seen} files");
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

/// Olympus ORF (`crates/raw/src/vendor/orf.rs`): the colour-filter layout comes from the file's Exif `CFAPattern`.
/// With it, the decoded mosaic's colours follow the camera's own JPEG better than with any other layout.
#[test]
fn corpus_orf_cfa_patterns() {
    let dir = corpus_root().join("raw");
    let cases = [("orf-olympus-e1.orf", "GRBG"), ("orf-olympus-e400.orf", "GRBG"), ("orf-olympus-xz2.orf", "RGGB")];
    let mut seen = 0;
    for (name, cfa) in cases {
        let Ok(bytes) = std::fs::read(dir.join(name)) else {
            eprintln!("skip: {name} absent");
            continue;
        };
        let img = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(img.cfa.as_ref().map(|c| c.name()).as_deref(), Some(cfa), "{name}: CFA layout");
        assert_eq!(green_on_main_diagonal(&img), cfa.starts_with('G'), "{name}: mosaic statistics disagree with {cfa}");
        let jpeg = embedded_preview(&bytes).unwrap_or_else(|| panic!("{name}: no embedded preview"));
        let (gw, gh) = (24, 18);
        let (sites, jpeg) = (raw_sites(&img, gw, gh), jpeg_cells(&jpeg, gw, gh));
        let agree = |l: &str| chroma_agreement(&sites, &jpeg, &lightcraft_raw::Cfa::bayer(l).unwrap().pattern);
        let best_other = ["RGGB", "GRBG", "GBRG", "BGGR"].into_iter().filter(|l| *l != cfa).map(agree).fold(f64::MIN, f64::max);
        let own = agree(cfa);
        let a = img.active_area;
        eprintln!("{name:32} {cfa}, active area at ({}, {}): colour agreement {own:.3} (others ≤ {best_other:.3})", a.x, a.y);
        assert!(own > 0.5 && own > best_other, "{name}: colours follow the JPEG better with another layout ({own} vs {best_other})");
        seen += 1;
    }
    eprintln!("ORF CFA layouts checked on {seen} files");
}

/// Samsung SRW (`crates/raw/src/vendor/srw.rs`): the 12-bit packings of the NX10 and NX20 generations (opposite bit
/// orders, told apart from the samples) and the 16-bit words of the EX1 and WB2000 decode to the image of the camera's own
/// JPEG, with the layout, framing, levels and white balance the file's maker note states.
#[test]
fn corpus_samsung_srw() {
    let dir = corpus_root().join("raw");
    // (file, bits, active area width x height, crop width x height, CFA layout, JPEG shows the whole active area)
    let cases = [
        ("srw-samsung-nx10.srw", 12, (4592, 3056), (4592, 3056), "BGGR", true),
        ("srw-samsung-nx20.srw", 12, (5472, 3648), (5472, 3648), "GRBG", true),
        ("srw-samsung-ex1.srw", 14, (3648, 2736), (3648, 2736), "RGGB", true),
    ];
    let mut seen = 0;
    for (name, bits, active, crop, cfa, whole) in cases {
        let Ok(bytes) = std::fs::read(dir.join(name)) else {
            eprintln!("skip: {name} absent");
            continue;
        };
        let img = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(img.format, RawFormat::Srw, "{name}");
        assert_eq!(img.bits, bits, "{name}: bits");
        assert_eq!((img.active_area.width, img.active_area.height), active, "{name}: active area");
        assert_eq!((img.crop.width, img.crop.height), crop, "{name}: crop");
        assert_eq!(img.cfa.as_ref().map(|c| c.name()).as_deref(), Some(cfa), "{name}: CFA layout");
        let wb = img.wb_multipliers.unwrap_or_else(|| panic!("{name}: no as-shot white balance"));
        assert!(wb[0] > 1.0 && wb[1] == 1.0 && wb[2] > 0.5, "{name}: white balance {wb:?}");
        assert!(img.white_at(0) > (1u32 << bits) as f32 * 0.9 && img.white_at(0) <= ((1u32 << bits) - 1) as f32, "{name}: white {}", img.white_at(0));
        assert!(img.black.mean() < 8.0, "{name}: black {}", img.black.mean());
        // the mosaic inside the framing holds no marker or padding values above the saturation level
        let lightcraft_raw::RawData::U16(d) = &img.data else { panic!("float data") };
        let a = img.active_area;
        let above = (a.y..a.y + a.height)
            .map(|y| d[y * img.width + a.x..y * img.width + a.x + a.width].iter().filter(|&&v| f32::from(v) > ((1u32 << bits) - 1) as f32).count())
            .sum::<usize>();
        assert_eq!(above, 0, "{name}: samples above the saturation level in the active area");
        if whole {
            let jpeg = embedded_preview(&bytes).unwrap_or_else(|| panic!("{name}: no embedded preview"));
            let (gw, gh) = (24, 18);
            let (sites, jpeg) = (raw_sites(&img, gw, gh), jpeg_cells(&jpeg, gw, gh));
            let layout = img.cfa.clone().unwrap_or_else(|| panic!("{name}: no CFA"));
            let green: Vec<f64> = sites.iter().map(|s| (0..4).filter(|&i| layout.pattern[i] == 1).map(|i| s[i]).sum::<f64>()).collect();
            let rho = correlation(&ranks(&green), &ranks(&jpeg.iter().map(|j| j[1]).collect::<Vec<_>>()));
            let agree = |l: &str| chroma_agreement(&sites, &jpeg, &lightcraft_raw::Cfa::bayer(l).unwrap().pattern);
            let best_other = ["RGGB", "GRBG", "GBRG", "BGGR"].into_iter().filter(|l| *l != cfa).map(agree).fold(f64::MIN, f64::max);
            let own = agree(cfa);
            eprintln!(
                "{name:26} {}x{} {bits}-bit: green rank correlation {rho:.3}, colour agreement {own:.3} (others <= {best_other:.3})",
                img.width, img.height
            );
            assert!(rho > 0.9, "{name}: rank correlation with the camera JPEG {rho}");
            assert!(own > 0.5 && own > best_other, "{name}: colours follow the JPEG better with another layout ({own} vs {best_other})");
        }
        seen += 1;
    }
    eprintln!("Samsung SRW checked on {seen} files");
}

/// Reference sensor sums and position-weighted sums from black-box decoding, independent
/// of this implementation. These cover complete images, padding, stripe boundaries and
/// lossy block quantizer changes; the source files are CC0, fetched by `xtask corpus`.
#[test]
fn corpus_fujifilm_compressed_samples() {
    use sha2::{Digest, Sha256};
    let checksums = include_str!("../../../docs/raf-corpus.sha256");
    let cases: &[(&str, usize, usize, u64, u64)] = &[
        ("raf-fuji-gfx100-3773.raf", 11808, 8754, 483668733090, 6453395759968151080),
        ("raf-fuji-gfx100-3775.raf", 11808, 8754, 120434414850, 6144091676930008551),
        ("raf-fuji-gfx100rf-8091.raf", 11808, 8754, 509005112862, 6771145666297463071),
        ("raf-fuji-gfx100s-4495.raf", 11808, 8754, 941987648006, 4811558780180901389),
        ("raf-fuji-gfx100s-4503.raf", 11808, 8754, 939580201129, 4821879272388485929),
        ("raf-fuji-gfx50s-1435.raf", 9216, 6210, 79545293042, 2565169091480702981),
        ("raf-fuji-xe5-8509.raf", 7872, 5196, 76435253154, 1299804440011842144),
        ("raf-fuji-xh2-6001.raf", 7872, 5196, 83480264024, 1799924693528487099),
        ("raf-fuji-xh2-6002.raf", 7872, 5196, 72476569659, 1547275125734054244),
        ("raf-fuji-xm5-7748.raf", 6336, 4182, 93243605328, 986712869515471030),
        ("raf-fuji-xt2-865.raf", 6048, 4038, 44212684244, 541983062567959797),
        ("raf-fuji-xt20-compressed.raf", 6048, 4038, 72690241711, 882852288583469317),
        ("raf-fuji-xt4-3914.raf", 6384, 4182, 45943146703, 599112804573868025),
        ("raf-fuji-xt4-3918.raf", 6384, 4182, 46401490361, 605690093061448074),
        ("raf-fuji-xt5-6122.raf", 7872, 5196, 109559716516, 1653376659872066392),
        ("raf-fuji-xt5-6123.raf", 7872, 5196, 109489917369, 1673725237502600190),
        ("raf-fuji-xt50-7807.raf", 7872, 5196, 76453904143, 1262190863834170778),
    ];
    let dir = corpus_root().join("raw");
    let mut seen = 0;
    for &(name, width, height, sum, weighted) in cases {
        let path = format!("corpus/raw/{name}");
        let checksum = checksums.lines().filter_map(|s| s.split_once("  ")).find(|(_, p)| *p == path).unwrap().0;
        assert_eq!(checksum.len(), 64, "{name}: missing published SHA-256");
        let Ok(bytes) = std::fs::read(dir.join(name)) else { continue };
        let actual_sha: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(actual_sha, checksum, "{name}: corpus file differs from its pinned identity");
        let img = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!((img.width, img.height), (width, height), "{name}");
        let lightcraft_raw::RawData::U16(data) = img.data else { panic!("{name}: float data") };
        let actual = data
            .iter()
            .enumerate()
            .fold((0u64, 0u64), |(s, w), (i, &v)| (s.wrapping_add(u64::from(v)), w.wrapping_add((i as u64 + 1).wrapping_mul(u64::from(v)))));
        assert_eq!(actual, (sum, weighted), "{name}: sensor samples differ from reference");
        seen += 1;
    }
    eprintln!("verified {seen} Fujifilm compressed sensor arrays");
}

/// The container rule for TIFF "shells" (a small IFD0 next to a private raw block) must not touch real
/// raws: every corpus file keeps the format its maker implies, and none is described as a private block.
#[test]
fn corpus_raws_keep_their_container_and_are_not_thumbnail_shells() {
    let dir = corpus_root().join("raw");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        eprintln!("skip: {} absent", dir.display());
        return;
    };
    let mut checked = 0;
    for p in rd.flatten().map(|e| e.path()).filter(|p| p.is_file()) {
        let name = p.file_name().unwrap().to_string_lossy().to_lowercase();
        let expected = match name.split(['-', '.']).next().unwrap_or_default() {
            "arw" => RawFormat::Arw,
            "cr2" => RawFormat::Cr2,
            "cr3" => RawFormat::Cr3,
            "dng" => RawFormat::Dng,
            "nef" => RawFormat::Nef,
            "nrw" => RawFormat::Nef, // probed as Nef when the file has several IFDs
            "orf" => RawFormat::Orf,
            "pef" => RawFormat::Pef,
            "raf" => RawFormat::Raf,
            "srw" => RawFormat::Srw,
            "rw2" | "rwl" | "raw" => RawFormat::Rw2,
            _ => continue,
        };
        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(probe(&bytes), Some(expected), "{name}");
        if let Err(RawError::Unsupported(why)) = probe_info(&bytes) {
            assert!(!why.contains("private block"), "{name}: {why}");
        }
        checked += 1;
    }
    eprintln!("{checked} corpus raws keep their container");
}

/// Matching Sony table dimensions are not evidence of a shared correction model. Exercise the
/// actual corpus headers and decoded files as well as the synthetic model-boundary unit tests.
#[test]
fn sony_embedded_distortion_is_limited_to_validated_models() {
    let root = corpus_root().join("raw");
    let mut checked = 0;
    for (name, model, expected) in [
        ("arw-sony-a7rm4a-compressed.arw", "ILCE-7RM4A", 1),
        ("arw-sony-a9m2-compressed.arw", "ILCE-9M2", 0),
        ("arw-sony-a7m3-compressed.arw", "ILCE-7M3", 0),
        ("arw-sony-a7m3-uncompressed.arw", "ILCE-7M3", 0),
        ("arw-sony-a7m4-14bit.arw", "ILCE-7M4", 0),
        ("arw-sony-a7m4-lossless-l.arw", "ILCE-7M4", 0),
        ("arw-sony-a7m4-lossless-m.arw", "ILCE-7M4", 0),
        ("arw-sony-a7m4-lossless-s.arw", "ILCE-7M4", 0),
        ("arw-sony-a7rm2-12bit-uncompressed.arw", "ILCE-7RM2", 0),
        ("arw-sony-rx100.arw", "DSC-RX100", 0),
        ("arw-sony-rx100m3.arw", "DSC-RX100M3", 0),
    ] {
        let path = root.join(name);
        if !path.exists() {
            eprintln!("skip: {} absent", path.display());
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let full = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(probe_info(&bytes).unwrap(), full.info(), "{name}");
        assert_eq!(full.metadata.model.as_deref(), Some(model), "{name}");
        assert_eq!(full.opcodes.list3.len(), expected, "{name}: only the validated Sony model gets a distortion warp");
        eprintln!("{name}: {expected} warp(s); header and full decode agree");
        checked += 1;
    }
    eprintln!("Sony distortion model boundary: {checked} corpus files checked");
}
