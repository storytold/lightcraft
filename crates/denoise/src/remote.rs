//! Remote denoise: run a model's tiles on another computer's graphics card over TCP. Meant for a LAN or a VPN such as
//! Tailscale or WireGuard: the connection itself is not encrypted.
//!
//! [`Remote`] is a [`TileRunner`] that sends each tile to a server; [`serve`] runs one (`lightcraft-cli
//! denoise-serve`). Both ends must run the same model, checked by its identity ([`ModelId`]) when they greet.
//!
//! Protocol (version 1). Each message is one JSON line (`\n`-terminated, ≤ 4 KiB), optionally followed by a payload of
//! `bytes` bytes: the tile's samples as little-endian half floats, byte-shuffled (every sample's low byte, then every
//! high byte) and deflated, which roughly halves what crosses a slow link.
//!
//! ```text
//! → {"v":1,"token":"…","op":"hello"}
//! ← {"ok":true,"v":1,"device":"GPU: …","model":{"id":"…","version":"…","sha256":"…"}}
//! → {"v":1,"token":"…","op":"run","model":"<sha256>","tile":T,"bytes":N}   + N bytes (4 planes of T × T)
//! ← {"ok":true,"tile":T,"bytes":N}                                        + N bytes (3 planes of 2T × 2T)
//! ← {"ok":false,"error":"…"}                                              (any failure)
//! ```
//!
//! A connection serves any number of requests. The server refuses a wrong token, another model than its own, tiles of
//! another size, oversized headers and payloads (also ones that inflate past their tile), and caps concurrent
//! connections; the client gives up on a server that keeps failing, so the computer's own runner does the rest.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use crate::product::{from_f16, to_f16};
use crate::run::{Error, TileRunner};

pub const PROTOCOL: u64 = 1;
pub const DEFAULT_PORT: u16 = 7990;
const MAX_LINE: usize = 4096;
const AUTH_HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const MIN_SOCKET_READ_TIMEOUT: Duration = Duration::from_millis(1);
/// Largest tile side in cells (a 1024-cell tile is a 2048 × 2048 pixel answer).
pub const MAX_TILE: usize = 1024;
/// Shortest token a server accepts.
pub const MIN_TOKEN: usize = 16;

/// Which model a server runs: both ends must agree before tiles cross.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ModelId {
    pub id: String,
    pub version: String,
    /// SHA-256 of the model file (hex).
    pub sha256: String,
}

fn bad(m: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, m.into())
}

fn read_line(r: &mut impl BufRead) -> std::io::Result<Option<String>> {
    let mut buf = Vec::new();
    let n = r.by_ref().take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() != Some(&b'\n') {
        return Err(bad("message header too long or truncated"));
    }
    String::from_utf8(buf).map(Some).map_err(|_| bad("header is not UTF-8"))
}

fn read_timeout_until(deadline: Option<Instant>, idle_timeout: Duration, now: Instant) -> std::io::Result<Duration> {
    let Some(deadline) = deadline else {
        return Ok(idle_timeout);
    };
    let remaining = deadline.saturating_duration_since(now);
    if remaining.is_zero() {
        return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "authentication header deadline elapsed"));
    }
    Ok(remaining.max(MIN_SOCKET_READ_TIMEOUT))
}

struct DeadlineReader {
    stream: TcpStream,
    deadline: Option<Instant>,
    idle_timeout: Duration,
}

impl DeadlineReader {
    fn new(stream: TcpStream, idle_timeout: Duration) -> std::io::Result<Self> {
        let budget = idle_timeout.min(AUTH_HEADER_TIMEOUT);
        let deadline = Instant::now()
            .checked_add(budget)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "authentication deadline is out of range"))?;
        Ok(Self { stream, deadline: Some(deadline), idle_timeout })
    }

    fn restore_idle_timeout(&mut self) -> std::io::Result<()> {
        self.stream.set_read_timeout(Some(self.idle_timeout))?;
        self.deadline = None;
        Ok(())
    }

    fn ensure_before_deadline(&self) -> std::io::Result<()> {
        if self.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "authentication header deadline elapsed"));
        }
        Ok(())
    }
}

