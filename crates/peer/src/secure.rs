//! The encrypted, mutually authenticated channel between two devices, over TCP.
//!
//! Handshake, protocol `lcc-peer-v2` (own design, after the Noise / SIGMA pattern, with the
//! commit-then-reveal step of short-authentication-string protocols; Sources: RFC 7748 X25519,
//! RFC 8032 Ed25519, RFC 5869 HKDF, RFC 8439 ChaCha20-Poly1305, RFC 6189 §4 for the commitment).
//! `hello = {device, key, eph, nonce}`: a device id, its Ed25519 public key, a fresh X25519 public
//! key and a fresh 32-byte random nonce; fields this version doesn't know are refused.
//! 1. The initiator (the side that connected) sends `Commit {proto, commit}` with
//!    `commit = SHA-256(label ‖ " commit" ‖ hello_initiator)`. The nonce keeps the hello secret.
//! 2. The responder sends its hello.
//! 3. The initiator reveals its hello; the responder checks it against the commitment and goes
//!    no further if it doesn't match.
//! 4. `transcript = SHA-256(label ‖ commit message ‖ hello_initiator ‖ hello_responder)` (each
//!    part with its length). Each side signs `transcript ‖ role` with its Ed25519 key and checks
//!    the other's signature against the key it sent. Nobody in the middle can swap the ephemeral
//!    keys without a device's private key.
//! 5. HKDF-SHA-256 over the X25519 shared secret (salt = transcript) gives one key per direction
//!    and the pairing code. The code covers both devices' keys, so a device in the middle (which
//!    must use its own key) makes the two screens show different codes.
//!
//! Why the commitment: without it a device relaying between two others could keep changing its
//! own hello towards one of them until both codes happen to match (a million cheap tries). Now
//! the initiator is bound to its hello before it sees the responder's, and the responder answers
//! before it sees the initiator's: each completed handshake is one blind guess at a six-digit
//! code. What is left is opening connection after connection as initiator and dropping each one
//! whose code doesn't fit; the service counts those and refuses pairing requests after a few
//! ([`crate::service`]).
//!
//! Version 1 sent the hellos first and had no commitment. A first message that isn't a v2
//! commitment, or a hello without a nonce, is [`Error::Version`]: both devices need the same
//! version of the app.
//!
//! Whether the other device may talk is the caller's decision: a paired device's key must match
//! the stored one ([`crate::identity::Store`]); a pairing needs the people to compare the codes.
//!
//! Frames: a 4-byte big-endian length, a 1-byte kind (JSON or binary, see [`Frame`]), then
//! ChaCha20-Poly1305 ciphertext with the kind as associated data; the nonce is a per-direction
//! counter, so a replayed, reordered, relabelled or altered frame fails to decrypt.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::identity::{Identity, random, verify};
use crate::{Error, Result};

/// The protocol this build speaks; a device speaking another one is refused ([`Error::Version`]).
pub const PROTOCOL: &str = "lcc-peer-v2";
const LABEL: &[u8] = PROTOCOL.as_bytes();
/// The largest handshake message.
const MAX_HELLO: usize = 4096;
/// The largest frame (a batch of catalog records, a chunk of a file).
pub const MAX_FRAME: usize = 64 << 20;
/// The largest frame before the other device is known to be paired (openings, replies, decisions):
/// an unpaired device can't make this side allocate more than this.
pub const MAX_SMALL_FRAME: usize = 64 << 10;
/// How long the whole handshake may take, however slowly the other side trickles bytes.
pub const HANDSHAKE_WITHIN: Duration = Duration::from_secs(10);

const KIND_JSON: u8 = 0;
const KIND_BYTES: u8 = 1;

/// One received frame: what [`Channel::send`] / [`Channel::send_json`] sent, or what
/// [`Channel::send_bytes`] sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// A message (JSON text, not yet parsed).
    Json(Vec<u8>),
    /// Binary data (a chunk of a file).
    Bytes(Vec<u8>),
}

/// The initiator's first message.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Commit {
    proto: String,
    /// SHA-256 over the label and the hello it will reveal, base64.
    commit: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    device: String,
    key: String,
    eph: String,
    /// 32 fresh random bytes, base64: what makes the commitment hide the hello.
    nonce: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Auth {
    sig: String,
}

fn write_raw(s: &mut TcpStream, bytes: &[u8]) -> Result<()> {
    let len = u32::try_from(bytes.len()).map_err(|_| Error::Protocol("message too large".into()))?;
    s.write_all(&len.to_be_bytes())?;
    s.write_all(bytes)?;
    s.flush()?;
    Ok(())
}

