//! Phase 4 exit gate (P4.7): one scripted session through MCP, headless, driving the real CLI
//! binary (`app-cli mcp --library …`) over stdio:
//!
//! 1. tethered shoot with the simulated PTP camera;
//! 2. the shoot's import preset (develop preset + keywords) lands on every shot;
//! 3. cull: pick, reject, rate;
//! 4. develop a few settings;
//! 5. publish the picks to a Hard Drive folder, to an SFTP server (an in-process russh server
//!    with an in-memory file system) and to Immich (an in-process fake server keeping state like
//!    a v3.3 server), as an album of renders stacked on their originals;
//! 6. a rating edited on the Immich side syncs back into the catalog;
//! 7. the selection printed as a contact sheet PDF.
//!
//! Everything is local (127.0.0.1) and temporary: HOME / XDG_CONFIG_HOME / APPDATA point into a
//! temp folder, account keys go to the encrypted key file (no system keychain).
#![cfg(not(target_arch = "wasm32"))]

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

// ---------------------------------------------------------------------------------------------
// The CLI binary

/// `app-cli` in this test's target folder. It lives in another package, so cargo neither hands us
/// its path nor rebuilds it for `cargo test -p dac-mcp`: build it here (a no-op when it is up to
/// date), so the session never drives a stale binary.
fn app_cli() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().and_then(Path::parent).unwrap().to_path_buf(); // target/<profile>
    let bin = dir.join(format!("app-cli{}", std::env::consts::EXE_SUFFIX));
    let mut cmd = Command::new(option_env!("CARGO").unwrap_or("cargo"));
    cmd.args(["build", "--quiet", "-p", "dac-cli", "--bin", "app-cli"]);
    if dir.file_name().is_some_and(|n| n == "release") {
        cmd.arg("--release");
    }
    if let Some(target) = dir.parent() {
        cmd.env("CARGO_TARGET_DIR", target);
    }
    let built = cmd.current_dir(env!("CARGO_MANIFEST_DIR")).status().is_ok_and(|s| s.success());
    assert!(built || bin.exists(), "building app-cli failed and there is none in {}", dir.display());
    bin
}

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Mcp {
    fn start(library: &Path, home: &Path) -> Mcp {
        let mut child = Command::new(app_cli())
            .args(["mcp", "--library"])
            .arg(library)
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("APPDATA", home.join("config"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut m = Mcp { child, stdin, stdout, next: 1 };
        let r =
            m.rpc("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "session-e2e", "version": "0"}}));
        assert_eq!(r["result"]["serverInfo"]["name"], dac_brand::MCP_SERVER);
        writeln!(m.stdin, "{}", json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).unwrap();
        m
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        writeln!(self.stdin, "{}", json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).unwrap();
        self.stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "the MCP server exited");
            let v: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"));
            if v["id"] == id {
                return v;
            }
        }
    }

    /// Call a tool; `Err(message)` when it reports an error.
    fn try_tool(&mut self, name: &str, args: Value) -> Result<Value, String> {
        let r = self.rpc("tools/call", json!({"name": name, "arguments": args}));
        assert!(r["error"].is_null(), "{name}: {r}");
        let res = &r["result"];
        let text = res["content"][0]["text"].as_str().unwrap_or("").to_string();
        if res["isError"] == true {
            return Err(text);
        }
        Ok(match res.get("structuredContent") {
            Some(v) => v.clone(),
            None => serde_json::from_str(&text).unwrap_or(Value::Null),
        })
    }

    fn tool(&mut self, name: &str, args: Value) -> Value {
        self.try_tool(name, args.clone()).unwrap_or_else(|e| panic!("{name} {args}: {e}"))
    }

    fn cmd(&mut self, command: &str, params: Value) -> Value {
        self.tool("run_command", json!({"command": command, "params": params}))
    }

    fn try_cmd(&mut self, command: &str, params: Value) -> Result<Value, String> {
        self.try_tool("run_command", json!({"command": command, "params": params}))
    }

    fn photo(&mut self, id: u64) -> Value {
        let all = self.tool("query_photos", json!({"limit": 1000}));
        all["photos"].as_array().unwrap().iter().find(|p| p["id"] == id).cloned().unwrap_or_else(|| panic!("no photo {id}: {all}"))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------------------------
// A fake Immich server (the subset of the v3 API that publish, link and sync use)

const IMMICH_KEY: &str = "e2e-immich-key";

#[derive(Default)]
struct Immich {
    assets: Vec<Value>,
    /// (id, name, asset ids)
    albums: Vec<(String, String, Vec<String>)>,
    stacks: Vec<Vec<String>>,
    clock: u32,
    uploads: u32,
}

impl Immich {
    fn tick(&mut self) -> String {
        self.clock += 1;
        format!("2026-10-11T01:{:02}:{:02}.000Z", self.clock / 60 % 60, self.clock % 60)
    }
    fn asset_mut(&mut self, id: &str) -> Option<&mut Value> {
        self.assets.iter_mut().find(|a| a["id"] == id)
    }
}

struct Request {
    method: String,
    target: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn read_request(s: impl Read) -> Option<Request> {
    let mut r = BufReader::new(s);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let mut it = line.split_whitespace();
    let (method, target) = (it.next()?.to_string(), it.next()?.to_string());
    let mut headers = HashMap::new();
    loop {
        let mut l = String::new();
        r.read_line(&mut l).ok()?;
        let l = l.trim_end();
        if l.is_empty() {
            break;
        }
        let (k, v) = l.split_once(':')?;
        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
    }
    let len = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0usize);
    let mut body = vec![0; len];
    r.read_exact(&mut body).ok()?;
    Some(Request { method, target, headers, body })
}

fn response(status: &str, body: &[u8]) -> Vec<u8> {
    let mut out =
        format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out
}

/// One part of a multipart/form-data body: (file name, bytes).
fn multipart_part(req: &Request, field: &str) -> Option<(String, Vec<u8>)> {
    let ct = req.headers.get("content-type")?;
    let boundary = ct.split("boundary=").nth(1)?.trim_matches('"').to_string();
    let delim = format!("--{boundary}").into_bytes();
    let body = &req.body;
    let find = |hay: &[u8], needle: &[u8], from: usize| hay.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|p| p + from);
    let mut at = find(body, &delim, 0)?;
    loop {
        let start = at + delim.len();
        let next = find(body, &delim, start)?;
        let part = body.get(start..next)?;
        let head_end = find(part, b"\r\n\r\n", 0)?;
        let head = String::from_utf8_lossy(&part[..head_end]);
        if head.contains(&format!("name=\"{field}\"")) {
            let name = head.split("filename=\"").nth(1).and_then(|x| x.split('"').next()).unwrap_or("").to_string();
            let data = part.get(head_end + 4..part.len().saturating_sub(2))?.to_vec(); // drop the CRLF before the delimiter
            return Some((name, data));
        }
        at = next;
    }
}

fn serve_immich(state: Arc<Mutex<Immich>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(mut s) = s else { return };
            let Some(req) = read_request(&mut s) else { continue };
            let out = immich_handle(&state, &req);
            let _ = s.write_all(&out);
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn immich_handle(state: &Mutex<Immich>, req: &Request) -> Vec<u8> {
    let json = |v: Value| response("200 OK", v.to_string().as_bytes());
    let not_found = || response("404 Not Found", b"{}");
    let m = req.method.as_str();
    let path = req.target.split('?').next().unwrap_or("");
    if path == "/api/server/version" {
        return json(json!({"major": 3, "minor": 3, "patch": 1}));
    }
    if req.headers.get("x-api-key").map(String::as_str) != Some(IMMICH_KEY) {
        return response("401 Unauthorized", br#"{"message":"Invalid API key"}"#);
    }
    let b: Value = serde_json::from_slice(&req.body).unwrap_or_default();
    let mut st = state.lock().unwrap();
    match (m, path) {
        ("GET", "/api/users/me") => json(json!({"id": "user-1", "email": "me@example.invalid", "name": "Me"})),
        ("GET", "/api/api-keys/me") => json(json!({"id": "k", "name": "e2e", "permissions": ["all"]})),
        ("POST", "/api/search/metadata") => {
            let after = b["updatedAfter"].as_str().unwrap_or("");
            let album: Option<Vec<String>> =
                b["albumIds"][0].as_str().map(|a| st.albums.iter().find(|x| x.0 == a).map(|x| x.2.clone()).unwrap_or_default());
            let items: Vec<Value> = if let Some(ids) = album {
                ids.iter().map(|i| json!({"id": i})).collect()
            } else if b["page"].as_u64().unwrap_or(1) == 1 {
                st.assets.iter().filter(|a| a["updatedAt"].as_str().unwrap_or("") > after).cloned().collect()
            } else {
                vec![]
            };
            json(json!({"assets": {"total": items.len(), "count": items.len(), "items": items, "nextPage": null}}))
        }
        ("POST", "/api/assets") => {
            let Some((name, data)) = multipart_part(req, "assetData") else { return response("400 Bad Request", b"{}") };
            let checksum = dac_hash::sha1_bytes(&data).to_base64();
            if let Some(a) = st.assets.iter().find(|a| a["checksum"] == checksum.as_str()) {
                return json(json!({"id": a["id"], "status": "duplicate"}));
            }
            st.uploads += 1;
            let id = format!("up-{}", st.uploads);
            let now = st.tick();
            let created = String::from_utf8_lossy(&req.body)
                .split("name=\"fileCreatedAt\"\r\n\r\n")
                .nth(1)
                .and_then(|x| x.split("\r\n").next())
                .unwrap_or("2026-10-11T00:00:00.000Z")
                .to_string();
            st.assets.push(json!({
                "id": id, "type": "IMAGE", "originalFileName": name, "updatedAt": now, "checksum": checksum,
                "localDateTime": created, "fileCreatedAt": created, "isFavorite": false,
                "exifInfo": {"fileSizeInByte": data.len()}, "tags": [], "visibility": "timeline",
            }));
            response("201 Created", json!({"id": id, "status": "created"}).to_string().as_bytes())
        }
        ("POST", "/api/assets/bulk-upload-check") => {
            let results: Vec<Value> = b["assets"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|x| match st.assets.iter().find(|a| a["checksum"] == x["checksum"]) {
                    Some(a) => json!({"id": x["id"], "action": "reject", "reason": "duplicate", "assetId": a["id"]}),
                    None => json!({"id": x["id"], "action": "accept"}),
                })
                .collect();
            json(json!({"results": results}))
        }
        ("GET", "/api/albums") => {
            json(json!(st.albums.iter().map(|(i, n, a)| json!({"id": i, "albumName": n, "assetCount": a.len()})).collect::<Vec<_>>()))
        }
        ("POST", "/api/albums") => {
            let id = format!("album-{}", st.albums.len() + 1);
            st.albums.push((id.clone(), b["albumName"].as_str().unwrap_or("").to_string(), vec![]));
            json(json!({"id": id, "albumName": b["albumName"]}))
        }
        (_, p) if p.starts_with("/api/albums/") => {
            let rest = p.trim_start_matches("/api/albums/");
            let (id, tail) = rest.split_once('/').unwrap_or((rest, ""));
            let ids: Vec<String> = b["ids"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
            let Some(al) = st.albums.iter_mut().find(|a| a.0 == id) else { return not_found() };
            match (m, tail) {
                ("PUT", "assets") => ids.into_iter().for_each(|i| {
                    if !al.2.contains(&i) {
                        al.2.push(i)
                    }
                }),
                ("DELETE", "assets") => al.2.retain(|x| !ids.contains(x)),
                _ => {}
            }
            json(json!({"id": al.0, "albumName": al.1, "assets": al.2.iter().map(|i| json!({"id": i})).collect::<Vec<_>>()}))
        }
        ("POST", "/api/stacks") => {
            st.stacks.push(b["assetIds"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect());
            json(json!({"id": format!("stack-{}", st.stacks.len())}))
        }
        ("PUT", "/api/tags") => {
            let out: Vec<Value> = b["tags"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|v| json!({"id": format!("tag-{}", v.replace('/', "-")), "value": v}))
                .collect();
            json(json!(out))
        }
        ("PUT", p) | ("DELETE", p) if p.starts_with("/api/tags/") && p.ends_with("/assets") => {
            let tag = p.trim_start_matches("/api/tags/").trim_end_matches("/assets").to_string();
            let value = tag.trim_start_matches("tag-").replace('-', "/");
            let now = st.tick();
            for id in b["ids"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                if let Some(a) = st.asset_mut(id) {
                    let mut tags: Vec<Value> =
                        a["tags"].as_array().cloned().unwrap_or_default().into_iter().filter(|x| x["id"] != tag.as_str()).collect();
                    if m == "PUT" {
                        tags.push(json!({"id": tag, "value": value}));
                    }
                    a["tags"] = json!(tags);
                    a["updatedAt"] = json!(now);
                }
            }
            json(json!([]))
        }
        ("DELETE", "/api/assets") => {
            let now = st.tick();
            for id in b["ids"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                if let Some(a) = st.asset_mut(id) {
                    a["isTrashed"] = json!(true);
                    a["updatedAt"] = json!(now);
                }
            }
            response("204 No Content", b"")
        }
        (_, p) if p.starts_with("/api/assets/") => {
            let id = p.trim_start_matches("/api/assets/").to_string();
            let now = st.tick();
            let Some(a) = st.asset_mut(&id) else { return not_found() };
            if m == "PUT" {
                if let Some(r) = b.get("rating") {
                    a["exifInfo"]["rating"] = r.clone();
                }
                if let Some(f) = b.get("isFavorite") {
                    a["isFavorite"] = f.clone();
                }
                if let Some(d) = b.get("description") {
                    a["exifInfo"]["description"] = d.clone();
                }
                if let Some(v) = b.get("visibility") {
                    a["visibility"] = v.clone();
                }
                a["updatedAt"] = json!(now);
            }
            json(a.clone())
        }
        _ => {
            eprintln!("fake Immich: unhandled {m} {path}");
            not_found()
        }
    }
}

// ---------------------------------------------------------------------------------------------
// An in-process SFTP server: russh + russh-sftp over an in-memory file system

const SFTP_USER: &str = "e2e";
const SFTP_PASSWORD: &str = "e2e-sftp-password";

#[derive(Default)]
struct MemFs {
    files: HashMap<String, Vec<u8>>,
    dirs: HashSet<String>,
}

type SharedFs = Arc<Mutex<MemFs>>;

mod sftp_server {
    use super::*;
    use russh::server::{Auth, ChannelOpenHandle, Msg, Session};
    use russh::{Channel, ChannelId};
    use russh_sftp::protocol::{Attrs, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version};

    pub struct Ssh {
        pub fs: SharedFs,
        pub channels: HashMap<ChannelId, Channel<Msg>>,
    }

    impl russh::server::Handler for Ssh {
        type Error = russh::Error;

        async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
            Ok(if user == SFTP_USER && password == SFTP_PASSWORD { Auth::Accept } else { Auth::reject() })
        }

        async fn channel_open_session(&mut self, channel: Channel<Msg>, reply: ChannelOpenHandle, _: &mut Session) -> Result<(), Self::Error> {
            self.channels.insert(channel.id(), channel);
            reply.accept().await;
            Ok(())
        }

        async fn channel_eof(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
            session.close(channel)?;
            Ok(())
        }

        async fn subsystem_request(&mut self, id: ChannelId, name: &str, session: &mut Session) -> Result<(), Self::Error> {
            match (name, self.channels.remove(&id)) {
                ("sftp", Some(ch)) => {
                    session.channel_success(id)?;
                    russh_sftp::server::run(ch.into_stream(), Sftp { fs: self.fs.clone(), handles: HashMap::new(), next: 0 }).await;
                }
                _ => session.channel_failure(id)?,
            }
            Ok(())
        }
    }

    struct Sftp {
        fs: SharedFs,
        handles: HashMap<String, String>,
        next: u32,
    }

    fn ok(id: u32) -> Status {
        Status { id, status_code: StatusCode::Ok, error_message: "Ok".into(), language_tag: "en-US".into() }
    }

    impl Sftp {
        fn attrs(&self, id: u32, path: &str) -> Result<Attrs, StatusCode> {
            let fs = self.fs.lock().unwrap();
            if path == "/" || fs.dirs.contains(path) {
                return Ok(Attrs { id, attrs: FileAttributes { permissions: Some(0o40755), size: Some(0), ..FileAttributes::empty() } });
            }
            match fs.files.get(path) {
                Some(f) => {
                    Ok(Attrs { id, attrs: FileAttributes { permissions: Some(0o100644), size: Some(f.len() as u64), ..FileAttributes::empty() } })
                }
                None => Err(StatusCode::NoSuchFile),
            }
        }
    }

    impl russh_sftp::server::Handler for Sftp {
        type Error = StatusCode;

        fn unimplemented(&self) -> Self::Error {
            StatusCode::OpUnsupported
        }

        async fn init(&mut self, _: u32, _: HashMap<String, String>) -> Result<Version, Self::Error> {
            Ok(Version::new())
        }

        async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
            let p = if path.is_empty() || path == "." { "/".to_string() } else { path };
            Ok(Name { id, files: vec![File::dummy(p)] })
        }

        async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
            self.attrs(id, &path)
        }

        async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
            self.attrs(id, &path)
        }

        async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
            let path = self.handles.get(&handle).cloned().ok_or(StatusCode::Failure)?;
            self.attrs(id, &path)
        }

        async fn mkdir(&mut self, id: u32, path: String, _: FileAttributes) -> Result<Status, Self::Error> {
            self.fs.lock().unwrap().dirs.insert(path);
            Ok(ok(id))
        }

        async fn open(&mut self, id: u32, filename: String, flags: OpenFlags, _: FileAttributes) -> Result<Handle, Self::Error> {
            {
                let mut fs = self.fs.lock().unwrap();
                let parent = filename.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
                if !parent.is_empty() && !fs.dirs.contains(parent) {
                    return Err(StatusCode::NoSuchFile);
                }
                if flags.contains(OpenFlags::CREATE) {
                    let f = fs.files.entry(filename.clone()).or_default();
                    if flags.contains(OpenFlags::TRUNCATE) {
                        f.clear();
                    }
                } else if !fs.files.contains_key(&filename) {
                    return Err(StatusCode::NoSuchFile);
                }
            }
            self.next += 1;
            let handle = format!("h{}", self.next);
            self.handles.insert(handle.clone(), filename);
            Ok(Handle { id, handle })
        }

        async fn write(&mut self, id: u32, handle: String, offset: u64, data: Vec<u8>) -> Result<Status, Self::Error> {
            let path = self.handles.get(&handle).cloned().ok_or(StatusCode::Failure)?;
            let mut fs = self.fs.lock().unwrap();
            let f = fs.files.get_mut(&path).ok_or(StatusCode::NoSuchFile)?;
            let (start, end) = (offset as usize, offset as usize + data.len());
            if f.len() < end {
                f.resize(end, 0);
            }
            f[start..end].copy_from_slice(&data);
            Ok(ok(id))
        }

        async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
            self.handles.remove(&handle);
            Ok(ok(id))
        }

        async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
            match self.fs.lock().unwrap().files.remove(&filename) {
                Some(_) => Ok(ok(id)),
                None => Err(StatusCode::NoSuchFile),
            }
        }
    }
}

