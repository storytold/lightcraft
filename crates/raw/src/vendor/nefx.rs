//! Nikon NEFX: the merged pixel-shift image NX Studio writes from a series of 4, 8, 16 or 32 shots.
//!
//! Clean room: the layout below was measured from five files of one body (Z 8: a 16-shot merge, 16512 × 11008, from
//! NX Studio 1.7 and 1.10; the 4-, 8- and 32-shot merges and other bodies are not verified), by a TIFF structure dump
//! and by checking candidate packings against byte counts and neighbouring-sample smoothness. No other decoder's
//! source was read for this format.
//!
//! * The file is a NEF: IFD0 (a 160 × 120 JPEG thumbnail, orientation, `Software` "NX Studio …"), SubIFDs with JPEG
//!   previews, and a raw SubIFD, plus Nikon's maker note.
//! * The raw SubIFD is already demosaiced: three samples per pixel, 14 bits each, photometric RGB (it also keeps the
//!   CFA tags of a camera NEF, which don't describe it). Its compression is 34713, but the data isn't compressed: one
//!   strip of exactly `width * height * 42 / 8` bytes.
//! * The strip is one LSB-first bit stream, R G B per pixel, rows of `width * 42 / 8` bytes with no padding (the
//!   measured width is a multiple of 4, so a row ends on a byte; any other width is refused, not guessed).
//! * The samples are linear camera values on the sensor's own scale: the black level (maker note `0x003d`, 1008 for
//!   the Z 8) is still in them and the as-shot white balance (`0x000c`) is the camera's, so the colour model of the
//!   camera's NEFs applies. The merge averages shots, so highlights clip anywhere between about 16360 and 16383
//!   (green in some files, blue exactly at 16383 in others): the white level is the 14-bit maximum, not measured.
//! * The raw SubIFD carries Nikon's distortion block (`0xc7d5`) as a camera NEF does, and it applies at the merged size
//!   as it is: the polynomial is normalised by the image area, and the optical-centre offset (entry `0x0008`) is
//!   already in merged pixels (a NEFX holds exactly twice the offset of a frame of its series, which is in sensor
//!   pixels), so [`super::nef_lens::distortion`] needs no scaling.
//!
//! The result is an ordinary three-sample [`RawImage`] (no CFA), which skips demosaicing like a LinearRaw DNG.

use super::nef::{BLACK_LEVEL, WB_RB_LEVELS, default_crop};
use crate::{BlackLevel, ColorData, MAX_SAMPLES, Mode, OpcodeLists, RawData, RawError, RawFormat, RawImage, Rect, Result};
use lcc_geom::Orientation;
use lcc_tiff::tags::{self as t, photometric};
use lcc_tiff::{Ifd, Tiff, makernote};
use rayon::prelude::*;

const BITS: u32 = 14;
/// The 14-bit maximum (see the module docs: the merged highlights clip below it by a varying amount).
const WHITE: f32 = 16383.0;

/// Whether `ifd` has the NEFX raw image's signature: RGB samples of 14 bits each in a Nikon "34713" strip.
fn is_nefx_ifd(ifd: &Ifd) -> bool {
    ifd.u16(t::PHOTOMETRIC) == Some(photometric::RGB)
        && ifd.u16(t::SAMPLES_PER_PIXEL) == Some(3)
        && ifd.u16(t::COMPRESSION) == Some(t::compression::NIKON)
        && ifd.u64s(t::BITS_PER_SAMPLE).as_deref() == Some(&[14, 14, 14][..])
}

/// The raw IFD of a NEFX (the largest with its signature).
pub(super) fn raw_ifd(tiff: &Tiff) -> Option<&Ifd> {
    tiff.all_ifds()
        .into_iter()
        .filter(|i| is_nefx_ifd(i))
        .max_by_key(|i| i.u64(t::IMAGE_WIDTH).unwrap_or(0).saturating_mul(i.u64(t::IMAGE_LENGTH).unwrap_or(0)))
}

