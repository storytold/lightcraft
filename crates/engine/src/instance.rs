//! Single-instance hand-off (gap 37): a second launch of the desktop app — a link the OS hands
//! over (`<scheme>://sync?path=…`), files dropped on the app's icon, or a plain double-click —
//! passes its arguments to the copy that is already running and exits, instead of opening the
//! same library twice.
//!
//! The running copy listens on a loopback TCP port (pure Rust, the same on every platform) and
//! writes `instance.json` (port, a random token, process id) into the settings folder. A new
//! launch reads that file, connects, sends one JSON line `{"v":1,"token":…,"args":[…]}` and waits
//! for `ok`. Anything else — no file, a stale file, nobody listening, a wrong token, a slow
//! answer — and the new launch simply starts as its own copy. A message is input from another
//! process: it is read with a size limit and a timeout, checked against the token, and capped in
//! argument count and length ([`parse_message`]); the app follows links through
//! [`crate::link::parse`], which allows a few actions only.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The file the running copy writes into the settings folder.
pub const FILE: &str = "instance.json";
/// The longest message line read.
pub const MAX_MESSAGE: usize = 64 * 1024;
/// The most arguments one hand-off carries.
pub const MAX_ARGS: usize = 256;
/// The longest argument (in bytes).
pub const MAX_ARG: usize = 8 * 1024;
/// How long either side waits on the other.
const TIMEOUT: Duration = Duration::from_secs(2);

/// What `instance.json` says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

#[derive(Serialize, Deserialize)]
struct Message {
    v: u32,
    token: String,
    args: Vec<String>,
}

/// The message a new launch sends.
pub fn message(token: &str, args: &[String]) -> String {
    let m = Message { v: 1, token: token.to_string(), args: args.to_vec() };
    serde_json::to_string(&m).unwrap_or_default()
}

/// Read a hand-off message: its arguments, when it carries the running copy's `token`.
pub fn parse_message(line: &str, token: &str) -> Result<Vec<String>, String> {
    if line.len() > MAX_MESSAGE {
        return Err("hand-off message too long".into());
    }
    let m: Message = serde_json::from_str(line.trim()).map_err(|e| format!("bad hand-off message: {e}"))?;
    if m.v != 1 {
        return Err(format!("unsupported hand-off version {}", m.v));
    }
    if token.is_empty() || !same(m.token.as_bytes(), token.as_bytes()) {
        return Err("hand-off token does not match".into());
    }
    if m.args.len() > MAX_ARGS {
        return Err(format!("hand-off carries more than {MAX_ARGS} arguments"));
    }
    if let Some(a) = m.args.iter().find(|a| a.len() > MAX_ARG || a.contains('\0')) {
        return Err(format!("hand-off argument not accepted ({} bytes)", a.len()));
    }
    Ok(m.args)
}

/// Read `instance.json`'s text.
pub fn parse_record(text: &str) -> Result<Record, String> {
    if text.len() > 4096 {
        return Err("instance file too long".into());
    }
    let r: Record = serde_json::from_str(text).map_err(|e| format!("bad instance file: {e}"))?;
    if r.port == 0 || r.token.is_empty() {
        return Err("instance file names no port or token".into());
    }
    Ok(r)
}

/// Compare without stopping at the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A random token (from the OS-seeded hasher keys std keeps per `RandomState`).
fn new_token() -> String {
    use std::hash::BuildHasher;
    let mut s = String::new();
    for i in 0..2u64 {
        let h = std::collections::hash_map::RandomState::new().hash_one((i, std::process::id(), std::time::SystemTime::now()));
        s.push_str(&format!("{h:016x}"));
    }
    s
}

/// The running copy's listener: hand-offs arrive on [`Server::rx`].
pub struct Server {
    path: PathBuf,
    token: String,
    pub rx: Receiver<Vec<String>>,
}