/// Fills `buf`; with a deadline, every read waits only for the time left, so a peer sending one
/// byte at a time can't stretch the read past it.
fn read_full(s: &mut TcpStream, buf: &mut [u8], deadline: Option<Instant>) -> Result<()> {
    let Some(deadline) = deadline else {
        s.read_exact(buf)?;
        return Ok(());
    };
    let mut got = 0;
    while got < buf.len() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Error::Io("the other device took too long".into()));
        }
        s.set_read_timeout(Some(left))?;
        let Some(rest) = buf.get_mut(got..) else { break };
        match s.read(rest) {
            Ok(0) => return Err(Error::Io("the connection closed".into())),
            Ok(n) => got = got.saturating_add(n),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// A 4-byte length no larger than `max`.
fn read_len(s: &mut TcpStream, max: usize, deadline: Option<Instant>) -> Result<usize> {
    let mut len = [0u8; 4];
    read_full(s, &mut len, deadline)?;
    let len = usize::try_from(u32::from_be_bytes(len)).unwrap_or(usize::MAX);
    if len > max {
        return Err(Error::Protocol(format!("message of {len} bytes refused")));
    }
    Ok(len)
}

fn read_raw(s: &mut TcpStream, max: usize, deadline: Option<Instant>) -> Result<Vec<u8>> {
    let len = read_len(s, max, deadline)?;
    let mut buf = vec![0u8; len];
    read_full(s, &mut buf, deadline)?;
    Ok(buf)
}

/// What the initiator commits to before it sees the responder's hello.
fn commitment(hello: &[u8]) -> [u8; 32] {
    Sha256::new().chain_update(LABEL).chain_update(b" commit").chain_update(hello).finalize().into()
}

/// What both sides sign and derive keys from: every handshake message so far, each with its
/// length so no two different exchanges give the same bytes.
fn transcript(commit_msg: &[u8], hello_initiator: &[u8], hello_responder: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new().chain_update(LABEL);
    for part in [commit_msg, hello_initiator, hello_responder] {
        // handshake messages are at most MAX_HELLO bytes
        h.update(u32::try_from(part.len()).unwrap_or(u32::MAX).to_be_bytes());
        h.update(part);
    }
    h.finalize().into()
}

fn decode32(b64: &str) -> Option<[u8; 32]> {
    B64.decode(b64).ok()?.try_into().ok()
}

/// The JSON object in `bytes`, if that is what they are (handshake messages are small).
fn json_object(bytes: &[u8]) -> Option<serde_json::Map<String, serde_json::Value>> {
    match serde_json::from_slice::<serde_json::Value>(bytes).ok()? {
        serde_json::Value::Object(m) => Some(m),
        _ => None,
    }
}

/// The commitment in the initiator's first message. A JSON message of another protocol (v1 sent
/// its hello first) is [`Error::Version`].
fn parse_commit(bytes: &[u8]) -> Result<[u8; 32]> {
    match serde_json::from_slice::<Commit>(bytes) {
        Ok(c) if c.proto == PROTOCOL => decode32(&c.commit).ok_or_else(|| Error::Protocol("bad commitment".into())),
        Ok(_) => Err(Error::Version),
        Err(_) => match json_object(bytes) {
            Some(m) if m.get("proto").and_then(|p| p.as_str()) != Some(PROTOCOL) => Err(Error::Version),
            _ => Err(Error::Protocol("bad hello".into())),
        },
    }
}

/// The other side's hello: every field present and well-formed, nothing else in it. A JSON
/// message without a nonce is another protocol's hello ([`Error::Version`]).
fn parse_hello(bytes: &[u8]) -> Result<(Hello, [u8; 32])> {
    let bad = || Error::Protocol("bad hello".into());
    let hello: Hello = match serde_json::from_slice(bytes) {
        Ok(h) => h,
        Err(_) => return Err(if json_object(bytes).is_some_and(|m| !m.contains_key("nonce")) { Error::Version } else { bad() }),
    };
    if !(1..=64).contains(&hello.device.len()) || !hello.device.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(bad());
    }
    decode32(&hello.key).ok_or_else(bad)?;
    decode32(&hello.nonce).ok_or_else(bad)?;
    let eph = decode32(&hello.eph).ok_or_else(|| Error::Protocol("bad key exchange".into()))?;
    Ok((hello, eph))
}

/// One side of an established channel.
pub struct Channel {
    stream: TcpStream,
    send: ChaCha20Poly1305,
    recv: ChaCha20Poly1305,
    sent: u64,
    received: u64,
    /// The largest frame accepted now ([`MAX_SMALL_FRAME`] until [`Channel::allow_large_frames`]).
    max_frame: usize,
    /// When set, reads fail once this instant passes (see [`Channel::set_deadline`]).
    deadline: Option<Instant>,
    /// The per-read wait set by [`Channel::set_timeout`] (restored after [`Channel::wait_readable`]).
    read_timeout: Option<Duration>,
    /// The other device's id and public key (proved by its signature).
    pub peer_device: String,
    pub peer_key: String,
    /// The six-digit code both screens show while pairing.
    pub code: String,
}

