//! OAuth 1.0a request signing (RFC 5849) as Flickr uses it: HMAC-SHA1 over the method, the URL
//! and every parameter except the uploaded file, keyed with the consumer secret and the token
//! secret.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

use hmac::{Hmac, Mac};
use sha1::Sha1;

/// RFC 3986 percent-encoding: everything but `A–Z a–z 0–9 - . _ ~`, as UTF-8 bytes.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The signature base string: `METHOD&url&params`, the parameters encoded, sorted by name and
/// then value (byte order), and joined as `k=v&…` before being encoded once more.
pub fn base_string(method: &str, url: &str, params: &[(String, String)]) -> String {
    let mut pairs: Vec<(String, String)> = params.iter().map(|(k, v)| (encode(k), encode(v))).collect();
    pairs.sort();
    let joined = pairs.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
    format!("{}&{}&{}", method.to_ascii_uppercase(), encode(url), encode(&joined))
}

/// `oauth_signature` for a request: Base64 of HMAC-SHA1(base string, `consumer&token` secrets).
pub fn sign(method: &str, url: &str, params: &[(String, String)], consumer_secret: &str, token_secret: &str) -> Result<String, String> {
    let key = format!("{}&{}", encode(consumer_secret), encode(token_secret));
    let mut mac = Hmac::<Sha1>::new_from_slice(key.as_bytes()).map_err(|e| format!("signing key: {e}"))?;
    mac.update(base_string(method, url, params).as_bytes());
    Ok(base64(&mac.finalize().into_bytes()))
}

/// Standard Base64 with padding.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let sym = |i: u32| ALPHABET.get((i & 63) as usize).map_or('=', |b| char::from(*b));
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let n = (b(0) << 16) | (b(1) << 8) | b(2);
        out.push(sym(n >> 18));
        out.push(sym(n >> 12));
        out.push(if chunk.len() > 1 { sym(n >> 6) } else { '=' });
        out.push(if chunk.len() > 2 { sym(n) } else { '=' });
    }
    out
}

/// A value no other request of this process (or another one) shares: a nonce or a multipart
/// boundary. It needs to be unique, not secret.
pub fn unique() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    // the standard library's per-process random hash keys mix them
    let mut h = RandomState::new().build_hasher();
    h.write_u64(n);
    h.write_u64(now);
    h.write_u32(std::process::id());
    format!("{:016x}{n:x}", h.finish())
}

/// Seconds since 1970 (`oauth_timestamp`).
pub fn timestamp() -> String {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
        p.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn encodes_like_rfc_3986() {
        assert_eq!(encode("abc-._~XYZ019"), "abc-._~XYZ019");
        assert_eq!(encode("a b+c&d=e/f"), "a%20b%2Bc%26d%3De%2Ff");
        assert_eq!(encode("café"), "caf%C3%A9");
        assert_eq!(encode("72157626737672178@N07"), "72157626737672178%40N07");
    }

    #[test]
    fn base64_pads_every_length() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    /// The worked example of the OAuth 1.0 specification (and RFC 5849): its published signature.
    #[test]
    fn signs_the_specification_example() {
        let p = pairs(&[
            ("file", "vacation.jpg"),
            ("size", "original"),
            ("oauth_consumer_key", "dpf43f3p2l4k3l03"),
            ("oauth_token", "nnch734d00sl2jdk"),
            ("oauth_signature_method", "HMAC-SHA1"),
            ("oauth_timestamp", "1191242096"),
            ("oauth_nonce", "kllo9940pd9333jh"),
            ("oauth_version", "1.0"),
        ]);
        assert_eq!(
            sign("GET", "http://photos.example.net/photos", &p, "kd94hf93k423kf44", "pfkkdhi9sl3r4s00").unwrap(),
            "tR3+Ty81lMeYAr/Fid0kMTYa/WM="
        );
    }

    /// Values with spaces, quotes, `&` and non-ASCII text, as an upload's title and tags have
    /// (expected value computed independently with Python's `hmac` and `urllib.parse.quote`).
    #[test]
    fn signs_an_upload_with_awkward_text() {
        let p = pairs(&[
            ("title", "Sunset at Snake River, café & “dunes”"),
            ("tags", "\"grand teton\" wyoming"),
            ("oauth_consumer_key", "k"),
            ("oauth_token", "t"),
            ("oauth_signature_method", "HMAC-SHA1"),
            ("oauth_timestamp", "1700000000"),
            ("oauth_nonce", "abc123"),
            ("oauth_version", "1.0"),
        ]);
        assert_eq!(sign("POST", "https://up.flickr.com/services/upload/", &p, "cs", "ts").unwrap(), "nZkYaH77Cay2wPsBU3FmW9X4as8=");
    }

    #[test]
    fn unique_values_differ() {
        let a: std::collections::HashSet<String> = (0..1000).map(|_| unique()).collect();
        assert_eq!(a.len(), 1000);
    }
}
