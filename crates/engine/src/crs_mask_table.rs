//! Lightroom's AI selections (`Mask/Image`: Subject, Sky, Object, People…) as it stored them.
//!
//! Lightroom keeps the selection a model computed in the packet itself: the component names it
//! by `crs:MaskDigest`, and the top-level property `crs:Table_<digest>` holds it. Read here from
//! black-box observation of Lightroom's own sidecars (no Adobe code or documentation):
//!
//! - The text is a base-85 encoding: groups of five characters, each a 32-bit little-endian word
//!   whose base-85 digits are written least significant first, with Z85's alphabet except that
//!   `&`, `<` and `>` (which XML would escape) are `` ` ``, `'` and `|`.
//! - The bytes are a 16-byte header (four little-endian `u32`, `2, 1, 0, 0` in every packet seen)
//!   followed by a TIFF: one 8-bit grey image, tiled, compressed with JPEG XL (52546, as in DNG
//!   1.7), lossy, its transfer declared linear. 255 is fully selected.
//! - The image is the selection's bounding box. The component places it: `crs:Origin` (`x,y`, its
//!   top-left corner) within `crs:WholeImageArea` (`top,left,bottom,right` as rationals) of a
//!   `crs:FullMaskSize` (`width,height`) mask over the photo as stored, before its orientation.
//!
//! The selection becomes our model-segmentation storage ([`SegMask`]: logits on a grid over the
//! box), so it renders like an Object selection made here and needs no model. Like Lightroom's
//! other mask positions it is on the stored image; [`turn_seg`] puts it in the photo's frame.

use lightcraft_develop::SegMask;
use lightcraft_geom::Orientation;
use lightcraft_meta::XmpValue;
use serde_json::{Value, json};

use crate::crs_masks::Values;

