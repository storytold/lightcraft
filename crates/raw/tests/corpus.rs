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
    "cr3-",                            // CR3 / CRX (M11.1)
    "arw-sony-a7m4-lossless-m",        // Sony lossless compressed M/S: subsampled (YCbCr) lossless JPEG
    "arw-sony-a7m4-lossless-s",        // "
    "raf-fuji-xt20-compressed",        // Fujifilm compressed RAF
    "rw2-panasonic-gh5.",              // Panasonic raw format 4 (quantised)
    "rw2-panasonic-gx80",              // "
    "rw2-panasonic-g9-b",              // "
    "orf-olympus-om1ii-hires50-14bit", // Olympus 14-bit compressed ORF (OM-1 Mark II High Res Shot)
    "sraw",                            // Canon sRAW / mRAW
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
        // DNG previews are optional (and some carry only an uncompressed RGB thumbnail); vendor raws always embed a JPEG
        if let Some(p) = &preview {
            assert!(p.starts_with(&[0xff, 0xd8]) && p.ends_with(&[0xff, 0xd9]), "{name}: preview is not a JPEG");
        } else {
            assert_eq!(fmt, RawFormat::Dng, "{name}: no embedded preview");
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
/// look table; we read them (and render with them). Camera-written DNGs here carry none.
#[test]
fn corpus_adobe_dngs_carry_profile_looks() {
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
        eprintln!(
            "{name:44} profile look: hsm {} look {} tone {}",
            look.hue_sat_map[0].is_some(),
            look.look_table.is_some(),
            look.tone_curve.is_some()
        );
    }
    eprintln!("{seen} Adobe-converted DNGs checked");
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

/// Per cell of a 16×12 grid: the mean red, green and blue of an 8-bit RGB image, linearised (gamma 2.2).
fn preview_cells(jpeg: &[u8]) -> Vec<[f64; 3]> {
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(jpeg), opts);
    let px = d.decode().unwrap();
    let (w, h) = d.dimensions().unwrap();
    let mut cells = vec![([0f64; 3], 0f64); 16 * 12];
    for y in (0..h).step_by(4) {
        for x in (0..w).step_by(4) {
            let cell = &mut cells[(y * 12 / h) * 16 + x * 16 / w];
            for c in 0..3 {
                cell.0[c] += (px[(y * w + x) * 3 + c] as f64 / 255.0).powf(2.2);
            }
            cell.1 += 1.0;
        }
    }
    cells.into_iter().map(|(s, n)| s.map(|v| v / n)).collect()
}

/// The same grid over a raw image's active area: black-subtracted, white-balanced means of its red, green and blue
/// sites, relative to white.
fn raw_cells(img: &lightcraft_raw::RawImage) -> Vec<[f64; 3]> {
    let lightcraft_raw::RawData::U16(d) = &img.data else { panic!("float data") };
    let (a, cfa) = (img.active_area, img.cfa.as_ref().unwrap());
    let wb = img.wb_multipliers.unwrap_or([1.0; 3]);
    let (black, white) = (img.black.mean() as f64, img.white_at(0) as f64);
    let mut cells = vec![([0f64; 3], [0f64; 3]); 16 * 12];
    for y in (0..a.height).step_by(3) {
        for x in (0..a.width).step_by(3) {
            let cell = &mut cells[(y * 12 / a.height) * 16 + x * 16 / a.width];
            let c = cfa.color_at(a.x + x, a.y + y) as usize;
            cell.0[c] += d[(a.y + y) * img.width + a.x + x] as f64 - black;
            cell.1[c] += 1.0;
        }
    }
    cells.into_iter().map(|(s, n)| [0, 1, 2].map(|c| s[c] / n[c] * wb[c] as f64 / (white - black))).collect()
}

/// ORFs decode to the picture their own embedded JPEG shows: over a 16×12 grid, brightness and the red/blue
/// balance follow the preview. A wrong decode fails the first, a wrong colour-filter layout the second (greens
/// taken for red and blue give no red/blue signal; swapped red and blue an inverted one, −0.97 on the E-M1 and
/// E-620 samples, which are BGGR, when read as RGGB).
#[test]
fn corpus_orf_matches_embedded_preview() {
    let dir = corpus_root().join("raw");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        eprintln!("skip: {} absent", dir.display());
        return;
    };
    let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    paths.sort();
    let mut seen = 0;
    for p in paths {
        let name = p.file_name().unwrap().to_string_lossy().to_lowercase();
        if !name.starts_with("orf-") || !name.ends_with(".orf") {
            continue;
        }
        let bytes = std::fs::read(&p).unwrap();
        let Ok(img) = decode(&bytes) else { continue };
        let (raw, jpeg) = (raw_cells(&img), preview_cells(&embedded_preview(&bytes).unwrap()));
        // cells that are neither near black nor clipped in either image
        let usable: Vec<usize> = (0..raw.len()).filter(|&i| raw[i].iter().chain(&jpeg[i]).all(|&v| v > 0.004 && v < 0.8)).collect();
        let pick = |f: &dyn Fn(&[f64; 3]) -> f64, cells: &[[f64; 3]]| usable.iter().map(|&i| f(&cells[i])).collect::<Vec<_>>();
        let luma = |c: &[f64; 3]| c[1].ln();
        let red_blue = |c: &[f64; 3]| (c[0] / c[2]).ln();
        let (rl, rc) = (correlation(&pick(&luma, &raw), &pick(&luma, &jpeg)), correlation(&pick(&red_blue, &raw), &pick(&red_blue, &jpeg)));
        eprintln!(
            "{name:44} {} cells: brightness correlation {rl:.3}, red/blue correlation {rc:.3}; {}",
            usable.len(),
            img.cfa.as_ref().unwrap().name()
        );
        assert!(rl > 0.9, "{name}: brightness correlation with the preview {rl}");
        assert!(rc > 0.7, "{name}: red/blue correlation with the preview {rc}");
        seen += 1;
    }
    eprintln!("{seen} ORFs compared with their previews");
}
