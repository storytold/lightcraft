//! Standard base64 (RFC 4648, with padding) for binary file I/O through the JSON ABI.
//! Own design; decoding rejects anything malformed instead of guessing.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    for chunk in bytes.chunks(3) {
        let b = [chunk.first().copied().unwrap_or(0), chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                let idx = ((n >> (18 - 6 * i)) & 63) as usize;
                out.push(char::from(ALPHABET.get(idx).copied().unwrap_or(b'A')));
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn value(c: u8) -> Option<u32> {
    match c {
        b'A'..=b'Z' => Some(u32::from(c - b'A')),
        b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

pub fn decode(text: &str) -> Result<Vec<u8>, String> {
    let s = text.as_bytes();
    if !s.len().is_multiple_of(4) {
        return Err("base64 length must be a multiple of 4".into());
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let chunks = s.len() / 4;
    for (i, c) in s.chunks(4).enumerate() {
        let last = i + 1 == chunks;
        let pad = c.iter().rev().take_while(|&&b| b == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return Err("bad base64 padding".into());
        }
        let mut n = 0u32;
        for (j, &b) in c.iter().enumerate() {
            let v = if j >= 4 - pad { 0 } else { value(b).ok_or("bad base64 character")? };
            n = (n << 6) | v;
        }
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(bytes.get(..3 - pad).unwrap_or(&[]));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_rejects_garbage() {
        for s in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar", &[0, 255, 128, 7]] {
            assert_eq!(decode(&encode(s)).unwrap(), s);
        }
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(encode(b"fo"), "Zm8=");
        for bad in ["Zm8", "Zm=8", "Z===", "Zm8=Zm8=", "Zm8*", "\u{e9}abc"] {
            assert!(decode(bad).is_err(), "{bad}");
        }
    }
}
