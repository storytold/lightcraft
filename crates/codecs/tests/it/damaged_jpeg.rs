//! JPEGs cut short (an interrupted download or copy: no end-of-image marker) are read from the
//! data that is there: `read_header` accepts them with `truncated` set, and a decode (at full size
//! or scaled) shows the picture so far. What has nothing to show stays refused.

use lightcraft_codecs::*;

const W: usize = 160;
const H: usize = 96;

fn pixels() -> Vec<u8> {
    let mut v = Vec::with_capacity(W * H * 3);
    for y in 0..H {
        for x in 0..W {
            v.extend([((x * 3 + y) % 256) as u8, ((y * 5 + x / 2) % 256) as u8, ((x ^ y) * 4 % 256) as u8]);
        }
    }
    v
}

fn encode(progressive: bool, optimized: bool, sampling: jpeg_encoder::SamplingFactor) -> Vec<u8> {
    let mut out = Vec::new();
    let mut e = jpeg_encoder::Encoder::new(&mut out, 85);
    e.set_progressive(progressive);
    e.set_optimized_huffman_tables(optimized);
    e.set_sampling_factor(sampling);
    e.encode(&pixels(), W as u16, H as u16, jpeg_encoder::ColorType::Rgb).unwrap();
    out
}

/// Interleaved baseline, 4:2:0, one scan.
fn baseline() -> Vec<u8> {
    encode(false, false, jpeg_encoder::SamplingFactor::R_4_2_0)
}

/// Baseline from our own encoder: restart intervals between bands of rows.
fn baseline_restarts() -> Vec<u8> {
    let rgb = pixels();
    encode_jpeg(&EncodeImage::new(W as u32, H as u32, 3, Samples::U8(&rgb)), 85, ChromaSubsampling::S420, &EncodeMeta::default()).unwrap()
}

/// Interleaved baseline with a restart interval of one MCU (every block row has restart markers
/// inside it), 4:4:4.
fn baseline_short_restarts() -> Vec<u8> {
    let mut out = Vec::new();
    let mut e = jpeg_encoder::Encoder::new(&mut out, 85);
    e.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_4_4);
    e.set_restart_interval(1);
    e.encode(&pixels(), W as u16, H as u16, jpeg_encoder::ColorType::Rgb).unwrap();
    out
}

/// Grey baseline, and grey progressive.
fn grey(progressive: bool) -> Vec<u8> {
    let luma: Vec<u8> = pixels().chunks(3).map(|p| p[1]).collect();
    let mut out = Vec::new();
    let mut e = jpeg_encoder::Encoder::new(&mut out, 85);
    e.set_progressive(progressive);
    e.encode(&luma, W as u16, H as u16, jpeg_encoder::ColorType::Luma).unwrap();
    out
}

/// Progressive, 4:2:0, a dozen scans.
fn progressive() -> Vec<u8> {
    encode(true, true, jpeg_encoder::SamplingFactor::R_4_2_0)
}

/// Where each scan (SOS marker) starts.
fn scan_starts(b: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut p = 2;
    while p + 4 <= b.len() {
        if b[p] != 0xFF {
            p += 1;
            continue;
        }
        match b[p + 1] {
            0xDA => {
                out.push(p);
                p += 2 + u16::from_be_bytes([b[p + 2], b[p + 3]]) as usize;
                while p + 1 < b.len() && !(b[p] == 0xFF && !matches!(b[p + 1], 0x00 | 0xFF | 0xD0..=0xD7)) {
                    p += 1;
                }
            }
            0xD9 => break,
            0xFF | 0x00 | 0x01 | 0xD0..=0xD8 => p += 1,
            _ => p += 2 + u16::from_be_bytes([b[p + 2], b[p + 3]]) as usize,
        }
    }
    out
}

fn mean_abs_diff(a: &Decoded, b: &Decoded) -> f32 {
    assert_eq!((a.image.width, a.image.height), (b.image.width, b.image.height));
    let sum: f32 = a.image.data.iter().zip(&b.image.data).map(|(p, q)| (0..3).map(|c| (p[c] - q[c]).abs()).sum::<f32>()).sum();
    sum / (a.image.data.len() * 3) as f32
}

