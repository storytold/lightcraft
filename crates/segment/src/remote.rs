//! Private SAM 3 transport. Bind the worker to loopback and forward it over SSH, or set a shared
//! [`TOKEN_ENV`] on both ends to reach it over a network you trust.
//!
//! With a token both ends prove they know it before any pixels move: the client sends a nonce, the
//! worker answers with its own nonce and an HMAC-SHA256 of both, the client checks that and answers
//! with its own HMAC. The token itself never crosses the socket.
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::{Click, Encoded, INPUT, MASK_SIDE, Sam3};
use serde::{Deserialize, Serialize};

const CONTROL_LIMIT: usize = 16 * 1024;
const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(9 * 60);
pub const ENV: &str = "LIGHTCRAFT_SAM3_REMOTE";
/// The secret both ends share. Without it they stay on loopback. Not `LIGHTCRAFT_SAM3_TOKEN`: that
/// is the bearer token of a gated download mirror.
pub const TOKEN_ENV: &str = "LIGHTCRAFT_SAM3_WORKER_TOKEN";
/// Shortest [`TOKEN_ENV`] the worker takes: it is all that guards the port.
const TOKEN_MIN: usize = 16;
/// How long a new connection has to prove the token, so that a silent one doesn't hold the
/// worker, which serves one client at a time.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Total TCP and authentication budget across all resolved addresses. DNS has its own five seconds.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Serialize, Deserialize)]
pub enum Operation {
    Prepare,
    Clicks(Vec<[f32; 3]>),
    Text(String),
    /// The photo's subject among its people and animals ([`Sam3::segment_subject`]), from where
    /// the camera focused (normalized `[x, y]`) when the file records it.
    Subject(Option<[f32; 2]>),
}

#[derive(Serialize, Deserialize)]
pub struct Request {
    pub width: usize,
    pub height: usize,
    pub temporary: bool,
    pub operation: Operation,
}

impl Request {
    pub fn validate(&self) -> Result<(), String> {
        if self.width > INPUT || self.height > INPUT || (self.width == 0) != (self.height == 0) {
            return Err("invalid SAM image dimensions".into());
        }
        if self.temporary && self.width == 0 {
            return Err("detail pass requires an image".into());
        }
        match &self.operation {
            Operation::Prepare | Operation::Subject(None) => {}
            Operation::Subject(Some(f)) if f.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) => {}
            Operation::Text(text) if text.len() <= 4096 && text.split([',', ';']).any(|p| !p.trim().is_empty()) => {}
            Operation::Clicks(clicks)
                if !clicks.is_empty()
                    && clicks.len() <= 64
                    && clicks.iter().all(|c| {
                        c.iter().all(|v| v.is_finite()) && (0.0..=1.0).contains(&c[0]) && (0.0..=1.0).contains(&c[1]) && (c[2] == 0.0 || c[2] == 1.0)
                    })
                    && clicks.iter().any(|c| c[2] == 1.0) => {}
            _ => return Err("invalid SAM prompt".into()),
        }
        Ok(())
    }
}

pub fn configured() -> Option<String> {
    std::env::var(ENV).ok().filter(|v| !v.trim().is_empty())
}

/// The shared secret in the environment, if any.
pub fn token() -> Option<String> {
    std::env::var(TOKEN_ENV).ok().filter(|v| !v.trim().is_empty())
}

pub fn read_frame(reader: &mut impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut size = [0; 4];
    reader.read_exact(&mut size)?;
    let size = u32::from_be_bytes(size) as usize;
    if size > limit {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "SAM frame exceeds limit"));
    }
    let mut data = vec![0; size];
    reader.read_exact(&mut data)?;
    Ok(data)
}

fn write_frame(writer: &mut impl Write, data: &[u8]) -> io::Result<()> {
    let size = u32::try_from(data.len()).map_err(io::Error::other)?;
    writer.write_all(&size.to_be_bytes())?;
    writer.write_all(data)
}

