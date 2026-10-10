//! Publish services and studio capture through the commands, on a library on disk.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::Session;

fn temp_dir(tag: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("dac-pub-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn png(path: &Path, seed: u8) {
    let (w, h) = (40usize, 30usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 5) as u8, (i / w * 7) as u8, seed, 255]).collect();
    let img = dac_raster::Rgba8 { width: w, height: h, data };
    let bytes = dac_codecs::encode_png(&dac_codecs::EncodeImage::rgba8(&img), &dac_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn library(tag: &str) -> (Session, PathBuf) {
    let root = temp_dir(tag);
    let mut s = Session::new().with_fs();
    s.open_library(root.join("lib"), false).unwrap();
    (s, root)
}

fn import(s: &mut Session, root: &Path, n: u8) -> Vec<u64> {
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let paths: Vec<String> = (0..n)
        .map(|i| {
            let p = src.join(format!("p{i}.png"));
            png(&p, i * 20);
            p.to_string_lossy().to_string()
        })
        .collect();
    let r = s.execute("library.import", &json!({"paths": paths})).unwrap();
    r["imported"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect()
}

fn files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> =
        std::fs::read_dir(dir).map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect()).unwrap_or_default();
    v.sort();
    v
}

#[test]
fn hard_drive_publish_cycle() {
    let (mut s, root) = library("cycle");
    let ids = import(&mut s, &root, 3);
    let out = root.join("published");
    let svc = s
        .execute(
            "publish.createService",
            &json!({"name": "Disk", "dir": out.to_string_lossy(), "export": {"format": "jpeg", "longEdge": 20, "naming": "{name}"}}),
        )
        .unwrap();
    let sid = svc["id"].as_str().unwrap().to_string();
    let set = svc["set"].as_u64().unwrap();
    s.execute("library.select", &json!({"ids": [ids[0], ids[1]]})).unwrap();
    let st = s.execute("publish.createCollection", &json!({"service": "Disk", "name": "Web", "addSelected": true})).unwrap();
    let coll = st["collection"].as_u64().unwrap();
    assert_eq!(st["new"].as_array().unwrap().len(), 2);
    // the collection is an album inside the service's set
    assert_eq!(s.catalog.album(dac_catalog::AlbumId(coll)).unwrap().parent, Some(dac_catalog::AlbumId(set)));

    let r = s.execute("publish.run", &json!({"collection": coll})).unwrap();
    assert_eq!(r["runs"][0]["published"].as_array().unwrap().len(), 2, "{r}");
    assert_eq!(files(&out.join("Web")), vec!["p0.jpg", "p1.jpg"]);
    let st = s.execute("publish.status", &json!({"collection": coll})).unwrap();
    assert_eq!((st["published"].as_array().unwrap().len(), st["pending"].as_u64()), (2, Some(0)));

    // an edit makes a photo modified; Mark as Up-to-Date clears it
    s.execute("library.select", &json!({"ids": [ids[0]]})).unwrap();
    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
    let st = s.execute("publish.status", &json!({"collection": coll})).unwrap();
    assert_eq!(st["modified"], json!([ids[0]]));
    let st = s.execute("publish.markUpToDate", &json!({"collection": coll})).unwrap();
    assert_eq!((st["marked"].as_u64(), st["modified"].as_array().unwrap().len()), (Some(1), 0));
    s.execute("photo.rate", &json!({"rating": 2})).unwrap();

    // add one, take one out: republish writes the new and modified, deletes the removed
    s.execute("album.addPhotos", &json!({"id": coll, "ids": [ids[2]]})).unwrap();
    s.execute("album.removePhotos", &json!({"id": coll, "ids": [ids[1]]})).unwrap();
    let st = s.execute("publish.status", &json!({"collection": coll})).unwrap();
    assert_eq!((st["new"].clone(), st["modified"].clone(), st["toRemove"].clone()), (json!([ids[2]]), json!([ids[0]]), json!([ids[1]])));
    let r = s.execute("publish.run", &json!({"service": sid})).unwrap();
    assert_eq!(r["runs"][0]["removed"], json!([ids[1]]));
    assert_eq!(files(&out.join("Web")), vec!["p0.jpg", "p2.jpg"]);
    assert_eq!(s.execute("publish.status", &json!({"collection": coll})).unwrap()["pending"], 0);
    // remote ids are in the catalog's remote links
    assert_eq!(s.catalog.remote_links().iter().filter(|r| r.service == dac_publish::SERVICE).count(), 2);

    // the configuration is in the library folder and survives a reopen
    let list = s.execute("publish.services", &json!({})).unwrap();
    assert_eq!(list["services"][0]["collections"][0]["published"], 2);
    s.close_library().unwrap();
    drop(s);
    let mut s2 = Session::new().with_fs();
    s2.open_library(root.join("lib"), false).unwrap();
    assert_eq!(s2.execute("publish.status", &json!({"collection": coll})).unwrap()["pending"], 0);

    // deleting the collection forgets the links, keeps the files
    s2.execute("publish.deleteCollection", &json!({"collection": coll})).unwrap();
    assert_eq!(s2.catalog.remote_links().iter().filter(|r| r.service == dac_publish::SERVICE).count(), 0);
    assert!(s2.catalog.album(dac_catalog::AlbumId(coll)).is_none());
    assert_eq!(files(&out.join("Web")).len(), 2);
    s2.execute("publish.deleteService", &json!({"service": sid})).unwrap();
    assert!(s2.catalog.album(dac_catalog::AlbumId(set)).is_none());
}

