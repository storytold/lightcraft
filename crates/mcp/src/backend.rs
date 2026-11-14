//! Where MCP tool calls end up: a control-channel method call.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde_json::{Value, json};

/// Reply payloads include base64-encoded previews and screenshots, unlike the small requests.
const MAX_REPLY: usize = 64 * 1024 * 1024;

/// Something that answers control-channel methods (`engine.execute`, `engine.commands`,
/// `ui.render`, `ui.screenshot`, `ui.key`, …). See `docs/control-protocol.md`.
pub trait Backend {
    /// Call one method. `Ok` carries the `result`, `Err` the error message.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String>;
    /// True when a real UI is attached (`ui.inspect`, `ui.clickWidget`, `ui.pointer` … work).
    fn has_ui(&self) -> bool;
    /// Short human description ("headless", "remote 127.0.0.1:7980").
    fn describe(&self) -> String;
    /// Progress/cancellation for the next headless batch export. Other backends ignore it.
    fn set_progress(&mut self, _hook: Option<ProgressHook>) {}
}

/// `(completed photos, total photos, next filename)`; false stops before the next photo.
pub type ProgressHook = Box<dyn FnMut(usize, usize, &str) -> bool + Send>;

/// A running LightCraft app, reached through its loopback control port.
pub struct Remote {
    addr: String,
    conn: Option<(BufReader<TcpStream>, TcpStream)>,
    next_id: u64,
    /// Sent as the first line of every connection when the app requires it
    /// (`LIGHTCRAFT_CONTROL_TOKEN`, else `<home>/.lightcraft/control-<port>.token`).
    token: Option<String>,
    /// Whether `token` was given by the caller or found, and so is looked up again on each connect.
    token_source: TokenSource,
    /// How a discovered token is found; `control_token_for` (a test replaces it).
    token_lookup: fn(&str) -> Option<String>,
}

/// Where a [`Remote`]'s token came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TokenSource {
    /// Set with [`Remote::with_token`] (an explicit `None` too): never looked up again.
    Explicit,
    /// Read from the environment or the token file. The app writes a new token each time it starts,
    /// so it is read again on every connect, or a restarted app could never be reached again.
    Discovered,
}

impl Remote {
    /// Connect to `addr` (e.g. `127.0.0.1:7980`), failing fast when nothing is listening.
    pub fn connect(addr: &str) -> std::io::Result<Self> {
        let mut r = Self::lazy(addr);
        r.reconnect()?;
        Ok(r)
    }

    /// A remote that connects on first use (the app may start after the MCP server).
    pub fn lazy(addr: &str) -> Self {
        Self {
            addr: addr.to_string(),
            conn: None,
            next_id: 1,
            token: control_token_for(addr),
            token_source: TokenSource::Discovered,
            token_lookup: control_token_for,
        }
    }