/// Bound the caller's DNS wait and allow at most one outstanding system lookup.
fn resolve(endpoint: &str) -> Result<Vec<SocketAddr>, String> {
    use std::sync::{Mutex, TryLockError, mpsc};
    type Lookup = mpsc::Receiver<Result<Vec<SocketAddr>, String>>;
    static LOOKUP: Mutex<Option<Lookup>> = Mutex::new(None);
    if let Ok(address) = endpoint.parse::<SocketAddr>() {
        return Ok(vec![address]);
    }
    let mut lookup = match LOOKUP.try_lock() {
        Ok(lookup) => lookup,
        Err(TryLockError::Poisoned(error)) => error.into_inner(),
        Err(TryLockError::WouldBlock) => return Err("SAM hostname resolution already in progress".into()),
    };
    if let Some(previous) = lookup.as_ref() {
        match previous.try_recv() {
            Err(mpsc::TryRecvError::Empty) => return Err("SAM hostname resolution still in progress".into()),
            _ => *lookup = None,
        }
    }
    let target = endpoint.to_owned();
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("sam-dns".into())
        .spawn(move || {
            let result = target.to_socket_addrs().map_err(|e| e.to_string()).and_then(|addresses| {
                let addresses: Vec<_> = addresses.take(65).collect();
                if addresses.len() > 64 { Err("too many SAM endpoint addresses".into()) } else { Ok(addresses) }
            });
            let _ = sender.send(result);
        })
        .map_err(|e| e.to_string())?;
    *lookup = Some(receiver);
    let result = wait_for_lookup(lookup.as_ref().ok_or("SAM resolver unavailable")?, Duration::from_secs(5));
    match result {
        Ok(result) => {
            *lookup = None;
            result.map_err(|e| format!("SAM endpoint must be a host:port: {e}"))
        }
        Err(error) => Err(format!("SAM hostname resolution: {error}")),
    }
}

/// Wait only for the caller's DNS budget; a stalled system resolver cannot extend it.
fn wait_for_lookup(
    receiver: &std::sync::mpsc::Receiver<Result<Vec<SocketAddr>, String>>,
    timeout: Duration,
) -> Result<Result<Vec<SocketAddr>, String>, std::sync::mpsc::RecvTimeoutError> {
    receiver.recv_timeout(timeout)
}

/// Apply the remaining handshake budget before every socket operation, including partial frames.
struct Deadline<'a> {
    stream: &'a mut TcpStream,
    expires: Instant,
}

impl<'a> Deadline<'a> {
    fn new(stream: &'a mut TcpStream, timeout: Duration) -> Self {
        Self { stream, expires: Instant::now() + timeout }
    }

    fn remaining(&self) -> io::Result<Duration> {
        self.expires
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "SAM handshake deadline expired"))
    }
}

impl Read for Deadline<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        let count = self.stream.read(buffer)?;
        self.remaining()?;
        Ok(count)
    }
}

impl Write for Deadline<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        let count = self.stream.write(buffer)?;
        self.remaining()?;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

pub struct Client {
    stream: TcpStream,
    pub key: Option<u64>,
}

impl Client {
    /// Connect using the configured shared token, if present.
    pub fn connect(endpoint: &str) -> Result<Self, String> {
        Self::connect_with(endpoint, token().as_deref())
    }

    /// Validate configuration before resolving a hostname or opening a socket.
    fn connect_with(endpoint: &str, token: Option<&str>) -> Result<Self, String> {
        validate_token(token)?;
        let addresses = resolve(endpoint)?;
        Self::connect_addresses(endpoint, &addresses, token)
    }

    /// Try every resolved address, including when a reachable peer fails authentication.
    fn connect_addresses(endpoint: &str, addresses: &[SocketAddr], token: Option<&str>) -> Result<Self, String> {
        Self::connect_until(endpoint, addresses, token, Instant::now() + CONNECT_TIMEOUT)
    }

