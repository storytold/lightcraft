//! Never-crash (P6.2): a damaged or hand-edited `publish.json` is an error or a bounded config,
//! never a panic; publishing to a full disk is an error that leaves the published folder as it was.

use dac_catalog::{AlbumId, PhotoId};
use dac_publish::hard_drive::safe_name;
use dac_publish::{CollectionConfig, PublishConfig, ServiceConfig, Upload, open_service};

fn scratch(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("dac-publish-robust-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn sample(out: &std::path::Path) -> PublishConfig {
    let mut c = PublishConfig::default();
    let id = c.alloc_id();
    c.services.push(ServiceConfig {
        id,
        kind: dac_publish::KIND_HARD_DRIVE.into(),
        name: "Portfolio".into(),
        settings: serde_json::json!({ "dir": out.display().to_string() }),
        export: serde_json::json!({ "format": "jpeg", "quality": 80, "long_edge": 2048 }),
        set: Some(AlbumId(7)),
        collections: vec![CollectionConfig { album: AlbumId(8), folder: "Best of".into() }],
    });
    c
}

#[test]
fn damaged_publish_json_never_panics() {
    let dir = scratch("cfg");
    let out = dir.join("out");
    sample(&out).save(&dir).unwrap();
    let seed = std::fs::read_to_string(dir.join(dac_publish::FILE)).unwrap();
    dac_fuzzkit::run_json("publish.json", &[&seed], 1500, |s| {
        std::fs::write(dir.join(dac_publish::FILE), s).unwrap();
        let Ok(mut cfg) = PublishConfig::load(&dir) else { return };
        assert!(cfg.services.len() <= dac_publish::config::MAX_SERVICES);
        let _ = cfg.alloc_id();
        for svc in &cfg.services {
            let _ = cfg.service(&svc.id);
            for coll in &svc.collections {
                // opening checks the folder names; nothing is written here
                if let Ok(s) = open_service(svc, coll) {
                    let _ = s.capabilities();
                }
            }
        }
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn file_names_never_escape_the_folder() {
    dac_fuzzkit::run_str("publish.safe_name", &["IMG_0001.jpg", "../a/b\\c:d", " . ", "\u{0}x\u{202e}"], 5000, |s| {
        if let Some(n) = safe_name(s) {
            assert!(!n.contains(['/', '\\', ':', '\0']) && n != "." && n != ".." && !n.is_empty(), "{n:?}");
        }
    });
}

#[test]
fn publishing_to_a_full_disk_is_an_error_and_keeps_the_old_file() {
    let dir = scratch("full");
    let cfg = sample(&dir);
    let (svc, coll) = (&cfg.services[0], &cfg.services[0].collections[0]);
    let mut s = open_service(svc, coll).unwrap();
    let up = |bytes: &'static [u8]| Upload { photo: PhotoId(1), file_name: "a.jpg", bytes, sidecars: &[], previous: None };
    let first = s.publish(&up(b"first version")).unwrap();
    let path = dir.join("Best of").join(&first.remote_id);
    {
        let _full = dac_catalog::safe_file::fail_writes_after(4);
        let e = s.publish(&up(b"second, longer version")).unwrap_err();
        assert!(e.to_string().contains("a.jpg"), "{e}");
    }
    assert_eq!(std::fs::read(&path).unwrap(), b"first version");
    // no temp files left behind
    let names: Vec<_> = std::fs::read_dir(dir.join("Best of")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names.len(), 1, "{names:?}");
    // saving publish.json to a full disk: an error, the old file intact
    cfg.save(&dir).unwrap();
    let before = std::fs::read(dir.join(dac_publish::FILE)).unwrap();
    {
        let _full = dac_catalog::safe_file::fail_writes_after(10);
        assert!(PublishConfig::default().save(&dir).is_err());
    }
    assert_eq!(std::fs::read(dir.join(dac_publish::FILE)).unwrap(), before);
    let _ = std::fs::remove_dir_all(&dir);
}
