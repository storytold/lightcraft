//! SHA-1 at import and its back-fill, and the Immich commands against an in-process server that
//! speaks the endpoints the client uses (bodies shaped like a v3.3.1 server's).
#![cfg(not(target_arch = "wasm32"))]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lcc_catalog::{Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

use crate::Session;
use crate::remote::{LINK_ONLY, MemorySecrets};

pub(crate) const KEY: &str = "engine-test-key-42";

/// A folder in Temp that stays (the sync tests keep its path in their `Setup`).
fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lcc-immich-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The test's folder in Temp, removed when the test ends.
fn scratch(tag: &str) -> lcc_testkit::TempDir {
    lcc_testkit::TempDir::new(&format!("lcc-immich-{tag}"))
}

pub(crate) fn png(seed: u8) -> Vec<u8> {
    let (w, h) = (40usize, 30usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 5) as u8, (i / w * 7) as u8, seed, 255]).collect();
    let img = lcc_raster::Rgba8 { width: w, height: h, data };
    lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap()
}

fn b64(bytes: &[u8]) -> String {
    lcc_hash::sha1_bytes(bytes).to_base64()
}

#[derive(Default)]
struct Fake {
    assets: Vec<(Value, Vec<u8>)>,
    /// Asset id → the XMP sidecar the server holds for it.
    sidecars: std::collections::HashMap<String, Vec<u8>>,
    /// The version `/server/version` answers (`None`: v3.3.1).
    version: Option<Value>,
    /// The account's unlock PIN and password (`None`: no such secret), and whether the session is
    /// elevated (locked-folder assets are listed, `GET /auth/status` says so).
    pin: Option<String>,
    password: Option<String>,
    elevated: bool,
}

fn read_req(s: impl Read) -> Option<(String, String, Option<String>, Vec<u8>)> {
    let mut r = BufReader::new(s);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let mut it = line.split_whitespace();
    let (m, t) = (it.next()?.to_string(), it.next()?.to_string());
    let (mut len, mut key) = (0usize, None);
    loop {
        let mut l = String::new();
        r.read_line(&mut l).ok()?;
        let l = l.trim_end();
        if l.is_empty() {
            break;
        }
        let (k, v) = l.split_once(':')?;
        if k.eq_ignore_ascii_case("content-length") {
            len = v.trim().parse().ok()?;
        }
        if k.eq_ignore_ascii_case("x-api-key") {
            key = Some(v.trim().to_string());
        }
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).ok()?;
    Some((m, t, key, body))
}

fn resp(status: &str, ctype: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: {ctype}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out
}

fn serve(fake: Fake) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        let mut fake = fake;
        for s in listener.incoming() {
            let Ok(mut s) = s else { return };
            let Some((m, t, key, body)) = read_req(&mut s) else { continue };
            seen2.lock().unwrap().push(format!("{m} {t}"));
            let path = t.split('?').next().unwrap_or_default().to_string();
            let query = t.split_once('?').map(|(_, q)| q).unwrap_or_default();
            let param = |name: &str| {
                query.split('&').find_map(|kv| {
                    let (k, v) = kv.split_once('=')?;
                    (k == name).then(|| v.to_string())
                })
            };
            let json = |v: Value| resp("200 OK", "application/json", v.to_string().as_bytes());
            let out = if path == "/api/server/version" {
                json(fake.version.clone().unwrap_or(json!({"major": 3, "minor": 3, "patch": 1})))
            } else if key.as_deref() != Some(KEY) {
                resp("401 Unauthorized", "application/json", br#"{"message":"Invalid API key"}"#)
            } else if path == "/api/users/me" {
                json(json!({"id": "user-1", "email": "me@example.invalid", "name": "Me", "isAdmin": false}))
            } else if path == "/api/api-keys/me" {
                json(json!({"id": "k", "name": "app", "permissions": ["asset.read", "asset.view", "asset.download"]}))
            } else if path == "/api/auth/status" {
                json(json!({"isElevated": fake.elevated, "password": fake.password.is_some(), "pinCode": fake.pin.is_some()}))
            } else if path == "/api/auth/session/unlock" {
                let b: Value = serde_json::from_slice(&body).unwrap_or_default();
                let pin = b["pinCode"].as_str().is_some_and(|p| fake.pin.as_deref() == Some(p));
                let pass = b["password"].as_str().is_some_and(|p| fake.password.as_deref() == Some(p));
                if pin || pass {
                    fake.elevated = true;
                    resp("204 No Content", "application/json", b"")
                } else {
                    resp(
                        "401 Unauthorized",
                        "application/json",
                        br#"{"message":"Wrong password or PIN code","statusCode":401,"error":"Unauthorized"}"#,
                    )
                }
            } else if path == "/api/search/metadata" {
                let q: Value = serde_json::from_slice(&body).unwrap_or_default();
                // a locked session leaves locked-folder assets out of the listing
                let items: Vec<Value> = if q["page"].as_u64().unwrap_or(1) == 1 {
                    fake.assets.iter().filter(|(a, _)| fake.elevated || a["visibility"].as_str() != Some("locked")).map(|(a, _)| a.clone()).collect()
                } else {
                    vec![]
                };
                json(json!({"assets": {"total": items.len(), "count": items.len(), "items": items, "nextPage": null}}))
            } else if path == "/api/asset-files" {
                let asset = param("assetId").unwrap_or_default();
                let files: Vec<Value> = match fake.sidecars.get(&asset) {
                    Some(_) => vec![json!({"id": format!("sidecar-{asset}"), "type": "sidecar", "path": format!("/x/{asset}.xmp")})],
                    None => Vec::new(),
                };
                json(Value::Array(files))
            } else if let Some(id) = path.strip_prefix("/api/asset-files/").and_then(|r| r.strip_suffix("/download")) {
                match id.strip_prefix("sidecar-").and_then(|a| fake.sidecars.get(a).map(|x| (a, x))) {
                    Some((_, xml)) => resp("200 OK", "application/xml", xml),
                    None => resp("404 Not Found", "application/json", b"{}"),
                }
            } else if path == "/api/tags" && m == "PUT" {
                let b: Value = serde_json::from_slice(&body).unwrap_or_default();
                let tags: Vec<Value> = b["tags"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(|v| json!({"id": format!("tag-{}", v.replace('/', "-")), "value": v}))
                    .collect();
                json(Value::Array(tags))
            } else if path.starts_with("/api/tags/") || path == "/api/jobs/sidecar" || path == "/api/assets/jobs" {
                resp("200 OK", "application/json", b"{}")
            } else if let Some(rest) = path.strip_prefix("/api/assets/") {
                let (id, tail) = rest.split_once('/').unwrap_or((rest, ""));
                if m == "PUT" && tail.is_empty() {
                    let update: Value = serde_json::from_slice(&body).unwrap_or_default();
                    let Some((a, _)) = fake.assets.iter_mut().find(|(a, _)| a["id"] == id) else {
                        let _ = s.write_all(&resp("404 Not Found", "application/json", b"{}"));
                        continue;
                    };
                    if !a["exifInfo"].is_object() {
                        a["exifInfo"] = json!({});
                    }
                    for (from, to) in [("rating", "rating"), ("description", "description"), ("latitude", "latitude"), ("longitude", "longitude")] {
                        if let Some(v) = update.get(from) {
                            a["exifInfo"][to] = v.clone();
                        }
                    }
                    if let Some(v) = update.get("isFavorite") {
                        a["isFavorite"] = v.clone();
                    }
                    json(a.clone())
                } else {
                    match fake.assets.iter().find(|(a, _)| a["id"] == id) {
                        Some((a, bytes)) => match tail {
                            "" => json(a.clone()),
                            "original" => resp("200 OK", "image/png", bytes),
                            "thumbnail" => resp("200 OK", "image/png", bytes),
                            _ => resp("404 Not Found", "application/json", b"{}"),
                        },
                        None => resp("404 Not Found", "application/json", b"{}"),
                    }
                }
            } else {
                resp("404 Not Found", "application/json", b"{}")
            };
            let _ = s.write_all(&out);
        }
    });
    (format!("http://127.0.0.1:{port}"), seen)
}

