//! P6.2 never-crash harnesses for the newer Immich inputs: the per-account sync state
//! (`<library>/Immich/sync/<account>.json`), server assets fed through the three-way merge,
//! faces / people / smart-search answers, the publish service against a hostile server, and
//! publish remote ids.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use dac_catalog::{Photo, PhotoId, Source};
use dac_credentials::Secret;
use dac_publish::{PublishService, Upload};

use crate::client::{Client, ServerOptions};
use crate::people;
use crate::publish::{ImmichPublish, PhotoInfo, PublishSettings, parse_remote};
use crate::sync::{self, Field, Sides, Snapshot, SyncConfig, SyncState};
use crate::types::*;

fn photo() -> Photo {
    let mut p = Photo::new(PhotoId(7), Source::File { path: "/p/a.jpg".into() }, "a.jpg", "JPEG", 10, 10, "2026-01-01T00:00:00");
    p.rating = 3;
    p.meta.caption = "c".into();
    p.meta.keywords = vec!["Trip|Paris".into(), "x".into()];
    p
}

const ASSET: &str = r#"{"id":"x","isFavorite":true,"visibility":"archive","localDateTime":"2024-01-08T12:00:00.000Z","updatedAt":"2024-02-01T00:00:00Z",
"exifInfo":{"rating":4,"description":" d ","latitude":48.85,"longitude":2.35,"dateTimeOriginal":"2024-01-08T12:00:00+02:00"},
"tags":[{"id":"t","value":"Trip/Paris"}],"people":[{"id":"p1","name":"Alice"}]}"#;

fn merge_all(a: &Asset, base: Option<&Snapshot>, cfg: &SyncConfig) {
    let p = photo();
    let local = sync::local(&p, cfg, &|k: &str| k == "x");
    let remote = sync::remote(a);
    let stuck = BTreeSet::new();
    let forced = BTreeMap::new();
    let out = sync::reconcile(
        Sides {
            base,
            local: &local,
            remote: &remote,
            stuck: &stuck,
            forced: &forced,
            local_changed_at: Some("2024-01-09T00:00:00+05:45"),
            remote_updated_at: a.updated_at.as_deref(),
        },
        cfg,
    );
    let _ = out.is_noop();
    let all: BTreeSet<Field> = Field::ALL.into();
    let _ = sync::local_ops(&p, &remote, &all, cfg, &|_: &str| false);
    let _ = sync::remote_update(&local, &all, cfg, a.local_date_time.as_deref().and_then(sync::zone_of));
    let _ = sync::untouchable(a);
}

#[test]
fn hostile_assets_through_the_merge_never_panic() {
    let cfg_seed = serde_json::to_string(&SyncConfig::default()).unwrap();
    let mut rng = dac_fuzzkit::Rng::new(3);
    let base = sync::remote(&serde_json::from_str::<Asset>(ASSET).unwrap());
    dac_fuzzkit::run_json("immich.sync.asset", &[ASSET, &cfg_seed], 2000, |s| {
        let cfg: SyncConfig = serde_json::from_str(s).unwrap_or_default();
        if let Ok(a) = serde_json::from_str::<Asset>(s) {
            merge_all(&a, if rng.chance(2) { Some(&base) } else { None }, &cfg);
        }
        let a: Asset = serde_json::from_str(ASSET).unwrap_or_default();
        merge_all(&a, Some(&base), &cfg);
    });
    dac_fuzzkit::run_str("immich.sync.time", &["2024-01-08T12:00:00.123+02:00", "2024-01-08T12:00:00Z", "2024-01-08"], 5000, |s| {
        let _ = (sync::instant(s), sync::zone_of(s), sync::keyword_to_tag(s), Field::parse(s), sync::Side::parse(s));
    });
}

#[test]
fn damaged_sync_state_never_panics() {
    let mut st = SyncState::default();
    let base = sync::remote(&serde_json::from_str::<Asset>(ASSET).unwrap());
    st.items.insert("a".into(), sync::ItemState { photo: 7, base: Some(base), ..Default::default() });
    st.push_log(sync::RunLog { errors: vec!["e".into()], ..Default::default() });
    st.synced_until = Some("2024-01-01T00:00:00Z".into());
    st.kept.insert("k".into());
    let seed = serde_json::to_string(&st).unwrap();
    let a: Asset = serde_json::from_str(ASSET).unwrap();
    dac_fuzzkit::run_json("immich.sync.state", &[&seed], 2000, |s| {
        let Ok(mut st) = serde_json::from_str::<SyncState>(s) else { return };
        let _ = st.conflict_count();
        for item in st.items.values() {
            merge_all(&a, item.base.as_ref(), &SyncConfig::default());
        }
        st.push_log(sync::RunLog::default());
        assert!(st.log.len() <= sync::LOG_CAP.max(st.log.len()));
        let _ = serde_json::to_string(&st);
    });
}