impl std::fmt::Debug for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Channel").field("peer_device", &self.peer_device).finish_non_exhaustive()
    }
}

fn nonce(counter: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    for (dst, src) in n.iter_mut().skip(4).zip(counter.to_be_bytes()) {
        *dst = src;
    }
    n
}

impl Channel {
    /// Runs the handshake; `initiator` is the side that connected.
    /// The whole handshake must finish within [`HANDSHAKE_WITHIN`]; afterwards the channel keeps
    /// that deadline (reads fail past it) until the caller sets another.
    pub fn handshake(stream: TcpStream, me: &Identity, initiator: bool) -> Result<Channel> {
        Self::handshake_within(stream, me, initiator, HANDSHAKE_WITHIN)
    }

    /// As [`Channel::handshake`], with the time the whole handshake may take: a few seconds for
    /// a connection from a device not yet known, so it can't sit on a connection slot.
    pub fn handshake_within(mut stream: TcpStream, me: &Identity, initiator: bool, within: Duration) -> Result<Channel> {
        let within = within.max(Duration::from_millis(1));
        let deadline = Some(Instant::now() + within);
        stream.set_write_timeout(Some(within))?;
        let eph_secret = x25519_dalek::StaticSecret::from(random::<32>()?);
        let eph_public = x25519_dalek::PublicKey::from(&eph_secret);
        let mine = serde_json::to_vec(&Hello {
            device: me.device_id.clone(),
            key: me.public_key()?,
            eph: B64.encode(eph_public.as_bytes()),
            nonce: B64.encode(random::<32>()?),
        })
        .map_err(|e| Error::Protocol(e.to_string()))?;
        let (commit_msg, theirs) = if initiator {
            let commit_msg = serde_json::to_vec(&Commit { proto: PROTOCOL.to_string(), commit: B64.encode(commitment(&mine)) })
                .map_err(|e| Error::Protocol(e.to_string()))?;
            write_raw(&mut stream, &commit_msg)?;
            // the responder's hello arrives before ours is known to anyone
            let theirs = read_raw(&mut stream, MAX_HELLO, deadline)?;
            write_raw(&mut stream, &mine)?;
            (commit_msg, theirs)
        } else {
            let commit_msg = read_raw(&mut stream, MAX_HELLO, deadline)?;
            let commit = parse_commit(&commit_msg)?;
            write_raw(&mut stream, &mine)?;
            let theirs = read_raw(&mut stream, MAX_HELLO, deadline)?;
            if commitment(&theirs) != commit {
                return Err(Error::Protocol("the other device's hello doesn't match its commitment".into()));
            }
            (commit_msg, theirs)
        };
        let (hello, eph) = parse_hello(&theirs)?;
        if hello.device.eq_ignore_ascii_case(&me.device_id) {
            return Err(Error::Protocol("connected to itself".into()));
        }
        let (first, second) = if initiator { (&mine, &theirs) } else { (&theirs, &mine) };
        let transcript = transcript(&commit_msg, first, second);

        let role = |init: bool| -> Vec<u8> { [&transcript[..], &[u8::from(init)]].concat() };
        let my_sig = serde_json::to_vec(&Auth { sig: B64.encode(me.sign(&role(initiator))?) }).map_err(|e| Error::Protocol(e.to_string()))?;
        write_raw(&mut stream, &my_sig)?;
        let their_sig: Auth =
            serde_json::from_slice(&read_raw(&mut stream, MAX_HELLO, deadline)?).map_err(|_| Error::Protocol("bad signature".into()))?;
        let sig = B64.decode(&their_sig.sig).map_err(|_| Error::Protocol("bad signature".into()))?;
        if !verify(&hello.key, &role(!initiator), &sig) {
            return Err(Error::Protocol("the other device's signature doesn't check out".into()));
        }

        let shared = eph_secret.diffie_hellman(&x25519_dalek::PublicKey::from(eph));
        if !shared.was_contributory() {
            return Err(Error::Protocol("bad key exchange".into()));
        }
        let hk = hkdf::Hkdf::<Sha256>::new(Some(&transcript), shared.as_bytes());
        let mut keys = [0u8; 68];
        hk.expand(b"keys+code", &mut keys).map_err(|_| Error::Protocol("key derivation".into()))?;
        let (i2r, rest) = keys.split_at(32);
        let (r2i, code) = rest.split_at(32);
        let code = <[u8; 4]>::try_from(code).map(u32::from_be_bytes).map_err(|_| Error::Protocol("key derivation".into()))? % 1_000_000;
        let (send, recv) = if initiator { (i2r, r2i) } else { (r2i, i2r) };
        // writes after the handshake keep the usual patience, however short `within` was
        stream.set_write_timeout(Some(HANDSHAKE_WITHIN))?;
        Ok(Channel {
            stream,
            send: ChaCha20Poly1305::new(Key::from_slice(send)),
            recv: ChaCha20Poly1305::new(Key::from_slice(recv)),
            sent: 0,
            received: 0,
            max_frame: MAX_SMALL_FRAME,
            deadline,
            read_timeout: None,
            peer_device: hello.device.to_ascii_lowercase(),
            peer_key: hello.key,
            code: format!("{code:06}"),
        })
    }