    /// Use this token (or none) instead of the environment / token file, now and on every reconnect.
    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token;
        self.token_source = TokenSource::Explicit;
        self
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Open a new connection (dropping any old one) and authenticate it. A discovered token is read
    /// again first: the app may have restarted with a new one.
    fn reconnect(&mut self) -> std::io::Result<()> {
        self.conn = None;
        if self.token_source == TokenSource::Discovered {
            self.token = (self.token_lookup)(&self.addr);
        }
        let mut last = std::io::Error::new(std::io::ErrorKind::NotFound, format!("cannot resolve {}", self.addr));
        for sa in self.addr.to_socket_addrs()? {
            // The token is a secret for the local app: never hand it to another host.
            if self.token.is_some() && !sa.ip().is_loopback() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("a control token is configured, so --connect must point at a loopback address (127.0.0.1 / ::1); got {sa}"),
                ));
            }
            match TcpStream::connect_timeout(&sa, Duration::from_millis(800)) {
                Ok(s) => {
                    s.set_nodelay(true).ok();
                    // The app answers within 60 s (its own timeout); leave headroom.
                    s.set_read_timeout(Some(Duration::from_secs(90))).ok();
                    let read = s.try_clone()?;
                    self.conn = Some((BufReader::new(read), s));
                    return self.authenticate();
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// First line of a new connection when a token is configured; the app answers
    /// `{"id":0,"ok":true,"result":"authenticated"}` or closes the connection. Any other success reply
    /// is `InvalidData`, and an app that does not know `auth` (started without a token, or older) is
    /// an error, never a silent downgrade.
    fn authenticate(&mut self) -> std::io::Result<()> {
        let Some(token) = self.token.clone() else { return Ok(()) };
        let Some((reader, writer)) = self.conn.as_mut() else { return Ok(()) };
        let line = json!({"id": 0, "method": "auth", "params": {"token": token}}).to_string();
        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        let reply = read_reply(reader, MAX_REPLY)?;
        let v: Value =
            serde_json::from_str(&reply).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad auth reply: {e}")))?;
        if v.get("ok").and_then(Value::as_bool) == Some(true) {
            if v.get("id").and_then(Value::as_u64) == Some(0) && v.get("result").and_then(Value::as_str) == Some("authenticated") {
                return Ok(());
            }
            self.conn = None;
            Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("unexpected reply to `auth` from the app at {}: {reply}", self.addr)))
        } else {
            self.conn = None;
            let error = v.get("error").and_then(Value::as_str).unwrap_or("?");
            // `control.rs` words it "unknown method `auth`".
            let lower = error.to_lowercase();
            if lower.contains("unknown method") || lower.contains("method not found") {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "the app at {} does not require a control token (it answered: {error}). To connect anyway, start lightcraft-cli with \
                         LIGHTCRAFT_CONTROL_NO_AUTH=1, or restart the app without LIGHTCRAFT_CONTROL_NO_AUTH so it requires the token.",
                        self.addr
                    ),
                ));
            }
            Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("control token rejected by the app at {}: {error}", self.addr)))
        }
    }
}

/// `LIGHTCRAFT_CONTROL_TOKEN`, else the token file the app writes for this port, else none
/// (an app started without authentication, or an older app). `LIGHTCRAFT_CONTROL_NO_AUTH=1` means
/// none at all: the same variable that starts the app without a token.
fn control_token_for(addr: &str) -> Option<String> {
    let no_auth = matches!(std::env::var("LIGHTCRAFT_CONTROL_NO_AUTH").as_deref(), Ok("1") | Ok("true"));
    if no_auth {
        return token_for_env(true, None, None);
    }
    let env_token = std::env::var("LIGHTCRAFT_CONTROL_TOKEN").ok();
    let file = addr.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()).and_then(|port| {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
        std::fs::read_to_string(std::path::PathBuf::from(home).join(".lightcraft").join(format!("control-{port}.token"))).ok()
    });
    token_for_env(false, env_token.as_deref(), file.as_deref())
}

/// The token to send: none when authentication is switched off, else the environment's, else the file's.
fn token_for_env(no_auth: bool, env_token: Option<&str>, file: Option<&str>) -> Option<String> {
    if no_auth {
        return None;
    }
    [env_token, file].into_iter().flatten().map(str::trim).find(|t| !t.is_empty()).map(str::to_string)
}

impl Remote {
    /// Send one request line and read the reply line. On failure the flag says whether the request
    /// may have reached the app (written in full); only an unsent request is safe to resend.
    fn roundtrip(&mut self, line: &str) -> Result<String, (std::io::Error, bool)> {
        // A connection the app already closed (it restarted, or closed it after an error reply) is
        // replaced before anything is sent on it, so that case still reconnects without a resend.
        if self.conn.as_ref().is_some_and(|(reader, writer)| !reader.buffer().is_empty() || peer_closed(writer)) {
            self.conn = None;
        }
        if self.conn.is_none() {
            self.reconnect().map_err(|e| (e, false))?;
        }
        let Some((reader, writer)) = self.conn.as_mut() else {
            return Err((std::io::Error::new(std::io::ErrorKind::NotConnected, "not connected"), false));
        };
        let mut data = Vec::with_capacity(line.len() + 1);
        data.extend_from_slice(line.as_bytes());
        data.push(b'\n');
        writer.write_all(&data).map_err(|e| (e, false))?;
        writer.flush().map_err(|e| (e, true))?;
        read_reply(reader, MAX_REPLY).map_err(|e| (e, true))
    }