/// Z85 with XML-safe stand-ins for `&`, `<`, `>`.
const ALPHABET: &[u8; 85] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ.-:+=^!/*?`'|()[]{}@%$#";
/// Longest table text read (a selection seen so far takes about 70 KB).
const MAX_TEXT: usize = 32 << 20;
/// Largest selection image decoded.
const MAX_PIXELS: u64 = 64 << 20;
/// Longest side of a selection image (Lightroom's seen so far are 2880 px wide): bounds the grid's resampling.
const MAX_EDGE: u64 = 1 << 15;
/// Most tiles a selection image may claim.
const MAX_TILES: u64 = 4096;
/// TIFF compression codes: none, JPEG XL (DNG 1.7).
const NONE: u64 = 1;
const JPEG_XL: u64 = 52546;
/// Logit of a fully selected (or unselected) cell: well inside [`SegMask`]'s ±15.9.
const FULL: f32 = 13.8;

/// The bytes a table's text encodes, `None` when it isn't one.
pub fn decode_text(s: &str) -> Option<Vec<u8>> {
    let b = s.trim().as_bytes();
    if b.is_empty() || !b.len().is_multiple_of(5) || b.len() > MAX_TEXT {
        return None;
    }
    let mut lut = [u8::MAX; 256];
    for (i, c) in ALPHABET.iter().enumerate() {
        lut[*c as usize] = i as u8;
    }
    let mut out = Vec::with_capacity(b.len() / 5 * 4);
    for g in b.as_chunks::<5>().0 {
        let mut v: u64 = 0;
        for c in g.iter().rev() {
            let d = lut[*c as usize];
            if d == u8::MAX {
                return None;
            }
            v = v * 85 + u64::from(d);
        }
        out.extend_from_slice(&u32::try_from(v).ok()?.to_le_bytes());
    }
    Some(out)
}

/// The grey selection image in a table's bytes: `(width, height, values 0..1)`.
pub fn decode_image(bytes: &[u8]) -> Option<(usize, usize, Vec<f32>)> {
    let tiff = bytes.get(16..)?;
    let parsed = lightcraft_tiff::Tiff::parse(tiff).ok()?;
    let ifd = parsed.ifds.first()?;
    let u = |tag: u16, i: usize| ifd.get(tag).and_then(|e| e.value.get_u64(i));
    let (w, h) = (u(0x100, 0)?, u(0x101, 0)?);
    let bits = u(0x102, 0).unwrap_or(1);
    let compression = u(0x103, 0).unwrap_or(NONE);
    let samples = u(0x115, 0).unwrap_or(1);
    if w == 0
        || h == 0
        || w > MAX_EDGE
        || h > MAX_EDGE
        || w.checked_mul(h)? > MAX_PIXELS
        || bits != 8
        || samples != 1
        || !matches!(compression, NONE | JPEG_XL)
    {
        return None;
    }
    // tiles, or strips as tiles the image's width wide
    let (tw, th, offsets, counts) = match (u(0x142, 0), u(0x143, 0)) {
        (Some(tw), Some(th)) => (tw, th, 0x144, 0x145),
        _ => (w, u(0x116, 0).unwrap_or(h).min(h), 0x111, 0x117),
    };
    if tw == 0 || th == 0 {
        return None;
    }
    let (across, down) = (w.div_ceil(tw), h.div_ceil(th));
    let n = across.checked_mul(down)?;
    if n > MAX_TILES || tw.checked_mul(th)? > MAX_PIXELS {
        return None;
    }
    let (w, h, tw, th) = (w as usize, h as usize, tw as usize, th as usize);
    let mut out = vec![0.0f32; w * h];
    for t in 0..n as usize {
        let (off, len) = (u(offsets, t)? as usize, u(counts, t)? as usize);
        let data = tiff.get(off..off.checked_add(len)?)?;
        let (x0, y0) = ((t % across as usize) * tw, (t / across as usize) * th);
        place(&mut out, (w, h), (x0, y0), (tw.min(w.saturating_sub(x0)), th.min(h.saturating_sub(y0))), &decode_tile(data, compression, tw, th)?)?;
    }
    Some((w, h, out))
}

/// Copy the `need` (width, height) part of a decoded `tile` (`(width, height, values)`) to `at` in the `size` image
/// `out`. `None` when the tile is smaller than that: its rows would land shifted, or leave a hole.
fn place(out: &mut [f32], size: (usize, usize), at: (usize, usize), need: (usize, usize), tile: &(usize, usize, Vec<f32>)) -> Option<()> {
    let (tile_w, tile_h, values) = tile;
    if *tile_w < need.0 || *tile_h < need.1 || values.len() < tile_w.checked_mul(*tile_h)? {
        return None;
    }
    for y in 0..need.1 {
        for x in 0..need.0 {
            if let (Some(dst), Some(src)) = (out.get_mut((at.1 + y) * size.0 + at.0 + x), values.get(y * tile_w + x)) {
                *dst = *src;
            }
        }
    }
    Some(())
}

/// One tile's values 0..1 as `(width, height, values)`.
fn decode_tile(data: &[u8], compression: u64, tw: usize, th: usize) -> Option<(usize, usize, Vec<f32>)> {
    if compression == NONE {
        let v: Vec<f32> = data.iter().take(tw * th).map(|b| f32::from(*b) / 255.0).collect();
        return (v.len() == tw * th).then_some((tw, th, v));
    }
    let d = lightcraft_codecs::decode(data, lightcraft_codecs::DecodeOptions { max_size: None, max_pixels: MAX_PIXELS }).ok()?;
    // the decoder hands back linear light; the stored samples are in the tile's own encoding
    // (Lightroom's declare a linear one, so this is the identity for them)
    let trc = d.space.trc.as_ref().map(|t| t[0].clone());
    let v = d.image.data.iter().map(|p| trc.as_ref().map_or(p[0], |t| t.from_linear(p[0])).clamp(0.0, 1.0)).collect();
    (d.image.width > 0 && d.image.height > 0).then_some((d.image.width, d.image.height, v))
}

fn pair(m: &XmpValue, k: &str) -> Option<(f64, f64)> {
    let s = m.field(k).and_then(XmpValue::text)?;
    let (a, b) = s.split_once(',')?;
    let (a, b) = (a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?);
    (a.is_finite() && b.is_finite()).then_some((a, b))
}

/// `top,left,bottom,right` as rationals (`0/1,0/1,1920/1,2880/1`).
fn area(m: &XmpValue) -> Option<[f64; 4]> {
    let s = m.field("crs:WholeImageArea").and_then(XmpValue::text)?;
    let v: Vec<f64> = s
        .split(',')
        .map(|r| {
            let (n, d) = r.split_once('/').unwrap_or((r, "1"));
            let (n, d) = (n.trim().parse::<f64>().ok()?, d.trim().parse::<f64>().ok()?);
            (d != 0.0).then(|| n / d).filter(|x| x.is_finite())
        })
        .collect::<Option<_>>()?;
    let [t, l, b, r] = v[..] else { return None };
    (b > t && r > l).then_some([t, l, b, r])
}

/// The `Mask/Image` component `m` as one of our shapes, from the selection `values` stores for it
/// (`None`: the packet holds no readable selection for it).
pub fn image_mask(values: &Values, m: &XmpValue) -> Option<Value> {
    let digest = m.field("crs:MaskDigest").and_then(XmpValue::text).filter(|d| d.bytes().all(|b| b.is_ascii_hexdigit()))?;
    let text = values.get(&format!("crs:Table_{digest}")).and_then(XmpValue::text)?;
    let (w, h, px) = decode_image(&decode_text(text)?)?;
    let (ox, oy) = pair(m, "crs:Origin").unwrap_or((0.0, 0.0));
    let [t, l, b, r] = area(m).or_else(|| pair(m, "crs:FullMaskSize").map(|(fw, fh)| [0.0, 0.0, fh, fw])).filter(|a| a[2] > a[0] && a[3] > a[1])?;
    let rect = [(ox - l) / (r - l), (oy - t) / (b - t), (ox + w as f64 - l) / (r - l), (oy + h as f64 - t) / (b - t)];
    if !rect.iter().all(|v| v.is_finite()) {
        return None;
    }
    let side = w.max(h).clamp(16, lightcraft_develop::segmask::MAX_SIDE);
    let logits = grid(w, h, &px, side);
    let seg = serde_json::to_value(SegMask::from_logits_in(side, &logits, rect)).ok()?;
    Some(json!({"kind": "object", "hint": [], "seg": seg}))
}

/// `side × side` logits from a `w × h` image of values 0..1, one axis at a time ([`taps`]); the longer axis goes
/// first, so the image in between is at most the shorter axis × `side`.
fn grid(w: usize, h: usize, px: &[f32], side: usize) -> Vec<f32> {
    let at = |x: usize, y: usize| px.get(y * w + x).copied().unwrap_or(0.0);
    let (across, down) = (taps(w, side), taps(h, side));
    let mut out = vec![0.0f32; side * side];
    if w <= h {
        let mut mid = vec![0.0f32; w * side];
        for (gy, t) in down.iter().enumerate() {
            for x in 0..w {
                mid[gy * w + x] = t.iter().map(|&(y, k)| at(x, y) * k).sum();
            }
        }
        for gy in 0..side {
            for (gx, t) in across.iter().enumerate() {
                out[gy * side + gx] = t.iter().map(|&(x, k)| mid.get(gy * w + x).copied().unwrap_or(0.0) * k).sum();
            }
        }
    } else {
        let mut mid = vec![0.0f32; side * h];
        for y in 0..h {
            for (gx, t) in across.iter().enumerate() {
                mid[y * side + gx] = t.iter().map(|&(x, k)| at(x, y) * k).sum();
            }
        }
        for (gy, t) in down.iter().enumerate() {
            for gx in 0..side {
                out[gy * side + gx] = t.iter().map(|&(y, k)| mid.get(y * side + gx).copied().unwrap_or(0.0) * k).sum();
            }
        }
    }
    out.into_iter().map(logit).collect()
}

/// The `n` samples of one axis brought to `m` cells: for each cell, the samples it takes and their weights. Fewer
/// cells than samples, a cell averages the samples it covers (a strand of hair thinner than a cell still counts);
/// more, it interpolates between the two nearest.
fn taps(n: usize, m: usize) -> Vec<Vec<(usize, f32)>> {
    let last = n.saturating_sub(1);
    let s = n as f64 / m as f64;
    (0..m)
        .map(|i| {
            if s > 1.0 {
                let (a, b) = (i as f64 * s, (i + 1) as f64 * s);
                (a.floor() as usize..(b.ceil() as usize).min(n))
                    .map(|j| (j, ((b.min(j as f64 + 1.0) - a.max(j as f64)) / s) as f32))
                    .filter(|&(_, k)| k > 0.0)
                    .collect()
            } else {
                let x = ((i as f64 + 0.5) * s - 0.5).max(0.0);
                let (j, f) = ((x.floor() as usize).min(last), (x - x.floor()) as f32);
                vec![(j, 1.0 - f), ((j + 1).min(last), f)]
            }
        })
        .collect()
}

fn logit(p: f32) -> f32 {
    let p = p.clamp(1e-6, 1.0 - 1e-6);
    (p / (1.0 - p)).ln().clamp(-FULL, FULL)
}

/// Turn a stored segmentation (`seg` JSON, on the image before `o`) into the frame `o` gives: its
/// rectangle's corners and its grid's cells go through the orientation. Left as it is when it
/// can't be read.
pub fn turn_seg(seg: &mut Value, o: Orientation) {
    if o == Orientation::Normal {
        return;
    }
    let Ok(s) = serde_json::from_value::<SegMask>(seg.clone()) else { return };
    let (Some(logits), Some(r)) = (s.logits(), s.bounds()) else { return };
    let side = s.side as usize;
    let mut out = vec![-FULL; side * side];
    for y in 0..side {
        for x in 0..side {
            let (u, v) = o.map(x as f64 + 0.5, y as f64 + 0.5, side as f64, side as f64);
            let (u, v) = (u.floor().clamp(0.0, (side - 1) as f64) as usize, v.floor().clamp(0.0, (side - 1) as f64) as usize);
            if let (Some(d), Some(src)) = (out.get_mut(v * side + u), logits.get(y * side + x)) {
                *d = *src;
            }
        }
    }
    let (a, b) = (o.map(r[0], r[1], 1.0, 1.0), o.map(r[2], r[3], 1.0, 1.0));
    let rect = [a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)];
    if let Ok(v) = serde_json::to_value(SegMask::from_logits_in(side, &out, rect)) {
        *seg = v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode bytes as a table's text (the inverse of [`decode_text`]).
    fn encode(bytes: &[u8]) -> String {
        let mut s = String::new();
        for w in bytes.chunks(4) {
            let mut b = [0u8; 4];
            b[..w.len()].copy_from_slice(w);
            let mut v = u32::from_le_bytes(b) as u64;
            for _ in 0..5 {
                s.push(ALPHABET[(v % 85) as usize] as char);
                v /= 85;
            }
        }
        s
    }

    #[test]
    fn the_alphabet_is_85_distinct_xml_safe_characters() {
        let mut seen = std::collections::BTreeSet::new();
        for c in ALPHABET {
            assert!(seen.insert(*c));
            assert!(!b"&<>\"\\ ".contains(c), "{}", *c as char);
        }
    }

    #[test]
    fn text_decodes_as_lightroom_writes_it() {
        // the header and TIFF signature as they open every table seen
        let b = decode_text("20000100000000000000iMH4080000").unwrap();
        assert_eq!(&b[..16], &[2, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&b[16..], b"II*\x00\x08\x00\x00\x00");
        let bytes: Vec<u8> = (0..=255u8).collect();
        assert_eq!(decode_text(&encode(&bytes)).unwrap(), bytes);
    }

    #[test]
    fn hostile_text_is_refused() {
        for s in ["", "abcd", "&&&&&", "#####", "0000\"", "00000 0000"] {
            assert_eq!(decode_text(s), None, "{s:?}");
        }
        assert_eq!(decode_image(&decode_text("20000100000000000000").unwrap()), None);
        assert_eq!(decode_image(&[0; 40]), None);
    }

    /// An uncompressed 8-bit grey TIFF of `w × h` with these samples, behind the 16-byte header.
    fn tiff(w: u32, h: u32, samples: &[u8]) -> Vec<u8> {
        let mut t = b"II*\x00\x08\x00\x00\x00".to_vec();
        let entries: [(u16, u16, u32); 8] =
            [(0x100, 4, w), (0x101, 4, h), (0x102, 3, 8), (0x103, 3, 1), (0x106, 3, 1), (0x111, 4, 0), (0x115, 3, 1), (0x117, 4, w * h)];
        let data_at = 8 + 2 + entries.len() as u32 * 12 + 4;
        t.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for (tag, typ, v) in entries {
            let v = if tag == 0x111 { data_at } else { v };
            t.extend_from_slice(&tag.to_le_bytes());
            t.extend_from_slice(&typ.to_le_bytes());
            t.extend_from_slice(&1u32.to_le_bytes());
            t.extend_from_slice(&v.to_le_bytes());
        }
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(samples);
        let mut out = [2u32, 1, 0, 0].iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>();
        out.extend(t);
        out
    }

    fn component(xmp_attrs: &str) -> (Values, XmpValue) {
        let x = format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
            <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" {xmp_attrs}>
            <crs:MaskGroupBasedCorrections><rdf:Seq><rdf:li><rdf:Description crs:What="Correction">
            <crs:CorrectionMasks><rdf:Seq><rdf:li crs:What="Mask/Image" crs:MaskSubType="1" crs:MaskDigest="ABCD" crs:MaskBlendMode="1"
             crs:Origin="2,1" crs:WholeImageArea="0/1,0/1,4/1,8/1" crs:FullMaskSize="8,4"/></rdf:Seq></crs:CorrectionMasks>
            </rdf:Description></rdf:li></rdf:Seq></crs:MaskGroupBasedCorrections></rdf:Description></rdf:RDF></x:xmpmeta>"#
        );
        let d = lightcraft_meta::parse_xmp(&x).unwrap();
        let c = d.values.get("crs:MaskGroupBasedCorrections").unwrap().items()[0].clone();
        let m = c.field("crs:CorrectionMasks").unwrap().items()[0].clone();
        (d.values, m)
    }

    #[test]
    fn a_stored_selection_becomes_a_segmentation_over_its_box() {
        // a 4 × 2 selection, its left half selected, placed at (2, 1) in an 8 × 4 mask
        let text = encode(&tiff(4, 2, &[255, 255, 0, 0, 255, 255, 0, 0]));
        let (values, m) = component(&format!("crs:Table_ABCD=\"{text}\""));
        let shape = image_mask(&values, &m).expect("decoded");
        assert_eq!(shape["kind"], "object");
        let seg: SegMask = serde_json::from_value(shape["seg"].clone()).unwrap();
        assert_eq!(seg.bounds(), Some([0.25, 0.25, 0.75, 0.75]));
        let (side, l) = (seg.side as usize, seg.logits().unwrap());
        let row = side / 2;
        assert!(l[row * side + side / 8] > 5.0 && l[row * side + side - 1 - side / 8] < -5.0, "selected left, not right");
    }

    #[test]
    fn a_correction_takes_the_stored_selection_with_its_blend_mode() {
        let text = encode(&tiff(4, 2, &[255, 255, 0, 0, 255, 255, 0, 0]));
        let (values, _) = component(&format!("crs:Table_ABCD=\"{text}\""));
        let (masks, skipped) = crate::crs_masks::masks(&values, crate::crs_masks::DEFAULT_ASPECT);
        assert!(skipped.is_empty(), "{skipped:?}");
        let c = &masks[0]["components"][0];
        assert_eq!((c["op"].as_str(), c["shape"]["kind"].as_str()), (Some("subtract"), Some("object")));
        // without the table: Subject our way, as before
        let (values, _) = component("");
        let (masks, _) = crate::crs_masks::masks(&values, crate::crs_masks::DEFAULT_ASPECT);
        assert_eq!(masks[0]["components"][0]["shape"]["kind"], "subject");
    }

    #[test]
    fn a_missing_or_damaged_selection_is_none() {
        let (values, m) = component("");
        assert_eq!(image_mask(&values, &m), None);
        let (values, m) = component("crs:Table_ABCD=\"0000\"");
        assert_eq!(image_mask(&values, &m), None);
    }

    #[test]
    fn a_tile_smaller_than_its_place_is_refused() {
        // a 4 × 2 tile placed at (2, 1) of an 8 × 4 image, then one a pixel too narrow or too short
        let mut out = vec![0.0; 32];
        assert_eq!(place(&mut out, (8, 4), (2, 1), (4, 2), &(4, 2, vec![1.0; 8])), Some(()));
        assert_eq!(out.iter().filter(|v| **v == 1.0).count(), 8);
        assert_eq!((out[8 + 2], out[2 * 8 + 5], out[8 + 1]), (1.0, 1.0, 0.0));
        assert_eq!(place(&mut out, (8, 4), (2, 1), (4, 2), &(3, 2, vec![1.0; 6])), None);
        assert_eq!(place(&mut out, (8, 4), (2, 1), (4, 2), &(4, 1, vec![1.0; 4])), None);
        assert_eq!(place(&mut out, (8, 4), (2, 1), (4, 2), &(4, 2, vec![1.0; 7])), None, "fewer values than it claims");
        // an edge tile need only cover the part inside the image
        assert_eq!(place(&mut out, (8, 4), (6, 3), (2, 1), &(4, 2, vec![1.0; 8])), Some(()));
    }

    #[test]
    fn a_thin_part_of_a_large_selection_survives_the_grid() {
        // a strand one pixel wide in a selection three times the grid's width: sampling every third column
        // would miss it, averaging what each cell covers keeps a third of it
        let side = lightcraft_develop::segmask::MAX_SIDE;
        let (w, h) = (3 * side, 16);
        let px: Vec<f32> = (0..w * h).map(|i| if i % w == 3 { 1.0 } else { 0.0 }).collect();
        let g = grid(w, h, &px, side);
        let row = side / 2 * side;
        let p = 1.0 / (1.0 + (-g[row + 1]).exp());
        assert!((p - 1.0 / 3.0).abs() < 0.01, "the cell over columns 3..6: {p}");
        assert!(g[row] <= -FULL + 1e-3 && g[row + 2] <= -FULL + 1e-3, "its neighbours stay unselected");
        // a small selection is interpolated as before: a 2 × 2 image's corners fill the grid's corners
        let g = grid(2, 2, &[1.0, 0.0, 0.0, 0.0], 16);
        assert!(g[0] >= FULL - 1e-3 && g[15] <= -FULL + 1e-3 && g[255] <= -FULL + 1e-3);
    }

    #[test]
    fn an_overlong_selection_is_refused() {
        // within the pixel budget but longer than any mask: refused before its resampling is set up
        let row = |w: u64| tiff(w as u32, 1, &vec![255; w as usize]);
        assert_eq!(decode_image(&row(MAX_EDGE)).map(|(w, h, _)| (w, h)), Some((MAX_EDGE as usize, 1)));
        assert_eq!(decode_image(&row(MAX_EDGE + 1)), None);
    }

    #[test]
    fn a_turned_segmentation_keeps_what_it_selects() {
        // left column selected over the box [0.1, 0.2, 0.5, 0.6] of the stored image
        let side = 16;
        let logits: Vec<f32> = (0..side * side).map(|i| if i % side < 4 { FULL } else { -FULL }).collect();
        let mut v = serde_json::to_value(SegMask::from_logits_in(side, &logits, [0.1, 0.2, 0.5, 0.6])).unwrap();
        // Exif 8: the stored image's left edge is the upright photo's bottom
        turn_seg(&mut v, Orientation::from_exif(8));
        let s: SegMask = serde_json::from_value(v).unwrap();
        let b = s.bounds().unwrap();
        for (got, want) in b.iter().zip([0.2, 0.5, 0.6, 0.9]) {
            assert!((got - want).abs() < 1e-9, "{b:?}");
        }
        let l = s.logits().unwrap();
        assert!(l[(side - 1) * side + side / 2] > 5.0, "bottom row selected");
        assert!(l[side / 2] < -5.0, "top row not");
    }
}
