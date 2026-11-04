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

pub fn start(port: u16, ctx: egui::Context) -> Receiver<ControlRequest> {
    let (tx, rx) = channel::<ControlRequest>();
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            log::error!("control server failed to bind 127.0.0.1:{port}: {e}");
            return rx;
        }
    };
    log::info!("control server listening on 127.0.0.1:{port}");
    std::thread::spawn(move || accept_loop(listener, tx, ctx));
    rx
}

fn accept_loop(listener: TcpListener, tx: Sender<ControlRequest>, ctx: egui::Context) {
    let open = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming().flatten() {
        let _ = spawn_connection(stream, tx.clone(), ctx.clone(), Arc::clone(&open), IDLE_TIMEOUT);
    }
}

/// Admit and serve one connection, releasing its slot when serving ends.
fn spawn_connection(
    mut stream: TcpStream,
    tx: Sender<ControlRequest>,
    ctx: egui::Context,
    open: Arc<AtomicUsize>,
    idle_timeout: Duration,
) -> Option<std::thread::JoinHandle<()>> {
    if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
        open.fetch_sub(1, Ordering::SeqCst);
        let _ = writeln!(stream, "{}", json!({"ok": false, "error": "too many control connections"}));
        return None;
    }
    Some(std::thread::spawn(move || {
        serve(stream, &tx, &ctx, idle_timeout);
        open.fetch_sub(1, Ordering::SeqCst);
    }))
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
fn serve(stream: TcpStream, tx: &Sender<ControlRequest>, ctx: &egui::Context, idle_timeout: Duration) {
    if let Err(e) = stream.set_read_timeout(Some(idle_timeout)) {
        log::warn!("control connection closed: could not set idle read timeout: {e}");
        return;
    }
    serve_stream(stream, |method, params| {
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
fn serve_stream(stream: TcpStream, handle: impl FnMut(String, Value) -> Option<Value>) {
    serve_lines(&stream, &stream, handle);
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
    let mut sink = [0u8; 8192];
    let mut reader = stream;
    drain(std::time::Instant::now, |wait| {
        stream.set_read_timeout(Some(wait))?;
        reader.read(&mut sink)
    });
}

/// Read and drop what a closing connection's peer still sends: [`MAX_LINE`] bytes, 500 ms per read and
/// [`DRAIN_TIME`] in all, on the clock `now`. `read` reads once, waiting `wait` at most (`Ok(0)` at the end, an
/// error when nothing came). The tests pass a clock and a peer of their own, which shows how long the drain
/// goes on without timing it.
fn drain(now: impl Fn() -> std::time::Instant, mut read: impl FnMut(Duration) -> std::io::Result<usize>) {
    let start = now();
    let mut left = MAX_LINE;
    while left > 0 {
        // a peer that keeps trickling bytes must not hold the connection slot past DRAIN_TIME
        let wait = DRAIN_TIME.saturating_sub(now().saturating_duration_since(start)).min(Duration::from_millis(500));
        if wait.is_zero() {
            break;
        }
        match read(wait) {
            Ok(0) | Err(_) => break,
            Ok(n) => left = left.saturating_sub(n),
        }
    }
}

/// Where [`await_reply_on`] gets the time and waits for a reply: the wall clock in the app, a clock of their own in
/// the tests (which shows when a request is given up without timing it).
trait ReplyClock {
    fn now(&self) -> std::time::Instant;
    /// Wait for the reply, `slice` at most.
    fn wait(&mut self, rrx: &Receiver<Value>, slice: Duration) -> Result<Value, std::sync::mpsc::RecvTimeoutError>;
}

/// The wall clock.
struct WallClock;

impl ReplyClock for WallClock {
    fn now(&self) -> std::time::Instant {
        std::time::Instant::now()
    }

    fn wait(&mut self, rrx: &Receiver<Value>, slice: Duration) -> Result<Value, std::sync::mpsc::RecvTimeoutError> {
        rrx.recv_timeout(slice)
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
    await_reply_on(&mut WallClock, rrx, state, frames, wake, no_frames, timeout)
}

/// [`await_reply`] on `clock`.
fn await_reply_on(
    clock: &mut impl ReplyClock,
    rrx: &Receiver<Value>,
    state: &std::sync::atomic::AtomicU8,
    frames: impl Fn() -> u64,
    wake: impl Fn(),
    no_frames: Duration,
    timeout: Duration,
) -> Value {
    let start = clock.now();
    let slice = (no_frames / 10).max(Duration::from_millis(1));
    let (mut last, mut since) = (frames(), clock.now());
    loop {
        match clock.wait(rrx, slice) {
            Ok(v) => return v,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return json!({"ok": false, "error": "the app dropped the request"}),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        let now = frames();
        if now != last {
            (last, since) = (now, clock.now());
        }
        let taken = state.load(Ordering::SeqCst) == lightcraft_ui_egui::control::TAKEN;
        if !taken && clock.now().saturating_duration_since(since) >= no_frames && ControlRequest::abandon(state) {
            return json!({"ok": false, "error": HIDDEN_WINDOW_ERROR});
        }
        if clock.now().saturating_duration_since(start) >= timeout {
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
/// returns the reply object (the request's `id` is added here).
fn serve_lines<R: Read, W: Write>(read: R, mut out: W, mut handle: impl FnMut(String, Value) -> Option<Value>) {
    let mut reader = BufReader::new(read);
    let mut buf = Vec::new();
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
            client.set_read_timeout(Some(lightcraft_testkit::scaled(Duration::from_secs(3)).max(Duration::from_secs(3)))).unwrap();
            let (stream, _) = listener.accept().unwrap();
            // Small test messages must not wait for Nagle's algorithm or a delayed ACK.
            client.set_nodelay(true).unwrap();
            stream.set_nodelay(true).unwrap();
            before(&mut client);
            let (tx, requests) = channel();
            let worker = spawn_connection(stream, tx, egui::Context::default(), Arc::clone(open), idle).unwrap();
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
            let req = lightcraft_testkit::recv_within(&connection.requests, Duration::from_secs(10), "the request reaches the app");
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
        // Given up by the frame window (50 ms): the timeout (a hang guard, a minute stretched by
        // LIGHTCRAFT_TIME_SCALE) answers "timeout".
        let timeout = lightcraft_testkit::scaled(Duration::from_secs(60)).max(Duration::from_secs(60));
        let r = await_reply(&rrx, &req.state, || 7, || {}, Duration::from_millis(50), timeout);
        assert_eq!(r["ok"], false);
        assert!(r["error"].as_str().unwrap().contains("not visible"), "{r}");
        assert!(!req.take(), "the app does not run it once it shows up");
        // When: on a clock that moves only while the wait waits, it is given up as soon as the frame window has
        // passed (the waits are slices of a tenth of it), not after waiting out more of the timeout first.
        let (req, rrx) = ControlRequest::new("ui.inspect", json!({}));
        let mut clock = Virtual::new();
        let r = await_reply_on(&mut clock, &rrx, &req.state, || 7, || {}, Duration::from_millis(50), Duration::from_secs(3600));
        assert!(r["error"].as_str().unwrap().contains("not visible"), "{r}");
        assert_eq!(clock.waits, [Duration::from_millis(5); 10], "given up after the 50 ms frame window");
        assert!(!req.take());
        // and with frames coming, at the timeout
        let (req, rrx) = ControlRequest::new("ui.inspect", json!({}));
        let mut clock = Virtual::new();
        let n = std::sync::atomic::AtomicU64::new(0);
        let r = await_reply_on(
            &mut clock,
            &rrx,
            &req.state,
            || n.fetch_add(1, Ordering::SeqCst),
            || {},
            Duration::from_millis(50),
            Duration::from_secs(1),
        );
        assert_eq!(r["error"], "timeout", "{r}");
        assert_eq!(clock.at, Duration::from_secs(1), "given up at the timeout");
        assert!(!req.take());
    }

    /// A clock that moves only while [`await_reply_on`] waits, by the slice it waits (no reply comes), and records
    /// the slices.
    struct Virtual {
        /// (any instant will do as the start: only the time since it counts)
        base: std::time::Instant,
        at: Duration,
        waits: Vec<Duration>,
    }

    impl Virtual {
        fn new() -> Self {
            Virtual { base: std::time::Instant::now(), at: Duration::ZERO, waits: Vec::new() }
        }
    }

    impl ReplyClock for Virtual {
        fn now(&self) -> std::time::Instant {
            self.base + self.at
        }

        fn wait(&mut self, _: &Receiver<Value>, slice: Duration) -> Result<Value, std::sync::mpsc::RecvTimeoutError> {
            self.at += slice;
            self.waits.push(slice);
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        }
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
        let r = await_reply(&rrx, &req.state, || 7, || {}, Duration::from_millis(20), lightcraft_testkit::scaled(Duration::from_secs(10)));
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
        let r = await_reply(
            &rrx,
            &state,
            || n.fetch_add(1, Ordering::SeqCst),
            || {},
            Duration::from_millis(30),
            lightcraft_testkit::scaled(Duration::from_secs(10)),
        );
        assert_eq!(r["ok"], true, "{r}");
        worker.join().unwrap();
    }

    fn run(input: &[u8]) -> (Vec<Value>, Vec<String>) {
        let mut out = Vec::new();
        let mut calls = Vec::new();
        serve_lines(input, &mut out, |m, _p| {
            calls.push(m);
            Some(json!({"ok": true, "result": null}))
        });
        let replies = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).collect();
        (replies, calls)
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
        serve_lines(endless, &mut out, |_, _| {
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
        let (tx, requests) = channel::<ControlRequest>();
        let server = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            serve(s, &tx, &egui::Context::default(), IDLE_TIMEOUT);
        });
        // the program lives until the test ends it (`ping -n 3600` waits a second between pings: an hour). The
        // client's read timeout is a hang guard (a minute, stretched by LIGHTCRAFT_TIME_SCALE): a connection the
        // program held open would end only with the program, so the read would time out instead.
        let guard = lightcraft_testkit::scaled(Duration::from_secs(60)).max(Duration::from_secs(60));
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(guard)).unwrap();
        let mut replies = BufReader::new(c.try_clone().unwrap());
        // one request answered first: the connection is being served (every handle the server makes for it
        // exists) before the program starts
        c.write_all(b"{\"id\":1,\"method\":\"x\"}\n").unwrap();
        let req = lightcraft_testkit::recv_within(&requests, Duration::from_secs(30), "the request reaches the app");
        let _ = req.take();
        let _ = req.reply.send(json!({"ok": true, "result": 1}));
        let mut first = String::new();
        replies.read_line(&mut first).unwrap();
        assert!(first.contains("\"id\":1"), "{first}");
        let mut child = std::process::Command::new("ping").args(["-n", "3600", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
        // junk makes the server answer once and close
        c.write_all(b"hello\n").unwrap();
        let mut text = String::new();
        let read = replies.read_to_string(&mut text);
        let still_running = child.try_wait().is_ok_and(|status| status.is_none());
        let _ = child.kill();
        let _ = child.wait();
        server.join().unwrap();
        assert!(read.is_ok(), "the close must be clean, not a reset or a wait for the program to exit: {read:?}");
        assert!(text.contains("closing the connection"), "{text}");
        assert!(still_running, "the client saw the close while the program still ran");
    }

    #[test]
    fn tcp_connection_is_closed_after_junk() {
        // End to end over a real loopback socket (the transport the app uses).
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            serve_stream(s, |_, _| Some(json!({"ok": true})));
        });
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(lightcraft_testkit::scaled(Duration::from_secs(10)).max(Duration::from_secs(10)))).unwrap();
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
            close_gracefully(&s);
            server_done.store(true, Ordering::SeqCst);
        });
        let mut c = TcpStream::connect(addr).unwrap();
        // one byte every 100 ms (each read returns well within its 500 ms timeout) until the server is done. The
        // peer gives up only after a hang guard (a minute, stretched by LIGHTCRAFT_TIME_SCALE): a drain that
        // ignored DRAIN_TIME would last until then.
        let guard = lightcraft_testkit::scaled(Duration::from_secs(60)).max(Duration::from_secs(60));
        let t0 = std::time::Instant::now();
        while !done.load(Ordering::SeqCst) && c.write_all(b"x").is_ok() {
            assert!(t0.elapsed() < guard, "the drain went on while the peer trickled");
            std::thread::sleep(Duration::from_millis(100));
        }
        server.join().unwrap();
    }

    /// How long the drain goes on, on a clock that moves only while it reads: a peer that trickles a byte every
    /// 100 ms is drained for DRAIN_TIME (each read waiting 500 ms at most), one that sends nothing for one read,
    /// one that hangs up not at all.
    #[test]
    fn the_drain_ends_at_drain_time_however_the_peer_trickles() {
        let base = std::time::Instant::now();
        let at = std::cell::Cell::new(Duration::ZERO);
        let mut waits = Vec::new();
        drain(
            || base + at.get(),
            |wait| {
                waits.push(wait);
                at.set(at.get() + Duration::from_millis(100));
                Ok(1)
            },
        );
        assert_eq!(at.get(), DRAIN_TIME, "drained for DRAIN_TIME");
        assert!(waits.len() == 10 && waits.iter().all(|w| *w <= Duration::from_millis(500)), "{waits:?}");
        let (mut quiet, mut closed) = (0, 0);
        drain(
            || base,
            |_| {
                quiet += 1;
                Err(std::io::ErrorKind::WouldBlock.into())
            },
        );
        drain(
            || base,
            |_| {
                closed += 1;
                Ok(0)
            },
        );
        assert_eq!((quiet, closed), (1, 1));
        // and at most MAX_LINE bytes, however fast they come
        let mut reads = 0;
        drain(
            || base,
            |_| {
                reads += 1;
                Ok(MAX_LINE / 4)
            },
        );
        assert_eq!(reads, 4);
    }
}
