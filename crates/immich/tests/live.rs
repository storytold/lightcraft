//! Integration tests against a real server: `cargo xtask immich up && cargo xtask immich seed`,
//! then `cargo test -p dac-immich --test live -- --ignored` (the nightly job). The server address
//! is `IMMICH_URL` (default `http://127.0.0.1:2284`), the key is read from `target/immich/api-key`.
#![cfg(not(target_arch = "wasm32"))]

use std::path::PathBuf;

use dac_credentials::Secret;
use dac_immich::types::MetadataSearch;
use dac_immich::{Client, ImmichError, ServerOptions};

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
    assert!(st.version >= dac_immich::MIN_VERSION);
    let mut all = Vec::new();
    c.search_all(&MetadataSearch { size: Some(3), with_exif: Some(true), ..Default::default() }, 100, |p| {
        all.extend_from_slice(p);
        true
    })
    .unwrap();
    assert!(all.len() >= 8, "seeded fixtures: {}", all.len());
    assert!(c.albums().unwrap().iter().any(|a| a.album_name == "xtask fixtures"));
    let a = &all[0];
    let thumb = c.thumbnail(&a.id, "thumbnail").unwrap();
    assert!(!thumb.is_empty());
    let dir = std::env::temp_dir().join(format!("dac-immich-live-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.join(&a.original_file_name);
    c.download_original(&a.id, &dest, None).unwrap();
    let sha = dac_hash::sha1_file(&dest).unwrap();
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
    use dac_immich::client::{NewAsset, UploadData};
    use dac_immich::types::{AssetUpdate, SmartSearch, UploadCheck};
    let Some((url, key)) = server() else { panic!("no target/immich/api-key: run `cargo xtask immich seed`") };
    let c = Client::new(&url, key, &ServerOptions::standard()).unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/immich/fixtures/fixture-00.png");
    let bytes = std::fs::read(&fixture).unwrap();
    let sha = dac_hash::sha1_bytes(&bytes).to_base64();
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