    /// Share one deadline across TCP attempts and authentication, even with many DNS results.
    fn connect_until(endpoint: &str, addresses: &[SocketAddr], token: Option<&str>, expires: Instant) -> Result<Self, String> {
        if token.is_none() && addresses.iter().any(|a| !a.ip().is_loopback()) {
            return Err(format!("SAM endpoint off this machine needs {TOKEN_ENV}; or use an SSH tunnel for remote access"));
        }
        let mut last = format!("SAM endpoint {endpoint} has no address");
        for address in addresses {
            let remaining = expires.checked_duration_since(Instant::now()).filter(|budget| !budget.is_zero());
            let Some(remaining) = remaining else {
                return Err(format!("SAM connection deadline expired: {last}"));
            };
            match TcpStream::connect_timeout(address, remaining.min(Duration::from_secs(5))) {
                Ok(mut stream) => {
                    stream.set_write_timeout(Some(Duration::from_secs(30))).map_err(|e| e.to_string())?;
                    if let Some(token) = token {
                        let mut handshake = Deadline::new(&mut stream, HELLO_TIMEOUT);
                        handshake.expires = handshake.expires.min(expires);
                        if let Err(error) = say_hello(&mut handshake, token) {
                            if Instant::now() >= expires {
                                return Err("SAM connection deadline expired during handshake".into());
                            }
                            last = error;
                            continue;
                        }
                    }
                    stream.set_write_timeout(Some(Duration::from_secs(30))).map_err(|e| e.to_string())?;
                    stream.set_read_timeout(Some(TIMEOUT)).map_err(|e| e.to_string())?;
                    return Ok(Self { stream, key: None });
                }
                Err(e) => last = e.to_string(),
            }
        }
        Err(last)
    }

    pub fn request(&mut self, request: &Request, rgb: &[u8]) -> Result<Option<Vec<f32>>, String> {
        request.validate()?;
        if rgb.len() != request.width * request.height * 3 {
            return Err("SAM RGB buffer size does not match dimensions".into());
        }
        let bytes = serde_json::to_vec(request).map_err(|e| e.to_string())?;
        write_frame(&mut self.stream, &bytes).map_err(|e| e.to_string())?;
        write_frame(&mut self.stream, rgb).map_err(|e| e.to_string())?;
        let response = read_frame(&mut self.stream, RESPONSE_LIMIT).map_err(|e| e.to_string())?;
        let result: Result<Option<Vec<f32>>, String> = serde_json::from_slice(&response).map_err(|e| e.to_string())?;
        let logits = result?;
        if logits.as_ref().is_some_and(|v| v.len() != MASK_SIDE * MASK_SIDE || v.iter().any(|n| !n.is_finite())) {
            return Err("invalid SAM mask response".into());
        }
        Ok(logits)
    }
}

/// A nonce of 128 unpredictable bits, as hex. `RandomState` is keyed from the operating system's
/// random source, so this needs no random-number crate.
fn nonce() -> String {
    use std::hash::{BuildHasher, Hasher};
    let word = || {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        hasher.finish()
    };
    format!("{:016x}{:016x}", word(), word())
}

/// HMAC-SHA256 (RFC 2104) under `key` of `parts`, each prefixed by its length, as hex.
fn hmac(key: &str, parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut block = [0u8; 64];
    if key.len() > block.len() {
        block[..32].copy_from_slice(&Sha256::digest(key.as_bytes()));
    } else {
        block[..key.len()].copy_from_slice(key.as_bytes());
    }
    let pad = |byte: u8| block.map(|b| b ^ byte);
    let mut inner = Sha256::new();
    inner.update(pad(0x36));
    for part in parts {
        inner.update((part.len() as u64).to_be_bytes());
        inner.update(part.as_bytes());
    }
    let mut outer = Sha256::new();
    outer.update(pad(0x5c));
    outer.update(inner.finalize());
    outer.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Compares every byte, so the time taken doesn't tell how much of a guess was right.
fn same_proof(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |difference, (x, y)| difference | (x ^ y)) == 0
}

#[derive(Serialize, Deserialize)]
struct Proof {
    nonce: String,
    proof: String,
}

/// Socket timeouts indicate a spent time budget, not a failed token proof.
fn handshake_io_error(error: io::Error) -> String {
    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => "SAM handshake timed out: time budget exhausted".into(),
        io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted | io::ErrorKind::BrokenPipe => {
            format!("SAM handshake connection closed early: {error}; {}", token_hint())
        }
        _ => format!("SAM handshake: {error}"),
    }
}

/// Serialize one bounded handshake message into the framed transport.
fn send<T: Serialize>(writer: &mut impl Write, value: &T) -> Result<(), String> {
    write_frame(writer, &serde_json::to_vec(value).map_err(|e| e.to_string())?).map_err(handshake_io_error)
}

