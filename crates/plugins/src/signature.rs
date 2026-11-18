//! Author signatures: the [`SIGNATURE_SECTION`] the SDK's `plugin-stamp sign` appends to a stamped
//! plug-in (`docs/plugins.md` → Signing your plug-in).
//!
//! The section is the file's last, holds `{"v": 1, "key", "sig"}` (an ed25519 public key and a
//! signature, standard base64), and the signature covers every byte before it: the code and the
//! stamped manifest. The app pins the key at the first signed install ([`crate::Manager`]) and asks
//! the user when a later file is signed by another key or not at all. A section that doesn't hold
//! up is [`Signature::Invalid`], never the same as unsigned.

use ed25519_dalek::VerifyingKey;
use serde_json::Value;
use sha2::{Digest, Sha256};
use wasmi::Module;

/// The custom section a plug-in's author signature is in. Neutral on purpose, as
/// [`crate::MANIFEST_SECTION`].
pub const SIGNATURE_SECTION: &str = "photo-plugin.signature";
/// Largest signature section read; `plugin-stamp sign` writes about 150 bytes.
pub const MAX_SIGNATURE_BYTES: usize = 1024;
/// The section format version this host reads.
const VERSION: u64 = 1;

/// An author's public key, as a signature section names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Signer([u8; 32]);

impl Signer {
    /// The key's bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The key in standard base64, as `plugins.json` keeps a pin.
    pub fn to_base64(&self) -> String {
        crate::b64::encode(&self.0)
    }

    /// A key in standard base64 that is an ed25519 public key, or why not.
    pub fn from_base64(text: &str) -> Result<Signer, String> {
        let bytes = crate::b64::decode(text).map_err(|_| "not base64".to_string())?;
        let key: [u8; 32] = bytes.as_slice().try_into().map_err(|_| "not 32 bytes".to_string())?;
        VerifyingKey::from_bytes(&key).map_err(|_| "not an ed25519 public key".to_string())?;
        Ok(Signer(key))
    }

    /// The key's fingerprint, as the user sees it: the first 16 hex digits of its SHA-256, in
    /// groups of four (`3f2a 9c41 07be d5e8`). `plugin-stamp` prints the same.
    pub fn fingerprint(&self) -> String {
        let digest = Sha256::digest(self.0);
        let hex: Vec<String> = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
        hex.chunks(2).map(|c| c.concat()).collect::<Vec<_>>().join(" ")
    }
}

/// A module's author signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signature {
    /// No [`SIGNATURE_SECTION`]: the file isn't signed.
    None,
    /// Signed by this key, and not changed since.
    Valid(Signer),
    /// A signature section that doesn't hold up, and why: the file is refused.
    Invalid(String),
}

impl Signature {
    /// The signer of a valid signature.
    pub fn signer(&self) -> Option<Signer> {
        match self {
            Signature::Valid(s) => Some(*s),
            _ => None,
        }
    }

    /// `"none"`, `"valid"` or `"invalid"`, as `plugin.inspectFile` gives it.
    pub fn as_str(&self) -> &'static str {
        match self {
            Signature::None => "none",
            Signature::Valid(_) => "valid",
            Signature::Invalid(_) => "invalid",
        }
    }
}

/// The signature of `bytes`, a module wasmi has validated as `module`. Valid when there is exactly
/// one [`SIGNATURE_SECTION`], it is the file's last section (written with the shortest lengths, as
/// `plugin-stamp` writes it), it is at most [`MAX_SIGNATURE_BYTES`], its JSON is version 1 with a
/// 32-byte key and a 64-byte signature, and the signature is the key's over every byte before it.
pub(crate) fn read(module: &Module, bytes: &[u8]) -> Signature {
    let mut found = module.custom_sections().filter(|s| s.name() == SIGNATURE_SECTION).map(|s| s.data());
    let Some(data) = found.next() else { return Signature::None };
    let count = found.count().saturating_add(1);
    match verify(bytes, data, count) {
        Ok(signer) => Signature::Valid(signer),
        Err(why) => Signature::Invalid(why),
    }
}

fn verify(bytes: &[u8], data: &[u8], count: usize) -> Result<Signer, String> {
    if count > 1 {
        return Err(format!("the file has {count} {SIGNATURE_SECTION} sections; a signed plug-in has one"));
    }
    if data.len() > MAX_SIGNATURE_BYTES {
        return Err(format!("the {SIGNATURE_SECTION} section is {} bytes (limit {MAX_SIGNATURE_BYTES})", data.len()));
    }
    let signed = bytes
        .strip_suffix(section(data).as_slice())
        .ok_or_else(|| format!("the {SIGNATURE_SECTION} section isn't the file's last section: the file was changed after signing"))?;
    let object = match serde_json::from_slice::<Value>(data) {
        Ok(Value::Object(m)) => m,
        Ok(_) => return Err("the signature isn't a JSON object".into()),
        Err(e) => return Err(format!("the signature isn't JSON ({e})")),
    };
    match object.get("v").and_then(Value::as_u64) {
        Some(VERSION) => {}
        Some(v) => return Err(format!("the signature is format version {v}; this version of the app reads version {VERSION}")),
        None => return Err("the signature has no format version `v`".into()),
    }
    let field = |name: &str| -> Result<Vec<u8>, String> {
        let text = object.get(name).and_then(Value::as_str).ok_or_else(|| format!("the signature has no `{name}` string"))?;
        crate::b64::decode(text).map_err(|_| format!("the signature's `{name}` isn't base64"))
    };
    let key: [u8; 32] = field("key")?.try_into().map_err(|_| "the signature's `key` isn't 32 bytes".to_string())?;
    let sig: [u8; 64] = field("sig")?.try_into().map_err(|_| "the signature's `sig` isn't 64 bytes".to_string())?;
    let verifying = VerifyingKey::from_bytes(&key).map_err(|_| "the signature's `key` isn't an ed25519 public key".to_string())?;
    verifying
        .verify_strict(signed, &ed25519_dalek::Signature::from_bytes(&sig))
        .map_err(|_| "the signature doesn't match the file: it was changed after signing".to_string())?;
    Ok(Signer(key))
}

/// The whole signature section holding `data` (id, size, name, data), with the shortest lengths.
fn section(data: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(data.len().saturating_add(SIGNATURE_SECTION.len()).saturating_add(5));
    leb128(&mut payload, SIGNATURE_SECTION.len());
    payload.extend_from_slice(SIGNATURE_SECTION.as_bytes());
    payload.extend_from_slice(data);
    let mut out = Vec::with_capacity(payload.len().saturating_add(6));
    out.push(0);
    leb128(&mut out, payload.len());
    out.extend_from_slice(&payload);
    out
}

fn leb128(out: &mut Vec<u8>, mut value: usize) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fingerprint_is_four_groups_of_four_hex_digits() {
        let f = Signer([7; 32]).fingerprint();
        assert_eq!(f.len(), 19, "{f}");
        assert!(f.split(' ').all(|g| g.len() == 4 && g.chars().all(|c| c.is_ascii_hexdigit())), "{f}");
        assert_ne!(f, Signer([8; 32]).fingerprint(), "another key, another fingerprint");
    }

    #[test]
    fn a_pin_round_trips_through_base64_and_refuses_what_isnt_a_key() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[3; 32]).verifying_key().to_bytes();
        let s = Signer(key);
        assert_eq!(Signer::from_base64(&s.to_base64()), Ok(s));
        assert!(Signer::from_base64("not base64!").is_err());
        assert!(Signer::from_base64(&crate::b64::encode(&[1; 31])).is_err(), "31 bytes");
    }
}
