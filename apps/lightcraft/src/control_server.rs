//! Loopback JSON-lines control server: one request per line, one reply per line.
//! This is the transport the MCP server (`lightcraft-cli mcp --connect`) wraps.
//!
//! The server only ever reads requests: a line that is not a JSON object with a string `method`
//! (an HTTP request line from a browser's cross-origin `fetch`, a stray `nc`, binary junk) gets
//! one error reply and the connection is closed, so nothing after it is executed. Lines are
//! bounded ([`MAX_LINE`]) and so is the number of open connections ([`MAX_CONNECTIONS`]); junk
//! never reaches the UI thread. See `docs/control-protocol.md`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use lightcraft_ui_egui::ControlRequest;
use serde_json::{Value, json};

/// Longest accepted request line (bytes, without the newline). Requests are small JSON objects;
/// anything longer is refused and the connection closed.
pub const MAX_LINE: usize = 4 * 1024 * 1024;
/// Connections served at once; further ones get an error line and are closed.
pub const MAX_CONNECTIONS: usize = 16;

/// Bind the control port on loopback. Bind first, then write the token file ([`prepare_token`]):
/// a port that is already taken by another run must not have that run's token file replaced.
pub fn bind(port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", port))
}

/// Serve the control channel on `listener` (from [`bind`]). `token`: when `Some`, the first line of
/// every connection must be `{"method":"auth","params":{"token":"…"}}` or the connection is closed.
/// See [`token_file`] / [`random_token`] for how the app and `lightcraft-cli` share it.
pub fn start(listener: TcpListener, ctx: egui::Context, token: Option<String>) -> Receiver<ControlRequest> {
    let (tx, rx) = channel::<ControlRequest>();
    let token: Option<Arc<str>> = token.map(Arc::from);
    let auth = if token.is_some() { "token required" } else { "NO AUTHENTICATION" };
    match listener.local_addr() {
        Ok(addr) => log::info!("control server listening on {addr} ({auth})"),
        Err(_) => log::info!("control server listening on 127.0.0.1 ({auth})"),
    }
    std::thread::spawn(move || accept_loop(listener, tx, ctx, token));
    rx
}

/// A token given by `--control-token` / `LIGHTCRAFT_CONTROL_TOKEN`, trimmed; empty after trimming is
/// absent. `lightcraft-cli` trims what it reads the same way, so the two sides compare equal.
pub fn clean_token(token: Option<String>) -> Option<String> {
    token.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

/// Bind `port`, then decide the token and write it to `token_path`, then serve: the order matters, so
/// that a port already held by another run leaves that run's token file alone (the error is returned
/// before anything is touched). `main.rs` passes [`token_file`]`(port)`; tests pass a temp path.
/// `ephemeral` is `--memory` (see [`prepare_token`]).
pub fn start_control(
    port: u16,
    explicit: Option<String>,
    no_auth: bool,
    ephemeral: bool,
    token_path: Option<std::path::PathBuf>,
    ctx: egui::Context,
) -> std::io::Result<Receiver<ControlRequest>> {
    let listener = bind(port)?;
    let token = prepare_token(token_path.as_deref(), explicit, no_auth, ephemeral);
    Ok(start(listener, ctx, token))
}

/// The token the control channel will require (`None`: authentication is off, and the token file of
/// an earlier run at `token_path` is removed). Without `explicit` a random one is generated. It is
/// also written to `token_path` for `lightcraft-cli`; call this only after [`bind`] succeeded.
///
/// `ephemeral` (a `--memory` session, which saves nothing): no file is written or removed. A
/// generated token is logged once instead, because that is the only way a client can learn it.
pub fn prepare_token(token_path: Option<&std::path::Path>, explicit: Option<String>, no_auth: bool, ephemeral: bool) -> Option<String> {
    if no_auth {
        log::warn!("control channel started WITHOUT authentication (LIGHTCRAFT_CONTROL_NO_AUTH)");
        if !ephemeral && let Some(path) = token_path {
            remove_file_if_present(path);
        }
        return None;
    }
    let generated = explicit.is_none();
    let token = explicit.unwrap_or_else(random_token);
    if ephemeral && generated {
        log::warn!(
            "control token for this --memory session: {token} (--memory saves nothing, so no token file is written; \
             give it to lightcraft-cli in LIGHTCRAFT_CONTROL_TOKEN, or start with --control-token)"
        );
        return Some(token);
    }
    let written = token_path
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no home directory"))
        .and_then(|path| write_token_at(path, &token).map(|()| path));
    match written {
        Ok(path) => log::info!("control token written to {} (lightcraft-cli reads it from there)", path.display()),
        Err(e) if generated => log::warn!(
            "could not write the control token file: {e}. This app generated its own token, so no client can learn it: \
             restart the app with --control-token (or LIGHTCRAFT_CONTROL_TOKEN) set to the same value for the app and for lightcraft-cli"
        ),
        Err(e) => log::warn!(
            "could not write the control token file: {e}. Give lightcraft-cli the same value, in LIGHTCRAFT_CONTROL_TOKEN, as the one this app was started with"
        ),
    }
    Some(token)
}

/// Accept connections until the listener fails, serving each on its own thread.
fn accept_loop(listener: TcpListener, tx: Sender<ControlRequest>, ctx: egui::Context, token: Option<Arc<str>>) {
    let open = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming().flatten() {
        let _ = spawn_connection(stream, tx.clone(), ctx.clone(), Arc::clone(&open), IDLE_TIMEOUT, token.clone());
    }
}

/// Admit and serve one connection, releasing its slot when serving ends.
fn spawn_connection(
    mut stream: TcpStream,
    tx: Sender<ControlRequest>,
    ctx: egui::Context,
    open: Arc<AtomicUsize>,
    idle_timeout: Duration,
    token: Option<Arc<str>>,
) -> Option<std::thread::JoinHandle<()>> {
    if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
        open.fetch_sub(1, Ordering::SeqCst);
        let _ = writeln!(stream, "{}", json!({"ok": false, "error": "too many control connections"}));
        return None;
    }
    Some(std::thread::spawn(move || {
        serve(stream, &tx, &ctx, idle_timeout, token.as_deref());
        open.fetch_sub(1, Ordering::SeqCst);
    }))
}

