//! The surface protocol's pairing: a client asks, the user approves in Settings ▸ Controls after
//! checking the code both sides show, and the client gets a token it presents from then on.
//! Only a SHA-256 of each token is stored (`control-surfaces.json` → `paired`).
//!
//! Message shapes and the method list are in `docs/control-surfaces.md`.

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::settings::{PairedClient, SurfaceSettings};

/// The protocol version this LightCraft speaks.
pub const PROTOCOL: u32 = 1;
/// A pairing request not answered within this time is dropped (ms).
pub const REQUEST_TTL_MS: f64 = 10.0 * 60.0 * 1000.0;
/// At most this many requests wait at once (one per client).
pub const MAX_PENDING: usize = 4;
/// Client ids and names are cut to this many characters.
pub const MAX_NAME: usize = 64;

/// Where a pairing request stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum PairState {
    /// Waiting for the user; `code` is shown on both sides.
    Pending {
        code: String,
    },
    /// Approved: the client keeps `token` (given out once).
    Approved {
        token: String,
    },
    Denied,
}

/// A request waiting in Settings ▸ Controls.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairRequest {
    pub request: String,
    pub client: String,
    pub name: String,
    pub code: String,
    #[serde(skip)]
    created_ms: u64,
    #[serde(skip)]
    state: PairState,
}

/// Pending pairing requests (in memory; they don't survive a restart).
#[derive(Clone, Debug, Default)]
pub struct Pairing {
    requests: Vec<PairRequest>,
    seed: Vec<u8>,
    counter: u64,
}

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 of a token, hex.
pub fn token_hash(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

/// Compare without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The paired client a token belongs to.
pub fn verify<'a>(settings: &'a SurfaceSettings, token: &str) -> Option<&'a PairedClient> {
    if token.is_empty() || token.len() > 256 {
        return None;
    }
    let h = token_hash(token);
    let mut found = None;
    for c in &settings.paired {
        if same(&c.token_sha256, &h) {
            found = Some(c);
        }
    }
    found
}

/// `s` cut to `MAX_NAME` characters, control characters removed.
pub fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(MAX_NAME).collect::<String>().trim().to_string()
}

impl Pairing {
    /// `seed`: unpredictable bytes from the host (codes and tokens are derived from it).
    pub fn new(seed: &[u8]) -> Pairing {
        Pairing { requests: Vec::new(), seed: seed.to_vec(), counter: 0 }
    }

    fn random(&mut self, salt: &[u8]) -> [u8; 32] {
        self.counter = self.counter.wrapping_add(1);
        let mut h = Sha256::new();
        h.update(&self.seed);
        h.update(self.counter.to_le_bytes());
        h.update(salt);
        h.finalize().into()
    }

    /// Requests waiting for the user.
    pub fn waiting(&self) -> impl Iterator<Item = &PairRequest> {
        self.requests.iter().filter(|r| matches!(r.state, PairState::Pending { .. }))
    }

    /// Drop requests older than [`REQUEST_TTL_MS`].
    pub fn expire(&mut self, now_ms: u64) {
        self.requests.retain(|r| (now_ms.saturating_sub(r.created_ms) as f64) < REQUEST_TTL_MS);
    }

    /// A client asks to pair (`surface.pair {client, name}`): a new request. One that client id
    /// already had waiting is withdrawn, its id and code with it: a request's id and code go
    /// only to the caller that made it, so another program asking under the same client id can't
    /// pick up the token, and the user sees the code change.
    pub fn ask(&mut self, client: &str, name: &str, now_ms: u64) -> Result<PairRequest, String> {
        self.expire(now_ms);
        let client = clean(client);
        if client.is_empty() {
            return Err("surface.pair: give a `client` id".into());
        }
        self.requests.retain(|r| !(r.client == client && matches!(r.state, PairState::Pending { .. })));
        if self.waiting().count() >= MAX_PENDING {
            return Err("surface.pair: too many pairing requests are waiting; answer them in Settings ▸ Controls first".into());
        }
        let r = self.random(client.as_bytes());
        let request = hex(r.get(..8).unwrap_or(&[]));
        let n = u32::from_le_bytes([r[8], r[9], r[10], r[11]]) % 1_000_000;
        let code = format!("{n:06}");
        let req = PairRequest { request, client, name: clean(name), code: code.clone(), created_ms: now_ms, state: PairState::Pending { code } };
        self.requests.push(req.clone());
        Ok(req)
    }

    /// Where request `id` stands (`surface.pair {request}`); an approved token is handed out once.
    pub fn poll(&mut self, id: &str, now_ms: u64) -> Option<PairState> {
        self.expire(now_ms);
        let i = self.requests.iter().position(|r| r.request == id)?;
        let state = self.requests.get(i)?.state.clone();
        if !matches!(state, PairState::Pending { .. }) {
            self.requests.remove(i);
        }
        Some(state)
    }

