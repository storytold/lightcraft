//! Immich import: server configuration (the key is never echoed), paged browse and a
//! download-then-import round against a local mock Immich server (`std::net`; never the internet).

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::Session;

struct Reply {
    status: u16,
    ctype: &'static str,
    body: Vec<u8>,
}

fn reply_json(status: u16, body: &str) -> Reply {
    Reply { status, ctype: "application/json", body: body.as_bytes().to_vec() }
}

fn reply_bytes(status: u16, ctype: &'static str, body: Vec<u8>) -> Reply {
    Reply { status, ctype, body }
}

struct Captured {
    method: String,
    path: String,
    body: Vec<u8>,
}

/// Serve `replies`, one per connection, capturing each request. The thread exits after that many
/// connections, so the join at the end of a test cannot hang.
fn start(replies: Vec<Reply>) -> (String, Arc<Mutex<Vec<Captured>>>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let total = replies.len();
    let queue = Arc::new(Mutex::new(VecDeque::from(replies)));
    let seen: Arc<Mutex<Vec<Captured>>> = Arc::new(Mutex::new(Vec::new()));
    let (q, s) = (queue.clone(), seen.clone());
    let handle = std::thread::spawn(move || {
        let mut done = 0;
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            if let Some(c) = read_request(&mut stream) {
                if let Some(r) = q.lock().unwrap_or_else(|e| e.into_inner()).pop_front() {
                    write_reply(&mut stream, &r);
                }
                s.lock().unwrap_or_else(|e| e.into_inner()).push(c);
            }
            done += 1;
            if done >= total {
                break;
            }
        }
    });
    (base, seen, handle)
}

fn read_request(stream: &mut TcpStream) -> Option<Captured> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 64 * 1024 {
            return None;
        }
    }
    let head_len = buf.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    let head = String::from_utf8_lossy(&buf[..head_len]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or_default().split_whitespace();
    let method = first.next().unwrap_or_default().to_string();
    let path = first.next().unwrap_or_default().to_string();
    let declared = head
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buf[head_len..].to_vec();
    while body.len() < declared {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some(Captured { method, path, body })
}

fn write_reply(stream: &mut TcpStream, r: &Reply) {
    let head = format!("HTTP/1.1 {} X\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", r.status, r.ctype, r.body.len());
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&r.body);
    let _ = stream.flush();
}