/// Starts the SFTP server; returns its port.
fn serve_sftp(fs: SharedFs) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            let key = russh::keys::PrivateKey::from(russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[7u8; 32]));
            let config = Arc::new(russh::server::Config {
                keys: vec![key],
                auth_rejection_time: Duration::from_millis(10),
                auth_rejection_time_initial: Some(Duration::ZERO),
                ..Default::default()
            });
            loop {
                let Ok((sock, _)) = listener.accept().await else { return };
                let (config, fs) = (config.clone(), fs.clone());
                tokio::spawn(async move {
                    let handler = sftp_server::Ssh { fs, channels: HashMap::new() };
                    if let Ok(session) = russh::server::run_stream(config, sock, handler).await {
                        let _ = session.await;
                    }
                });
            }
        });
    });
    port
}

// ---------------------------------------------------------------------------------------------
// The session

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("dac-session-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn ids_of(v: &Value) -> Vec<u64> {
    v.as_array().into_iter().flatten().filter_map(Value::as_u64).collect()
}

fn published(run: &Value) -> Vec<(u64, String)> {
    let r = &run["runs"][0];
    assert!(r["failed"].as_array().is_none_or(Vec::is_empty), "publish failures: {run}");
    r["published"].as_array().unwrap().iter().map(|p| (p["id"].as_u64().unwrap(), p["remoteId"].as_str().unwrap().to_string())).collect()
}