fn row(d: &Decoded, y: usize) -> &[[f32; 3]] {
    &d.image.data[y * d.image.width..(y + 1) * d.image.width]
}

fn header_of(b: &[u8]) -> Header {
    read_header(b).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn a_whole_file_is_not_truncated() {
    for b in [baseline(), baseline_restarts(), progressive()] {
        let h = header_of(&b);
        assert_eq!((h.width, h.height, h.truncated), (W as u32, H as u32, false));
    }
}

#[test]
fn a_baseline_file_cut_mid_scan_is_read_from_the_rows_present() {
    for whole in [baseline(), baseline_restarts(), baseline_short_restarts()] {
        let full = decode(&whole, DecodeOptions::default()).unwrap();
        let cut = &whole[..whole.len() * 6 / 10];
        let h = header_of(cut);
        assert_eq!((h.format, h.width, h.height, h.truncated), (Format::Jpeg, W as u32, H as u32, true));
        let d = decode(cut, DecodeOptions::default()).unwrap();
        assert_eq!((d.image.width, d.image.height, d.source_width, d.source_height), (W, H, W as u32, H as u32));
        // the rows that came through are the picture's own
        for y in 0..H / 4 {
            for (a, b) in row(&d, y).iter().zip(row(&full, y)) {
                assert!((0..3).all(|c| (a[c] - b[c]).abs() < 1e-3), "row {y}");
            }
        }
        // the rest is a flat neutral grey, not smeared colour
        let last = row(&d, H - 1);
        assert!(last.iter().all(|p| (p[0] - p[1]).abs() < 1e-3 && (p[1] - p[2]).abs() < 1e-3 && p[0] > 0.05 && p[0] < 0.6), "{:?}", last[0]);
        assert!(last.iter().all(|p| *p == last[0]), "{:?}", last[0]);
    }
}

#[test]
fn a_progressive_file_cut_after_a_scan_shows_what_those_scans_hold() {
    let whole = progressive();
    let full = decode(&whole, DecodeOptions::default()).unwrap();
    let starts = scan_starts(&whole);
    assert!(starts.len() >= 8, "{starts:?}");
    let mut errors = Vec::new();
    for (i, &start) in starts.iter().enumerate().skip(1) {
        // up to the next scan's marker: the scan before it is whole (a cut exactly at a scan's end
        // can't be told from one inside it)
        let cut = &whole[..start + 2];
        let h = header_of(cut);
        assert_eq!((h.width, h.height, h.truncated), (W as u32, H as u32, true), "cut before scan {i}");
        let d = decode(cut, DecodeOptions::default()).unwrap_or_else(|e| panic!("cut before scan {i}: {e}"));
        errors.push(mean_abs_diff(&d, &full));
    }
    // more scans, closer to the whole picture; the last cuts hold nearly all of it
    assert!(errors.windows(2).all(|w| w[1] <= w[0] + 0.01), "{errors:?}");
    assert!(*errors.last().unwrap() < 0.05, "{errors:?}");
    assert!(errors[0] > *errors.last().unwrap(), "{errors:?}");
}

#[test]
fn a_progressive_file_cut_inside_a_scan_or_a_segment_is_read_too() {
    let whole = progressive();
    let starts = scan_starts(&whole);
    let full = decode(&whole, DecodeOptions::default()).unwrap();
    for (i, &start) in starts.iter().enumerate().skip(2) {
        // a few bytes into the scan's data, and inside the segments just before it
        for cut in [start + 20, start - 1, start - 3, start + 5] {
            let h = header_of(&whole[..cut]);
            assert!(h.truncated, "cut at {cut}");
            let d = decode(&whole[..cut], DecodeOptions::default()).unwrap_or_else(|e| panic!("cut at {cut}: {e}"));
            // (the first scans hold only the DC of each component: a blur)
            if i >= 5 {
                assert!(mean_abs_diff(&d, &full) < 0.2, "cut at {cut}");
            }
        }
    }
}

#[test]
fn a_cut_file_also_decodes_scaled() {
    // photos decode through DCT scaling (a different decoder) for thumbnails and previews
    for whole in [baseline(), baseline_restarts(), progressive()] {
        let cut = &whole[..whole.len() * 3 / 4];
        let d = decode(cut, DecodeOptions::fit(64, 64)).unwrap_or_else(|e| panic!("{e}"));
        assert!(d.image.width <= 64 && d.image.height <= 64 && d.image.width >= 32, "{}x{}", d.image.width, d.image.height);
        assert_eq!((d.source_width, d.source_height), (W as u32, H as u32));
        let t = decode_thumbnail(cut, 64).unwrap();
        assert!(t.image.width <= 64 && t.image.height <= 64);
    }
}

#[test]
fn a_file_with_nothing_to_show_stays_refused() {
    let base = baseline();
    let sos = scan_starts(&base)[0];
    let header_len = 2 + u16::from_be_bytes([base[sos + 2], base[sos + 3]]) as usize;
    // before the scan header, in it, and right after it (no image data at all)
    for cut in [2, 20, sos - 1, sos, sos + 5, sos + header_len] {
        assert!(read_header(&base[..cut]).is_err(), "baseline cut at {cut}");
    }
    let prog = progressive();
    let starts = scan_starts(&prog);
    // progressive: nothing until a whole scan is there (this encoder's first one is the DC of the luma)
    for cut in [starts[0], starts[0] + 20, starts[1] - 5, starts[1] - 1] {
        assert!(read_header(&prog[..cut]).is_err(), "progressive cut at {cut}");
    }
    assert!(read_header(&prog[..starts[1] + 2]).is_ok(), "one whole scan is enough");
    // frames we can't read cut short either way
    for sof in [0xC9u8, 0xC5] {
        let mut b = base.clone();
        let p = b.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
        b[p + 1] = sof;
        b.truncate(b.len() / 2);
        assert!(read_header(&b).is_err(), "SOF {sof:#x}");
    }
}

#[test]
fn what_decoders_could_not_fill_in_stays_refused() {
    // non-interleaved scans (one component each) cut in the first scan: the other components have no data
    let b = encode(false, true, jpeg_encoder::SamplingFactor::R_4_2_0);
    let starts = scan_starts(&b);
    assert_eq!(starts.len(), 3, "one scan per component");
    assert!(read_header(&b[..starts[1] - 1]).is_err());
    // 4 components (CMYK): not read from a cut file
    let cmyk: Vec<u8> = (0..W * H * 4).map(|i| (i * 7 % 256) as u8).collect();
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, 85).encode(&cmyk, W as u16, H as u16, jpeg_encoder::ColorType::Cmyk).unwrap();
    assert!(read_header(&out).is_ok(), "the whole file is fine");
    assert!(read_header(&out[..out.len() * 2 / 3]).is_err());
}