    fn send_frame(&mut self, kind: u8, plaintext: &[u8]) -> Result<()> {
        if plaintext.len() > MAX_FRAME {
            return Err(Error::Protocol("frame too large".into()));
        }
        let n = nonce(self.sent);
        self.sent = self.sent.checked_add(1).ok_or_else(|| Error::Protocol("channel exhausted".into()))?;
        // the kind travels in the clear but is authenticated: one buffer, the ciphertext
        let ct = self
            .send
            .encrypt(Nonce::from_slice(&n), Payload { msg: plaintext, aad: &[kind] })
            .map_err(|_| Error::Protocol("encryption failed".into()))?;
        let len = u32::try_from(ct.len()).map_err(|_| Error::Protocol("frame too large".into()))?.to_be_bytes();
        self.stream.write_all(&[len[0], len[1], len[2], len[3], kind])?;
        self.stream.write_all(&ct)?;
        self.stream.flush()?;
        Ok(())
    }

    /// Sends a message frame (the other side receives [`Frame::Json`]).
    pub fn send(&mut self, plaintext: &[u8]) -> Result<()> {
        self.send_frame(KIND_JSON, plaintext)
    }

    /// Sends a binary frame (the other side receives [`Frame::Bytes`]): at most [`MAX_FRAME`]
    /// bytes, and the other side takes more than [`MAX_SMALL_FRAME`] only once it has called
    /// [`Channel::allow_large_frames`]. Nothing but the frame itself is buffered.
    pub fn send_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.send_frame(KIND_BYTES, bytes)
    }

    /// The next frame, of either kind. The length is checked against the current cap before
    /// anything is allocated; one frame is buffered, no more.
    pub fn recv_frame(&mut self) -> Result<Frame> {
        let len = read_len(&mut self.stream, self.max_frame.saturating_add(16), self.deadline)?;
        let mut kind = [0u8; 1];
        read_full(&mut self.stream, &mut kind, self.deadline)?;
        let [kind] = kind;
        if !matches!(kind, KIND_JSON | KIND_BYTES) {
            return Err(Error::Protocol("a frame of an unknown kind".into()));
        }
        let mut ct = vec![0u8; len];
        read_full(&mut self.stream, &mut ct, self.deadline)?;
        let n = nonce(self.received);
        self.received = self.received.checked_add(1).ok_or_else(|| Error::Protocol("channel exhausted".into()))?;
        let plain = self
            .recv
            .decrypt(Nonce::from_slice(&n), Payload { msg: &ct, aad: &[kind] })
            .map_err(|_| Error::Protocol("a message failed to decrypt".into()))?;
        Ok(if kind == KIND_BYTES { Frame::Bytes(plain) } else { Frame::Json(plain) })
    }

    /// The next message frame; a binary frame here is an error.
    pub fn recv(&mut self) -> Result<Vec<u8>> {
        match self.recv_frame()? {
            Frame::Json(bytes) => Ok(bytes),
            Frame::Bytes(_) => Err(Error::Protocol("unexpected message: binary data".into())),
        }
    }

    pub fn send_json<T: Serialize>(&mut self, value: &T) -> Result<()> {
        let bytes = serde_json::to_vec(value).map_err(|e| Error::Protocol(e.to_string()))?;
        self.send(&bytes)
    }

    pub fn recv_json<T: for<'de> Deserialize<'de>>(&mut self) -> Result<T> {
        let bytes = self.recv()?;
        serde_json::from_slice(&bytes).map_err(|e| Error::Protocol(format!("unexpected message: {e}")))
    }

    /// Limits how long a read may wait (each read on its own; clears any overall deadline).
    pub fn set_timeout(&mut self, t: Option<Duration>) -> Result<()> {
        self.deadline = None;
        self.read_timeout = t;
        self.stream.set_read_timeout(t)?;
        Ok(())
    }

    /// Every read from now on must finish within `within` of now, in total.
    pub fn set_deadline(&mut self, within: Duration) {
        self.deadline = Some(Instant::now() + within);
    }

    /// Accepts frames up to [`MAX_FRAME`]: only once the other device is known to be paired.
    pub fn allow_large_frames(&mut self) {
        self.max_frame = MAX_FRAME;
    }

    /// Waits up to `wait` for the other side to send something, without reading it: `true` when
    /// a frame (or its beginning) is there to receive, `false` when nothing came. A closed
    /// connection is an error. Lets one thread watch the channel while it waits for something else.
    pub fn wait_readable(&mut self, wait: Duration) -> Result<bool> {
        self.stream.set_read_timeout(Some(wait.max(Duration::from_millis(1))))?;
        let mut byte = [0u8; 1];
        let seen = self.stream.peek(&mut byte);
        self.stream.set_read_timeout(self.read_timeout)?;
        match seen {
            Ok(0) => Err(Error::Io("the connection closed".into())),
            Ok(_) => Ok(true),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => {
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn pair() -> (Result<Channel>, Result<Channel>, Identity, Identity) {
        let a = Identity::generate("A").unwrap();
        let b = Identity::generate("B").unwrap();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let b2 = b.clone();
        let t = std::thread::spawn(move || Channel::handshake(l.accept().unwrap().0, &b2, false));
        let ca = Channel::handshake(TcpStream::connect(addr).unwrap(), &a, true);
        (ca, t.join().unwrap(), a, b)
    }

    /// A well-formed hello of `who`, with `extra` added to it.
    fn hello_of(who: &Identity, extra: &[(&str, serde_json::Value)]) -> Vec<u8> {
        let eph = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([7u8; 32]));
        let mut v = serde_json::json!({
            "device": who.device_id, "key": who.public_key().unwrap(), "eph": B64.encode(eph.as_bytes()), "nonce": B64.encode([9u8; 32]),
        });
        for (k, val) in extra {
            if val.is_null() {
                v.as_object_mut().unwrap().remove(*k);
            } else {
                v[*k] = val.clone();
            }
        }
        serde_json::to_vec(&v).unwrap()
    }

    fn commit_of(hello: &[u8]) -> Vec<u8> {
        serde_json::to_vec(&Commit { proto: PROTOCOL.into(), commit: B64.encode(commitment(hello)) }).unwrap()
    }

    /// What a real responder makes of an initiator that sends `commit`, then `reveal`.
    fn responder_sees(commit: Vec<u8>, reveal: Vec<u8>) -> Error {
        let me = Identity::generate("B").unwrap();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let mut s = TcpStream::connect(addr).unwrap();
            let _ = write_raw(&mut s, &commit);
            let _ = read_raw(&mut s, MAX_HELLO, Some(Instant::now() + Duration::from_secs(5)));
            let _ = write_raw(&mut s, &reveal);
            // stays open until the responder has decided
            let _ = read_raw(&mut s, MAX_HELLO, Some(Instant::now() + Duration::from_secs(5)));
        });
        let r = Channel::handshake_within(l.accept().unwrap().0, &me, false, Duration::from_secs(5));
        drop(l);
        t.join().unwrap();
        r.err().unwrap_or_else(|| panic!("the handshake went through"))
    }

    /// What a real initiator makes of a responder that answers its commitment with `answer`.
    fn initiator_sees(answer: Vec<u8>) -> Error {
        let me = Identity::generate("A").unwrap();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let mut s = l.accept().unwrap().0;
            let _ = read_raw(&mut s, MAX_HELLO, Some(Instant::now() + Duration::from_secs(5)));
            let _ = write_raw(&mut s, &answer);
            let _ = read_raw(&mut s, MAX_HELLO, Some(Instant::now() + Duration::from_secs(5)));
            let _ = read_raw(&mut s, MAX_HELLO, Some(Instant::now() + Duration::from_secs(5)));
        });
        let r = Channel::handshake_within(TcpStream::connect(addr).unwrap(), &me, true, Duration::from_secs(5));
        t.join().unwrap();
        r.err().unwrap_or_else(|| panic!("the handshake went through"))
    }

    #[test]
    fn both_sides_agree_and_messages_flow_both_ways() {
        let (ca, cb, a, b) = pair();
        let (mut ca, mut cb) = (ca.unwrap(), cb.unwrap());
        assert_eq!(ca.code, cb.code);
        assert_eq!(ca.code.len(), 6);
        assert_eq!(ca.peer_device, b.device_id);
        assert_eq!(cb.peer_key, a.public_key().unwrap());
        ca.send(b"one").unwrap();
        ca.send(b"two").unwrap();
        assert_eq!(cb.recv().unwrap(), b"one");
        assert_eq!(cb.recv().unwrap(), b"two");
        cb.send(b"back").unwrap();
        assert_eq!(ca.recv().unwrap(), b"back");
    }

    /// The point of the commitment: the initiator can't pick its hello after seeing the
    /// responder's. A reveal other than the committed hello ends the handshake before the
    /// responder signs or derives anything.
    #[test]
    fn a_hello_other_than_the_committed_one_is_refused() {
        let m = Identity::generate("M").unwrap();
        let committed = hello_of(&m, &[]);
        // another nonce, another key exchange value, or a commitment over nothing at all
        let other_nonce = hello_of(&m, &[("nonce", serde_json::json!(B64.encode([1u8; 32])))]);
        let other_eph = hello_of(&m, &[("eph", serde_json::json!(B64.encode([5u8; 32])))]);
        for reveal in [other_nonce, other_eph] {
            let e = responder_sees(commit_of(&committed), reveal);
            assert!(e.to_string().contains("commitment"), "{e}");
        }
        let e = responder_sees(serde_json::to_vec(&Commit { proto: PROTOCOL.into(), commit: B64.encode([0u8; 32]) }).unwrap(), committed.clone());
        assert!(e.to_string().contains("commitment"), "{e}");
        // the committed hello itself passes the commitment check (and then fails at the signature)
        let e = responder_sees(commit_of(&committed), committed);
        assert!(!e.to_string().contains("commitment"), "{e}");
    }

    /// Hellos carry exactly the fields this version knows: nothing extra to vary, none missing,
    /// each one well-formed. Each case commits to what it sends, so only the hello is at fault.
    #[test]
    fn hostile_hellos_are_refused_by_both_sides() {
        let m = Identity::generate("M").unwrap();
        let j = |v: serde_json::Value| v;
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("an unknown field", hello_of(&m, &[("junk", j(serde_json::json!(1)))])),
            ("a nested unknown field", hello_of(&m, &[("x", j(serde_json::json!({"a": [1, 2, {"b": null}]})))])),
            ("no device", hello_of(&m, &[("device", serde_json::Value::Null)])),
            ("no key", hello_of(&m, &[("key", serde_json::Value::Null)])),
            ("no eph", hello_of(&m, &[("eph", serde_json::Value::Null)])),
            ("a device id that is a path", hello_of(&m, &[("device", j(serde_json::json!("../../etc")))])),
            ("an empty device id", hello_of(&m, &[("device", j(serde_json::json!("")))])),
            ("a very long device id", hello_of(&m, &[("device", j(serde_json::json!("a".repeat(200))))])),
            ("a number for a key", hello_of(&m, &[("key", j(serde_json::json!(7)))])),
            ("a short key", hello_of(&m, &[("key", j(serde_json::json!("AAAA")))])),
            ("a short nonce", hello_of(&m, &[("nonce", j(serde_json::json!("AAAA")))])),
            ("a nonce that isn't base64", hello_of(&m, &[("nonce", j(serde_json::json!("!!!")))])),
            ("a short key exchange value", hello_of(&m, &[("eph", j(serde_json::json!("AAAA")))])),
            ("an all-zero key exchange value", hello_of(&m, &[("eph", j(serde_json::json!(B64.encode([0u8; 32]))))])),
            ("an array", b"[1,2,3]".to_vec()),
            ("not JSON", b"\xff\xfe\x00".to_vec()),
            ("nothing", Vec::new()),
            ("deep nesting", [vec![b'['; 3000], vec![b']'; 1000]].concat()),
        ];
        for (what, hello) in cases {
            let e = responder_sees(commit_of(&hello), hello.clone());
            assert!(!e.to_string().is_empty(), "{what}");
            let e = initiator_sees(hello);
            assert!(!e.to_string().is_empty(), "{what}");
        }
        // too large to be read at all
        let big = vec![b'x'; MAX_HELLO + 1];
        assert!(responder_sees(big.clone(), Vec::new()).to_string().contains("refused"));
        assert!(initiator_sees(big).to_string().contains("refused"));
    }

    /// Hostile first messages: a commitment that isn't one, of the right protocol.
    #[test]
    fn hostile_commitments_are_refused() {
        let cases: Vec<Vec<u8>> = vec![
            serde_json::to_vec(&serde_json::json!({"proto": PROTOCOL, "commit": "AAAA"})).unwrap(),
            serde_json::to_vec(&serde_json::json!({"proto": PROTOCOL, "commit": 5})).unwrap(),
            serde_json::to_vec(&serde_json::json!({"proto": PROTOCOL})).unwrap(),
            serde_json::to_vec(&serde_json::json!({"proto": PROTOCOL, "commit": B64.encode([0u8; 32]), "more": 1})).unwrap(),
            b"\"lcc-peer-v2\"".to_vec(),
            b"{".to_vec(),
            Vec::new(),
        ];
        for commit in cases {
            let e = responder_sees(commit.clone(), Vec::new());
            assert!(matches!(e, Error::Protocol(_)), "{:?}: {e:?}", String::from_utf8_lossy(&commit));
        }
    }

    /// An old build (hellos first, no commitment, no nonce) and a new one fail cleanly, and the
    /// error says what to do about it.
    #[test]
    fn another_protocol_version_is_told_apart() {
        let old = Identity::generate("Old").unwrap();
        // what version 1 sent first: its hello
        let v1_hello = hello_of(&old, &[("nonce", serde_json::Value::Null)]);
        let e = responder_sees(v1_hello.clone(), Vec::new());
        assert_eq!(e, Error::Version);
        assert!(e.to_string().contains("same app version"), "{e}");
        // a later version's commitment
        let v3 = serde_json::to_vec(&serde_json::json!({"proto": "lcc-peer-v3", "commit": B64.encode([0u8; 32])})).unwrap();
        assert_eq!(responder_sees(v3, Vec::new()), Error::Version);
        let v3 = serde_json::to_vec(&serde_json::json!({"proto": "lcc-peer-v3", "commit": B64.encode([0u8; 32]), "suite": 2})).unwrap();
        assert_eq!(responder_sees(v3, Vec::new()), Error::Version);
        // a responder that answers a commitment with a hello without a nonce
        assert_eq!(initiator_sees(v1_hello), Error::Version);
    }

    #[test]
    fn a_tampered_frame_fails_to_decrypt() {
        let (ca, cb, _, _) = pair();
        let (ca, mut cb) = (ca.unwrap(), cb.unwrap());
        // bypass the cipher: a raw frame of garbage where a sealed one belongs
        let mut raw = ca.stream.try_clone().unwrap();
        raw.write_all(&[0, 0, 0, 40, KIND_JSON]).unwrap();
        raw.write_all(&[7u8; 40]).unwrap();
        assert!(cb.recv().is_err());
    }

    #[test]
    fn garbage_instead_of_a_handshake_is_an_error() {
        let me = Identity::generate("A").unwrap();
        for junk in [&b"\x00\x00\x00\x05hello"[..], b"\xff\xff\xff\xff", b"\x00\x00\x00\x02{}"] {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = l.local_addr().unwrap();
            let junk = junk.to_vec();
            let t = std::thread::spawn(move || {
                let mut s = TcpStream::connect(addr).unwrap();
                let _ = s.write_all(&junk);
            });
            let r = Channel::handshake(l.accept().unwrap().0, &me, false);
            t.join().unwrap();
            assert!(r.is_err());
        }
    }

    #[test]
    fn an_unpaired_peer_cant_send_a_large_frame_until_allowed() {
        let (ca, cb, _, _) = pair();
        let (mut ca, mut cb) = (ca.unwrap(), cb.unwrap());
        // only the 4-byte length: refused before any 64 MiB buffer is made or read
        let mut raw = ca.stream.try_clone().unwrap();
        raw.write_all(&((MAX_FRAME as u32).to_be_bytes())).unwrap();
        let err = cb.recv().unwrap_err();
        assert!(err.to_string().contains("refused"), "{err}");

        let (ca2, cb2, _, _) = pair();
        let (mut ca2, mut cb2) = (ca2.unwrap(), cb2.unwrap());
        cb2.allow_large_frames();
        let big = vec![3u8; MAX_SMALL_FRAME * 4];
        let t = std::thread::spawn(move || {
            ca2.send(&big).unwrap();
            ca2
        });
        assert_eq!(cb2.recv().unwrap().len(), MAX_SMALL_FRAME * 4);
        t.join().unwrap();
        let _ = ca.send(b"x");
    }

    /// Binary frames and message frames travel on one channel and arrive as what they were sent.
    #[test]
    fn binary_and_message_frames_are_told_apart() {
        let (ca, cb, _, _) = pair();
        let (mut ca, mut cb) = (ca.unwrap(), cb.unwrap());
        ca.send_json(&serde_json::json!({"t": "chunk", "len": 3})).unwrap();
        ca.send_bytes(&[1, 2, 3]).unwrap();
        ca.send_bytes(&[]).unwrap();
        ca.send(b"{}").unwrap();
        assert!(matches!(cb.recv_frame().unwrap(), Frame::Json(j) if j.starts_with(b"{")));
        assert_eq!(cb.recv_frame().unwrap(), Frame::Bytes(vec![1, 2, 3]));
        assert_eq!(cb.recv_frame().unwrap(), Frame::Bytes(Vec::new()));
        assert_eq!(cb.recv_frame().unwrap(), Frame::Json(b"{}".to_vec()));
        // binary data where a message is expected is an error, not a parse of file bytes
        ca.send_bytes(b"{\"t\":\"hello\"}").unwrap();
        let e = cb.recv_json::<serde_json::Value>().unwrap_err();
        assert!(e.to_string().contains("binary"), "{e}");
        // the channel is still in step afterwards
        cb.send_bytes(b"back").unwrap();
        assert_eq!(ca.recv_frame().unwrap(), Frame::Bytes(b"back".to_vec()));
    }

    /// The caps hold for binary frames as for messages, and the kind can't be changed on the way.
    #[test]
    fn hostile_binary_frames_are_refused() {
        // over the small cap before the other device is known to be paired
        let (ca, cb, _, _) = pair();
        let (mut ca, mut cb) = (ca.unwrap(), cb.unwrap());
        let big = vec![0u8; MAX_SMALL_FRAME + 1];
        let t = std::thread::spawn(move || {
            let _ = ca.send_bytes(&big);
            ca
        });
        assert!(cb.recv_frame().unwrap_err().to_string().contains("refused"));
        let mut ca = t.join().unwrap();
        // larger than any frame may be: refused by the sender itself
        assert!(ca.send_bytes(&vec![0u8; MAX_FRAME + 1]).is_err());

        // an unknown kind, and a sealed message relabelled as binary on the way
        let (ca, cb, _, _) = pair();
        let (ca, mut cb) = (ca.unwrap(), cb.unwrap());
        let mut raw = ca.stream.try_clone().unwrap();
        raw.write_all(&[0, 0, 0, 16, 9]).unwrap();
        raw.write_all(&[0u8; 16]).unwrap();
        assert!(cb.recv_frame().unwrap_err().to_string().contains("unknown kind"));

        let (ca, cb, _, _) = pair();
        let (ca, mut cb) = (ca.unwrap(), cb.unwrap());
        let sealed = ca.send.encrypt(Nonce::from_slice(&nonce(0)), Payload { msg: b"{}", aad: &[KIND_JSON] }).unwrap();
        let mut raw = ca.stream.try_clone().unwrap();
        raw.write_all(&[0, 0, 0, sealed.len() as u8, KIND_BYTES]).unwrap();
        raw.write_all(&sealed).unwrap();
        assert!(cb.recv_frame().unwrap_err().to_string().contains("decrypt"));
    }

    #[test]
    fn waiting_for_the_other_side_reads_nothing() {
        let (ca, cb, _, _) = pair();
        let (mut ca, mut cb) = (ca.unwrap(), cb.unwrap());
        let started = Instant::now();
        assert!(!cb.wait_readable(Duration::from_millis(50)).unwrap());
        assert!(started.elapsed() < Duration::from_secs(2));
        ca.send(b"now").unwrap();
        assert!(cb.wait_readable(Duration::from_secs(5)).unwrap());
        assert!(cb.wait_readable(Duration::from_secs(5)).unwrap(), "nothing was consumed");
        assert_eq!(cb.recv().unwrap(), b"now");
        assert!(!cb.wait_readable(Duration::from_millis(20)).unwrap());
        drop(ca);
        assert!(cb.wait_readable(Duration::from_secs(5)).is_err());
    }

    /// A slow-loris peer trickling one byte at a time can't hold a handshake past its deadline.
    #[test]
    fn a_trickling_peer_is_cut_off_at_the_handshake_deadline() {
        let me = Identity::generate("A").unwrap();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let mut s = TcpStream::connect(addr).unwrap();
            // a length that never completes, one byte every 30 ms for 1.5 s
            for b in [0u8, 0, 0x01, 0].into_iter().chain(std::iter::repeat(b'{')).take(50) {
                if s.write_all(&[b]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        });
        let started = Instant::now();
        let r = Channel::handshake_within(l.accept().unwrap().0, &me, false, Duration::from_millis(250));
        assert!(r.is_err());
        assert!(started.elapsed() < Duration::from_secs(3), "took {:?}", started.elapsed());
        t.join().unwrap();
    }

    /// Connecting to a host that accepts but never answers doesn't hang the initiator.
    #[test]
    fn a_silent_listener_times_out_the_outgoing_handshake() {
        let me = Identity::generate("A").unwrap();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let s = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let _held = l.accept().unwrap();
        let started = Instant::now();
        assert!(Channel::handshake_within(s, &me, true, Duration::from_millis(150)).is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn a_deadline_bounds_reads_after_the_handshake() {
        let (ca, cb, _, _) = pair();
        let (ca, mut cb) = (ca.unwrap(), cb.unwrap());
        cb.set_deadline(Duration::from_millis(200));
        let mut raw = ca.stream.try_clone().unwrap();
        let t = std::thread::spawn(move || {
            raw.write_all(&[0, 0, 0, 40, KIND_JSON]).unwrap();
            for _ in 0..30 {
                if raw.write_all(&[1]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        });
        let started = Instant::now();
        assert!(cb.recv().is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(cb);
        t.join().unwrap();
    }
}
