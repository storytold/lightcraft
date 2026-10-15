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
}

impl Remote {
    /// Connect to `addr` (e.g. `127.0.0.1:7980`), failing fast when nothing is listening.
    pub fn connect(addr: &str) -> std::io::Result<Self> {
        let mut r = Self { addr: addr.to_string(), conn: None, next_id: 1 };
        r.reconnect()?;
        Ok(r)
    }

    /// A remote that connects on first use (the app may start after the MCP server).
    pub fn lazy(addr: &str) -> Self {
        Self { addr: addr.to_string(), conn: None, next_id: 1 }
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    fn reconnect(&mut self) -> std::io::Result<()> {
        self.conn = None;
        let mut last = std::io::Error::new(std::io::ErrorKind::NotFound, format!("cannot resolve {}", self.addr));
        for sa in self.addr.to_socket_addrs()? {
            match TcpStream::connect_timeout(&sa, Duration::from_millis(800)) {
                Ok(s) => {
                    s.set_nodelay(true).ok();
                    // The app answers within 60 s (its own timeout); leave headroom.
                    s.set_read_timeout(Some(Duration::from_secs(90))).ok();
                    let read = s.try_clone()?;
                    self.conn = Some((BufReader::new(read), s));
                    return Ok(());
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

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

impl Backend for Remote {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let line = json!({"id": id, "method": method, "params": params}).to_string();
        // Retry once with a fresh connection (the app may have restarted), but only when the request
        // never reached the app. After a full write the command may have run, so resending it could
        // apply a mutation twice (toggle, undo, ...): report the unknown outcome instead.
        let reply = match self.roundtrip(&line) {
            Ok(r) => r,
            Err((_, false)) => {
                self.conn = None;
                self.roundtrip(&line).map_err(|(e, sent)| {
                    self.conn = None;
                    if sent {
                        self.unknown_outcome(method, &e)
                    } else {
                        format!("LightCraft app at {} is not reachable: {e} (start it with `lightcraft --control PORT`)", self.addr)
                    }
                })?
            }
            Err((e, true)) => {
                self.conn = None;
                return Err(self.unknown_outcome(method, &e));
            }
        };
        let v = parse_reply(&reply, id).map_err(|e| {
            self.conn = None;
            self.unknown_outcome(method, &e)
        })?;
        if v.get("id").is_none() {
            self.conn = None;
        }
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

    #[test]
    fn unreachable_app_is_reported() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);
        let err = Remote::lazy(&addr).call("engine.commands", json!({})).unwrap_err();
        assert!(err.contains("not reachable"), "{err}");
    }
}