/// Decode one handshake message, rejecting oversized or malformed frames.
fn receive<T: serde::de::DeserializeOwned>(reader: &mut impl Read) -> Result<T, String> {
    let frame = read_frame(reader, CONTROL_LIMIT).map_err(handshake_io_error)?;
    serde_json::from_slice(&frame).map_err(|_| format!("unexpected SAM handshake; {}", token_hint()))
}

/// The client's half of the handshake: the worker proves the token before any pixels go.
fn say_hello(stream: &mut (impl Read + Write), token: &str) -> Result<(), String> {
    let mine = nonce();
    send(stream, &mine)?;
    let reply: Proof = receive(stream)?;
    if reply.nonce.len() < 32 || !same_proof(&reply.proof, &hmac(token, &["worker", &mine, &reply.nonce])) {
        return Err(format!("SAM worker doesn't know the token; {}", token_hint()));
    }
    send(stream, &hmac(token, &["client", &mine, &reply.nonce]))
}

/// The worker's half of the handshake: the client proves the token before a request is read.
fn check_hello(stream: &mut (impl Read + Write), token: &str) -> Result<(), String> {
    let theirs: String = receive(stream)?;
    if theirs.len() < 32 {
        return Err(format!("unexpected SAM handshake; {}", token_hint()));
    }
    let mine = nonce();
    send(stream, &Proof { nonce: mine.clone(), proof: hmac(token, &["worker", &theirs, &mine]) })?;
    let proof: String = receive(stream)?;
    if same_proof(&proof, &hmac(token, &["client", &theirs, &mine])) { Ok(()) } else { Err(format!("wrong SAM worker token; {}", token_hint())) }
}

pub fn text_logits(model: &mut Sam3, enc: &mut Encoded, text: &str) -> crate::Result<Option<Vec<f32>>> {
    let mut merged: Option<Vec<f32>> = None;
    for phrase in text.split([',', ';']).map(str::trim).filter(|p| !p.is_empty()) {
        if let Some(p) = model.segment_text(enc, phrase, 0.5)? {
            if let Some(m) = &mut merged {
                for (a, b) in m.iter_mut().zip(p.logits) {
                    *a = a.max(b);
                }
            } else {
                merged = Some(p.logits);
            }
        }
    }
    Ok(merged)
}

fn predict(model: &mut Sam3, enc: &mut Encoded, operation: Operation) -> Result<Option<Vec<f32>>, String> {
    match operation {
        Operation::Prepare => {
            model.prepare_clicks(enc).map_err(|e| e.to_string())?;
            Ok(None)
        }
        Operation::Text(text) => text_logits(model, enc, &text).map_err(|e| e.to_string()),
        // (the reference score threshold, as a local Find Subject uses)
        Operation::Subject(focus) => {
            let p = model.segment_subject(enc, focus.map(|f| (f[0], f[1])), 0.5).map_err(|e| e.to_string())?;
            Ok(p.map(|p| p.logits))
        }
        Operation::Clicks(points) => {
            let clicks: Vec<_> = points.into_iter().map(|p| Click { x: p[0], y: p[1], positive: p[2] == 1.0 }).collect();
            Ok(Some(model.segment_clicks(enc, &clicks).map_err(|e| e.to_string())?.logits))
        }
    }
}

