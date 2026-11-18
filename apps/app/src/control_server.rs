//! Loopback JSON-lines control server: one request per line, one reply per line.
//! This is the transport the MCP server (`<cli> mcp --connect`) wraps.
//!
//! The server only ever reads requests: a line that is not a JSON object with a string `method`
//! (an HTTP request line from a browser's cross-origin `fetch`, a stray `nc`, binary junk) gets
//! one error reply and the connection is closed, so nothing after it is executed. Lines are
//! bounded ([`MAX_LINE`]) and so are connections: at most [`MAX_PENDING`] not yet authenticated,
//! each of which must authenticate within [`AUTH_DEADLINE`], and at most [`MAX_CONNECTIONS`]
//! authenticated, each closed after [`IDLE_TIMEOUT`] without a request (clients reconnect). Silent or
//! unauthenticated connections therefore can't take the slots MCP and the CLI need; junk
//! never reaches the UI thread. The first request on a connection must be `auth` with the run's
//! token ([`lcc_engine::control_auth`]); anything else gets one error reply and the
//! connection is closed, so a process or web page that only knows the port can't run commands.
//! See `docs/control-protocol.md`.
//!
//! A connection that subscribes to a control surface's layout (`surface.subscribe`,
//! `docs/control-surfaces.md`) also receives pushed event lines (`{"event": ...}`) between its
//! replies; lines never interleave. The surfaces-only listener ([`start_surfaces`], Settings,
//! Controls) serves nothing but `surface.*` methods; it takes no `auth` (surfaces pair instead,
//! `surface.pair`) and its connections may stay idle, waiting for pushed events.

use std::cell::Cell;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lcc_engine::control_auth;
use lcc_ui_egui::ControlRequest;
use lcc_ui_egui::surface::EventSink;
use serde_json::{Value, json};

/// Longest accepted request line (bytes, without the newline). Requests are small JSON objects;
/// anything longer is refused and the connection closed.
pub const MAX_LINE: usize = 4 * 1024 * 1024;
/// Authenticated connections served at once; a further one is refused after its `auth`.
pub const MAX_CONNECTIONS: usize = 16;
/// Connections not yet authenticated at once; further ones get an error line and are closed.
pub const MAX_PENDING: usize = 16;
/// A connection must have authenticated this long after it was accepted, or it is closed.
pub const AUTH_DEADLINE: Duration = Duration::from_secs(10);
/// An authenticated connection that sends no request for this long is closed.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// A surfaces-only connection may wait this long for pushed events without sending a request.
pub const SURFACE_IDLE_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// Connection numbers (0 is the in-process channel).
static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

/// Open-connection counts, kept apart so unauthenticated peers can't use up authenticated slots.
#[derive(Default)]
struct Slots {
    pending: AtomicUsize,
    authed: AtomicUsize,
}

impl Slots {
    /// Take one slot of `counter` if fewer than `max` are taken.
    fn take(counter: &AtomicUsize, max: usize) -> bool {
        counter.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| (n < max).then_some(n + 1)).is_ok()
    }
}

/// Reads from a socket with the connection's current deadline: until `auth_by` before it
/// authenticates (an absolute deadline, so dribbling bytes doesn't extend it), then `idle` per read.
struct TimedReader<'a> {
    stream: &'a TcpStream,
    auth_by: Instant,
    idle: Duration,
    authed: &'a Cell<bool>,
}

impl Read for TimedReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let (wait, why) = if self.authed.get() {
            (self.idle, IDLE_CLOSE_ERROR)
        } else {
            (self.auth_by.saturating_duration_since(Instant::now()), "no `auth` request in time; closing the connection")
        };
        let timed_out = || std::io::Error::new(std::io::ErrorKind::TimedOut, why);
        if wait.is_zero() {
            return Err(timed_out());
        }
        self.stream.set_read_timeout(Some(wait))?;
        let mut s = self.stream;
        s.read(buf).map_err(|e| if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) { timed_out() } else { e })
    }
}

/// Serve the control channel on `127.0.0.1:port`, requests going to `tx`; every connection must
/// authenticate with `token`. `publish` (which stores the token for clients) runs only once the
/// port is ours: a second instance started with the same port must not replace the running
/// instance's token and then fail to bind, which would lock every client out of the first one.
/// `false` when the bind fails.
pub fn start(port: u16, token: String, tx: Sender<ControlRequest>, ctx: egui::Context, publish: impl FnOnce(&str)) -> bool {
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            log::error!("control server failed to bind 127.0.0.1:{port}: {e}");
            return false;
        }
    };
    publish(&token);
    log::info!("control server listening on 127.0.0.1:{port}");
    std::thread::spawn(move || accept_loop(listener, tx, ctx, Some(Arc::new(token)), None));
    true
}