/// Where the app writes a generated token and where `lightcraft-cli` looks for it:
/// `<home>/.lightcraft/control-<port>.token` (user-readable only on Unix).
pub fn token_file(port: u16) -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(std::path::PathBuf::from(home).join(".lightcraft").join(format!("control-{port}.token")))
}

/// Delete `path` if it exists (an earlier run's token file); other errors are logged, not fatal.
fn remove_file_if_present(path: &std::path::Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log::warn!("could not remove the stale control token file {}: {e}", path.display()),
    }
}

/// 128 bits from the standard library's randomly keyed hasher (its keys come from the OS
/// CSPRNG). Good enough for a per-session loopback secret; swap in `getrandom` if the
/// workspace already depends on it.
pub fn random_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::with_capacity(32);
    for i in 0u64..2 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i ^ std::process::id() as u64);
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

/// Write `token` to `path` (creating its folder), readable by the owner only on Unix, whether or not
/// the file already existed.
fn write_token_at(path: &std::path::Path, token: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    // `mode` above applies only when the file is created: restrict one that already existed too.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(token.as_bytes())
}

/// Compare without leaking where the strings differ.
fn token_matches(expected: &str, given: &str) -> bool {
    let (a, b) = (expected.as_bytes(), given.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Five minutes allows pauses between automation commands without retaining abandoned slots forever.
const IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How long a request waits for its reply.
const REPLY_TIMEOUT: Duration = Duration::from_secs(60);
/// A request the app has not picked up while it drew no frame for this long is given up: the
/// window is not being redrawn (on Wayland, a window on a hidden workspace or minimized gets no
/// frame callbacks, so the app's loop is paused: issue #718).
const NO_FRAMES_AFTER: Duration = Duration::from_secs(5);

/// The reply for a request given up because the app drew no frames.
pub const HIDDEN_WINDOW_ERROR: &str = "the app drew no frame for 5 s, so the request was not run: its window is probably not visible \
(on Wayland a window on a hidden workspace or minimized is not redrawn, which pauses the app). Bring the window on screen and send it again";

/// Reads and writes go through the one `stream` (`&TcpStream` is `Read` and `Write`): no
/// `try_clone`. On Windows a cloned socket handle is inheritable, so every program the app starts
/// while a client is connected (a link in the browser, Show in Explorer, an external editor) kept a
/// copy of the connection: closing it then reached the client only when that program exited, as a
/// reset instead of a clean close.
fn serve(stream: TcpStream, tx: &Sender<ControlRequest>, ctx: &egui::Context, idle_timeout: Duration, token: Option<&str>) {
    if let Err(e) = stream.set_read_timeout(Some(idle_timeout)) {
        log::warn!("control connection closed: could not set idle read timeout: {e}");
        return;
    }
    serve_stream(stream, token, |method, params| {
        let (req, rrx) = ControlRequest::new(method, params);
        let state = Arc::clone(&req.state);
        tx.send(req).ok()?;
        ctx.request_repaint();
        Some(await_reply(&rrx, &state, || ctx.cumulative_pass_nr(), || ctx.request_repaint(), NO_FRAMES_AFTER, REPLY_TIMEOUT))
    });
}

/// The longest a closing connection is drained, however slowly its peer keeps sending.
const DRAIN_TIME: Duration = Duration::from_secs(1);

/// [`serve_lines`] on a socket, then close it so the peer reads every reply.
fn serve_stream(stream: TcpStream, token: Option<&str>, handle: impl FnMut(String, Value) -> Option<Value>) {
    serve_lines(&stream, &stream, token, handle);
    close_gracefully(&stream);
}

/// Close a connection the server is done with without destroying its last reply. A socket closed
/// while the peer's later lines are unread, or arrive after it, answers them with a reset, and a
/// reset can discard the replies the peer has not read yet (on Windows it does): the client sees
/// "connection reset" instead of why. Send FIN, then read and drop what is left (at most
/// [`MAX_LINE`] bytes, 500 ms per read and [`DRAIN_TIME`] in all) before closing. From #1970
/// (LochLogic).
fn close_gracefully(stream: &TcpStream) {
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let start = std::time::Instant::now();
    let mut sink = [0u8; 8192];
    let mut left = MAX_LINE;
    let mut reader = stream;
    while left > 0 {
        // a peer that keeps trickling bytes must not hold the connection slot past DRAIN_TIME
        let wait = DRAIN_TIME.saturating_sub(start.elapsed()).min(Duration::from_millis(500));
        if wait.is_zero() || stream.set_read_timeout(Some(wait)).is_err() {
            break;
        }
        match reader.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(n) => left = left.saturating_sub(n),
        }
    }
}

/// Wait for a request's reply. While the app has not picked the request up and `frames` (its
/// frame counter) stands still for `no_frames`, the request is abandoned (it will not run late)
/// and the reply says why; a request the app is running gets the whole `timeout`.
fn await_reply(
    rrx: &Receiver<Value>,
    state: &std::sync::atomic::AtomicU8,
    frames: impl Fn() -> u64,
    wake: impl Fn(),
    no_frames: Duration,
    timeout: Duration,
) -> Value {
    let start = std::time::Instant::now();
    let slice = (no_frames / 10).max(Duration::from_millis(1));
    let (mut last, mut since) = (frames(), std::time::Instant::now());
    loop {
        match rrx.recv_timeout(slice) {
            Ok(v) => return v,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return json!({"ok": false, "error": "the app dropped the request"}),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        let now = frames();
        if now != last {
            (last, since) = (now, std::time::Instant::now());
        }
        let taken = state.load(Ordering::SeqCst) == lightcraft_ui_egui::control::TAKEN;
        if !taken && since.elapsed() >= no_frames && ControlRequest::abandon(state) {
            return json!({"ok": false, "error": HIDDEN_WINDOW_ERROR});
        }
        if start.elapsed() >= timeout {
            // not run yet: make sure it never is; already running: it may still finish
            ControlRequest::abandon(state);
            return json!({"ok": false, "error": "timeout"});
        }
        wake();
    }
}

/// One parsed line.
enum Line {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Blank,
    /// Not a request: reply with this error and close the connection.
    Reject(String),
}

fn parse_line(bytes: &[u8]) -> Line {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Line::Reject("not a control request (invalid UTF-8); closing the connection".into());
    };
    let text = text.trim();
    if text.is_empty() {
        return Line::Blank;
    }
    let msg = match serde_json::from_str::<Value>(text) {
        Ok(v) => v,
        Err(e) => return Line::Reject(format!("bad JSON ({e}); expected one JSON request object per line; closing the connection")),
    };
    let Some(obj) = msg.as_object() else {
        return Line::Reject("not a control request (expected a JSON object); closing the connection".into());
    };
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return Line::Reject("not a control request (missing string `method`); closing the connection".into());
    };
    Line::Request {
        id: obj.get("id").cloned().unwrap_or(Value::Null),
        method: method.to_string(),
        params: obj.get("params").cloned().unwrap_or(json!({})),
    }
}

