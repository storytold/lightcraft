//! SHA-1 at import and its back-fill, and the Immich commands against an in-process server that
//! speaks the endpoints the client uses (bodies shaped like a v3.3.1 server's).
#![cfg(not(target_arch = "wasm32"))]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dac_catalog::{Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

use crate::Session;
use crate::remote::{LINK_ONLY, MemorySecrets};

const KEY: &str = "engine-test-key-42";

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("dac-immich-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn png(seed: u8) -> Vec<u8> {
    let (w, h) = (40usize, 30usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 5) as u8, (i / w * 7) as u8, seed, 255]).collect();
    let img = dac_raster::Rgba8 { width: w, height: h, data };
    dac_codecs::encode_png(&dac_codecs::EncodeImage::rgba8(&img), &dac_codecs::EncodeMeta::default()).unwrap()
}

fn b64(bytes: &[u8]) -> String {
    dac_hash::sha1_bytes(bytes).to_base64()
}

struct Fake {
    assets: Vec<(Value, Vec<u8>)>,
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
        for s in listener.incoming() {
            let Ok(mut s) = s else { return };
            let Some((m, t, key, body)) = read_req(&mut s) else { continue };
            seen2.lock().unwrap().push(format!("{m} {t}"));
            let path = t.split('?').next().unwrap_or_default().to_string();
            let json = |v: Value| resp("200 OK", "application/json", v.to_string().as_bytes());
            let out = if path == "/api/server/version" {
                json(json!({"major": 3, "minor": 3, "patch": 1}))
            } else if key.as_deref() != Some(KEY) {
                resp("401 Unauthorized", "application/json", br#"{"message":"Invalid API key"}"#)
            } else if path == "/api/users/me" {
                json(json!({"id": "user-1", "email": "me@example.invalid", "name": "Me", "isAdmin": false}))
            } else if path == "/api/api-keys/me" {
                json(json!({"id": "k", "name": "app", "permissions": ["asset.read", "asset.view", "asset.download"]}))
            } else if path == "/api/search/metadata" {
                let q: Value = serde_json::from_slice(&body).unwrap_or_default();
                let items: Vec<Value> =
                    if q["page"].as_u64().unwrap_or(1) == 1 { fake.assets.iter().map(|(a, _)| a.clone()).collect() } else { vec![] };
                json(json!({"assets": {"total": items.len(), "count": items.len(), "items": items, "nextPage": null}}))
            } else if let Some(rest) = path.strip_prefix("/api/assets/") {
                let (id, tail) = rest.split_once('/').unwrap_or((rest, ""));
                match fake.assets.iter().find(|(a, _)| a["id"] == id) {
                    Some((a, bytes)) => match tail {
                        "" => json(a.clone()),
                        "original" => resp("200 OK", "image/png", bytes),
                        "thumbnail" => resp("200 OK", "image/png", bytes),
                        _ => resp("404 Not Found", "application/json", b"{}"),
                    },
                    None => resp("404 Not Found", "application/json", b"{}"),
                }
            } else {
                resp("404 Not Found", "application/json", b"{}")
            };
            let _ = s.write_all(&out);
        }
    });
    (format!("http://127.0.0.1:{port}"), seen)
}