    /// The user approved request `id`: a token for the client, its hash saved in `settings`.
    pub fn approve(&mut self, id: &str, settings: &mut SurfaceSettings, now_s: u64) -> Result<String, String> {
        let i =
            self.requests.iter().position(|r| r.request == id && matches!(r.state, PairState::Pending { .. })).ok_or("no such pairing request")?;
        let (client, name) = self.requests.get(i).map(|r| (r.client.clone(), r.name.clone())).ok_or("no such pairing request")?;
        let token = hex(&self.random(b"token"));
        settings.paired.retain(|c| c.client != client);
        settings.paired.push(PairedClient {
            client: client.clone(),
            name: if name.is_empty() { client } else { name },
            token_sha256: token_hash(&token),
            approved_at: now_s,
        });
        if let Some(r) = self.requests.get_mut(i) {
            r.state = PairState::Approved { token: token.clone() };
        }
        Ok(token)
    }

    /// Trust a local client without asking (a plugin LightCraft installed or found installed for
    /// this user): a new token, its hash saved in `settings` (any earlier token of the client
    /// stops working). The host hands the token to the plugin through a file only this user can
    /// read; the port stays loopback-only and the token stays required.
    pub fn trust(&mut self, client: &str, name: &str, settings: &mut SurfaceSettings, now_s: u64) -> String {
        let client = clean(client);
        let token = hex(&self.random(b"trust"));
        settings.paired.retain(|c| c.client != client);
        settings.paired.push(PairedClient { client: client.clone(), name: clean(name), token_sha256: token_hash(&token), approved_at: now_s });
        // a request the client may have left waiting is answered by the token instead
        self.requests.retain(|r| r.client != client);
        token
    }

    pub fn deny(&mut self, id: &str) -> bool {
        match self.requests.iter_mut().find(|r| r.request == id && matches!(r.state, PairState::Pending { .. })) {
            Some(r) => {
                r.state = PairState::Denied;
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusted_local_client() {
        let mut p = Pairing::new(b"seed");
        let mut s = SurfaceSettings::default();
        let waiting = p.ask("lightcraft-logi", "plugin", 0).unwrap();
        let t1 = p.trust("lightcraft-logi", "LightCraft plugin", &mut s, 5);
        assert_eq!(verify(&s, &t1).map(|c| c.client.as_str()), Some("lightcraft-logi"));
        assert!(p.poll(&waiting.request, 1).is_none(), "its waiting request is gone");
        let t2 = p.trust("lightcraft-logi", "LightCraft plugin", &mut s, 6);
        assert_ne!(t1, t2);
        assert!(verify(&s, &t1).is_none(), "the old token stops working");
        assert_eq!(s.paired.len(), 1);
        assert!(!s.to_json().contains(&t2));
    }

    #[test]
    fn pair_approve_verify() {
        let mut p = Pairing::new(b"seed");
        let mut s = SurfaceSettings::default();
        let r = p.ask("lightcraft-logi", "Logi Actions plugin", 0).unwrap();
        assert_eq!(r.code.len(), 6);
        // asking again withdraws the open request: its id and code never go to a second caller
        let again = p.ask("lightcraft-logi", "again", 1).unwrap();
        assert_ne!(again.request, r.request);
        assert_eq!(p.poll(&r.request, 2), None, "the first request is withdrawn");
        assert_eq!(p.waiting().count(), 1, "one open request per client");
        let r = again;
        assert_eq!(p.poll(&r.request, 2), Some(PairState::Pending { code: r.code.clone() }));
        let token = p.approve(&r.request, &mut s, 100).unwrap();
        assert_eq!(token.len(), 64);
        assert!(!s.to_json().contains(&token), "only the hash is stored");
        assert_eq!(p.poll(&r.request, 3), Some(PairState::Approved { token: token.clone() }));
        assert_eq!(p.poll(&r.request, 4), None, "handed out once");
        assert_eq!(verify(&s, &token).map(|c| c.client.as_str()), Some("lightcraft-logi"));
        assert!(verify(&s, "wrong").is_none());
        assert!(verify(&s, "").is_none());
        // re-pairing the same client replaces its token
        let r2 = p.ask("lightcraft-logi", "", 5).unwrap();
        let t2 = p.approve(&r2.request, &mut s, 101).unwrap();
        assert_eq!(s.paired.len(), 1);
        assert!(verify(&s, &token).is_none() && verify(&s, &t2).is_some());
        // the settings file keeps it
        let back = SurfaceSettings::from_json(&s.to_json()).unwrap();
        assert!(verify(&back, &t2).is_some());
    }

    #[test]
    fn deny_expire_and_limits() {
        let mut p = Pairing::new(b"x");
        let r = p.ask("a", "A", 0).unwrap();
        assert!(p.deny(&r.request));
        assert_eq!(p.poll(&r.request, 1), Some(PairState::Denied));
        let r = p.ask("b", "B", 0).unwrap();
        p.expire(REQUEST_TTL_MS as u64 + 1);
        assert_eq!(p.poll(&r.request, REQUEST_TTL_MS as u64 + 1), None);
        for c in ["c", "d", "e", "f"] {
            p.ask(c, "", 0).unwrap();
        }
        assert!(p.ask("g", "", 0).is_err());
        assert!(p.ask("  ", "", 0).is_err());
        assert!(p.approve("nope", &mut SurfaceSettings::default(), 0).is_err());
        let long: String = "é".repeat(500);
        assert_eq!(clean(&long).chars().count(), MAX_NAME);
        assert_eq!(clean("a\u{0}b\nc"), "abc");
    }
}
