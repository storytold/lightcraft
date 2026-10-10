use std::path::Path;

use crate::*;

fn l(v: &[(&str, u64)]) -> Vec<(String, u64)> {
    v.iter().map(|(p, s)| (p.to_string(), *s)).collect()
}

#[test]
fn a_shot_is_imported_once_its_size_holds_still() {
    let mut s = StudioSession::new("Shoot", "/in");
    assert!(s.fresh(&l(&[("/in/a.cr3", 100)])).is_empty());
    // still growing
    assert!(s.fresh(&l(&[("/in/a.cr3", 200)])).is_empty());
    assert_eq!(s.fresh(&l(&[("/in/a.cr3", 200), ("/in/b.cr3", 0)])), vec!["/in/a.cr3"]);
    // never twice; the empty file waits
    assert!(s.fresh(&l(&[("/in/a.cr3", 200), ("/in/b.cr3", 0)])).is_empty());
    assert_eq!(s.fresh(&l(&[("/in/b.cr3", 5)])), Vec::<String>::new());
    assert_eq!(s.fresh(&l(&[("/in/b.cr3", 5)])), vec!["/in/b.cr3"]);
    assert_eq!(s.taken(), 2);
}

#[test]
fn existing_files_can_be_skipped() {
    let mut s = StudioSession::new("x", "/in");
    s.skip_existing(&l(&[("/in/old.jpg", 9)]));
    s.fresh(&l(&[("/in/old.jpg", 9)]));
    assert!(s.fresh(&l(&[("/in/old.jpg", 9)])).is_empty());
}

#[test]
fn import_params_carry_the_session() {
    let mut s = StudioSession::new("Studio/A {x}", "/in");
    assert_eq!(s.name, "Studio_A _x_");
    s.copy = true;
    s.naming = Some("{session}-{seq:4}".into());
    s.preset = Some("p1".into());
    s.metadata_preset = Some("Studio".into());
    s.keywords = vec!["studio".into()];
    s.collection = Some("Shoot".into());
    s.imported(&[7, 8]);
    assert_eq!(s.newest, Some(8));
    let p = s.import_params(&["/in/a.jpg".into()], Some(Path::new("/lib/Originals")));
    assert_eq!(p["mode"], "copy");
    assert_eq!(p["rename"], "Studio_A _x_-{seq:4}");
    assert_eq!(p["renameStart"], 3);
    assert_eq!(p["preset"], "p1");
    assert_eq!(p["metadataPreset"], "Studio");
    assert_eq!(p["keywords"][0], "studio");
    assert_eq!(p["albumName"], "Shoot");
    assert_eq!(p["organize"], "flat");
    assert!(p["destination"].as_str().unwrap().ends_with("Studio_A _x_"));
    s.copy = false;
    let p = s.import_params(&[], None);
    assert_eq!(p["mode"], "add");
    assert!(p.get("rename").is_none());
    assert_eq!(safe_session_name("  ..  "), "Session");
}

#[test]
fn session_file_round_trips_and_damage_is_reported() {
    let d = std::env::temp_dir().join(format!("dac-tether-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    assert_eq!(StudioSession::load(&d).unwrap(), None);
    let mut s = StudioSession::new("A", "/in");
    s.fresh(&l(&[("/in/a", 1)]));
    StudioSession::save(Some(&s), &d).unwrap();
    assert_eq!(StudioSession::load(&d).unwrap(), Some(s));
    StudioSession::save(None, &d).unwrap();
    assert_eq!(StudioSession::load(&d).unwrap(), None);
    std::fs::write(d.join(FILE), b"[1,").unwrap();
    assert!(StudioSession::load(&d).is_err());
    std::fs::write(d.join("f.jpg"), b"x").unwrap();
    std::fs::write(d.join(".hidden"), b"x").unwrap();
    let names: Vec<String> = list_folder(&d).unwrap().into_iter().map(|(p, _)| p).collect();
    assert!(names.iter().any(|p| p.ends_with("f.jpg")) && !names.iter().any(|p| p.ends_with(".hidden")));
    assert!(list_folder(&d.join("nope")).is_err());
}
