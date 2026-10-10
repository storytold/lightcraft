//! Tests against an in-process HTTP server replaying responses recorded from a v3.3.1 server
//! (`tests/fixtures/*.json`, recorded with `cargo xtask immich up && cargo xtask immich seed`).
#![cfg(not(target_arch = "wasm32"))]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dac_catalog::{Catalog, Op, Photo, PhotoId, Source, SyncState};
use dac_credentials::Secret;

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
    assert_eq!(client::missing_permissions(&["asset.read".into()]).len(), 4, "import, external libraries, sync, people");
    // the key goes in its header, never in the URL
    let seen = seen.lock().unwrap();
    assert!(seen.iter().all(|r| !r.target.contains(KEY)));
    assert!(seen.iter().any(|r| r.header("x-api-key") == Some(KEY)));
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
    let e = ImmichError::from(dac_net::NetError::UntrustedCertificate { host: "nas.local".into(), fingerprint: "AB:CD".into() });
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
    assert_eq!(client::normalize_url("http://nas:2283/api/").unwrap(), "http://nas:2283");
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
    let f = dac_catalog::Filter { immich: Some("notLinked".into()), ..Default::default() };
    let mut ids = cat.query(&f, &dac_catalog::Sort::default());
    ids.sort();
    assert_eq!(ids, vec![probable, other]);
    let rule: dac_catalog::Rule = serde_json::from_value(serde_json::json!({"field": "immich", "op": "is", "value": "probable"})).unwrap();
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
    assert_eq!(p.flag, dac_catalog::Flag::Pick);
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
    assert_eq!((q.rating, q.flag), (0, dac_catalog::Flag::Reject));
    assert_eq!(q.meta.gps, None, "out-of-range coordinates are dropped");
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
    let dir = std::env::temp_dir().join(format!("dac-immich-accounts-{}", std::process::id()));
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
    let _ = std::fs::remove_dir_all(&dir);
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
    dac_fuzzkit::run_json("immich.json", &seeds, 1500, |s| {
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
    // each iteration is a few loopback requests: a short loop (DAC_FUZZ_ITERS raises it)
    dac_fuzzkit::run_json("immich.client", &seeds, 60, |s| {
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
    let dir = std::env::temp_dir().join(format!("dac-immich-robust-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
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
    dac_fuzzkit::run_json("immich.connections", &[&seed], 800, |s| {
        std::fs::write(&path, s).unwrap();
        if let Ok(acc) = crate::Accounts::load(&path) {
            for a in &acc.immich {
                let _ = Client::new(&a.url, Secret::new("k"), &opts());
                let _ = extlib::to_local(&a.path_maps, "/usr/src/app/upload/x.jpg");
            }
        }
    });
    let _ = std::fs::remove_dir_all(&dir);
}
