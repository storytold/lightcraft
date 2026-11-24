//! Standard base64 (RFC 4648, padded) for MCP image content.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encode bytes as padded standard base64.
pub use craft_mime::transfer::base64 as base64_encode;

/// Decode padded or unpadded standard base64 (whitespace ignored). `None` on invalid input.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    let mut padded = false;
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace()) {
        if c == b'=' {
            padded = true;
            continue;
        }
        if padded {
            return None;
        }
        let v = ALPHABET.iter().position(|&a| a == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if bits == 6 {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{base64_decode, base64_encode};

    #[test]
    fn rfc4648_vectors() {
        for (i, o) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")]
        {
            assert_eq!(base64_encode(i.as_bytes()), o);
        }
        assert_eq!(base64_encode(&[0xff, 0xfe, 0xfd]), "//79");
    }

    #[test]
    fn roundtrip() {
        let data: Vec<u8> = (0..=255u8).chain((0..100).map(|i| (i * 37) as u8)).collect();
        for n in 0..data.len() {
            assert_eq!(base64_decode(&base64_encode(&data[..n])).unwrap(), &data[..n]);
        }
        assert!(base64_decode("not base64!").is_none());
    }

    #[test]
    fn rejects_malformed() {
        assert!(base64_decode("Z").is_none());
        assert!(base64_decode("Zm9vY").is_none());
        assert!(base64_decode("Zg==!!!not-base64").is_none());
        assert_eq!(base64_decode("Zg==\n").unwrap(), b"f");
    }
}