impl Read for DeadlineReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let Some(deadline) = self.deadline else {
            return self.stream.read(buf);
        };
        let timeout = read_timeout_until(Some(deadline), self.idle_timeout, Instant::now())?;
        self.stream.set_read_timeout(Some(timeout))?;
        let read = self.stream.read(buf)?;
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "authentication header deadline elapsed"));
        }
        Ok(read)
    }
}

/// Samples → half floats → byte-shuffled → deflated.
pub fn pack(data: &[f32]) -> Vec<u8> {
    let n = data.len();
    let mut b = vec![0u8; n * 2];
    let (lo, hi) = b.split_at_mut(n);
    for ((v, l), h) in data.iter().zip(lo.iter_mut()).zip(hi.iter_mut()) {
        [*l, *h] = to_f16(*v).to_le_bytes();
    }
    miniz_oxide::deflate::compress_to_vec(&b, 1)
}

/// The inverse of [`pack`] for exactly `n` samples; refuses anything that inflates to more.
pub fn unpack(bytes: &[u8], n: usize) -> std::io::Result<Vec<f32>> {
    let b = miniz_oxide::inflate::decompress_to_vec_with_limit(bytes, n * 2).map_err(|_| bad("payload doesn't inflate to the tile size"))?;
    if b.len() != n * 2 {
        return Err(bad("payload size doesn't match the tile"));
    }
    let (lo, hi) = b.split_at(n);
    Ok(lo.iter().zip(hi).map(|(l, h)| from_f16(u16::from_le_bytes([*l, *h]))).collect())
}

fn write_payload(w: &mut impl Write, mut header: Value, data: &[f32]) -> std::io::Result<()> {
    let packed = pack(data);
    header["bytes"] = json!(packed.len());
    writeln!(w, "{header}")?;
    w.write_all(&packed)
}

/// Read a `bytes`-byte payload holding `n` samples (`bytes` is capped by the tile, so a hostile header can't make us
/// allocate more than an uncompressed tile plus slack).
fn read_payload(r: &mut impl Read, bytes: usize, n: usize) -> std::io::Result<Vec<f32>> {
    if bytes > n * 2 + 4096 {
        return Err(bad("payload larger than its tile"));
    }
    let mut b = vec![0u8; bytes];
    r.read_exact(&mut b)?;
    unpack(&b, n)
}

fn field(v: &Value, k: &str) -> Option<usize> {
    v.get(k).and_then(Value::as_u64).and_then(|x| usize::try_from(x).ok())
}

/// Constant-time comparison (the token must not leak through timing).
fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut d = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        d |= (a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0)) as usize;
    }
    d == 0
}

/// What a server runs a tile with: four planes of `tile × tile` cells in, three planes of `2·tile × 2·tile` out.
pub type RunTile = Arc<dyn Fn(&[f32]) -> Result<Vec<f32>, Error> + Send + Sync>;

/// A server's settings.
#[derive(Clone, Debug)]
pub struct ServerOptions {
    /// Shared secret every request must carry (at least [`MIN_TOKEN`] characters).
    pub token: String,
    pub max_connections: usize,
    /// Idle and transfer limit per connection.
    pub timeout: Duration,
}

impl Default for ServerOptions {
    fn default() -> Self {
        ServerOptions { token: String::new(), max_connections: 8, timeout: Duration::from_secs(120) }
    }
}