/// The error of the notice sent when an idle connection is closed. Clients match the reply's
/// `"closed": "idle"` marker; `crates/mcp/src/backend.rs` also matches this text, for apps from
/// before the marker, so keep the two in step.
const IDLE_CLOSE_ERROR: &str = "control connection idle read timed out; closing the connection";

/// Why the server closes a connection; its error reply says so.
#[derive(Debug, PartialEq)]
enum Closing {
    /// Nothing complete arrived within the idle timeout. Nothing more is read, so a request that
    /// crosses the notice is never run: the reply carries `"closed": "idle"` to tell clients it is
    /// safe to send that request again on a new connection.
    Idle,
    /// Junk, an overlong line or a failed read.
    Error(String),
}

impl Closing {
    fn reply(&self) -> Value {
        match self {
            Self::Idle => json!({"ok": false, "error": IDLE_CLOSE_ERROR, "closed": "idle"}),
            Self::Error(e) => json!({"ok": false, "error": e}),
        }
    }
}

/// Read one `\n`-terminated line of at most `max` bytes into `buf` (newline stripped).
/// `Ok(false)` at end of stream; `Err` when the line is too long or the read fails.
fn read_bounded_line(reader: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> Result<bool, Closing> {
    buf.clear();
    // `take` bounds the bytes buffered for one line: a peer can't make us allocate more than `max`.
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    let n = reader.take(limit).read_until(b'\n', buf).map_err(|e| match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => Closing::Idle,
        _ => Closing::Error(e.to_string()),
    })?;
    if n == 0 {
        return Ok(false);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        return Ok(true);
    }
    if buf.len() > max {
        return Err(Closing::Error(format!("request line longer than {max} bytes; closing the connection")));
    }
    // Last line without a newline, then end of stream.
    Ok(true)
}