impl Server {
    /// Listen and write `instance.json` into `dir`.
    pub fn start(dir: &Path) -> Result<Server, String> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| format!("hand-off listener: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let token = new_token();
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join(FILE);
        let rec = Record { port, token: token.clone(), pid: std::process::id() };
        let text = serde_json::to_string(&rec).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        let (tx, rx) = channel();
        let t = token.clone();
        std::thread::Builder::new()
            .name("instance-handoff".into())
            .spawn(move || serve(listener, &t, tx))
            .map_err(|e| format!("hand-off thread: {e}"))?;
        Ok(Server { path, token, rx })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // only our own file: a later copy may have written its own
        if std::fs::read_to_string(&self.path).ok().and_then(|t| parse_record(&t).ok()).is_some_and(|r| r.token == self.token) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn serve(listener: TcpListener, token: &str, tx: Sender<Vec<String>>) {
    for conn in listener.incoming() {
        let Ok(mut conn) = conn else { continue };
        match read_one(&mut conn, token) {
            Ok(args) => {
                let _ = conn.write_all(b"ok\n");
                if tx.send(args).is_err() {
                    return; // the app has ended
                }
            }
            Err(e) => {
                log::info!("hand-off refused: {e}");
                let _ = conn.write_all(b"no\n");
            }
        }
    }
}

fn read_one(conn: &mut TcpStream, token: &str) -> Result<Vec<String>, String> {
    let _ = conn.set_read_timeout(Some(TIMEOUT));
    let _ = conn.set_write_timeout(Some(TIMEOUT));
    let mut line = String::new();
    BufReader::new(conn.take(MAX_MESSAGE as u64 + 1)).read_line(&mut line).map_err(|e| format!("hand-off read: {e}"))?;
    parse_message(&line, token)
}

/// Hand `args` to the copy `dir`'s `instance.json` names. `Ok(true)`: it took them, this launch
/// can exit; `Ok(false)`: nobody took them (start as usual).
pub fn hand_off(dir: &Path, args: &[String]) -> Result<bool, String> {
    let Ok(text) = std::fs::read_to_string(dir.join(FILE)) else { return Ok(false) };
    let Ok(rec) = parse_record(&text) else { return Ok(false) };
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, rec.port));
    let Ok(mut conn) = TcpStream::connect_timeout(&addr, TIMEOUT) else { return Ok(false) };
    let _ = conn.set_read_timeout(Some(TIMEOUT));
    let _ = conn.set_write_timeout(Some(TIMEOUT));
    let mut msg = message(&rec.token, args);
    if msg.len() > MAX_MESSAGE {
        return Err("too many or too long arguments to hand to the running app".into());
    }
    msg.push('\n');
    if conn.write_all(msg.as_bytes()).is_err() {
        return Ok(false);
    }
    let mut reply = String::new();
    let _ = BufReader::new(conn.take(16)).read_line(&mut reply);
    Ok(reply.trim() == "ok")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lcc-instance-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn second_launch_hands_its_args_to_the_first() {
        let d = dir("handoff");
        let server = Server::start(&d).unwrap();
        let args = vec![crate::link::sync_link("/p/a b"), "/photos/x.jpg".to_string()];
        assert!(hand_off(&d, &args).unwrap());
        assert_eq!(server.rx.recv_timeout(Duration::from_secs(5)).unwrap(), args);
        // a plain launch (no arguments) is handed over too: the running copy comes forward
        assert!(hand_off(&d, &[]).unwrap());
        assert_eq!(server.rx.recv_timeout(Duration::from_secs(5)).unwrap(), Vec::<String>::new());
        drop(server);
        assert!(!d.join(FILE).exists(), "the file goes with the running copy");
        // nobody running: start as usual
        assert!(!hand_off(&d, &args).unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn stale_or_hostile_instance_files_mean_start_as_usual() {
        let d = dir("stale");
        std::fs::create_dir_all(&d).unwrap();
        let free = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap().port();
        for text in [
            String::new(),
            "{".into(),
            "null".into(),
            r#"{"port":0,"token":"x","pid":1}"#.into(),
            r#"{"port":70000,"token":"x","pid":1}"#.into(),
            r#"{"port":1,"token":"","pid":1}"#.into(),
            format!(r#"{{"port":{free},"token":"x","pid":1}}"#),
            "x".repeat(10_000),
        ] {
            std::fs::write(d.join(FILE), &text).unwrap();
            assert!(!hand_off(&d, &["a".into()]).unwrap(), "{text:.40}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_wrong_token_is_refused() {
        let d = dir("token");
        let server = Server::start(&d).unwrap();
        let mut rec = parse_record(&std::fs::read_to_string(d.join(FILE)).unwrap()).unwrap();
        rec.token = "f".repeat(32);
        std::fs::write(d.join(FILE), serde_json::to_string(&rec).unwrap()).unwrap();
        assert!(!hand_off(&d, &["x".into()]).unwrap());
        assert!(server.rx.recv_timeout(Duration::from_millis(300)).is_err());
        // a client that sends garbage, or nothing, doesn't stop the listener
        for junk in [&b"\xff\xfe\n"[..], b"", &vec![b'a'; MAX_MESSAGE + 10]] {
            let mut c = TcpStream::connect((Ipv4Addr::LOCALHOST, rec.port)).unwrap();
            let _ = c.write_all(junk);
            drop(c);
        }
        drop(server);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn hostile_messages_are_errors() {
        let t = "tok";
        let ok = message(t, &["a".into()]);
        assert_eq!(parse_message(&ok, t).unwrap(), vec!["a".to_string()]);
        for bad in [
            String::new(),
            "\u{0}".into(),
            "[]".into(),
            r#"{"v":1}"#.into(),
            r#"{"v":2,"token":"tok","args":[]}"#.into(),
            r#"{"v":1,"token":"to","args":[]}"#.into(),
            r#"{"v":1,"token":"tokk","args":[]}"#.into(),
            r#"{"v":1,"token":"tok","args":[1]}"#.into(),
            r#"{"v":-1,"token":"tok","args":[]}"#.into(),
            r#"{"v":1,"token":"tok","args":["a\u0000b"]}"#.into(),
            message(t, &vec!["a".to_string(); MAX_ARGS + 1]),
            message(t, &["a".repeat(MAX_ARG + 1)]),
            format!("{}{}", ok, " ".repeat(MAX_MESSAGE)),
            "{".repeat(100_000),
        ] {
            assert!(parse_message(&bad, t).is_err(), "{bad:.60}");
        }
        assert!(parse_message(&ok, "").is_err(), "no token, no hand-off");
    }
}