fn take(seen: &Arc<Mutex<Vec<Captured>>>) -> Vec<Captured> {
    std::mem::take(&mut *seen.lock().unwrap_or_else(|e| e.into_inner()))
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-immich-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A small procedural PNG (distinct per seed), as the import pipeline expects to receive.
fn png(seed: u8) -> Vec<u8> {
    let (w, h) = (48usize, 32usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 5) as u8, (i / w * 7) as u8, seed, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap()
}

fn add_server(s: &mut Session, base: &str) {
    s.execute("immich.servers", &json!({"add": {"url": base, "apiKey": "k", "name": "Home"}})).unwrap();
}

#[test]
fn servers_are_configured_without_ever_echoing_the_key() {
    let lib = temp_dir("srv");
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let r = s.execute("immich.servers", &json!({"add": {"url": "http://127.0.0.1:9", "apiKey": "s3cr3t", "name": "Home"}})).unwrap();
    assert_eq!(r["servers"][0]["name"], "Home", "{r}");
    assert_eq!(r["servers"][0]["insecure"], true, "http shows as not encrypted");
    assert!(!r.to_string().contains("s3cr3t"), "{r}");
    let prefs = std::fs::read_to_string(lib.join("prefs.json")).unwrap();
    assert!(prefs.contains("s3cr3t"), "stored in prefs.json (plaintext in v1, labelled so)");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(lib.join("prefs.json")).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // re-adding the same url replaces the entry (fixing a key shouldn't pile up servers)
    let r = s.execute("immich.servers", &json!({"add": {"url": "http://127.0.0.1:9", "apiKey": "new", "name": "Home"}})).unwrap();
    assert_eq!(r["servers"].as_array().map(Vec::len), Some(1), "{r}");
    let r = s.execute("immich.servers", &json!({"remove": {"name": "home"}})).unwrap();
    assert_eq!(r["servers"].as_array().map(Vec::len), Some(0), "{r}");
    let e = s.execute("immich.browse", &json!({"server": "Nope"})).unwrap_err().to_string();
    assert!(e.contains("no Immich server"), "{e}");
    let _ = std::fs::remove_dir_all(&lib);
}

#[test]
fn a_passing_test_marks_the_server_verified_and_readding_resets_it() {
    let lib = temp_dir("verify");
    let (base, _seen, _h) = start(vec![reply_json(200, r#"{"res":"pong"}"#), reply_json(200, r#"{"major":1,"minor":135,"patch":3}"#)]);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    add_server(&mut s, &base);
    assert!(!s.immich_servers[0].verified, "a fresh server is not verified");
    let r = s.execute("immich.test", &json!({})).unwrap();
    assert_eq!(r["version"], "1.135.3", "{r}");
    assert!(s.immich_servers[0].verified, "a passing test verifies the server");
    assert!(std::fs::read_to_string(lib.join("prefs.json")).unwrap().contains("\"verified\": true"), "the flag is persisted");
    // re-adding the same url replaces the entry — a changed key or URL needs a new test
    let r = s.execute("immich.servers", &json!({"add": {"url": base, "apiKey": "k2", "name": "Home"}})).unwrap();
    assert_eq!(r["servers"].as_array().map(Vec::len), Some(1), "{r}");
    assert!(!s.immich_servers[0].verified, "a re-added server starts unverified");
    let _ = std::fs::remove_dir_all(&lib);
}

#[test]
fn verify_records_a_passed_test_without_touching_the_network() {
    // nothing listens on port 9 — `verify` only records a result, it must not dial anything
    let mut s = Session::new().with_fs();
    add_server(&mut s, "http://127.0.0.1:9");
    let r = s.execute("immich.verify", &json!({"server": "Home"})).unwrap();
    assert_eq!(r["servers"][0]["verified"], true, "{r}");
    assert!(s.immich_servers[0].verified);
    let e = s.execute("immich.verify", &json!({"server": "Nope"})).unwrap_err().to_string();
    assert!(e.contains("unknown Immich server"), "{e}");
    let e = s.execute("immich.verify", &json!({})).unwrap_err().to_string();
    assert!(e.contains("`server`"), "{e}");
}

#[test]
fn browse_resolves_the_album_by_name_and_maps_the_page() {
    let (base, seen, h) = start(vec![
        reply_json(200, r#"[{"id":"al1","albumName":"Trip","assetCount":2}]"#),
        reply_json(
            200,
            r#"{"assets":{"total":1,"count":1,"items":[{"id":"a1","checksum":"cafe","originalFileName":"IMG_1.JPG","fileCreatedAt":"2026-01-02T03:04:05.000Z","isFavorite":true,"rating":4,"other":1}],"nextPage":null}}"#,
        ),
    ]);
    let mut s = Session::new().with_fs();
    add_server(&mut s, &base);
    let r = s.execute("immich.browse", &json!({"server": "Home", "album": "trip", "isFavorite": true, "rating": 4, "pageSize": 1})).unwrap();
    let got = take(&seen);
    h.join().unwrap();
    assert_eq!(r["assets"][0]["id"], "a1", "{r}");
    assert_eq!(r["assets"][0]["fileName"], "IMG_1.JPG", "{r}");
    assert_eq!(r["assets"][0]["rating"], 4, "{r}");
    assert_eq!(r["maybeMore"], true, "a full page means another one may exist");
    assert_eq!(got.first().map(|c| (c.method.as_str(), c.path.as_str())), Some(("GET", "/api/albums")));
    let search = got.iter().find(|c| c.path == "/api/search/metadata").expect("a search ran");
    let sent: Value = serde_json::from_slice(&search.body).unwrap();
    assert!(sent.get("query").is_none(), "the search DTO is the body itself: {sent}");
    assert_eq!(sent["albumIds"][0], "al1");
    assert_eq!(sent["isFavorite"], true);
    assert_eq!(sent["rating"], 4);
    assert_eq!(sent["size"], 1);
}

#[test]
fn import_downloads_then_runs_the_library_import_pipeline() {
    let lib = temp_dir("imp");
    let (base, seen, h) = start(vec![
        reply_json(200, r#"{"id":"a1","checksum":"c1","originalFileName":"IMG_1.JPG"}"#),
        reply_bytes(200, "image/png", png(1)),
        reply_json(200, r#"{"id":"a2","checksum":"c2","originalFileName":"sub/IMG 2.JPG"}"#),
        reply_bytes(200, "image/png", png(2)),
    ]);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    add_server(&mut s, &base);
    let r = s.execute("immich.import", &json!({"server": "Home", "ids": ["a1", "a2"]})).unwrap();
    let got = take(&seen);
    h.join().unwrap();
    assert_eq!(r["report"]["imported"].as_array().map(Vec::len), Some(2), "{r}");
    assert_eq!(r["failed"].as_array().map(Vec::len), Some(0), "{r}");
    assert_eq!(s.catalog.len(), 2);
    // the originals landed inside the library (copy mode is the default)…
    let originals = lib.join("Originals");
    let paths: Vec<String> = s
        .catalog
        .photos()
        .map(|ph| match &ph.source {
            lightcraft_catalog::Source::File { path } => path.clone(),
            _ => String::from("<not a file>"),
        })
        .collect();
    for p in &paths {
        assert!(std::path::Path::new(p).starts_with(&originals) && std::path::Path::new(p).exists(), "{p} outside {originals:?} or missing");
    }
    // …and nothing of the import was left lying around in the library itself
    let stray = std::fs::read_dir(&lib)
        .map(|rd| rd.filter_map(Result::ok).filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("jpg"))).count())
        .unwrap_or(0);
    assert_eq!(stray, 0, "no stray downloads in the library folder");
    // one undo step for the whole import, like any other import
    s.execute("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.catalog.len(), 0);
    // every id was looked up and downloaded, by its own path
    let paths: Vec<&str> = got.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, vec!["/api/assets/a1", "/api/assets/a1/original", "/api/assets/a2", "/api/assets/a2/original"]);
    let _ = std::fs::remove_dir_all(&lib);
}

#[test]
fn import_needs_ids_and_names_what_it_could_not_fetch() {
    let lib = temp_dir("impfail");
    let (base, _seen, _h) = start(vec![reply_json(500, r#"{"message":"volume full"}"#)]);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    add_server(&mut s, &base);
    let e = s.execute("immich.import", &json!({"server": "Home", "ids": []})).unwrap_err().to_string();
    assert!(e.contains("ids"), "{e}");
    // nothing can be fetched: an actionable error, no partial import
    let e = s.execute("immich.import", &json!({"server": "Home", "ids": ["a1"]})).unwrap_err().to_string();
    assert!(e.contains("a1"), "{e}");
    assert_eq!(s.catalog.len(), 0);
    let _ = std::fs::remove_dir_all(&lib);
}
