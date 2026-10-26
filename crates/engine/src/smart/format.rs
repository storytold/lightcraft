//! The optional lossless container. Pixels are scene-linear source RGB, before any edit or tone curve.
use lightcraft_raster::Rgb32f;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(super) const MAGIC: &[u8] = b"LCSP2\n";
const END: &[u8] = b"LCSPEND2";
pub(super) const MAX_PIXELS: u64 = 16 * 1024 * 1024;
pub(super) const MAX_BYTES: u64 = 256 * 1024 * 1024;

/// Storage choice for this library. Missing preferences retain the original compact format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    #[default]
    Compact,
    Lossless,
}

impl Format {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "compact" => Some(Self::Compact),
            "lossless" => Some(Self::Lossless),
            _ => None,
        }
    }
}

pub(super) fn dimensions(head: &Value) -> Result<(usize, usize), String> {
    let w = head["w"].as_u64().ok_or("missing smart preview width")?;
    let h = head["h"].as_u64().ok_or("missing smart preview height")?;
    if w == 0 || h == 0 || w > 8192 || h > 8192 || w.checked_mul(h).is_none_or(|n| n > MAX_PIXELS) {
        return Err("smart preview dimensions exceed the limit".into());
    }
    Ok((w as usize, h as usize))
}

/// Parse only the bounded JSON header, retaining unknown fields for future header rewrites.
pub(super) fn split(bytes: &[u8]) -> Result<(Format, Value, &[u8]), String> {
    let (format, rest) = if let Some(rest) = bytes.strip_prefix(super::MAGIC) {
        (Format::Compact, rest)
    } else {
        (Format::Lossless, bytes.strip_prefix(MAGIC).ok_or("not a smart preview")?)
    };
    let nl =
        rest.iter().take(super::HEADER_MAX as usize - MAGIC.len()).position(|b| *b == b'\n').ok_or("smart preview header is missing or too long")?;
    let head: Value = serde_json::from_slice(rest.get(..nl).ok_or("bad smart preview header")?).map_err(|e| e.to_string())?;
    dimensions(&head)?;
    if format == Format::Lossless && (head["format"].as_str() != Some("tiff32") || head["enc"].as_u64() != Some(1)) {
        return Err("unsupported lossless smart preview encoding".into());
    }
    Ok((format, head, rest.get(nl + 1..).ok_or("missing smart preview pixels")?))
}

pub(super) fn pack(format: Format, head: &Value, body: &[u8]) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec(head).map_err(|e| e.to_string())?;
    if json.len() + 1 + MAGIC.len() > super::HEADER_MAX as usize || body.len() as u64 > MAX_BYTES - super::HEADER_MAX {
        return Err("smart preview exceeds the file or header limit".into());
    }
    let magic = if format == Format::Compact { super::MAGIC } else { MAGIC };
    let mut out = magic.to_vec();
    out.extend_from_slice(&json);
    out.push(b'\n');
    out.extend_from_slice(body);
    Ok(out)
}

fn checksum(bytes: &[u8]) -> String {
    format!("{:032x}", lightcraft_preview::Hasher128::new().update(bytes).finish().0)
}

pub(super) fn encode(img: &Rgb32f, info: Option<&lightcraft_pipeline::SourceInfo>) -> Result<Vec<u8>, String> {
    let mut head = serde_json::json!({"w": img.width, "h": img.height, "format": "tiff32", "enc": 1,
        "look_version": crate::camera_preview::LOOK_VERSION});
    dimensions(&head)?;
    if img.width.checked_mul(img.height) != Some(img.data.len()) || img.data.iter().flatten().any(|v| !v.is_finite()) {
        return Err("invalid smart preview source pixels".into());
    }
    let encoded =
        lightcraft_codecs::EncodeImage::new(img.width as u32, img.height as u32, 3, lightcraft_codecs::Samples::F32(img.data.as_flattened()));
    let mut body =
        lightcraft_codecs::encode_tiff(&encoded, lightcraft_codecs::TiffCompression::Deflate, &Default::default()).map_err(|e| e.to_string())?;
    head["payload_bytes"] = body.len().into();
    head["payload_hash"] = checksum(&body).into();
    // the camera tone curve in the header, the other decoder facts in a tail after the end marker
    let tail = super::facts_tail(info, &mut head)?;
    body.extend_from_slice(END);
    body.extend_from_slice(&tail);
    pack(Format::Lossless, &head, &body)
}

pub(super) fn payload<'a>(head: &Value, body: &'a [u8]) -> Result<&'a [u8], String> {
    let bytes = body.strip_suffix(END).ok_or("truncated lossless smart preview")?;
    if head["payload_bytes"].as_u64() != Some(bytes.len() as u64) || head["payload_hash"].as_str() != Some(&checksum(bytes)) {
        return Err("damaged lossless smart preview pixels".into());
    }
    Ok(bytes)
}

/// Fast completeness check, without decompressing the image or scanning a large payload. `tail` is
/// the byte count of the decoder facts after the end marker (0: none).
pub(super) fn complete(head: &Value, payload_offset: u64, tail: u64, file: &mut std::fs::File) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Some(bytes) = head["payload_bytes"].as_u64().filter(|n| *n > 8 && *n <= MAX_BYTES) else { return false };
    let Some(end_at) = payload_offset.checked_add(bytes) else { return false };
    let expected = end_at.checked_add(END.len() as u64).and_then(|n| n.checked_add(tail));
    if file.metadata().ok().map(|m| m.len()).filter(|n| *n <= MAX_BYTES) != expected {
        return false;
    }
    let mut marker = [0; 8];
    file.seek(SeekFrom::Start(end_at)).is_ok() && file.read_exact(&mut marker).is_ok() && marker == END
}
