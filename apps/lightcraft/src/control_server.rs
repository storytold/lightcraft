//! Loopback JSON-lines control server: one request per line, one reply per line.
//! This is the transport the MCP server (`lightcraft-cli mcp --connect`) wraps.
//!
//! The server only ever reads requests: a line that is not a JSON object with a string `method`
//! (an HTTP request line from a browser's cross-origin `fetch`, a stray `nc`, binary junk) gets
//! one error reply and the connection is closed, so nothing after it is executed. Lines are
//! bounded ([`MAX_LINE`]) and so is the number of open connections ([`MAX_CONNECTIONS`]); junk
//! never reaches the UI thread. See `docs/control-protocol.md`.
//!
//! A connection that subscribes to a control surface's layout (`surface.subscribe`,
//! `docs/control-surfaces.md`) also receives pushed event lines (`{"event": ...}`) between its
//! replies; lines never interleave. The surfaces-only listener ([`Mode::SurfacesOnly`], Settings,
//! Controls) serves nothing but `surface.*` methods, which need pairing.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lightcraft_ui_egui::ControlRequest;
use lightcraft_ui_egui::surface::EventSink;
use serde_json::{Value, json};

/// Longest accepted request line (bytes, without the newline). Requests are small JSON objects;
/// anything longer is refused and the connection closed.
pub const MAX_LINE: usize = 4 * 1024 * 1024;
/// Connections served at once; further ones get an error line and are closed.
pub const MAX_CONNECTIONS: usize = 16;

/// What a listener serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Every method (`--control PORT`).
    Full,
    /// Only `surface.*` (the control-surface port of Settings, Controls).
    SurfacesOnly,
}

/// Connection numbers (0 is the in-process channel).
static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

/// Serve `--control PORT`: requests go to `tx`.
pub fn start(port: u16, tx: Sender<ControlRequest>, ctx: egui::Context) {
    match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => {
            log::info!("control server listening on 127.0.0.1:{port}");
            let stop = Arc::new(AtomicBool::new(false));
            std::thread::spawn(move || accept_loop(l, tx, ctx, Mode::Full, stop));
        }
        Err(e) => log::error!("control server failed to bind 127.0.0.1:{port}: {e}"),
    }
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
    std::thread::spawn(move || accept_loop(l, tx, ctx, Mode::SurfacesOnly, flag));
    Ok((stop, port))
}

fn accept_loop(listener: TcpListener, tx: Sender<ControlRequest>, ctx: egui::Context, mode: Mode, stop: Arc<AtomicBool>) {
    let open = Arc::new(AtomicUsize::new(0));
    // a surface connection (a paired device) may sit listening for pushed layouts for hours
    let idle = if mode == Mode::SurfacesOnly { None } else { Some(IDLE_TIMEOUT) };
    loop {
        if stop.load(Ordering::SeqCst) {
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
        let _ = spawn_connection(stream, tx.clone(), ctx.clone(), Arc::clone(&open), mode, idle);
    }
}

/// Admit and serve one connection, releasing its slot when serving ends.
fn spawn_connection(
    mut stream: TcpStream,
    tx: Sender<ControlRequest>,
    ctx: egui::Context,
    open: Arc<AtomicUsize>,
    mode: Mode,
    idle_timeout: Option<Duration>,
) -> Option<std::thread::JoinHandle<()>> {
    if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
        open.fetch_sub(1, Ordering::SeqCst);
        let _ = writeln!(stream, "{}", json!({"ok": false, "error": "too many control connections"}));
        return None;
    }
    Some(std::thread::spawn(move || {
        serve(stream, &tx, &ctx, mode, idle_timeout);
        open.fetch_sub(1, Ordering::SeqCst);
    }))
}

/// A writer shared by a connection's replies and its pushed events: each line goes out whole.
/// It writes through `&TcpStream` (no `try_clone`: see [`serve`]); `lock` keeps lines whole.
#[derive(Clone)]
struct LineWriter {
    out: Arc<TcpStream>,
    lock: Arc<Mutex<()>>,
    buf: Vec<u8>,
}

