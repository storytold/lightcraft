//! The client against a local stand-in for Flickr (std `TcpListener`; never the internet). The
//! stand-in checks every request's OAuth signature with the same secrets, as Flickr does.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use super::*;

const KEY: &str = "consumer-key";
const SECRET: &str = "consumer-secret";

/// A request as the stand-in received it.
#[derive(Clone, Debug)]
struct Seen {
    method: String,
    /// Path without the query.
    path: String,
    /// Query parameters (GET) or the multipart text fields (POST), decoded.
    params: Vec<(String, String)>,
    /// The `photo` part: (file name, content type, bytes).
    photo: Option<(String, String, Vec<u8>)>,
}

/// Answers each request with `reply(index, &request)` → (status, body).
struct Fake {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

fn fake(reply: impl Fn(usize, &Seen) -> (u16, String) + Send + Sync + 'static) -> Fake {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for (i, s) in l.incoming().enumerate() {
            let Ok(mut s) = s else { break };
            let req = read_request(&mut s);
            log.lock().unwrap().push(req.clone());
            let (status, body) = reply(i, &req);
            let _ = write!(s, "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    Fake { base, seen }
}

fn read_request(s: &mut std::net::TcpStream) -> Seen {
    let mut got = Vec::new();
    let mut buf = [0u8; 8192];
    let head_end = loop {
        let n = s.read(&mut buf).unwrap();
        assert!(n > 0, "connection closed in the head");
        got.extend_from_slice(&buf[..n]);
        if let Some(i) = got.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&got[..head_end]).to_string();
    let len: usize = head.lines().find_map(|l| l.strip_prefix("Content-Length: ")).map_or(0, |v| v.trim().parse().unwrap());
    while got.len() < head_end + len {
        let n = s.read(&mut buf).unwrap();
        got.extend_from_slice(&buf[..n]);
    }
    let body = got[head_end..].to_vec();
    let mut first = head.lines().next().unwrap().split(' ');
    let method = first.next().unwrap().to_string();
    let target = first.next().unwrap().to_string();
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let mut seen = Seen { method, path: path.to_string(), params: reply::form(query), photo: None };
    if head.contains("Content-Type: application/x-www-form-urlencoded") {
        seen.params.extend(reply::form(&String::from_utf8_lossy(&body)));
    }
    if let Some(b) = head.lines().find_map(|l| l.strip_prefix("Content-Type: multipart/form-data; boundary=")) {
        let sep = format!("--{}", b.trim());
        let text = body.clone();
        let mut parts = Vec::new();
        let mut rest: &[u8] = &text;
        while let Some(i) = rest.windows(sep.len()).position(|w| w == sep.as_bytes()) {
            parts.push(rest[..i].to_vec());
            rest = &rest[i + sep.len()..];
        }
        for p in parts.into_iter().skip(1) {
            let p = p.strip_prefix(b"\r\n").unwrap_or(&p).to_vec();
            let p = p.strip_suffix(b"\r\n").unwrap_or(&p).to_vec();
            let split = p.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            let h = String::from_utf8_lossy(&p[..split]).to_string();
            let v = p[split + 4..].to_vec();
            let name = h.split("name=\"").nth(1).unwrap().split('"').next().unwrap().to_string();
            if name == "photo" {
                let file = h.split("filename=\"").nth(1).unwrap().split('"').next().unwrap().to_string();
                let ct = h.lines().find_map(|l| l.strip_prefix("Content-Type: ")).unwrap().to_string();
                seen.photo = Some((file, ct, v));
            } else {
                seen.params.push((name, String::from_utf8(v).unwrap()));
            }
        }
    }
    seen
}

/// Whether `req` carries a valid signature for these secrets (as Flickr checks it).
fn signed_ok(base: &str, req: &Seen, token_secret: &str) -> bool {
    let sig = reply::field(&req.params, "oauth_signature").unwrap().to_string();
    let rest: Vec<_> = req.params.iter().filter(|(k, _)| k != "oauth_signature").cloned().collect();
    oauth::sign(&req.method, &format!("{base}{}", req.path), &rest, SECRET, token_secret).unwrap() == sig
}

fn client(f: &Fake) -> Client {
    let mut c = Client::new(ApiKey { key: KEY.into(), secret: SECRET.into() });
    c.endpoints = Endpoints::all(&f.base);
    c.connect_timeout = Duration::from_secs(5);
    c.stall_timeout = Duration::from_secs(5);
    c.retry_pause = Duration::from_millis(10);
    c
}

fn token() -> Token {
    Token { token: "access-token".into(), secret: "access-secret".into(), user_nsid: "123@N01".into(), username: "me".into(), fullname: "Me".into() }
}

#[test]
fn signs_in_with_the_out_of_band_flow() {
    let f = fake(|i, _| match i {
        0 => (200, "oauth_callback_confirmed=true&oauth_token=req-token&oauth_token_secret=req-secret".into()),
        _ => (200, "fullname=Ansel%20A&oauth_token=access-token&oauth_token_secret=access-secret&user_nsid=123%40N01&username=ansel".into()),
    });
    let c = client(&f);
    let cancel = AtomicBool::new(false);
    let req = c.request_token(&cancel).unwrap();
    assert_eq!(req, RequestToken { token: "req-token".into(), secret: "req-secret".into() });
    assert_eq!(c.authorize_url(&req), format!("{}/services/oauth/authorize?oauth_token=req-token&perms=write", f.base));
    let t = c.access_token(&req, " 123-456-789 ", &cancel).unwrap();
    assert_eq!((t.username.as_str(), t.fullname.as_str(), t.user_nsid.as_str(), t.token.as_str()), ("ansel", "Ansel A", "123@N01", "access-token"));
    let seen = f.seen.lock().unwrap();
    assert_eq!(seen[0].path, "/services/oauth/request_token");
    assert_eq!(reply::field(&seen[0].params, "oauth_callback"), Some("oob"));
    assert_eq!(reply::field(&seen[0].params, "oauth_consumer_key"), Some(KEY));
    assert!(signed_ok(&f.base, &seen[0], ""), "request token: signed with the consumer secret alone");
    assert_eq!(seen[1].path, "/services/oauth/access_token");
    assert_eq!(reply::field(&seen[1].params, "oauth_verifier"), Some("123-456-789"));
    assert_eq!(reply::field(&seen[1].params, "oauth_token"), Some("req-token"));
    assert!(signed_ok(&f.base, &seen[1], "req-secret"), "access token: signed with the request token's secret");
}

#[test]
fn a_refused_sign_in_says_why() {
    let f = fake(|_, _| (401, "oauth_problem=verifier_invalid".into()));
    let c = client(&f);
    let req = RequestToken { token: "t".into(), secret: "s".into() };
    let e = c.access_token(&req, "111-222-333", &AtomicBool::new(false)).unwrap_err();
    assert!(e.to_string().contains("did not accept the code"), "{e}");
    assert!(c.access_token(&req, "  ", &AtomicBool::new(false)).is_err(), "an empty code is refused before asking Flickr");
    let no_key = Client::new(ApiKey::default());
    assert!(no_key.request_token(&AtomicBool::new(false)).is_err());
}

#[test]
fn uploads_a_signed_photo_with_its_details() {
    let f = fake(|_, _| (200, "<?xml version=\"1.0\" encoding=\"utf-8\" ?>\n<rsp stat=\"ok\">\n<photoid>987654</photoid>\n</rsp>".into()));
    let c = client(&f);
    let info = PhotoInfo {
        title: "The Tetons & the Snake River".into(),
        description: "Developed in LightCraft".into(),
        tags: vec!["grand teton".into(), "Wyoming".into(), "wyoming".into(), "\"quoted\"".into(), " ".into()],
        privacy: Privacy::FriendsAndFamily,
        hidden: true,
    };
    let bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let id = c.upload(&token(), &info, "tetons.jpg", &bytes, &AtomicBool::new(false)).unwrap();
    assert_eq!(id, "987654");
    assert_eq!(photo_url("123@N01", &id), "https://www.flickr.com/photos/123%40N01/987654");
    let seen = f.seen.lock().unwrap();
    let r = &seen[0];
    assert_eq!((r.method.as_str(), r.path.as_str()), ("POST", "/services/upload/"));
    let p = |k: &str| reply::field(&r.params, k).map(str::to_string);
    assert_eq!(p("title").as_deref(), Some("The Tetons & the Snake River"));
    assert_eq!(p("description").as_deref(), Some("Developed in LightCraft"));
    assert_eq!(p("tags").as_deref(), Some("\"grand teton\" Wyoming quoted"));
    assert_eq!((p("is_public"), p("is_friend"), p("is_family")), (Some("0".into()), Some("1".into()), Some("1".into())));
    assert_eq!(p("hidden").as_deref(), Some("2"));
    assert_eq!(p("oauth_token").as_deref(), Some("access-token"));
    assert!(signed_ok(&f.base, r, "access-secret"), "every field but the photo is signed");
    let (name, ct, got) = r.photo.clone().unwrap();
    assert_eq!((name.as_str(), ct.as_str()), ("tetons.jpg", "image/jpeg"));
    assert_eq!(got, bytes);
}

#[test]
fn transient_failures_are_retried_and_account_failures_are_not() {
    // Flickr busy twice, then fine
    let f = fake(|i, _| if i < 2 { (503, "busy".into()) } else { (200, "<rsp stat=\"ok\"><photoid>5</photoid></rsp>".into()) });
    let c = client(&f);
    assert_eq!(c.upload(&token(), &PhotoInfo::default(), "a.jpg", b"x", &AtomicBool::new(false)).unwrap(), "5");
    assert_eq!(f.seen.lock().unwrap().len(), 3);
    // a revoked sign-in: asked once, and the error says to sign in again
    let f = fake(|_, _| (200, "<rsp stat=\"fail\"><err code=\"98\" msg=\"Invalid auth token\" /></rsp>".into()));
    let e = client(&f).upload(&token(), &PhotoInfo::default(), "a.jpg", b"x", &AtomicBool::new(false)).unwrap_err();
    assert!(e.needs_sign_in() && !e.is_transient(), "{e:?}");
    assert!(e.to_string().contains("sign in to Flickr again"), "{e}");
    assert_eq!(f.seen.lock().unwrap().len(), 1);
}

#[test]
fn a_lost_answer_is_not_retried() {
    // Flickr takes the whole photo, then the answer never comes (or a gateway gives up):
    // the photo may be on Flickr already, so it is sent once and the error says so
    for (delay, status) in [(Duration::from_secs(5), 200), (Duration::ZERO, 502), (Duration::ZERO, 504)] {
        let seen = Arc::new(Mutex::new(0usize));
        let count = seen.clone();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { break };
                let _ = read_request(&mut s);
                *count.lock().unwrap() += 1;
                std::thread::sleep(delay);
                let _ = write!(s, "HTTP/1.1 {status} X\r\nContent-Length: 0\r\n\r\n");
            }
        });
        let mut c = Client::new(ApiKey { key: KEY.into(), secret: SECRET.into() });
        c.endpoints = Endpoints::all(&base);
        c.stall_timeout = Duration::from_millis(300);
        c.retry_pause = Duration::from_millis(10);
        let e = c.upload(&token(), &PhotoInfo::default(), "a.jpg", b"x", &AtomicBool::new(false)).unwrap_err();
        assert!(e.outcome_unknown() && !e.is_transient(), "{status}: {e:?}");
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(*seen.lock().unwrap(), 1, "HTTP {status}: sent once, not retried");
    }
    // no connection at all: nothing reached Flickr, so it is safe to try again
    let closed = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let e = Error::Http(lightcraft_fetch::http::HttpError::Connect(format!("{closed}: refused")));
    assert!(e.is_transient() && !e.outcome_unknown());
    assert!(!Error::Flickr { code: 3, message: "General upload failure".into() }.is_transient());
}

