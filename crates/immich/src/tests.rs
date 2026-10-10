//! The client against a local HTTP server (`std::net::TcpListener`; never the internet): the paths
//! it builds, the headers and bodies it sends, how it reads paging and errors, and what it refuses.
//! Tests may unwrap and index (clippy.toml).
//!
//! A real-server round trip lives here too, but only when `IMMICH_TEST_URL` + `IMMICH_TEST_KEY`
//! name one; CI never has them.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use crate::api::{Client, Limits, SearchQuery};
use crate::error::Error;

pub struct Captured {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Captured {
    fn header(&self, name: &str) -> String {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone()).unwrap_or_default()
    }
}

#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
    /// A `Location` header, for redirect replies.
    pub location: Option<String>,
}

pub fn json(status: u16, body: &str) -> Reply {
    Reply { status, content_type: "application/json", body: body.to_string(), location: None }
}

/// A redirect answer pointing at `location`.
pub fn redirect(status: u16, location: &str) -> Reply {
    Reply { status, content_type: "text/plain", body: String::new(), location: Some(location.to_string()) }
}

/// Serve `replies`, one per connection, and capture what arrived. The thread exits after that many
/// connections, so the join at the end of a test cannot hang.
pub fn start(replies: Vec<Reply>) -> (String, Arc<Mutex<Vec<Captured>>>, std::thread::JoinHandle<()>) {
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
                let reply = q.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
                // captured before the reply is written: the client cannot finish the call
                // before the capture is visible to the test
                s.lock().unwrap_or_else(|e| e.into_inner()).push(c);
                if let Some(r) = reply {
                    write_reply(&mut stream, &r);
                }
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
    let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect();
    let declared = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse::<usize>().ok()).unwrap_or(0);
    let mut body = buf[head_len..].to_vec();
    while body.len() < declared {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some(Captured { method, path, headers, body })
}

fn write_reply(stream: &mut TcpStream, r: &Reply) {
    let location = r.location.as_ref().map(|l| format!("Location: {l}\r\n")).unwrap_or_default();
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 {} X\r\nContent-Type: {}\r\n{location}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            r.status,
            r.content_type,
            r.body.len(),
            r.body
        )
        .as_bytes(),
    );
    let _ = stream.write_all(r.body.as_bytes());
    let _ = stream.flush();
}

fn take(seen: &Arc<Mutex<Vec<Captured>>>) -> Vec<Captured> {
    std::mem::take(&mut *seen.lock().unwrap_or_else(|e| e.into_inner()))
}

fn limits(max_download: u64) -> Limits {
    Limits { max_download, ..Limits::default() }
}

