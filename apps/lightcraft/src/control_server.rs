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
    for mut stream in listener.incoming().flatten() {
        if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            open.fetch_sub(1, Ordering::SeqCst);
            let _ = writeln!(stream, "{}", json!({"ok": false, "error": "too many control connections"}));
            continue;
        }
        let tx = tx.clone();
        let ctx = ctx.clone();
        let open = Arc::clone(&open);
        std::thread::spawn(move || {
            serve(stream, &tx, &ctx);
            open.fetch_sub(1, Ordering::SeqCst);
        });
    }
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

/// Reads and writes go through the one `stream` (`&TcpStream` is `Read` and `Write`): no
/// `try_clone`. On Windows a cloned socket handle is inheritable, so every program the app starts
/// while a client is connected (a link in the browser, Show in Explorer, an external editor) kept a
/// copy of the connection: closing it then reached the client only when that program exited, as a
/// reset instead of a clean close.
fn serve(stream: TcpStream, tx: &Sender<ControlRequest>, ctx: &egui::Context) {
    serve_lines(&stream, &stream, |method, params| {
        let (req, rrx) = ControlRequest::new(method, params);
        let state = Arc::clone(&req.state);
        tx.send(req).ok()?;
        ctx.request_repaint();
        Some(await_reply(&rrx, &state, || ctx.cumulative_pass_nr(), || ctx.request_repaint(), NO_FRAMES_AFTER, REPLY_TIMEOUT))
    });
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

/// Read one `\n`-terminated line of at most `max` bytes into `buf` (newline stripped).
/// `Ok(false)` at end of stream; `Err` when the line is too long or the read fails.
fn read_bounded_line(reader: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> Result<bool, String> {
    buf.clear();
    // `take` bounds the bytes buffered for one line: a peer can't make us allocate more than `max`.
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    let n = reader.take(limit).read_until(b'\n', buf).map_err(|e| e.to_string())?;
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
        return Err(format!("request line longer than {max} bytes; closing the connection"));
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
        let reject = match read_bounded_line(&mut reader, &mut buf, MAX_LINE) {
            Ok(false) => return,
            Err(e) => e,
            Ok(true) => match parse_line(&buf) {
                Line::Blank => continue,
                Line::Reject(e) => e,
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
        let _ = writeln!(out, "{}", json!({"ok": false, "error": reject})).and_then(|()| out.flush());
        return;
    }
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
        assert!(t0.elapsed() < Duration::from_secs(2));
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
            serve(s, &tx, &egui::Context::default());
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
            let read = s.try_clone().unwrap();
            serve_lines(read, s, |_, _| Some(json!({"ok": true})));
        });
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        c.write_all(b"{\"id\":7,\"method\":\"x\"}\nhello\n{\"method\":\"y\"}\n").unwrap();
        let mut text = String::new();
        c.read_to_string(&mut text).unwrap(); // returns at EOF: the server closed the connection
        server.join().unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines[0].contains("\"id\":7"));
        assert!(lines[1].contains("closing the connection"));
    }
}