    fn unknown_outcome(&self, method: &str, e: &std::io::Error) -> String {
        let addr = &self.addr;
        format!("lost the LightCraft app at {addr} after sending `{method}` ({e}); the outcome is unknown and it was not resent, check the app")
    }
}

fn read_reply(reader: &mut impl BufRead, max: usize) -> std::io::Result<String> {
    let mut reply = String::new();
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    reader.take(limit).read_line(&mut reply)?;
    if reply.ends_with('\n') {
        Ok(reply)
    } else if reply.len() > max {
        Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("control reply longer than {max} bytes")))
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "control channel closed before a complete reply line"))
    }
}

fn parse_reply(reply: &str, id: u64) -> std::io::Result<Value> {
    let invalid = |message| std::io::Error::new(std::io::ErrorKind::InvalidData, message);
    let v: Value = serde_json::from_str(reply).map_err(|e| invalid(format!("bad reply from app: {e}")))?;
    let ok = v.get("ok").and_then(Value::as_bool);
    match v.get("id") {
        Some(reply_id) if reply_id.as_u64() == Some(id) => {}
        // The server refuses connections or malformed input before dispatch without an id.
        None if ok == Some(false) => {}
        _ => return Err(invalid(format!("control reply id does not match request {id}"))),
    }
    match ok {
        Some(true) if v.get("result").is_some() => Ok(v),
        Some(false) if v.get("error").and_then(Value::as_str).is_some() => Ok(v),
        _ => Err(invalid("control reply must contain boolean `ok` and a result or string error".to_string())),
    }
}

/// True when the app has closed `stream` (or it is unusable), checked without blocking or consuming data.
fn peer_closed(stream: &TcpStream) -> bool {
    if stream.set_nonblocking(true).is_err() {
        return true;
    }
    let mut byte = [0u8; 1];
    let open = matches!(stream.peek(&mut byte), Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock);
    stream.set_nonblocking(false).is_err() || !open
}

/// The app's idle-close notice (`apps/lightcraft/src/control_server.rs`), matched when a reply lacks
/// the `closed` marker (an app from before it). Keep the two in step.
const IDLE_CLOSE_ERROR: &str = "control connection idle read timed out; closing the connection";

/// True for the app's idle-close notice: an id-less error the app writes when it closes a connection
/// on which it read nothing for its idle timeout. Read in reply to a request, it means the app closed
/// the connection without reading the request (the request crossed the notice), so nothing ran.
fn is_idle_close(reply: &Value) -> bool {
    reply.get("id").is_none()
        && reply.get("ok").and_then(Value::as_bool) == Some(false)
        && (reply.get("closed").and_then(Value::as_str) == Some("idle") || reply.get("error").and_then(Value::as_str) == Some(IDLE_CLOSE_ERROR))
}

