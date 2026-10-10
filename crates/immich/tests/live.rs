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
