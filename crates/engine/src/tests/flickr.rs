//! Flickr commands against a local stand-in for Flickr (never the internet): sign-in, the saved
//! account, and uploads of demo photos rendered with their edits.

#![cfg(not(target_arch = "wasm32"))]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::Session;

/// A request the stand-in received: its path and body.
#[derive(Clone, Debug)]
struct Seen {
    path: String,
    body: Vec<u8>,
}

/// Answers request `i` with `reply(i, path)` → (status, body).
fn fake(reply: impl Fn(usize, &str) -> (u16, String) + Send + Sync + 'static) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for (i, s) in l.incoming().enumerate() {
            let Ok(mut s) = s else { break };
            let mut got = Vec::new();
            let mut buf = [0u8; 65536];
            let head_end = loop {
                let n = s.read(&mut buf).unwrap();
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
            let path = head.split(' ').nth(1).unwrap().split('?').next().unwrap().to_string();
            log.lock().unwrap().push(Seen { path: path.clone(), body: got[head_end..].to_vec() });
            let (status, body) = reply(i, &path);
            let _ = write!(s, "HTTP/1.1 {status} X\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        }
    });
    (base, seen)
}

/// The stand-in's answers: a sign-in, then uploads numbered from 1000.
fn flickr_ok(_: usize, path: &str) -> (u16, String) {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1000);
    match path {
        "/services/oauth/request_token" => (200, "oauth_callback_confirmed=true&oauth_token=req&oauth_token_secret=reqsecret".into()),
        "/services/oauth/access_token" => {
            (200, "fullname=Dorothea%20L&oauth_token=acc&oauth_token_secret=accsecret&user_nsid=42%40N01&username=dorothea".into())
        }
        // albums: the list, and adding a photo (both "ok")
        "/services/rest/" => {
            (200, r#"{"photosets":{"page":1,"pages":1,"photoset":[{"id":"111","title":{"_content":"Showcase"},"photos":2}]},"stat":"ok"}"#.into())
        }
        _ => (200, format!("<rsp stat=\"ok\"><photoid>{}</photoid></rsp>", N.fetch_add(1, std::sync::atomic::Ordering::Relaxed))),
    }
}