/// Start the surfaces-only listener on `port` (0: any free port); set the returned flag to stop
/// it. Returns the flag and the port.
pub fn start_surfaces(port: u16, tx: Sender<ControlRequest>, ctx: egui::Context) -> Result<(Arc<AtomicBool>, u16), String> {
    let l = TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("can't listen on 127.0.0.1:{port}: {e}"))?;
    l.set_nonblocking(true).map_err(|e| e.to_string())?;
    let port = l.local_addr().map(|a| a.port()).unwrap_or(port);
    log::info!("control-surface listener on 127.0.0.1:{port}");
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    std::thread::spawn(move || accept_loop(l, tx, ctx, None, Some(flag)));
    Ok((stop, port))
}

/// Accept connections until `stop` is set (a non-blocking listener) or forever. With a `token`
/// every method is served after `auth`; without one only `surface.*` (the surfaces-only port).
fn accept_loop(listener: TcpListener, tx: Sender<ControlRequest>, ctx: egui::Context, token: Option<Arc<String>>, stop: Option<Arc<AtomicBool>>) {
    let slots = Arc::new(Slots::default());
    let idle = if token.is_some() { IDLE_TIMEOUT } else { SURFACE_IDLE_TIMEOUT };
    loop {
        if stop.as_ref().is_some_and(|s| s.load(Ordering::SeqCst)) {
            log::info!("control listener stopped");
            return;
        }
        let stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(_) => continue,
        };
        // a non-blocking listener hands out non-blocking sockets on some systems
        let _ = stream.set_nonblocking(false);
        let tx = tx.clone();
        let ctx = ctx.clone();
        let token = token.clone();
        accept_one(stream, &slots, AUTH_DEADLINE, idle, move |stream, timeouts| {
            serve(stream, &tx, &ctx, token.as_deref().map(String::as_str), timeouts)
        });
    }
}

/// Deadlines and slots one connection is served with.
struct Timeouts {
    slots: Arc<Slots>,
    auth_by: Instant,
    idle: Duration,
}

/// Admit `stream` as a pending connection (or refuse it when [`MAX_PENDING`] are pending) and
/// serve it on its own thread.
fn accept_one(
    mut stream: TcpStream,
    slots: &Arc<Slots>,
    auth_deadline: Duration,
    idle: Duration,
    serve: impl FnOnce(TcpStream, &Timeouts) + Send + 'static,
) {
    if !Slots::take(&slots.pending, MAX_PENDING) {
        let _ = writeln!(stream, "{}", json!({"ok": false, "error": "too many unauthenticated control connections"}));
        close_gracefully(&stream);
        return;
    }
    let t = Timeouts { slots: Arc::clone(slots), auth_by: Instant::now() + auth_deadline, idle };
    std::thread::spawn(move || serve(stream, &t));
}

/// How long a request waits for its reply.
const REPLY_TIMEOUT: Duration = Duration::from_secs(60);
/// A request the app has not picked up while it drew no frame for this long is given up: the
/// window is not being redrawn (on Wayland, a window on a hidden workspace or minimized gets no
/// frame callbacks, so the app's loop is paused: issue #718).
const NO_FRAMES_AFTER: Duration = Duration::from_secs(5);

/// The reply for a request given up because the app drew no frames.
pub const HIDDEN_WINDOW_ERROR: &str = "the app drew no frame for 5 s, so the request was not run: its window is probably not visible \
(on Wayland a window on a hidden workspace or minimized is not redrawn, which pauses the app). Bring the window on screen and send it again";

/// A writer shared by a connection's replies and its pushed events: each line goes out whole.
/// It writes through the connection's one socket (no `try_clone`, see [`serve_stream`]).
#[derive(Clone)]
struct LineWriter {
    out: Arc<TcpStream>,
    lock: Arc<Mutex<()>>,
    buf: Vec<u8>,
}

impl Write for LineWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let data = std::mem::take(&mut self.buf);
        let _line = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let mut out: &TcpStream = &self.out;
        out.write_all(&data)?;
        out.flush()
    }
}

