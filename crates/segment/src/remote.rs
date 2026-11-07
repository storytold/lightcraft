//! Private SAM 3 transport. Bind the worker to loopback and forward it over SSH, or set a shared
//! [`TOKEN_ENV`] on both ends to reach it over a network you trust.
//!
//! With a token both ends prove they know it before any pixels move: the client sends a nonce, the
//! worker answers with its own nonce and an HMAC-SHA256 of both, the client checks that and answers
//! with its own HMAC. The token itself never crosses the socket.
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

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

#[derive(Serialize, Deserialize)]
pub enum Operation {
    Prepare,
    Clicks(Vec<[f32; 3]>),
    Text(String),
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
            Operation::Prepare => {}
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

pub struct Client {
    stream: TcpStream,
    pub key: Option<u64>,
}

impl Client {
    pub fn connect(endpoint: &str) -> Result<Self, String> {
        Self::connect_with(endpoint, token().as_deref())
    }

    fn connect_with(endpoint: &str, token: Option<&str>) -> Result<Self, String> {
        let addresses: Vec<SocketAddr> = endpoint.to_socket_addrs().map_err(|e| format!("SAM endpoint must be a host:port: {e}"))?.collect();
        if token.is_none() && addresses.iter().any(|a| !a.ip().is_loopback()) {
            return Err(format!("SAM endpoint off this machine needs {TOKEN_ENV}; or use an SSH tunnel for remote access"));
        }
        let mut last = format!("SAM endpoint {endpoint} has no address");
        for address in addresses {
            match TcpStream::connect_timeout(&address, Duration::from_secs(5)) {
                Ok(mut stream) => {
                    stream.set_write_timeout(Some(Duration::from_secs(30))).map_err(|e| e.to_string())?;
                    if let Some(token) = token {
                        stream.set_read_timeout(Some(HELLO_TIMEOUT)).map_err(|e| e.to_string())?;
                        say_hello(&mut stream, token)?;
                    }
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

fn send<T: Serialize>(writer: &mut impl Write, value: &T) -> Result<(), String> {
    write_frame(writer, &serde_json::to_vec(value).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

fn receive<T: serde::de::DeserializeOwned>(reader: &mut impl Read) -> Result<T, String> {
    let frame = read_frame(reader, CONTROL_LIMIT).map_err(|e| format!("SAM handshake: {e}"))?;
    serde_json::from_slice(&frame).map_err(|_| "unexpected SAM handshake".to_string())
}

/// The client's half of the handshake: the worker proves the token before any pixels go.
fn say_hello(stream: &mut (impl Read + Write), token: &str) -> Result<(), String> {
    let mine = nonce();
    send(stream, &mine)?;
    let reply: Proof = receive(stream)?;
    if reply.nonce.len() < 32 || !same_proof(&reply.proof, &hmac(token, &["worker", &mine, &reply.nonce])) {
        return Err("SAM worker doesn't know the token: is something else listening on that port?".into());
    }
    send(stream, &hmac(token, &["client", &mine, &reply.nonce]))
}

/// The worker's half of the handshake: the client proves the token before a request is read.
fn check_hello(stream: &mut (impl Read + Write), token: &str) -> Result<(), String> {
    let theirs: String = receive(stream)?;
    if theirs.len() < 32 {
        return Err("unexpected SAM handshake".into());
    }
    let mine = nonce();
    send(stream, &Proof { nonce: mine.clone(), proof: hmac(token, &["worker", &theirs, &mine]) })?;
    let proof: String = receive(stream)?;
    if same_proof(&proof, &hmac(token, &["client", &theirs, &mine])) { Ok(()) } else { Err("wrong SAM worker token".into()) }
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
        Operation::Clicks(points) => {
            let clicks: Vec<_> = points.into_iter().map(|p| Click { x: p[0], y: p[1], positive: p[2] == 1.0 }).collect();
            Ok(Some(model.segment_clicks(enc, &clicks).map_err(|e| e.to_string())?.logits))
        }
    }
}

fn serve_client(stream: &mut TcpStream, dir: &Path, device: candle_core::Device, token: Option<&str>) -> Result<(), String> {
    stream.set_write_timeout(Some(Duration::from_secs(30))).map_err(|e| e.to_string())?;
    if let Some(token) = token {
        stream.set_read_timeout(Some(HELLO_TIMEOUT)).map_err(|e| e.to_string())?;
        check_hello(stream, token)?;
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

/// Why a worker may not listen like this, if it may not: off loopback it needs a real token.
fn listen_error(address: SocketAddr, token: Option<&str>) -> Option<String> {
    match token {
        Some(t) if t.len() < TOKEN_MIN => Some(format!("{TOKEN_ENV} must have at least {TOKEN_MIN} characters")),
        None if !address.ip().is_loopback() => Some(format!("SAM worker off loopback requires {TOKEN_ENV}")),
        _ => None,
    }
}

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
            send(&mut stream, &Proof { nonce: "0".repeat(32), proof: "0".repeat(64) }).unwrap();
            hello
        });
        assert!(Client::connect_with(&address.to_string(), Some(TOKEN)).is_err());
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
        assert!(listen_error("127.0.0.1:8793".parse().unwrap(), Some("short")).is_some());
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
    fn nonces_differ() {
        assert_ne!(nonce(), nonce());
        assert_eq!(nonce().len(), 32);
    }
}
