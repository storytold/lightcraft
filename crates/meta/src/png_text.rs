//! Bounded PNG text decoding, shared by generation indexing and the metadata viewer.
pub(crate) const TEXT_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PngTextField {
    pub name: String,
    pub value: String,
}

#[derive(Default)]
pub(crate) struct Decoder {
    decoded: usize,
    chunks: usize,
}

fn nul(b: &[u8]) -> Option<(&[u8], &[u8])> {
    let at = b.iter().position(|b| *b == 0)?;
    Some((b.get(..at)?, b.get(at + 1..)?))
}

impl Decoder {
    pub fn chunk(&mut self, typ: &[u8], data: &[u8], crc: &[u8]) -> Option<PngTextField> {
        if data.len() > TEXT_LIMIT || self.decoded >= 2 * TEXT_LIMIT || self.chunks >= 32 {
            return None;
        }
        self.chunks += 1;
        let (keyword, mut text) = nul(data)?;
        if keyword.is_empty() || keyword.len() > 79 {
            return None;
        }
        let mut hash = crc32fast::Hasher::new();
        hash.update(typ);
        hash.update(data);
        if hash.finalize().to_be_bytes() != crc {
            return None;
        }
        let compressed = match typ {
            b"tEXt" => false,
            b"zTXt" => {
                let (&0, rest) = text.split_first()? else {
                    return None;
                };
                text = rest;
                true
            }
            b"iTXt" => {
                let (&flag, rest) = text.split_first()?;
                let (&0, rest) = rest.split_first()? else {
                    return None;
                };
                if flag > 1 {
                    return None;
                }
                let (_, rest) = nul(rest)?;
                let (_, rest) = nul(rest)?;
                text = rest;
                flag == 1
            }
            _ => return None,
        };
        let limit = TEXT_LIMIT.min(2 * TEXT_LIMIT - self.decoded);
        let inflated;
        if compressed {
            match miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(text, limit) {
                Ok(v) => inflated = v,
                Err(_) => {
                    self.decoded += limit;
                    return None;
                }
            }
            text = &inflated;
        }
        if text.len() > limit {
            self.decoded += limit;
            return None;
        }
        self.decoded += text.len();
        let value = match std::str::from_utf8(text) {
            Ok(v) => v.to_owned(),
            Err(_) if typ != b"iTXt" => text.iter().map(|b| char::from(*b)).collect(),
            Err(_) => return None,
        };
        let name = keyword.iter().map(|b| char::from(*b)).collect();
        Some(PngTextField { name, value })
    }
}

/// Original text, including non-JSON generator metadata such as Automatic1111's `parameters`.
/// CRC-checked tEXt, zTXt and iTXt only; at most 32 chunks, 4 MiB each, 8 MiB total decoded.
pub fn png_text_fields(bytes: &[u8]) -> Vec<PngTextField> {
    let mut out = Vec::new();
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return out;
    }
    let mut decoder = Decoder::default();
    let mut pos = 8usize;
    for _ in 0..100_000 {
        let Some(header) = bytes.get(pos..pos.saturating_add(8)) else {
            break;
        };
        let Some(length) = header.get(..4).and_then(|b| b.try_into().ok()).map(u32::from_be_bytes) else {
            break;
        };
        let Some(typ) = header.get(4..8) else {
            break;
        };
        let start = pos.saturating_add(8);
        let end = start.saturating_add(length as usize);
        let (Some(data), Some(crc)) = (bytes.get(start..end), bytes.get(end..end.saturating_add(4))) else {
            break;
        };
        if matches!(typ, b"tEXt" | b"zTXt" | b"iTXt")
            && let Some(field) = decoder.chunk(typ, data, crc)
            && field.name != "XML:com.adobe.xmp"
        {
            out.push(field);
        }
        pos = end.saturating_add(4);
        if typ == b"IEND" {
            break;
        }
    }
    out
}