/// Bytes per row: 14 bits × 3 samples = 42 bits a pixel, so four pixels are 21 bytes. `None` for a width that isn't a
/// whole number of such groups (not measured).
fn row_stride(width: usize) -> Option<usize> {
    (width > 0 && width.is_multiple_of(4)).then_some(width / 4)?.checked_mul(21)
}

pub(super) fn decode(bytes: &[u8], tiff: &Tiff, raw: &Ifd, mode: Mode) -> Result<RawImage> {
    let ifd0 = tiff.ifds.first().ok_or_else(|| RawError::Corrupt("NEFX: no IFD".into()))?;
    let info = raw.image()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let total = w.checked_mul(h).and_then(|n| n.checked_mul(3)).filter(|&n| n > 0 && n <= MAX_SAMPLES).ok_or(RawError::Limit("image too large"))?;
    let stride = row_stride(w).ok_or_else(|| RawError::Unsupported(format!("NEFX of width {w} (the packing is measured for multiples of 4)")))?;
    let expected = stride.checked_mul(h).ok_or(RawError::Limit("image too large"))?;
    let chunks = info.chunks(bytes.len() as u64);
    let [chunk] = chunks.as_slice() else {
        return Err(RawError::Unsupported(format!("NEFX in {} strips or tiles (one strip is measured)", chunks.len())));
    };
    if info.planar != 1 || usize::try_from(chunk.len).ok() != Some(expected) {
        return Err(RawError::Unsupported(format!("NEFX strip of {} bytes for {w} x {h} (the measured packing needs {expected})", chunk.len)));
    }
    // the whole strip must be in the file, in either mode: a cut-off file is damaged, not a shorter picture
    let src = lcc_tiff::image::chunk_bytes(bytes, chunk)
        .filter(|s| s.len() == expected)
        .ok_or_else(|| RawError::Corrupt("NEFX: image data outside the file".into()))?;
    let data = match mode {
        Mode::Header => Vec::new(),
        Mode::Full => {
            let mut out: Vec<u16> = Vec::new();
            out.try_reserve_exact(total).map_err(|_| RawError::Limit("not enough memory to decode this raw"))?;
            out.resize(total, 0);
            // row by row straight from the file's bytes into the output: no second copy of the strip
            out.par_chunks_mut(w * 3).zip(src.par_chunks(stride)).for_each(|(row, line)| crate::unpack::unpack_lsb(line, BITS, row));
            out
        }
    };

    let make = ifd0.string(t::MAKE).unwrap_or_default();
    let model = ifd0.string(t::MODEL).unwrap_or_default();
    let mn =
        tiff.exif().and_then(|e| e.get(t::MAKER_NOTE)).and_then(|e| makernote::parse_makernote(bytes, e.offset, e.count() as u64, tiff.order, &make));
    // maker note 0x003d: R, G1, G2, B (14-bit units, which these 14-bit samples are)
    let black = match mn.as_ref().and_then(|m| m.ifd.f64s(BLACK_LEVEL)).as_deref() {
        Some([r, g1, g2, b]) if [r, g1, g2, b].iter().all(|v| (0.0..16384.0).contains(*v)) => {
            vec![*r as f32, ((*g1 + *g2) / 2.0) as f32, *b as f32]
        }
        _ => vec![0.0; 3],
    };
    let black = BlackLevel { repeat_rows: 1, repeat_cols: 1, values: black, ..Default::default() };
    let wb = mn.as_ref().and_then(|m| m.ifd.f64s(WB_RB_LEVELS)).and_then(|v| match v.as_slice() {
        [r, b, ..] if *r > 0.1 && *r < 10.0 && *b > 0.1 && *b < 10.0 => Some([*r as f32, 1.0, *b as f32]),
        _ => None,
    });
    let active = Rect::new(0, 0, w, h);
    let crop = default_crop(mn.as_ref(), w, h);
    let mut metadata = lcc_meta::from_tiff(tiff);
    metadata.width = Some(crop.width as u32);
    metadata.height = Some(crop.height as u32);
    let img = RawImage {
        format: RawFormat::Nef,
        width: w,
        height: h,
        cpp: 3,
        data: RawData::U16(data),
        cfa: None,
        bits: BITS,
        black,
        white: vec![WHITE],
        active_area: active,
        crop,
        orientation: Orientation::from_exif(ifd0.u16(t::ORIENTATION).unwrap_or(1)),
        color: ColorData { kelvin_presets: crate::kelvin_builtin::find(Some(&make), &model), ..Default::default() },
        wb_multipliers: wb,
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: OpcodeLists { list3: super::nef_lens::distortion(&model, raw, active, crop).into_iter().collect(), ..Default::default() },
        metadata,
        lens_metadata: Default::default(),
    };
    img.validate_for(mode)?;
    Ok(img)
}