#[test]
fn bytes_after_the_end_of_image_are_not_damage() {
    let mut b = baseline();
    b.extend_from_slice(&[0; 100]);
    assert!(!header_of(&b).truncated);
    b.truncate(baseline().len() - 2);
    assert!(header_of(&b).truncated, "the end-of-image marker is gone");
}

/// No cut point of any of them may panic, and a header that accepts a cut promises a decode (full
/// size and scaled) that gives a full-size picture.
#[test]
fn every_cut_point_is_an_error_or_a_picture() {
    let files = [
        ("baseline", baseline()),
        ("restarts", baseline_restarts()),
        ("short restarts", baseline_short_restarts()),
        ("grey", grey(false)),
        ("grey progressive", grey(true)),
        ("progressive", progressive()),
    ];
    for (name, whole) in files {
        let n = whole.len();
        let mut accepted = 0;
        for cut in 0..=n {
            let part = &whole[..cut];
            let Ok(h) = read_header(part) else { continue };
            accepted += 1;
            assert_eq!((h.width, h.height), (W as u32, H as u32), "{name} cut at {cut}");
            assert_eq!(h.truncated, cut < n, "{name} cut at {cut}/{n}");
            let d = decode(part, DecodeOptions::default()).unwrap_or_else(|e| panic!("{name} cut at {cut}/{n}: {e}"));
            assert_eq!((d.image.width, d.image.height), (W, H), "{name} cut at {cut}");
            let s = decode(part, DecodeOptions::fit(64, 64)).unwrap_or_else(|e| panic!("{name} scaled, cut at {cut}/{n}: {e}"));
            assert_eq!(s.source_width, W as u32, "{name} cut at {cut}");
        }
        assert!(accepted > n / 3, "{name}: {accepted} of {n}");
    }
}