/// Write pushed events to the connection until it closes.
fn pump_events(events: Receiver<Value>, mut out: LineWriter, open: Arc<AtomicBool>) {
    loop {
        match events.recv_timeout(Duration::from_millis(500)) {
            Ok(v) => {
                if writeln!(out, "{v}").and_then(|()| out.flush()).is_err() {
                    open.store(false, Ordering::SeqCst);
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) if open.load(Ordering::SeqCst) => {}
            Err(_) => return,
        }
    }
}

/// Reads and writes go through the one `stream` ([`serve_stream`]): no `try_clone`. On Windows a
/// cloned socket handle is inheritable, so every program the app starts while a client is
/// connected (a link in the browser, Show in Explorer, an external editor) kept a copy of the
/// connection: closing it then reached the client only when that program exited, as a reset
/// instead of a clean close. Pushed events (`surface.subscribe`) are written by a second thread
/// through the same socket, a line at a time.
fn serve(stream: TcpStream, tx: &Sender<ControlRequest>, ctx: &egui::Context, token: Option<&str>, t: &Timeouts) {
    let stream = Arc::new(stream);
    let conn = NEXT_CONN.fetch_add(1, Ordering::SeqCst);
    let open = Arc::new(AtomicBool::new(true));
    let (etx, erx) = channel::<Value>();
    let out = LineWriter { out: Arc::clone(&stream), lock: Arc::new(Mutex::new(())), buf: Vec::new() };
    let pump = {
        let (out, open) = (out.clone(), Arc::clone(&open));
        std::thread::spawn(move || pump_events(erx, out, open))
    };
    let sink = EventSink { tx: etx, open: Arc::clone(&open) };
    let surfaces_only = token.is_none();
    serve_stream(&stream, out, token, t, |method, params| {
        if surfaces_only && !method.starts_with("surface.") {
            return Some(Reply::Ready(json!({"ok": false, "error": "this port serves control surfaces only (surface.* methods)"})));
        }
        let pipelined = PIPELINED.contains(&method.as_str());
        let (req, rrx) = ControlRequest::new(method, params);
        let req = req.on_connection(conn, sink.clone());
        let state = Arc::clone(&req.state);
        tx.send(req).ok()?;
        ctx.request_repaint();
        let ctx = ctx.clone();
        let wait: Waiter =
            Box::new(move || await_reply(&rrx, &state, || ctx.cumulative_pass_nr(), || ctx.request_repaint(), NO_FRAMES_AFTER, REPLY_TIMEOUT));
        Some(if pipelined { Reply::Pipelined(wait) } else { Reply::Wait(wait) })
    });
    open.store(false, Ordering::SeqCst);
    drop(sink);
    let _ = pump.join();
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
        let taken = state.load(Ordering::SeqCst) == lcc_ui_egui::control::TAKEN;
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

/// [`serve_lines`] on a socket, then close it so the peer reads every reply. Reads and writes go
/// through the one stream, not a `try_clone`: on Windows a cloned socket handle is inheritable, and
/// a program the app starts (browser, Explorer, an editor) would keep the connection open (#1991).
/// With no `token` (the surfaces-only port) the connection takes an authenticated slot at once.
fn serve_stream(stream: &TcpStream, out: impl Write + Send, token: Option<&str>, t: &Timeouts, handle: impl FnMut(String, Value) -> Option<Reply>) {
    let authed = Cell::new(false);
    let reader = TimedReader { stream, auth_by: t.auth_by, idle: t.idle, authed: &authed };
    // The pending slot is given back on authenticating; an authenticated one is taken then.
    let admit = || {
        let ok = Slots::take(&t.slots.authed, MAX_CONNECTIONS);
        t.slots.pending.fetch_sub(1, Ordering::SeqCst);
        authed.set(true);
        ok
    };
    serve_lines(reader, out, token, admit, handle);
    close_gracefully(stream);
    let counter = if authed.get() { &t.slots.authed } else { &t.slots.pending };
    counter.fetch_sub(1, Ordering::SeqCst);
}

/// Close a connection the server is done with without destroying its last reply. A socket closed
/// while the peer's later lines sit unread is reset (on Windows at once), and a reset can discard
/// the error reply the peer has not read yet, so the client sees "connection reset" instead of
/// why. Send FIN, then read and drop what is left (bounded in bytes and time) before closing.
fn close_gracefully(stream: &TcpStream) {
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let mut sink = [0u8; 8192];
    let mut left = MAX_LINE;
    let mut reader = stream;
    while left > 0 {
        match reader.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(n) => left = left.saturating_sub(n),
        }
    }
}

/// The error of the notice sent when an idle connection is closed. Clients match the reply's
/// `"closed": "idle"` marker; `crates/mcp/src/backend.rs` also matches this text, for apps from
/// before the marker, so keep the two in step.
const IDLE_CLOSE_ERROR: &str = "control connection idle too long; closing the connection";

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
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut if e.to_string() == IDLE_CLOSE_ERROR => Closing::Idle,
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

/// Methods whose requests are pipelined: the next line is read (and handed to the app) without
/// waiting for this one's reply, so a control surface's dial ticks never queue up one per frame in
/// the socket (a knob turned quickly sent ticks faster than the app drew frames, and they drained
/// seconds late, presses included). The app still runs them in order, and their replies go out in
/// order, after the replies to every earlier request.
pub const PIPELINED: &[&str] = &["surface.input"];

/// Requests of one connection handed to the app but not answered yet, at most (the reader waits
/// beyond that).
const PIPELINE_DEPTH: usize = 256;

/// Waits for a request's reply.
type Waiter = Box<dyn FnOnce() -> Value + Send>;

/// How a request is answered.
enum Reply {
    /// At once (the request was refused without reaching the app).
    Ready(Value),
    /// When the app has run it; the next request is read only after this reply went out.
    Wait(Waiter),
    /// When the app has run it; the next request is read at once ([`PIPELINED`]).
    Pipelined(Waiter),
}

/// A reply to write, in order: its request's id (`None`: an error that closes the connection),
/// the reply, and who waits for it to be written.
struct Queued {
    id: Option<Value>,
    reply: Reply,
    written: Option<std::sync::mpsc::SyncSender<bool>>,
}

/// Write the replies `queue` hands over, in order, until it closes or a write fails.
fn write_replies<W: Write>(queue: Receiver<Queued>, mut out: W) {
    for q in queue {
        let mut v = match q.reply {
            Reply::Ready(v) => v,
            Reply::Wait(f) | Reply::Pipelined(f) => f(),
        };
        if let (Some(id), Some(o)) = (q.id, v.as_object_mut()) {
            o.insert("id".into(), id);
        }
        let ok = writeln!(out, "{v}").and_then(|()| out.flush()).is_ok();
        if let Some(w) = q.written {
            let _ = w.send(ok);
        }
        if !ok {
            return;
        }
    }
}

/// Serve requests from `read`, answering on `out`, until the peer closes, sends something that is
/// not a request, fails to authenticate, or `handle` returns `None` (app gone). With a `token` the
/// first request must be `auth` with it and is answered here (without one, `admit` is asked at once); `handle` gets the later ones as
/// `(method, params)` and returns the reply object (the request's `id` is added here).
/// After a correct `auth`, `admit` is asked for an authenticated slot; `false` refuses the connection.
fn serve_lines<R: Read, W: Write + Send>(
    read: R,
    out: W,
    token: Option<&str>,
    mut admit: impl FnMut() -> bool,
    mut handle: impl FnMut(String, Value) -> Option<Reply>,
) {
    std::thread::scope(|scope| {
        let (queue, rx) = std::sync::mpsc::sync_channel::<Queued>(PIPELINE_DEPTH);
        let writer = scope.spawn(move || write_replies(rx, out));
        // hand a reply to the writer; with `wait`, until it is written. Whether to go on.
        let send = |id: Option<Value>, reply: Reply, wait: bool| -> bool {
            let (written, done) = if wait {
                let (t, r) = std::sync::mpsc::sync_channel(1);
                (Some(t), Some(r))
            } else {
                (None, None)
            };
            if queue.send(Queued { id, reply, written }).is_err() {
                return false;
            }
            done.is_none_or(|d| d.recv().unwrap_or(false))
        };
        let mut reader = BufReader::new(read);
        let mut buf = Vec::new();
        let mut authenticated = false;
        if token.is_none() {
            if !admit() {
                send(None, Reply::Ready(json!({"ok": false, "error": "too many control connections"})), true);
                drop(queue);
                let _ = writer.join();
                return;
            }
            authenticated = true;
        }
        let token = token.unwrap_or_default();
        loop {
            let closing = match read_bounded_line(&mut reader, &mut buf, MAX_LINE) {
                Ok(false) => break,
                Err(closing) => closing,
                Ok(true) => match parse_line(&buf) {
                    Line::Blank => continue,
                    Line::Reject(e) => Closing::Error(e),
                    Line::Request { id, method, params } if !authenticated => {
                        let (mut r, mut ok) = control_auth::check_auth_request(&method, &params, token);
                        if ok && !admit() {
                            r = json!({"ok": false, "error": "too many control connections"});
                            ok = false;
                        }
                        if !send(Some(id), Reply::Ready(r), true) || !ok {
                            break;
                        }
                        authenticated = true;
                        continue;
                    }
                    Line::Request { id, method, params } => {
                        let Some(r) = handle(method, params) else { break };
                        let wait = !matches!(r, Reply::Pipelined(_));
                        if !send(Some(id), r, wait) {
                            break;
                        }
                        continue;
                    }
                },
            };
            send(None, Reply::Ready(closing.reply()), true);
            break;
        }
        drop(queue);
        let _ = writer.join();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn timeouts() -> Timeouts {
        Timeouts { slots: Arc::new(Slots::default()), auth_by: Instant::now() + AUTH_DEADLINE, idle: IDLE_TIMEOUT }
    }

    /// Accept connections on a loopback port with the given deadlines; returns the address and slots.
    fn server(auth_deadline: Duration, idle: Duration) -> (std::net::SocketAddr, Arc<Slots>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let slots = Arc::new(Slots::default());
        let s2 = Arc::clone(&slots);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                accept_one(stream, &s2, auth_deadline, idle, |s, t| {
                    serve_stream(&s, &s, Some(TOKEN), t, |_, _| Some(Reply::Ready(json!({"ok": true}))))
                });
            }
        });
        (addr, slots)
    }

    fn authed_request(addr: std::net::SocketAddr) -> String {
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        writeln!(c, "{}", control_auth::auth_request(json!(1), TOKEN)).unwrap();
        writeln!(c, "{{\"id\":2,\"method\":\"x\"}}").unwrap();
        let mut r = BufReader::new(c);
        let (mut a, mut b) = (String::new(), String::new());
        r.read_line(&mut a).unwrap();
        r.read_line(&mut b).unwrap();
        a + &b
    }

    /// Silent connections filling every unauthenticated slot don't lock a real client out for
    /// long: they are closed at the auth deadline, and the client then gets in.
    #[test]
    fn silent_connections_are_closed_at_the_auth_deadline() {
        let (addr, slots) = server(Duration::from_millis(300), IDLE_TIMEOUT);
        let mut silent: Vec<TcpStream> = (0..MAX_PENDING).map(|_| TcpStream::connect(addr).unwrap()).collect();
        let t0 = Instant::now();
        while slots.pending.load(Ordering::SeqCst) < MAX_PENDING && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut text = String::new();
        silent[0].set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        silent[0].read_to_string(&mut text).unwrap();
        assert!(text.contains("no `auth` request in time"), "{text}");
        let t1 = Instant::now();
        while slots.pending.load(Ordering::SeqCst) >= MAX_PENDING && t1.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let r = authed_request(addr);
        assert!(r.contains("\"authenticated\":true") && r.contains("\"id\":2"), "{r}");
    }

    /// Unauthenticated connections never use authenticated slots: with every pending slot taken
    /// by silent peers, an authenticated client already connected keeps working, and the
    /// authenticated count is unaffected.
    #[test]
    fn unauthenticated_connections_do_not_take_authenticated_slots() {
        let (addr, slots) = server(Duration::from_secs(30), IDLE_TIMEOUT);
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        writeln!(c, "{}", control_auth::auth_request(json!(1), TOKEN)).unwrap();
        let mut r = BufReader::new(c.try_clone().unwrap());
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        let _silent: Vec<TcpStream> = (0..MAX_PENDING + 4).map(|_| TcpStream::connect(addr).unwrap()).collect();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(slots.authed.load(Ordering::SeqCst), 1);
        assert!(slots.pending.load(Ordering::SeqCst) <= MAX_PENDING);
        writeln!(c, "{{\"id\":9,\"method\":\"x\"}}").unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        assert!(line.contains("\"id\":9"), "{line}");
    }

    /// An authenticated connection that goes quiet is closed and its slot given back.
    #[test]
    fn idle_authenticated_connection_is_closed() {
        let (addr, slots) = server(Duration::from_secs(30), Duration::from_millis(300));
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        writeln!(c, "{}", control_auth::auth_request(json!(1), TOKEN)).unwrap();
        let mut text = String::new();
        c.read_to_string(&mut text).unwrap();
        let reply: Value = serde_json::from_str(text.lines().nth(1).unwrap_or_default()).unwrap();
        assert_eq!(reply["error"], IDLE_CLOSE_ERROR);
        assert_eq!(reply["closed"], "idle", "clients may resend a request that crossed this notice");
        assert!(reply.get("id").is_none());
        let t0 = Instant::now();
        while slots.authed.load(Ordering::SeqCst) != 0 && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(slots.authed.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn auth_beyond_the_authenticated_cap_is_refused() {
        let input = format!(
            "{}
{{\"id\":2,\"method\":\"x\"}}
",
            control_auth::auth_request(json!(1), TOKEN)
        );
        let mut out = Vec::new();
        let mut ran = false;
        serve_lines(
            input.as_bytes(),
            &mut out,
            Some(TOKEN),
            || false,
            |_, _| {
                ran = true;
                None
            },
        );
        assert!(!ran);
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(text.contains("too many control connections"), "{text}");
    }

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// Serve `input` as it arrives, without authenticating first: replies and the methods run.
    fn run_raw(input: &[u8]) -> (Vec<Value>, Vec<String>) {
        let mut out = Vec::new();
        let mut calls = Vec::new();
        serve_lines(
            input,
            &mut out,
            Some(TOKEN),
            || true,
            |m, _p| {
                calls.push(m);
                Some(Reply::Ready(json!({"ok": true, "result": null})))
            },
        );
        let replies = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).collect();
        (replies, calls)
    }

    /// Serve `input` after a successful `auth`; the `auth` reply is left out.
    fn run(input: &[u8]) -> (Vec<Value>, Vec<String>) {
        let mut all = format!("{}\n", control_auth::auth_request(json!(0), TOKEN)).into_bytes();
        all.extend_from_slice(input);
        let (mut replies, calls) = run_raw(&all);
        assert_eq!(replies.remove(0)["ok"], true, "auth");
        (replies, calls)
    }

    #[test]
    fn nothing_runs_before_the_connection_authenticates() {
        for first in [
            r#"{"id":1,"method":"engine.execute","params":{"command":"folder.move"}}"#.to_string(),
            r#"{"id":1,"method":"auth","params":{}}"#.to_string(),
            control_auth::auth_request(json!(1), &"0".repeat(64)).to_string(),
        ] {
            let (r, calls) = run_raw(format!("{first}\n{{\"id\":2,\"method\":\"app.quit\"}}\n").as_bytes());
            assert!(calls.is_empty(), "nothing may run: {calls:?}");
            assert_eq!(r.len(), 1, "one refusal, then the connection is closed: {r:?}");
            assert_eq!(r[0]["id"], 1);
            assert_eq!(r[0]["ok"], false);
            assert!(!r[0].to_string().contains(TOKEN));
        }
    }

    #[test]
    fn an_authenticated_connection_runs_requests() {
        let input = format!("\n{}\n{{\"id\":2,\"method\":\"ui.inspect\"}}\n", control_auth::auth_request(json!(1), &TOKEN.to_uppercase()));
        let (r, calls) = run_raw(input.as_bytes());
        assert_eq!(calls, ["ui.inspect"]);
        assert_eq!(r[0]["id"], 1);
        assert_eq!(r[0]["result"]["authenticated"], true);
        assert_eq!(r[1]["id"], 2);
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
        for (r, calls) in [run(http), run_raw(http)] {
            assert!(calls.is_empty(), "nothing may run: {calls:?}");
            assert_eq!(r.len(), 1);
            assert_eq!(r[0]["ok"], false);
            assert!(r[0]["error"].as_str().unwrap().contains("closing the connection"));
        }
    }

    #[test]
    fn http_request_with_an_auth_line_in_the_body_is_still_refused() {
        // Even a page that knew the token could only send it after the HTTP request line.
        let http =
            format!("POST / HTTP/1.1\r\nHost: 127.0.0.1:7980\r\n\r\n{}\n{{\"method\":\"app.quit\"}}\n", control_auth::auth_request(json!(1), TOKEN));
        let (r, calls) = run_raw(http.as_bytes());
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
        serve_lines(
            endless,
            &mut out,
            Some(TOKEN),
            || true,
            |_, _| {
                ran = true;
                None
            },
        );
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

    /// A second instance on a port that is already served neither publishes its token (which
    /// would lock clients out of the running instance) nor starts.
    #[test]
    fn a_taken_port_publishes_no_token() {
        let taken = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = taken.local_addr().unwrap().port();
        let mut published = None;
        let rx = start(port, TOKEN.into(), channel().0, egui::Context::default(), |t| published = Some(t.to_string()));
        assert!(!rx);
        assert_eq!(published, None, "the token must not be published when the bind failed");
    }

    #[test]
    fn a_free_port_publishes_the_token_once_bound() {
        let port = TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port();
        let mut published = None;
        let rx = start(port, TOKEN.into(), channel().0, egui::Context::default(), |t| {
            // the port is already ours when the token is stored
            assert!(TcpListener::bind(("127.0.0.1", port)).is_err());
            published = Some(t.to_string());
        });
        assert!(rx);
        assert_eq!(published.as_deref(), Some(TOKEN));
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
            serve(s, &tx, &egui::Context::default(), Some(TOKEN), &timeouts());
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
            serve_stream(&s, &s, Some(TOKEN), &timeouts(), |_, _| Some(Reply::Ready(json!({"ok": true}))));
        });
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        writeln!(c, "{}", control_auth::auth_request(json!(6), TOKEN)).unwrap();
        c.write_all(b"{\"id\":7,\"method\":\"x\"}\nhello\n{\"method\":\"y\"}\n").unwrap();
        let mut text = String::new();
        // Returns at EOF: the server closed the connection, gracefully, so every reply arrives even
        // though `y` was never read (a plain close resets the connection on Windows).
        c.read_to_string(&mut text).unwrap();
        server.join().unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(lines[0].contains("\"authenticated\":true"));
        assert!(lines[1].contains("\"id\":7"));
        assert!(lines[2].contains("closing the connection"));
    }

    /// A pushed event and a reply never share a line, and events stop when the peer goes.
    #[test]
    fn pushed_events_and_replies_arrive_as_whole_lines() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        let open = Arc::new(AtomicBool::new(true));
        let (etx, erx) = channel::<Value>();
        let out = LineWriter { out: Arc::new(server), lock: Arc::new(Mutex::new(())), buf: Vec::new() };
        let pump = {
            let (out, open) = (out.clone(), Arc::clone(&open));
            std::thread::spawn(move || pump_events(erx, out, open))
        };
        let mut reply = out.clone();
        for i in 0..50 {
            etx.send(json!({"event": "surface.layout", "n": i})).unwrap();
            writeln!(reply, "{}", json!({"id": i, "ok": true})).unwrap();
            reply.flush().unwrap();
        }
        client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut r = BufReader::new(client);
        let (mut events, mut replies) = (0, 0);
        let mut line = String::new();
        while events + replies < 100 {
            line.clear();
            r.read_line(&mut line).unwrap();
            let v: Value = serde_json::from_str(line.trim()).unwrap();
            if v.get("event").is_some() { events += 1 } else { replies += 1 }
        }
        assert_eq!((events, replies), (50, 50));
        open.store(false, Ordering::SeqCst);
        drop(etx);
        pump.join().unwrap();
    }

    /// A fake control surface drives a headless app over a real loopback socket: it is
    /// refused until paired, pairs, subscribes, turns the dial and hears the new value pushed
    /// back; the surfaces-only port refuses everything else. Then a burst of dial ticks from a
    /// second client is applied within a few frames, without a backlog.
    #[test]
    fn a_surface_client_pairs_and_turns_the_dial_and_a_burst_of_ticks_has_no_backlog() {
        use std::time::Instant;

        use lcc_ui_egui::headless::Headless;
        use lcc_ui_egui::surface::SurfaceStore;
        use lcc_ui_egui::{LccApp, Services};

        struct Store(Mutex<Option<String>>);
        impl SurfaceStore for Store {
            fn load(&self) -> Result<Option<String>, String> {
                Ok(self.0.lock().unwrap().clone())
            }
            fn save(&self, t: &str) -> Result<(), String> {
                *self.0.lock().unwrap() = Some(t.to_string());
                Ok(())
            }
        }
        // a 1:1 dial feel and no per-value tuning: 25 ticks are 25 steps of 0.01 EV (the shipped
        // feel is slower); this test checks the round trip, not the shipped speed
        let motion = lcc_surface::settings::MotionSettings { speed: 1.0, acceleration: 1.0, ..Default::default() };
        let settings = lcc_surface::SurfaceSettings { enabled: true, motion, tuning: Default::default(), ..Default::default() };
        let services = Services { png: None, surface_store: Some(Box::new(Store(Mutex::new(Some(settings.to_json()))))), ..Default::default() };
        let mut h = Headless::new(LccApp::new(lcc_engine::Session::with_demo(), services), [1200.0, 800.0], 1.0);
        h.settle(Duration::from_secs(20));
        let first = h.app.session.visible_cloned()[0];
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [first.0]}}), Duration::from_secs(20));
        h.app.ui.view = lcc_ui_egui::state::ViewMode::Detail;
        h.settle(Duration::from_secs(20));
        let before = h.app.session.develop_of(first).unwrap().light.exposure;
        let (stop, port) = start_surfaces(0, h.control_sender(), egui::Context::default()).unwrap();

        // the client, on its own thread (the app answers between frames on this one)
        let client = std::thread::spawn(move || {
            let send = |c: &mut TcpStream, id: u64, method: &str, params: Value| {
                writeln!(c, "{}", json!({"id": id, "method": method, "params": params})).unwrap();
            };
            // read lines until the reply with `id`; events seen on the way are kept
            let read = |r: &mut BufReader<TcpStream>, id: u64, events: &mut Vec<Value>| -> Value {
                loop {
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    let v: Value = serde_json::from_str(line.trim()).unwrap();
                    if v.get("event").is_some() {
                        events.push(v);
                    } else if v["id"] == id {
                        return v;
                    }
                }
            };
            let connect = || {
                let c = TcpStream::connect(("127.0.0.1", port)).unwrap();
                c.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
                let r = BufReader::new(c.try_clone().unwrap());
                (c, r)
            };
            let mut events = Vec::new();
            let (mut c, mut r) = connect();
            send(&mut c, 1, "engine.execute", json!({"command": "edit.undo"}));
            let v = read(&mut r, 1, &mut events);
            assert!(v["error"].as_str().unwrap().contains("control surfaces only"), "{v}");
            send(&mut c, 2, "surface.input", json!({"events": [{"control": "dial", "ticks": 1}]}));
            assert_eq!(read(&mut r, 2, &mut events)["ok"], false, "not paired");
            send(&mut c, 3, "surface.pair", json!({"client": "e2e", "name": "E2E Surface"}));
            let v = read(&mut r, 3, &mut events);
            let request = v["result"]["request"].as_str().unwrap().to_string();
            // wait for the approval (the test approves on the app's thread)
            let token = loop {
                send(&mut c, 4, "surface.pair", json!({"request": request}));
                let v = read(&mut r, 4, &mut events);
                if v["result"]["status"] == "approved" {
                    break v["result"]["token"].as_str().unwrap().to_string();
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            drop(c);
            let (mut c, mut r) = connect();
            send(&mut c, 5, "surface.hello", json!({"protocol": 1, "client": "e2e", "token": token}));
            assert_eq!(read(&mut r, 5, &mut events)["result"]["paired"], true);
            send(&mut c, 6, "surface.subscribe", json!({"device": "mx-creative-console"}));
            let v = read(&mut r, 6, &mut events);
            assert_eq!(v["result"]["layout"]["dials"][1]["binding"]["target"], "light.exposure", "{v}");
            send(&mut c, 7, "surface.input", json!({"events": [{"control": "roller", "ticks": 25}]}));
            assert_eq!(read(&mut r, 7, &mut events)["ok"], true);
            // the new value arrives as a pushed layout
            let t0 = Instant::now();
            loop {
                if events.iter().any(|e| e["layout"]["dials"][1]["value"] == "+0.25") {
                    break;
                }
                assert!(t0.elapsed() < Duration::from_secs(30), "no pushed value: {events:?}");
                let mut line = String::new();
                if r.read_line(&mut line).is_ok() && !line.trim().is_empty() {
                    events.push(serde_json::from_str(line.trim()).unwrap());
                }
            }
        });
        let t0 = Instant::now();
        while !client.is_finished() {
            h.step();
            let waiting: Vec<String> = h.app.surface.pairing.waiting().map(|w| w.request.clone()).collect();
            for w in waiting {
                let mut s = h.app.surface.settings().clone();
                h.app.surface.pairing.approve(&w, &mut s, 0).unwrap();
                h.app.surface.runtime.settings_mut().paired = s.paired;
            }
            std::thread::sleep(Duration::from_millis(2));
            assert!(t0.elapsed() < Duration::from_secs(60), "the client didn't finish");
        }
        client.join().unwrap();
        let after = h.app.session.develop_of(first).unwrap().light.exposure;
        assert!((after - before - 0.25).abs() < 1e-6, "{before} → {after}");

        // TEST-NOTES 2026-10-16 item 8, on the same app (starting one is most of this test's
        // time): a knob turned fast sends a tick per request, faster than the app draws frames.
        // Ticks used to wait for a frame each (the connection read its next request only after
        // the reply), so they queued in the socket and were applied seconds late, a press behind
        // them "clicking itself" later. Now hundreds of ticks sent at once, with slow frames
        // (40 ms), are all applied within a few frames, the value is their sum, and a press sent
        // after them runs in the same breath.
        const TOKEN: &str = "stress-token";
        {
            let settings = h.app.surface.runtime.settings_mut();
            settings.paired.push(lcc_surface::settings::PairedClient {
                client: "stress".into(),
                name: "Stress".into(),
                token_sha256: lcc_surface::protocol::token_hash(TOKEN),
                approved_at: 0,
            });
            // no acceleration: every tick is one step (0.01 EV), so the sum is exact
            settings.motion.acceleration = 0.1;
        }
        let before = after;
        let rating_before = h.app.session.catalog.photo(first).unwrap().rating;
        const TICKS: usize = 300;
        let frames = Arc::new(AtomicU64::new(0));
        let client = {
            let frames = Arc::clone(&frames);
            std::thread::spawn(move || {
                let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
                c.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut reply = |id: u64| loop {
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    let v: Value = serde_json::from_str(line.trim()).unwrap();
                    if v.get("event").is_none() {
                        assert_eq!(v["id"], id, "replies come in order: {v}");
                        return v;
                    }
                };
                writeln!(c, "{}", json!({"id": 1, "method": "surface.hello", "params": {"protocol": 1, "client": "stress", "token": TOKEN}}))
                    .unwrap();
                assert_eq!(reply(1)["result"]["paired"], true);
                // every tick its own request, all at once (as a plugin forwards a fast spin), then a key press
                let t0 = Instant::now();
                let f0 = frames.load(Ordering::SeqCst);
                let mut burst = String::new();
                for i in 0..TICKS {
                    burst.push_str(
                        &json!({"id": 10 + i, "method": "surface.input", "params": {"events": [{"control": "roller", "ticks": 1}]}}).to_string(),
                    );
                    burst.push('\n');
                }
                let press =
                    json!({"id": 9999, "method": "surface.input", "params": {"events": [{"action": "c.photo.rate.rating-5", "press": true}]}});
                burst.push_str(&format!("{press}\n"));
                c.write_all(burst.as_bytes()).unwrap();
                for i in 0..TICKS {
                    assert_eq!(reply(10 + i as u64)["ok"], true);
                }
                assert_eq!(reply(9999)["ok"], true);
                (t0.elapsed(), frames.load(Ordering::SeqCst) - f0)
            })
        };
        let t0 = Instant::now();
        while !client.is_finished() {
            h.step();
            frames.fetch_add(1, Ordering::SeqCst);
            // a slow frame (a busy app): one tick per frame would take 12 s
            std::thread::sleep(Duration::from_millis(40));
            assert!(t0.elapsed() < Duration::from_secs(60), "the client didn't finish");
        }
        let (took, used) = client.join().unwrap();
        // the last flush and gesture end
        for _ in 0..40 {
            h.step();
            std::thread::sleep(Duration::from_millis(30));
        }
        let after = h.app.session.develop_of(first).unwrap().light.exposure;
        assert!((after - before - 0.01 * TICKS as f64).abs() < 1e-6, "{before} → {after}: every tick, once");
        assert!(used <= 10, "{TICKS} ticks took {used} frames ({took:?}): they queued up");
        let rating = h.app.session.catalog.photo(first).unwrap().rating;
        assert!(rating == 5 && rating_before != 5, "the key press ran once the ticks were in: {rating_before} → {rating}");
        stop.store(true, Ordering::SeqCst);
    }
}