#[cfg(test)]
pub(crate) mod tests {
    //! Synthetic NEFX-like files: the structure measured in merged pixel-shift files from NX Studio (see the decoder's
    //! module docs), at a size a test can afford.
    use crate::{RawData, RawFormat};
    use lcc_tiff::tags::{self as t, photometric};
    use lcc_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value};

    /// Samples spread over the whole 14-bit range, with both extremes in the first row.
    pub(crate) fn samples(w: usize, h: usize) -> Vec<u16> {
        let mut v: Vec<u16> = (0..w * h * 3).map(|i| ((i.wrapping_mul(2_654_435_761) >> 7) % 16384) as u16).collect();
        v[0] = 0;
        v[1] = 16383;
        if let Some(last) = v.last_mut() {
            *last = 16383;
        }
        v
    }

    /// LSB-first 14-bit stream, R G B per pixel, rows byte aligned (`w` a multiple of 4: no padding).
    pub(crate) fn pack(samples: &[u16], w: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for row in samples.chunks(w * 3) {
            let (mut acc, mut n) = (0u64, 0u32);
            for &s in row {
                acc |= u64::from(s) << n;
                n += 14;
                while n >= 8 {
                    out.push(acc as u8);
                    acc >>= 8;
                    n -= 8;
                }
            }
            if n > 0 {
                out.push(acc as u8);
            }
        }
        out
    }

    /// The raw SubIFD of a NEFX: three 14-bit samples per pixel, "compressed" 34713 but exactly packed, photometric RGB,
    /// with the CFA tags the camera's NEFs carry.
    pub(crate) fn raw_ifd(w: u32, h: u32, strip: Vec<u8>) -> IfdBuilder {
        let mut raw = IfdBuilder::new();
        raw.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
        raw.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
        raw.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
        raw.set(t::BITS_PER_SAMPLE, Value::Short(vec![14, 14, 14]));
        raw.set(t::COMPRESSION, Value::Short(vec![t::compression::NIKON]));
        raw.set(t::PHOTOMETRIC, Value::Short(vec![photometric::RGB]));
        raw.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![3]));
        raw.set(t::PLANAR_CONFIGURATION, Value::Short(vec![1]));
        raw.set(t::CFA_REPEAT_PATTERN_DIM, Value::Short(vec![2, 2]));
        raw.set(t::CFA_PATTERN_EP, Value::Byte(vec![0, 1, 1, 2]));
        raw.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![strip] });
        raw
    }

    /// A Z 8 NEFX of `w` × `h` pixels holding `data`: IFD0 with the camera, orientation and software, a maker note with
    /// as-shot white balance, black level and image area, and the raw SubIFD.
    pub(crate) fn nefx(w: usize, h: usize, data: &[u16]) -> Vec<u8> {
        nefx_with(raw_ifd(w as u32, h as u32, pack(data, w)), w, h)
    }

    pub(crate) fn nefx_with(raw: IfdBuilder, w: usize, h: usize) -> Vec<u8> {
        let order = ByteOrder::Little;
        let mut mn = IfdBuilder::new();
        mn.set(0x000c, Value::Rational(vec![(948, 512), (730, 512), (512, 512), (512, 512)]));
        mn.set(0x001b, Value::Short(vec![11, 8280, 5520, 8280, 5520, 0, 0]));
        mn.set(0x003d, Value::Short(vec![1008, 1008, 1008, 1008]));
        mn.set(0x0045, Value::Short(vec![0, 0, w as u16, h as u16]));
        let mut note = b"Nikon\0\x02\x11\0\0".to_vec();
        note.extend(TiffWriter::new(order, false).write(&[mn]).unwrap());
        let mut exif = IfdBuilder::new();
        exif.set(t::MAKER_NOTE, Value::Undefined(note));
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii("NIKON CORPORATION".into()));
        ifd0.set(t::MODEL, Value::Ascii("NIKON Z 8".into()));
        ifd0.set(t::ORIENTATION, Value::Short(vec![8]));
        ifd0.set(t::SOFTWARE, Value::Ascii("NX Studio 1.10 W".into()));
        ifd0.set_child(t::EXIF_IFD, exif);
        ifd0.add_sub_ifd(raw);
        TiffWriter::new(order, false).write(&[ifd0]).unwrap()
    }

    /// Step 1 of a NEFX's way through the library: does `probe` call it a raw?
    #[test]
    fn a_nefx_is_probed_as_a_nikon_raw() {
        let (w, h) = (16, 6);
        let bytes = nefx(w, h, &samples(w, h));
        assert_eq!(crate::probe(&bytes), Some(RawFormat::Nef));
    }

    /// Step 2: does the decoder give the three-channel samples and the file's levels, white balance and orientation?
    #[test]
    fn a_nefx_decodes_to_three_channel_linear_samples() {
        let (w, h) = (16, 6);
        let data = samples(w, h);
        let img = crate::decode(&nefx(w, h, &data)).expect("a NEFX decodes");
        assert_eq!((img.format, img.width, img.height, img.cpp, img.bits), (RawFormat::Nef, w, h, 3, 14));
        assert_eq!(img.data, RawData::U16(data));
        assert!(img.cfa.is_none(), "the samples are not a mosaic");
        assert_eq!(img.white, vec![16383.0]);
        for s in 0..3 {
            assert_eq!(img.black.at(3, 2, s, 3), 1008.0);
        }
        let wb = img.wb_multipliers.expect("as-shot white balance");
        assert!((wb[0] - 948.0 / 512.0).abs() < 1e-4 && wb[1] == 1.0 && (wb[2] - 730.0 / 512.0).abs() < 1e-4, "{wb:?}");
        assert_eq!(img.orientation, lcc_geom::Orientation::from_exif(8));
        assert_eq!((img.active_area.width, img.active_area.height), (w, h));
    }

    #[test]
    fn the_header_probe_has_the_full_decodes_geometry_and_no_samples() {
        let (w, h) = (16, 6);
        let bytes = nefx(w, h, &samples(w, h));
        let full = crate::decode(&bytes).unwrap();
        let info = crate::probe_info(&bytes).unwrap();
        assert_eq!(info, full.info());
        assert_eq!((info.cpp, info.width, info.height), (3, w, h));
    }

    #[test]
    fn a_wider_nefx_keeps_every_row_in_place() {
        // 16512 wide is the measured size; the rows are 86688 bytes: check a few rows of that width
        let (w, h) = (16512, 3);
        let data = samples(w, h);
        let img = crate::decode(&nefx(w, h, &data)).unwrap();
        assert_eq!(img.data, RawData::U16(data));
    }

    /// The raw SubIFD of a good 16 × 6 file with `edit` applied, as a whole file.
    fn edited(edit: impl FnOnce(&mut IfdBuilder)) -> Vec<u8> {
        let (w, h) = (16usize, 6usize);
        let mut raw = raw_ifd(w as u32, h as u32, pack(&samples(w, h), w));
        edit(&mut raw);
        nefx_with(raw, w, h)
    }

    #[test]
    fn layouts_that_were_not_measured_are_refused_with_a_reason() {
        let strip = |n: usize| ImageData::Strips { rows_per_strip: 6, strips: vec![vec![0x55; n]] };
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("a strip one byte short", edited(|r| _ = r.set_image(strip(16 * 6 * 42 / 8 - 1)))),
            ("a strip one byte long", edited(|r| _ = r.set_image(strip(16 * 6 * 42 / 8 + 1)))),
            ("two strips", edited(|r| _ = r.set_image(ImageData::Strips { rows_per_strip: 3, strips: vec![vec![0; 252]; 2] }))),
            ("a width that is not a multiple of 4", edited(|r| _ = r.set(t::IMAGE_WIDTH, Value::Long(vec![15])))),
            ("zero width", edited(|r| _ = r.set(t::IMAGE_WIDTH, Value::Long(vec![0])))),
            ("planar samples", edited(|r| _ = r.set(t::PLANAR_CONFIGURATION, Value::Short(vec![2])))),
            ("12-bit samples", edited(|r| _ = r.set(t::BITS_PER_SAMPLE, Value::Short(vec![12, 12, 12])))),
            ("four samples", edited(|r| _ = r.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![4])))),
            ("dimensions past every limit", edited(|r| _ = r.set(t::IMAGE_WIDTH, Value::Long(vec![0xFFFF_FFFC])))),
            ("dimensions that overflow", edited(|r| _ = r.set(t::IMAGE_LENGTH, Value::Long(vec![u32::MAX])))),
        ];
        for (what, bytes) in cases {
            for mode in [crate::decode(&bytes).map(|_| ()), crate::probe_info(&bytes).map(|_| ())] {
                assert!(mode.is_err(), "{what} must be refused");
            }
        }
        // the layout that is measured is not refused
        assert!(crate::decode(&edited(|_| {})).is_ok());
    }

    /// Whatever the damage, decoding gives an error or a picture of the declared size, never a panic.
    fn survives(bytes: &[u8]) {
        if let Ok(img) = crate::decode(bytes)
            && let RawData::U16(d) = &img.data
        {
            assert_eq!(d.len(), img.width * img.height * img.cpp);
        }
        let _ = crate::probe_info(bytes);
        let _ = crate::probe(bytes);
    }

    #[test]
    fn truncated_and_corrupted_nefx_files_fail_cleanly() {
        let (w, h) = (16, 6);
        let good = nefx(w, h, &samples(w, h));
        // every prefix of the file: the strip or a directory is cut off
        for n in 0..good.len() {
            let cut = good.get(..n).unwrap_or(&good);
            survives(cut);
            if n < good.len() {
                // a prefix that loses the image data is an error, in both modes
                if let Ok(img) = crate::decode(cut) {
                    assert_eq!(img.data, RawData::U16(samples(w, h)), "a cut file that still decodes holds the whole image");
                }
            }
        }
        // every byte flipped, then every byte zeroed, in the directories and the maker note (the strip is data)
        for i in 0..good.len() {
            for fill in [0xFFu8, 0x00] {
                let mut bad = good.clone();
                if let Some(b) = bad.get_mut(i) {
                    *b = fill;
                }
                survives(&bad);
            }
        }
        // a strip offset and byte count that point far past the end
        let tiff = lcc_tiff::Tiff::parse(&good).unwrap();
        let raw = super::raw_ifd(&tiff).unwrap();
        for tag in [t::STRIP_OFFSETS, t::STRIP_BYTE_COUNTS] {
            let at = raw.get(tag).unwrap().offset as usize;
            for v in [u32::MAX, u32::MAX - 20, 0] {
                let mut bad = good.clone();
                if let Some(slot) = bad.get_mut(at..at + 4) {
                    slot.copy_from_slice(&v.to_le_bytes());
                }
                survives(&bad);
            }
        }
    }

    #[test]
    fn a_cut_off_strip_is_corrupt_not_a_shorter_picture() {
        let (w, h) = (16, 6);
        let good = nefx(w, h, &samples(w, h));
        let tiff = lcc_tiff::Tiff::parse(&good).unwrap();
        let raw = super::raw_ifd(&tiff).unwrap();
        let chunk = raw.image().unwrap().chunks(good.len() as u64)[0];
        // keep everything up to the middle of the strip
        let cut = &good[..chunk.offset as usize + chunk.len as usize / 2];
        for r in [crate::decode(cut).map(|_| ()), crate::probe_info(cut).map(|_| ())] {
            // (the TIFF reader itself may already refuse a strip that leaves the file: then it isn't called a raw at all)
            assert!(matches!(r, Err(crate::RawError::Corrupt(_) | crate::RawError::NotRaw)), "{r:?}");
        }
    }

    /// A NEFX whose raw SubIFD carries Nikon's distortion block and centre offset.
    fn with_lens(offset: (f64, f64)) -> Vec<u8> {
        use super::super::nef_lens::tests::{distortion_block, nef_info_blob, offset_block};
        let blob =
            nef_info_blob(&[(0x0005, distortion_block(1, 21.6, 4, [0.0399, 0.0192, -0.1272], -0.025)), (0x0008, offset_block(offset.0, offset.1))]);
        let (w, h) = (24usize, 16usize);
        let mut raw = raw_ifd(w as u32, h as u32, pack(&samples(w, h), w));
        raw.set(0xc7d5, Value::Undefined(blob));
        nefx_with(raw, w, h)
    }

    #[test]
    fn the_distortion_correction_applies_at_the_merged_size_with_the_stored_optical_centre() {
        let warp = |img: &crate::RawImage| match img.opcodes.list3.as_slice() {
            [crate::Opcode::WarpRectilinear { planes, center }] => Some((planes.clone(), *center)),
            _ => None,
        };
        let centred = warp(&crate::decode(&with_lens((0.0, 0.0))).unwrap()).expect("the in-file distortion becomes a warp at the merged size");
        // an offset (in merged pixels, as NX Studio writes it) moves the warp's centre and still warps
        let shifted = warp(&crate::decode(&with_lens((0.4, -0.3))).unwrap()).expect("an off-centre optical centre still warps");
        assert_eq!(centred.0.len(), shifted.0.len());
        assert!((centred.1[0] - 0.5).abs() < 1e-9 && (centred.1[1] - 0.5).abs() < 1e-9, "{:?}", centred.1);
        assert!(shifted.1[0] > 0.5 && shifted.1[1] < 0.5, "x moves right, y up: {:?}", shifted.1);
        // no distortion block, no warp
        let plain = crate::decode(&nefx(16, 6, &samples(16, 6))).unwrap();
        assert!(plain.opcodes.list3.is_empty());
    }

    #[test]
    fn a_tiff_export_from_nx_studio_is_not_a_raw() {
        // 16-bit RGB, uncompressed, Software NX Studio: what "Save as TIFF" writes
        let mut ifd = IfdBuilder::new();
        ifd.set(t::MAKE, Value::Ascii("NIKON CORPORATION".into()));
        ifd.set(t::MODEL, Value::Ascii("NIKON Z 8".into()));
        ifd.set(t::SOFTWARE, Value::Ascii("NX Studio 1.10 W".into()));
        ifd.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
        ifd.set(t::IMAGE_WIDTH, Value::Long(vec![4]));
        ifd.set(t::IMAGE_LENGTH, Value::Long(vec![4]));
        ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![16, 16, 16]));
        ifd.set(t::COMPRESSION, Value::Short(vec![1]));
        ifd.set(t::PHOTOMETRIC, Value::Short(vec![photometric::RGB]));
        ifd.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![3]));
        ifd.set_image(ImageData::Strips { rows_per_strip: 4, strips: vec![vec![0x40; 4 * 4 * 6]] });
        let bytes = TiffWriter::new(ByteOrder::Little, false).write(&[ifd]).unwrap();
        assert_eq!(crate::probe(&bytes), None);
    }
}
