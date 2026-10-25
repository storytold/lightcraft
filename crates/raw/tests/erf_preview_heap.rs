//! Peak heap of measuring an Epson ERF's white balance from its embedded preview (`crates/raw/src/wb_estimate.rs`),
//! measured with dhat's testing mode. The header probe of every ERF decodes that preview, so a JPEG whose frame
//! header claims far more pixels than the measurement reads must be refused from that header, before the frame it
//! declares is allocated (issue #2560). One test per binary (dhat's profiler is global).

use lightcraft_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value, tags as t};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// A 16 × 8 baseline JPEG whose SOF0 header declares a `w` × `h` frame; the coded data stays that of 16 × 8.
fn jpeg_claiming(w: u16, h: u16) -> Vec<u8> {
    let px: Vec<u8> = (0..16 * 8).flat_map(|i| [120u8, (i % 16 * 15) as u8, 200]).collect();
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, 90).encode(&px, 16, 8, jpeg_encoder::ColorType::Rgb).unwrap();
    let sof = jpeg.windows(2).position(|m| m == [0xFF, 0xC0]).unwrap();
    jpeg[sof + 5..sof + 7].copy_from_slice(&h.to_be_bytes());
    jpeg[sof + 7..sof + 9].copy_from_slice(&w.to_be_bytes());
    jpeg
}

/// A 20 × 6 Epson ERF (a `BLOCKS16` CFA sub-IFD, as `vendor::erf` reads it) followed by `preview`, stored the way
/// Epson stores it: its first byte as `ee`, found by the structural scan in `preview::scan_for_jpeg`.
fn erf_with_preview(preview: &[u8]) -> Vec<u8> {
    let (w, h) = (20u32, 6u32);
    let px: Vec<u16> = (0..w * h).map(|i| (i * 37 % 4096) as u16).collect();
    // ten 12-bit samples MSB first in 15 bytes, then a pad byte
    let mut blocks = Vec::new();
    for g in px.chunks(10) {
        for p in g.chunks(2) {
            let (a, b) = (p[0], p[1]);
            blocks.extend_from_slice(&[(a >> 4) as u8, ((a & 15) << 4) as u8 | (b >> 8) as u8, b as u8]);
        }
        blocks.push(0);
    }
    let mut raw = IfdBuilder::new();
    raw.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
    raw.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
    raw.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
    raw.set(t::BITS_PER_SAMPLE, Value::Short(vec![12]));
    raw.set(t::COMPRESSION, Value::Short(vec![32769]));
    raw.set(t::PHOTOMETRIC, Value::Short(vec![t::photometric::CFA]));
    raw.set(t::CFA_REPEAT_PATTERN_DIM, Value::Short(vec![2, 2]));
    raw.set(t::CFA_PATTERN_EP, Value::Byte(vec![0, 1, 1, 2]));
    raw.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![blocks] });
    let mut ifd0 = IfdBuilder::new();
    ifd0.set(t::MAKE, Value::Ascii("SEIKO EPSON CORP.".into()));
    ifd0.set(t::MODEL, Value::Ascii("R-D1".into()));
    ifd0.add_sub_ifd(raw);
    let mut file = TiffWriter::new(ByteOrder::Big, false).write(&[ifd0]).unwrap();
    let at = file.len();
    file.extend_from_slice(preview);
    file[at] = 0xee;
    file
}

#[test]
fn erf_preview_claiming_a_huge_frame_costs_no_memory_sized_by_it() {
    // 8192 × 8192: 192 MiB of RGB if it were decoded, four times the pixels the measurement reads
    let preview = jpeg_claiming(8192, 8192);
    let file = erf_with_preview(&preview);
    assert_eq!(lightcraft_raw::embedded_preview(&file), Some(preview), "the scan finds the preview");
    let _profiler = dhat::Profiler::builder().testing().build();
    let info = lightcraft_raw::probe_info(&file).unwrap();
    let peak = dhat::HeapStats::get().max_bytes;
    // the preview is not used; the white balance still comes from the mosaic (grey world)
    assert!(info.wb_multipliers.is_some(), "no white balance");
    eprintln!("peak heap {peak} bytes probing a {} byte file", file.len());
    assert!(peak < 2 << 20, "peak heap {peak} bytes probing a {} byte file", file.len());
}
