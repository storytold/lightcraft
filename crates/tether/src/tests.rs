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
fn session_file_round_trips_and_damage_is_reported() {
    let d = std::env::temp_dir().join(format!("lcc-tether-{}", std::process::id()));
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

/// P6.2: a full disk while saving `tether.json` is an error that leaves the old file and no
/// partial temp file behind.
#[test]
fn full_disk_save_leaves_no_temp_file() {
    let d = std::env::temp_dir().join(format!("lcc-tether-full-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let s = StudioSession::new("A", "/w");
    StudioSession::save(Some(&s), &d).unwrap();
    let before = std::fs::read(d.join(FILE)).unwrap();
    let r = crate::write_atomic(&d.join(FILE), b"{\"name\":\"B\",\"folder\":\"/x\"}", &mut |f, b| {
        std::io::Write::write_all(f, &b[..b.len() / 2])?;
        Err(std::io::Error::new(std::io::ErrorKind::StorageFull, "No space left on device"))
    });
    assert!(r.is_err());
    assert_eq!(std::fs::read(d.join(FILE)).unwrap(), before);
    let names: Vec<_> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names.len(), 1, "{names:?}");
    let _ = std::fs::remove_dir_all(&d);
}
