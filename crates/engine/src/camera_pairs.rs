//! Pairs files: the sampled colour pairs of a library, shared without any images.
//!
//! `lightcraft-cli calibrate --pairs FILE` writes the colour pairs a camera profile is fitted from
//! (white-balanced camera RGB against the colour of the raw's own embedded camera JPEG, a few thousand
//! per photo) and `calibrate --from-pairs FILE…` pools them, with pairs sampled from raw files, into
//! the same fit. A photographer can share a file of their library's pairs so profiles can be fitted on
//! many photographers' cameras and light, with no photo leaving their disk.
//!
//! The file is UTF-8 JSON, format version 1:
//!
//! ```text
//! { "format": "lightcraft-colour-pairs", "version": 1,
//!   "pair": "...describes the data layout...",
//!   "photos": [ { "model": "ILCE-7CR", "id": "<32 hex>", "dro": true, "pairs": 4000, "data": "<base64>" }, … ] }
//! ```
//!
//! Per photo: `model` (Exif `Model`), `id` (a 128-bit hash of the raw file's bytes: it tells the same photo
//! apart across files and is neither the file name nor any metadata), `dro` (Sony's Dynamic Range Optimizer
//! was on, absent when the file doesn't say), `pairs` (the count) and `data`. `data` is `pairs` × 6 values
//! as little-endian `f32` (white-balanced camera RGB, then the camera JPEG's linear Rec.2020 RGB), each
//! pair after the other, then split into four planes by byte position (all first bytes, all second bytes,
//! …) so that zlib finds the repeated exponents, then zlib-compressed and base64-encoded. A file holds
//! nothing else: no file names or paths, GPS, serial numbers, dates or image data. Each photo's pairs are in value order
//! (sorted by their six values), never in the order they were sampled across the picture.
use crate::camera_profiles::{PAIRS_PER_FILE, Pair, PhotoPairs, canonical_order};
use serde::{Deserialize, Serialize};

const FORMAT: &str = "lightcraft-colour-pairs";
const VERSION: u32 = 1;
const LAYOUT: &str = "per photo, `pairs` x 6 little-endian f32 (white-balanced camera RGB, then the camera JPEG's linear Rec.2020 RGB), split into 4 byte planes, zlib, base64";
/// Most photos read from one pairs file: several full `calibrate` runs (300 photos each by default).
/// Each decodes to at most [`PAIRS_PER_FILE`] pairs (about 190 KB in memory), so a file costs at most
/// about 380 MB however well its data compresses.
pub const MAX_PHOTOS: usize = 2000;
const PAIR_BYTES: usize = 24;
/// Longest JSON entry a real photo needs: its most pairs as bytes, base64 of a zlib stream that could
/// not shrink them (a few bytes of headers per 64 KB block) and room for its other fields.
const MAX_PHOTO_JSON: u64 = (PAIRS_PER_FILE * PAIR_BYTES + 1024).div_ceil(3) as u64 * 4 + 1024;
/// Largest pairs file read: [`MAX_PHOTOS`] photos at their largest.
pub const MAX_FILE: u64 = MAX_PHOTOS as u64 * MAX_PHOTO_JSON + 4096;
/// Longest model name kept.
const MAX_MODEL: usize = 64;

#[derive(Serialize, Deserialize)]
struct File {
    format: String,
    version: u32,
    #[serde(default)]
    pair: String,
    photos: Vec<Photo>,
}

#[derive(Serialize, Deserialize)]
struct Photo {
    model: String,
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dro: Option<bool>,
    pairs: usize,
    data: String,
}

fn validate_model(model: &str) -> Result<&str, &'static str> {
    let model = super::camera_profiles::model_key(model);
    if model.is_empty() || model.chars().count() > MAX_MODEL || model.chars().any(char::is_control) {
        return Err("unusable camera model");
    }
    Ok(model)
}

/// The pairs file for `photos`, each photo's pairs in value order ([`canonical_order`]).
pub fn write(photos: &[&PhotoPairs]) -> Result<Vec<u8>, String> {
    if photos.len() > MAX_PHOTOS {
        return Err(format!("{} photos is more than a pairs file may hold ({MAX_PHOTOS})", photos.len()));
    }
    let photos = photos
        .iter()
        .enumerate()
        .map(|(n, p)| {
            let model = validate_model(&p.model).map_err(|why| format!("photo {}: {why}", n + 1))?;
            let mut pairs = p.pairs.clone();
            canonical_order(&mut pairs);
            Ok(Photo { model: model.to_owned(), id: p.id.clone(), dro: p.dro, pairs: pairs.len(), data: base64(&pack(&pairs)) })
        })
        .collect::<Result<Vec<_>, String>>()?;
    serde_json::to_vec(&File { format: FORMAT.into(), version: VERSION, pair: LAYOUT.into(), photos }).map_err(|e| e.to_string())
}