pub(crate) fn asset(id: &str, name: &str, bytes: &[u8], extra: Value) -> Value {
    let mut a = json!({
        "id": id,
        "type": "IMAGE",
        "checksum": b64(bytes),
        "originalFileName": name,
        "localDateTime": "2024-03-04T05:06:07.000Z",
        "updatedAt": format!("2026-10-0{}T00:00:00.000Z", id.len() % 9),
        "isFavorite": false,
        "exifInfo": {"fileSizeInByte": bytes.len()},
    });
    if let (Some(o), Some(e)) = (a.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            o.insert(k.clone(), v.clone());
        }
    }
    a
}

fn session() -> Session {
    let mut s = Session::new().with_fs();
    s.set_secret_store(Arc::new(MemorySecrets::default()));
    s
}

/// Pump until `done` says so (background threads), at most 10 s.
fn pump_until(s: &mut Session, mut done: impl FnMut(&Session, &Value) -> bool) -> Value {
    let t = Instant::now();
    loop {
        let v = s.execute("remote.pump", &json!({})).unwrap();
        if done(s, &v) {
            return v;
        }
        assert!(t.elapsed() < Duration::from_secs(10), "timed out: {v}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn import_records_sha1_and_the_backfill_fills_old_photos() {
    let dir = scratch("sha1");
    let a = dir.join("a.png");
    std::fs::write(&a, png(1)).unwrap();
    let mut s = session();
    s.execute("library.import", &json!({"paths": [a.to_string_lossy()]})).unwrap();
    let p = s.catalog.photos().next().unwrap();
    assert_eq!(p.sha1.as_deref(), Some(lcc_hash::sha1_bytes(&png(1)).to_hex().as_str()));

    // an older catalog: photos without SHA-1 get it in the background
    let b = dir.join("b.png");
    std::fs::write(&b, png(2)).unwrap();
    let id = s.catalog.alloc_photo_id();
    let ph = Photo::new(id, Source::File { path: b.to_string_lossy().into() }, "b.png", "PNG", 40, 30, "now");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(ph) }).unwrap();
    let gone = s.catalog.alloc_photo_id();
    let ph = Photo::new(gone, Source::File { path: dir.join("missing.png").to_string_lossy().into() }, "missing.png", "PNG", 1, 1, "now");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(ph) }).unwrap();
    let v = pump_until(&mut s, |s, v| s.catalog.photo(id).unwrap().sha1.is_some() && v["sha1"]["active"] == false);
    assert_eq!(s.catalog.photo(id).unwrap().sha1.as_deref(), Some(lcc_hash::sha1_bytes(&png(2)).to_hex().as_str()));
    assert_eq!(v["sha1"]["failed"], 1, "the missing file is not retried: {v}");
    assert!(s.catalog.photo(gone).unwrap().sha1.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn connect_link_and_open_in_immich() {
    let dir = scratch("link");
    let (one, two) = (png(10), png(11));
    let f = dir.join("one.png");
    std::fs::write(&f, &one).unwrap();
    let probable = dir.join("IMG_2.png");
    std::fs::write(&probable, png(12)).unwrap();
    let fake = Fake {
        assets: vec![
            (asset("aaaa-1", "renamed-on-server.png", &one, json!({})), one.clone()),
            // different bytes (edited on the server), same name, time and size: probable
            (asset("bbbb-2", "img_2.png", &two, json!({"exifInfo": {"fileSizeInByte": png(12).len()}})), two.clone()),
        ],
        ..Fake::default()
    };
    let (url, _) = serve(fake);
    let mut s = session();
    s.execute("library.import", &json!({"paths": [f.to_string_lossy(), probable.to_string_lossy()]})).unwrap();
    let id_one = s.catalog.photos().find(|p| p.file_name == "one.png").unwrap().id;
    let id_two = s.catalog.photos().find(|p| p.file_name == "IMG_2.png").unwrap().id;
    s.catalog.apply(Op::SetCaptured { id: id_two, captured: Some("2024-03-04T05:06:07".into()) }).unwrap();

    // a bad key: a clear, structured failure, nothing saved
    let r = s.execute("immich.connect", &json!({"url": url, "apiKey": "wrong"})).unwrap();
    assert_eq!(r["ok"], false);
    assert_eq!(r["error"]["kind"], "badKey");
    assert!(s.immich_accounts().unwrap().immich.is_empty());

    let r = s.execute("immich.connect", &json!({"url": url, "apiKey": KEY})).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["version"], "3.3.1");
    assert_eq!(r["missingPermissions"][0]["feature"], "import");
    let account = r["account"].as_str().unwrap().to_string();
    // the key is in the secret store only: not in results, not in the journal
    let st = s.execute("immich.status", &json!({})).unwrap();
    assert!(!st.to_string().contains(KEY));
    assert!(!format!("{:?}", s.journal).contains(KEY));
    assert_eq!(st["accounts"][0]["userName"], "Me");

    s.execute("immich.link", &json!({})).unwrap();
    pump_until(&mut s, |_, v| v["links"][&account]["active"] == false);
    assert_eq!(lcc_immich::link::link_state(&s.catalog, id_one), "linked");
    assert_eq!(lcc_immich::link::link_state(&s.catalog, id_two), "probable");
    let l = s.execute("immich.links", &json!({"id": id_one.0})).unwrap();
    assert_eq!(l["links"][0]["url"], format!("{url}/photos/aaaa-1"));
    // the Library filter
    let probable_ids: Vec<PhotoId> =
        s.catalog.photos().filter(|p| lcc_catalog::query::immich_state_is(&s.catalog, p.id, "probable")).map(|p| p.id).collect();
    assert_eq!(probable_ids, vec![id_two]);
    // confirm the probable one
    assert_eq!(s.execute("immich.confirmLink", &json!({"ids": [id_two.0]})).unwrap()["confirmed"], 1);
    assert_eq!(lcc_immich::link::link_state(&s.catalog, id_two), "linked");
    // the linked filter also works through the session's filter
    s.filter.immich = Some("linked".into());
    assert_eq!(s.visible().len(), 2);
    s.filter.immich = None;

    assert_eq!(s.execute("immich.disconnect", &json!({"forgetLinks": true})).unwrap()["forgotLinks"], 2);
    assert!(s.immich_accounts().unwrap().immich.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn locked_folders_stay_hidden_until_the_session_is_unlocked() {
    let dir = scratch("locked");
    let (normal, hidden_shot) = (png(31), png(32));
    let fake = Fake {
        assets: vec![
            (asset("cccc-3", "beach.png", &normal, json!({})), normal.clone()),
            (asset("dddd-4", "secret.png", &hidden_shot, json!({"visibility": "locked"})), hidden_shot.clone()),
        ],
        pin: Some("2468".into()),
        ..Fake::default()
    };
    let (url, seen) = serve(fake);
    let mut s = session();
    s.execute("immich.connect", &json!({"url": url, "apiKey": KEY})).unwrap();

    // the session starts locked; authStatus says so
    let st = s.execute("immich.authStatus", &json!({})).unwrap();
    assert_eq!(st["ok"], true, "{st}");
    assert_eq!(st["isElevated"], false, "{st}");
    assert_eq!(st["pinCode"], true, "{st}");

    // while locked, the server leaves the locked asset out of its listings
    let br = s.execute("immich.browse", &json!({"source": "timeline"})).unwrap();
    let names: Vec<&str> = br["assets"].as_array().unwrap().iter().filter_map(|a| a["fileName"].as_str()).collect();
    assert_eq!(names, vec!["beach.png"], "{br}");

    // a wrong secret is a clear failure and leaves the session locked
    let r = s.execute("immich.unlockSession", &json!({"secret": "1111"})).unwrap();
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(r["error"]["kind"], "status", "{r}");
    assert!(r["error"]["message"].as_str().is_some_and(|m| m.contains("refused")), "{r}");
    let st = s.execute("immich.authStatus", &json!({})).unwrap();
    assert_eq!(st["isElevated"], false, "{st}");

    // the right PIN unlocks (4–6 digits go as pinCode)
    let r = s.execute("immich.unlockSession", &json!({"secret": "2468"})).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let st = s.execute("immich.authStatus", &json!({})).unwrap();
    assert_eq!(st["isElevated"], true, "{st}");

    // unlocked, the locked asset is listed like any other and can be imported
    let br = s.execute("immich.browse", &json!({"source": "timeline"})).unwrap();
    assert_eq!(br["assets"].as_array().unwrap().len(), 2, "{br}");
    let dest = dir.join("out");
    let r = s.execute("immich.import", &json!({"assets": ["dddd-4"], "destination": dest.to_string_lossy()})).unwrap();
    assert_eq!(r["started"], true, "{r}");
    let v = pump_until(&mut s, |_, v| v["import"]["active"] == false);
    assert_eq!(v["import"]["imported"], 1, "{v}");
    assert!(s.catalog.photos().any(|p| p.file_name == "secret.png"), "{:?}", v["import"]);

    // the secret is nowhere but in the request body: not in the journal, a result or a URL
    assert!(!format!("{:?}", s.journal).contains("2468"), "the PIN must not be journaled");
    assert!(!s.execute("immich.status", &json!({})).unwrap().to_string().contains("2468"));
    assert!(!s.execute("immich.authStatus", &json!({})).unwrap().to_string().contains("2468"));
    let seen = seen.lock().unwrap();
    assert!(seen.iter().all(|r| !r.contains("2468")), "the PIN never goes into a URL: {seen:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_copies_maps_metadata_and_skips_duplicates() {
    let dir = scratch("import");
    let (a, b, c) = (png(20), png(21), png(22));
    let have = dir.join("have.png");
    std::fs::write(&have, &c).unwrap();
    let fake = Fake {
        assets: vec![
            (
                asset(
                    "a-1",
                    "beach.png",
                    &a,
                    json!({"isFavorite": true, "exifInfo": {"rating": 4, "description": "Beach", "fileSizeInByte": a.len()}, "tags": [{"id": "t", "value": "trips/sea"}]}),
                ),
                a.clone(),
            ),
            (asset("b-2", "beach.png", &b, json!({})), b.clone()),
            (asset("c-3", "dupe.png", &c, json!({})), c.clone()),
        ],
        ..Fake::default()
    };
    let (url, _) = serve(fake);
    let mut s = session();
    s.execute("library.import", &json!({"paths": [have.to_string_lossy()]})).unwrap();
    s.execute("immich.connect", &json!({"url": url, "apiKey": KEY})).unwrap();
    let br = s.execute("immich.browse", &json!({"source": "timeline"})).unwrap();
    assert_eq!(br["assets"].as_array().unwrap().len(), 3);
    assert!(br["assets"][2]["inCatalog"].is_u64(), "{br}");
    let dest = dir.join("dest");
    s.execute(
        "immich.import",
        &json!({"assets": ["a-1", "b-2", "c-3"], "mode": "copy", "destination": dest.to_string_lossy(), "albumName": "From Immich"}),
    )
    .unwrap();
    let v = pump_until(&mut s, |_, v| v["import"]["active"] == false);
    assert_eq!(v["import"]["imported"], 2, "{v}");
    assert_eq!(v["import"]["skipped"], 1, "{v}");
    let beach = s.catalog.photos().find(|p| p.remote_of_test(&s.catalog) == Some("a-1".into())).unwrap();
    assert_eq!(beach.rating, 4);
    assert_eq!(beach.flag, lcc_catalog::Flag::Pick);
    assert_eq!(beach.meta.caption, "Beach");
    assert_eq!(beach.meta.keywords, vec!["trips|sea".to_string()]);
    assert_eq!(beach.sha1.as_deref(), Some(lcc_hash::sha1_bytes(&a).to_hex().as_str()));
    // two assets with the same name land side by side in the dated folder
    let names: Vec<String> = s.catalog.photos().map(|p| p.file_name.clone()).collect();
    assert!(names.contains(&"beach.png".to_string()) && names.contains(&"beach-1.png".to_string()), "{names:?}");
    assert!(s.catalog.albums().any(|al| al.name == "From Immich"));
    // the asset the catalog already had is not downloaded again, but it is part of what was asked
    // for: its photo is linked to it (by checksum) and is in the album with the new ones
    assert_eq!(v["import"]["present"], 1, "{v}");
    let had = s.catalog.photos().find(|p| p.file_name == "have.png").unwrap();
    assert_eq!(had.remote_of_test(&s.catalog), Some("c-3".into()), "linked by checksum");
    assert_eq!(lcc_immich::link::link_state(&s.catalog, had.id), "linked");
    let album = s.catalog.albums().find(|al| al.name == "From Immich").unwrap();
    assert_eq!(album.photos.len(), 3, "the two new photos and the one already here");
    assert!(album.photos.contains(&had.id));
    assert_eq!(s.catalog.albums().filter(|al| al.name == "From Immich").count(), 1);
    // importing an album whose assets are all here already still makes the album and fills it
    s.execute("immich.import", &json!({"assets": ["a-1", "c-3"], "mode": "copy", "destination": dest.to_string_lossy(), "albumName": "Again"}))
        .unwrap();
    let v = pump_until(&mut s, |_, v| v["import"]["active"] == false);
    assert_eq!((v["import"]["imported"].as_u64(), v["import"]["present"].as_u64()), (Some(0), Some(2)), "{v}");
    assert_eq!(s.catalog.albums().find(|al| al.name == "Again").map(|al| al.photos.len()), Some(2));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn link_only_downloads_the_original_on_demand() {
    let dir = scratch("linkonly");
    let a = png(30);
    let (url, _) = serve(Fake { assets: vec![(asset("l-1", "far.png", &a, json!({})), a.clone())], ..Fake::default() });
    let mut s = session();
    s.remote.cache_dir = Some(dir.join("cache"));
    s.execute("immich.connect", &json!({"url": url, "apiKey": KEY})).unwrap();
    s.execute("immich.import", &json!({"assets": ["l-1"], "mode": "link"})).unwrap();
    pump_until(&mut s, |_, v| v["import"]["active"] == false);
    let p = s.catalog.photos().next().unwrap().clone();
    assert_eq!(p.preview_only.as_deref(), Some(LINK_ONLY));
    assert!(p.sha1.is_none());
    assert_eq!(s.execute("immich.links", &json!({"id": p.id.0})).unwrap()["linkOnly"], true);
    // as if the preview were a JPEG standing in for a raw: the original's kind and format win
    s.catalog.apply(Op::SetKind { id: p.id, kind: lcc_catalog::MediaKind::Raw, format: "JPEG".into() }).unwrap();
    assert_eq!(s.execute("immich.fetchOriginal", &json!({"id": p.id.0})).unwrap()["started"], true);
    pump_until(&mut s, |s, _| s.remote.fetching.is_empty());
    let q = s.catalog.photo(p.id).unwrap();
    assert!(q.preview_only.is_none(), "{:?}", s.remote.fetch_errors);
    assert_eq!(q.file_name, "far.png");
    assert_eq!((q.kind, q.format.as_str()), (lcc_catalog::MediaKind::Image, "PNG"), "kind and format follow the original");
    assert_eq!(q.sha1.as_deref(), Some(lcc_hash::sha1_bytes(&a).to_hex().as_str()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn offline_and_unknown_accounts_never_crash() {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut s = session();
    let r = s.execute("immich.test", &json!({"url": format!("http://127.0.0.1:{port}"), "apiKey": KEY})).unwrap();
    assert_eq!(r["error"]["kind"], "offline");
    assert_eq!(r["error"]["retryable"], true);
    assert!(s.execute("immich.link", &json!({})).is_err(), "no account");
    assert!(s.execute("immich.connect", &json!({"url": "", "apiKey": KEY})).unwrap()["ok"] == false);
    assert!(s.execute("immich.connect", &json!({"url": "http://x"})).is_err(), "missing key");
    assert!(s.execute("immich.setPathMaps", &json!({"account": "nope", "pathMaps": []})).is_err());
}

#[test]
fn sidecars_are_written_for_mapped_folders_only() {
    let dir = scratch("extlib");
    let inside = dir.join("Photos/x.png");
    let outside = dir.join("Other/y.png");
    for (p, n) in [(&inside, 40), (&outside, 41)] {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, png(n)).unwrap();
    }
    let (url, _) = serve(Fake { assets: vec![], ..Fake::default() });
    let mut s = session();
    s.execute("library.import", &json!({"paths": [inside.to_string_lossy(), outside.to_string_lossy()]})).unwrap();
    s.execute("immich.connect", &json!({"url": url, "apiKey": KEY})).unwrap();
    assert!(s.execute("immich.writeSidecars", &json!({})).is_err(), "no mapping yet");
    let local = dir.join("Photos").to_string_lossy().to_string();
    s.execute("immich.setPathMaps", &json!({"pathMaps": [{"container": "/mnt/photos", "local": local}]})).unwrap();
    let r = s.execute("immich.writeSidecars", &json!({})).unwrap();
    assert_eq!(r["written"], 1, "{r}");
    assert!(dir.join("Photos/x.xmp").exists() || dir.join("Photos/x.png.xmp").exists());
    assert!(!dir.join("Other/y.xmp").exists());
    // the new push writes a mapped photo even before the link pass matched it (the sidecar is a
    // local file, and writing it is what lets Immich see the metadata)
    let r = s.execute("immich.pushSidecars", &json!({"externalOnly": true})).unwrap();
    assert_eq!(r["written"], 1, "{r}");
    // and it does not touch a photo outside the mapping
    assert!(!dir.join("Other/y.xmp").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

trait RemoteOf {
    fn remote_of_test(&self, cat: &lcc_catalog::Catalog) -> Option<String>;
}

impl RemoteOf for Photo {
    fn remote_of_test(&self, cat: &lcc_catalog::Catalog) -> Option<String> {
        cat.remote_of(self.id).next().map(|r| r.remote_id.clone())
    }
}

// Feature: Connect and Check run off the UI thread; their results come through `remote.pump`.
#[test]
fn background_connect_and_check_report_through_the_pump_and_reconnecting_clears_a_failed_check() {
    let (url, _) = serve(Fake { assets: vec![], ..Fake::default() });
    let mut s = session();
    let r = s.execute("immich.connect", &json!({"url": url, "apiKey": "wrong", "background": true})).unwrap();
    assert_eq!(r["started"], true);
    pump_until(&mut s, |_, v| v["connecting"] == false);
    let st = s.execute("immich.status", &json!({})).unwrap();
    assert_eq!(st["connected"]["error"]["kind"], "badKey", "{st}");
    assert!(s.immich_accounts().unwrap().immich.is_empty());

    s.execute("immich.connect", &json!({"url": url, "apiKey": KEY, "background": true})).unwrap();
    pump_until(&mut s, |_, v| v["connecting"] == false);
    let st = s.execute("immich.status", &json!({"check": true, "background": true})).unwrap();
    assert_eq!(st["connected"]["ok"], true, "{st}");
    let account = st["connected"]["account"].as_str().unwrap().to_string();
    assert_eq!(st["accounts"][0]["id"], account.as_str());
    pump_until(&mut s, |_, v| v["checking"].as_array().is_some_and(Vec::is_empty));
    let st = s.execute("immich.status", &json!({})).unwrap();
    assert_eq!(st["accounts"][0]["server"]["ok"], true, "{st}");
    assert_eq!(st["accounts"][0]["server"]["version"], "3.3.1");
    assert!(!st.to_string().contains(KEY));
    assert!(!format!("{:?}", s.journal).contains(KEY));

    // Regression: a failed check ("no API key is stored") stayed on the account card after the key was connected
    // again, so the panel kept saying the key was missing. Lose the key, then check: the failure is remembered
    s.set_secret_store(Arc::new(MemorySecrets::default()));
    s.execute("immich.status", &json!({"check": true, "background": true})).unwrap();
    let st = s.execute("immich.status", &json!({})).unwrap();
    assert_eq!(st["accounts"][0]["server"]["error"]["kind"], "noKey", "{st}");
    // connect again: the old failure must not linger
    s.execute("immich.connect", &json!({"url": url, "apiKey": KEY})).unwrap();
    let st = s.execute("immich.status", &json!({})).unwrap();
    assert_eq!(st["accounts"][0]["id"], account.as_str());
    assert!(st["accounts"][0].get("server").is_none(), "{st}");
}

// Feature: without a usable keychain, keys live in an encrypted file unlocked once per session.
#[test]
fn encrypted_key_file_is_unlocked_once_and_keeps_keys() {
    let dir = scratch("keyfile");
    let (url, _) = serve(Fake { assets: vec![], ..Fake::default() });
    let fresh = || {
        let mut s = Session::new().with_fs();
        s.remote.secrets_file = Some(dir.join("credentials.enc"));
        s.remote.credentials_prefs = Some(dir.join("credentials.json"));
        s.remote.connections_path = Some(dir.join("connections.json"));
        s
    };
    let mut s = fresh();
    s.execute("credentials.useStore", &json!({"store": "file"})).unwrap();
    let st = s.execute("credentials.status", &json!({})).unwrap();
    assert_eq!(st["needsPassphrase"], true, "{st}");
    assert_eq!(st["file"]["exists"], false);
    // locked: connecting says how to unlock, never panics
    let e = s.execute("immich.connect", &json!({"url": url, "apiKey": KEY})).unwrap_err().to_string();
    assert!(e.contains("passphrase"), "{e}");
    assert!(s.execute("credentials.unlock", &json!({"passphrase": "short"})).is_err(), "too short for a new file");
    let r = s.execute("credentials.unlock", &json!({"passphrase": "correct horse battery"})).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["created"], true);
    assert_eq!(s.execute("immich.connect", &json!({"url": url, "apiKey": KEY})).unwrap()["ok"], true);
    let st = s.execute("credentials.status", &json!({})).unwrap();
    assert_eq!(st["active"], "encrypted file");
    assert_eq!(st["needsPassphrase"], false);
    let on_disk = std::fs::read_to_string(dir.join("credentials.enc")).unwrap();
    assert!(!on_disk.contains(KEY), "the key is encrypted");
    for f in ["connections.json", "credentials.json"] {
        let txt = std::fs::read_to_string(dir.join(f)).unwrap();
        assert!(!txt.contains(KEY) && !txt.contains("correct horse"), "{f} has no secrets");
    }
    assert!(!format!("{:?}", s.journal).contains("correct horse"));

    // the next session: a wrong passphrase is refused, the right one (in the background) opens it
    let mut s = fresh();
    let r = s.execute("credentials.unlock", &json!({"passphrase": "wrong horse battery"})).unwrap();
    assert_eq!(r["error"]["kind"], "wrongPassphrase");
    let account = s.immich_accounts().unwrap().immich[0].id.clone();
    assert!(s.immich_client(&account).is_err(), "still locked");
    assert_eq!(s.execute("credentials.unlock", &json!({"passphrase": "correct horse battery", "background": true})).unwrap()["started"], true);
    pump_until(&mut s, |_, v| v["unlocking"] == false);
    let (_, c) = s.immich_client(&account).unwrap();
    assert_eq!(c.me().unwrap().name, "Me");
    s.execute("credentials.lock", &json!({})).unwrap();
    assert!(s.immich_client(&account).is_err(), "locked again");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Poll `f` every 2 s until it gives `Some`, at most `secs`.
fn wait_for<T>(secs: u64, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let t = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(t.elapsed() < Duration::from_secs(secs), "timed out: {what}");
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// IMM-EXTLIB against a real server (`cargo xtask immich up && cargo xtask immich seed`; the
/// compose file mounts `target/immich/extlib` at `/mnt/extlib`): an external library over a
/// folder of generated photos links them by mapped path (Immich does not hash those files; no
/// upload), and the XMP sidecars the app
/// writes reach Immich (rating, description, keywords) after a rescan.
#[test]
#[ignore = "needs `cargo xtask immich up` and `seed` (nightly)"]
fn live_external_library_links_by_path_and_reads_sidecars() {
    let url = std::env::var("IMMICH_URL").unwrap_or_else(|_| "http://127.0.0.1:2284".into());
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/immich");
    let key = std::fs::read_to_string(root.join("api-key")).expect("run `cargo xtask immich seed`").trim().to_string();
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let run = format!("run-{stamp}");
    let dir = root.join("extlib").join(&run);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    // photos no other run has: the run's stamp is in the pixels
    let mut files = Vec::new();
    for i in 0..3u8 {
        let (w, h) = (48usize, 32usize);
        let data: Vec<[u8; 4]> = (0..w * h).map(|p| [(p % w * 5) as u8 ^ (stamp >> (i * 8)) as u8, (p / w * 7) as u8, i * 80, 255]).collect();
        let img = lcc_raster::Rgba8 { width: w, height: h, data };
        let bytes = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
        let f = dir.join(format!("shared-{i}.png"));
        std::fs::write(&f, bytes).unwrap();
        files.push(f.to_string_lossy().to_string());
    }
    let mut s = session();
    s.execute("library.import", &json!({"paths": files})).unwrap();
    pump_until(&mut s, |s, _| s.catalog.photos().all(|p| p.sha1.is_some()));
    let r = s.execute("immich.connect", &json!({"url": url, "apiKey": key})).unwrap();
    assert_eq!(r["ok"], true, "{r}");

    let c = lcc_immich::Client::new(&url, lcc_credentials::Secret::new(&key), &lcc_immich::ServerOptions::standard()).unwrap();
    let me = c.me().unwrap();
    let lib = c.create_library(&me.id, &format!("live {run}"), &[format!("/mnt/extlib/{run}")]).unwrap();
    let cleanup = |c: &lcc_immich::Client| {
        let _ = c.delete_library(&lib.id);
        let _ = std::fs::remove_dir_all(&dir);
    };
    // the helper sees the library and, once mapped, which folder it covers
    s.execute("immich.setPathMaps", &json!({"pathMaps": [{"container": "/mnt/extlib", "local": dir.parent().unwrap().to_string_lossy()}]})).unwrap();
    let libs = s.execute("immich.libraries", &json!({})).unwrap();
    let covered = libs["coverage"].as_array().unwrap().iter().any(|c| c["library"] == lib.id.as_str());
    assert!(covered, "{libs}");
    let sc = s.execute("immich.scanLibraries", &json!({})).unwrap();
    assert!(sc["scanned"].as_array().unwrap().iter().any(|x| x == lib.id.as_str()), "{sc}");

    // linked by checksum, nothing uploaded
    let ids: Vec<PhotoId> = s.catalog.photos().map(|p| p.id).collect();
    wait_for(180, "the scan finds the files and the link pass matches them", || {
        s.execute("immich.link", &json!({"full": true})).unwrap();
        pump_until(&mut s, |_, v| v["links"].as_object().is_some_and(|m| m.values().all(|l| l["active"] == false)));
        ids.iter().all(|id| lcc_immich::link::link_state(&s.catalog, *id) == "linked").then_some(())
    });
    for id in &ids {
        let r = s.catalog.remote_of(*id).next().unwrap();
        let a = c.asset(&r.remote_id).unwrap();
        assert!(a.original_path.starts_with(&format!("/mnt/extlib/{run}/")), "{}", a.original_path);
    }

    // metadata → XMP sidecars → rescan → Immich
    let first = ids[0];
    s.execute("photo.rate", &json!({"ids": [first.0], "rating": 4})).unwrap();
    s.execute("photo.setMeta", &json!({"ids": [first.0], "caption": "shared caption", "keywords": ["sharedtag"]})).unwrap();
    let w = s.execute("immich.writeSidecars", &json!({})).unwrap();
    assert_eq!(w["written"], 3, "{w}");
    assert_eq!(w["refreshed"], 3, "{w}");
    let asset_id = s.catalog.remote_of(first).next().unwrap().remote_id.clone();
    let a = wait_for(180, "Immich reads the sidecar", || {
        let a = c.asset(&asset_id).ok()?;
        let e = a.exif_info.clone()?;
        (e.rating == Some(4) && e.description.as_deref() == Some("shared caption") && a.tags.iter().any(|t| t.value == "sharedtag")).then_some(a)
    });
    assert_eq!(a.exif_info.and_then(|e| e.rating), Some(4));
    cleanup(&c);
}

// IMM-SIDECARS: the XMP Immich stores for an asset is read back into the catalog (and written
// beside the original), so a library that published with its sidecar can be rebuilt elsewhere; a newer local edit is
// left alone unless overwritten; the panel's background pull reports through `remote.pump`; and a push sends the
// fields Immich keeps in its sidecar.
#[test]
fn sidecars_pull_respects_local_edits_runs_in_the_background_and_push_sends_immichs_fields() {
    let dir = scratch("sidecars");
    // what another instance sent with its upload: metadata and `crs:` develop settings
    let xmp_full = r#"<?xpacket begin='' id='W5M0MpCehiHzreSzNTczkc9d'?>
<x:xmpmeta xmlns:x='adobe:ns:meta/'>
 <rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'>
  <rdf:Description rdf:about='' xmlns:dc='http://purl.org/dc/elements/1.1/' xmlns:xmp='http://ns.adobe.com/xap/1.0/'
   xmlns:crs='http://ns.adobe.com/camera-raw-settings/1.0/' crs:Version="15.0" crs:Exposure2012="+1.25">
   <dc:description><rdf:Alt><rdf:li xml:lang='x-default'>From Immich</rdf:li></rdf:Alt></dc:description>
   <dc:subject><rdf:Bag><rdf:li>sea</rdf:li></rdf:Bag></dc:subject>
   <xmp:Rating>4</xmp:Rating>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end='w'?>"#;
    let xmp_edit = r#"<x:xmpmeta xmlns:x='adobe:ns:meta/'><rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'>
<rdf:Description rdf:about='' xmlns:xmp='http://ns.adobe.com/xap/1.0/' xmlns:dc='http://purl.org/dc/elements/1.1/'>
<dc:description><rdf:Alt><rdf:li xml:lang='x-default'>Remote caption</rdf:li></rdf:Alt></dc:description><xmp:Rating>5</xmp:Rating></rdf:Description>
</rdf:RDF></x:xmpmeta>"#;
    let xmp_bg = r#"<x:xmpmeta xmlns:x='adobe:ns:meta/'><rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'>
<rdf:Description rdf:about='' xmlns:xmp='http://ns.adobe.com/xap/1.0/'><xmp:Rating>2</xmp:Rating></rdf:Description></rdf:RDF></x:xmpmeta>"#;
    let mut fake = Fake::default();
    let mut paths = Vec::new();
    for (name, asset_id, seed, xmp) in [
        ("shot.png", "asset-pull", 30u8, Some(xmp_full)),
        ("edit.png", "asset-edit", 32, Some(xmp_edit)),
        ("bg.png", "asset-bg", 34, Some(xmp_bg)),
        ("push.png", "asset-push", 33, None),
    ] {
        let bytes = png(seed);
        let f = dir.join(name);
        std::fs::write(&f, &bytes).unwrap();
        paths.push(f.to_string_lossy().to_string());
        fake.assets.push((asset(asset_id, name, &bytes, json!({"updatedAt": "2026-10-01T00:00:00.000Z"})), bytes));
        if let Some(xmp) = xmp {
            fake.sidecars.insert(asset_id.into(), xmp.as_bytes().to_vec());
        }
    }
    let (url, seen) = serve(fake);
    let mut s = session();
    s.execute("library.import", &json!({"paths": paths})).unwrap();
    let id_of = |s: &Session, name: &str| s.catalog.photos().find(|p| p.file_name == name).unwrap().id;
    let (id, id_edit, id_bg, id_push) = (id_of(&s, "shot.png"), id_of(&s, "edit.png"), id_of(&s, "bg.png"), id_of(&s, "push.png"));
    s.execute("immich.connect", &json!({"url": url, "apiKey": KEY})).unwrap();
    s.execute("immich.link", &json!({})).unwrap();
    pump_until(&mut s, |_, v| v["links"].as_object().is_some_and(|m| m.values().all(|l| l["active"] == false)));
    for linked in [id, id_edit, id_bg, id_push] {
        assert_eq!(lcc_immich::link::link_state(&s.catalog, linked), "linked");
    }

    // pull: what Immich stored is applied, and the local sidecar written
    let r = s.execute("immich.pullSidecars", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(r["pulled"], 1, "{r}");
    assert_eq!(r["applied"], 1, "{r}");
    assert_eq!(r["written"], 1, "{r}");
    assert_eq!(r["failed"].as_array().map(Vec::len), Some(0), "{r}");
    let p = s.catalog.photo(id).unwrap();
    assert_eq!(p.rating, 4);
    assert_eq!(p.meta.caption, "From Immich");
    assert!(p.meta.keywords.iter().any(|k| k == "sea"), "{:?}", p.meta.keywords);
    assert!((p.develop.light.exposure - 1.25).abs() < 1e-6, "the develop settings came back: {}", p.develop.light.exposure);
    // the local sidecar was written too, now carrying the app's own settings
    let local = dir.join("shot.xmp");
    let written = std::fs::read_to_string(&local).or_else(|_| std::fs::read_to_string(dir.join("shot.png.xmp"))).unwrap();
    assert!(written.contains("lc:settings"), "{written}");

    // an edit made here, later than the asset's own change, is left alone unless overwritten
    let mut edited = (**s.catalog.photo(id_edit).unwrap()).clone();
    edited.meta.caption = "Local caption".into();
    s.catalog
        .apply(Op::SetDevelop {
            id: id_edit,
            settings: edited.develop.clone(),
            label: "Local edit".into(),
            edited: Some("2999-01-01T00:00:00".into()),
        })
        .unwrap();
    s.catalog.apply(Op::SetMeta { id: id_edit, meta: Box::new(edited.meta) }).unwrap();
    let r = s.execute("immich.pullSidecars", &json!({"ids": [id_edit.0]})).unwrap();
    assert_eq!(r["applied"], 0, "{r}");
    assert_eq!(r["skipped"][0]["reason"], "editedLocally", "{r}");
    assert_eq!(s.catalog.photo(id_edit).unwrap().meta.caption, "Local caption");
    let r = s.execute("immich.pullSidecars", &json!({"ids": [id_edit.0], "overwrite": true})).unwrap();
    assert_eq!(r["applied"], 1, "{r}");
    assert_eq!(s.catalog.photo(id_edit).unwrap().meta.caption, "Remote caption");

    // Feature: the panel starts the pull on a worker; `remote.pump` takes the result in and the account card's status
    // line reads it from there
    let r = s.execute("immich.pullSidecars", &json!({"ids": [id_bg.0], "background": true})).unwrap();
    assert_eq!(r["started"], true, "{r}");
    let v = pump_until(&mut s, |_, v| v["sidecars"]["active"] == false && v["sidecars"]["last"].is_object());
    assert_eq!(v["sidecars"]["direction"], "pull", "{v}");
    assert_eq!(v["sidecars"]["last"]["applied"], 1, "{v}");
    assert_eq!(s.catalog.photo(id_bg).unwrap().rating, 2);

    // push: the server applies the metadata, and gets the tag (a managed asset keeps its sidecar)
    s.execute("photo.rate", &json!({"ids": [id_push.0], "rating": 3})).unwrap();
    s.execute("photo.setMeta", &json!({"ids": [id_push.0], "caption": "Pushed caption", "keywords": ["trips/sea"]})).unwrap();
    let r = s.execute("immich.pushSidecars", &json!({"ids": [id_push.0]})).unwrap();
    assert_eq!(r["pushed"], 1, "{r}");
    let c = lcc_immich::Client::new(&url, lcc_credentials::Secret::new(KEY), &lcc_immich::ServerOptions::standard()).unwrap();
    let a = c.asset("asset-push").unwrap();
    let e = a.exif_info.unwrap_or_default();
    assert_eq!(e.rating, Some(3));
    assert_eq!(e.description.as_deref(), Some("Pushed caption"));
    let requests = seen.lock().unwrap();
    assert!(requests.iter().any(|x| x == "PUT /api/assets/asset-push"), "{requests:?}");
    assert!(requests.iter().any(|x| x == "PUT /api/tags"), "{requests:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pulling_sidecars_needs_a_server_that_lists_them() {
    let (url, _) = serve(Fake { version: Some(json!({"major": 3, "minor": 1, "patch": 0})), ..Fake::default() });
    let mut s = session();
    s.execute("immich.connect", &json!({"url": url, "apiKey": KEY})).unwrap();
    let r = s.execute("immich.pullSidecars", &json!({})).unwrap();
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(r["error"]["kind"], "tooOld", "{r}");
}

#[path = "tests_immich_sync.rs"]
pub(crate) mod sync_tests;
