//! Tests against an in-process HTTP server replaying responses recorded from a v3.3.1 server
//! (`tests/fixtures/*.json`, recorded with `cargo xtask immich up && cargo xtask immich seed`).
#![cfg(not(target_arch = "wasm32"))]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lcc_catalog::{Catalog, Op, Photo, PhotoId, Source, SyncState};
use lcc_credentials::Secret;

use crate::client::{self, Client, ServerOptions};
use crate::extlib::{self, PathMap};
use crate::link::{self, Index, MatchKind};
use crate::types::*;
use crate::{ImmichError, mapping};

const VERSION: &str = include_str!("../tests/fixtures/version.json");
const ME: &str = include_str!("../tests/fixtures/users-me.json");
const KEY_ME: &str = include_str!("../tests/fixtures/api-keys-me.json");
const PAGE1: &str = include_str!("../tests/fixtures/search-page1.json");
const PAGE2: &str = include_str!("../tests/fixtures/search-page2.json");
const ALBUMS: &str = include_str!("../tests/fixtures/albums.json");
const AUTH_STATUS: &str = include_str!("../tests/fixtures/auth-status.json");

const KEY: &str = "test-key-0123456789";

#[derive(Debug, Clone)]
struct Req {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Req {
    fn header(&self, n: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.as_str())
    }
}

fn read_req(s: impl Read) -> Option<Req> {
    let mut r = BufReader::new(s);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let mut it = line.split_whitespace();
    let (method, target) = (it.next()?.to_string(), it.next()?.to_string());
    let mut headers = Vec::new();
    loop {
        let mut l = String::new();
        r.read_line(&mut l).ok()?;
        let l = l.trim_end();
        if l.is_empty() {
            break;
        }
        let (k, v) = l.split_once(':')?;
        headers.push((k.trim().to_string(), v.trim().to_string()));
    }
    let len: usize = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    let mut body = vec![0; len];
    r.read_exact(&mut body).ok()?;
    Some(Req { method, target, headers, body })
}

fn response(status: &str, body: &[u8]) -> Vec<u8> {
    let mut out =
        format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out
}

/// A server answering every connection with `handler` until the test ends.
fn serve(handler: impl Fn(&Req) -> Vec<u8> + Send + Sync + 'static) -> (String, Arc<Mutex<Vec<Req>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(mut s) = s else { return };
            let Some(req) = read_req(&mut s) else { continue };
            let out = handler(&req);
            seen2.lock().unwrap().push(req);
            let _ = s.write_all(&out);
        }
    });
    (format!("http://127.0.0.1:{port}"), seen)
}

