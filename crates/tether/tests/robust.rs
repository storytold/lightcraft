//! Never-crash (P6.2): a damaged or hand-edited `tether.json` is an error or a usable session,
//! never a panic.

use dac_tether::{StudioSession, safe_session_name};

#[test]
fn damaged_tether_json_never_panics() {
    let dir = std::env::temp_dir().join(format!("dac-tether-robust-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = StudioSession::new("Shoot {1}", "/watched");
    s.copy = true;
    s.naming = Some("{session}-{seq:4}".into());
    s.keywords = vec!["studio".into()];
    s.collection = Some("Shoot".into());
    s.preset = Some("p1".into());
    let listing = vec![("/watched/a.cr3".to_string(), 10u64), ("/watched/b.cr3".to_string(), 0)];
    s.fresh(&listing);
    s.fresh(&listing);
    s.imported(&[3, 4]);
    StudioSession::save(Some(&s), &dir).unwrap();
    let seed = std::fs::read_to_string(dir.join(dac_tether::FILE)).unwrap();
    dac_fuzzkit::run_json("tether.json", &[&seed, "null"], 2000, |t| {
        std::fs::write(dir.join(dac_tether::FILE), t).unwrap();
        let Ok(Some(mut s)) = StudioSession::load(&dir) else { return };
        let _ = s.naming_template();
        let _ = s.import_params(&["/watched/c.cr3".into()], Some(std::path::Path::new("/lib/Originals")));
        s.imported(&[u64::MAX]);
        s.skip_existing(&listing);
        let _ = s.fresh(&listing);
        let _ = (s.taken(), s.to_json());
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn session_names_are_always_safe() {
    dac_fuzzkit::run_str("tether.name", &["Shoot 1", "../..", "{session}", " . "], 5000, |t| {
        let n = safe_session_name(t);
        assert!(!n.is_empty() && !n.contains(['/', '\\', ':', '\0', '{', '}']) && n != "..", "{n:?}");
    });
}