/// Serve `model` (tiles of `tile` cells, run by `run` on `device`) until the listener fails. `log` gets one line per event.
pub fn serve(
    listener: TcpListener,
    model: ModelId,
    tile: usize,
    device: String,
    run: RunTile,
    opts: ServerOptions,
    log: Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<(), String> {
    if opts.token.trim().chars().count() < MIN_TOKEN {
        return Err(format!("the token must be at least {MIN_TOKEN} characters"));
    }
    if tile == 0 || tile > MAX_TILE {
        return Err(format!("tile size {tile} is outside 1..={MAX_TILE}"));
    }
    let active = Arc::new(AtomicUsize::new(0));
    let shared = Arc::new((model, device));
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_else(|_| "?".into());
        if active.load(Ordering::SeqCst) >= opts.max_connections.max(1) {
            let mut s = stream;
            let _ = writeln!(s, "{}", json!({"ok": false, "error": "server busy"}));
            log(&format!("{peer}: refused (busy)"));
            continue;
        }
        active.fetch_add(1, Ordering::SeqCst);
        let (sh, r, o, l, n) = (shared.clone(), run.clone(), opts.clone(), log.clone(), active.clone());
        let spawned = std::thread::Builder::new().name(format!("denoise {peer}")).spawn(move || {
            if let Err(e) = handle(stream, &sh.0, tile, &sh.1, &*r, &o, &*l) {
                l(&format!("{peer}: {e}"));
            }
            n.fetch_sub(1, Ordering::SeqCst);
        });
        if spawned.is_err() {
            active.fetch_sub(1, Ordering::SeqCst);
            log("could not start a connection thread");
        }
    }
    Ok(())
}

fn handle(
    stream: TcpStream,
    model: &ModelId,
    tile: usize,
    device: &str,
    run: &dyn Fn(&[f32]) -> Result<Vec<f32>, Error>,
    opts: &ServerOptions,
    log: &dyn Fn(&str),
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(opts.timeout))?;
    stream.set_write_timeout(Some(opts.timeout))?;
    stream.set_nodelay(true)?;
    let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let mut r = BufReader::new(DeadlineReader::new(stream.try_clone()?, opts.timeout)?);
    let mut w = stream;
    let mut tiles = 0usize;
    let mut authenticated = false;
    let fail = |w: &mut TcpStream, msg: &str| writeln!(w, "{}", json!({"ok": false, "error": msg}));
    while let Some(line) = read_line(&mut r)? {
        if !authenticated {
            r.get_ref().ensure_before_deadline()?;
        }
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            fail(&mut w, "malformed request")?;
            break;
        };
        if req.get("v").and_then(Value::as_u64) != Some(PROTOCOL) {
            fail(&mut w, &format!("protocol version {PROTOCOL} only"))?;
            break;
        }
        if !same(req.get("token").and_then(Value::as_str).unwrap_or(""), &opts.token) {
            fail(&mut w, "bad token")?;
            log(&format!("{peer}: bad token"));
            break;
        }
        if !authenticated {
            r.get_ref().ensure_before_deadline()?;
            r.get_mut().restore_idle_timeout()?;
            authenticated = true;
        }
        match req.get("op").and_then(Value::as_str) {
            Some("hello") => writeln!(w, "{}", json!({"ok": true, "v": PROTOCOL, "device": device, "model": model, "tile": tile}))?,
            Some("run") => {
                let (Some(t), Some(bytes)) = (field(&req, "tile"), field(&req, "bytes")) else {
                    fail(&mut w, "missing tile size or payload size")?;
                    break;
                };
                if t != tile {
                    fail(&mut w, &format!("this server runs {tile}-cell tiles, not {t}"))?;
                    break;
                }
                // the payload is read before any refusal below, so the stream stays in step
                let input = match read_payload(&mut r, bytes, 4 * t * t) {
                    Ok(d) => d,
                    Err(e) => {
                        fail(&mut w, &e.to_string())?;
                        break;
                    }
                };
                if req.get("model").and_then(Value::as_str) != Some(model.sha256.as_str()) {
                    fail(&mut w, &format!("model mismatch: this server runs {} {}", model.id, model.version))?;
                    continue;
                }
                match run(&input) {
                    Ok(out) if out.len() == 3 * 4 * t * t && out.iter().all(|v| v.is_finite()) => {
                        write_payload(&mut w, json!({"ok": true, "tile": t}), &out)?;
                        tiles += 1;
                    }
                    Ok(_) => fail(&mut w, "the model's answer has the wrong size or numbers that are not finite")?,
                    Err(e) => fail(&mut w, &e.to_string())?,
                }
            }
            _ => {
                fail(&mut w, "unknown op")?;
                break;
            }
        }
        w.flush()?;
    }
    if tiles > 0 {
        log(&format!("{peer}: {tiles} tiles"));
    }
    Ok(())
}

