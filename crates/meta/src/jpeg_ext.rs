//! JPEG Extended XMP continuation verification.

const XMP_EXT_SIG: &[u8] = b"http://ns.adobe.com/xmp/extension/\0";

/// The GUID of the Extended XMP segments in a JPEG file when every segment shares it and
/// their chunks tile exactly the full length they declare (no gap, overlap or excess).
pub fn jpeg_extended_xmp_guid(b: &[u8]) -> Option<String> {
    if !b.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut ext: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut ext_headers: Vec<(&[u8], u32)> = Vec::new();
    let mut i = 2usize;
    while i + 4 <= b.len() {
        if b[i] != 0xff {
            break;
        }
        let marker = b[i + 1];
        if matches!(marker, 0xd9 | 0xda) {
            break;
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if len < 2 {
            break;
        }
        let Some(start) = i.checked_add(4) else { break };
        let Some(declared_end) = i.checked_add(2).and_then(|at| at.checked_add(len)) else { break };
        let end = declared_end.min(b.len());
        let Some(payload) = b.get(start..end) else { break };
        if marker == 0xe1 && payload.starts_with(XMP_EXT_SIG) {
            let p = &payload[XMP_EXT_SIG.len()..];
            if p.len() >= 40 {
                let full = u32::from_be_bytes([p[32], p[33], p[34], p[35]]);
                let off = u32::from_be_bytes([p[36], p[37], p[38], p[39]]);
                ext_headers.push((&p[..32], full));
                ext.push((off, p[40..].to_vec()));
            }
        }
        i = end;
    }
    if ext.is_empty() {
        return None;
    }
    ext.sort_by_key(|p| p.0);
    verified_extension_guid(&ext_headers, &ext)
}

/// The GUID shared by all Extended XMP segments when their offset-sorted chunks are contiguous from
/// zero and add up to the full length every segment declares; `None` otherwise.
fn verified_extension_guid(headers: &[(&[u8], u32)], chunks_by_offset: &[(u32, Vec<u8>)]) -> Option<String> {
    let (&(guid, full), rest) = headers.split_first()?;
    if rest.iter().any(|&(other_guid, other_full)| other_guid != guid || other_full != full) {
        return None;
    }
    let mut next = 0u64;
    for (offset, data) in chunks_by_offset {
        if u64::from(*offset) != next {
            return None;
        }
        next = next.checked_add(u64::try_from(data.len()).ok()?)?;
    }
    (next == u64::from(full)).then(|| String::from_utf8_lossy(guid).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(marker: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0xff, marker];
        let len = u16::try_from(payload.len() + 2).unwrap();
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn jpeg_extended_xmp_guid_needs_matching_segments_and_length() {
        let build = |segments: &[(u8, u32, u32, &[u8])]| {
            let mut j = vec![0xff, 0xd8];
            for &(guid, full, off, part) in segments {
                let mut p = XMP_EXT_SIG.to_vec();
                p.extend_from_slice(&[guid; 32]);
                p.extend_from_slice(&full.to_be_bytes());
                p.extend_from_slice(&off.to_be_bytes());
                p.extend_from_slice(part);
                j.extend(seg(0xe1, &p));
            }
            jpeg_extended_xmp_guid(&j)
        };
        assert!(build(&[(b'A', 10, 0, b"hello"), (b'B', 10, 5, b"world")]).is_none(), "two GUIDs");
        assert!(build(&[(b'A', 10, 0, b"hello")]).is_none(), "half the declared length");
        assert!(build(&[(b'A', 9, 0, b"hello"), (b'A', 9, 5, b"world")]).is_none(), "longer than declared");
        assert!(build(&[(b'A', 10, 0, b"hello"), (b'A', 10, 4, b"oworl")]).is_none(), "overlapping chunks");
        assert_eq!(build(&[(b'A', 10, 0, b"hello"), (b'A', 10, 5, b"world")]), Some("A".repeat(32)));
    }
}
