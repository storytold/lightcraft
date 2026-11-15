use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::Session;

fn session(dir: &std::path::Path, name: &str, port: u16) -> Session {
    let mut s = Session::new().with_fs();
    s.device_sync.store_path = Some(dir.join(format!("{name}.json")));
    s.device_sync.port = Some(port);
    s.device_sync.exchange_every = Some(Duration::from_millis(200));
    s.open_library(dir.join(format!("{name}-lib")), false).unwrap();
    s
}

fn write_png(path: &std::path::Path, seed: usize) {
    let (w, h) = (16usize, 12usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 9 + seed) as u8, (i / w * 13) as u8, 7, 255]).collect();
    let img = lcc_raster::Rgba8 { width: w, height: h, data };
    let bytes = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn status(s: &mut Session) -> Value {
    s.execute("sync.status", &json!({})).unwrap()
}

fn device_id(s: &mut Session) -> String {
    status(s)["deviceId"].as_str().unwrap().to_string()
}

/// Ticks every session (as their apps would) until `done`.
fn tick_until(all: &mut [&mut Session], what: &str, done: impl Fn(&mut [&mut Session]) -> bool) {
    let t = Instant::now();
    while !done(all) {
        for s in all.iter_mut() {
            s.execute("sync.tick", &json!({})).unwrap();
        }
        assert!(t.elapsed() < Duration::from_secs(60), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn pair_up(a: &mut Session, b: &mut Session) {
    a.execute("sync.rename", &json!({"name": "Studio"})).unwrap();
    b.execute("sync.rename", &json!({"name": "Tablet"})).unwrap();
    a.execute("sync.find", &json!({"on": true})).unwrap();
    b.execute("sync.find", &json!({"on": true})).unwrap();
    let b_id = device_id(b);
    let wait = |s: &mut Session, what: &str, f: &dyn Fn(&Value) -> bool| {
        let t = Instant::now();
        loop {
            let v = status(s);
            if f(&v) {
                return;
            }
            assert!(t.elapsed() < Duration::from_secs(20), "timed out waiting for {what}: {v}");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    wait(a, "a to find b", &|v| v["found"].as_array().is_some_and(|f| f.iter().any(|d| d["deviceId"] == b_id.as_str())));
    a.execute("sync.pair", &json!({"device": b_id})).unwrap();
    wait(a, "a's code", &|v| v["pairing"]["stage"] == "confirm");
    wait(b, "b's code", &|v| v["pairing"]["stage"] == "confirm");
    a.execute("sync.confirm", &json!({"accept": true})).unwrap();
    b.execute("sync.confirm", &json!({"accept": true})).unwrap();
    wait(a, "a paired", &|v| v["paired"].as_array().is_some_and(|p| p.len() == 1));
    wait(b, "b paired", &|v| v["paired"].as_array().is_some_and(|p| p.len() == 1));
    a.execute("sync.clearPairing", &json!({})).unwrap();
    b.execute("sync.clearPairing", &json!({})).unwrap();
}

fn lan_skipped() -> bool {
    // LAN discovery times out on the macOS CI runner; checked in person on real Macs instead
    if cfg!(target_os = "macos") && std::env::var_os("CI").is_some() {
        lcc_testkit::skip!(System, "LAN discovery on the macOS CI runner (tested in person)");
        return true;
    }
    false
}

fn collection<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    v["catalogue"]["collections"].as_array()?.iter().find(|c| c["name"] == name)
}

/// Collections only, two catalogues: the owner allows a collection, the other device takes it in
/// full; the photo, its original and its edits arrive; an edit made there comes back; a photo
/// added there reaches the owner with its file; what the owner doesn't allow stays home.
#[test]
fn a_shared_collection_brings_photos_files_and_edits_both_ways() {
    if lan_skipped() {
        return;
    }
    let dir = lcc_testkit::TempDir::new("lcc-engine-device-sync");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let (p1, p2, p3) = (dir.join("src/one.png"), dir.join("src/two.png"), dir.join("src/three.png"));
    write_png(&p1, 1);
    write_png(&p2, 2);
    write_png(&p3, 3);
    let port = 23000 + (std::process::id() % 8000) as u16;
    let (mut a, mut b) = (session(&dir, "a", port), session(&dir, "b", port));
    let ids = a.execute("library.import", &json!({"paths": [p1.to_string_lossy(), p2.to_string_lossy()]})).unwrap();
    let (one, two) = (ids["imported"][0].as_u64().unwrap(), ids["imported"][1].as_u64().unwrap());
    let trip = a.execute("album.create", &json!({"name": "Trip"})).unwrap()["id"].as_u64().unwrap();
    let private = a.execute("album.create", &json!({"name": "Private"})).unwrap()["id"].as_u64().unwrap();
    // (album.addPhotos wants a selection)
    a.selection = crate::Selection::single(lcc_catalog::PhotoId(one));
    a.execute("album.addPhotos", &json!({"id": trip, "ids": [one]})).unwrap();
    a.execute("album.addPhotos", &json!({"id": private, "ids": [two]})).unwrap();
    assert_eq!(a.catalog.album(lcc_catalog::AlbumId(trip)).unwrap().photos.len(), 1);
    a.execute("photo.rate", &json!({"ids": [one], "rating": 4})).unwrap();
    pair_up(&mut a, &mut b);
    let (a_id, b_id) = (device_id(&mut a), device_id(&mut b));

    // A starts a collections-only sync and allows Trip in full; Private stays its own
    a.execute("sync.setup", &json!({"device": b_id, "mode": "collections"})).unwrap();
    a.execute("sync.allow", &json!({"album": trip, "allow": "sync"})).unwrap();
    assert!(a.execute("sync.take", &json!({"album": trip, "take": "previews"})).is_err(), "the owner allows, it doesn't take");
    tick_until(&mut [&mut a, &mut b], "the invitation", |all| status(all[1])["offers"].as_array().is_some_and(|o| !o.is_empty()));
    b.execute("sync.answer", &json!({"device": a_id, "accept": true})).unwrap();
    tick_until(&mut [&mut a, &mut b], "b to see Trip", |all| collection(&status(all[1]), "Trip").is_some());
    let v = status(&mut b);
    assert!(collection(&v, "Private").is_none(), "a collection that isn't allowed isn't listed: {v}");
    assert_eq!(b.catalog.photos().count(), 0, "nothing comes before it is taken");
    assert!(b.execute("sync.take", &json!({"collection": collection(&v, "Trip").unwrap()["id"], "take": "fullSync"})).is_ok());

    // the photo arrives with its rating, then its original, checked against its key
    tick_until(&mut [&mut a, &mut b], "the photo at b", |all| all[1].catalog.photos().count() == 1);
    let pb = b.catalog.photos().next().unwrap().clone();
    assert_eq!(pb.rating, 4);
    let lcc_catalog::Source::File { path } = &pb.source else { panic!("a file") };
    let path = std::path::PathBuf::from(path);
    tick_until(&mut [&mut a, &mut b], "the original at b", |_| path.is_file());
    assert_eq!(std::fs::read(&path).unwrap(), std::fs::read(&p1).unwrap());
    assert!(b.catalog.albums().any(|al| al.name == "Trip" && al.photos == vec![pb.id]));
    tick_until(&mut [&mut a, &mut b], "b's pill to settle", |all| all[1].sync_pill(pb.id).is_some_and(|p| p.text() == "EF"));
    assert_eq!(a.sync_pill(lcc_catalog::PhotoId(one)).map(|p| p.letters()), Some("EF".into()));
    assert_eq!(a.sync_pill(lcc_catalog::PhotoId(two)), None, "a photo in no shared collection has no pill");

    // an edit on B comes back to A, as a change of that field only
    let mut d = (*pb.develop).clone();
    d.light.exposure = 0.5;
    b.set_develop(pb.id, d, "test").unwrap();
    a.execute("photo.rate", &json!({"ids": [one], "rating": 2})).unwrap();
    tick_until(&mut [&mut a, &mut b], "edits both ways", |all| {
        let (pa, pb) = (all[0].catalog.photo(lcc_catalog::PhotoId(one)).unwrap(), all[1].catalog.photos().next().unwrap());
        pa.develop.light.exposure == 0.5 && pb.rating == 2 && pa.rating == 2 && pb.develop.light.exposure == 0.5
    });
    assert!(a.undo.is_empty() || a.undo.iter().all(|u| !u.label.contains("Sync")), "another device's edit is not an undo step");

    // a photo B adds to the collection reaches A, file and all
    let added = b.execute("library.import", &json!({"paths": [p3.to_string_lossy()]})).unwrap()["imported"][0].as_u64().unwrap();
    let trip_b = b.catalog.albums().find(|al| al.name == "Trip").unwrap().id.0;
    b.selection = crate::Selection::single(lcc_catalog::PhotoId(added));
    b.execute("album.addPhotos", &json!({"id": trip_b, "ids": [added]})).unwrap();
    tick_until(&mut [&mut a, &mut b], "b's photo at a", |all| all[0].catalog.photos().count() == 3);
    let pa = a.catalog.photos().find(|p| p.file_name == "three.png").unwrap().clone();
    let lcc_catalog::Source::File { path } = &pa.source else { panic!("a file") };
    let path = std::path::PathBuf::from(path);
    tick_until(&mut [&mut a, &mut b], "b's original at a", |_| path.is_file());
    assert!(a.catalog.album(lcc_catalog::AlbumId(trip)).unwrap().photos.contains(&pa.id));

    // the owner stops allowing it: B keeps what it has, and later edits stay on A
    a.execute("sync.allow", &json!({"album": trip, "allow": "none"})).unwrap();
    tick_until(&mut [&mut a, &mut b], "b to lose the collection", |all| !all[1].catalog.albums().any(|al| al.name == "Trip"));
    assert_eq!(b.catalog.photos().count(), 2, "stopping a sync never deletes photos");
    a.execute("photo.rate", &json!({"ids": [one], "rating": 5})).unwrap();
    for _ in 0..20 {
        a.execute("sync.tick", &json!({})).unwrap();
        b.execute("sync.tick", &json!({})).unwrap();
        std::thread::sleep(Duration::from_millis(30));
    }
    assert_eq!(b.catalog.photo(pb.id).unwrap().rating, 2);
    let _ = private;
    // bad parameters are errors, not panics
    for (cmd, p) in [
        ("sync.setup", json!({"device": "nobody", "mode": "collections"})),
        ("sync.setup", json!({"device": b_id, "mode": "everything"})),
        ("sync.allow", json!({"album": 999_999, "allow": "sync"})),
        ("sync.allow", json!({"album": trip, "allow": "all"})),
        ("sync.take", json!({"collection": "nope", "take": "previews"})),
        ("sync.answer", json!({"device": "nobody", "accept": true})),
        ("sync.rename", json!({"name": "Tablet"})),
    ] {
        assert!(a.execute(cmd, &p).is_err(), "{cmd} {p}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Cancelling with no pairing under way (or no service at all) is harmless and answers the status.
#[test]
fn cancel_pairing_without_a_pairing_is_harmless() {
    let dir = lcc_testkit::TempDir::new("lcc-engine-sync-cancel");
    let mut a = session(&dir, "a", 31000 + (std::process::id() % 2000) as u16);
    let v = a.execute("sync.cancelPairing", &json!({})).unwrap();
    assert!(v["pairing"].is_null());
    a.execute("sync.find", &json!({"on": true})).unwrap();
    let v = a.execute("sync.cancelPairing", &json!({})).unwrap();
    assert!(v["pairing"].is_null());
}

// ── in-process exchanges (no network): the catalogue side of sync ─────────────────────────────

/// A session with a sync identity and its catalogue's replica loaded, without starting the service.
fn offline(dir: &std::path::Path, name: &str) -> (Session, lcc_peer::Peer) {
    let mut s = Session::new().with_fs();
    s.set_secret_store(std::sync::Arc::new(crate::remote::MemorySecrets::default()));
    let store = dir.join(format!("{name}.json"));
    let st = lcc_peer::Store::load_or_create(&store, name).unwrap();
    st.save(&store).unwrap();
    s.device_sync.store_path = Some(store);
    s.open_library(dir.join(format!("{name}-lib")), false).unwrap();
    super::ensure_lib(&mut s);
    let peer = lcc_peer::Peer { device_id: st.identity.device_id, name: name.into(), public_key: String::new(), paired_at: 0 };
    (s, peer)
}

/// Everything `from` has for `to`, through JSON as on the wire.
fn hand_over(from: &mut Session, fp: &lcc_peer::Peer, to: &mut Session, tp: &lcc_peer::Peer) {
    for _ in 0..50 {
        let mut lf = from.device_sync.lib.take().unwrap();
        super::catalog::scan(from, &mut lf);
        let (batch, out) = super::catalog::prepare(from, &lf, &tp.device_id);
        from.device_sync.lib = Some(lf);
        let batch: lcc_sync::proto::Batch = serde_json::from_slice(&serde_json::to_vec(&batch).unwrap()).unwrap();
        let mut lt = to.device_sync.lib.take().unwrap();
        super::catalog::scan(to, &mut lt);
        super::catalog::apply(to, &mut lt, fp, &batch).unwrap();
        to.device_sync.lib = Some(lt);
        from.device_sync.lib.as_mut().unwrap().replica.confirmed(&tp.device_id, &out);
        if !out.more {
            return;
        }
    }
}

fn swap(a: &mut Session, ap: &lcc_peer::Peer, b: &mut Session, bp: &lcc_peer::Peer) {
    for _ in 0..3 {
        hand_over(a, ap, b, bp);
        hand_over(b, bp, a, ap);
    }
}

/// `a` starts a whole-catalogue group that `b` joins.
fn offline_group(a: &mut Session, ap: &lcc_peer::Peer, b: &mut Session, bp: &lcc_peer::Peer) {
    let la = a.device_sync.lib.as_mut().unwrap();
    la.replica.start_group("g", lcc_sync::Mode::Catalogue, Some(ap.device_id.clone()), &ap.name, super::now_ms()).unwrap();
    la.replica.invited.insert(bp.device_id.clone());
    let g = la.replica.group.clone().unwrap();
    let lb = b.device_sync.lib.as_mut().unwrap();
    lb.replica.join_group(&g, &bp.name, super::now_ms()).unwrap();
    for s in [&mut *a, &mut *b] {
        let mut l = s.device_sync.lib.take().unwrap();
        super::catalog::register_all(s, &mut l);
        s.device_sync.lib = Some(l);
    }
    swap(a, ap, b, bp);
}

fn key_of(s: &Session, id: u64) -> Option<String> {
    super::record::key_of(s.catalog.photo(lcc_catalog::PhotoId(id))?, &s.catalog)
}

/// Runs the SHA-1 back-fill until nothing is left to hash.
fn hash_all(s: &mut Session) {
    let t = Instant::now();
    loop {
        s.execute("remote.pump", &json!({"sync": false})).unwrap();
        if !s.remote.sha1_busy && super::super::remote::sha1_pending(s, 1).is_empty() {
            return;
        }
        assert!(t.elapsed() < Duration::from_secs(20), "hashing timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The same original on two devices, one of which hasn't hashed it (or synced before photos were
/// keyed by SHA-1): it ends as one photo with one key on both, the edits of both kept, and a
/// replica that still knew it by its content hash moves to the new key.
#[test]
fn one_original_has_one_key_whichever_device_hashed_it() {
    let dir = lcc_testkit::TempDir::new("lcc-engine-sync-identity");
    let src = dir.join("one.png");
    write_png(&src, 5);
    let (mut a, ap) = offline(&dir, "a");
    let (mut b, bp) = offline(&dir, "b");
    let ia = a.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap()["imported"][0].as_u64().unwrap();
    let ib = b.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap()["imported"][0].as_u64().unwrap();
    let sha1 = a.catalog.photo(lcc_catalog::PhotoId(ia)).unwrap().sha1.clone().expect("import hashes the original");
    // B hasn't hashed its copy
    b.apply_system(lcc_catalog::Op::SetSha1 { id: lcc_catalog::PhotoId(ib), sha1: None }).unwrap();
    let trip = a.execute("album.create", &json!({"name": "Trip"})).unwrap()["id"].as_u64().unwrap();
    a.selection = crate::Selection::single(lcc_catalog::PhotoId(ia));
    a.execute("album.addPhotos", &json!({"id": trip, "ids": [ia]})).unwrap();
    for s in [&mut a, &mut b] {
        let mut l = s.device_sync.lib.take().unwrap();
        super::catalog::index(s, &mut l);
        s.device_sync.lib = Some(l);
    }
    assert_eq!(key_of(&a, ia), Some(format!("sha1:{sha1}")));
    let old_key = key_of(&b, ib).unwrap();
    assert!(!old_key.starts_with("sha1:"), "an unhashed photo is known by its content hash");
    offline_group(&mut a, &ap, &mut b, &bp);
    a.execute("sync.allow", &json!({"album": trip, "allow": "sync"})).unwrap();
    let cid = a.device_sync.lib.as_ref().unwrap().replica.collection_of_album(trip).unwrap().to_string();
    swap(&mut a, &ap, &mut b, &bp);
    b.execute("sync.take", &json!({"collection": cid, "take": "fullSync"})).unwrap();
    // B edits under the old key before it learns the SHA-1
    b.execute("photo.rate", &json!({"ids": [ib], "rating": 3})).unwrap();
    a.execute("photo.rate", &json!({"ids": [ia], "rating": 1})).unwrap();
    let mut d = (*a.catalog.photo(lcc_catalog::PhotoId(ia)).unwrap().develop).clone();
    d.light.exposure = 0.7;
    a.set_develop(lcc_catalog::PhotoId(ia), d, "test").unwrap();
    swap(&mut a, &ap, &mut b, &bp);
    for s in [&a, &b] {
        assert_eq!(s.catalog.photos().count(), 1, "one photo, not a second one for the other key");
    }
    assert_eq!(b.catalog.photo(lcc_catalog::PhotoId(ib)).unwrap().sha1, None, "another device's SHA-1 is only a hint");
    // B hashes the photo itself (first in line), and then moves to the key
    hash_all(&mut b);
    swap(&mut a, &ap, &mut b, &bp);
    let pb = b.catalog.photo(lcc_catalog::PhotoId(ib)).unwrap();
    assert_eq!(pb.sha1.as_deref(), Some(sha1.as_str()), "B's own hash of the same bytes");
    assert_eq!(key_of(&b, ib), key_of(&a, ia));
    assert_eq!(pb.develop.light.exposure, 0.7);
    let (ra, rb) = (a.catalog.photo(lcc_catalog::PhotoId(ia)).unwrap().rating, pb.rating);
    assert_eq!(ra, rb, "both end with the later rating");
    assert!(b.catalog.albums().any(|al| al.name == "Trip" && al.photos == vec![lcc_catalog::PhotoId(ib)]));
    let rb = &b.device_sync.lib.as_ref().unwrap().replica;
    assert!(!rb.photos.contains_key(&old_key), "nothing is left under the old key");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A link-only Immich photo (made here as `immich.import` with `mode: link` makes it) of `src`'s
/// bytes, on account `account`, the checksum given as Immich gives it (base64).
fn link_only(s: &mut Session, src: &std::path::Path, account: &str) -> lcc_catalog::PhotoId {
    let preview = src.with_file_name(format!("preview-{}.png", s.catalog.photos().count()));
    write_png(&preview, 99);
    let id = s.execute("library.import", &json!({"paths": [preview.to_string_lossy()]})).unwrap()["imported"][0].as_u64().unwrap();
    let id = lcc_catalog::PhotoId(id);
    let ph = s.catalog.photo(id).unwrap().clone();
    let sha1 = lcc_hash::sha1_file(src).unwrap();
    let ops = vec![
        lcc_catalog::Op::SetSha1 { id, sha1: None },
        lcc_catalog::Op::SetContent {
            id,
            width: ph.width,
            height: ph.height,
            file_size: ph.file_size,
            content_hash: ph.content_hash.clone(),
            preview_only: Some(crate::remote::LINK_ONLY.into()),
            preview_checked: lcc_catalog::APP_VERSION.to_string(),
        },
        lcc_catalog::Catalog::link_remote_op(lcc_catalog::RemoteIdentity {
            photo_id: id,
            service: "immich".into(),
            account_id: account.into(),
            remote_id: "asset-1".into(),
            remote_checksum: Some(sha1.to_base64()),
            remote_updated_at: None,
            last_synced_at: None,
            sync_state: lcc_catalog::SyncState::Synced,
        }),
    ];
    s.apply_system(lcc_catalog::Op::Batch { ops }).unwrap();
    let mut l = s.device_sync.lib.take().unwrap();
    super::catalog::index(s, &mut l);
    s.device_sync.lib = Some(l);
    id
}

/// The phone's camera roll uploaded straight to Immich by the Immich app, a desktop that has only
/// Immich's link-only photo of it, and the phone itself with the file: device sync sees one
/// photo, keyed alike on both, and never asks the phone for an original Immich serves.
#[test]
fn a_link_only_immich_photo_and_the_file_are_one_photo() {
    let dir = lcc_testkit::TempDir::new("lcc-engine-sync-linkonly");
    let src = dir.join("roll.png");
    write_png(&src, 7);
    let (mut phone, pp) = offline(&dir, "phone");
    let (mut desk, dp) = offline(&dir, "desk");
    let ip =
        lcc_catalog::PhotoId(phone.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap()["imported"][0].as_u64().unwrap());
    let idk = link_only(&mut desk, &src, "http://immich#u1");
    assert_eq!(key_of(&phone, ip.0), key_of(&desk, idk.0), "the same key from the file and from Immich's checksum");
    assert!(key_of(&desk, idk.0).is_some_and(|k| k.starts_with("sha1:")));
    // the link-only photo's record carries none of the preview's identity
    let info = super::record::info(desk.catalog.photo(idk).unwrap());
    assert!(info["content_hash"].is_null() && info["sha1"].is_null() && info["preview_only"].is_null());

    let roll = phone.execute("album.create", &json!({"name": "Roll"})).unwrap()["id"].as_u64().unwrap();
    phone.selection = crate::Selection::single(ip);
    phone.execute("album.addPhotos", &json!({"id": roll, "ids": [ip.0]})).unwrap();
    phone.execute("photo.rate", &json!({"ids": [ip.0], "rating": 5})).unwrap();
    offline_group(&mut phone, &pp, &mut desk, &dp);
    phone.execute("sync.allow", &json!({"album": roll, "allow": "sync"})).unwrap();
    let cid = phone.device_sync.lib.as_ref().unwrap().replica.collection_of_album(roll).unwrap().to_string();
    swap(&mut phone, &pp, &mut desk, &dp);
    desk.execute("sync.take", &json!({"collection": cid, "take": "fullQuality"})).unwrap();
    swap(&mut phone, &pp, &mut desk, &dp);
    assert_eq!(desk.catalog.photos().count(), 1, "no second photo for the phone's copy");
    assert_eq!(desk.catalog.photo(idk).unwrap().rating, 5);
    assert!(desk.catalog.albums().any(|a| a.name == "Roll" && a.photos == vec![idk]));
    // the desktop's edit goes back to the phone's photo
    desk.execute("photo.rate", &json!({"ids": [idk.0], "rating": 2})).unwrap();
    swap(&mut phone, &pp, &mut desk, &dp);
    assert_eq!(phone.catalog.photo(ip).unwrap().rating, 2);
    assert_eq!(phone.catalog.photos().count(), 1);
    // its original comes from Immich, not from the phone
    let mut l = desk.device_sync.lib.take().unwrap();
    let wants = super::catalog::wants(&desk, &mut l, &pp.device_id);
    desk.device_sync.lib = Some(l);
    assert!(wants.iter().all(|w| w.depth != lcc_sync::pill::Depth::Full), "{:?}", wants.iter().map(|w| &w.key).collect::<Vec<_>>());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Keys from another device are hostile: a bad SHA-1 key, a content hash naming nothing, an
/// oversized or empty one change nothing here and never panic.
#[test]
fn hostile_photo_keys_change_nothing() {
    let dir = lcc_testkit::TempDir::new("lcc-engine-sync-hostile-keys");
    let src = dir.join("x.png");
    write_png(&src, 3);
    let (mut a, ap) = offline(&dir, "a");
    let (mut b, bp) = offline(&dir, "b");
    b.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    offline_group(&mut a, &ap, &mut b, &bp);
    let long = "k".repeat(10_000);
    for (key, info) in [
        ("sha1:zz", json!({"content_hash": "nothing"})),
        ("sha1:", json!({})),
        (long.as_str(), json!({"content_hash": long})),
        ("", json!(null)),
        ("sha1:a9993e364706816aba3e25717850c26c9cd0d89d", json!({"content_hash": "sha1:a9993e364706816aba3e25717850c26c9cd0d89d"})),
        ("sha1:a9993e364706816aba3e25717850c26c9cd0d89d", json!({"content_hash": 5})),
    ] {
        let batch = lcc_sync::proto::Batch {
            items: vec![
                lcc_sync::replica::Item::Photo { key: key.into(), info, fields: Default::default(), here: Default::default() },
                lcc_sync::replica::Item::Member {
                    coll: "c".into(),
                    key: key.into(),
                    on: lcc_sync::clock::Lww::new(true, lcc_sync::clock::Stamp::ZERO),
                },
            ],
            upto: 1,
            more: false,
            ..Default::default()
        };
        let mut l = b.device_sync.lib.take().unwrap();
        let _ = super::catalog::apply(&mut b, &mut l, &ap, &batch);
        b.device_sync.lib = Some(l);
    }
    assert_eq!(b.catalog.photos().count(), 1);
    assert!(b.catalog.photos().all(|p| p.sha1.as_deref().is_some_and(|h| h.len() == 40)));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Another device claims a wrong SHA-1 for a photo here that isn't hashed: the photo is hashed
/// here, keeps what it computed, and the claim changes nothing in the catalogue.
#[test]
fn a_wrong_claimed_sha1_changes_nothing() {
    let dir = lcc_testkit::TempDir::new("lcc-engine-sync-wrong-sha1");
    let src = dir.join("x.png");
    write_png(&src, 11);
    let (mut a, ap) = offline(&dir, "a");
    let (mut b, bp) = offline(&dir, "b");
    let ib = lcc_catalog::PhotoId(b.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap()["imported"][0].as_u64().unwrap());
    let real = b.catalog.photo(ib).unwrap().sha1.clone().unwrap();
    b.apply_system(lcc_catalog::Op::SetSha1 { id: ib, sha1: None }).unwrap();
    offline_group(&mut a, &ap, &mut b, &bp);
    let h = b.catalog.photo(ib).unwrap().content_hash.clone().unwrap();
    let wrong = "sha1:a9993e364706816aba3e25717850c26c9cd0d89d".to_string();
    let batch = lcc_sync::proto::Batch {
        items: vec![lcc_sync::replica::Item::Photo { key: wrong.clone(), info: json!({"content_hash": h}), fields: Default::default(), here: Default::default() }],
        upto: 1,
        more: false,
        ..Default::default()
    };
    let mut l = b.device_sync.lib.take().unwrap();
    super::catalog::apply(&mut b, &mut l, &ap, &batch).unwrap();
    b.device_sync.lib = Some(l);
    assert_eq!(b.catalog.photo(ib).unwrap().sha1, None, "the claim isn't written");
    assert!(b.remote.sha1_first.contains(&ib), "but the photo is hashed next");
    hash_all(&mut b);
    let mut l = b.device_sync.lib.take().unwrap();
    super::catalog::scan(&b, &mut l);
    b.device_sync.lib = Some(l);
    assert_eq!(b.catalog.photo(ib).unwrap().sha1.as_deref(), Some(real.as_str()));
    assert_eq!(key_of(&b, ib.0), Some(format!("sha1:{real}")));
    let l = b.device_sync.lib.as_ref().unwrap();
    assert!(!l.keys.contains_key(&wrong) && l.hints.is_empty(), "the claimed key names nothing here");
    assert_eq!(b.catalog.photos().count(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Importing a file an Immich link-only photo already stands for leaves one photo: the link-only
/// one takes the file, keeps its link and edits, and joins the import's collection.
#[test]
fn importing_the_original_of_a_link_only_photo_keeps_one_photo() {
    let dir = lcc_testkit::TempDir::new("lcc-engine-linkonly-import");
    let src = dir.join("roll.png");
    write_png(&src, 21);
    let (mut s, _) = offline(&dir, "d");
    let keep = link_only(&mut s, &src, "http://immich#u1");
    s.execute("photo.rate", &json!({"ids": [keep.0], "rating": 4})).unwrap();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()], "albumName": "Import"})).unwrap();
    assert_eq!(s.catalog.photos().count(), 1, "{r}");
    assert_eq!(r["imported"], json!([keep.0]));
    let p = s.catalog.photo(keep).unwrap();
    assert_eq!(p.preview_only, None);
    assert_eq!(p.rating, 4);
    assert!(matches!(&p.source, lcc_catalog::Source::File { path } if std::path::Path::new(path) == src.as_path()));
    assert_eq!(p.sha1.as_deref(), Some(lcc_hash::sha1_file(&src).unwrap().to_hex().as_str()));
    assert!(s.catalog.remote_of(keep).any(|r| r.remote_id == "asset-1"));
    assert!(s.catalog.albums().any(|a| a.name == "Import" && a.photos == vec![keep]));
    let _ = std::fs::remove_dir_all(&dir);
}

// ── Immich albums as collections ──────────────────────────────────────────────────────────────

/// Pumps both sessions' remote work (Immich workers) and the exchanges between them until `done`.
fn pump_both(a: &mut Session, ap: &lcc_peer::Peer, b: &mut Session, bp: &lcc_peer::Peer, what: &str, done: impl Fn(&Session, &Session) -> bool) {
    let t = Instant::now();
    while !done(a, b) {
        for s in [&mut *a, &mut *b] {
            s.execute("remote.pump", &json!({"sync": false})).unwrap();
        }
        swap(a, ap, b, bp);
        assert!(t.elapsed() < Duration::from_secs(30), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn immich_collection<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    v["catalogue"]["collections"].as_array()?.iter().find(|c| c["name"] == name && c["immich"] == true)
}

fn album_photos(s: &Session, name: &str) -> Vec<lcc_catalog::PhotoId> {
    s.catalog.albums().find(|a| a.name == name && !a.folder).map(|a| a.photos.clone()).unwrap_or_default()
}

/// Two devices with the same Immich account: its album is one collection under "Immich" on both;
/// each takes what it chose (link-only previews, originals, nothing); the photo has one key and
/// its edits travel; a photo added on either device is uploaded once.
#[test]
fn immich_albums_are_collections_each_device_takes_as_it_chose() {
    use crate::tests_immich::sync_tests::{Server, serve_state};
    let dir = lcc_testkit::TempDir::new("lcc-engine-sync-immich-colls");
    let x = crate::tests_immich::png(50);
    let mut server = Server::default();
    server.real_uploads = true;
    server.check_checksums = true;
    server.assets.push(crate::tests_immich::asset("as-1", "beach.png", &x, json!({"visibility": "timeline"})));
    server.bytes.insert("as-1".into(), x.clone());
    server.albums.push(("al-1".into(), "Holiday".into(), vec!["as-1".into()]));
    let state = std::sync::Arc::new(std::sync::Mutex::new(server));
    let url = serve_state(state.clone(), Default::default(), Default::default());
    let (mut a, ap) = offline(&dir, "a");
    let (mut b, bp) = offline(&dir, "b");
    for s in [&mut a, &mut b] {
        assert_eq!(s.execute("immich.connect", &json!({"url": url, "apiKey": crate::tests_immich::KEY})).unwrap()["ok"], true);
    }
    offline_group(&mut a, &ap, &mut b, &bp);
    for s in [&mut a, &mut b] {
        s.execute("immich.collections", &json!({"on": true})).unwrap();
        s.execute("sync.immichRefresh", &json!({})).unwrap();
    }
    pump_both(&mut a, &ap, &mut b, &bp, "the album as a collection", |a, b| {
        let names = |s: &Session| s.device_sync.lib.as_ref().is_some_and(|l| l.replica.collections.values().any(|e| e.c.is_immich() && e.c.meta.v.name == "Holiday"));
        names(a) && names(b)
    });
    let v = status(&mut a);
    let c = immich_collection(&v, "Holiday").unwrap().clone();
    assert_eq!(c["photos"], 1, "{c}");
    assert_eq!(a.catalog.photos().count(), 0, "nothing comes before it is taken");
    a.execute("sync.take", &json!({"collection": c["id"], "take": "previews"})).unwrap();
    b.execute("sync.take", &json!({"collection": c["id"], "take": "fullQuality"})).unwrap();
    assert!(a.execute("sync.take", &json!({"collection": c["id"], "take": "fullSync"})).is_err(), "Immich albums go up to full quality");
    for s in [&mut a, &mut b] {
        s.execute("sync.immichRefresh", &json!({})).unwrap();
    }
    pump_both(&mut a, &ap, &mut b, &bp, "the photo on both", |a, b| album_photos(a, "Holiday").len() == 1 && album_photos(b, "Holiday").len() == 1);
    let pa = a.catalog.photo(album_photos(&a, "Holiday")[0]).unwrap().clone();
    let pb = b.catalog.photo(album_photos(&b, "Holiday")[0]).unwrap().clone();
    assert_eq!(pa.preview_only.as_deref(), Some(crate::remote::LINK_ONLY), "previews: a link-only photo");
    assert_eq!(pb.preview_only, None, "full quality: the original");
    assert_eq!(key_of(&a, pa.id.0), key_of(&b, pb.id.0));
    for s in [&a, &b] {
        let folder = s.catalog.albums().find(|al| al.folder && al.name == "Immich").expect("an Immich set");
        assert!(s.catalog.albums().any(|al| al.name == "Holiday" && al.parent == Some(folder.id)));
        assert_eq!(s.catalog.photos().count(), 1);
    }
    // the record travels between the devices
    a.execute("photo.rate", &json!({"ids": [pa.id.0], "rating": 4})).unwrap();
    swap(&mut a, &ap, &mut b, &bp);
    assert_eq!(b.catalog.photo(pb.id).unwrap().rating, 4);

    // the same new photo added on both devices: uploaded once, then in the album
    let y = dir.join("new.png");
    std::fs::write(&y, crate::tests_immich::png(51)).unwrap();
    for s in [&mut a, &mut b] {
        let id = s.execute("library.import", &json!({"paths": [y.to_string_lossy()]})).unwrap()["imported"][0].as_u64().unwrap();
        let al = s.catalog.albums().find(|al| al.name == "Holiday" && !al.folder).unwrap().id.0;
        s.selection = crate::Selection::single(lcc_catalog::PhotoId(id));
        s.execute("album.addPhotos", &json!({"id": al, "ids": [id]})).unwrap();
        let t = Instant::now();
        let pid = lcc_catalog::PhotoId(id);
        while !s.catalog.remote_of(pid).any(|r| r.service == "immich") || s.remote.coll.pushing {
            let mut l = s.device_sync.lib.take().unwrap();
            super::catalog::scan(s, &mut l);
            s.device_sync.lib = Some(l);
            s.execute("remote.pump", &json!({"sync": false})).unwrap();
            assert!(t.elapsed() < Duration::from_secs(20), "timed out uploading");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let st = state.lock().unwrap();
    assert_eq!(st.uploads, 1, "the second device found it by checksum");
    assert_eq!(st.albums[0].2.len(), 2);
    drop(st);
    for s in [&a, &b] {
        let p = s.catalog.photos().find(|p| p.file_name == "new.png").unwrap();
        assert!(s.catalog.remote_of(p.id).any(|r| r.service == "immich"), "linked to the one asset");
    }
    // taking nothing removes the collection from the catalogue; its photos stay
    a.execute("sync.take", &json!({"collection": c["id"], "take": "none"})).unwrap();
    assert!(album_photos(&a, "Holiday").is_empty() && !a.catalog.albums().any(|al| al.name == "Holiday"));
    assert_eq!(a.catalog.photos().count(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A private Immich connection's collections stay on its device; shared, its key reaches the
/// other device's key store (only there) and that device lists and takes the albums itself.
#[test]
fn a_shared_immich_connection_reaches_the_other_device_and_a_private_one_does_not() {
    use crate::tests_immich::sync_tests::{Server, serve_state};
    let dir = lcc_testkit::TempDir::new("lcc-engine-sync-immich-share");
    let x = crate::tests_immich::png(60);
    let mut server = Server::default();
    server.assets.push(crate::tests_immich::asset("as-1", "dog.png", &x, json!({"visibility": "timeline"})));
    server.bytes.insert("as-1".into(), x);
    server.albums.push(("al-1".into(), "Pets".into(), vec!["as-1".into()]));
    let url = serve_state(std::sync::Arc::new(std::sync::Mutex::new(server)), Default::default(), Default::default());
    let (mut a, ap) = offline(&dir, "a");
    let (mut b, bp) = offline(&dir, "b");
    assert_eq!(a.execute("immich.connect", &json!({"url": url, "apiKey": crate::tests_immich::KEY})).unwrap()["ok"], true);
    offline_group(&mut a, &ap, &mut b, &bp);
    a.execute("immich.collections", &json!({"on": true})).unwrap();
    a.execute("sync.immichRefresh", &json!({})).unwrap();
    let has = |s: &Session| s.device_sync.lib.as_ref().is_some_and(|l| l.replica.collections.values().any(|e| e.c.meta.v.name == "Pets"));
    pump_both(&mut a, &ap, &mut b, &bp, "a's collections", |a, _| has(a));
    for _ in 0..5 {
        pump_both(&mut a, &ap, &mut b, &bp, "", |_, _| true);
        a.execute("remote.pump", &json!({"sync": false})).unwrap();
        swap(&mut a, &ap, &mut b, &bp);
    }
    assert!(!has(&b), "a private connection's collections aren't told");
    assert!(b.immich_accounts().unwrap().immich.is_empty(), "nor its key");

    a.execute("immich.share", &json!({"on": true})).unwrap();
    pump_both(&mut a, &ap, &mut b, &bp, "the shared connection at b", |_, b| has(b));
    let acc = b.immich_accounts().unwrap().immich[0].clone();
    assert!(acc.shared && acc.collections && acc.user_id == "user-1");
    let key = b.secret_store().unwrap().get(&lcc_immich::accounts::secret_key(&acc.id)).unwrap().unwrap();
    assert_eq!(key.expose(), crate::tests_immich::KEY, "the key is in b's key store");
    let replica = serde_json::to_string(&b.device_sync.lib.as_ref().unwrap().replica).unwrap();
    assert!(!replica.contains(crate::tests_immich::KEY), "never in the replica");
    for cmd in ["sync.status", "immich.status"] {
        assert!(!b.execute(cmd, &json!({})).unwrap().to_string().contains(crate::tests_immich::KEY), "never in {cmd}");
    }
    // b takes the album and brings the photo in from Immich with its own (shared) key
    let cid = b.device_sync.lib.as_ref().unwrap().replica.collections.iter().find(|(_, e)| e.c.meta.v.name == "Pets").unwrap().0.clone();
    b.execute("sync.take", &json!({"collection": cid, "take": "previews"})).unwrap();
    b.execute("sync.immichRefresh", &json!({})).unwrap();
    pump_both(&mut a, &ap, &mut b, &bp, "the photo at b", |_, b| album_photos(b, "Pets").len() == 1);
    // hostile shared connections are dropped
    let bad = lcc_sync::proto::SharedAccount { url: "x".repeat(5000), user_id: String::new(), user_name: String::new(), key: "\u{0}".into(), pinned: None };
    assert!(!bad.sane());
    assert!(!format!("{bad:?}").contains('\u{0}') && format!("{:?}", lcc_sync::proto::SharedAccount { key: "secret-k".into(), ..bad }).contains("<hidden>"));
    let _ = std::fs::remove_dir_all(&dir);
}