/// What a server said in its greeting.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ServerInfo {
    pub address: String,
    pub device: String,
    pub model: ModelId,
    pub tile: usize,
}

/// Tiles the server may get wrong (an error, a broken connection) before it stops being used.
pub const MAX_FAILURES: usize = 3;

/// A server as a [`TileRunner`], for one model: keeps up to `slots` connections open and gives up on the server after
/// [`MAX_FAILURES`] failed tiles (the caller runs those tiles itself).
pub struct Remote {
    address: String,
    token: String,
    model_sha256: String,
    tile: usize,
    slots: usize,
    timeout: Duration,
    busy: AtomicUsize,
    failures: AtomicUsize,
    last_error: Mutex<Option<String>>,
    pool: Mutex<Vec<(BufReader<TcpStream>, TcpStream)>>,
}

impl Remote {
    /// `address` as `host[:port]` (default port [`DEFAULT_PORT`]; `tcp://` and a trailing `/` are ignored).
    pub fn new(address: &str, token: &str, model_sha256: &str, tile: usize) -> Remote {
        let a = address.trim().trim_start_matches("tcp://").trim_end_matches('/');
        let has_port = a.rsplit_once(':').is_some_and(|(h, p)| !h.is_empty() && !h.ends_with(':') && p.parse::<u16>().is_ok()) && !a.ends_with(']');
        let address = if has_port { a.to_string() } else { format!("{a}:{DEFAULT_PORT}") };
        Remote {
            address,
            token: token.into(),
            model_sha256: model_sha256.to_ascii_lowercase(),
            tile,
            slots: 4,
            timeout: Duration::from_secs(60),
            busy: AtomicUsize::new(0),
            failures: AtomicUsize::new(0),
            last_error: Mutex::new(None),
            pool: Mutex::new(Vec::new()),
        }
    }

    pub fn with_slots(mut self, n: usize) -> Remote {
        self.slots = n.clamp(1, 16);
        self
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    /// Tiles it can take at once.
    pub fn slots(&self) -> usize {
        self.slots
    }

    /// Still used (fewer than [`MAX_FAILURES`] failed tiles).
    pub fn healthy(&self) -> bool {
        self.failures.load(Ordering::Relaxed) < MAX_FAILURES
    }

    pub fn failures(&self) -> usize {
        self.failures.load(Ordering::Relaxed)
    }

    /// Why the last tile failed.
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn err(&self, reason: impl std::fmt::Display) -> Error {
        Error::Runtime(format!("remote {}: {reason}", self.address))
    }

    fn connect(&self) -> Result<(BufReader<TcpStream>, TcpStream), Error> {
        let addr = self.address.to_socket_addrs().map_err(|e| self.err(e))?.next().ok_or_else(|| self.err("the address doesn't resolve"))?;
        let s = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| self.err(e))?;
        s.set_read_timeout(Some(self.timeout)).map_err(|e| self.err(e))?;
        s.set_write_timeout(Some(self.timeout)).map_err(|e| self.err(e))?;
        s.set_nodelay(true).map_err(|e| self.err(e))?;
        let r = BufReader::new(s.try_clone().map_err(|e| self.err(e))?);
        Ok((r, s))
    }