impl Backend for Remote {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let line = json!({"id": id, "method": method, "params": params}).to_string();
        // Send again once, on a fresh connection, but only when the request never reached the app: it
        // could not be written (the app restarted), or the app's idle-close notice crossed it (the app
        // stopped reading before it arrived). After a full write anything else may mean the command
        // ran, so resending it could apply a mutation twice (toggle, undo, ...): report the unknown
        // outcome instead. One resend at most, whatever the reason, so this can't loop.
        let mut resent = false;
        let v = loop {
            let reply = match self.roundtrip(&line) {
                Ok(r) => r,
                Err((e, sent)) => {
                    self.conn = None;
                    if sent {
                        return Err(self.unknown_outcome(method, &e));
                    }
                    // The app refused the handshake (token rejected, no token needed, non-loopback
                    // address): a second try fails the same way, and the message says what to do.
                    if e.kind() == std::io::ErrorKind::PermissionDenied {
                        return Err(e.to_string());
                    }
                    if resent {
                        return Err(format!("LightCraft app at {} is not reachable: {e} (start it with `lightcraft --control PORT`)", self.addr));
                    }
                    resent = true;
                    continue;
                }
            };
            let v = parse_reply(&reply, id).map_err(|e| {
                self.conn = None;
                self.unknown_outcome(method, &e)
            })?;
            if v.get("id").is_none() {
                self.conn = None;
                if !resent && is_idle_close(&v) {
                    resent = true;
                    continue;
                }
            }
            break v;
        };
        if v.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(v.get("result").cloned().unwrap_or(Value::Null))
        } else {
            Err(v.get("error").and_then(Value::as_str).unwrap_or("unknown error").to_string())
        }
    }

    fn has_ui(&self) -> bool {
        true
    }

    fn describe(&self) -> String {
        format!("remote {}", self.addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn with_reply(reply: &[u8], check: impl FnOnce(&mut Remote, Result<Value, String>)) {
        let _serial = crate::serial_sockets_and_children();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let reply = reply.to_vec();
        let worker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let request: Value = serde_json::from_str(&request).unwrap();
            assert_eq!(request["id"], 1);
            writer.write_all(&reply).unwrap();
            writer.shutdown(std::net::Shutdown::Write).unwrap();
            let mut extra = String::new();
            assert_eq!(reader.read_line(&mut extra).unwrap(), 0, "request was resent on the same connection");
            listener
        });
        let mut remote = Remote::lazy(&addr);
        let result = remote.call("engine.execute", json!({"command": "edit.undo"}));
        check(&mut remote, result);
        drop(remote);
        let listener = worker.join().unwrap();
        listener.set_nonblocking(true).unwrap();
        assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock, "request was resent on a new connection");
    }

    #[test]
    fn mismatched_reply_id_is_rejected_without_resending() {
        with_reply(b"{\"id\":999,\"ok\":true,\"result\":\"unrelated\"}\n", |remote, result| {
            let err = result.unwrap_err();
            assert!(err.contains("outcome is unknown"), "{err}");
            assert!(remote.conn.is_none(), "discard the untrustworthy connection");
        });
    }

    #[test]
    fn unterminated_reply_is_rejected_without_resending() {
        with_reply(b"{\"id\":1,\"ok\":true,\"result\":7}", |remote, result| {
            let err = result.unwrap_err();
            assert!(err.contains("outcome is unknown"), "{err}");
            assert!(remote.conn.is_none(), "discard the incomplete response");
        });
    }

    #[test]
    fn malformed_replies_are_rejected_without_resending() {
        for reply in [
            "not JSON\n",
            "[]\n",
            "{\"id\":1,\"ok\":\"true\",\"result\":7}\n",
            "{\"id\":1,\"result\":7}\n",
            "{\"id\":1,\"ok\":true}\n",
            "{\"ok\":true,\"result\":7}\n",
            "{\"id\":null,\"ok\":true,\"result\":7}\n",
            "{\"id\":\"1\",\"ok\":true,\"result\":7}\n",
            "{\"id\":1.0,\"ok\":true,\"result\":7}\n",
            "{\"id\":999,\"ok\":false,\"error\":\"unrelated\"}\n",
            "{\"id\":1,\"ok\":false}\n",
            "{\"id\":1,\"ok\":false,\"error\":7}\n",
            "{\"ok\":false}\n",
        ] {
            with_reply(reply.as_bytes(), |remote, result| {
                let err = result.unwrap_err();
                assert!(err.contains("outcome is unknown"), "{reply}: {err}");
                assert!(remote.conn.is_none());
            });
        }
        with_reply(b"\xff\n", |remote, result| {
            assert!(result.unwrap_err().contains("outcome is unknown"));
            assert!(remote.conn.is_none());
        });
    }

    #[test]
    fn valid_replies_and_errors_are_preserved() {
        for (reply, expected) in [
            ("{\"id\":1,\"ok\":true,\"result\":7}\n", Ok(json!(7))),
            ("{\"id\":1,\"ok\":true,\"result\":null}\r\n", Ok(Value::Null)),
            ("{\"id\":1,\"ok\":false,\"error\":\"unknown command\"}\n", Err("unknown command".to_string())),
            ("{\"ok\":false,\"error\":\"too many control connections\"}\n", Err("too many control connections".to_string())),
            // The idle-close text in reply to the request (it has the id) is the command's own error.
            (
                "{\"id\":1,\"ok\":false,\"error\":\"control connection idle read timed out; closing the connection\"}\n",
                Err(IDLE_CLOSE_ERROR.to_string()),
            ),
        ] {
            with_reply(reply.as_bytes(), |remote, result| {
                assert_eq!(result, expected);
                assert_eq!(remote.conn.is_none(), !reply.contains("\"id\""));
            });
        }
    }

    #[test]
    fn reply_line_is_bounded_and_requires_a_newline() {
        let reply = b"{\"id\":1,\"ok\":true,\"result\":7}\n";
        let payload_len = reply.len() - 1;
        assert_eq!(read_reply(&mut reply.as_slice(), payload_len).unwrap().as_bytes(), reply);
        assert_eq!(read_reply(&mut reply.as_slice(), payload_len - 1).unwrap_err().kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(read_reply(&mut &reply[..payload_len], payload_len).unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);
        assert_eq!(read_reply(&mut &b""[..], payload_len).unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);
        let mut oversized = std::io::Cursor::new(vec![b'x'; 100]);
        assert_eq!(read_reply(&mut oversized, 10).unwrap_err().kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(oversized.position(), 11, "read only the bound plus one byte");
    }

    #[test]
    fn preview_sized_replies_use_the_documented_limit() {
        let input = std::io::repeat(b'x').take(MAX_REPLY as u64).chain(std::io::Cursor::new(b"\n"));
        let reply = read_reply(&mut BufReader::new(input), MAX_REPLY).unwrap();
        assert_eq!(reply.len(), MAX_REPLY + 1);
        assert!(reply.ends_with('\n'));
        let mut oversized = BufReader::new(std::io::repeat(b'x'));
        assert_eq!(read_reply(&mut oversized, MAX_REPLY).unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn consecutive_replies_match_their_requests_on_one_connection() {
        let _serial = crate::serial_sockets_and_children();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let worker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            for id in 1..=2 {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["id"], id);
                writeln!(writer, "{}", json!({"id": id, "ok": true, "result": id})).unwrap();
            }
        });
        let mut remote = Remote::lazy(&addr);
        for id in 1..=2 {
            assert_eq!(remote.call("engine.commands", json!({})).unwrap(), json!(id));
        }
        worker.join().unwrap();
    }

    #[test]
    fn lost_reply_is_not_resent() {
        let _serial = crate::serial_sockets_and_children();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let seen = Arc::new(AtomicUsize::new(0));
        let seen2 = seen.clone();
        std::thread::spawn(move || {
            // Every connection: read the request, count it, drop without replying.
            while let Ok((s, _)) = listener.accept() {
                let mut line = String::new();
                if BufReader::new(s).read_line(&mut line).unwrap_or(0) > 0 {
                    seen2.fetch_add(1, Ordering::SeqCst);
                }
            }
        });
        let mut remote = Remote::lazy(&addr);
        let err = remote.call("engine.execute", json!({})).unwrap_err();
        assert!(err.contains("outcome is unknown"), "{err}");
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(seen.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn connection_closed_by_app_reconnects_before_sending() {
        let _serial = crate::serial_sockets_and_children();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let seen = Arc::new(AtomicUsize::new(0));
        let seen2 = seen.clone();
        std::thread::spawn(move || {
            // Every connection: answer one request, then close (as after an app restart).
            while let Ok((s, _)) = listener.accept() {
                let mut writer = s.try_clone().unwrap();
                let mut line = String::new();
                if BufReader::new(s).read_line(&mut line).unwrap_or(0) > 0 {
                    seen2.fetch_add(1, Ordering::SeqCst);
                    let request: Value = serde_json::from_str(&line).unwrap();
                    writeln!(writer, "{}", json!({"id": request["id"], "ok": true, "result": 7})).unwrap();
                }
            }
        });
        let mut remote = Remote::lazy(&addr);
        assert_eq!(remote.call("engine.commands", json!({})).unwrap(), json!(7));
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(remote.call("engine.commands", json!({})).unwrap(), json!(7));
        assert_eq!(seen.load(Ordering::SeqCst), 2);
    }

    /// Accept the next connection within 5 s, as a blocking stream with a 3 s read timeout.
    fn accept(listener: &TcpListener) -> (BufReader<TcpStream>, TcpStream) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("accept: {e}"),
            }
        };
        // BSD kernels (macOS, FreeBSD) let an accepted socket inherit the listener's non-blocking flag.
        stream.set_nonblocking(false).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        (BufReader::new(stream.try_clone().unwrap()), stream)
    }

    fn read_request(reader: &mut BufReader<TcpStream>) -> Value {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    /// The app's idle-close notice, as `apps/lightcraft/src/control_server.rs` writes it.
    fn idle_notice() -> Value {
        json!({"ok": false, "error": IDLE_CLOSE_ERROR, "closed": "idle"})
    }

    /// Issue #3069. The app closes an idle connection with an id-less notice. Either the notice
    /// arrives before the next request (the client sees the close and reconnects before sending),
    /// or the request crosses it (sent just as the app stopped reading): the notice then comes
    /// back as the request's reply although the app never ran it, so the client sends it once more
    /// on a new connection. Both orders are forced here, so the test does not depend on timing.
    #[test]
    fn idle_close_reconnects_without_replaying_a_command() {
        let _serial = crate::serial_sockets_and_children();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (closed, expired) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut ran = Vec::new();
            let mut run = |request: &Value, writer: &mut TcpStream| {
                ran.push(request["id"].as_u64().unwrap());
                writeln!(writer, "{}", json!({"id": request["id"], "ok": true, "result": ran.len()})).unwrap();
            };

            // 1. Idle with nothing sent: the notice (in an older app's form, without `closed`) and
            //    the close reach the client before its next request.
            let (mut reader, mut writer) = accept(&listener);
            run(&read_request(&mut reader), &mut writer);
            writer.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
            let mut line = String::new();
            let error = reader.read_line(&mut line).unwrap_err();
            assert!(matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut));
            assert!(line.is_empty(), "no command is resent while idle");
            writeln!(writer, "{}", json!({"ok": false, "error": IDLE_CLOSE_ERROR})).unwrap();
            drop((reader, writer));
            closed.send(()).unwrap();

            // 2. The client reconnected before sending. Its next request crosses the notice: the
            //    app's read timed out just before the request arrived, so it is not run. (Reading
            //    it here only shows that it was sent on this connection and keeps the close clean.)
            let (mut reader, mut writer) = accept(&listener);
            run(&read_request(&mut reader), &mut writer);
            let crossed = read_request(&mut reader);
            writeln!(writer, "{}", idle_notice()).unwrap();
            drop((reader, writer));

            // 3. The crossed request, sent once more on a new connection, and nothing after it.
            let (mut reader, mut writer) = accept(&listener);
            let resent = read_request(&mut reader);
            assert_eq!(resent, crossed, "the same request is sent again");
            run(&resent, &mut writer);
            line.clear();
            assert_eq!(reader.read_line(&mut line).unwrap(), 0, "nothing else arrives: {line}");
            ran
        });
        let mut remote = Remote::lazy(&addr);
        assert_eq!(remote.call("engine.execute", json!({"command": "edit.undo"})).unwrap(), json!(1));
        expired.recv_timeout(Duration::from_secs(5)).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !remote.conn.as_ref().is_some_and(|(_, writer)| peer_closed(writer)) {
            assert!(std::time::Instant::now() < deadline, "the idle close never reached the client");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(remote.call("engine.execute", json!({"command": "edit.undo"})).unwrap(), json!(2));
        assert_eq!(remote.call("engine.execute", json!({"command": "edit.undo"})).unwrap(), json!(3));
        drop(remote);
        assert_eq!(server.join().unwrap(), [1, 2, 3], "each command ran once");
    }

    /// The resend after an idle close happens once: a second notice is the call's error.
    #[test]
    fn idle_close_is_resent_only_once() {
        let _serial = crate::serial_sockets_and_children();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut reader, mut writer) = accept(&listener);
                assert_eq!(read_request(&mut reader)["id"], 1);
                writeln!(writer, "{}", idle_notice()).unwrap();
            }
            listener
        });
        let mut remote = Remote::lazy(&addr);
        assert_eq!(remote.call("engine.execute", json!({"command": "edit.undo"})), Err(IDLE_CLOSE_ERROR.to_string()));
        assert!(remote.conn.is_none(), "the closed connection is not reused");
        drop(remote);
        let listener = server.join().unwrap();
        assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock, "a third connection was opened");
    }

    #[test]
    fn only_the_idle_close_notice_counts_as_undelivered() {
        let idle = [
            json!({"ok": false, "error": IDLE_CLOSE_ERROR, "closed": "idle"}),
            // an app from before the `closed` marker
            json!({"ok": false, "error": IDLE_CLOSE_ERROR}),
            // the marker decides when present, whatever the wording
            json!({"ok": false, "error": "idle, closing", "closed": "idle"}),
        ];
        for reply in idle {
            assert!(is_idle_close(&reply), "{reply}");
        }
        let delivered = [
            // a reply to the request itself
            json!({"id": 1, "ok": false, "error": IDLE_CLOSE_ERROR}),
            json!({"id": 1, "ok": false, "error": IDLE_CLOSE_ERROR, "closed": "idle"}),
            // other refusals and closes
            json!({"ok": false, "error": "too many control connections"}),
            json!({"ok": false, "error": "bad JSON; closing the connection"}),
            json!({"ok": false, "error": "bad JSON; closing the connection", "closed": "junk"}),
            json!({"ok": false, "error": format!("{IDLE_CLOSE_ERROR}.")}),
            json!({"ok": true, "result": IDLE_CLOSE_ERROR, "closed": "idle"}),
        ];
        for reply in delivered {
            assert!(!is_idle_close(&reply), "{reply}");
        }
    }

    #[test]
    fn unreachable_app_is_reported() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);
        let err = Remote::lazy(&addr).call("engine.commands", json!({})).unwrap_err();
        assert!(err.contains("not reachable"), "{err}");
    }

    #[test]
    fn a_token_is_never_sent_to_a_non_loopback_address() {
        // 192.0.2.1 is TEST-NET-1: never routable, so a refusal here happened before any socket opened.
        let started = std::time::Instant::now();
        let err = Remote::lazy("192.0.2.1:7980").with_token(Some("x".into())).call("engine.commands", json!({})).unwrap_err();
        assert!(err.contains("loopback"), "{err}");
        assert!(started.elapsed() < Duration::from_millis(700), "a connect attempt was made: {err}");
    }

    /// A server that answers the first line of each connection with `replies[n]` for connection n.
    fn serve_first_lines(replies: Vec<&'static str>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let worker = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for reply in replies {
                let (stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut writer = stream.try_clone().unwrap();
                let mut line = String::new();
                BufReader::new(stream).read_line(&mut line).unwrap();
                seen.push(line.trim().to_string());
                writer.write_all(reply.as_bytes()).unwrap();
            }
            seen
        });
        (addr, worker)
    }

    #[test]
    fn an_app_without_auth_is_an_error_that_names_the_opt_out() {
        let _serial = crate::serial_sockets_and_children();
        let (addr, worker) = serve_first_lines(vec![
            "{\"id\":0,\"ok\":false,\"error\":\"unknown method `auth`\"}
",
        ]);
        let mut remote = Remote::lazy(&addr).with_token(Some("secret".into()));
        let err = remote.call("engine.commands", json!({})).unwrap_err();
        assert!(err.contains("LIGHTCRAFT_CONTROL_NO_AUTH") && err.contains("does not require a control token"), "{err}");
        assert_eq!(remote.token.as_deref(), Some("secret"), "the token is kept, not dropped");
        assert!(remote.conn.is_none());
        let seen = worker.join().unwrap();
        assert!(seen[0].contains("\"auth\""), "{seen:?}");
    }

    #[test]
    fn the_token_comes_from_the_environment_then_the_file_and_not_at_all_without_auth() {
        assert_eq!(token_for_env(false, Some(" env "), Some("file")).as_deref(), Some("env"));
        assert_eq!(
            token_for_env(
                false,
                Some("  "),
                Some(
                    "file
"
                )
            )
            .as_deref(),
            Some("file")
        );
        assert_eq!(token_for_env(false, None, Some("file")).as_deref(), Some("file"));
        assert_eq!(token_for_env(false, None, None), None);
        assert_eq!(token_for_env(true, Some("env"), Some("file")), None, "NO_AUTH wins over any token");
    }

    #[test]
    fn a_rejected_token_is_an_error_and_is_not_retried_without_it() {
        let _serial = crate::serial_sockets_and_children();
        let rejected = "{\"ok\":false,\"error\":\"control token rejected; closing the connection\"}
";
        let (addr, worker) = serve_first_lines(vec![rejected]);
        let mut remote = Remote::lazy(&addr).with_token(Some("wrong".into()));
        let err = remote.call("engine.commands", json!({})).unwrap_err();
        assert!(err.contains("token rejected"), "{err}");
        assert_eq!(remote.token.as_deref(), Some("wrong"));
        // a refused handshake is reported as it is: one connection, no retry, no downgrade
        let seen = worker.join().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].contains("wrong"), "{seen:?}");
    }

    static CURRENT_TOKEN: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

    /// Stands in for reading the token file: whatever the test last stored.
    fn current_token(_addr: &str) -> Option<String> {
        Some(CURRENT_TOKEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone())
    }

    /// Change what [`current_token`] returns, as a restarted app writing a new token file would.
    fn set_current_token(t: &str) {
        *CURRENT_TOKEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = t.to_string();
    }

    #[test]
    fn a_discovered_token_is_read_again_when_the_app_restarts() {
        let _serial = crate::serial_sockets_and_children();
        set_current_token("tokenA");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let worker = std::thread::spawn(move || {
            let mut auth_lines = Vec::new();
            for n in 1..=2u64 {
                let (stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut writer = stream.try_clone().unwrap();
                let mut reader = BufReader::new(stream);
                let mut auth = String::new();
                reader.read_line(&mut auth).unwrap();
                auth_lines.push(auth.trim().to_string());
                writer.write_all(b"{\"id\":0,\"ok\":true,\"result\":\"authenticated\"}\n").unwrap();
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                let id = serde_json::from_str::<Value>(&request).unwrap()["id"].clone();
                writer.write_all(format!("{}\n", json!({"id": id, "ok": true, "result": n})).as_bytes()).unwrap();
            }
            auth_lines
        });
        let mut remote = Remote::lazy(&addr);
        remote.token_lookup = current_token;
        assert_eq!(remote.call("engine.commands", json!({})).unwrap(), json!(1));
        // the app restarts: its connection is gone and the new run wrote another token
        set_current_token("tokenB");
        remote.conn = None;
        assert_eq!(remote.call("engine.commands", json!({})).unwrap(), json!(2));
        let seen = worker.join().unwrap();
        assert!(seen[0].contains("tokenA") && !seen[0].contains("tokenB"), "{seen:?}");
        assert!(seen[1].contains("tokenB") && !seen[1].contains("tokenA"), "{seen:?}");
    }

    #[test]
    fn an_explicit_token_is_never_replaced_by_a_lookup() {
        let _serial = crate::serial_sockets_and_children();
        set_current_token("discovered");
        let (addr, worker) = serve_first_lines(vec!["{\"id\":0,\"ok\":true,\"result\":\"authenticated\"}\n"]);
        let mut remote = Remote::lazy(&addr).with_token(Some("fixed".into()));
        remote.token_lookup = current_token;
        let _ = remote.reconnect();
        let seen = worker.join().unwrap();
        assert!(seen[0].contains("fixed") && !seen[0].contains("discovered"), "{seen:?}");
        assert_eq!(remote.token.as_deref(), Some("fixed"));
    }

    #[test]
    fn only_the_exact_authenticated_reply_is_accepted() {
        let _serial = crate::serial_sockets_and_children();
        for reply in [
            "{\"id\":0,\"ok\":true}\n",
            "{\"id\":0,\"ok\":true,\"result\":null}\n",
            "{\"id\":0,\"ok\":true,\"result\":\"anything else\"}\n",
            "{\"id\":7,\"ok\":true,\"result\":\"authenticated\"}\n",
            "{\"ok\":true,\"result\":\"authenticated\"}\n",
        ] {
            let (addr, worker) = serve_first_lines(vec![reply]);
            let mut remote = Remote::lazy(&addr).with_token(Some("t".into()));
            let err = remote.reconnect().unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{reply}: {err}");
            assert!(remote.conn.is_none(), "the connection is dropped: {reply}");
            worker.join().unwrap();
        }
        let (addr, worker) = serve_first_lines(vec!["{\"id\":0,\"ok\":true,\"result\":\"authenticated\"}\n"]);
        let mut remote = Remote::lazy(&addr).with_token(Some("t".into()));
        remote.reconnect().unwrap();
        assert!(remote.conn.is_some());
        worker.join().unwrap();
    }
}