/// Authenticate before accepting image data, then serve requests with a per-session model cache.
fn serve_client(stream: &mut TcpStream, dir: &Path, device: candle_core::Device, token: Option<&str>) -> Result<(), String> {
    stream.set_write_timeout(Some(Duration::from_secs(30))).map_err(|e| e.to_string())?;
    if let Some(token) = token {
        check_hello(&mut Deadline::new(stream, HELLO_TIMEOUT), token)?;
        stream.set_write_timeout(Some(Duration::from_secs(30))).map_err(|e| e.to_string())?;
    }
    stream.set_read_timeout(Some(Duration::from_secs(10 * 60))).map_err(|e| e.to_string())?;
    let mut model = None;
    let mut cache = None;
    loop {
        let frame = read_frame(stream, CONTROL_LIMIT).map_err(|e| e.to_string())?;
        let request: Request = serde_json::from_slice(&frame).map_err(|e| e.to_string())?;
        request.validate()?;
        let rgb = read_frame(stream, request.width * request.height * 3).map_err(|e| e.to_string())?;
        if rgb.len() != request.width * request.height * 3 {
            return Err("invalid RGB payload".into());
        }
        let started = std::time::Instant::now();
        let result = (|| {
            if model.is_none() {
                model = Some(Sam3::load_on(dir, device.clone()).map_err(|e| e.to_string())?);
            }
            let m = model.as_mut().ok_or("SAM model unavailable")?;
            let mut temporary = None;
            if request.width > 0 {
                let encoded = m.encode(&rgb, request.width, request.height).map_err(|e| e.to_string())?;
                if request.temporary {
                    temporary = Some(encoded);
                } else {
                    cache = Some(encoded);
                }
            }
            let enc = if request.temporary { temporary.as_mut() } else { cache.as_mut() }.ok_or("SAM image cache empty")?;
            predict(m, enc, request.operation)
        })();
        eprintln!(
            "SAM3 Metal request elapsed={:.3}s image={}x{} detail={} success={}",
            started.elapsed().as_secs_f64(),
            request.width,
            request.height,
            request.temporary,
            result.is_ok()
        );
        let response = serde_json::to_vec(&result).map_err(|e| e.to_string())?;
        write_frame(stream, &response).map_err(|e| e.to_string())?;
        if result.is_err() {
            return Err("SAM inference failed; session released".into());
        }
    }
}

/// Explain how to align optional authentication on a loopback connection.
fn token_hint() -> String {
    format!("set the same {TOKEN_ENV} on both ends, or unset it on both (loopback only)")
}

/// Enforce the documented character minimum on clients and workers before any network I/O.
fn validate_token(token: Option<&str>) -> Result<(), String> {
    if token.is_some_and(|value| value.chars().take(TOKEN_MIN).count() < TOKEN_MIN) {
        return Err(format!("{TOKEN_ENV} must have at least {TOKEN_MIN} characters"));
    }
    Ok(())
}

/// Why a worker may not listen like this, if it may not: off loopback it needs a real token.
fn listen_error(address: SocketAddr, token: Option<&str>) -> Option<String> {
    if let Err(error) = validate_token(token) {
        return Some(error);
    }
    match token {
        None if !address.ip().is_loopback() => Some(format!("SAM worker off loopback requires {TOKEN_ENV}")),
        _ => None,
    }
}