    fn reply(&self, r: &mut BufReader<TcpStream>) -> Result<Value, Error> {
        let line = read_line(r).map_err(|e| self.err(e))?.ok_or_else(|| self.err("the connection closed"))?;
        let v: Value = serde_json::from_str(&line).map_err(|_| self.err("malformed reply"))?;
        if v.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(self.err(v.get("error").and_then(Value::as_str).unwrap_or("request failed")));
        }
        Ok(v)
    }

    /// Greet the server: its device and model. Fails when it runs another model or tile size than this one.
    pub fn hello(&self) -> Result<ServerInfo, Error> {
        let (mut r, mut s) = self.connect()?;
        writeln!(s, "{}", json!({"v": PROTOCOL, "token": self.token, "op": "hello"})).map_err(|e| self.err(e))?;
        let v = self.reply(&mut r)?;
        let m = v.get("model").cloned().unwrap_or_default();
        let text = |o: &Value, k: &str| o.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let info = ServerInfo {
            address: self.address.clone(),
            device: text(&v, "device"),
            model: ModelId { id: text(&m, "id"), version: text(&m, "version"), sha256: text(&m, "sha256") },
            tile: field(&v, "tile").unwrap_or(0),
        };
        if !info.model.sha256.eq_ignore_ascii_case(&self.model_sha256) {
            return Err(self.err(format!("it runs another model ({} {})", info.model.id, info.model.version)));
        }
        if info.tile != self.tile {
            return Err(self.err(format!("it runs {}-cell tiles, this model {}", info.tile, self.tile)));
        }
        self.pool.lock().unwrap_or_else(PoisonError::into_inner).push((r, s));
        Ok(info)
    }

    /// Run a tile if a slot is free right now; `None` when all are busy or the server was given up (the caller runs
    /// the tile elsewhere). A failure counts towards giving up on the server.
    pub fn try_run(&self, input: &[f32]) -> Option<Result<Vec<f32>, Error>> {
        if !self.healthy() {
            return None;
        }
        if self.busy.fetch_add(1, Ordering::SeqCst) >= self.slots {
            self.busy.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        let r = self.run_now(input);
        self.busy.fetch_sub(1, Ordering::SeqCst);
        if let Err(e) = &r {
            self.failures.fetch_add(1, Ordering::Relaxed);
            *self.last_error.lock().unwrap_or_else(PoisonError::into_inner) = Some(e.to_string());
        }
        Some(r)
    }

    fn run_now(&self, input: &[f32]) -> Result<Vec<f32>, Error> {
        let t = self.tile;
        if input.len() != 4 * t * t {
            return Err(Error::Input(format!("a tile of {} samples for {t}-cell tiles", input.len())));
        }
        let pooled = self.pool.lock().unwrap_or_else(PoisonError::into_inner).pop();
        let (mut r, mut s) = match pooled {
            Some(c) => c,
            None => self.connect()?,
        };
        let req = json!({"v": PROTOCOL, "token": self.token, "op": "run", "model": self.model_sha256, "tile": t});
        write_payload(&mut s, req, input).map_err(|e| self.err(e))?;
        s.flush().map_err(|e| self.err(e))?;
        let v = self.reply(&mut r)?;
        if field(&v, "tile") != Some(t) {
            return Err(self.err("the answer is for another tile size"));
        }
        let bytes = field(&v, "bytes").ok_or_else(|| self.err("an answer without its size"))?;
        let out = read_payload(&mut r, bytes, 3 * 4 * t * t).map_err(|e| self.err(e))?;
        self.pool.lock().unwrap_or_else(PoisonError::into_inner).push((r, s));
        // the other computer's numbers go straight into the render: NaN or infinity (an f16 can carry
        // both) is a failed tile, which `Shared` runs here instead
        if !out.iter().all(|v| v.is_finite()) {
            return Err(self.err("the answer holds values that aren't finite"));
        }
        Ok(out)
    }
}

/// Tiles on the server when one of its slots is free, else (and for any tile it fails) on `local`: the server adds
/// speed to this computer's runner instead of replacing it.
pub struct Shared<'a> {
    pub remote: &'a Remote,
    pub local: &'a dyn TileRunner,
}

