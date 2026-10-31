//! The unsupported CRX variant list of `docs/cr3.md`, enforced in CI with synthetic fixtures.
//!
//! Each test wraps a hand-built CRX stream in a minimal but structurally valid CR3 container
//! (`ftyp` + `moov` → `trak` → `stbl` with a `CRAW` entry carrying a `CMP1` descriptor), like the unit
//! fixtures in `vendor/crx.rs` but driven through the public container API. For every unsupported
//! coding variant it asserts the exact `RawError::Unsupported` string, and that header probing
//! (`probe_info`, the import path) and full decoding fail with the *identical* error: when the engine
//! falls back to the embedded preview it shows the reason from either call (`engine/files.rs`
//! `preview_reason`), so the two layers must agree on it. Control fixtures decode successfully,
//! including both edges of each accepted range, so a rejection below can only come from the
//! documented gate.
//!
//! The accepted ranges are the ones in `vendor/crx.rs`: uniform quantization values 4..=44 and
//! adaptive QP values 131..=175. Odd QP-map heights are not a rejected variant any more (the last
//! QP row is used on its own); the unit tests in `vendor/crx.rs` cover them. The deeper
//! `"Canon CRX adaptive low-frequency quantization"` dequantization gate is unreachable through
//! this API: plane validation rejects any gain on the low-frequency bands first. It is defence in
//! depth, not a contract, and has no test here.

use lightcraft_raw::{RawData, RawError, decode, probe_info};

// --- minimal CR3 container -------------------------------------------------