impl LineWriter {
    fn new(out: Arc<TcpStream>) -> Self {
        LineWriter { out, lock: Arc::new(Mutex::new(())), buf: Vec::new() }
    }
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
                    // (a client that stopped reading: the write timed out. Closing the socket
                    // also wakes the connection's reader, so its slot is freed)
                    open.store(false, Ordering::SeqCst);
                    let _ = out.out.shutdown(Shutdown::Both);
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) if open.load(Ordering::SeqCst) => {}
            Err(_) => return,
        }
    }
}

/// Five minutes allows pauses between automation commands without retaining abandoned slots forever.
const IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How long a request waits for its reply.
const REPLY_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a write to a client may block: one that stopped reading loses its connection.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
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
///
/// `idle_timeout`: how long the connection may send nothing before it is closed (`None`: never).
fn serve(stream: TcpStream, tx: &Sender<ControlRequest>, ctx: &egui::Context, mode: Mode, idle_timeout: Option<Duration>) {
    if let Err(e) = stream.set_read_timeout(idle_timeout) {
        log::warn!("control connection closed: could not set idle read timeout: {e}");
        return;
    }
    if let Err(e) = stream.set_write_timeout(Some(WRITE_TIMEOUT)) {
        log::warn!("control connection closed: could not set write timeout: {e}");
        return;
    }
    let stream = Arc::new(stream);
    let conn = NEXT_CONN.fetch_add(1, Ordering::SeqCst);
    let open = Arc::new(AtomicBool::new(true));
    let (etx, erx) = channel::<Value>();
    let out = LineWriter::new(Arc::clone(&stream));
    let pump = {
        let (out, open) = (out.clone(), Arc::clone(&open));
        std::thread::spawn(move || pump_events(erx, out, open))
    };
    let sink = EventSink { tx: etx, open: Arc::clone(&open) };
    serve_lines(&*stream, out, |method, params| {
        if mode == Mode::SurfacesOnly && !method.starts_with("surface.") {
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
    close_gracefully(&stream);
}

/// The longest a closing connection is drained, however slowly its peer keeps sending.
const DRAIN_TIME: Duration = Duration::from_secs(1);

/// [`serve_lines`] on a socket, then close it so the peer reads every reply.
#[cfg(test)]
fn serve_stream(stream: TcpStream, handle: impl FnMut(String, Value) -> Option<Reply>) {
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
/// not a request, or `handle` returns `None` (app gone). `handle` gets `(method, params)` and
/// says how the request is answered (the request's `id` is added here). Replies go out in the
/// order of the requests; a [`Reply::Pipelined`] one doesn't hold back reading the next line.
fn serve_lines<R: Read, W: Write + Send>(read: R, out: W, mut handle: impl FnMut(String, Value) -> Option<Reply>) {
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
        loop {
            let closing = match read_bounded_line(&mut reader, &mut buf, MAX_LINE) {
                Ok(false) => break,
                Err(closing) => closing,
                Ok(true) => match parse_line(&buf) {
                    Line::Blank => continue,
                    Line::Reject(e) => Closing::Error(e),
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
            let worker = spawn_connection(stream, tx, egui::Context::default(), Arc::clone(open), Mode::Full, Some(idle)).unwrap();
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

    /// A pushed event and a reply never share a line, and events stop when the peer goes.
    /// A subscriber that stops reading: the event writer gives up when a write times out, and
    /// closes the connection, so the slot comes free.
    #[test]
    fn a_client_that_stops_reading_loses_its_connection() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        server.set_write_timeout(Some(Duration::from_millis(200))).unwrap();
        let open = Arc::new(AtomicBool::new(true));
        let (etx, erx) = channel::<Value>();
        let out = LineWriter::new(Arc::new(server));
        let pump = {
            let open = Arc::clone(&open);
            std::thread::spawn(move || pump_events(erx, out, open))
        };
        // far more than the socket buffers hold, never read
        let big = "x".repeat(64 * 1024);
        for i in 0..1000 {
            if etx.send(json!({"event": "surface.layout", "n": i, "pad": big})).is_err() {
                break;
            }
        }
        let t = std::time::Instant::now();
        while !pump.is_finished() {
            assert!(t.elapsed() < Duration::from_secs(20), "the writer never gave up");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!open.load(Ordering::SeqCst));
        drop(client);
    }

    #[test]
    fn pushed_events_and_replies_arrive_as_whole_lines() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        let open = Arc::new(AtomicBool::new(true));
        let (etx, erx) = channel::<Value>();
        let out = LineWriter::new(Arc::new(server));
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

    /// A fake control surface drives a headless LightCraft over a real loopback socket: it is
    /// refused until paired, pairs, subscribes, turns the dial and hears the new value pushed
    /// back; the surfaces-only port refuses everything else.
    #[test]
    fn a_surface_client_pairs_subscribes_and_turns_the_dial_end_to_end() {
        use std::time::Instant;

        use lightcraft_ui_egui::headless::Headless;
        use lightcraft_ui_egui::surface::SurfaceStore;
        use lightcraft_ui_egui::{LightcraftApp, Services};

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
        let settings = lightcraft_surface::SurfaceSettings { enabled: true, ..Default::default() };
        let services = Services { png: None, surface_store: Some(Box::new(Store(Mutex::new(Some(settings.to_json()))))), ..Default::default() };
        let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1200.0, 800.0], 1.0);
        h.settle(Duration::from_secs(20));
        let first = h.app.session.visible_cloned()[0];
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [first.0]}}), Duration::from_secs(20));
        h.app.ui.view = lightcraft_ui_egui::state::ViewMode::Detail;
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
        stop.store(true, Ordering::SeqCst);
    }

    /// A knob turned fast sends a tick per request, faster than the app draws frames. Ticks used to
    /// wait for a frame each (the connection read its next request only after the reply), so they
    /// queued in the socket and were applied seconds late, a press behind them "clicking itself"
    /// later. Now hundreds of ticks sent at once, with slow frames (40 ms), are all applied within a
    /// few frames, the value is their sum, and a press sent after them runs in the same breath.
    #[test]
    fn a_burst_of_dial_ticks_is_applied_within_a_few_frames_without_a_backlog() {
        use std::time::Instant;

        use lightcraft_ui_egui::headless::Headless;
        use lightcraft_ui_egui::{LightcraftApp, Services};

        struct Store(Mutex<Option<String>>);
        impl lightcraft_ui_egui::surface::SurfaceStore for Store {
            fn load(&self) -> Result<Option<String>, String> {
                Ok(self.0.lock().unwrap().clone())
            }
            fn save(&self, t: &str) -> Result<(), String> {
                *self.0.lock().unwrap() = Some(t.to_string());
                Ok(())
            }
        }
        const TOKEN: &str = "stress-token";
        let mut settings = lightcraft_surface::SurfaceSettings { enabled: true, ..Default::default() };
        settings.paired.push(lightcraft_surface::settings::PairedClient {
            client: "stress".into(),
            name: "Stress".into(),
            token_sha256: lightcraft_surface::protocol::token_hash(TOKEN),
            approved_at: 0,
        });
        // no acceleration: every tick is one step (0.01 EV), so the sum is exact
        settings.motion.acceleration = 0.1;
        let services = Services { png: None, surface_store: Some(Box::new(Store(Mutex::new(Some(settings.to_json()))))), ..Default::default() };
        let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1200.0, 800.0], 1.0);
        h.settle(Duration::from_secs(20));
        let first = h.app.session.visible_cloned()[0];
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [first.0]}}), Duration::from_secs(20));
        h.app.ui.view = lightcraft_ui_egui::state::ViewMode::Detail;
        h.settle(Duration::from_secs(20));
        let before = h.app.session.develop_of(first).unwrap().light.exposure;
        let rating_before = h.app.session.catalog.photo(first).unwrap().rating;
        let (stop, port) = start_surfaces(0, h.control_sender(), egui::Context::default()).unwrap();
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

    fn run(input: &[u8]) -> (Vec<Value>, Vec<String>) {
        let mut out = Vec::new();
        let mut calls = Vec::new();
        serve_lines(input, &mut out, |m, _p| {
            calls.push(m);
            Some(Reply::Ready(json!({"ok": true, "result": null})))
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
        let (tx, _rx) = channel::<ControlRequest>();
        let server = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            serve(s, &tx, &egui::Context::default(), Mode::Full, Some(IDLE_TIMEOUT));
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
            serve_stream(s, |_, _| Some(Reply::Ready(json!({"ok": true}))));
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
