//! Photoshop PSD/PSB merged-composite reader (clean-room, from the public file-format description):
//! 8/16/32-bit gray, RGB, CMYK, indexed, duotone (as gray); raw or PackBits/RLE image data;
//! ICC (resource 1039), EXIF (1058), XMP (1060); transparency when the layer count is negative.

use crate::convert::{Buf, Meta, Model, Raw, check_size, finish};
use crate::{DecodeOptions, Decoded, Error, Format, Result};

const F: Format = Format::Psd;

fn err(s: &str) -> Error {
    Error::Malformed(F, s.to_string())
}

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.p.checked_add(n).filter(|&e| e <= self.b.len()).ok_or_else(|| err("truncated"))?;
        let s = &self.b[self.p..end];
        self.p = end;
        Ok(s)
    }
    fn u16(&mut self) -> Result<u16> {
        let s = self.take(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Result<u32> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn u64(&mut self) -> Result<u64> {
        let s = self.take(8)?;
        Ok(u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
    }
}

pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Decoded> {
    let mut r = Rd { b: bytes, p: 0 };
    if r.take(4)? != b"8BPS" {
        return Err(err("bad signature"));
    }
    let version = r.u16()?;
    let psb = match version {
        1 => false,
        2 => true,
        _ => return Err(err("bad version")),
    };
    r.take(6)?;
    let channels = r.u16()? as usize;
    let height = r.u32()? as usize;
    let width = r.u32()? as usize;
    let depth = r.u16()?;
    let mode = r.u16()?;
    check_size(F, width as u64, height as u64, opts)?;
    if channels == 0 || channels > 56 {
        return Err(err("bad channel count"));
    }

    // Colour mode data (palette for indexed).
    let cm_len = r.u32()? as usize;
    let color_mode_data = r.take(cm_len)?;

    // Image resources.
    let res_len = r.u32()? as usize;
    let res = r.take(res_len)?;
    let (icc, exif, xmp) = resources(res);

    // Layer and mask information: only the layer count sign matters (merged transparency).
    let lm_len = if psb { r.u64()? } else { u64::from(r.u32()?) };
    let lm = r.take(usize::try_from(lm_len).map_err(|_| err("truncated"))?)?;
    let merged_transparency = if lm.len() >= if psb { 10 } else { 6 } {
        let off = if psb { 8 } else { 4 };
        let count = i16::from_be_bytes([lm[off], lm[off + 1]]);
        count < 0
    } else {
        false
    };

    // Image data.
    let compression = r.u16()?;
    let bpc = match depth {
        8 => 1usize,
        16 => 2,
        32 => 4,
        1 => return Err(Error::Unsupported(F, "1-bit bitmap PSD")),
        _ => return Err(err("bad depth")),
    };
    let (model, n_color) = match mode {
        1 | 8 => (Model::Gray, 1),
        2 => (Model::Rgb, 1),
        3 => (Model::Rgb, 3),
        4 => (Model::Cmyk, 4),
        7 => (Model::Gray, 1),
        9 => return Err(Error::Unsupported(F, "Lab PSD")),
        _ => return Err(err("unknown colour mode")),
    };
    if channels < n_color {
        return Err(err("too few channels"));
    }
    let alpha = merged_transparency && channels > n_color && mode != 2;
    // Only the colour (+ alpha) planes are decoded; extra channels (spot colours, masks) are
    // stored after them and never touched.
    let used = n_color + alpha as usize;
    let too_large = || Error::TooLarge(width as u64, height as u64);
    let n = width.checked_mul(height).ok_or_else(too_large)?;
    let row_bytes = width.checked_mul(bpc).ok_or_else(too_large)?;
    let plane = n.checked_mul(bpc).ok_or_else(too_large)?;
    let mut planes: Vec<Vec<u8>> = Vec::with_capacity(used);
    match compression {
        0 => {
            // Raw planes are borrowed from the input, so their size is bounded by the file.
            for _ in 0..used {
                planes.push(r.take(plane)?.to_vec());
            }
        }
        1 => {
            // Row byte-count table: one entry per row per channel (all channels, since the data
            // follows it). Taken from the input before anything is allocated from it.
            let entry = if psb { 4 } else { 2 };
            let table_len = channels.checked_mul(height).and_then(|c| c.checked_mul(entry)).ok_or_else(|| err("truncated"))?;
            let table = r.take(table_len)?;
            let count = |i: usize| -> usize {
                let at = i.saturating_mul(entry);
                match table.get(at..at.saturating_add(entry)) {
                    Some(&[a, b]) => usize::from(u16::from_be_bytes([a, b])),
                    Some(&[a, b, c, d]) => u32::from_be_bytes([a, b, c, d]) as usize,
                    _ => 0,
                }
            };
            // PackBits expands at most 64× (a 2-byte repeat run yields 128 bytes), so a row needs
            // at least this many encoded bytes. Requiring it (and the data being present) before
            // allocating bounds the decoded planes by the file size: a tiny file can no longer
            // declare gigabytes of zero-padded or repeat-run rows.
            let min_row = row_bytes.div_ceil(128).saturating_mul(2);
            let mut data_len = 0usize;
            for i in 0..used.saturating_mul(height) {
                let c = count(i);
                if c < min_row {
                    return Err(err("RLE row too short for its width"));
                }
                data_len = data_len.saturating_add(c);
            }
            if data_len > bytes.len().saturating_sub(r.p) {
                return Err(err("truncated"));
            }
            for c in 0..used {
                let mut out = zeroed::<u8>(plane, too_large)?;
                for (y, row) in out.chunks_exact_mut(row_bytes).enumerate() {
                    let src = r.take(count(c * height + y))?;
                    unpackbits(src, row)?;
                }
                planes.push(out);
            }
        }
        _ => return Err(Error::Unsupported(F, "ZIP-compressed merged image data")),
    }
    let samples = n.checked_mul(used).ok_or_else(too_large)?;

    // Interleave to device samples.
    let buf = if mode == 2 {
        // Indexed → 8-bit RGB via the 768-byte palette.
        let (Some(pal), true) = (color_mode_data.get(..768), depth == 8) else {
            return Err(err("bad indexed palette"));
        };
        let idx = planes.first().ok_or_else(|| err("missing plane"))?;
        let mut v = zeroed::<u8>(n.checked_mul(3).ok_or_else(too_large)?, too_large)?;
        for (o, &i) in v.as_chunks_mut::<3>().0.iter_mut().zip(idx) {
            let i = i as usize;
            *o = [pal[i], pal[256 + i], pal[512 + i]];
        }
        Buf::U8(v)
    } else {
        let cmyk = model == Model::Cmyk;
        match bpc {
            1 => {
                let mut v = zeroed::<u8>(samples, too_large)?;
                for (c, p) in planes.iter().enumerate() {
                    for (o, &x) in v.iter_mut().skip(c).step_by(used).zip(p) {
                        // PSD CMYK stores 255 = no ink; our CMYK convention is 0 = no ink.
                        *o = if cmyk && c < 4 { 255 - x } else { x };
                    }
                }
                Buf::U8(v)
            }
            2 => {
                let mut v = zeroed::<u16>(samples, too_large)?;
                for (c, p) in planes.iter().enumerate() {
                    for (o, s) in v.iter_mut().skip(c).step_by(used).zip(p.as_chunks::<2>().0) {
                        let x = u16::from_be_bytes(*s);
                        *o = if cmyk && c < 4 { 65535 - x } else { x };
                    }
                }
                Buf::U16(v)
            }
            _ => {
                let mut v = zeroed::<f32>(samples, too_large)?;
                for (c, p) in planes.iter().enumerate() {
                    for (o, s) in v.iter_mut().skip(c).step_by(used).zip(p.as_chunks::<4>().0) {
                        let x = f32::from_be_bytes(*s);
                        *o = if cmyk && c < 4 { 1.0 - x } else { x };
                    }
                }
                Buf::F32(v)
            }
        }
    };
    let raw = Raw { width, height, model, alpha, premultiplied: false, buf, bit_depth: depth as u8 };
    finish(F, raw, Meta { icc, exif, xmp, ..Default::default() }, (width as u32, height as u32), opts)
}

/// Stored dimensions and EXIF orientation (resource 1058) from the header and image resources,
/// without reading the merged image data. Refused, as a decode would refuse it: an unsupported
/// depth, colour mode or compression, or merged image data that runs past the end of the file
/// (truncated). The run-length data itself is not unpacked.
pub(crate) fn header(bytes: &[u8]) -> Result<(u32, u32, u16)> {
    let mut r = Rd { b: bytes, p: 0 };
    if r.take(4)? != b"8BPS" {
        return Err(err("bad signature"));
    }
    let psb = match r.u16()? {
        1 => false,
        2 => true,
        _ => return Err(err("bad version")),
    };
    r.take(6)?;
    let channels = r.u16()? as usize;
    let height = r.u32()? as usize;
    let width = r.u32()? as usize;
    let depth = r.u16()?;
    let mode = r.u16()?;
    check_size(F, width as u64, height as u64, &DecodeOptions::default())?;
    if channels == 0 || channels > 56 {
        return Err(err("bad channel count"));
    }
    let cm_len = r.u32()? as usize;
    let color_mode_data = r.take(cm_len)?;
    let res_len = r.u32()? as usize;
    let (_, exif, _) = resources(r.take(res_len)?);
    let lm_len = if psb { r.u64()? as usize } else { r.u32()? as usize };
    r.take(lm_len)?;
    let compression = r.u16()?;
    let bpc = match depth {
        8 => 1usize,
        16 => 2,
        32 => 4,
        1 => return Err(Error::Unsupported(F, "1-bit bitmap PSD")),
        _ => return Err(err("bad depth")),
    };
    let n_color = match mode {
        1 | 8 | 7 | 2 => 1,
        3 => 3,
        4 => 4,
        9 => return Err(Error::Unsupported(F, "Lab PSD")),
        _ => return Err(err("unknown colour mode")),
    };
    if channels < n_color {
        return Err(err("too few channels"));
    }
    if mode == 2 && (color_mode_data.len() < 768 || depth != 8) {
        return Err(err("bad indexed palette"));
    }
    let rest = bytes.len().saturating_sub(r.p);
    let need = match compression {
        0 => width.checked_mul(height).and_then(|n| n.checked_mul(bpc)).and_then(|n| n.checked_mul(channels)),
        1 => {
            // the row byte counts, then the rows they count
            let rows = channels.checked_mul(height).ok_or_else(|| err("truncated"))?;
            let table = rows.checked_mul(if psb { 4 } else { 2 }).filter(|&t| t <= rest).ok_or_else(|| err("truncated"))?;
            let mut total = table;
            for _ in 0..rows {
                total = total.saturating_add(if psb { r.u32()? as usize } else { r.u16()? as usize });
            }
            Some(total)
        }
        _ => return Err(Error::Unsupported(F, "ZIP-compressed merged image data")),
    };
    if need.is_none_or(|n| n > rest) {
        return Err(err("truncated"));
    }
    let orientation = exif.as_deref().map(crate::exif::summarize).unwrap_or_default().orientation.unwrap_or(1);
    Ok((width as u32, height as u32, orientation))
}

/// A zero-filled buffer whose allocation failure is an error, not an abort.
fn zeroed<T: Clone + Default>(len: usize, too_large: impl Fn() -> Error) -> Result<Vec<T>> {
    let mut v = Vec::new();
    v.try_reserve_exact(len).map_err(|_| too_large())?;
    v.resize(len, T::default());
    Ok(v)
}

/// PackBits: decode one row into `row` (pre-zeroed, so short run data leaves zero padding).
/// A run that would write past the end of the row is an error; trailing bytes are ignored.
fn unpackbits(mut src: &[u8], row: &mut [u8]) -> Result<()> {
    let mut at = 0;
    while at < row.len() {
        let Some((&h, rest)) = src.split_first() else { break };
        src = rest;
        let h = h as i8;
        if h >= 0 {
            let n = h as usize + 1;
            let lit = src.get(..n).unwrap_or(src);
            let dst = row.get_mut(at..at + lit.len()).ok_or_else(|| err("RLE row overruns its width"))?;
            dst.copy_from_slice(lit);
            at += lit.len();
            src = src.get(n..).unwrap_or(&[]);
        } else if h != -128 {
            let n = (1 - h as isize) as usize;
            let Some((&b, rest)) = src.split_first() else { break };
            src = rest;
            row.get_mut(at..at + n).ok_or_else(|| err("RLE row overruns its width"))?.fill(b);
            at += n;
        }
    }
    Ok(())
}

fn resources(mut b: &[u8]) -> (Option<Vec<u8>>, Option<Vec<u8>>, Option<String>) {
    let (mut icc, mut exif, mut xmp) = (None, None, None);
    while b.len() >= 12 && &b[0..4] == b"8BIM" {
        let id = u16::from_be_bytes([b[4], b[5]]);
        let name_len = b[6] as usize;
        let name_total = (1 + name_len + 1) & !1;
        let p = 6 + name_total;
        if p + 4 > b.len() {
            break;
        }
        let size = u32::from_be_bytes([b[p], b[p + 1], b[p + 2], b[p + 3]]) as usize;
        let start = p + 4;
        let Some(data) = start.checked_add(size).and_then(|end| b.get(start..end)) else { break };
        match id {
            1039 => icc = Some(data.to_vec()),
            1058 => exif = Some(if data.starts_with(b"Exif\0\0") { data[6..].to_vec() } else { data.to_vec() }),
            1060 => xmp = Some(String::from_utf8_lossy(data).trim_end_matches('\0').to_string()),
            _ => {}
        }
        let next = start + ((size + 1) & !1);
        b = b.get(next..).unwrap_or(&[]);
    }
    (icc, exif, xmp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packbits() {
        let mut out = [0u8; 6];
        unpackbits(&[0xFE, 0xAA, 0x02, 0x80, 0x00, 0x2A], &mut out).unwrap();
        assert_eq!(out, [0xAA, 0xAA, 0xAA, 0x80, 0x00, 0x2A]);
        // Short run data is zero-padded.
        let mut out = [0u8; 4];
        unpackbits(&[0x01, 1], &mut out).unwrap();
        assert_eq!(out, [1, 0, 0, 0]);
        // Truncated literal run: copies what is there.
        let mut out = [0u8; 4];
        unpackbits(&[0x03, 7, 8], &mut out).unwrap();
        assert_eq!(out, [7, 8, 0, 0]);
    }

    #[test]
    fn packbits_overrun_is_an_error() {
        // Repeat run of 128 into a 4-byte row.
        let mut out = [0u8; 4];
        assert!(unpackbits(&[0x81, 0xAA], &mut out).is_err());
        // Literal run of 6 into a 4-byte row.
        assert!(unpackbits(&[0x05, 1, 2, 3, 4, 5, 6], &mut out).is_err());
    }

    /// PSD header + empty colour-mode/resources/layer sections + compression id.
    fn header(channels: u16, w: u32, h: u32, depth: u16, mode: u16, compression: u16) -> Vec<u8> {
        let mut b = b"8BPS".to_vec();
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(&[0; 6]);
        b.extend_from_slice(&channels.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&depth.to_be_bytes());
        b.extend_from_slice(&mode.to_be_bytes());
        b.extend_from_slice(&[0; 12]); // colour mode data, resources, layer & mask: all empty
        b.extend_from_slice(&compression.to_be_bytes());
        b
    }

    #[test]
    fn huge_rle_with_truncated_count_table_is_rejected_before_allocating() {
        // 1 × 2^30 px, 56 channels: the row table alone would be 56 Gi entries; the file has 4 bytes.
        let mut b = header(56, 1, 1 << 30, 32, 3, 1);
        b.extend_from_slice(&[0, 0, 0, 0]);
        let e = decode(&b, &DecodeOptions::default()).unwrap_err();
        assert!(matches!(e, Error::Malformed(_, ref m) if m == "truncated"), "{e:?}");
    }

    #[test]
    fn huge_rle_of_empty_rows_is_rejected_before_allocating() {
        // 2^30 × 1 px RGB at 32 bits: every row count is 0, which used to zero-pad 3 × 4 GiB.
        let mut b = header(3, 1 << 30, 1, 32, 3, 1);
        b.extend_from_slice(&[0; 6]);
        let e = decode(&b, &DecodeOptions::default()).unwrap_err();
        assert!(matches!(e, Error::Malformed(_, ref m) if m.contains("too short")), "{e:?}");
        // 1 Gpx RGB 8-bit whose (plausible) row counts claim more data than the file holds.
        let mut b = header(3, 1 << 20, 1 << 10, 8, 3, 1);
        b.extend_from_slice(&[0xFF; 3 * 1024 * 2]);
        let e = decode(&b, &DecodeOptions::default()).unwrap_err();
        assert!(matches!(e, Error::Malformed(_, ref m) if m == "truncated"), "{e:?}");
    }

    #[test]
    fn rle_repeat_run_past_row_width_is_an_error() {
        // 4 × 1 gray 8-bit; the single row is a 128-byte repeat run.
        let mut b = header(1, 4, 1, 8, 1, 1);
        b.extend_from_slice(&2u16.to_be_bytes());
        b.extend_from_slice(&[0x81, 0x55]);
        let e = decode(&b, &DecodeOptions::default()).unwrap_err();
        assert!(matches!(e, Error::Malformed(_, ref m) if m.contains("overruns")), "{e:?}");
    }

    #[test]
    fn rle_gray16_and_extra_channels_decode() {
        // 2 × 2 gray 16-bit with an extra (unused) channel whose data is absent: only the used
        // plane is decoded. Row: repeat run of 4 bytes 0x80 -> samples 0x8080.
        let mut b = header(2, 2, 2, 16, 1, 1);
        for _ in 0..4 {
            b.extend_from_slice(&2u16.to_be_bytes());
        }
        b.extend_from_slice(&[0xFD, 0x80, 0xFD, 0x80]);
        let d = decode(&b, &DecodeOptions::default()).unwrap();
        assert_eq!((d.width, d.height), (2, 2));
        let p = d.image.get(1, 1);
        let q = d.image.get(0, 0);
        assert!(p[0] > 0.0 && (p[0] - q[0]).abs() < 1e-6, "{p:?}");
    }
}