#[test]
fn a_cancelled_upload_stops() {
    let f = fake(|_, _| (503, "busy".into()));
    let mut c = client(&f);
    c.retry_pause = Duration::from_secs(30);
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    let t0 = std::time::Instant::now();
    let e = c.upload(&token(), &PhotoInfo::default(), "a.jpg", b"x", &cancel).unwrap_err();
    assert!(e.is_cancelled(), "{e:?}");
    assert!(t0.elapsed() < Duration::from_secs(10), "the retry pause wakes up for the cancel");
}

#[test]
fn tag_and_file_name_edge_cases() {
    assert_eq!(tags_argument(&[]), "");
    assert_eq!(tags_argument(&["  a   b ".into(), "A B".into(), "c".into()]), "\"a b\" c");
    let body = multipart("B", &[], "we\"ird\r\nname.png", b"x");
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("filename=\"weirdname.png\"") && text.contains("Content-Type: image/png"), "{text}");
    let body = multipart("B", &[], "", b"x");
    assert!(String::from_utf8_lossy(&body).contains("filename=\"photo.jpg\""));
    assert_eq!(Privacy::parse("friendsAndFamily"), Some(Privacy::FriendsAndFamily));
    assert_eq!(Privacy::parse("everyone"), None);
}

#[test]
fn lists_the_accounts_albums_across_pages() {
    let f = fake(|_, req| {
        let page = reply::field(&req.params, "page").unwrap_or("1").to_string();
        let body = if page == "1" {
            r#"{"photosets":{"page":1,"pages":2,"perpage":500,"total":3,"photoset":[
                {"id":"72157600000000001","title":{"_content":"Grand Teton"},"photos":"12","count_photos":12},
                {"id":72157600000000002,"title":{"_content":"Dunes & Deserts"},"photos":3},
                {"title":{"_content":"no id: skipped"},"photos":1}]},"stat":"ok"}"#
        } else {
            r#"{"photosets":{"page":"2","pages":"2","photoset":[{"id":"72157600000000003","title":"Plain title","count_photos":"0"}]},"stat":"ok"}"#
        };
        (200, body.to_string())
    });
    let albums = client(&f).list_albums(&token(), &AtomicBool::new(false)).unwrap();
    let got: Vec<(&str, &str, u64)> = albums.iter().map(|a| (a.id.as_str(), a.title.as_str(), a.photos)).collect();
    assert_eq!(got, [("72157600000000001", "Grand Teton", 12), ("72157600000000002", "Dunes & Deserts", 3), ("72157600000000003", "Plain title", 0)]);
    let seen = f.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    for r in seen.iter() {
        assert_eq!((r.method.as_str(), r.path.as_str()), ("GET", "/services/rest/"));
        assert_eq!(reply::field(&r.params, "method"), Some("flickr.photosets.getList"));
        assert_eq!(reply::field(&r.params, "nojsoncallback"), Some("1"));
        assert!(signed_ok(&f.base, r, "access-secret"));
    }
}