fn session(base: &str) -> Session {
    let mut s = Session::with_demo();
    s.flickr.endpoints = lightcraft_flickr::Endpoints::all(base);
    s
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-engine-flickr-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
fn signs_in_saves_the_account_and_signs_out() {
    let (base, seen) = fake(flickr_ok);
    let dir = tmp("signin");
    let file = dir.join("flickr.json");
    let mut s = session(&base);
    s.set_flickr_account_file(file.clone());
    let st = s.execute("flickr.status", &json!({})).unwrap();
    assert_eq!((st["signedIn"].as_bool(), st["saved"].as_bool(), st["pending"].as_bool()), (Some(false), Some(true), Some(false)));
    // no key yet (unless this build carries one)
    if st["builtinKey"] == false {
        let e = s.execute("flickr.signIn", &json!({})).unwrap_err().to_string();
        assert!(e.contains("API key"), "{e}");
    }
    assert!(s.execute("flickr.setApiKey", &json!({"key": "abc"})).is_err(), "a key without its secret");
    assert!(s.execute("flickr.setApiKey", &json!({"key": "a b", "secret": "c"})).is_err(), "spaces");
    s.execute("flickr.setApiKey", &json!({"key": "mykey", "secret": "mysecret"})).unwrap();
    assert!(s.execute("flickr.completeSignIn", &json!({"code": "1-2-3"})).is_err(), "no sign-in started");
    let r = s.execute("flickr.signIn", &json!({})).unwrap();
    assert_eq!(r["authorizeUrl"], format!("{base}/services/oauth/authorize?oauth_token=req&perms=write"));
    assert_eq!(s.flickr_status()["pending"], true);
    let st = s.execute("flickr.completeSignIn", &json!({"code": "123-456-789"})).unwrap();
    assert_eq!((st["signedIn"].as_bool(), st["username"].as_str(), st["pending"].as_bool()), (Some(true), Some("dorothea"), Some(false)));
    assert_eq!(seen.lock().unwrap().len(), 2);

    // a new session on the same file is signed in
    let mut again = session(&base);
    again.set_flickr_account_file(file.clone());
    assert_eq!(again.flickr_status()["username"], "dorothea");
    assert_eq!(again.flickr_status()["apiKey"], "mykey");
    // a different key signs out (the sign-in belongs to the key)
    again.execute("flickr.setApiKey", &json!({"key": "other", "secret": "x"})).unwrap();
    assert_eq!(again.flickr_status()["signedIn"], false);
    s.execute("flickr.signOut", &json!({})).unwrap();
    let mut third = session(&base);
    third.set_flickr_account_file(file.clone());
    assert_eq!(third.flickr_status()["signedIn"], false);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_damaged_account_file_starts_signed_out_and_says_so() {
    let dir = tmp("damaged");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("flickr.json");
    std::fs::write(&file, "{{{").unwrap();
    let mut s = Session::new();
    s.set_flickr_account_file(file.clone());
    let st = s.flickr_status();
    assert_eq!(st["signedIn"], false);
    assert!(st["problem"].as_str().unwrap().contains("flickr.json"), "{st}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "{{{", "left alone until something is saved");
    let _ = std::fs::remove_dir_all(&dir);
}

fn signed_in(base: &str) -> Session {
    let mut s = session(base);
    s.execute("flickr.setApiKey", &json!({"key": "k", "secret": "s"})).unwrap();
    s.flickr_finish_sign_in(lightcraft_flickr::Token {
        token: "acc".into(),
        secret: "accsecret".into(),
        user_nsid: "42@N01".into(),
        username: "dorothea".into(),
        fullname: String::new(),
    })
    .unwrap();
    s
}

#[test]
fn uploads_rendered_photos_with_their_titles_captions_and_keywords() {
    let (base, seen) = fake(flickr_ok);
    let mut s = signed_in(&base);
    let ids: Vec<u64> = s.catalog.photos().take(2).map(|p| p.id.0).collect();
    s.execute("photo.setMeta", &json!({"ids": [ids[0]], "title": "Morning Fog", "caption": "Over the lake", "keywords": ["fog", "lake shore"]}))
        .unwrap();
    s.execute("photo.setMeta", &json!({"ids": [ids[1]], "title": ""})).unwrap();
    let r = s.execute("flickr.upload", &json!({"ids": ids, "longEdge": 320, "privacy": "friends"})).unwrap();
    assert_eq!((r["uploaded"].as_u64(), r["failed"].as_u64(), r["cancelled"].as_bool()), (Some(2), Some(0), Some(false)), "{r}");
    let urls: Vec<&str> = r["photos"].as_array().unwrap().iter().map(|p| p["url"].as_str().unwrap()).collect();
    assert!(urls.iter().all(|u| u.starts_with("https://www.flickr.com/photos/42%40N01/")), "{urls:?}");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    let first = String::from_utf8_lossy(&seen[0].body).to_string();
    for want in ["Morning Fog", "Over the lake", "name=\"is_friend\"\r\n\r\n1"] {
        assert!(first.contains(want), "{want} missing from the upload");
    }
    // keywords as tags (in the library's keyword order), the one with a space quoted
    let tags = first.split("name=\"tags\"\r\n\r\n").nth(1).and_then(|t| t.split("\r\n").next()).unwrap();
    assert!(["fog \"lake shore\"", "\"lake shore\" fog"].contains(&tags), "{tags}");
    assert!(seen.iter().all(|r| r.path == "/services/upload/"));
    // the photo part is a JPEG rendered at the asked size
    let at = seen[0].body.windows(4).position(|w| w == [0xFF, 0xD8, 0xFF, 0xE0] || w == [0xFF, 0xD8, 0xFF, 0xE1]).expect("a JPEG in the upload");
    let jpeg = &seen[0].body[at..];
    let img = lightcraft_codecs::decode(jpeg, lightcraft_codecs::DecodeOptions::default()).unwrap();
    assert_eq!(img.image.width.max(img.image.height), 320);
    // an untitled photo goes up under its file name
    let second = String::from_utf8_lossy(&seen[1].body).to_string();
    let name = s.catalog.photo(lightcraft_catalog::PhotoId(ids[1])).unwrap().file_name.clone();
    let stem = std::path::Path::new(&name).file_stem().unwrap().to_string_lossy().to_string();
    assert!(second.contains(&format!("name=\"title\"\r\n\r\n{stem}\r\n")), "title from {name}");
    assert_eq!(s.flickr_status()["lastUpload"]["uploaded"], 2);
}

#[test]
fn only_you_uploads_private_on_the_wire() {
    let (base, seen) = fake(flickr_ok);
    let mut s = signed_in(&base);
    let id = s.catalog.photos().next().unwrap().id.0;
    let r = s.execute("flickr.upload", &json!({"ids": [id], "longEdge": 128, "privacy": "private"})).unwrap();
    assert_eq!(r["uploaded"].as_u64(), Some(1), "{r}");
    let seen = seen.lock().unwrap();
    let body = String::from_utf8_lossy(&seen[0].body).to_string();
    for field in ["is_public", "is_friend", "is_family"] {
        assert!(body.contains(&format!("name=\"{field}\"\r\n\r\n0\r\n")), "{field} is 0 for Only you");
    }
}

#[test]
fn a_refused_sign_in_stops_the_upload_and_signs_out() {
    let (base, seen) = fake(|_, _| (200, "<rsp stat=\"fail\"><err code=\"98\" msg=\"Invalid auth token\" /></rsp>".into()));
    let mut s = signed_in(&base);
    let ids: Vec<u64> = s.catalog.photos().take(3).map(|p| p.id.0).collect();
    let r = s.execute("flickr.upload", &json!({"ids": ids, "longEdge": 128})).unwrap();
    assert_eq!((r["uploaded"].as_u64(), r["failed"].as_u64(), r["signInNeeded"].as_bool()), (Some(0), Some(1), Some(true)), "{r}");
    assert_eq!(seen.lock().unwrap().len(), 1, "the other photos are not tried");
    assert_eq!(s.flickr_status()["signedIn"], false);
}

#[test]
fn upload_needs_a_sign_in_photos_and_sane_options() {
    let mut s = Session::with_demo();
    let id = s.catalog.photos().next().unwrap().id.0;
    s.execute("flickr.setApiKey", &json!({"key": "k", "secret": "s"})).unwrap();
    let e = s.execute("flickr.upload", &json!({"ids": [id]})).unwrap_err().to_string();
    assert!(e.contains("sign in"), "{e}");
    let mut s = signed_in("http://127.0.0.1:9");
    for (p, why) in [
        (json!({"ids": [id], "privacy": "everyone"}), "privacy"),
        (json!({"ids": [id], "quality": 0}), "quality"),
        (json!({"ids": [id], "longEdge": 10}), "longEdge"),
        (json!({"ids": [id], "tags": "yes"}), "tags"),
        (json!({"ids": [999999]}), "no photo"),
    ] {
        let e = s.execute("flickr.upload", &p).unwrap_err().to_string();
        assert!(e.contains(why), "{p}: {e}");
    }
    let many: Vec<Value> = (0..=crate::flickr::MAX_PHOTOS as u64).map(|i| json!(i)).collect();
    assert!(s.execute("flickr.upload", &json!({"ids": many})).unwrap_err().to_string().contains("at most"));
}

#[test]
fn upload_options_render_like_an_export() {
    let o = crate::flickr::UploadOptions::from_json(&json!({"longEdge": 0, "quality": 80, "removeLocation": false})).unwrap();
    let e = o.export_options();
    assert_eq!((e.resize, e.quality, e.remove_location), (None, 80, false));
    let d = crate::flickr::UploadOptions::default().export_options();
    assert_eq!((d.format, d.color_space, d.remove_location), (crate::export::ExportFormat::Jpeg, crate::export::OutputSpace::Srgb, true));
    assert_eq!(d.resize.map(|r| r.value), Some(2048.0));
    crate::export::ExportOptions::validate("flickr.upload", &serde_json::to_value(&d).unwrap()).unwrap();
}

#[test]
fn lists_albums_and_uploads_into_one() {
    let (base, seen) = fake(flickr_ok);
    let mut s = signed_in(&base);
    let r = s.execute("flickr.albums", &json!({})).unwrap();
    assert_eq!(r["albums"], json!([{"id": "111", "title": "Showcase", "photos": 2}]));
    let ids: Vec<u64> = s.catalog.photos().take(2).map(|p| p.id.0).collect();
    let r = s.execute("flickr.upload", &json!({"ids": ids, "longEdge": 128, "album": "111"})).unwrap();
    assert_eq!((r["uploaded"].as_u64(), r["albumFailed"].as_u64(), r["album"].as_str()), (Some(2), Some(0), Some("111")), "{r}");
    let seen = seen.lock().unwrap();
    let adds: Vec<String> = seen
        .iter()
        .filter(|q| q.path == "/services/rest/" && String::from_utf8_lossy(&q.body).contains("addPhoto"))
        .map(|q| String::from_utf8_lossy(&q.body).to_string())
        .collect();
    assert_eq!(adds.len(), 2, "each uploaded photo joins the album");
    let photo_ids: Vec<String> = r["photos"].as_array().unwrap().iter().map(|p| p["photoId"].as_str().unwrap().to_string()).collect();
    for (add, id) in adds.iter().zip(&photo_ids) {
        assert!(add.contains("photoset_id=111") && add.contains(&format!("photo_id={id}")), "{add}");
    }
    drop(seen);
    let r = s.execute("flickr.upload", &json!({"ids": [ids[0]], "longEdge": 128, "album": null})).unwrap();
    assert_eq!(r["album"], Value::Null);
}

#[test]
fn a_photo_that_cannot_join_the_album_still_counts_as_uploaded() {
    let (base, _) = fake(|_, path| match path {
        "/services/rest/" => (200, r#"{"stat":"fail","code":1,"message":"Photoset not found"}"#.into()),
        _ => flickr_ok(0, path),
    });
    let mut s = signed_in(&base);
    let ids: Vec<u64> = s.catalog.photos().take(2).map(|p| p.id.0).collect();
    let r = s.execute("flickr.upload", &json!({"ids": ids, "longEdge": 128, "album": "999"})).unwrap();
    assert_eq!((r["uploaded"].as_u64(), r["failed"].as_u64(), r["albumFailed"].as_u64()), (Some(2), Some(0), Some(2)), "{r}");
    assert!(r["photos"][0]["albumError"].as_str().unwrap().contains("Photoset not found"));
    for bad in [json!(12), json!("a b"), json!("x".repeat(65))] {
        let e = s.execute("flickr.upload", &json!({"ids": ids, "album": bad})).unwrap_err().to_string();
        assert!(e.contains("album"), "{e}");
    }
}

#[test]
fn makes_a_new_album_around_the_first_photo() {
    let (base, seen) = fake(|_, path| match path {
        // made: the answer to flickr.photosets.create (addPhoto takes any "ok")
        "/services/rest/" => (200, r#"{"photoset":{"id":"777","url":"x"},"stat":"ok"}"#.into()),
        _ => flickr_ok(0, path),
    });
    let mut s = signed_in(&base);
    let ids: Vec<u64> = s.catalog.photos().take(3).map(|p| p.id.0).collect();
    let r = s.execute("flickr.upload", &json!({"ids": ids, "longEdge": 128, "newAlbum": " Road Trip "})).unwrap();
    assert_eq!((r["uploaded"].as_u64(), r["albumFailed"].as_u64(), r["album"].as_str()), (Some(3), Some(0), Some("777")), "{r}");
    assert_eq!(r["createdAlbum"], json!({"id": "777", "title": "Road Trip"}));
    let first = r["photos"][0]["photoId"].as_str().unwrap().to_string();
    let rest: Vec<String> =
        seen.lock().unwrap().iter().filter(|q| q.path == "/services/rest/").map(|q| String::from_utf8_lossy(&q.body).to_string()).collect();
    assert_eq!(rest.len(), 3, "one create, then the two other photos added: {rest:?}");
    assert!(rest[0].contains("photosets.create") && rest[0].contains("title=Road%20Trip") && rest[0].contains(&format!("primary_photo_id={first}")));
    assert!(rest[1..].iter().all(|b| b.contains("photosets.addPhoto") && b.contains("photoset_id=777")));
}

#[test]
fn a_new_album_that_cannot_be_made_leaves_the_photos_uploaded() {
    let (base, seen) = fake(|_, path| match path {
        "/services/rest/" => (200, r#"{"stat":"fail","code":3,"message":"User has reached the maximum number of photosets"}"#.into()),
        _ => flickr_ok(0, path),
    });
    let mut s = signed_in(&base);
    let ids: Vec<u64> = s.catalog.photos().take(2).map(|p| p.id.0).collect();
    let r = s.execute("flickr.upload", &json!({"ids": ids, "longEdge": 128, "newAlbum": "Too Many"})).unwrap();
    assert_eq!((r["uploaded"].as_u64(), r["albumFailed"].as_u64(), r["album"].as_str()), (Some(2), Some(2), None), "{r}");
    assert!(r["albumCreateError"].as_str().unwrap().contains("can't have more albums"), "{r}");
    assert_eq!(r["createdAlbum"], Value::Null);
    let creates = seen.lock().unwrap().iter().filter(|q| q.path == "/services/rest/").count();
    assert_eq!(creates, 1, "tried once, not once per photo");
    // names and combinations that aren't accepted
    for (p, why) in [
        (json!({"ids": ids, "newAlbum": "  "}), "name"),
        (json!({"ids": ids, "newAlbum": "x".repeat(256)}), "255"),
        (json!({"ids": ids, "newAlbum": "A", "album": "111"}), "not both"),
    ] {
        let e = s.execute("flickr.upload", &p).unwrap_err().to_string();
        assert!(e.contains(why), "{p}: {e}");
    }
}

#[test]
fn keywords_left_out_of_export_never_become_tags() {
    let (base, seen) = fake(flickr_ok);
    let mut s = signed_in(&base);
    let id = s.catalog.photos().next().unwrap().id.0;
    s.execute("photo.setMeta", &json!({"ids": [id], "keywords": ["private client", "Places|Wyoming", "dunes"]})).unwrap();
    s.execute("keyword.edit", &json!({"keyword": "private client", "includeOnExport": false})).unwrap();
    let info = crate::flickr::photo_info(s.catalog.photo(lightcraft_catalog::PhotoId(id)).unwrap(), &s.catalog, &Default::default());
    assert!(!info.tags.iter().any(|t| t.contains("private")), "{:?}", info.tags);
    assert!(!info.tags.iter().any(|t| t.contains('|')), "a nested keyword goes by its name: {:?}", info.tags);
    assert!(info.tags.iter().any(|t| t == "Wyoming") && info.tags.iter().any(|t| t == "dunes"), "{:?}", info.tags);
    // and what goes to Flickr is what the exported file carries
    s.execute("flickr.upload", &json!({"ids": [id], "longEdge": 128})).unwrap();
    let body = String::from_utf8_lossy(&seen.lock().unwrap()[0].body).to_string();
    let tags = body.split("name=\"tags\"\r\n\r\n").nth(1).and_then(|t| t.split("\r\n").next()).unwrap_or_default().to_string();
    assert!(!tags.contains("private") && !tags.contains('|') && tags.contains("Wyoming"), "{tags}");
}

#[test]
fn a_refused_sign_in_signs_out_only_the_account_that_uploaded() {
    let mut s = signed_in("http://127.0.0.1:9");
    let refused = json!({"signInNeeded": true});
    // the upload used an earlier sign-in; someone else has signed in since: they stay
    s.flickr_record_upload(&refused, "an-older-token");
    assert_eq!(s.flickr_status()["signedIn"], true);
    // the sign-in that was refused is the current one: forgotten
    s.flickr_record_upload(&refused, "acc");
    assert_eq!(s.flickr_status()["signedIn"], false);
}