/// Listen only with valid authentication settings and isolate failures to the current session.
pub fn serve(address: SocketAddr, dir: &Path, device: candle_core::Device, token: Option<&str>) -> Result<(), String> {
    if let Some(error) = listen_error(address, token) {
        return Err(error);
    }
    if !device.is_metal() {
        return Err("SAM worker requires a Metal device".into());
    }
    if !crate::is_model_dir(dir) {
        return Err("SAM model files are missing".into());
    }
    let listener = TcpListener::bind(address).map_err(|e| e.to_string())?;
    eprintln!("SAM3 worker listening on {address}; device=Metal; idle unload=600s");
    for stream in listener.incoming() {
        let mut stream = stream.map_err(|e| e.to_string())?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| serve_client(&mut stream, dir, device.clone(), token)));
        match result {
            Ok(Err(error)) => eprintln!("SAM3 session closed: {error}"),
            Err(_) => eprintln!("SAM3 session panicked; model released"),
            Ok(Ok(())) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    const TOKEN: &str = "0123456789abcdef";

    fn worker(token: &'static str) -> (SocketAddr, thread::JoinHandle<Result<(), String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || check_hello(&mut listener.accept().unwrap().0, token));
        (address, handle)
    }

    #[test]
    fn the_right_token_is_let_in() {
        let (address, worker) = worker(TOKEN);
        assert!(Client::connect_with(&address.to_string(), Some(TOKEN)).is_ok());
        assert!(worker.join().unwrap().is_ok());
    }

    #[test]
    fn a_wrong_token_is_refused_by_both_ends() {
        let (address, worker) = worker(TOKEN);
        let error = Client::connect_with(&address.to_string(), Some("fedcba9876543210")).err().unwrap();
        assert!(error.contains("doesn't know the token"), "{error}");
        assert!(worker.join().unwrap().is_err());
    }

    #[test]
    fn the_token_never_crosses_the_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let eavesdropper = thread::spawn(move || {
            let mut stream = listener.accept().unwrap().0;
            let hello = read_frame(&mut stream, CONTROL_LIMIT).unwrap();
            let client_nonce: String = serde_json::from_slice(&hello).unwrap();
            let worker_nonce = "0".repeat(32);
            let proof = hmac(TOKEN, &["worker", &client_nonce, &worker_nonce]);
            send(&mut stream, &Proof { nonce: worker_nonce, proof }).unwrap();
            let response = read_frame(&mut stream, CONTROL_LIMIT).unwrap();
            [hello, response].concat()
        });
        assert!(Client::connect_with(&address.to_string(), Some(TOKEN)).is_ok());
        let hello = eavesdropper.join().unwrap();
        assert!(!String::from_utf8_lossy(&hello).contains(TOKEN));
    }

    #[test]
    fn a_host_name_resolves() {
        let (address, worker) = worker(TOKEN);
        assert!(Client::connect_with(&format!("localhost:{}", address.port()), Some(TOKEN)).is_ok());
        assert!(worker.join().unwrap().is_ok());
    }

    #[test]
    fn a_client_without_a_token_stays_on_loopback() {
        let error = Client::connect_with("192.0.2.1:8793", None).err().unwrap();
        assert!(error.contains(TOKEN_ENV), "{error}");
    }

    #[test]
    fn a_worker_without_a_token_stays_on_loopback() {
        assert!(listen_error("0.0.0.0:8793".parse().unwrap(), None).is_some());
        assert!(listen_error("127.0.0.1:8793".parse().unwrap(), None).is_none());
        assert!(listen_error("0.0.0.0:8793".parse().unwrap(), Some(TOKEN)).is_none());
    }

    #[test]
    fn a_short_token_is_refused_everywhere() {
        for token in ["short", "😀😀😀😀"] {
            assert!(listen_error("127.0.0.1:8793".parse().unwrap(), Some(token)).is_some());
            let error = Client::connect_with("invalid endpoint", Some(token)).err().unwrap();
            assert!(error.contains("at least 16 characters"), "{error}");
        }
    }

    #[test]
    fn proofs_compare_whole() {
        assert!(same_proof("abc", "abc"));
        assert!(!same_proof("abc", "abd"));
        assert!(!same_proof("abc", "abcd"));
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        use sha2::{Digest, Sha256};
        // the length prefix is this module's own framing, so feed the RFC's message through it
        let message = "what do ya want for nothing?";
        let mut framed = (message.len() as u64).to_be_bytes().to_vec();
        framed.extend_from_slice(message.as_bytes());
        let mut block = [0u8; 64];
        block[..4].copy_from_slice(b"Jefe");
        let inner = Sha256::new().chain_update(block.map(|b| b ^ 0x36)).chain_update(&framed).finalize();
        let outer = Sha256::new().chain_update(block.map(|b| b ^ 0x5c)).chain_update(inner).finalize();
        let expected: String = outer.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hmac("Jefe", &[message]), expected);
    }

    #[test]
    fn long_key_hmac_matches_known_digest() {
        // Independently calculated HMAC-SHA256 of u64-length-framed "worker" and "nonce".
        assert_eq!(hmac(&"k".repeat(100), &["worker", "nonce"]), "ca680cdb3c168664794fc03fad171b59b7922939bf1357ea334ac215e3c422cf");
    }

    #[test]
    fn handshake_failure_tries_the_next_address() {
        let (wrong, rejected) = worker("fedcba9876543210");
        let (right, accepted) = worker(TOKEN);
        assert!(Client::connect_addresses("test", &[wrong, right], Some(TOKEN)).is_ok());
        assert!(rejected.join().unwrap().is_err());
        assert!(accepted.join().unwrap().is_ok());
    }

    #[test]
    fn an_expired_connection_budget_stops_before_the_next_address() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let error = Client::connect_until("test", &[address, address], Some(TOKEN), Instant::now()).err().unwrap();
        assert!(error.contains("connection deadline expired"), "{error}");
        listener.set_nonblocking(true).unwrap();
        assert_eq!(listener.accept().unwrap_err().kind(), io::ErrorKind::WouldBlock);
    }

    #[test]
    fn handshake_errors_explain_token_configuration() {
        let mut invalid = Vec::new();
        send(&mut invalid, &Proof { nonce: "0".repeat(32), proof: "0".repeat(64) }).unwrap();
        let mut stream = io::Cursor::new(invalid);
        let error = check_hello(&mut stream, TOKEN).unwrap_err();
        assert!(error.contains(TOKEN_ENV), "{error}");
        assert!(error.contains("both ends"), "{error}");
    }

    #[test]
    fn expired_handshake_budget_prevents_further_reads_and_writes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        client.write_all(b"ab").unwrap();
        let mut deadline = Deadline::new(&mut server, HELLO_TIMEOUT);
        let mut byte = [0];
        deadline.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [b'a']);
        // Advance the same connection's deadline deterministically, with unread data available.
        deadline.expires = Instant::now();
        assert_eq!(deadline.read(&mut byte).unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(deadline.write(b"x").unwrap_err().kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn handshake_timeouts_do_not_recommend_changing_the_token() {
        for kind in [io::ErrorKind::TimedOut, io::ErrorKind::WouldBlock] {
            let error = handshake_io_error(io::Error::from(kind));
            assert!(error.contains("time budget exhausted"), "{error}");
            assert!(!error.contains(TOKEN_ENV), "{error}");
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let _client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let mut deadline = Deadline::new(&mut server, Duration::ZERO);
        let error = receive::<String>(&mut deadline).unwrap_err();
        assert!(error.contains("time budget exhausted"), "{error}");
        assert!(!error.contains(TOKEN_ENV), "{error}");
    }

    /// Only an early close suggests a possible one-sided token configuration.
    #[test]
    fn early_handshake_closes_explain_token_alignment() {
        for kind in [io::ErrorKind::UnexpectedEof, io::ErrorKind::ConnectionReset, io::ErrorKind::ConnectionAborted, io::ErrorKind::BrokenPipe] {
            let error = handshake_io_error(io::Error::from(kind));
            assert!(error.contains("closed early"), "{error}");
            assert!(error.contains(TOKEN_ENV), "{error}");
        }
        let error = receive::<String>(&mut io::Cursor::new(Vec::<u8>::new())).unwrap_err();
        assert!(error.contains(TOKEN_ENV), "{error}");
        assert!(!handshake_io_error(io::Error::from(io::ErrorKind::PermissionDenied)).contains(TOKEN_ENV));
    }

    /// An idle socket must time out in the kernel; a watchdog prevents a missing timeout hanging CI.
    #[test]
    fn an_idle_handshake_socket_has_a_positive_read_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let _client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let watchdog_socket = server.try_clone().unwrap();
        let (done, finished) = std::sync::mpsc::channel();
        let watchdog = thread::spawn(move || {
            if finished.recv_timeout(Duration::from_secs(10)).is_err() {
                let _ = watchdog_socket.shutdown(std::net::Shutdown::Both);
                true
            } else {
                false
            }
        });
        let result = receive::<String>(&mut Deadline::new(&mut server, Duration::from_millis(100)));
        let _ = done.send(());
        let watchdog_fired = watchdog.join().unwrap();
        assert!(!watchdog_fired, "idle read exceeded the watchdog budget");
        assert!(server.read_timeout().unwrap().is_some(), "deadline must configure the socket timeout");
        let error = result.unwrap_err();
        assert!(error.contains("time budget exhausted"), "{error}");
        assert!(!error.contains(TOKEN_ENV), "{error}");
    }

    #[test]
    fn a_stalled_resolver_has_a_bounded_wait() {
        let (_sender, receiver) = std::sync::mpsc::sync_channel(1);
        assert_eq!(wait_for_lookup(&receiver, Duration::ZERO), Err(std::sync::mpsc::RecvTimeoutError::Timeout));
    }

    #[test]
    fn numeric_endpoints_need_no_dns() {
        let address: SocketAddr = "127.0.0.1:8793".parse().unwrap();
        assert_eq!(resolve(&address.to_string()).unwrap(), vec![address]);
    }

    #[test]
    fn nonces_differ() {
        assert_ne!(nonce(), nonce());
        assert_eq!(nonce().len(), 32);
    }
}
