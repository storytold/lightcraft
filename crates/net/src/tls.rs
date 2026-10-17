//! Which server certificates a connection trusts.
//!
//! A certificate passes when it chains to the Mozilla roots (`webpki-roots`) or to one of the
//! connection's extra CA certificates, or when the SHA-256 fingerprint of the server's own
//! certificate is pinned. Otherwise, when a trust-on-first-use hook is set, the hook sees the
//! certificate (host, fingerprint, why it failed) and decides; an accepted fingerprint is pinned
//! for the rest of the [`Trust`]'s life and is readable through [`Trust::pinned`] so the caller
//! can store it with the account. Home servers with self-signed certificates (common for Immich)
//! work this way without ever turning verification off: handshake signatures are always checked.

use std::sync::{Arc, Mutex, PoisonError};

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, RootCertStore, SignatureScheme};
use sha2::{Digest, Sha256};

use crate::NetError;

/// SHA-256 of a DER certificate.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint(pub [u8; 32]);

impl Fingerprint {
    pub fn of(der: &[u8]) -> Fingerprint {
        Fingerprint(Sha256::digest(der).into())
    }

    /// Parse the `AB:CD:…` form (colons and case optional).
    pub fn parse(s: &str) -> Option<Fingerprint> {
        let hex: String = s.chars().filter(|c| *c != ':').collect();
        if hex.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
        }
        Some(Fingerprint(out))
    }
}

impl std::fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, b) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(":")?;
            }
            write!(f, "{b:02X}")?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Fingerprint({self})")
    }
}

/// What the trust-on-first-use hook is shown.
#[derive(Debug, Clone)]
pub struct CertInfo {
    pub host: String,
    pub fingerprint: Fingerprint,
    /// Why normal verification refused it (self-signed, unknown issuer, expired, wrong name…).
    pub reason: String,
    /// The server's certificate (DER), for a details view.
    pub der: Vec<u8>,
}

/// Decides about a certificate nothing else vouches for: `true` accepts (and pins) it.
/// Called on the requesting thread during the handshake; a UI typically answers from a
/// fingerprint the user confirmed earlier, or fails the request and asks.
pub type TofuHook = Arc<dyn Fn(&CertInfo) -> bool + Send + Sync>;

/// The trust settings of one connection (account). Cheap to clone; clones share the pins.
#[derive(Clone, Default)]
pub struct Trust {
    extra_roots: Vec<Vec<u8>>,
    pinned: Arc<Mutex<Vec<Fingerprint>>>,
    tofu: Option<TofuHook>,
}

impl std::fmt::Debug for Trust {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Trust")
            .field("extra_roots", &self.extra_roots.len())
            .field("pinned", &self.pinned())
            .field("tofu", &self.tofu.is_some())
            .finish()
    }
}

impl Trust {
    /// Mozilla roots only.
    pub fn new() -> Trust {
        Trust::default()
    }

    /// Also trust certificates issued by this CA (DER or PEM).
    pub fn with_ca(mut self, cert: &[u8]) -> Result<Trust, NetError> {
        let ders = if cert.starts_with(b"-----") { pem_certificates(cert)? } else { vec![cert.to_vec()] };
        if ders.is_empty() {
            return Err(NetError::Tls("no certificate in the CA file".into()));
        }
        self.extra_roots.extend(ders);
        Ok(self)
    }

    /// Trust the server certificate with this fingerprint (one the user confirmed earlier).
    pub fn with_pinned(self, fp: Fingerprint) -> Trust {
        self.pin(fp);
        self
    }

    /// Ask `hook` about certificates nothing else vouches for.
    pub fn with_tofu(mut self, hook: TofuHook) -> Trust {
        self.tofu = Some(hook);
        self
    }

    /// The pinned fingerprints, including those accepted by the hook since.
    pub fn pinned(&self) -> Vec<Fingerprint> {
        self.pinned.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn pin(&self, fp: Fingerprint) {
        let mut p = self.pinned.lock().unwrap_or_else(PoisonError::into_inner);
        if !p.contains(&fp) {
            p.push(fp);
        }
    }

    fn is_pinned(&self, fp: &Fingerprint) -> bool {
        self.pinned.lock().unwrap_or_else(PoisonError::into_inner).contains(fp)
    }

    /// The rustls client configuration for this trust.
    pub(crate) fn client_config(&self) -> Result<Arc<rustls::ClientConfig>, NetError> {
        let provider = Arc::new(rustls_rustcrypto::provider());
        let mut roots = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        for der in &self.extra_roots {
            roots.add(CertificateDer::from(der.clone())).map_err(|e| NetError::Tls(format!("CA certificate: {e}")))?;
        }
        let webpki =
            WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone()).build().map_err(|e| NetError::Tls(e.to_string()))?;
        let verifier = Arc::new(Verifier { webpki, provider: provider.clone(), trust: self.clone() });
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| NetError::Tls(e.to_string()))?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        Ok(Arc::new(config))
    }
}

/// Pull the CERTIFICATE blocks out of a PEM file.
fn pem_certificates(pem: &[u8]) -> Result<Vec<Vec<u8>>, NetError> {
    let text = std::str::from_utf8(pem).map_err(|_| NetError::Tls("CA file is not text".into()))?;
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("-----BEGIN CERTIFICATE-----") {
        let body = rest.get(start + 27..).unwrap_or_default();
        let end = body.find("-----END CERTIFICATE-----").ok_or_else(|| NetError::Tls("unterminated PEM certificate".into()))?;
        let b64: String = body.get(..end).unwrap_or_default().chars().filter(|c| !c.is_whitespace()).collect();
        out.push(base64_decode(&b64).ok_or_else(|| NetError::Tls("bad base64 in PEM certificate".into()))?);
        rest = body.get(end..).unwrap_or_default();
    }
    Ok(out)
}

pub(crate) fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[derive(Debug)]
struct Verifier {
    webpki: Arc<WebPkiServerVerifier>,
    provider: Arc<CryptoProvider>,
    trust: Trust,
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let reason = match self.webpki.verify_server_cert(end_entity, intermediates, server_name, ocsp, now) {
            Ok(ok) => return Ok(ok),
            Err(e) => e.to_string(),
        };
        let fp = Fingerprint::of(end_entity.as_ref());
        if self.trust.is_pinned(&fp) {
            return Ok(ServerCertVerified::assertion());
        }
        let host = server_name.to_str().into_owned();
        if let Some(hook) = &self.trust.tofu {
            let info = CertInfo { host: host.clone(), fingerprint: fp, reason, der: end_entity.as_ref().to_vec() };
            if hook(&info) {
                log::info!("net: accepted certificate {fp} for {host}");
                self.trust.pin(fp);
                return Ok(ServerCertVerified::assertion());
            }
        }
        Err(rustls::Error::Other(rustls::OtherError(Arc::new(NetError::UntrustedCertificate { host, fingerprint: fp.to_string() }))))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_round_trip() {
        let fp = Fingerprint::of(b"abc");
        let s = fp.to_string();
        assert_eq!(s.len(), 95);
        assert_eq!(Fingerprint::parse(&s), Some(fp));
        assert_eq!(Fingerprint::parse(&s.replace(':', "").to_lowercase()), Some(fp));
        assert_eq!(Fingerprint::parse("zz"), None);
        assert_eq!(Fingerprint::parse(&"é".repeat(32)), None);
    }

    #[test]
    fn base64() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("!!"), None);
        assert!(Trust::new().with_ca(b"-----BEGIN CERTIFICATE-----\nAAA").is_err());
        assert!(Trust::new().with_ca(b"-----nothing").is_err());
    }
}