/// Serve requests from `read`, answering on `out`, until the peer closes, sends something that is
/// not a request, or `handle` returns `None` (app gone). `handle` gets `(method, params)` and
/// returns the reply object (the request's `id` is added here). With `token`, the first line must
/// be an `auth` request carrying it; anything else gets one error reply and the connection closes,
/// before any command can run.
fn serve_lines<R: Read, W: Write>(read: R, mut out: W, token: Option<&str>, mut handle: impl FnMut(String, Value) -> Option<Value>) {
    let mut reader = BufReader::new(read);
    let mut buf = Vec::new();
    if let Some(expected) = token {
        let outcome = match read_bounded_line(&mut reader, &mut buf, MAX_LINE) {
            Ok(false) => return,
            Err(Closing::Idle) => return,
            Err(Closing::Error(e)) => Err(e),
            Ok(true) => match parse_line(&buf) {
                Line::Request { id, method, params } if method == "auth" => {
                    let given = params.get("token").and_then(Value::as_str).unwrap_or("");
                    if token_matches(expected, given) { Ok(id) } else { Err("control token rejected; closing the connection".to_string()) }
                }
                _ => Err("this control channel requires {\"method\":\"auth\",\"params\":{\"token\":…}} as the first line; closing the connection"
                    .to_string()),
            },
        };
        match outcome {
            Ok(id) => {
                let _ = writeln!(out, "{}", json!({"id": id, "ok": true, "result": "authenticated"})).and_then(|()| out.flush());
            }
            Err(reject) => {
                let _ = writeln!(out, "{}", json!({"ok": false, "error": reject})).and_then(|()| out.flush());
                return;
            }
        }
    }
    loop {
        let closing = match read_bounded_line(&mut reader, &mut buf, MAX_LINE) {
            Ok(false) => return,
            Err(closing) => closing,
            Ok(true) => match parse_line(&buf) {
                Line::Blank => continue,
                Line::Reject(e) => Closing::Error(e),
                Line::Request { id, method, params } => {
                    let Some(mut r) = handle(method, params) else { return };
                    if let Some(o) = r.as_object_mut() {
                        o.insert("id".into(), id);
                    }
                    if writeln!(out, "{r}").and_then(|()| out.flush()).is_err() {
                        return;
                    }
                    continue;
                }
            },
        };
        let _ = writeln!(out, "{}", closing.reply()).and_then(|()| out.flush());
        return;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Own the client and worker together: even a failing timeout assertion closes the socket
    /// before joining, so the regression cannot leave a worker stuck in a read.
    struct Connection {
        client: BufReader<TcpStream>,
        requests: Receiver<ControlRequest>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl Connection {
        /// `before` acts on the client before the worker starts reading, so what it sends is
        /// already there however late this thread gets scheduled.
        fn new(open: &Arc<AtomicUsize>, idle: Duration, before: impl FnOnce(&mut TcpStream)) -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            client.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let (stream, _) = listener.accept().unwrap();
            // Small test messages must not wait for Nagle's algorithm or a delayed ACK.
            client.set_nodelay(true).unwrap();
            stream.set_nodelay(true).unwrap();
            before(&mut client);
            let (tx, requests) = channel();
            let worker = spawn_connection(stream, tx, egui::Context::default(), Arc::clone(open), idle, None).unwrap();
            Self { client: BufReader::new(client), requests, worker: Some(worker) }
        }

        fn join(&mut self) {
            if let Some(worker) = self.worker.take() {
                worker.join().unwrap();
            }
        }
    }

    impl Drop for Connection {
        fn drop(&mut self) {
            let _ = self.client.get_ref().shutdown(std::net::Shutdown::Both);
            while let Ok(req) = self.requests.try_recv() {
                let _ = req.reply.send(json!({"ok": false, "error": "test closing"}));
            }
            self.join();
        }
    }

    #[test]
    fn idle_connections_release_their_slots_without_dispatching() {
        for input in ["", "{\"method\":\"engine.execute\""] {
            let open = Arc::new(AtomicUsize::new(0));
            let mut connection = Connection::new(&open, Duration::from_millis(100), |c| c.write_all(input.as_bytes()).unwrap());
            assert_eq!(open.load(Ordering::SeqCst), 1);
            let mut reply = String::new();
            let closed = connection.client.read_to_string(&mut reply);
            assert!(closed.is_ok(), "idle connection stayed open: {closed:?}");
            connection.join();
            assert_eq!(open.load(Ordering::SeqCst), 0, "the production worker releases its slot");
            assert!(connection.requests.try_recv().is_err(), "incomplete input never runs");
            let reply: Value = serde_json::from_str(&reply).unwrap();
            assert_eq!(reply["ok"], false);
            assert_eq!(reply["error"], IDLE_CLOSE_ERROR);
            assert_eq!(reply["closed"], "idle", "clients may resend a request that crossed this notice");
            assert!(reply.get("id").is_none());

            // A subsequent connection is admitted through the same accounting path.
            let mut next = Connection::new(&open, Duration::from_secs(1), |c| c.shutdown(std::net::Shutdown::Write).unwrap());
            let mut eof = String::new();
            next.client.read_to_string(&mut eof).unwrap();
            next.join();
            assert!(eof.is_empty());
            assert_eq!(open.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn persistent_requests_and_slow_replies_outlive_the_idle_read_timeout() {
        let open = Arc::new(AtomicUsize::new(0));
        let request = |id: u64| format!("{}\n", json!({"id": id, "method": "engine.commands"}));
        // Each request reaches the socket before the worker reads it (the next one while the worker
        // waits for the reply), so only the reply waits outlast the idle timeout, however slowly
        // this thread runs.
        let mut connection = Connection::new(&open, Duration::from_millis(100), |c| c.write_all(request(1).as_bytes()).unwrap());
        for id in [1, 2] {
            let req = connection.requests.recv_timeout(Duration::from_secs(10)).unwrap();
            assert_eq!(req.method, "engine.commands");
            assert!(req.take());
            if id == 1 {
                connection.client.get_mut().write_all(request(2).as_bytes()).unwrap();
            } else {
                connection.client.get_mut().shutdown(std::net::Shutdown::Write).unwrap();
            }
            // A dispatched command waits for its reply, not for socket input.
            std::thread::sleep(Duration::from_millis(250));
            req.reply.send(json!({"ok": true, "result": id})).unwrap();
            if id == 2 {
                // The worker must reach EOF even before the client reads its final reply.
                connection.join();
            }
            let mut reply = String::new();
            connection.client.read_line(&mut reply).unwrap();
            let reply: Value = serde_json::from_str(&reply).unwrap();
            assert_eq!(reply, json!({"id": id, "ok": true, "result": id}));
        }
        let mut eof = String::new();
        connection.client.read_to_string(&mut eof).unwrap();
        connection.join();
        assert!(eof.is_empty());
        assert_eq!(open.load(Ordering::SeqCst), 0);
    }

    /// Issue #718: an app that draws no frames (a hidden Wayland window) gets the request given
    /// up with a reason, quickly, and the app then skips it instead of running it late.
    #[test]
    fn a_request_to_an_app_drawing_no_frames_is_given_up_with_a_reason() {
        let (req, rrx) = ControlRequest::new("ui.inspect", json!({}));
        let t0 = std::time::Instant::now();
        let r = await_reply(&rrx, &req.state, || 7, || {}, Duration::from_millis(50), Duration::from_secs(10));
        // the frame window (50 ms), not the 10 s timeout; the margin is for a slow machine
        assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
        assert_eq!(r["ok"], false);
        assert!(r["error"].as_str().unwrap().contains("not visible"), "{r}");
        assert!(!req.take(), "the app does not run it once it shows up");
    }

    /// A request the app is running is not given up because frames stopped (a long command
    /// blocks the frame loop), and its reply arrives.
    #[test]
    fn a_request_being_run_waits_for_its_reply() {
        let (req, rrx) = ControlRequest::new("export.run", json!({}));
        assert!(req.take());
        let reply = req.reply.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let _ = reply.send(json!({"ok": true, "result": 1}));
        });
        let r = await_reply(&rrx, &req.state, || 7, || {}, Duration::from_millis(20), Duration::from_secs(10));
        assert_eq!(r["ok"], true, "{r}");
        worker.join().unwrap();
    }

    /// Frames still coming: the request just waits, then is answered.
    #[test]
    fn a_drawing_app_is_waited_for() {
        let (req, rrx) = ControlRequest::new("ui.inspect", json!({}));
        let n = std::sync::atomic::AtomicU64::new(0);
        let reply = req.reply.clone();
        let state = Arc::clone(&req.state);
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            let _ = reply.send(json!({"ok": true, "result": 2}));
        });
        let r = await_reply(&rrx, &state, || n.fetch_add(1, Ordering::SeqCst), || {}, Duration::from_millis(30), Duration::from_secs(10));
        assert_eq!(r["ok"], true, "{r}");
        worker.join().unwrap();
    }

    /// [`serve_lines`] on `input` without a token: the replies and the methods that ran.
    fn run(input: &[u8]) -> (Vec<Value>, Vec<String>) {
        let mut out = Vec::new();
        let mut calls = Vec::new();
        serve_lines(input, &mut out, None, |m, _p| {
            calls.push(m);
            Some(json!({"ok": true, "result": null}))
        });
        let replies = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).collect();
        (replies, calls)
    }

    /// [`serve_lines`] on `input` with `token` required: the replies and the methods that ran.
    fn run_with_token(token: &str, input: &[u8]) -> (Vec<Value>, Vec<String>) {
        let mut out = Vec::new();
        let mut calls = Vec::new();
        serve_lines(input, &mut out, Some(token), |m, _p| {
            calls.push(m);
            Some(json!({"ok": true, "result": null}))
        });
        let replies = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).collect();
        (replies, calls)
    }

    #[test]
    fn a_token_from_the_command_line_or_environment_is_trimmed_and_may_be_absent() {
        assert_eq!(
            clean_token(Some(
                "  abc
"
                .into()
            ))
            .as_deref(),
            Some("abc")
        );
        assert_eq!(clean_token(Some(" 	 ".into())), None);
        assert_eq!(clean_token(Some(String::new())), None);
        assert_eq!(clean_token(None), None);
    }

    #[test]
    fn the_token_file_is_replaced_and_owner_only_even_when_it_already_existed() {
        let dir = std::env::temp_dir().join(format!("lc-token-at-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("nested").join("control-1.token");
        write_token_at(&file, "first-and-longer").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // a file left by something else, readable by everyone
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        write_token_at(&file, "second").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "second");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fresh folder for a token file; the caller removes it.
    fn temp_token_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lc-token-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_memory_session_generates_a_token_but_writes_no_file() {
        let dir = temp_token_dir("memory");
        let path = dir.join("control-1.token");
        let token = prepare_token(Some(&path), None, false, true).expect("a token is generated");
        assert_eq!(token.len(), 32);
        assert!(!path.exists(), "--memory writes no token file");
        // with authentication off nothing is removed either: nothing of ours is on disk
        std::fs::write(&path, "someone else's").unwrap();
        assert_eq!(prepare_token(Some(&path), None, true, true), None);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "someone else's");
        // a normal run writes the token, and without authentication removes the file again
        let token = prepare_token(Some(&path), None, false, false).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), token);
        assert_eq!(prepare_token(Some(&path), None, true, false), None);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_taken_port_is_reported_before_any_token_is_written() {
        let held = bind(0).unwrap();
        let port = held.local_addr().unwrap().port();
        assert!(bind(port).is_err(), "the second bind fails");
        // the startup sequence main.rs runs: another run's token file must survive a failed bind
        let dir = temp_token_dir("taken");
        let path = dir.join(format!("control-{port}.token"));
        std::fs::write(&path, "sentinel").unwrap();
        let result = std::panic::catch_unwind(|| {
            for (explicit, no_auth, memory) in [(None, false, false), (Some("t".to_string()), false, false), (None, true, false), (None, false, true)]
            {
                let err = start_control(port, explicit, no_auth, memory, Some(path.clone()), egui::Context::default());
                assert!(err.is_err(), "the port is held");
                assert_eq!(std::fs::read_to_string(&path).unwrap(), "sentinel", "no_auth={no_auth} memory={memory}");
            }
        });
        let _ = std::fs::remove_dir_all(&dir);
        assert!(result.is_ok(), "a failed bind touched the token file");
    }

    #[test]
    fn a_stale_token_file_is_removed_and_a_missing_one_is_fine() {
        let dir = std::env::temp_dir().join(format!("lc-token-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("control-1.token");
        std::fs::write(&file, "old").unwrap();
        remove_file_if_present(&file);
        assert!(!file.exists());
        remove_file_if_present(&file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn token_gate_runs_nothing_until_authenticated() {
        // no auth line at all
        let (r, calls) = run_with_token("s3cret", b"{\"id\":1,\"method\":\"engine.execute\"}\n");
        assert!(calls.is_empty(), "{calls:?}");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["ok"], false);
        // wrong token
        let (r, calls) = run_with_token("s3cret", b"{\"id\":0,\"method\":\"auth\",\"params\":{\"token\":\"nope\"}}\n{\"method\":\"a\"}\n");
        assert!(calls.is_empty());
        assert_eq!(r[0]["ok"], false);
        // right token: the auth reply, then commands run as before
        let (r, calls) = run_with_token("s3cret", b"{\"id\":0,\"method\":\"auth\",\"params\":{\"token\":\"s3cret\"}}\n{\"id\":1,\"method\":\"a\"}\n");
        assert_eq!(calls, ["a"]);
        assert_eq!(r[0], json!({"id": 0, "ok": true, "result": "authenticated"}));
        assert_eq!(r[1]["id"], 1);
    }

    #[test]
    fn requests_are_answered_in_order_with_ids() {
        let (r, calls) = run(b"{\"id\":1,\"method\":\"a\"}\n\n{\"id\":2,\"method\":\"b\",\"params\":{}}\r\n{\"method\":\"c\"}");
        assert_eq!(calls, ["a", "b", "c"]);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0]["id"], 1);
        assert_eq!(r[1]["id"], 2);
        assert_eq!(r[2]["id"], Value::Null);
    }

    #[test]
    fn http_request_closes_before_the_body_runs() {
        // A browser's cross-origin `fetch` POST: the request line is rejected and the connection
        // closed, so the JSON body is never executed.
        let http = b"POST / HTTP/1.1\r\nHost: 127.0.0.1:7980\r\nContent-Type: text/plain\r\n\r\n\
            {\"id\":1,\"method\":\"engine.execute\",\"params\":{\"command\":\"folder.move\"}}\n";
        let (r, calls) = run(http);
        assert!(calls.is_empty(), "nothing may run: {calls:?}");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["ok"], false);
        assert!(r[0]["error"].as_str().unwrap().contains("closing the connection"));
        assert!(r[0].get("closed").is_none(), "only an idle close invites a resend: {}", r[0]);
    }

    #[test]
    fn non_request_json_closes_the_connection() {
        for junk in [
            &b"[1,2]\n{\"method\":\"a\"}\n"[..],
            b"{\"id\":1}\n{\"method\":\"a\"}\n",
            b"{\"method\":5}\n{\"method\":\"a\"}\n",
            b"\xff\xfe\n{\"method\":\"a\"}\n",
        ] {
            let (r, calls) = run(junk);
            assert!(calls.is_empty());
            assert_eq!(r.len(), 1);
            assert_eq!(r[0]["ok"], false);
        }
    }

    #[test]
    fn a_request_before_junk_still_runs() {
        let (r, calls) = run(b"{\"method\":\"a\"}\nGET / HTTP/1.1\n{\"method\":\"b\"}\n");
        assert_eq!(calls, ["a"]);
        assert_eq!(r.len(), 2);
        assert_eq!(r[1]["ok"], false);
    }

    #[test]
    fn overlong_line_is_refused_without_buffering_it_all() {
        // An endless line without a newline: reading stops after MAX_LINE + 1 bytes.
        let endless = std::io::repeat(b'x');
        let mut out = Vec::new();
        let mut ran = false;
        serve_lines(endless, &mut out, None, |_, _| {
            ran = true;
            None
        });
        assert!(!ran);
        let reply: Value = serde_json::from_slice(out.strip_suffix(b"\n").unwrap()).unwrap();
        assert!(reply["error"].as_str().unwrap().contains("longer than"));
    }

    #[test]
    fn line_at_the_limit_is_accepted() {
        let mut buf = Vec::new();
        let line = vec![b'a'; 10];
        let mut input = line.clone();
        input.push(b'\n');
        assert_eq!(read_bounded_line(&mut &input[..], &mut buf, 10), Ok(true));
        assert_eq!(buf, line);
        assert!(read_bounded_line(&mut &[b'a'; 11][..], &mut buf, 10).is_err());
    }

    /// A program the app starts while a client is connected doesn't keep the connection open:
    /// when the server closes it, the client sees that at once, not when the program exits.
    #[cfg(windows)]
    #[test]
    fn a_program_started_meanwhile_does_not_hold_the_connection_open() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, _rx) = channel::<ControlRequest>();
        let server = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            serve(s, &tx, &egui::Context::default(), IDLE_TIMEOUT, None);
        });
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        // the connection is being served: start a program that lives about 4 s
        std::thread::sleep(Duration::from_millis(200));
        let mut child = std::process::Command::new("cmd").args(["/c", "ping -n 5 127.0.0.1 >nul"]).spawn().unwrap();
        std::thread::sleep(Duration::from_millis(200));
        // junk makes the server answer once and close
        c.write_all(b"hello\n").unwrap();
        let t0 = std::time::Instant::now();
        let mut text = String::new();
        let read = c.read_to_string(&mut text);
        let waited = t0.elapsed();
        server.join().unwrap();
        let _ = child.wait();
        assert!(read.is_ok(), "the close must be clean, not a reset: {read:?}");
        assert!(text.contains("closing the connection"), "{text}");
        assert!(waited < Duration::from_secs(2), "the client saw the close only after {waited:?}, when the program exited");
    }

    #[test]
    fn tcp_connection_is_closed_after_junk() {
        // End to end over a real loopback socket (the transport the app uses).
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            serve_stream(s, None, |_, _| Some(json!({"ok": true})));
        });
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        c.write_all(b"{\"id\":7,\"method\":\"x\"}\nhello\n").unwrap();
        let mut replies = BufReader::new(c.try_clone().unwrap());
        let (mut first, mut second) = (String::new(), String::new());
        replies.read_line(&mut first).unwrap();
        replies.read_line(&mut second).unwrap();
        assert!(first.contains("\"id\":7"), "{first}");
        assert!(second.contains("closing the connection"), "{second}");
        // A client that keeps sending after the junk: its next line reaches the server after the
        // rejection. A socket closed by then answers that line with a reset, which the client reads
        // as "connection reset" (os error 10053 / 10054 on Windows) instead of the end of the
        // replies. The pause lets a reset arrive before the read.
        c.write_all(b"{\"method\":\"y\"}\n").unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let mut rest = String::new();
        let read = replies.read_to_string(&mut rest);
        server.join().unwrap();
        assert!(read.is_ok(), "the close must be clean, not a reset: {read:?}");
        assert_eq!(rest, "", "nothing follows the rejection");
    }

    #[test]
    fn a_closing_connection_is_not_held_open_by_a_trickling_peer() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server_done = Arc::clone(&done);
        let server = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            let t0 = std::time::Instant::now();
            close_gracefully(&s);
            server_done.store(true, Ordering::SeqCst);
            t0.elapsed()
        });
        let mut c = TcpStream::connect(addr).unwrap();
        // one byte every 100 ms: each read returns well within its 500 ms timeout
        for _ in 0..100 {
            if done.load(Ordering::SeqCst) || c.write_all(b"x").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let drained = server.join().unwrap();
        assert!(drained < DRAIN_TIME + Duration::from_secs(1), "drained for {drained:?}");
    }
}