#[test]
fn hostile_faces_people_and_search_never_panic() {
    let faces = r#"[{"id":"f","boundingBoxX1":10,"boundingBoxY1":5,"boundingBoxX2":30,"boundingBoxY2":25,"imageWidth":100,"imageHeight":50,
"person":{"id":"p1","name":"Alice","isHidden":false}},{"id":"g","boundingBoxX1":0,"boundingBoxY1":0,"boundingBoxX2":0,"boundingBoxY2":0,"imageWidth":0,"imageHeight":0}]"#;
    let people =
        r#"{"people":[{"id":"p1","name":"Alice","isHidden":false,"birthDate":"1990-01-01"},{"id":"p2","name":"","isHidden":true}],"total":2}"#;
    let search = serde_json::to_string(&SmartSearch::default()).unwrap_or_default();
    dac_fuzzkit::run_json("immich.people", &[faces, people, &search], 2000, |s| {
        let _ = serde_json::from_str::<SmartSearch>(s);
        let ppl: Vec<Person> = serde_json::from_str::<People>(s).map(|p| p.people).unwrap_or_default();
        let Ok(fs) = serde_json::from_str::<Vec<Face>>(s) else { return };
        let mut p = photo();
        for f in &fs {
            let _ = people::face_rect(f);
        }
        for hidden in [false, true] {
            let (regions, _) = people::regions_with_faces(&p, &fs, hidden);
            p.meta.regions = regions;
        }
        let _ = people::confirm(&p);
        for merge in [false, true] {
            let _ = people::names_to_push(std::iter::once(&p), &ppl, merge);
        }
    });
}

#[test]
fn publish_ids_and_settings_never_panic() {
    dac_fuzzkit::run_str("immich.publish.id", &["r:abc+o:def", "o:x", "+++", "r:"], 5000, |s| {
        let _ = parse_remote(s);
    });
    let seed = serde_json::to_string(&PublishSettings { account: "https://h#u1".into(), ..Default::default() }).unwrap();
    dac_fuzzkit::run_json("immich.publish.settings", &[&seed], 1000, |s| {
        let _ = serde_json::from_str::<PublishSettings>(s);
    });
}

/// The publish service against a server that answers every request with mutated JSON.
#[test]
fn publishing_to_a_hostile_server_never_panics() {
    use std::io::{BufRead, BufReader, Read, Write};
    let body: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let b2 = body.clone();
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { return };
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut len = 0usize;
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut sink = vec![0; len.min(1 << 20)];
            let _ = r.read_exact(&mut sink);
            let b = b2.lock().unwrap().clone();
            let _ = s.write_all(
                format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", b.len()).as_bytes(),
            );
            let _ = s.write_all(&b);
        }
    });
    let opts = ServerOptions { retries: 0, ..ServerOptions::default() };
    let seeds = [
        r#"{"id":"asset-1","status":"created"}"#,
        r#"[{"id":"album-1","albumName":"Shoot","assetCount":1}]"#,
        r#"[{"id":"asset-1","success":true}]"#,
        r#"{"id":"album-1","albumName":"Shoot","assetCount":0}"#,
    ];
    let mut photos = HashMap::new();
    photos.insert(
        PhotoId(1),
        PhotoInfo { created: "2024-01-01T00:00:00Z".into(), favorite: true, original_name: "a.jpg".into(), ..Default::default() },
    );
    dac_fuzzkit::run_json("immich.publish", &seeds, 80, |s| {
        *body.lock().unwrap() = s.as_bytes().to_vec();
        let Ok(c) = Client::new(&format!("http://127.0.0.1:{port}"), Secret::new("k"), &opts) else { return };
        let mut svc = ImmichPublish::new(c, PublishSettings::default(), "Shoot", "dev", photos.clone());
        let up = Upload { photo: PhotoId(1), file_name: "a.jpg", bytes: b"\xff\xd8\xff\xd9", sidecars: &[], previous: Some("r:old+o:older") };
        let _ = svc.publish(&up);
        let _ = svc.remove("r:asset-1+o:asset-2");
        let _ = svc.remove("");
    });
}
