//! A library opened from a new place (folder moved or copied; on iOS, the app's container path
//! changes with a reinstall) finds the photos kept inside it again.

use std::path::Path;

use serde_json::json;

use crate::Session;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-moved-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_png(path: &Path, seed: u8) {
    let (w, h) = (40usize, 30usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 6) as u8, (i / w * 8) as u8, seed, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn copy_dir(from: &Path, to: &Path) {
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
            copy_dir(&e.path(), &dest);
        } else if e.file_name() != "catalog.lock" && e.file_name() != "catalog.lock.owner" {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

fn path_of(s: &Session, name: &str) -> String {
    s.catalog
        .photos()
        .find_map(|p| match &p.source {
            lightcraft_catalog::Source::File { path } if path.ends_with(name) => Some(path.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no photo {name}"))
}

#[test]
fn photos_inside_a_moved_library_are_found_again() {
    let root = temp_dir("root");
    let (before, after) = (root.join("old/Container-A/LightCraft Library"), root.join("new/Container-B/LightCraft Library"));
    let outside = root.join("Pictures");
    write_png(&outside.join("in-place.png"), 1);
    write_png(&root.join("card/copied.png"), 2);
    {
        let mut s = Session::new().with_fs();
        s.open_library(&before, false).unwrap();
        // one added in place (outside the library), one copied into it
        s.execute("library.import", &json!({"paths": [outside.join("in-place.png").to_string_lossy()]})).unwrap();
        s.execute("library.import", &json!({"paths": [root.join("card/copied.png").to_string_lossy()], "mode": "copy"})).unwrap();
        assert!(path_of(&s, "copied.png").starts_with(&*before.to_string_lossy()));
        s.close_library().unwrap();
    }
    // the library moves (as an iOS reinstall moves the app's container); the old one is gone
    std::fs::create_dir_all(&after).unwrap();
    copy_dir(&before, &after);
    std::fs::remove_dir_all(root.join("old")).unwrap();

    let copied = {
        let mut s = Session::new().with_fs();
        s.open_library(&after, false).unwrap();
        let copied = path_of(&s, "copied.png");
        assert!(copied.starts_with(&*after.to_string_lossy()), "relinked into the library's new place: {copied}");
        assert!(Path::new(&copied).is_file());
        assert_eq!(path_of(&s, "in-place.png"), outside.join("in-place.png").to_string_lossy(), "a photo outside the library is left alone");
        assert!(crate::cmd::missing::missing(&s).is_empty(), "nothing missing");
        assert!(s.undo.is_empty(), "an automatic repair, not an undo step");
        s.close_library().unwrap();
        copied
    };
    // journaled: the next open finds it already fixed
    let mut s = Session::new().with_fs();
    s.open_library(&after, false).unwrap();
    assert_eq!(path_of(&s, "copied.png"), copied);
    s.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_file_of_another_size_or_a_present_original_is_not_taken() {
    let root = temp_dir("guard");
    let lib = root.join("now/LightCraft Library");
    // a library photo recorded at an old place, whose file is still there
    write_png(&root.join("then/LightCraft Library/Originals/kept.png"), 3);
    write_png(&lib.join("Originals/kept.png"), 4);
    // and one whose file is gone, with a different file at the same place in the new library
    write_png(&root.join("then/LightCraft Library/Originals/other.png"), 5);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute(
        "library.import",
        &json!({"paths": [root.join("then/LightCraft Library/Originals/kept.png").to_string_lossy(), root.join("then/LightCraft Library/Originals/other.png").to_string_lossy()]}),
    )
    .unwrap();
    std::fs::remove_file(root.join("then/LightCraft Library/Originals/other.png")).unwrap();
    std::fs::write(lib.join("Originals/other.png"), b"not the same file at all").unwrap();
    let plan = crate::cmd::missing::plan_moved_library(&s.catalog, &lib);
    assert!(plan.is_empty(), "{plan:?}");
    let _ = std::fs::remove_dir_all(&root);
}