fn box_bytes(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = (u32::try_from(body.len() + 8).unwrap()).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

/// A 52-byte `CMP1` descriptor: 4×4 single-tile 14-bit four-plane RGGB lossless, 108 CRX header bytes.
fn cmp1() -> Vec<u8> {
    let mut c = vec![0u8; 52];
    c[0..2].copy_from_slice(&0xff00u16.to_be_bytes());
    c[2..4].copy_from_slice(&48u16.to_be_bytes()); // base length 48 + the 4-byte header
    c[4..6].copy_from_slice(&0x100u16.to_be_bytes());
    for (at, value) in [(8, 4u32), (12, 4), (16, 4), (20, 4)] {
        c[at..at + 4].copy_from_slice(&value.to_be_bytes()); // width, height, tile width, tile height
    }
    c[24] = 14; // bit depth
    c[25] = 0x40; // four planes, CFA 0 (RGGB)
    c[28..32].copy_from_slice(&108u32.to_be_bytes());
    c
}

fn set_words(coding: &mut [u8], words: [(usize, u32); 5]) {
    for (at, value) in words {
        coding[at..at + 4].copy_from_slice(&value.to_be_bytes());
    }
}

/// 16×16 single-tile three-level 0x100 C-RAW, 540 CRX header bytes.
fn craw_coding() -> Vec<u8> {
    let mut c = cmp1();
    set_words(&mut c, [(8, 16), (12, 16), (16, 16), (20, 16), (28, 540)]);
    c[26] = 3; // encoding 0, three wavelet levels
    c
}

/// The same image coded as adaptive-quantized 0x200 C-RAW, 872 CRX header bytes.
fn adaptive_coding() -> Vec<u8> {
    let mut c = craw_coding();
    c[4..6].copy_from_slice(&0x200u16.to_be_bytes());
    c[28..32].copy_from_slice(&872u32.to_be_bytes());
    c
}

/// A `CMP1` with the optional median-precision extension (`type 3` burst coding), which `supports()` rejects.
fn cmp1_with_median_precision() -> Vec<u8> {
    let mut c = cmp1();
    c[32] = 0x80; // extension present
    let mut extension = vec![0u8; 36];
    extension[0..4].copy_from_slice(&36u32.to_be_bytes());
    extension[4] = 0x40; // median precision present
    extension[32] = 10; // the precision itself
    c.extend(extension);
    c
}

/// Wrap a `CMP1` descriptor and its CRX sample in a one-track CR3 file. The `CRAW` entry's 16-bit
/// dimensions mirror the descriptor; decoding is driven by `CMP1` alone.
fn cr3_file(coding: &[u8], sample: &[u8]) -> Vec<u8> {
    let dimension = |at: usize| u32::from_be_bytes(coding[at..at + 4].try_into().unwrap()) as u16;
    let mut craw = vec![0u8; 82];
    craw[24..26].copy_from_slice(&dimension(8).to_be_bytes());
    craw[26..28].copy_from_slice(&dimension(12).to_be_bytes());
    craw.extend(box_bytes(b"CMP1", coding));
    let mut stsd = vec![0u8; 4];
    stsd.extend(1u32.to_be_bytes()); // entry count
    stsd.extend(box_bytes(b"CRAW", &craw));
    let mut stsz = vec![0u8; 4];
    stsz.extend(u32::try_from(sample.len()).unwrap().to_be_bytes());
    stsz.extend(1u32.to_be_bytes()); // one sample
    let mut co64 = vec![0u8; 4];
    co64.extend(1u32.to_be_bytes()); // entry count
    co64.extend([0u8; 8]); // sample offset, patched once the layout is known
    let tables = [box_bytes(b"stsd", &stsd), box_bytes(b"stsz", &stsz), box_bytes(b"co64", &co64)].concat();
    let trak = box_bytes(b"trak", &box_bytes(b"mdia", &box_bytes(b"minf", &box_bytes(b"stbl", &tables))));
    let mut file = box_bytes(b"ftyp", b"crx \0\0\0\x01crx isom");
    file.extend(box_bytes(b"moov", &trak));
    let offset = file.len();
    file.extend_from_slice(sample);
    let at = file.len() - sample.len() - 8; // the co64 offset field ends the moov
    file[at..at + 8].copy_from_slice(&u64::try_from(offset).unwrap().to_be_bytes());
    file
}

// --- CRX stream construction (the crx.rs fixtures, rebuilt here) ------------

fn marker_body(size: u32, flags: u32) -> Vec<u8> {
    [size.to_be_bytes(), flags.to_be_bytes()].concat()
}

fn marker(out: &mut Vec<u8>, code: u16, body: &[u8]) {
    out.extend(code.to_be_bytes());
    out.extend(u16::try_from(body.len()).unwrap().to_be_bytes());
    out.extend_from_slice(body);
}

/// Four 2×2 lossless planes at coefficient 1 (decodes to 8193 everywhere), with overridable
/// marker codes and flag bits so each variant gate can be aimed at precisely one field.
fn lossless_stream(tile: u16, plane: u16, band: u16, plane_flags: u32, band_flags: u32) -> Vec<u8> {
    let mut out = Vec::new();
    marker(&mut out, tile, &marker_body(4, 0));
    for p in 0..4u32 {
        marker(&mut out, plane, &marker_body(1, (p << 28) | 0x0800_0000 | plane_flags));
        marker(&mut out, band, &marker_body(1, 0x0020_0000 | band_flags));
    }
    out.extend([0x1e; 4]); // per plane, one byte: row 0 = run flag 0, Rice 001 (+1), Rice 1 (0); row 1 = full-row run; zero alignment
    out
}

/// Single-tile 16×16 three-level C-RAW with uniform quantization `quant` in every band
/// (quant 4 decodes to 8192 everywhere).
fn craw_stream(quant: u8) -> Vec<u8> {
    let mut out = Vec::new();
    marker(&mut out, 0xff01, &marker_body(52, 0)); // four 13-byte planes
    for plane in 0..4u32 {
        marker(&mut out, 0xff02, &marker_body(13, (plane << 28) | 0x0800_0000));
        for band in 0..10u32 {
            marker(&mut out, 0xff03, &marker_body(u32::from(band >= 7) + 1, (band << 28) | (u32::from(quant) << 19)));
        }
    }
    for _ in 0..4 {
        out.extend([0x80; 4]); // LL3 and the three 1×1 HF3 bands
        out.extend([0xf0; 3]); // three 2×2 all-zero HF2 bands
        for _ in 0..3 {
            out.extend([0xff, 0xf8]); // three 4×4 all-zero HF1 bands
        }
    }
    out
}

/// Accepted adaptive-quantization gains: none on the low-frequency bands.
const ADAPTIVE_GAINS: [u32; 10] = [0, 0, 0, 0, 4, 4, 8, 8, 8, 16];
/// A five-byte QP map for the 1×4 map of a 16×16 image: the 32-bit literal 300 (QP 150 from a
/// prediction of zero), then three more rows at 150.
const ADAPTIVE_QP: [u8; 5] = [0, 0, 1, 0x2c, 0x94];

/// `ADAPTIVE_QP` with another first symbol. It is a 32-bit literal, mapped like every Rice-coded
/// symbol: even literals give `literal / 2`, odd ones `-(literal + 1) / 2`. The three zero residuals
/// that follow stay valid for any literal of 6 or more, which leaves the Rice parameter at 2.
fn qp_with_literal(literal: u32) -> [u8; 5] {
    let mut qp = ADAPTIVE_QP;
    qp[0..4].copy_from_slice(&literal.to_be_bytes());
    qp
}

/// Single-tile 16×16 adaptive-quantized 0x200 C-RAW (decodes to 8192 everywhere), with the
/// per-band gains, the two reserved header fields and the QP map bytes under test control.
fn adaptive_stream(gains: &[u32; 10], tile_reserved: u16, band_reserved: u16, qp: [u8; 5]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut tile = Vec::new();
    tile.extend(60u32.to_be_bytes()); // tile data size
    tile.extend(0x4000u32.to_be_bytes()); // index 0, adaptive-tile magic
    tile.extend(5u32.to_be_bytes()); // QP map length
    tile.extend(3u16.to_be_bytes()); // QP padding
    tile.extend(tile_reserved.to_be_bytes());
    marker(&mut out, 0xff11, &tile);
    for plane in 0..4u32 {
        marker(&mut out, 0xff12, &marker_body(13, (plane << 28) | 0x0800_0000));
        for (band, &gain) in gains.iter().enumerate() {
            let mut body = Vec::new();
            body.extend(u32::from(band >= 7).wrapping_add(1).to_be_bytes()); // band size
            body.extend(((band as u32) << 28 | gain).to_be_bytes());
            body.extend(u32::from(band < 4).to_be_bytes()); // LL3 base quantization
            body.extend([0u8; 2]); // band padding
            body.extend(band_reserved.to_be_bytes());
            marker(&mut out, 0xff13, &body);
        }
    }
    out.extend([0u8; 4]); // zero padding between the marker tree and the data area
    out.extend(qp);
    out.extend([0u8; 3]); // QP padding
    for _ in 0..4 {
        out.extend([0x80; 4]);
        out.extend([0xf0; 3]);
        for _ in 0..3 {
            out.extend([0xff, 0xf8]);
        }
    }
    out
}

// --- assertions -------------------------------------------------------------

/// `probe_info` (Mode::Header, the import path) and `decode` (Mode::Full) must both reject the
/// file with exactly this `Unsupported` message: same error class, same text.
fn assert_rejected(name: &str, file: &[u8], expected: &str) {
    let probed = match probe_info(file) {
        Ok(_) => panic!("{name}: probe_info unexpectedly succeeded"),
        Err(error) => error,
    };
    let full = match decode(file) {
        Ok(_) => panic!("{name}: decode unexpectedly succeeded"),
        Err(error) => error,
    };
    assert_eq!(probed, full, "{name}: probe_info and decode must report the same error");
    assert!(matches!(&probed, RawError::Unsupported(why) if why == expected), "{name}: expected Unsupported(\"{expected}\"), got {probed:?}");
}

// --- controls ---------------------------------------------------------------

#[test]
fn control_lossless_fixture_decodes_through_the_container() {
    let file = cr3_file(&cmp1(), &lossless_stream(0xff01, 0xff02, 0xff03, 0, 0));
    let raw = decode(&file).unwrap();
    assert_eq!((raw.width, raw.height, raw.bits, raw.cpp), (4, 4, 14, 1));
    assert_eq!(raw.data, RawData::U16(vec![8193; 16]));
    assert_eq!(probe_info(&file).unwrap(), raw.info());
}

#[test]
fn control_craw_and_adaptive_fixtures_decode_through_the_container() {
    let craw = cr3_file(&craw_coding(), &craw_stream(4));
    let raw = decode(&craw).unwrap();
    assert_eq!(raw.data, RawData::U16(vec![8192; 256]));
    assert_eq!(probe_info(&craw).unwrap(), raw.info());
    let adaptive = cr3_file(&adaptive_coding(), &adaptive_stream(&ADAPTIVE_GAINS, 0, 0, ADAPTIVE_QP));
    let raw = decode(&adaptive).unwrap();
    assert_eq!(raw.data, RawData::U16(vec![8192; 256]));
    assert_eq!(probe_info(&adaptive).unwrap(), raw.info());
}

// --- supports() gates -------------------------------------------------------

#[test]
fn unknown_version_and_wavelet_level_counts_are_unsupported() {
    let mut coding = cmp1();
    coding[4..6].copy_from_slice(&0x300u16.to_be_bytes());
    assert_rejected(
        "version 0x300",
        &cr3_file(&coding, &lossless_stream(0xff01, 0xff02, 0xff03, 0, 0)),
        "Canon CRX version 0x300, encoding 0, 4 planes, 0 wavelet levels",
    );
    let mut coding = craw_coding();
    coding[26] = 2; // two wavelet levels: neither lossless nor three-level C-RAW
    assert_rejected("two wavelet levels", &cr3_file(&coding, &craw_stream(4)), "Canon CRX version 0x100, encoding 0, 4 planes, 2 wavelet levels");
}

#[test]
fn bit_depth_outside_8_to_16_is_unsupported() {
    // CMP1 parsing only accepts depths 1..=16, so the reachable side of the gate is 1..=7.
    let mut coding = cmp1();
    coding[24] = 7;
    assert_rejected("7-bit", &cr3_file(&coding, &lossless_stream(0xff01, 0xff02, 0xff03, 0, 0)), "Canon CRX 7-bit samples");
}

#[test]
fn vertical_craw_tiling_is_unsupported() {
    let mut coding = craw_coding();
    coding[20..24].copy_from_slice(&8u32.to_be_bytes()); // two tile rows
    assert_rejected("vertical tiles", &cr3_file(&coding, &craw_stream(4)), "Canon CRX C-RAW with vertical tiles");
}

#[test]
fn v200_requires_a_single_tile_14_bit_image() {
    let mut coding = adaptive_coding();
    coding[24] = 12;
    assert_rejected(
        "0x200 12-bit",
        &cr3_file(&coding, &adaptive_stream(&ADAPTIVE_GAINS, 0, 0, ADAPTIVE_QP)),
        "Canon CRX version 0x200 requires a single-tile 14-bit Bayer C-RAW image",
    );
    let mut coding = adaptive_coding();
    coding[16..20].copy_from_slice(&8u32.to_be_bytes()); // two tile columns
    assert_rejected(
        "0x200 two tiles",
        &cr3_file(&coding, &adaptive_stream(&ADAPTIVE_GAINS, 0, 0, ADAPTIVE_QP)),
        "Canon CRX version 0x200 requires a single-tile 14-bit Bayer C-RAW image",
    );
}

#[test]
fn extended_median_bit_depth_header_is_unsupported() {
    assert_rejected(
        "median extension",
        &cr3_file(&cmp1_with_median_precision(), &lossless_stream(0xff01, 0xff02, 0xff03, 0, 0)),
        "Canon CRX extended median-bit-depth header",
    );
}

// --- validate_planes gates --------------------------------------------------

#[test]
fn rounded_plane_coding_is_unsupported() {
    assert_rejected("rounded plane", &cr3_file(&cmp1(), &lossless_stream(0xff01, 0xff02, 0xff03, 1 << 25, 0)), "Canon CRX plane coding flags");
}

#[test]
fn partial_subband_coding_is_unsupported() {
    assert_rejected(
        "partial subband",
        &cr3_file(&cmp1(), &lossless_stream(0xff01, 0xff02, 0xff03, 0, 0x0800_0000)),
        "Canon CRX partial subband coding",
    );
}

#[test]
fn adaptive_gains_on_low_frequency_bands_are_unsupported() {
    // Plane validation rejects any gain on bands 0-3 before decoding. The deeper dequantization
    // gate behind it ("Canon CRX adaptive low-frequency quantization") is therefore unreachable
    // through the public API; this asserts the reachable gate's exact string.
    let mut gains = ADAPTIVE_GAINS;
    gains[0] = 4;
    assert_rejected(
        "LL band gain",
        &cr3_file(&adaptive_coding(), &adaptive_stream(&gains, 0, 0, ADAPTIVE_QP)),
        "Canon CRX adaptive quantization subband parameters",
    );
}

#[test]
fn uniform_quantization_outside_4_to_44_is_unsupported() {
    for quant in [3u8, 45] {
        assert_rejected(&format!("quant {quant}"), &cr3_file(&craw_coding(), &craw_stream(quant)), &format!("Canon CRX quantization value {quant}"));
    }
}

#[test]
fn uniform_quantization_edges_of_the_accepted_range_decode() {
    // Zero coefficients decode to the mid level whatever the step, so only the gate decides.
    for quant in [4u8, 44] {
        let file = cr3_file(&craw_coding(), &craw_stream(quant));
        let raw = decode(&file).unwrap();
        assert_eq!(raw.data, RawData::U16(vec![8192; 256]), "quant {quant}");
        assert_eq!(probe_info(&file).unwrap(), raw.info(), "quant {quant}");
    }
}

// --- qp_map gate ------------------------------------------------------------

#[test]
fn adaptive_qp_outside_131_to_175_is_unsupported() {
    // Literal 260 → 130 and 352 → 176 sit just outside the accepted range; literal 5 → −3 is far outside.
    for (literal, value) in [(260u32, 130i32), (352, 176), (5, -3)] {
        assert_rejected(
            &format!("QP {value}"),
            &cr3_file(&adaptive_coding(), &adaptive_stream(&ADAPTIVE_GAINS, 0, 0, qp_with_literal(literal))),
            &format!("Canon CRX adaptive QP value {value}"),
        );
    }
}

#[test]
fn adaptive_qp_edges_of_the_accepted_range_decode() {
    // Literal 262 → QP 131 and 350 → QP 175 (the EOS R6 Mark II's highest); zero coefficients decode to the mid
    // level whatever the step.
    for (literal, value) in [(262u32, 131i32), (350, 175)] {
        let file = cr3_file(&adaptive_coding(), &adaptive_stream(&ADAPTIVE_GAINS, 0, 0, qp_with_literal(literal)));
        let raw = decode(&file).unwrap();
        assert_eq!(raw.data, RawData::U16(vec![8192; 256]), "QP {value}");
        assert_eq!(probe_info(&file).unwrap(), raw.info(), "QP {value}");
    }
}

// --- layout() marker gates --------------------------------------------------

#[test]
fn ff1x_marker_family_inside_a_0x100_stream_is_unsupported() {
    for (name, tile, plane, band) in
        [("ff11 tile", 0xff11, 0xff02, 0xff03), ("ff12 plane", 0xff01, 0xff12, 0xff03), ("ff13 band", 0xff01, 0xff02, 0xff13)]
    {
        assert_rejected(name, &cr3_file(&cmp1(), &lossless_stream(tile, plane, band, 0, 0)), "Canon CRX ff11/ff12/ff13 marker family");
    }
}

#[test]
fn marker_header_length_mismatch_is_unsupported() {
    let mut stream = lossless_stream(0xff01, 0xff02, 0xff03, 0, 0);
    stream[2..4].copy_from_slice(&12u16.to_be_bytes()); // tile header claims 12 body bytes, 8 expected
    assert_rejected("0x100 tile length", &cr3_file(&cmp1(), &stream), "Canon CRX marker 0xff01 header length 12");
    let mut stream = adaptive_stream(&ADAPTIVE_GAINS, 0, 0, ADAPTIVE_QP);
    stream[2..4].copy_from_slice(&8u16.to_be_bytes()); // tile header claims 8 body bytes, 16 expected
    assert_rejected("0x200 tile length", &cr3_file(&adaptive_coding(), &stream), "Canon CRX marker 0xff11 header length 8");
}

#[test]
fn nonzero_reserved_fields_in_0x200_headers_are_unsupported() {
    assert_rejected(
        "tile reserved",
        &cr3_file(&adaptive_coding(), &adaptive_stream(&ADAPTIVE_GAINS, 1, 0, ADAPTIVE_QP)),
        "Canon CRX adaptive quantization tile header",
    );
    assert_rejected(
        "band reserved",
        &cr3_file(&adaptive_coding(), &adaptive_stream(&ADAPTIVE_GAINS, 0, 1, ADAPTIVE_QP)),
        "Canon CRX adaptive subband reserved field",
    );
}

// --- vendor/cr3.rs adapter gate ---------------------------------------------

#[test]
fn non_four_plane_codings_are_rejected_at_the_adapter() {
    // CMP1 parsing allows one plane (with any CFA), which the adapter rejects before the CRX layer.
    let mut coding = cmp1();
    coding[25] = 0x10; // one plane
    assert_rejected("one plane", &cr3_file(&coding, &lossless_stream(0xff01, 0xff02, 0xff03, 0, 0)), "Canon CR3 encoding 0 with 1 planes");
}

#[test]
fn non_crx_encodings_are_rejected_at_the_adapter() {
    let mut coding = cmp1();
    coding[26] = 0x10; // encoding 1, still zero wavelet levels
    assert_rejected("encoding 1", &cr3_file(&coding, &lossless_stream(0xff01, 0xff02, 0xff03, 0, 0)), "Canon CR3 encoding 1 with 4 planes");
}