fn asset(id: &str, name: &str, bytes: &[u8], extra: Value) -> Value {
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
    let dir = temp_dir("sha1");
    let a = dir.join("a.png");
    std::fs::write(&a, png(1)).unwrap();
    let mut s = session();
    s.execute("library.import", &json!({"paths": [a.to_string_lossy()]})).unwrap();
    let p = s.catalog.photos().next().unwrap();
    assert_eq!(p.sha1.as_deref(), Some(dac_hash::sha1_bytes(&png(1)).to_hex().as_str()));

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
    assert_eq!(s.catalog.photo(id).unwrap().sha1.as_deref(), Some(dac_hash::sha1_bytes(&png(2)).to_hex().as_str()));
    assert_eq!(v["sha1"]["failed"], 1, "the missing file is not retried: {v}");
    assert!(s.catalog.photo(gone).unwrap().sha1.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn connect_link_and_open_in_immich() {
    let dir = temp_dir("link");
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
    assert_eq!(dac_immich::link::link_state(&s.catalog, id_one), "linked");
    assert_eq!(dac_immich::link::link_state(&s.catalog, id_two), "probable");
    let l = s.execute("immich.links", &json!({"id": id_one.0})).unwrap();
    assert_eq!(l["links"][0]["url"], format!("{url}/photos/aaaa-1"));
    // the Library filter
    let probable_ids: Vec<PhotoId> =
        s.catalog.photos().filter(|p| dac_catalog::query::immich_state_is(&s.catalog, p.id, "probable")).map(|p| p.id).collect();
    assert_eq!(probable_ids, vec![id_two]);
    // confirm the probable one
    assert_eq!(s.execute("immich.confirmLink", &json!({"ids": [id_two.0]})).unwrap()["confirmed"], 1);
    assert_eq!(dac_immich::link::link_state(&s.catalog, id_two), "linked");
    // the linked filter also works through the session's filter
    s.filter.immich = Some("linked".into());
    assert_eq!(s.visible().len(), 2);
    s.filter.immich = None;

    assert_eq!(s.execute("immich.disconnect", &json!({"forgetLinks": true})).unwrap()["forgotLinks"], 2);
    assert!(s.immich_accounts().unwrap().immich.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_copies_maps_metadata_and_skips_duplicates() {
    let dir = temp_dir("import");
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
    assert_eq!(beach.flag, dac_catalog::Flag::Pick);
    assert_eq!(beach.meta.caption, "Beach");
    assert_eq!(beach.meta.keywords, vec!["trips|sea".to_string()]);
    assert_eq!(beach.sha1.as_deref(), Some(dac_hash::sha1_bytes(&a).to_hex().as_str()));
    // two assets with the same name land side by side in the dated folder
    let names: Vec<String> = s.catalog.photos().map(|p| p.file_name.clone()).collect();
    assert!(names.contains(&"beach.png".to_string()) && names.contains(&"beach-1.png".to_string()), "{names:?}");
    assert!(s.catalog.albums().any(|al| al.name == "From Immich"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn link_only_downloads_the_original_on_demand() {
    let dir = temp_dir("linkonly");
    let a = png(30);
    let (url, _) = serve(Fake { assets: vec![(asset("l-1", "far.png", &a, json!({})), a.clone())] });
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
    s.catalog.apply(Op::SetKind { id: p.id, kind: dac_catalog::MediaKind::Raw, format: "JPEG".into() }).unwrap();
    assert_eq!(s.execute("immich.fetchOriginal", &json!({"id": p.id.0})).unwrap()["started"], true);
    pump_until(&mut s, |s, _| s.remote.fetching.is_empty());
    let q = s.catalog.photo(p.id).unwrap();
    assert!(q.preview_only.is_none(), "{:?}", s.remote.fetch_errors);
    assert_eq!(q.file_name, "far.png");
    assert_eq!((q.kind, q.format.as_str()), (dac_catalog::MediaKind::Image, "PNG"), "kind and format follow the original");
    assert_eq!(q.sha1.as_deref(), Some(dac_hash::sha1_bytes(&a).to_hex().as_str()));
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
    let dir = temp_dir("extlib");
    let inside = dir.join("Photos/x.png");
    let outside = dir.join("Other/y.png");
    for (p, n) in [(&inside, 40), (&outside, 41)] {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, png(n)).unwrap();
    }
    let (url, _) = serve(Fake { assets: vec![] });
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
    let _ = std::fs::remove_dir_all(&dir);
}

trait RemoteOf {
    fn remote_of_test(&self, cat: &dac_catalog::Catalog) -> Option<String>;
}

impl RemoteOf for Photo {
    fn remote_of_test(&self, cat: &dac_catalog::Catalog) -> Option<String> {
        cat.remote_of(self.id).next().map(|r| r.remote_id.clone())
    }
}

// Feature: Connect and Check run off the UI thread; their results come through `remote.pump`.
#[test]
fn background_connect_and_check_report_through_the_pump() {
    let (url, _) = serve(Fake { assets: vec![] });
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
}

// Feature: without a usable keychain, keys live in an encrypted file unlocked once per session.
#[test]
fn encrypted_key_file_is_unlocked_once_and_keeps_keys() {
    let dir = temp_dir("keyfile");
    let (url, _) = serve(Fake { assets: vec![] });
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
        let img = dac_raster::Rgba8 { width: w, height: h, data };
        let bytes = dac_codecs::encode_png(&dac_codecs::EncodeImage::rgba8(&img), &dac_codecs::EncodeMeta::default()).unwrap();
        let f = dir.join(format!("shared-{i}.png"));
        std::fs::write(&f, bytes).unwrap();
        files.push(f.to_string_lossy().to_string());
    }
    let mut s = session();
    s.execute("library.import", &json!({"paths": files})).unwrap();
    pump_until(&mut s, |s, _| s.catalog.photos().all(|p| p.sha1.is_some()));
    let r = s.execute("immich.connect", &json!({"url": url, "apiKey": key})).unwrap();
    assert_eq!(r["ok"], true, "{r}");

    let c = dac_immich::Client::new(&url, dac_credentials::Secret::new(&key), &dac_immich::ServerOptions::standard()).unwrap();
    let me = c.me().unwrap();
    let lib = c.create_library(&me.id, &format!("live {run}"), &[format!("/mnt/extlib/{run}")]).unwrap();
    let cleanup = |c: &dac_immich::Client| {
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
        ids.iter().all(|id| dac_immich::link::link_state(&s.catalog, *id) == "linked").then_some(())
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