/// The recorded server: fixtures by path, 401 without the right key.
fn recorded(req: &Req) -> Vec<u8> {
    let path = req.target.split('?').next().unwrap_or_default();
    let public = matches!(path, "/api/server/version" | "/api/server/ping");
    if !public && req.header("x-api-key") != Some(KEY) {
        return response("401 Unauthorized", br#"{"message":"Invalid API key","statusCode":401}"#);
    }
    match (req.method.as_str(), path) {
        ("GET", "/api/server/ping") => response("200 OK", br#"{"res":"pong"}"#),
        ("GET", "/api/server/version") => response("200 OK", VERSION.as_bytes()),
        ("GET", "/api/users/me") => response("200 OK", ME.as_bytes()),
        ("GET", "/api/api-keys/me") => response("200 OK", KEY_ME.as_bytes()),
        ("GET", "/api/albums") => response("200 OK", ALBUMS.as_bytes()),
        ("POST", "/api/search/metadata") => {
            let q: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            match q["page"].as_u64() {
                Some(1) | None => response("200 OK", PAGE1.as_bytes()),
                Some(2) => response("200 OK", PAGE2.as_bytes()),
                _ => response("200 OK", br#"{"albums":{"total":0,"count":0,"items":[]},"assets":{"total":0,"count":0,"items":[],"nextPage":null}}"#),
            }
        }
        ("GET", "/api/auth/status") => response("200 OK", AUTH_STATUS.as_bytes()),
        ("POST", "/api/auth/session/unlock") => {
            // a recorded 204 has an empty body; the right PIN or password unlocks, anything else is refused
            let b: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            if b["pinCode"] == "2468" || b["password"] == "square lamp house" {
                response("204 No Content", b"")
            } else {
                response("401 Unauthorized", br#"{"message":"Wrong password or PIN code","statusCode":401,"error":"Unauthorized"}"#)
            }
        }
        _ => response("404 Not Found", br#"{"message":"Not found"}"#),
    }
}

fn opts() -> ServerOptions {
    ServerOptions { retries: 2, backoff: Duration::from_millis(5), stall_timeout: Some(Duration::from_secs(5)), ..ServerOptions::default() }
}

fn client(url: &str, key: &str) -> Client {
    Client::new(url, Secret::new(key), &opts()).unwrap()
}

#[test]
fn connect_reads_version_user_and_permissions() {
    let (url, seen) = serve(recorded);
    let c = client(&url, KEY);
    c.ping().unwrap();
    let st = c.status().unwrap();
    assert_eq!(st.version, ServerVersion { major: 3, minor: 3, patch: 1 });
    assert_eq!(st.user.email, "admin@example.invalid");
    assert!(st.user.is_admin);
    assert_eq!(st.permissions, Some(vec!["all".to_string()]));
    assert!(client::missing_permissions(&["all".into()]).is_empty());
    assert_eq!(
        client::missing_permissions(&["asset.read".into()]).len(),
        8,
        "import, external libraries, sync, people, stacks, edit versions, album sync, sidecars"
    );
    // the key goes in its header, never in the URL
    let seen = seen.lock().unwrap();
    assert!(seen.iter().all(|r| !r.target.contains(KEY)));
    assert!(seen.iter().any(|r| r.header("x-api-key") == Some(KEY)));
}

#[test]
fn auth_status_reads_elevation_and_unlock_secrets() {
    let (url, _) = serve(recorded);
    let c = client(&url, KEY);
    let st = c.auth_status().unwrap();
    assert!(!st.is_elevated, "the recorded server's session is locked");
    assert!(st.password && st.pin_code);
    assert_eq!(st.pin_expires_at, None);
    // an elevated session may carry an end of its elevation (parsed, not required)
    let (url, _) = serve(|req| {
        let path = req.target.split('?').next().unwrap_or_default();
        match (req.method.as_str(), path) {
            ("GET", "/api/auth/status") => {
                response("200 OK", br#"{"isElevated": true, "expiresAt": "2026-11-10T18:00:00.000Z", "password": true, "pinCode": false}"#)
            }
            _ => response("404 Not Found", br#"{}"#),
        }
    });
    let st = client(&url, KEY).auth_status().unwrap();
    assert!(st.is_elevated);
    assert_eq!(st.expires_at.as_deref(), Some("2026-11-10T18:00:00.000Z"));
    assert!(!st.pin_code);
}

#[test]
fn unlock_sends_the_secret_in_the_body_and_names_a_refusal() {
    let (url, seen) = serve(recorded);
    let c = client(&url, KEY);
    // the right PIN: 204 with an empty body
    c.unlock_session(&SessionUnlock { pin_code: Some("2468".into()), password: None }).unwrap();
    // the right password, sent as such
    c.unlock_session(&SessionUnlock { pin_code: None, password: Some("square lamp house".into()) }).unwrap();
    // a wrong secret is refused and the error says so, without the secret
    let e = c.unlock_session(&SessionUnlock { pin_code: Some("9999".into()), password: None }).unwrap_err();
    assert!(!e.retryable());
    assert!(e.to_string().contains("refused"), "{e}");
    assert!(!e.to_string().contains("9999"), "{e}");
    // the secret travels in the request body, never in the URL or a query
    let seen = seen.lock().unwrap();
    let unlocks: Vec<&Req> = seen.iter().filter(|r| r.target == "/api/auth/session/unlock").collect();
    assert_eq!(unlocks.len(), 3);
    assert!(unlocks.iter().all(|r| !r.target.contains("2468") && !r.target.contains("square")));
    let first: serde_json::Value = serde_json::from_slice(&unlocks[0].body).unwrap();
    assert_eq!(first, serde_json::json!({"pinCode": "2468"}), "only the chosen secret is sent");
    let second: serde_json::Value = serde_json::from_slice(&unlocks[1].body).unwrap();
    assert_eq!(second, serde_json::json!({"password": "square lamp house"}));
    assert!(unlocks.iter().all(|r| r.header("x-api-key") == Some(KEY)), "unlocking needs the key's session");
}

#[test]
fn an_upload_with_a_sidecar_sends_the_file_parts_first() {
    // Immich v3.3.1 rejects a multipart upload whose text fields come before `assetData` when a
    // `sidecarData` is present ("Unsupported file type <name>"), so the order is part of the API.
    let (url, seen) = serve(|_| response("200 OK", br#"{"id":"asset-1","status":"created"}"#));
    let c = client(&url, KEY);
    c.upload(&crate::client::NewAsset {
        data: crate::client::UploadData::Bytes(b"png"),
        file_name: "shot.png",
        mime: "image/png",
        device_asset_id: "device-1",
        device_id: "app",
        created: "2024-01-01T12:00:00.000Z",
        favorite: false,
        sidecar: Some(b"<x:xmpmeta/>"),
    })
    .unwrap();
    let seen = seen.lock().unwrap();
    let body = String::from_utf8_lossy(&seen[0].body);
    let asset = body.find("name=\"assetData\"").unwrap_or(usize::MAX);
    let sidecar = body.find("name=\"sidecarData\"").unwrap_or(usize::MAX);
    let text = body.find("name=\"deviceAssetId\"").unwrap_or(0);
    assert!(asset < text, "assetData comes before the text fields:\n{body}");
    assert!(sidecar < text, "sidecarData comes before the text fields:\n{body}");
}

#[test]
fn copying_a_sidecar_sends_only_the_sidecar_flag() {
    let (url, seen) = serve(|_| response("204 No Content", b""));
    let c = client(&url, KEY);
    c.copy_sidecar("source-1", "target-2").unwrap();
    c.delete_assets(&["temp-3".into()], true).unwrap();
    let seen = seen.lock().unwrap();
    let copy = seen.iter().find(|r| r.method == "PUT").unwrap();
    assert_eq!(copy.target, "/api/assets/copy");
    let b: serde_json::Value = serde_json::from_slice(&copy.body).unwrap();
    assert_eq!(b["sourceId"], "source-1");
    assert_eq!(b["targetId"], "target-2");
    assert_eq!(b["sidecar"], true);
    for k in ["albums", "favorite", "sharedLinks", "stack"] {
        assert_eq!(b[k], false, "only the sidecar is copied ({k})");
    }
    let delete = seen.iter().find(|r| r.method == "DELETE").unwrap();
    let b: serde_json::Value = serde_json::from_slice(&delete.body).unwrap();
    assert_eq!(b["ids"], serde_json::json!(["temp-3"]));
    assert_eq!(b["force"], true, "the temporary asset is removed at once");
}

#[test]
fn asset_files_list_and_serve_the_sidecar_within_the_cap() {
    const XML: &[u8] =
        b"<?xpacket?><x:xmpmeta xmlns:x='adobe:ns:meta/'><rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'/></x:xmpmeta>";
    let (url, seen) = serve(move |req| {
        let path = req.target.split('?').next().unwrap_or_default();
        if req.header("x-api-key") != Some(KEY) {
            return response("401 Unauthorized", br#"{"message":"Invalid API key"}"#);
        }
        match (req.method.as_str(), path) {
            ("GET", "/api/asset-files") => {
                response("200 OK", br#"[{"id":"file-1","type":"preview","path":"/p"},{"id":"file-2","type":"sidecar","path":"/x.jpg.xmp"}]"#)
            }
            ("GET", "/api/asset-files/file-2/download") => {
                let mut out =
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/xml\r\nConnection: close\r\n\r\n", XML.len())
                        .into_bytes();
                out.extend_from_slice(XML);
                out
            }
            // a sidecar larger than the cap: refused from its Content-Length alone
            ("GET", "/api/asset-files/big/download") => {
                b"HTTP/1.1 200 OK\r\nContent-Length: 16777217\r\nContent-Type: application/xml\r\nConnection: close\r\n\r\n".to_vec()
            }
            _ => response("404 Not Found", br#"{}"#),
        }
    });
    let c = client(&url, KEY);
    let files = c.asset_files("aaaa-bbbb", Some("sidecar")).unwrap();
    assert_eq!(files.len(), 2);
    assert!(!files[0].is_sidecar());
    assert_eq!(c.asset_sidecar("aaaa-bbbb").unwrap().map(|f| f.id), Some("file-2".into()));
    assert_eq!(c.download_asset_file("file-2").unwrap(), XML);
    assert!(matches!(c.download_asset_file("big"), Err(ImmichError::Protocol(_))), "the cap is enforced");
    // ids and kinds that could reach another path or query are refused
    assert!(c.asset_sidecar("../x").is_err());
    assert!(c.asset_files("aaaa", Some("sidecar&x=1")).is_err());
    let seen = seen.lock().unwrap();
    assert!(seen.iter().any(|r| r.target == "/api/asset-files?assetId=aaaa-bbbb&type=sidecar"));
}

#[test]
fn the_key_never_shows_in_debug_output_or_errors() {
    let c = client("http://127.0.0.1:9", KEY);
    assert!(!format!("{c:?}").contains(KEY));
    let e = c.me().unwrap_err();
    assert!(!e.to_string().contains(KEY));
}

#[test]
fn a_bad_key_is_a_clear_error() {
    let (url, _) = serve(recorded);
    let e = client(&url, "wrong").status().unwrap_err();
    assert_eq!(e, ImmichError::BadKey);
    assert!(e.to_string().contains("API key"), "{e}");
    assert!(!e.retryable());
}

#[test]
fn old_servers_are_refused_with_an_upgrade_message() {
    let (url, _) = serve(|_| response("200 OK", br#"{"major":1,"minor":106,"patch":4}"#));
    let e = client(&url, KEY).check_version().unwrap_err();
    assert_eq!(e, ImmichError::TooOld(ServerVersion { major: 1, minor: 106, patch: 4 }));
    assert!(e.to_string().contains("upgrade"), "{e}");
}

#[test]
fn an_offline_server_is_retryable_and_never_panics() {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let e = client(&format!("http://127.0.0.1:{port}"), KEY).version().unwrap_err();
    assert!(matches!(e, ImmichError::Offline(_)), "{e:?}");
    assert!(e.retryable());
    assert!(e.to_string().contains("retry"), "{e}");
}

#[test]
fn server_errors_are_retried_then_reported() {
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    let (url, _) = serve(move |r| if n2.fetch_add(1, Ordering::SeqCst) < 2 { response("503 Service Unavailable", b"{}") } else { recorded(r) });
    assert_eq!(client(&url, KEY).version().unwrap().major, 3);
    assert_eq!(n.load(Ordering::SeqCst), 3);

    let (url, seen) = serve(|_| response("500 Internal Server Error", b"oops"));
    let e = client(&url, KEY).version().unwrap_err();
    assert_eq!(e, ImmichError::Server(500));
    assert_eq!(seen.lock().unwrap().len(), 3, "first try + 2 retries");
}

#[test]
fn garbage_answers_are_protocol_errors() {
    let (url, _) = serve(|_| response("200 OK", b"<html>not immich</html>"));
    assert!(matches!(client(&url, KEY).version(), Err(ImmichError::Protocol(_))));
    let (url, _) = serve(|_| response("200 OK", br#"{"assets":{"items":[{"id":"x","exifInfo":{"rating":"five"}}]}}"#));
    assert!(matches!(client(&url, KEY).search(&MetadataSearch::default()), Err(ImmichError::Protocol(_))));
}

#[test]
fn tls_to_a_plain_server_fails_cleanly() {
    let (url, _) = serve(recorded);
    let e = client(&url.replace("http://", "https://"), KEY).version().unwrap_err();
    assert!(matches!(e, ImmichError::Tls(_) | ImmichError::Offline(_) | ImmichError::Protocol(_)), "{e:?}");
    // a certificate nobody vouches for carries its fingerprint to the confirmation dialog
    let e = ImmichError::from(lcc_net::NetError::UntrustedCertificate { host: "nas.local".into(), fingerprint: "AB:CD".into() });
    assert_eq!(e.kind(), "untrustedCertificate");
    assert!(e.to_string().contains("AB:CD"));
    // a pin that is not a fingerprint is refused before connecting
    let bad = ServerOptions { pinned: Some("nope".into()), ..opts() };
    assert!(matches!(Client::new("https://nas.local", Secret::new(KEY), &bad), Err(ImmichError::Tls(_))));
}

#[test]
fn hostile_ids_are_not_put_into_paths() {
    let (url, seen) = serve(recorded);
    let c = client(&url, KEY);
    assert!(matches!(c.asset("../../server/version"), Err(ImmichError::Protocol(_))));
    assert!(matches!(c.thumbnail("a?b=c", "thumbnail"), Err(ImmichError::Protocol(_))));
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn urls_are_normalized() {
    assert_eq!(client::normalize_url(" photos.example.org/ ").unwrap(), "https://photos.example.org");
    assert_eq!(client::normalize_url("https://nas:2283/api/").unwrap(), "https://nas:2283");
    // plain http is accepted (home servers often have no certificate) but warned about, except for this machine
    for lan in ["http://nas:2283", "http://192.168.1.5:2283", "http://photos.example.org", "http://localhost.example.org"] {
        assert_eq!(client::normalize_url(lan).unwrap(), lan);
        assert!(Client::new(lan, Secret::new("k"), &ServerOptions::default()).is_ok(), "{lan}");
        assert!(client::plain_http_warning(lan).is_some_and(|w| w.contains("not encrypted")), "{lan}");
    }
    for quiet in ["http://localhost:2283", "http://127.0.0.1:2283", "http://[::1]:2283", "http://immich.localhost", "https://nas:2283", "nas"] {
        assert!(client::normalize_url(quiet).is_ok(), "{quiet}");
        assert_eq!(client::plain_http_warning(quiet), None, "{quiet}");
    }
    assert_eq!(client::plain_http_warning(""), None);
    assert!(client::normalize_url("").is_err());
    assert_eq!(client::asset_web_url("http://nas:2283", "abc"), "http://nas:2283/photos/abc");
    assert!(client::api_key_page("http://nas:2283").starts_with("http://nas:2283/user-settings"));
}

fn all_assets(url: &str) -> Vec<Asset> {
    let mut all = Vec::new();
    let q = MetadataSearch { size: Some(3), with_exif: Some(true), ..MetadataSearch::default() };
    client(url, KEY)
        .search_all(&q, 100, |page| {
            all.extend_from_slice(page);
            true
        })
        .unwrap();
    all
}

#[test]
fn listing_pages_until_the_end() {
    let (url, seen) = serve(recorded);
    let all = all_assets(&url);
    assert_eq!(all.len(), 6);
    assert_eq!(seen.lock().unwrap().len(), 3, "pages 1, 2 and the empty 3");
    let a = &all[0];
    assert_eq!(a.original_file_name, "fixture-07.png");
    assert_eq!(a.file_size(), Some(2401));
    assert_eq!(a.local_capture().as_deref(), Some("2024-01-08T12:00:00"));
    assert_eq!(a.sha1_hex().unwrap().len(), 40);
}

fn photo(c: &mut Catalog, name: &str, sha1: Option<String>, captured: &str, size: u64) -> PhotoId {
    let id = c.alloc_photo_id();
    let mut p = Photo::new(id, Source::File { path: format!("/pics/{name}") }, name, "PNG", 64, 48, "2026-01-01T00:00:00");
    p.sha1 = sha1;
    p.captured = Some(captured.into());
    p.file_size = size;
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

#[test]
fn assets_link_by_checksum_else_probably_by_name_time_and_size() {
    let (url, _) = serve(recorded);
    let assets = all_assets(&url);
    let mut cat = Catalog::default();
    let by_sum = photo(&mut cat, "renamed.png", assets[0].sha1_hex(), "1999-01-01T00:00:00", 1);
    let a1 = &assets[1];
    let probable = photo(&mut cat, &a1.original_file_name.to_uppercase(), None, &a1.local_capture().unwrap(), a1.file_size().unwrap());
    let other = photo(&mut cat, "nothing.png", None, "2024-01-01T00:00:00", 5);
    let account = "http://nas#u";
    let index = Index::new(&cat);
    let (ops, found) = link::link_ops(&cat, &index, account, &assets, "2026-10-10T00:00:00");
    assert_eq!(ops.len(), 2);
    assert_eq!(found.iter().map(|m| (m.photo, m.kind)).collect::<Vec<_>>(), vec![(by_sum, MatchKind::Checksum), (probable, MatchKind::Probable)]);
    for op in ops {
        cat.apply(op).unwrap();
    }
    assert_eq!(link::link_state(&cat, by_sum), "linked");
    assert_eq!(link::link_state(&cat, probable), "probable");
    assert_eq!(link::link_state(&cat, other), "none");
    assert_eq!(cat.photo_of_remote(link::SERVICE, account, &assets[0].id), Some(by_sum));
    // the filter and the smart-collection rule see the states
    let f = lcc_catalog::Filter { immich: Some("notLinked".into()), ..Default::default() };
    let mut ids = cat.query(&f, &lcc_catalog::Sort::default());
    ids.sort();
    assert_eq!(ids, vec![probable, other]);
    let rule: lcc_catalog::Rule = serde_json::from_value(serde_json::json!({"field": "immich", "op": "is", "value": "probable"})).unwrap();
    assert!(rule.matches(cat.photo(probable).unwrap(), &cat));
    assert!(!rule.matches(cat.photo(by_sum).unwrap(), &cat));
    // a second pass over the same listing changes nothing
    let index = Index::new(&cat);
    assert!(link::link_ops(&cat, &index, account, &assets, "2026-10-11T00:00:00").0.is_empty());
    // confirming makes the probable link a real one
    let op = link::confirm_op(&cat, probable, account).unwrap();
    cat.apply(op).unwrap();
    assert_eq!(link::link_state(&cat, probable), "linked");
    assert_eq!(cat.remote_of(probable).next().unwrap().sync_state, SyncState::Synced);
    assert!(link::confirm_op(&cat, probable, account).is_none());
}

#[test]
fn a_year_or_month_alone_links_probably_as_the_start_of_its_period() {
    let asset = |id: &str, local: &str| Asset {
        id: id.into(),
        original_file_name: "scan.tif".into(),
        local_date_time: Some(local.into()),
        exif_info: Some(ExifInfo { file_size_in_byte: Some(900), ..ExifInfo::default() }),
        ..Asset::default()
    };
    let mut cat = Catalog::default();
    let month = photo(&mut cat, "scan.tif", None, "1983-04", 900);
    let year = photo(&mut cat, "SCAN.TIF", None, "1984", 900);
    let index = Index::new(&cat);
    let found = |a: &Asset| index.find(a).map(|m| (m.photo, m.kind));
    assert_eq!(
        found(&asset("m", "1983-04-01T00:00:00.000Z")),
        Some((month, MatchKind::Probable)),
        "a month alone matches the first moment of that month"
    );
    assert_eq!(
        found(&asset("y", "1984-01-01T00:00:00.000Z")),
        Some((year, MatchKind::Probable)),
        "a year alone matches the first moment of that year"
    );
    assert_eq!(found(&asset("x", "1983-04-02T00:00:00.000Z")), None, "another day of the month is not the same capture time");
}

#[test]
fn ambiguous_fallbacks_and_trashed_assets_are_not_linked() {
    let (url, _) = serve(recorded);
    let mut assets = all_assets(&url);
    let a = assets[0].clone();
    let mut cat = Catalog::default();
    for _ in 0..2 {
        photo(&mut cat, &a.original_file_name, None, &a.local_capture().unwrap(), a.file_size().unwrap());
    }
    let index = Index::new(&cat);
    assert!(link::link_ops(&cat, &index, "acc", std::slice::from_ref(&a), "now").0.is_empty());
    let mut cat = Catalog::default();
    photo(&mut cat, "x.png", a.sha1_hex(), "2020-01-01T00:00:00", 1);
    assets[0].is_trashed = true;
    let index = Index::new(&cat);
    assert!(link::link_ops(&cat, &index, "acc", &assets[..1], "now").0.is_empty());
}

#[test]
fn metadata_maps_one_way_on_import() {
    let a: Asset = serde_json::from_value(serde_json::json!({
        "id": "a1",
        "isFavorite": true,
        "localDateTime": "2024-05-06T07:08:09.000Z",
        "exifInfo": {"rating": 4, "description": " Sunset ", "latitude": 47.5, "longitude": 8.25, "city": "Zürich"},
        "tags": [{"id": "t", "value": "travel/swiss"}, {"id": "u", "value": "travel/swiss"}]
    }))
    .unwrap();
    let mut p = Photo::new(PhotoId(1), Source::File { path: "/x.jpg".into() }, "x.jpg", "JPEG", 1, 1, "now");
    mapping::apply(&a, &mut p);
    assert_eq!(p.rating, 4);
    assert_eq!(p.flag, lcc_catalog::Flag::Pick);
    assert_eq!(p.meta.caption, "Sunset");
    assert_eq!(p.meta.keywords, vec!["travel|swiss".to_string()]);
    assert_eq!(p.meta.gps, Some((47.5, 8.25)));
    assert_eq!(p.meta.city, "Zürich");
    assert_eq!(p.captured.as_deref(), Some("2024-05-06T07:08:09"));
    // rejected wins over favourite, and clears stars
    let r: Asset =
        serde_json::from_value(serde_json::json!({"id": "b", "isFavorite": true, "exifInfo": {"rating": -1, "latitude": 999.0, "longitude": 1.0}}))
            .unwrap();
    let mut q = Photo::new(PhotoId(2), Source::File { path: "/y.jpg".into() }, "y.jpg", "JPEG", 1, 1, "now");
    q.rating = 3;
    mapping::apply(&r, &mut q);
    assert_eq!((q.rating, q.flag), (0, lcc_catalog::Flag::Reject));
    assert_eq!(q.meta.gps, None, "out-of-range coordinates are dropped");
}

/// A photo dated only by its file's modification time doesn't send that date to Immich, whether
/// the import wrote it in local time with a zone or, as older imports did, in UTC.
#[test]
fn a_file_date_is_not_a_capture_date() {
    let dir = lcc_testkit::TempDir::new("lcc-immich-file-date");
    let path = dir.join("x.jpg");
    std::fs::write(&path, b"x").unwrap();
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let mut p = Photo::new(PhotoId(1), Source::File { path: path.to_string_lossy().to_string() }, "x.jpg", "JPEG", 1, 1, "now");
    for file_date in [lcc_catalog::dates::local(mtime).unwrap(), lcc_catalog::dates::civil(mtime)] {
        p.captured = Some(file_date.clone());
        assert_eq!(mapping::captured(&p), None, "{file_date}");
    }
    p.captured = Some("2024-05-06T07:08:09".into());
    assert_eq!(mapping::captured(&p), Some("2024-05-06T07:08:09"));
}

#[test]
fn external_library_paths_map_both_ways() {
    let maps = vec![
        PathMap { container: "/mnt/photos".into(), local: "/home/me/Photos".into() },
        PathMap { container: "/mnt/photos/2024".into(), local: "/media/disk/2024".into() },
    ];
    assert_eq!(extlib::to_local(&maps, "/mnt/photos/a/b.jpg").as_deref(), Some("/home/me/Photos/a/b.jpg"));
    assert_eq!(extlib::to_local(&maps, "/mnt/photos/2024/c.jpg").as_deref(), Some("/media/disk/2024/c.jpg"));
    assert_eq!(extlib::to_local(&maps, "/mnt/photosX/c.jpg"), None, "whole components only");
    assert_eq!(extlib::to_container(&maps, "/home/me/Photos/a").as_deref(), Some("/mnt/photos/a"));
    let libs = vec![Library { id: "L".into(), name: "NAS".into(), import_paths: vec!["/mnt/photos".into()], ..Library::default() }];
    let cov = extlib::coverage(&["/home/me/Photos/2023".into(), "/elsewhere".into()], &libs, &maps);
    assert_eq!(cov[0].library.as_deref(), Some("L"));
    assert_eq!(cov[0].container.as_deref(), Some("/mnt/photos/2023"));
    assert_eq!(cov[1].library, None);
    let s = extlib::suggest(&["/home/me/photos".into()], &libs);
    assert_eq!(s, vec![PathMap { container: "/mnt/photos".into(), local: "/home/me/photos".into() }]);
}

#[test]
fn accounts_file_round_trips_without_secrets() {
    // a folder that doesn't exist yet: saving makes it
    let tmp = lcc_testkit::TempDir::new("lcc-immich-accounts");
    let dir = tmp.join("settings");
    let path = dir.join("connections.json");
    let mut a = crate::Accounts::default();
    a.upsert(crate::Account { id: "http://nas#u".into(), url: "http://nas".into(), user_id: "u".into(), ..Default::default() });
    a.get_mut("http://nas#u").unwrap().path_maps.push(PathMap { container: "/a".into(), local: "/b".into() });
    // reconnecting keeps the mapping
    a.upsert(crate::Account { id: "http://nas#u".into(), url: "http://nas".into(), user_id: "u".into(), ..Default::default() });
    assert_eq!(a.immich[0].path_maps.len(), 1);
    a.save(&path).unwrap();
    assert_eq!(crate::Accounts::load(&path).unwrap(), a);
    std::fs::write(&path, b"{not json").unwrap();
    assert!(crate::Accounts::load(&path).is_err());
    assert_eq!(crate::Accounts::load(&dir.join("missing.json")).unwrap(), crate::Accounts::default());
}

// Regression: Immich doesn't hash external-library files (checksum = SHA-1 of `path:` + path),
// so they are linked by their mapped path, never by checksum.
#[test]
fn external_library_assets_link_by_mapped_path() {
    let mut cat = Catalog::default();
    let shared = photo(&mut cat, "shared.png", Some("ab".repeat(20)), "2024-01-01T00:00:00", 10);
    let a = crate::types::Asset {
        id: "ext-1".into(),
        library_id: Some("lib".into()),
        original_path: "/mnt/photos/shared.png".into(),
        original_file_name: "shared.png".into(),
        checksum: "W+lhGqxqiJGkv2Suuu2b+NqwfVY=".into(),
        ..Default::default()
    };
    let maps = [crate::extlib::PathMap { container: "/mnt/photos".into(), local: "/pics/".into() }];
    assert!(Index::new(&cat).find(&a).is_none(), "no mapping: no link");
    let index = Index::with_path_maps(&cat, &maps);
    let (ops, found) = link::link_ops(&cat, &index, "acc", std::slice::from_ref(&a), "now");
    assert_eq!(found.iter().map(|m| (m.photo, m.kind)).collect::<Vec<_>>(), vec![(shared, MatchKind::Path)]);
    for op in ops {
        cat.apply(op).unwrap();
    }
    assert_eq!(link::link_state(&cat, shared), "linked");
    // an uploaded asset at that path (no library) is not matched by path
    let up = crate::types::Asset { library_id: None, id: "up".into(), ..a };
    assert!(index.find(&up).is_none());
}

#[path = "tests_sync.rs"]
mod sync_tests;

#[test]
fn share_uploads_makes_an_album_and_a_password_link() {
    let (url, seen) = serve(|req| {
        let path = req.target.split('?').next().unwrap_or_default().to_string();
        match (req.method.as_str(), path.as_str()) {
            ("POST", "/api/assets") => response("201 Created", br#"{"id":"11111111-1111-1111-1111-111111111111","status":"created"}"#),
            ("POST", "/api/albums") => response("201 Created", br#"{"id":"22222222-2222-2222-2222-222222222222","albumName":"Trip"}"#),
            ("PUT", p) if p.ends_with("/assets") => response("200 OK", br#"[]"#),
            ("POST", "/api/shared-links") => response("201 Created", br#"{"id":"33333333-3333-3333-3333-333333333333","key":"abcDEF_123-x"}"#),
            _ => response("404 Not Found", br#"{"message":"Not found"}"#),
        }
    });
    let c = client(&url, KEY);
    let items = vec![
        crate::share::ShareItem { file_name: "a.jpg".into(), bytes: vec![0xFF, 0xD8, 0xFF, 0xD9], created: "2026-01-01T00:00:00.000Z".into() },
        crate::share::ShareItem { file_name: "b.jpg".into(), bytes: vec![0xFF, 0xD8, 0xFF, 0xD9], created: "2026-01-02T00:00:00.000Z".into() },
    ];
    let o = crate::share::ShareOptions {
        expires_at: Some("2026-12-31T00:00:00.000Z".into()),
        password: Some("secret".into()),
        allow_download: false,
        show_metadata: false,
        description: None,
    };
    let r = crate::share::share_photos(&c, "Trip", "test", &items, &o, &mut |_, _| true).unwrap();
    assert_eq!(r.uploaded, 2);
    assert_eq!(r.url, format!("{url}/share/abcDEF_123-x"));
    {
        let seen = seen.lock().unwrap();
        let link = seen.iter().find(|r| r.target == "/api/shared-links").unwrap();
        let body: serde_json::Value = serde_json::from_slice(&link.body).unwrap();
        assert_eq!(body["type"], "ALBUM");
        assert_eq!(body["albumId"], "22222222-2222-2222-2222-222222222222");
        assert_eq!(body["password"], "secret");
        assert_eq!(body["allowDownload"], false);
        assert_eq!(body["expiresAt"], "2026-12-31T00:00:00.000Z");
    }
    // nothing to share and a cancelled share are errors, not panics
    assert!(crate::share::share_photos(&c, "Trip", "test", &[], &o, &mut |_, _| true).is_err());
    assert!(crate::share::share_photos(&c, "Trip", "test", &items, &o, &mut |_, _| false).is_err());
}

#[test]
fn share_rejects_a_link_without_a_usable_key() {
    let (url, _) = serve(|_| response("201 Created", br#"{"id":"x","key":"../evil"}"#));
    let c = client(&url, KEY);
    assert!(c.create_shared_link("22222222-2222-2222-2222-222222222222", &crate::share::ShareOptions::default()).is_err());
}

// ---- P6.2: never crash on what a server (or a damaged settings file) sends

const LIBRARIES: &str = include_str!("../tests/fixtures/libraries.json");

/// Everything the app does with a server's assets: map them onto a photo, link them into a
/// catalog, translate their paths.
fn use_assets(assets: &[Asset]) {
    let mut cat = Catalog::new();
    photo(&mut cat, "a.png", Some("a9993e364706816aba3e25717850c26c9cd0d89d".into()), "2024-05-01T10:00:00", 1234);
    let index = Index::with_path_maps(&cat, &[PathMap { container: "/usr/src/app/upload".into(), local: "/pics".into() }]);
    let (ops, _) = link::link_ops(&cat, &index, "acct", assets, "2026-01-01T00:00:00");
    for op in ops {
        let _ = cat.apply(op);
    }
    for a in assets {
        let (_, _, _, _) = (a.is_image(), a.file_size(), a.local_capture(), a.sha1_hex());
        let _ = index.find(a);
        let mut p = Photo::new(PhotoId(1), Source::File { path: "/x".into() }, "x", "JPEG", 1, 1, "2026-01-01");
        mapping::apply(a, &mut p);
        let _ = mapping::ops(a, &p);
    }
}

#[test]
fn hostile_server_json_never_panics() {
    let seeds = [VERSION, ME, KEY_ME, PAGE1, PAGE2, ALBUMS, LIBRARIES];
    lcc_fuzzkit::run_json("immich.json", &seeds, 1500, |s| {
        let _ = serde_json::from_str::<ServerVersion>(s);
        let _ = serde_json::from_str::<User>(s);
        let _ = serde_json::from_str::<ApiKeyInfo>(s);
        let _ = serde_json::from_str::<Vec<Album>>(s);
        let _ = serde_json::from_str::<People>(s);
        if let Ok(libs) = serde_json::from_str::<Vec<Library>>(s) {
            let folders = vec!["/pics".to_string(), "/".to_string(), String::new()];
            let maps = extlib::suggest(&folders, &libs);
            let _ = extlib::coverage(&folders, &libs, &maps);
            for f in &folders {
                let _ = extlib::to_container(&maps, f);
                let _ = extlib::to_local(&maps, f);
            }
        }
        if let Ok(r) = serde_json::from_str::<SearchResponse>(s) {
            use_assets(&r.assets.items);
        }
    });
}

#[test]
fn hostile_server_answers_through_the_client_never_panic() {
    let body = Arc::new(Mutex::new(Vec::new()));
    let b2 = body.clone();
    let (url, _) = serve(move |_| response("200 OK", &b2.lock().unwrap()));
    let c = client(&url, KEY);
    let seeds = [VERSION, ME, PAGE1, ALBUMS, LIBRARIES];
    // each iteration is a few loopback requests: a short loop (LCC_FUZZ_ITERS raises it)
    lcc_fuzzkit::run_json("immich.client", &seeds, 60, |s| {
        *body.lock().unwrap() = s.as_bytes().to_vec();
        let _ = c.status();
        let _ = c.albums();
        let _ = c.libraries();
        let _ = c.search_all(&MetadataSearch::default(), 3, |a| {
            use_assets(a);
            true
        });
    });
}

#[test]
fn damaged_connections_file_never_panics() {
    let dir = lcc_testkit::TempDir::new("lcc-immich-robust");
    let path = dir.join("connections.json");
    let mut a = crate::Accounts::default();
    a.upsert(crate::Account {
        id: crate::Account::make_id("https://photos.example", "u1"),
        url: "https://photos.example".into(),
        user_id: "u1".into(),
        user_name: "Me".into(),
        email: "me@example.org".into(),
        version: Some(ServerVersion { major: 3, minor: 3, patch: 1 }),
        permissions: Some(vec!["all".into()]),
        pinned: None,
        path_maps: vec![PathMap { container: "/usr/src/app/upload".into(), local: "/pics".into() }],
        linked_until: None,
        sync: Default::default(),
    });
    a.save(&path).unwrap();
    let seed = std::fs::read_to_string(&path).unwrap();
    lcc_fuzzkit::run_json("immich.connections", &[&seed], 800, |s| {
        std::fs::write(&path, s).unwrap();
        if let Ok(acc) = crate::Accounts::load(&path) {
            for a in &acc.immich {
                let _ = Client::new(&a.url, Secret::new("k"), &opts());
                let _ = extlib::to_local(&a.path_maps, "/usr/src/app/upload/x.jpg");
            }
        }
    });
}

/// P6.2: a full disk while saving `connections.json` is an error that leaves the old file and
/// no partial temp file behind.
#[test]
fn full_disk_connections_save_leaves_no_temp_file() {
    let dir = lcc_testkit::TempDir::new("lcc-immich-full");
    let path = dir.join("connections.json");
    crate::Accounts::default().save(&path).unwrap();
    let before = std::fs::read(&path).unwrap();
    let mut a = crate::Accounts::default();
    a.upsert(crate::Account { id: "x".into(), url: "https://h".into(), user_name: "a long enough name to overflow".into(), ..Default::default() });
    {
        let _full = lcc_catalog::safe_file::fail_writes_after(8);
        assert!(a.save(&path).is_err());
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
}

// ---- IMM-PUBLISH: album rename, upload retries

fn publish_photos() -> std::collections::HashMap<PhotoId, crate::publish::PhotoInfo> {
    let mut m = std::collections::HashMap::new();
    m.insert(
        PhotoId(1),
        crate::publish::PhotoInfo { created: "2024-01-01T00:00:00.000Z".into(), original_name: "a.jpg".into(), ..Default::default() },
    );
    m
}

/// A server with one album `al-1` named "Old", counting uploads; the first `fail_uploads` uploads
/// answer 503, and with `arrived` an upload that failed still reached the server (the checksum
/// check finds it afterwards).
fn publish_server(fail_uploads: usize, arrived: bool) -> (String, Arc<Mutex<Vec<Req>>>) {
    publish_server_with(fail_uploads, arrived, false)
}

/// `trashed`: the checksum match is in the trash (and was found before any upload).
fn publish_server_with(fail_uploads: usize, arrived: bool, trashed: bool) -> (String, Arc<Mutex<Vec<Req>>>) {
    let uploads = Arc::new(AtomicUsize::new(0));
    let name = Arc::new(Mutex::new("Old".to_string()));
    serve(move |req| {
        let path = req.target.split('?').next().unwrap_or_default();
        match (req.method.as_str(), path) {
            ("GET", "/api/albums") => {
                let n = name.lock().unwrap().clone();
                response("200 OK", format!(r#"[{{"id":"al-1","albumName":"{n}","assetCount":0}}]"#).as_bytes())
            }
            ("PATCH", "/api/albums/al-1") => {
                let v: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                *name.lock().unwrap() = v["albumName"].as_str().unwrap().to_string();
                response("200 OK", br#"{"id":"al-1","albumName":"x","assetCount":0}"#)
            }
            ("POST", "/api/albums") => response("201 Created", br#"{"id":"al-new","albumName":"x","assetCount":0}"#),
            ("POST", "/api/assets/bulk-upload-check") => {
                if trashed || (arrived && uploads.load(Ordering::SeqCst) > 0) {
                    response("200 OK", br#"{"results":[{"id":"x","action":"reject","reason":"duplicate","assetId":"as-1"}]}"#)
                } else {
                    response("200 OK", br#"{"results":[{"id":"x","action":"accept"}]}"#)
                }
            }
            ("POST", "/api/assets") => {
                let n = uploads.fetch_add(1, Ordering::SeqCst);
                if n < fail_uploads {
                    response("503 Service Unavailable", br#"{"message":"busy"}"#)
                } else {
                    response("201 Created", br#"{"id":"as-1","status":"created"}"#)
                }
            }
            ("GET", "/api/assets/as-1") => response("200 OK", format!(r#"{{"id":"as-1","isTrashed":{trashed},"deviceId":"dev"}}"#).as_bytes()),
            ("POST", "/api/trash/restore/assets") => response("204 No Content", b""),
            ("PUT", "/api/albums/al-1/assets") | ("PUT", "/api/albums/al-new/assets") => response("200 OK", br#"[{"id":"as-1","success":true}]"#),
            _ => response("404 Not Found", br#"{"message":"Not found"}"#),
        }
    })
}

fn publish_once(url: &str, name: &str, state: Option<std::path::PathBuf>) -> Result<String, lcc_publish::PublishError> {
    use lcc_publish::{PublishService, Upload};
    let mut svc = crate::publish::ImmichPublish::new(client(url, KEY), Default::default(), name, "dev", publish_photos())
        .with_upload_retry(3, Duration::from_millis(5));
    if let Some(p) = state {
        svc = svc.with_album_state(p);
    }
    let up = Upload { photo: PhotoId(1), file_name: "a.jpg", bytes: b"\xff\xd8\xff\xd9", sidecars: &[], previous: None };
    svc.publish(&up).map(|p| p.remote_id)
}

#[test]
fn a_renamed_collection_renames_its_remembered_album() {
    // the state file's folder doesn't exist yet
    let tmp = lcc_testkit::TempDir::new("lcc-immich-pubalbum");
    let dir = tmp.join("state");
    let state = dir.join("svc-1-5.album");
    let (url, seen) = publish_server(0, false);
    // first run: found by name, remembered
    publish_once(&url, "Old", Some(state.clone())).unwrap();
    assert_eq!(std::fs::read_to_string(&state).unwrap(), "al-1");
    // renamed (and moved into a set): the same album is renamed, no new album
    publish_once(&url, "Trips / New", Some(state.clone())).unwrap();
    {
        let seen = seen.lock().unwrap();
        let patch = seen.iter().find(|r| r.method == "PATCH").expect("renamed");
        assert!(String::from_utf8_lossy(&patch.body).contains("Trips / New"));
        assert!(!seen.iter().any(|r| r.method == "POST" && r.target == "/api/albums"), "no new album");
    }
    // without the state file a new name means a new album
    let (url, seen) = publish_server(0, false);
    publish_once(&url, "Other", None).unwrap();
    assert!(seen.lock().unwrap().iter().any(|r| r.method == "POST" && r.target == "/api/albums"));
}

#[test]
fn failed_uploads_are_retried_without_duplicates() {
    // two 503s, then it goes through
    let (url, seen) = publish_server(2, false);
    assert_eq!(publish_once(&url, "Old", None).unwrap(), "r:as-1");
    assert_eq!(seen.lock().unwrap().iter().filter(|r| r.method == "POST" && r.target == "/api/assets").count(), 3);
    // the upload arrived although the answer was lost: the retry finds it by checksum
    let (url, seen) = publish_server(1, true);
    assert_eq!(publish_once(&url, "Old", None).unwrap(), "r:as-1");
    assert_eq!(seen.lock().unwrap().iter().filter(|r| r.method == "POST" && r.target == "/api/assets").count(), 1);
    // gives up after the retries, with an error
    let (url, _) = publish_server(100, false);
    assert!(publish_once(&url, "Old", None).is_err());
}

#[test]
fn a_trashed_checksum_match_is_restored_not_reused_as_is() {
    let (url, seen) = publish_server_with(0, false, true);
    assert_eq!(publish_once(&url, "Old", None).unwrap(), "r:as-1");
    let seen = seen.lock().unwrap();
    let restore = seen.iter().find(|r| r.method == "POST" && r.target == "/api/trash/restore/assets").expect("restored");
    assert!(String::from_utf8_lossy(&restore.body).contains("as-1"));
    assert!(!seen.iter().any(|r| r.method == "POST" && r.target == "/api/assets"), "no upload");
}

/// A server whose album `al-1` ("Old") has `activities` as the answer to `GET /activities`.
fn comment_server(activities: &'static str) -> (String, Arc<Mutex<Vec<Req>>>) {
    serve(move |req| {
        let path = req.target.split('?').next().unwrap_or_default();
        match (req.method.as_str(), path) {
            ("GET", "/api/albums") => response("200 OK", br#"[{"id":"al-1","albumName":"Old","assetCount":1}]"#),
            ("GET", "/api/activities") => response("200 OK", activities.as_bytes()),
            _ => response("404 Not Found", br#"{"message":"Not found"}"#),
        }
    })
}

fn comments_of(url: &str, name: &str, remote: &str) -> Result<Vec<lcc_publish::Comment>, lcc_publish::PublishError> {
    use lcc_publish::PublishService;
    let mut svc = crate::publish::ImmichPublish::new(client(url, KEY), Default::default(), name, "dev", publish_photos());
    assert!(svc.capabilities().comments);
    svc.comments(remote)
}

#[test]
fn comments_come_from_the_album_activity_oldest_first_without_likes() {
    let (url, seen) = comment_server(
        r#"[
        {"id":"b","type":"comment","createdAt":"2026-11-02T10:00:00.000Z","comment":" Lovely light ","user":{"name":"Ann","email":"a@x"}},
        {"id":"a","type":"comment","createdAt":"2026-11-01T09:00:00.000Z","comment":"First!","user":{"name":"","email":"bob@x"}},
        {"id":"l","type":"like","createdAt":"2026-11-03T09:00:00.000Z","user":{"name":"Cy"}},
        {"id":"e","type":"comment","createdAt":"2026-11-04T09:00:00.000Z","comment":"  "}
    ]"#,
    );
    let list = comments_of(&url, "Old", "r:as-1").unwrap();
    assert_eq!(list.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), ["First!", "Lovely light"]);
    assert_eq!(list[0].author, "bob@x");
    assert_eq!(list[1].author, "Ann");
    assert_eq!(list[0].date.as_deref(), Some("2026-11-01T09:00:00.000Z"));
    let seen = seen.lock().unwrap();
    let req = seen.iter().find(|r| r.target.starts_with("/api/activities")).expect("asked");
    assert!(req.target.contains("albumId=al-1") && req.target.contains("assetId=as-1") && req.target.contains("type=comment"), "{}", req.target);
    assert!(!seen.iter().any(|r| r.method != "GET"), "read only: no album is created or renamed");
}

#[test]
fn comments_of_an_album_that_is_gone_are_none_and_a_server_error_is_reported() {
    let (url, seen) = comment_server("[]");
    assert!(comments_of(&url, "Other", "r:as-1").unwrap().is_empty());
    assert!(!seen.lock().unwrap().iter().any(|r| r.target.starts_with("/api/activities")));
    let (url, _) = serve(|_| response("403 Forbidden", br#"{"message":"missing permission activity.read"}"#));
    assert!(comments_of(&url, "Old", "r:as-1").is_err());
}

#[test]
fn hostile_activity_answers_are_bounded_and_never_panic() {
    // too many entries are cut, long text and names are clipped
    let many: String = (0..600)
        .map(|i| {
            let (text, name) = if i < 3 { ("é".repeat(9000), "n".repeat(900)) } else { ("hi".to_string(), "n".to_string()) };
            format!(r#"{{"id":"c{i}","type":"comment","createdAt":"2026-11-01T09:00:00Z","comment":"{text}","user":{{"name":"{name}"}}}}"#)
        })
        .collect::<Vec<_>>()
        .join(",");
    let (url, _) = comment_server(Box::leak(format!("[{many}]").into_boxed_str()));
    let list = comments_of(&url, "Old", "r:as-1").unwrap();
    assert!(list.len() <= MAX_ACTIVITIES);
    assert!(list.iter().all(|c| c.text.chars().count() <= 4000 && c.author.chars().count() <= 200));
    // an answer over the byte cap is refused
    let big = format!("[{}]", " ".repeat(MAX_ACTIVITY_BYTES as usize + 10));
    let (url, _) = comment_server(Box::leak(big.into_boxed_str()));
    assert!(comments_of(&url, "Old", "r:as-1").is_err());
    // wrong shapes and hostile ids are errors, not panics
    for body in ["{}", "null", "[1,2]", r#"[{"id":5}]"#, r#"[{"type":null,"comment":7}]"#, "[", ""] {
        let (url, _) = comment_server(Box::leak(body.to_string().into_boxed_str()));
        let _ = comments_of(&url, "Old", "r:as-1");
    }
    let (url, _) = comment_server("[]");
    for id in ["", "r:", "r:../x", "o:a b+r:", "garbage"] {
        let _ = comments_of(&url, "Old", id);
    }
}

/// The album add/remove endpoints answer 200 whether or not each asset was taken: the per-asset
/// results decide. A `duplicate` add and a `not_found` remove leave the album as asked.
#[test]
fn album_add_and_remove_read_the_per_asset_results() {
    let (url, _) = serve(|req| match (req.method.as_str(), req.target.as_str()) {
        ("PUT", "/api/albums/al-1/assets") => response(
            "200 OK",
            br#"[{"id":"as-1","success":true},{"id":"as-2","success":false,"error":"duplicate"},{"id":"as-3","success":false,"error":"no_permission"},{"id":"other","success":false,"error":"unknown"}]"#,
        ),
        ("DELETE", "/api/albums/al-1/assets") => {
            response("200 OK", br#"[{"id":"as-1","success":false,"error":"not_found"},{"id":"as-2","success":false,"error":"no_permission"}]"#)
        }
        // a server that answers with something else than the list: the status decides
        ("PUT", "/api/albums/al-2/assets") => response("200 OK", br#"{"id":"al-2","albumName":"x"}"#),
        _ => response("404 Not Found", br#"{"message":"Not found"}"#),
    });
    let c = client(&url, KEY);
    let ids: Vec<String> = ["as-1", "as-2", "as-3"].iter().map(|s| s.to_string()).collect();
    assert_eq!(c.album_add_checked("al-1", &ids).unwrap(), vec![("as-3".to_string(), "no_permission".to_string())]);
    assert_eq!(c.album_remove_checked("al-1", &ids[..2]).unwrap(), vec![("as-2".to_string(), "no_permission".to_string())]);
    // the plain calls fail when an asset was refused, and say how many
    let e = c.album_add("al-1", &ids).unwrap_err();
    assert!(matches!(&e, ImmichError::Status(_, m) if m.contains("1 of 3") && m.contains("no_permission")), "{e:?}");
    assert!(c.album_remove("al-1", &ids[..1]).is_ok());
    assert!(c.album_add("al-2", &ids).is_ok());
    assert!(matches!(c.album_add("nope", &ids), Err(ImmichError::NotFound(_))));
}