/// Read and validate a pairs file (`origin` names it in errors). A damaged or hostile file is an error, never a panic
/// or a huge allocation: files over [`MAX_FILE`] bytes or [`MAX_PHOTOS`] photos are refused before any photo's data
/// is decoded, and each photo decodes to at most [`PAIRS_PER_FILE`] pairs.
pub fn read(bytes: &[u8], origin: &str) -> Result<Vec<PhotoPairs>, String> {
    if bytes.len() as u64 > MAX_FILE {
        return Err(format!("{origin}: {} bytes is too large for a pairs file", bytes.len()));
    }
    let file: File = serde_json::from_slice(bytes).map_err(|e| format!("{origin}: not a pairs file: {e}"))?;
    if file.format != FORMAT {
        return Err(format!("{origin}: not a pairs file (format {:?})", file.format));
    }
    if file.version != VERSION {
        return Err(format!("{origin}: pairs file version {} is not supported (this build reads version {VERSION})", file.version));
    }
    if file.photos.len() > MAX_PHOTOS {
        return Err(format!("{origin}: {} photos is more than a pairs file may hold ({MAX_PHOTOS})", file.photos.len()));
    }
    file.photos
        .into_iter()
        .enumerate()
        .map(|(n, p)| {
            let bad = |why: &str| format!("{origin}: photo {}: {why}", n + 1);
            let model = validate_model(&p.model).map_err(bad)?;
            if p.id.len() != 32 || !p.id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                return Err(bad("id is not 32 hex digits"));
            }
            if p.pairs > PAIRS_PER_FILE {
                return Err(bad("too many colour pairs"));
            }
            let packed = unbase64(&p.data).ok_or_else(|| bad("data is not base64"))?;
            let raw = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&packed, p.pairs * PAIR_BYTES + 1)
                .map_err(|_| bad("data does not decompress"))?;
            let pairs = unpack(&raw, p.pairs).ok_or_else(|| bad("data does not match its pair count or holds unusable values"))?;
            Ok(PhotoPairs { model: model.to_owned(), id: p.id, dro: p.dro, pairs })
        })
        .collect()
}

/// Bytes of `pairs`: f32 little endian, shuffled into byte planes, zlib.
fn pack(pairs: &[Pair]) -> Vec<u8> {
    let n = pairs.len();
    let mut planes = vec![0u8; n * PAIR_BYTES];
    for (i, (a, b)) in pairs.iter().enumerate() {
        for (k, v) in a.iter().chain(b.iter()).enumerate() {
            for (byte, x) in (*v as f32).to_le_bytes().into_iter().enumerate() {
                planes[byte * n * 6 + i * 6 + k] = x;
            }
        }
    }
    miniz_oxide::deflate::compress_to_vec_zlib(&planes, 9)
}

