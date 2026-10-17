//! Integration tests against a real server: `cargo xtask immich up && cargo xtask immich seed`,
//! then `cargo test -p lcc-immich --test live -- --ignored` (the nightly job). The server address
//! is `IMMICH_URL` (default `http://127.0.0.1:2284`), the key is read from `target/immich/api-key`.
#![cfg(not(target_arch = "wasm32"))]

use std::path::PathBuf;

use lcc_credentials::Secret;
use lcc_immich::types::MetadataSearch;
use lcc_immich::{Client, ImmichError, ServerOptions};

fn server() -> Option<(String, Secret)> {
    let url = std::env::var("IMMICH_URL").unwrap_or_else(|_| "http://127.0.0.1:2284".into());
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/immich/api-key");
    let key = std::fs::read_to_string(root).ok()?;
    Some((url, Secret::new(key.trim())))
}

#[test]
#[ignore = "needs `cargo xtask immich up` and `seed` (nightly)"]
fn live_connect_list_and_download() {
    let Some((url, key)) = server() else { panic!("no target/immich/api-key: run `cargo xtask immich seed`") };
    let c = Client::new(&url, key, &ServerOptions::standard()).unwrap();
    let st = c.status().unwrap();
    assert!(st.version >= lcc_immich::MIN_VERSION);
    let mut all = Vec::new();
    c.search_all(&MetadataSearch { size: Some(3), with_exif: Some(true), ..Default::default() }, 100, |p| {
        all.extend_from_slice(p);
        true
    })
    .unwrap();
    assert!(all.len() >= 8, "seeded fixtures: {}", all.len());
    assert!(c.albums().unwrap().iter().any(|a| a.album_name == "xtask fixtures"));
    let a = &all[0];
    // a freshly seeded server makes thumbnails in the background: wait for it (CI starts empty)
    let mut thumb = c.thumbnail(&a.id, "thumbnail");
    for _ in 0..60 {
        if !matches!(thumb, Err(ImmichError::NotFound(_))) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
        thumb = c.thumbnail(&a.id, "thumbnail");
    }
    let thumb = thumb.unwrap();
    assert!(!thumb.is_empty());
    let dir = std::env::temp_dir().join(format!("lcc-immich-live-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.join(&a.original_file_name);
    c.download_original(&a.id, &dest, None).unwrap();
    let sha = lcc_hash::sha1_file(&dest).unwrap();
    assert_eq!(sha.to_base64(), a.checksum, "the original's SHA-1 is Immich's checksum");
    let _ = std::fs::remove_dir_all(&dir);
    // a bad key is refused with BadKey
    let bad = Client::new(&url, Secret::new("not-a-key"), &ServerOptions::default()).unwrap();
    assert_eq!(bad.me().unwrap_err(), ImmichError::BadKey);
}

/// IMM-SYNC / IMM-PUBLISH / IMM-SEARCH calls against the real server: the checksum pre-check and
/// upload, update, tags, albums, stacks, faces, people, smart search.
#[test]
#[ignore = "needs `cargo xtask immich up` and `seed` (nightly)"]
fn live_sync_publish_and_search_calls() {
    use lcc_immich::client::{NewAsset, UploadData};
    use lcc_immich::types::{AssetUpdate, SmartSearch, UploadCheck};
    let Some((url, key)) = server() else { panic!("no target/immich/api-key: run `cargo xtask immich seed`") };
    let c = Client::new(&url, key, &ServerOptions::standard()).unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/immich/fixtures/fixture-00.png");
    let bytes = std::fs::read(&fixture).unwrap();
    let sha = lcc_hash::sha1_bytes(&bytes).to_base64();
    let check = c.upload_check(&[UploadCheck { id: "x".into(), checksum: sha }]).unwrap();
    assert_eq!(check[0].action, "reject", "the seeded fixture is a duplicate: {check:?}");
    let existing = check[0].asset_id.clone().unwrap();
    let (id, dup) = c
        .upload(&NewAsset {
            data: UploadData::Bytes(&bytes),
            file_name: "fixture-00.png",
            mime: "image/png",
            device_asset_id: "live-test",
            device_id: "live",
            created: "2024-01-01T12:00:00.000Z",
            favorite: false,
            sidecar: None,
        })
        .unwrap();
    assert!(dup && id == existing, "{id} {dup}");
    let u = AssetUpdate { rating: Some(Some(2)), description: Some("live sync".into()), ..Default::default() };
    c.update_asset(&id, &u).unwrap();
    assert_eq!(c.asset(&id).unwrap().exif_info.and_then(|e| e.rating), Some(2));
    let tags = c.upsert_tags(&["live/sync".into()]).unwrap();
    let t = tags.iter().find(|t| t.value == "live/sync").unwrap();
    c.tag_assets(&t.id, std::slice::from_ref(&id)).unwrap();
    assert!(c.asset(&id).unwrap().tags.iter().any(|x| x.value == "live/sync"));
    c.untag_assets(&t.id, std::slice::from_ref(&id)).unwrap();
    c.update_asset(&id, &AssetUpdate { rating: Some(None), description: Some(String::new()), ..Default::default() }).unwrap();
    let al = c.create_album("live publish test").unwrap();
    c.album_add(&al.id, std::slice::from_ref(&id)).unwrap();
    assert_eq!(c.album_assets(&al.id).unwrap(), vec![id.clone()]);
    c.album_remove(&al.id, std::slice::from_ref(&id)).unwrap();
    assert!(c.album_assets(&al.id).unwrap().is_empty());
    c.rename_album(&al.id, "live publish test (renamed)").unwrap();
    c.faces(&id).unwrap();
    c.people_all().unwrap();
    c.smart_search(&SmartSearch { query: "a colourful picture".into(), size: Some(5), ..Default::default() }).unwrap();
    let two: Vec<String> = c.search(&MetadataSearch { size: Some(2), ..Default::default() }).unwrap().items.into_iter().map(|a| a.id).collect();
    if two.len() == 2 {
        c.stack(&two).unwrap();
    }
}

/// IMM-PUBLISH end to end: a collection published, renamed into a collection set (the same album
/// is renamed), and published again without a duplicate upload.
#[test]
#[ignore = "needs `cargo xtask immich up` and `seed` (nightly)"]
fn live_publish_renames_its_album() {
    use lcc_publish::{PublishService, Upload};
    let Some((url, key)) = server() else { panic!("no target/immich/api-key: run `cargo xtask immich seed`") };
    let open = |name: &str, state: &std::path::Path| {
        let c = Client::new(&url, Secret::new(key.expose()), &ServerOptions::standard()).unwrap();
        lcc_immich::publish::ImmichPublish::new(c, Default::default(), name, "live", Default::default()).with_album_state(state.to_path_buf())
    };
    let dir = std::env::temp_dir().join(format!("lcc-immich-live-pub-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let state = dir.join("svc-1-1.album");
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let (first, second) = (format!("live pub {stamp}"), format!("Trips / live pub {stamp}"));
    // a seeded fixture: already on the server, so publishing finds it by checksum
    let bytes = std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/immich/fixtures/fixture-00.png")).unwrap();
    let up = Upload { photo: lcc_catalog::PhotoId(1), file_name: "fixture-00.png", bytes: &bytes, sidecars: &[], previous: None };
    let r1 = open(&first, &state).publish(&up).unwrap().remote_id;
    let album = std::fs::read_to_string(&state).unwrap();
    let r2 = open(&second, &state).publish(&up).unwrap().remote_id;
    assert_eq!(r1, r2, "found by checksum, not uploaded again");
    let c = Client::new(&url, key, &ServerOptions::standard()).unwrap();
    let all = c.albums().unwrap();
    assert!(all.iter().any(|a| a.id == album && a.album_name == second), "renamed in place");
    assert!(!all.iter().any(|a| a.album_name == first));
    let _ = std::fs::remove_dir_all(&dir);
}

/// RT.C1/C3 against the real server: stack the fixtures, read the stack back from both
/// `GET /stacks` and the asset, re-stack with a new primary (merging), find albums by asset, and
/// dissolve the stack.
#[test]
#[ignore = "needs `cargo xtask immich up` and `seed` (nightly)"]
fn live_stacks_round_trip() {
    let Some((url, key)) = server() else { panic!("no target/immich/api-key: run `cargo xtask immich seed`") };
    let c = Client::new(&url, key, &ServerOptions::standard()).unwrap();
    let mut all = Vec::new();
    c.search_all(&MetadataSearch { size: Some(50), ..Default::default() }, 10, |p| {
        all.extend_from_slice(p);
        true
    })
    .unwrap();
    let ids: Vec<String> = all.iter().filter(|a| !a.is_trashed).take(3).map(|a| a.id.clone()).collect();
    assert_eq!(ids.len(), 3, "seeded fixtures");
    // start clean
    let old: Vec<String> = c.stacks().unwrap().into_iter().filter(|s| s.assets.iter().any(|a| ids.contains(&a.id))).map(|s| s.id).collect();
    if !old.is_empty() {
        c.delete_stacks(&old).unwrap();
    }
    c.stack(&ids[..2]).unwrap();
    let st = c.asset_stack(&ids[1]).unwrap().expect("stacked");
    assert_eq!(st.primary_asset_id, ids[0]);
    // a new primary listed with the old primary: Immich merges the old stack in
    c.stack(&[ids[2].clone(), ids[0].clone()]).unwrap();
    let mine: Vec<_> = c.stacks().unwrap().into_iter().filter(|s| s.assets.iter().any(|a| ids.contains(&a.id))).collect();
    assert_eq!(mine.len(), 1, "{mine:?}");
    assert_eq!(mine[0].primary_asset_id, ids[2]);
    assert_eq!(mine[0].assets.len(), 3);
    let albums = c.albums_of(&ids[0]).unwrap();
    assert!(albums.iter().any(|a| a.album_name == "xtask fixtures"), "{albums:?}");
    c.delete_stacks(&[mine[0].id.clone()]).unwrap();
    assert!(c.asset_stack(&ids[0]).unwrap().is_none());
}

/// IMM-SYNC album rule: the calls it makes on a real server (create, add, rename, remove, delete);
/// deleting the album leaves its assets on the server.
#[test]
#[ignore = "needs `cargo xtask immich up` and `seed` (nightly)"]
fn live_album_create_rename_delete_keeps_assets() {
    let Some((url, key)) = server() else { panic!("no target/immich/api-key: run `cargo xtask immich seed`") };
    let c = Client::new(&url, key, &ServerOptions::standard()).unwrap();
    let mut some = Vec::new();
    c.search_all(&MetadataSearch { size: Some(2), ..Default::default() }, 1, |p| {
        some.extend(p.iter().map(|a| a.id.clone()));
        false
    })
    .unwrap();
    assert!(some.len() >= 2, "seeded fixtures");
    let name = format!("album sync {}", std::process::id());
    let a = c.create_album(&name).unwrap();
    c.album_add(&a.id, &some).unwrap();
    let renamed = format!("{name} renamed");
    c.rename_album(&a.id, &renamed).unwrap();
    assert!(c.albums().unwrap().iter().any(|x| x.id == a.id && x.album_name == renamed));
    c.album_remove(&a.id, &some[..1]).unwrap();
    assert_eq!(c.album_assets(&a.id).unwrap().len(), some.len() - 1);
    c.delete_album(&a.id).unwrap();
    assert!(!c.albums().unwrap().iter().any(|x| x.id == a.id));
    assert!(c.asset(&some[0]).is_ok(), "the album's assets stay");
}