impl TileRunner for Shared<'_> {
    fn run(&self, input: &[f32]) -> Result<Vec<f32>, Error> {
        match self.remote.try_run(input) {
            Some(Ok(out)) => Ok(out),
            Some(Err(_)) | None => self.local.run(input),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef-test";
    const T: usize = 8;

    /// A stand-in model: each output pixel is its cell's first plane, times `k`.
    fn model(k: f32) -> RunTile {
        Arc::new(move |input: &[f32]| {
            let t = T;
            let mut out = vec![0f32; 3 * 4 * t * t];
            for c in 0..3 {
                for y in 0..2 * t {
                    for x in 0..2 * t {
                        out[(c * 2 * t + y) * 2 * t + x] = input[(y / 2) * t + x / 2] * k;
                    }
                }
            }
            Ok(out)
        })
    }

    fn id(sha: &str) -> ModelId {
        ModelId { id: "test".into(), version: "1".into(), sha256: sha.into() }
    }

    fn start(sha: &str, run: RunTile) -> String {
        start_with_options(sha, run, ServerOptions { token: TOKEN.into(), ..Default::default() })
    }

    fn start_with_options(sha: &str, run: RunTile, opts: ServerOptions) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let m = id(sha);
        std::thread::spawn(move || serve(l, m, T, "CPU (test)".into(), run, opts, Arc::new(|_| {})));
        addr
    }

    struct Local(RunTile, AtomicUsize);
    impl TileRunner for Local {
        fn run(&self, input: &[f32]) -> Result<Vec<f32>, Error> {
            self.1.fetch_add(1, Ordering::SeqCst);
            (self.0)(input)
        }
    }

    fn tile_input(seed: f32) -> Vec<f32> {
        (0..4 * T * T).map(|i| (i as f32 * 0.37 + seed).sin() * 0.5 + 0.5).collect()
    }

    #[test]
    fn remote_tiles_match_local_ones_within_half_float_rounding() {
        let addr = start("aa11", model(2.0));
        let remote = Remote::new(&addr, TOKEN, "AA11", T);
        let info = remote.hello().unwrap();
        assert_eq!((info.device.as_str(), info.tile), ("CPU (test)", T));
        let input = tile_input(0.3);
        let got = remote.try_run(&input).unwrap().unwrap();
        let want = model(2.0)(&input).unwrap();
        assert!(got.iter().zip(&want).all(|(a, b)| (a - b).abs() < 2e-3));
    }

    #[test]
    fn shared_runner_falls_back_and_gives_up_on_a_failing_server() {
        let local = Local(model(2.0), AtomicUsize::new(0));
        // nobody listening: every tile runs locally, and after a few the server is given up
        let dead = Remote::new("127.0.0.1:1", TOKEN, "aa11", T);
        let shared = Shared { remote: &dead, local: &local };
        for i in 0..6 {
            assert!(shared.run(&tile_input(i as f32)).is_ok());
        }
        assert_eq!(local.1.load(Ordering::SeqCst), 6);
        assert!(!dead.healthy() && dead.last_error().is_some());
        // a server with another model is refused at the greeting and per tile
        let addr = start("bb22", model(2.0));
        let other = Remote::new(&addr, TOKEN, "aa11", T);
        assert!(other.hello().unwrap_err().to_string().contains("another model"));
        assert!(other.try_run(&tile_input(0.0)).unwrap().unwrap_err().to_string().contains("model mismatch"));
        // a wrong token
        let addr = start("aa11", model(2.0));
        assert!(Remote::new(&addr, "wrong-token-0000000", "aa11", T).hello().unwrap_err().to_string().contains("bad token"));
        // all slots busy: the tile runs locally without waiting
        let busy = Remote::new(&addr, TOKEN, "aa11", T).with_slots(1);
        busy.busy.store(1, Ordering::SeqCst);
        assert!(busy.try_run(&tile_input(0.0)).is_none());
    }

    #[test]
    fn hostile_requests_are_refused_and_the_server_keeps_working() {
        let addr = start("aa11", model(1.0));
        let talk = |bytes: &[u8]| -> String {
            let mut s = TcpStream::connect(&addr).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            s.write_all(bytes).unwrap();
            let mut line = String::new();
            let _ = BufReader::new(s).read_line(&mut line);
            line
        };
        assert!(talk(b"not json\n").contains("malformed"));
        let v2 = format!("{}\n", json!({"v": 2, "token": TOKEN, "op": "hello"}));
        assert!(talk(v2.as_bytes()).contains("protocol"));
        let big = format!("{}\n", json!({"v": 1, "token": TOKEN, "op": "run", "model": "aa11", "tile": 4096, "bytes": 10}));
        assert!(talk(big.as_bytes()).contains("8-cell"));
        // a payload that inflates past its tile
        let bomb = miniz_oxide::deflate::compress_to_vec(&vec![0u8; 1 << 20], 9);
        let hdr = format!("{}\n", json!({"v": 1, "token": TOKEN, "op": "run", "model": "aa11", "tile": T, "bytes": bomb.len()}));
        assert!(talk(&[hdr.as_bytes(), &bomb].concat()).contains("false"));
        assert!(Remote::new(&addr, TOKEN, "aa11", T).hello().is_ok());
        assert!(same("abc", "abc") && !same("abc", "abd") && !same("abc", "abcd"));
        // a short token is refused when the server starts
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let opts = ServerOptions { token: "short".into(), ..Default::default() };
        assert!(serve(l, id("aa11"), T, "x".into(), model(1.0), opts, Arc::new(|_| {})).is_err());
    }

    #[test]
    fn denoise_auth_header_progress_does_not_reset_deadline() {
        let timeout = Duration::from_millis(500);
        let addr = start_with_options("aa11", model(1.0), ServerOptions { token: TOKEN.into(), max_connections: 1, timeout });
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.set_nodelay(true).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let header = format!("{}\n", json!({"v": PROTOCOL, "token": TOKEN, "op": "hello"}));
        let chunks = header.as_bytes().chunks(16).collect::<Vec<_>>();
        for (i, chunk) in chunks.iter().enumerate() {
            if stream.write_all(chunk).is_err() {
                break;
            }
            if i + 1 < chunks.len() {
                std::thread::sleep(Duration::from_millis(180));
            }
        }
        let mut response = String::new();
        let read = BufReader::new(stream).read_line(&mut response);
        assert!(read.is_err() || !response.contains("\"ok\":true"), "trickled header was accepted: {response}");
    }

    #[test]
    fn denoise_auth_header_expiry_releases_slot() {
        let timeout = Duration::from_millis(450);
        let addr = start_with_options("aa11", model(1.0), ServerOptions { token: TOKEN.into(), max_connections: 1, timeout });
        let mut slow = TcpStream::connect(&addr).unwrap();
        slow.set_nodelay(true).unwrap();
        slow.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut slow_reader = slow.try_clone().unwrap();
        let keep_writing = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let writer_running = keep_writing.clone();
        let writer = std::thread::spawn(move || {
            while writer_running.load(Ordering::SeqCst) {
                if slow.write_all(b" ").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        });

        let mut byte = [0u8; 1];
        let closed = match slow_reader.read(&mut byte) {
            Ok(0) => true,
            Ok(_) => false,
            Err(e) => !matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut),
        };
        keep_writing.store(false, Ordering::SeqCst);
        let _ = writer.join();
        assert!(closed, "slow unauthenticated connection did not close at its total header deadline");

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut recovered = false;
        while Instant::now() < deadline {
            let mut valid = TcpStream::connect(&addr).unwrap();
            valid.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
            let hello = format!("{}\n", json!({"v": PROTOCOL, "token": TOKEN, "op": "hello"}));
            valid.write_all(hello.as_bytes()).unwrap();
            let mut response = String::new();
            BufReader::new(valid).read_line(&mut response).unwrap();
            if response.contains("\"ok\":true") {
                recovered = true;
                break;
            }
            assert!(response.contains("server busy"), "unexpected response while waiting for slot recovery: {response}");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(recovered, "slot was not released before the recovery deadline");
    }

    #[test]
    fn denoise_auth_header_before_deadline_succeeds() {
        let timeout = Duration::from_millis(900);
        let addr = start_with_options("aa11", model(1.0), ServerOptions { token: TOKEN.into(), max_connections: 1, timeout });
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.set_nodelay(true).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let header = format!("{}\n", json!({"v": PROTOCOL, "token": TOKEN, "op": "hello"}));
        let chunks = header.as_bytes().chunks(20).collect::<Vec<_>>();
        for (i, chunk) in chunks.iter().enumerate() {
            stream.write_all(chunk).unwrap();
            if i + 1 < chunks.len() {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        assert!(response.contains("\"ok\":true"), "valid fragmented header was refused: {response}");
    }

    #[test]
    fn denoise_auth_first_run_preserves_buffered_payload() {
        let addr = start("aa11", model(1.0));
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let input = tile_input(0.2);
        let payload = pack(&input);
        let header = format!("{}\n", json!({"v": PROTOCOL, "token": TOKEN, "op": "run", "model": "aa11", "tile": T, "bytes": payload.len()}));
        let mut message = header.into_bytes();
        message.extend_from_slice(&payload);
        stream.write_all(&message).unwrap();

        let mut reader = BufReader::new(stream);
        let response = read_line(&mut reader).unwrap().unwrap();
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response.get("ok").and_then(Value::as_bool), Some(true));
        let bytes = field(&response, "bytes").unwrap();
        let output = read_payload(&mut reader, bytes, 3 * 4 * T * T).unwrap();
        assert_eq!(output.len(), 3 * 4 * T * T);

        let second_input = tile_input(0.7);
        let second_payload = pack(&second_input);
        let second_header =
            format!("{}\n", json!({"v": PROTOCOL, "token": TOKEN, "op": "run", "model": "aa11", "tile": T, "bytes": second_payload.len()}));
        let mut second_message = second_header.into_bytes();
        second_message.extend_from_slice(&second_payload);
        reader.get_mut().write_all(&second_message).unwrap();
        let response = read_line(&mut reader).unwrap().unwrap();
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response.get("ok").and_then(Value::as_bool), Some(true));
        let bytes = field(&response, "bytes").unwrap();
        let output = read_payload(&mut reader, bytes, 3 * 4 * T * T).unwrap();
        assert_eq!(output.len(), 3 * 4 * T * T);

        let bad_token = format!("{}\n", json!({"v": PROTOCOL, "token": "invalid", "op": "hello"}));
        reader.get_mut().write_all(bad_token.as_bytes()).unwrap();
        let response = read_line(&mut reader).unwrap().unwrap();
        assert!(response.contains("bad token"));
    }

    #[test]
    fn denoise_auth_restores_normal_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        let timeout = Duration::from_secs(37);
        let mut reader = DeadlineReader::new(server, timeout).unwrap();
        let deadline = reader.deadline.unwrap();
        let budget = deadline.saturating_duration_since(Instant::now());
        assert!(!budget.is_zero() && budget <= AUTH_HEADER_TIMEOUT);
        let after_deadline = deadline + Duration::from_secs(1);

        reader.restore_idle_timeout().unwrap();

        assert_eq!(reader.deadline, None);
        assert_eq!(read_timeout_until(reader.deadline, reader.idle_timeout, after_deadline).unwrap(), timeout);
        drop(client);
    }

    #[test]
    fn denoise_auth_expired_budget_never_sets_zero_timeout() {
        let now = Instant::now();
        let err = read_timeout_until(Some(now), Duration::from_secs(10), now).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        let just_before = read_timeout_until(Some(now), Duration::from_secs(10), now - Duration::from_nanos(1)).unwrap();
        assert!(!just_before.is_zero());
    }

    #[test]
    fn packing_round_trips_and_addresses_are_normalised() {
        let v: Vec<f32> = (0..4096).map(|i| 0.5 + ((i * 7919 % 101) as f32 - 50.0) * 0.002).collect();
        let back = unpack(&pack(&v), v.len()).unwrap();
        assert!(v.iter().zip(&back).all(|(a, b)| (a - b).abs() < 1e-3));
        assert!(unpack(&pack(&v), v.len() + 1).is_err());
        assert_eq!(Remote::new("tcp://100.1.2.3/", TOKEN, "a", T).address(), "100.1.2.3:7990");
        assert_eq!(Remote::new("box:8000", TOKEN, "a", T).address(), "box:8000");
        assert_eq!(Remote::new("[::1]", TOKEN, "a", T).address(), "[::1]:7990");
    }
}