#[test]
fn adds_a_photo_to_an_album() {
    let f = fake(|i, _| match i {
        0 => (200, r#"{"stat":"ok"}"#.into()),
        1 => (200, r#"{"stat":"fail","code":3,"message":"Photo already in set"}"#.into()),
        _ => (200, r#"{"stat":"fail","code":1,"message":"Photoset not found"}"#.into()),
    });
    let c = client(&f);
    let cancel = AtomicBool::new(false);
    c.add_to_album(&token(), "72157600000000001", "987654", &cancel).unwrap();
    c.add_to_album(&token(), "72157600000000001", "987654", &cancel).expect("already in the album is fine");
    let e = c.add_to_album(&token(), "gone", "987654", &cancel).unwrap_err();
    assert!(e.to_string().contains("Photoset not found"), "{e}");
    let seen = f.seen.lock().unwrap();
    let r = &seen[0];
    assert_eq!((r.method.as_str(), r.path.as_str()), ("POST", "/services/rest/"));
    assert_eq!(reply::field(&r.params, "method"), Some("flickr.photosets.addPhoto"));
    assert_eq!((reply::field(&r.params, "photoset_id"), reply::field(&r.params, "photo_id")), (Some("72157600000000001"), Some("987654")));
    assert!(signed_ok(&f.base, r, "access-secret"), "a change is a signed form POST");
}

#[test]
fn creates_an_album_around_an_uploaded_photo() {
    let f = fake(|i, _| match i {
        0 => (200, r#"{"photoset":{"id":"72157700000000009","url":"https://www.flickr.com/photos/me/sets/72157700000000009/"},"stat":"ok"}"#.into()),
        _ => (200, r#"{"stat":"fail","code":2,"message":"Invalid primary photo id"}"#.into()),
    });
    let c = client(&f);
    let cancel = AtomicBool::new(false);
    let id = c.create_album(&token(), "  Wyoming & Montana 2026 ", "987654", &cancel).unwrap();
    assert_eq!(id, "72157700000000009");
    let e = c.create_album(&token(), "Trip", "nope", &cancel).unwrap_err();
    assert!(e.to_string().contains("Invalid primary photo id"), "{e}");
    assert!(c.create_album(&token(), "   ", "987654", &cancel).is_err(), "a name is needed, before asking Flickr");
    let seen = f.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    let r = &seen[0];
    assert_eq!((r.method.as_str(), reply::field(&r.params, "method")), ("POST", Some("flickr.photosets.create")));
    assert_eq!((reply::field(&r.params, "title"), reply::field(&r.params, "primary_photo_id")), (Some("Wyoming & Montana 2026"), Some("987654")));
    assert!(signed_ok(&f.base, r, "access-secret"));
}