/// A file cut short that carries metadata (EXIF orientation, ICC) keeps it.
#[test]
fn a_cut_file_keeps_its_orientation() {
    let rgb = pixels();
    let exif = lightcraft_codecs::exif::minimal_exif(6);
    let meta = EncodeMeta { exif: Some(&exif), ..Default::default() };
    let whole = encode_jpeg(&EncodeImage::new(W as u32, H as u32, 3, Samples::U8(&rgb)), 85, ChromaSubsampling::S420, &meta).unwrap();
    let cut = &whole[..whole.len() * 2 / 3];
    let h = header_of(cut);
    assert_eq!((h.orientation, h.truncated), (6, true));
    assert_eq!(decode(cut, DecodeOptions::default()).unwrap().orientation, 6);
}

/// A download that reserved the file's size and filled it only so far ends in zeros: they are not
/// picture data, and the rest is as flat grey as for a file that simply stops.
#[test]
fn a_zero_filled_tail_is_not_picture_data() {
    for whole in [baseline(), baseline_short_restarts()] {
        let cut = whole.len() * 6 / 10;
        let mut padded = whole[..cut].to_vec();
        padded.resize(whole.len(), 0);
        let (plain, zeros) = (decode(&whole[..cut], DecodeOptions::default()).unwrap(), decode(&padded, DecodeOptions::default()).unwrap());
        assert!(header_of(&padded).truncated);
        assert_eq!(mean_abs_diff(&plain, &zeros), 0.0);
        let last = row(&zeros, H - 1);
        assert!(last.iter().all(|p| *p == last[0] && (p[0] - p[1]).abs() < 1e-3), "{:?}", last[0]);
    }
    let prog = progressive();
    let starts = scan_starts(&prog);
    let mut padded = prog[..starts[6] + 40].to_vec();
    padded.resize(prog.len(), 0);
    assert!(header_of(&padded).truncated);
    assert!(decode(&padded, DecodeOptions::default()).is_ok());
}

/// A file with a bad segment length inside its data still ends with its end-of-image marker: it is
/// damaged, not cut short, and read (or refused) as it always was.
#[test]
fn a_damaged_complete_file_is_not_called_cut_short() {
    let mut b = baseline();
    let sos = scan_starts(&b)[0];
    let at = sos + 2 + u16::from_be_bytes([b[sos + 2], b[sos + 3]]) as usize + 20;
    b[at..at + 4].copy_from_slice(&[0xFF, 0xC4, 0xFF, 0xFF]);
    assert!(b.ends_with(&[0xFF, 0xD9]));
    assert!(read_header(&b).is_err());
}

/// A thumbnail of a big file cut short is not built from a full-size float image: the decode is
/// reduced on the way, and still covers the box asked for.
#[test]
fn a_big_cut_file_decodes_to_a_thumbnail_size() {
    let (w, h) = (1280usize, 768usize);
    let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 13 % 251) as u8).collect();
    let whole =
        encode_jpeg(&EncodeImage::new(w as u32, h as u32, 3, Samples::U8(&rgb)), 80, ChromaSubsampling::S420, &EncodeMeta::default()).unwrap();
    let cut = &whole[..whole.len() / 2];
    let d = decode(cut, DecodeOptions::fit(256, 256)).unwrap();
    assert_eq!((d.source_width, d.source_height), (w as u32, h as u32));
    assert_eq!(d.image.width, 256);
    assert!(d.image.height >= 150 && d.image.height <= 154, "{}", d.image.height);
}