fn unpack(planes: &[u8], n: usize) -> Option<Vec<Pair>> {
    if planes.len() != n * PAIR_BYTES {
        return None;
    }
    let value = |i: usize, k: usize| -> Option<f64> {
        let v = f32::from_le_bytes(std::array::from_fn(|byte| planes[byte * n * 6 + i * 6 + k]));
        (v.is_finite() && v.abs() < 1e6).then_some(f64::from(v))
    };
    (0..n)
        .map(|i| {
            let mut c = [0.0; 6];
            for (k, slot) in c.iter_mut().enumerate() {
                *slot = value(i, k)?;
            }
            Some(([c[0], c[1], c[2]], [c[3], c[4], c[5]]))
        })
        .collect()
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        for k in 0..4 {
            if k <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * k)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn unbase64(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_end_matches('=').as_bytes();
    if text.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for &c in text {
        let v = ALPHABET.iter().position(|&a| a == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(model: &str, id: &str, n: usize) -> PhotoPairs {
        let single = |v: f64| f64::from(v as f32);
        let pairs = (0..n)
            .map(|i| {
                let x = i as f64 / n.max(1) as f64;
                ([single(x), single(x * 0.5 + 0.01), single(1.0 - x)], [single(x * x), single(x.sqrt()), single(0.3)])
            })
            .collect();
        PhotoPairs { model: model.into(), id: id.into(), dro: Some(true), pairs }
    }

    const ID: &str = "00112233445566778899aabbccddeeff";

    #[test]
    fn writer_and_reader_reject_unusable_models() {
        let good = photo("ILCE-7CR", ID, 1);
        let file: serde_json::Value = serde_json::from_slice(&write(&[&good]).unwrap()).unwrap();
        for model in
            ["".into(), " \t\n".into(), "a\nb".into(), "a\0b".into(), "a\u{85}b".into(), "a".repeat(MAX_MODEL + 1), "\u{e9}".repeat(MAX_MODEL + 1)]
        {
            let invalid = photo(&model, ID, 1);
            let error = write(&[&good, &invalid]).unwrap_err();
            assert_eq!(error, "photo 2: unusable camera model", "{model:?}");
            let mut file = file.clone();
            file["photos"][0]["model"] = model.clone().into();
            let error = read(&serde_json::to_vec(&file).unwrap(), "t").unwrap_err();
            assert_eq!(error, "t: photo 1: unusable camera model", "{model:?}");
        }
    }

    #[test]
    fn model_limits_count_unicode_characters_after_trimming() {
        for model in ["ILCE-7CR".into(), "a".repeat(MAX_MODEL), "\u{e9}".repeat(MAX_MODEL)] {
            let expected = photo(&model, ID, 1);
            let padded = photo(&format!(" \t{model}\r\n"), ID, 1);
            let bytes = write(&[&padded]).unwrap();
            let mut file: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(file["photos"][0]["model"], model);
            assert_eq!(read(&bytes, "t").unwrap().as_slice(), std::slice::from_ref(&expected));
            file["photos"][0]["model"] = padded.model.into();
            assert_eq!(read(&serde_json::to_vec(&file).unwrap(), "t").unwrap(), [expected]);
        }
    }

    #[test]
    fn write_enforces_photo_limit() {
        let photo = photo("ILCE-7M4", ID, 1);
        let mut photos = vec![&photo; MAX_PHOTOS];
        let bytes = write(&photos).unwrap();
        assert_eq!(read(&bytes, "at limit").unwrap().len(), MAX_PHOTOS);
        photos.push(&photo);
        let error = write(&photos).unwrap_err();
        assert!(error.contains("photos is more than a pairs file may hold"), "{error}");
    }

    #[test]
    fn base64_round_trips() {
        for len in 0..9 {
            let data: Vec<u8> = (0..len as u8).map(|b| b.wrapping_mul(37)).collect();
            assert_eq!(unbase64(&base64(&data)).unwrap(), data);
        }
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert!(unbase64("a$bc").is_none());
    }

    #[test]
    fn pairs_round_trip_exactly_and_hold_nothing_else() {
        let photos = [photo("ILCE-7CR", ID, 500), photo("X-T4", "ffeeddccbbaa99887766554433221100", 3)];
        let bytes = write(&photos.iter().collect::<Vec<_>>()).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains(FORMAT) && !text.contains("path") && !text.contains(".ARW"));
        assert_eq!(read(&bytes, "t").unwrap(), photos);
        let none = write(&[]).unwrap();
        assert!(read(&none, "t").unwrap().is_empty());
    }

    #[test]
    fn damaged_and_hostile_files_are_errors() {
        let good: serde_json::Value = serde_json::from_slice(&write(&[&photo("ILCE-7CR", ID, 10)]).unwrap()).unwrap();
        let bad = |f: &dyn Fn(&mut serde_json::Value)| {
            let mut w = good.clone();
            f(&mut w);
            read(&serde_json::to_vec(&w).unwrap(), "t").is_err()
        };
        assert!(bad(&|w| w["version"] = 2.into()));
        assert!(bad(&|w| w["format"] = "other".into()));
        assert!(bad(&|w| w["photos"][0]["model"] = "".into()));
        assert!(bad(&|w| w["photos"][0]["model"] = "a\nb".into()));
        assert!(bad(&|w| w["photos"][0]["id"] = "../../x".into()));
        assert!(bad(&|w| w["photos"][0]["pairs"] = 11.into()));
        assert!(bad(&|w| w["photos"][0]["pairs"] = 1_000_000_000u64.into()));
        assert!(bad(&|w| w["photos"][0]["data"] = "!!!!".into()));
        assert!(bad(&|w| w["photos"][0]["data"] = "AAAA".into()));
        assert!(!bad(&|w| w["photos"][0]["dro"] = serde_json::Value::Null));
        assert!(read(b"{not json", "t").is_err());
        let mut p = photo("ILCE-7CR", ID, 4);
        p.pairs[1].0[0] = f64::NAN;
        assert!(read(&write(&[&p]).unwrap(), "t").is_err());
    }

    /// Tiny zlib streams of zeros inflate about 700×, so the photo count is capped before any data is decoded.
    #[test]
    fn too_many_photos_are_refused_before_decoding() {
        let one = write(&[&photo("ILCE-7CR", ID, 0)]).unwrap();
        let w: serde_json::Value = serde_json::from_slice(&one).unwrap();
        let mut many = w.clone();
        many["photos"] = serde_json::Value::Array(vec![w["photos"][0].clone(); MAX_PHOTOS + 1]);
        let bytes = serde_json::to_vec(&many).unwrap();
        assert!((bytes.len() as u64) < MAX_FILE);
        let err = read(&bytes, "t").unwrap_err();
        assert!(err.contains("photos is more than"), "{err}");
        many["photos"] = serde_json::Value::Array(vec![w["photos"][0].clone(); MAX_PHOTOS]);
        assert_eq!(read(&serde_json::to_vec(&many).unwrap(), "t").unwrap().len(), MAX_PHOTOS);
        // a real photo at its most pairs fits the per-photo budget behind MAX_FILE
        let full = write(&[&photo("ILCE-7CR", ID, PAIRS_PER_FILE)]).unwrap();
        assert!((full.len() as u64) < MAX_PHOTO_JSON + 4096);
    }
}