#[test]
fn paths_come_from_the_url_the_user_typed() {
    for (suffix, want) in [
        ("", "/api/server/ping"),
        ("/", "/api/server/ping"),
        ("/api", "/api/server/ping"),
        ("/immich", "/immich/api/server/ping"),
        ("/photos/", "/photos/api/server/ping"),
    ] {
        let (base, seen, h) = start(vec![json(200, r#"{"res":"pong"}"#)]);
        let client = Client::new(&format!("{base}{suffix}"), "k", Limits::default()).unwrap();
        assert_eq!(client.ping().unwrap(), "pong", "typed {base}{suffix}");
        let got = take(&seen);
        h.join().unwrap();
        assert_eq!(got.first().unwrap().path, want, "typed {base}{suffix}");
    }
}

#[test]
fn the_key_goes_in_a_header_and_never_in_the_debug() {
    let (base, seen, h) = start(vec![json(200, r#"{"res":"pong"}"#)]);
    let client = Client::new(&base, "s3cr3t-key", Limits::default()).unwrap();
    client.ping().unwrap();
    let got = take(&seen);
    h.join().unwrap();
    assert_eq!(got.first().unwrap().header("x-api-key"), "s3cr3t-key");
    let d = format!("{client:?}");
    assert!(d.contains("[redacted]") && !d.contains("s3cr3t-key"), "{d}");
}

#[test]
fn a_version_answers_and_a_pong_is_a_string() {
    let (base, _seen, h) = start(vec![json(200, r#"{"res":"pong"}"#), json(200, r#"{"major":3,"minor":2,"patch":0,"externally_visible":true}"#)]);
    let client = Client::new(&base, "k", Limits::default()).unwrap();
    assert_eq!(client.ping().unwrap(), "pong");
    assert_eq!(client.version().unwrap().to_string(), "3.2.0");
    h.join().unwrap();
}

#[test]
fn a_same_host_redirect_is_followed_with_the_key() {
    // a reverse proxy answers 308 and points somewhere else on the same host
    let (base, seen, h) = start(vec![redirect(308, "/immich/api/server/ping"), json(200, r#"{"res":"pong"}"#)]);
    let client = Client::new(&base, "s3cr3t", Limits::default()).unwrap();
    assert_eq!(client.ping().unwrap(), "pong", "the redirect is followed to the answer");
    let got = take(&seen);
    h.join().unwrap();
    assert_eq!(got.len(), 2, "two connections: the redirect, then the target");
    assert_eq!(got[0].path, "/api/server/ping");
    assert_eq!(got[1].path, "/immich/api/server/ping");
    assert_eq!(got[1].header("x-api-key"), "s3cr3t", "the key travels to the same host");
}

#[test]
fn a_redirect_to_another_host_is_refused() {
    // the key must never travel to a host the user did not name
    let (base, seen, h) = start(vec![redirect(302, "https://elsewhere.example/api/server/ping")]);
    let client = Client::new(&base, "s3cr3t", Limits::default()).unwrap();
    let e = client.ping().unwrap_err().to_string();
    assert!(e.contains("different host") && e.contains("elsewhere.example"), "{e}");
    let got = take(&seen);
    h.join().unwrap();
    assert_eq!(got.len(), 1, "nothing was sent to the other host");
}

#[test]
fn a_redirect_loop_stops() {
    // the client gives up after MAX_REDIRECTS follows: five connections, five replies
    let (base, _seen, h) = start(vec![redirect(308, "/api/server/ping"); 5]);
    let client = Client::new(&base, "k", Limits::default()).unwrap();
    let e = client.ping().unwrap_err().to_string();
    assert!(e.contains("keeps redirecting"), "{e}");
    h.join().unwrap();
}

#[test]
fn a_redirect_without_a_location_says_so() {
    let (base, _seen, h) = start(vec![Reply { status: 308, content_type: "text/plain", body: String::new(), location: None }]);
    let client = Client::new(&base, "k", Limits::default()).unwrap();
    let e = client.ping().unwrap_err().to_string();
    assert!(e.contains("redirect without a Location"), "{e}");
    h.join().unwrap();
}

#[test]
fn a_search_sends_our_filters_and_reads_camelcase_assets() {
    // the first answer is the current servers' page object, the second a bare array (older ones)
    let (base, seen, h) = start(vec![
        json(
            200,
            r#"{"assets":{"total":1,"count":1,"items":[{"id":"a1","checksum":"beef","originalFileName":"IMG_1.HEIC","fileCreatedAt":"2026-01-02T03:04:05.000Z","isFavorite":true,"rating":4,"newThing":{"x":1}}],"nextPage":null}}"#,
        ),
        json(200, r#"{"assets":[{"id":"a1","checksum":"beef","originalFileName":"IMG_1.HEIC","isFavorite":false}],"count":1}"#),
    ]);
    let client = Client::new(&base, "k", Limits::default()).unwrap();
    let q = SearchQuery { album_ids: vec!["alb".into()], is_favorite: Some(true), page: 2, page_size: 1, ..Default::default() };
    let page = client.search(&q).unwrap();
    let got = take(&seen);
    let sent: serde_json::Value = serde_json::from_slice(&got.first().unwrap().body).unwrap();
    // the search DTO is the body itself — a `query` wrapper is silently ignored by the server
    assert!(sent.get("query").is_none(), "{sent}");
    assert_eq!(sent["albumIds"][0], "alb");
    assert_eq!(sent["isFavorite"], true);
    assert_eq!(sent["page"], 2);
    assert_eq!(sent["size"], 1);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items.first().unwrap().original_file_name, "IMG_1.HEIC");
    assert!(page.items.first().unwrap().is_favorite);
    assert_eq!(page.items.first().unwrap().checksum, "beef");
    assert!(page.maybe_more, "a full page asks for the next one");
    let short = client.search(&SearchQuery { page_size: 100, ..Default::default() }).unwrap();
    assert!(!short.maybe_more, "a short page means the end");
    assert_eq!(short.page, 1);
    assert_eq!(short.items.first().unwrap().file_created_at, None, "the bare-array answer is read too");
    h.join().unwrap();
}

#[test]
fn a_next_page_cursor_means_more_pages() {
    // a current server says there is another page even when the page came back short
    let (base, _seen, h) = start(vec![json(
        200,
        r#"{"assets":{"total":9,"count":1,"items":[{"id":"a1","checksum":"beef","originalFileName":"IMG_1.HEIC"}],"nextPage":3}}"#,
    )]);
    let client = Client::new(&base, "k", Limits::default()).unwrap();
    let page = client.search(&SearchQuery { page_size: 60, ..Default::default() }).unwrap();
    assert_eq!(page.items.len(), 1);
    assert!(page.maybe_more, "the server says a page follows");
    h.join().unwrap();
}

#[test]
fn server_errors_become_actionable_errors() {
    let (base, _seen, h) = start(vec![
        json(500, r#"{"message":"machine learning is wedged"}"#),
        json(401, r#"{"message":"Invalid API key"}"#),
        json(404, "nothing here"),
        Reply { status: 200, content_type: "application/json", body: "this is not json".into(), location: None },
        Reply { status: 503, content_type: "text/html", body: "<html><body><h1>502 Bad Gateway</h1></body></html>".into(), location: None },
    ]);
    let client = Client::new(&base, "k", Limits::default()).unwrap();
    let e = client.ping().unwrap_err();
    assert_eq!(e, Error::Api { status: 500, message: "machine learning is wedged".into() });
    assert_eq!(client.ping().unwrap_err(), Error::Unauthorized);
    assert!(matches!(client.album_assets("nope").unwrap_err(), Error::NotFound(_)));
    assert!(matches!(client.ping().unwrap_err(), Error::Api { status: 0, .. }));
    let e = client.ping().unwrap_err();
    match &e {
        Error::Api { status: 503, message } => assert!(!message.contains('<'), "{message}"),
        other => panic!("expected a 503, got {other:?}"),
    }
    h.join().unwrap();
}

#[test]
fn a_download_over_the_cap_is_refused_before_it_runs() {
    let (base, _seen, h) = start(vec![json(200, &"x".repeat(4000))]);
    let client = Client::new(&base, "k", limits(100)).unwrap();
    let mut sink = Vec::new();
    let e = client.download_original("a1", &mut sink, |_| {}).unwrap_err();
    assert!(matches!(e, Error::Limit(_)), "{e:?}");
    assert!(sink.is_empty());
    h.join().unwrap();
}

#[test]
fn a_download_reports_progress_and_returns_its_bytes() {
    let (base, _seen, h) = start(vec![json(200, &"y".repeat(300_000))]);
    let client = Client::new(&base, "k", Limits::default()).unwrap();
    let mut sink = Vec::new();
    let mut last = 0;
    let n = client.download_original("a1", &mut sink, |done| last = done).unwrap();
    assert_eq!(n, 300_000);
    assert_eq!(sink.len(), 300_000);
    assert_eq!(last, 300_000);
    h.join().unwrap();
}

#[test]
fn an_upload_is_multipart_with_file_sidecar_and_options() {
    let (base, seen, h) = start(vec![json(200, r#"{"id":"new1","checksum":"cafe","originalFileName":"out.jpg","isFavorite":false}"#)]);
    let client = Client::new(&base, "k", Limits::default()).unwrap();
    let asset =
        client.upload_asset(b"jpegbytes", "export/out.jpg", "image/jpeg", Some(b"<x:xmpmeta/>"), Some("2026-01-02T03:04:05.000Z"), true).unwrap();
    let got = take(&seen);
    h.join().unwrap();
    assert_eq!(asset.id, "new1");
    let req = got.first().unwrap();
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/api/assets");
    let ct = req.header("content-type");
    assert!(ct.starts_with("multipart/form-data; boundary="), "{ct}");
    let body = String::from_utf8_lossy(&req.body).into_owned();
    assert!(body.contains("name=\"file\"; filename=\"export_out.jpg\""), "{body}");
    assert!(body.contains("jpegbytes"));
    assert!(body.contains("name=\"sidecarData\""));
    assert!(body.contains("<x:xmpmeta/>"));
    assert!(body.contains("name=\"options\""));
    assert!(body.contains("\"fileCreatedAt\":\"2026-01-02T03:04:05.000Z\""));
    assert!(body.contains("\"isFavorite\":true"));
}

#[test]
fn album_and_stack_calls_put_their_json() {
    let (base, seen, h) = start(vec![
        json(201, r#"{"id":"alb1","albumName":"LightCraft","assetCount":0,"description":null}"#),
        json(200, "{}"),
        json(201, r#"{"id":"stack1","primaryAssetId":"p1"}"#),
        json(200, r#"[{"id":"alb1","albumName":"LightCraft","assetCount":3},{"id":"alb2","albumName":"Other"}]"#),
    ]);
    let client = Client::new(&base, "k", Limits::default()).unwrap();
    let album = client.create_album("LightCraft").unwrap();
    assert_eq!((album.id.as_str(), album.album_name.as_str(), album.asset_count), ("alb1", "LightCraft", 0));
    client.add_to_album("alb 1", &["x1".into(), "x2".into()]).unwrap();
    let stack = client.create_stack("p1", &["c1".into()]).unwrap();
    assert_eq!(stack, "stack1");
    let albums = client.albums().unwrap();
    assert_eq!(albums.len(), 2);
    let got = take(&seen);
    h.join().unwrap();
    let paths: Vec<&str> = got.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, vec!["/api/albums", "/api/albums/alb%201/assets", "/api/stacks", "/api/albums"]);
    assert_eq!(got.get(1).unwrap().method, "PUT");
    assert!(String::from_utf8_lossy(&got.get(1).unwrap().body).contains("\"x1\""));
}

#[test]
fn an_unusable_configuration_is_an_error_not_a_panic() {
    assert!(Client::new("ftp://example.com", "k", Limits::default()).is_err());
    assert!(Client::new("http://example.com", "", Limits::default()).is_err());
    assert!(Client::new("http://example.com", "bad\nkey", Limits::default()).is_err());
    // a host that is not listening is an error, and it comes back
    let client = Client::new("http://127.0.0.1:1", "k", Limits::default()).unwrap();
    assert!(matches!(client.ping(), Err(Error::Transport(_))));
}

#[test]
fn the_extra_filters_travel_in_the_search_body() {
    let (base, seen, h) = start(vec![json(200, r#"{"assets":[]}"#)]);
    let client = Client::new(&base, "k", Limits::default()).unwrap();
    let q = SearchQuery {
        rating: Some(4),
        checksum: Some("abcd".into()),
        created_after: Some("2026-01-01T00:00:00.000Z".into()),
        created_before: Some("2026-02-01T00:00:00.000Z".into()),
        asset_type: Some("IMAGE".into()),
        page_size: 20,
        ..Default::default()
    };
    client.search(&q).unwrap();
    let got = take(&seen);
    h.join().unwrap();
    let sent: serde_json::Value = serde_json::from_slice(&got.first().unwrap().body).unwrap();
    assert_eq!(sent["rating"], 4);
    assert_eq!(sent["checksum"], "abcd");
    assert_eq!(sent["createdAfter"], "2026-01-01T00:00:00.000Z");
    assert_eq!(sent["createdBefore"], "2026-02-01T00:00:00.000Z");
    assert_eq!(sent["type"], "IMAGE");
}

#[test]
fn an_asset_meta_and_a_thumbnail_use_the_asset_endpoints() {
    let (base, seen, h) = start(vec![
        json(200, r#"{"id":"a1","checksum":"beef","originalFileName":"IMG_2.HEIC","rating":3,"isFavorite":true}"#),
        Reply { status: 200, content_type: "image/jpeg", body: "j".repeat(1000), location: None },
    ]);
    let client = Client::new(&base, "k", Limits::default()).unwrap();
    let a = client.asset("a1").unwrap();
    assert_eq!(a.original_file_name, "IMG_2.HEIC");
    assert_eq!(a.rating, Some(3));
    let mut sink = Vec::new();
    let n = client.thumbnail("a 1", "preview", &mut sink, |_| {}).unwrap();
    let got = take(&seen);
    h.join().unwrap();
    assert_eq!((n, sink.len()), (1000, 1000));
    assert_eq!(got.first().unwrap().path, "/api/assets/a1");
    assert_eq!(got.get(1).unwrap().path, "/api/assets/a%201/thumbnail?size=preview");
}

#[test]
fn a_thumbnail_over_the_cap_is_refused() {
    let (base, _seen, h) = start(vec![Reply { status: 200, content_type: "image/jpeg", body: "j".repeat(5000), location: None }]);
    let client = Client::new(&base, "k", Limits { max_thumb: 100, ..Limits::default() }).unwrap();
    let mut sink = Vec::new();
    let e = client.thumbnail("a1", "preview", &mut sink, |_| {}).unwrap_err();
    assert!(matches!(e, Error::Limit(_)), "{e:?}");
    h.join().unwrap();
}

#[test]
fn a_real_server_round_trip_when_one_is_configured() {
    let (Ok(url), Ok(key)) = (std::env::var("IMMICH_TEST_URL"), std::env::var("IMMICH_TEST_KEY")) else {
        eprintln!("IMMICH_TEST_URL/IMMICH_TEST_KEY not set: skipping the live round trip");
        return;
    };
    let client = Client::new(&url, &key, Limits::default()).unwrap();
    let v = client.version().unwrap();
    assert!(v.major >= 1, "unexpected server version {v}");
    let albums = client.albums().unwrap();
    let made = client.create_album(&format!("lightcraft-probe-{}", std::process::id())).unwrap();
    let created = client.upload_asset(b"\xff\xd8\xff\xd9", &format!("probe-{}.jpg", std::process::id()), "image/jpeg", None, None, false).unwrap();
    client.add_to_album(&made.id, std::slice::from_ref(&created.id)).unwrap();
    assert!(!created.checksum.is_empty());
    let mut sink = Vec::new();
    client.download_original(&created.id, &mut sink, |_| {}).unwrap();
    assert_eq!(sink, b"\xff\xd8\xff\xd9");
    eprintln!("live server {v} answered: {} albums, asset {}", albums.len(), created.id);
}