#[test]
fn tether_cull_develop_publish_sync_print() {
    let root = temp_dir("gate");
    let home = root.join("home");
    std::fs::create_dir_all(home.join("config")).unwrap();
    let mut mcp = Mcp::start(&root.join("library"), &home);

    // account keys: the encrypted key file, so no system keychain is needed (CI)
    mcp.cmd("credentials.useStore", json!({"store": "file"}));
    let r = mcp.cmd("credentials.unlock", json!({"passphrase": "e2e-passphrase"}));
    assert_eq!(r["ok"], true, "{r}");

    // 1 + 2. tethered shoot with the import preset: develop preset + keywords
    let cam = mcp.cmd(
        "tether.connect",
        json!({"device": "sim", "session": "Gate Shoot", "preset": "lc.warm-glow", "keywords": ["studio", "gate"], "collection": "Gate Shoot"}),
    );
    assert_eq!(cam["name"], "Simulated PTP Camera", "{cam}");
    let mut shots = Vec::new();
    for _ in 0..3 {
        let r = mcp.cmd("tether.capture", json!({}));
        shots.extend(ids_of(&r["imported"]));
    }
    // the camera's own shutter button: picked up by the folder scan
    mcp.cmd("tether.simShoot", json!({}));
    for _ in 0..3 {
        let r = mcp.cmd("tether.scan", json!({}));
        shots.extend(ids_of(&r["imported"]));
    }
    assert_eq!(shots.len(), 4, "four shots imported: {shots:?}");
    for id in &shots {
        let p = mcp.photo(*id);
        let kw: Vec<&str> = p["keywords"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert!(kw.contains(&"studio") && kw.contains(&"gate"), "import keywords on {p}");
        let d = mcp.cmd("develop.get", json!({"id": id}));
        assert_eq!(d["wb"]["temp"].as_f64(), Some(7200.0), "the develop preset (Warm Glow) on {id}: {}", d["wb"]);
    }
    mcp.cmd("tether.disconnect", json!({}));

    // 3. cull
    let (a, b, c, d) = (shots[0], shots[1], shots[2], shots[3]);
    mcp.cmd("photo.flag", json!({"ids": [a, c], "flag": "pick"}));
    mcp.cmd("photo.reject", json!({"ids": [b]}));
    mcp.cmd("photo.rate", json!({"ids": [a], "rating": 5}));
    mcp.cmd("photo.rate", json!({"ids": [c], "rating": 3}));
    mcp.cmd("photo.rate", json!({"ids": [d], "rating": 1}));
    let picks = mcp.tool("query_photos", json!({"filter": {"flag": "pick"}}));
    let mut pick_ids: Vec<u64> = picks["photos"].as_array().unwrap().iter().filter_map(|p| p["id"].as_u64()).collect();
    pick_ids.sort();
    assert_eq!(pick_ids, vec![a, c], "{picks}");
    assert_eq!(mcp.photo(b)["flag"], "reject");

    // 4. develop
    let r = mcp.tool("set_develop", json!({"id": a, "values": {"light.exposure": 0.7, "light.contrast": 20, "color.vibrance": 25}}));
    assert_eq!(r["controls"][0]["value"], 0.7, "{r}");
    mcp.tool("set_develop", json!({"id": c, "values": {"light.exposure": -0.3}}));
    let dev = mcp.cmd("develop.get", json!({"id": a}));
    assert_eq!(dev["light"]["exposure"].as_f64(), Some(0.7));
    assert_eq!(mcp.photo(a)["edited"], true);

    // 5. publish the picks …
    mcp.cmd("library.select", json!({"ids": [a, c]}));
    let export = json!({"format": "jpeg", "quality": 85, "longEdge": 320});

    // … to a Hard Drive folder
    let disk = root.join("published");
    let svc = mcp.cmd("publish.createService", json!({"kind": "hardDrive", "name": "Disk", "dir": disk.to_string_lossy(), "export": export}));
    let coll = mcp.cmd("publish.createCollection", json!({"service": svc["id"], "name": "Gate Picks", "addSelected": true}));
    let disk_coll = coll["collection"].as_u64().unwrap();
    let run = mcp.cmd("publish.run", json!({"collection": disk_coll}));
    let files = published(&run);
    assert_eq!(files.len(), 2, "{run}");
    let jpgs: Vec<PathBuf> = walk(&disk).into_iter().filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("jpg"))).collect();
    assert_eq!(jpgs.len(), 2, "{jpgs:?}");
    for j in &jpgs {
        assert!(std::fs::read(j).unwrap().starts_with(&[0xff, 0xd8]), "{}", j.display());
    }
    let st = mcp.cmd("publish.status", json!({"collection": disk_coll}));
    assert_eq!(st["published"].as_array().map(Vec::len).or(st["published"].as_u64().map(|n| n as usize)), Some(2), "{st}");

    // … to SFTP (a saved upload server, its password in the key file)
    let fs: SharedFs = Arc::default();
    let port = serve_sftp(fs.clone());
    mcp.cmd(
        "web.saveServer",
        json!({"server": {"name": "Gate SFTP", "host": "127.0.0.1", "port": port, "user": SFTP_USER, "path": "/upload"}, "password": SFTP_PASSWORD}),
    );
    assert!(
        mcp.try_cmd("publish.createService", json!({"kind": "sftp", "name": "No server", "settings": {}})).is_err(),
        "an SFTP service needs a server"
    );
    let svc = mcp.cmd("publish.createService", json!({"kind": "sftp", "name": "SFTP", "settings": {"server": "Gate SFTP"}, "export": export}));
    let coll = mcp.cmd("publish.createCollection", json!({"service": svc["id"], "name": "Gate Picks", "addSelected": true}));
    let sftp_coll = coll["collection"].as_u64().unwrap();
    let run = mcp.cmd("publish.run", json!({"collection": sftp_coll}));
    let sent = published(&run);
    assert_eq!(sent.len(), 2, "{run}");
    {
        let fs = fs.lock().unwrap();
        for (_, rel) in &sent {
            let f = fs.files.get(&format!("/upload/{rel}")).unwrap_or_else(|| panic!("/upload/{rel} not on the server: {:?}", fs.files.keys()));
            assert!(f.starts_with(&[0xff, 0xd8]) && f.len() > 1000, "{rel}: {} bytes", f.len());
        }
    }
    // trust on first use: the saved server now pins the host key
    let servers = mcp.cmd("web.servers", json!({}));
    let fp = servers
        .as_array()
        .and_then(|a| a.first())
        .map(|x| x["knownFingerprint"].clone())
        .unwrap_or(servers["servers"][0]["knownFingerprint"].clone());
    assert!(fp.as_str().is_some_and(|f| f.starts_with("SHA256:")), "{servers}");
    // taken out of the collection: deleted on the server at the next publish
    let (gone_photo, gone_rel) = sent.iter().find(|(p, _)| *p == c).cloned().unwrap();
    mcp.cmd("album.removePhotos", json!({"id": sftp_coll, "ids": [gone_photo]}));
    let run = mcp.cmd("publish.run", json!({"collection": sftp_coll}));
    assert_eq!(run["runs"][0]["removed"], json!([gone_photo]), "{run}");
    assert!(!fs.lock().unwrap().files.contains_key(&format!("/upload/{gone_rel}")));

    // … to Immich: an album of renders stacked on their originals
    let immich = Arc::new(Mutex::new(Immich::default()));
    let url = serve_immich(immich.clone());
    let r = mcp.cmd("immich.connect", json!({"url": url, "apiKey": IMMICH_KEY}));
    assert_eq!(r["ok"], true, "{r}");
    let account = mcp.cmd("immich.status", json!({}))["accounts"][0]["id"].as_str().unwrap().to_string();
    let svc = mcp.cmd(
        "publish.createService",
        json!({"kind": "immich", "name": "Immich", "settings": {"account": account, "send": "both"}, "export": export}),
    );
    let coll = mcp.cmd("publish.createCollection", json!({"service": svc["id"], "name": "Gate Picks", "addSelected": true}));
    let run = mcp.cmd("publish.run", json!({"collection": coll["collection"]}));
    let sent = published(&run);
    assert_eq!(sent.len(), 2, "{run}");
    {
        let st = immich.lock().unwrap();
        assert_eq!(st.uploads, 4, "two renders and two originals");
        assert_eq!(st.albums.len(), 1);
        assert_eq!(st.albums[0].1, "Gate Picks");
        assert_eq!(st.albums[0].2.len(), 4, "{:?}", st.albums);
        assert_eq!(st.stacks.len(), 2, "each render stacked on its original");
    }

    // 6. the published originals are the photos' Immich links; a rating changed in Immich syncs back
    // publish linked them: no immich.link needed
    assert_eq!(mcp.cmd("immich.links", json!({"id": a}))["state"], "linked");
    let original = mcp.cmd("immich.links", json!({"id": a}))["links"][0]["assetId"].as_str().unwrap().to_string();
    assert!(immich.lock().unwrap().stacks.iter().any(|s| s.get(1) == Some(&original)), "the linked asset is the stacked original");
    let r = mcp.cmd("immich.sync", json!({}));
    assert_eq!(r["ok"], true, "{r}");
    {
        let mut st = immich.lock().unwrap();
        let now = st.tick();
        let asset = st.asset_mut(&original).unwrap();
        assert_eq!(asset["exifInfo"]["rating"], 5, "the first sync sent the catalog rating: {asset}");
        asset["exifInfo"]["rating"] = json!(2);
        asset["updatedAt"] = json!(now);
    }
    let r = mcp.cmd("immich.sync", json!({}));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(mcp.photo(a)["rating"], 2, "the rating edited in Immich came back: {r}");

    // 7. print the selection (the picks) as a contact sheet PDF
    mcp.cmd("library.select", json!({"ids": [a, c]}));
    let pdf = root.join("contact-sheet.pdf");
    let r = mcp.cmd("print.render", json!({"template": "4×5 Contact Sheet", "path": pdf.to_string_lossy(), "dpi": 40}));
    assert!(r["warnings"].as_array().is_none_or(Vec::is_empty), "{r}");
    let bytes = std::fs::read(&pdf).unwrap();
    assert!(bytes.starts_with(b"%PDF-") && bytes.len() > 2000, "{} bytes", bytes.len());
    let images = bytes.windows(b"/Subtype /Image".len()).filter(|w| *w == b"/Subtype /Image").count()
        + bytes.windows(b"/Subtype/Image".len()).filter(|w| *w == b"/Subtype/Image").count();
    assert!(images >= 1, "the contact sheet holds the photos");

    drop(mcp);
    let _ = std::fs::remove_dir_all(&root);
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}