#[test]
fn bad_publish_params_are_errors() {
    let (mut s, root) = library("bad");
    for p in [
        json!({"dir": "relative/path"}),
        json!({}),
        json!({"kind": "flickr", "dir": "/tmp"}),
        json!({"dir": root.to_string_lossy(), "export": {"path": "/etc/x"}}),
        json!({"dir": root.to_string_lossy(), "export": {"format": "nope"}}),
    ] {
        assert!(s.execute("publish.createService", &p).is_err(), "{p}");
    }
    assert!(s.execute("publish.run", &json!({"collection": 12345})).is_err());
    assert!(s.execute("publish.run", &json!({})).is_err());
    // a damaged publish.json is reported, not replaced
    std::fs::write(root.join("lib").join(dac_publish::FILE), b"{").unwrap();
    let e = s.execute("publish.services", &json!({})).unwrap_err().to_string();
    assert!(e.contains("damaged"), "{e}");
    // an in-memory library has no publish services
    let mut m = Session::new();
    assert!(m.execute("publish.services", &json!({})).is_err());
}

#[test]
fn studio_capture_imports_new_shots() {
    let (mut s, root) = library("studio");
    let watch = root.join("camera");
    std::fs::create_dir_all(&watch).unwrap();
    png(&watch.join("before.png"), 1);
    let r = s
        .execute(
            "tether.start",
            &json!({"folder": watch.to_string_lossy(), "session": "Shoot", "copy": true, "naming": "{session}-{seq:3}", "keywords": "studio, test"}),
        )
        .unwrap();
    assert_eq!((r["name"].as_str(), r["collection"].as_str(), r["active"].as_bool()), (Some("Shoot"), Some("Shoot"), Some(true)));
    // a shot arrives: imported on the scan after its size held still
    png(&watch.join("IMG_1.png"), 2);
    let r = s.execute("tether.scan", &json!({})).unwrap();
    assert_eq!(r["imported"], json!([]));
    let r = s.execute("tether.scan", &json!({})).unwrap();
    let ids = r["imported"].as_array().unwrap().clone();
    assert_eq!(ids.len(), 1, "{r}");
    let id = dac_catalog::PhotoId(ids[0].as_u64().unwrap());
    // the file that was there before the session is not imported
    assert_eq!(s.catalog.len(), 1);
    // newest shot selected, renamed, copied into Originals/<session>, keyworded, in the collection
    assert_eq!(s.selection.active, Some(id));
    let p = s.catalog.photo(id).unwrap();
    assert_eq!(p.file_name, "Shoot-001.png");
    match &p.source {
        dac_catalog::Source::File { path } => assert!(path.contains("Originals") && path.contains("Shoot"), "{path}"),
        other => panic!("{other:?}"),
    }
    assert!(p.meta.keywords.iter().any(|k| k == "studio"));
    assert!(s.catalog.albums().any(|a| a.name == "Shoot" && a.photos.contains(&id)));
    // the next shot continues the numbering
    png(&watch.join("IMG_2.png"), 3);
    s.execute("tether.scan", &json!({})).unwrap();
    let r = s.execute("tether.scan", &json!({})).unwrap();
    let id2 = dac_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
    assert_eq!(s.catalog.photo(id2).unwrap().file_name, "Shoot-002.png");
    assert_eq!(s.execute("tether.status", &json!({})).unwrap()["shots"], 2);
    // stop: scans do nothing
    s.execute("tether.stop", &json!({})).unwrap();
    png(&watch.join("IMG_3.png"), 4);
    for _ in 0..2 {
        assert_eq!(s.execute("tether.scan", &json!({})).unwrap()["active"], false);
    }
    // the folder going away is reported, not a failure
    s.execute("tether.start", &json!({})).unwrap();
    std::fs::remove_dir_all(&watch).unwrap();
    assert!(s.execute("tether.scan", &json!({})).unwrap()["error"].is_string());
    assert!(s.execute("tether.start", &json!({"folder": "/no/such/folder"})).is_err());
    assert!(s.execute("tether.settings", &json!({"preset": "no-such-preset"})).is_err());
    s.execute("tether.stop", &json!({"end": true})).unwrap();
    assert_eq!(s.execute("tether.status", &json!({})).unwrap(), Value::Null);
}
