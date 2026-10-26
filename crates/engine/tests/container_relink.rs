//! A reinstall gives an iOS app a new container folder; photos added in place must follow it, not go offline.

use lightcraft_catalog::Source;
use lightcraft_engine::Session;
use serde_json::json;

fn write_png(path: &std::path::Path, seed: u8) {
    let (w, h) = (48usize, 32usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 5) as u8, (i / w * 7) as u8, seed, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn photos_added_in_place_follow_the_app_container_to_its_new_folder() {
    let root = std::env::temp_dir().join(format!("lc-container-relink-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let apps = root.join("Containers/Data/Application");
    let (old, new) = (apps.join("OLD-UUID"), apps.join("NEW-UUID"));
    write_png(&old.join("Documents/TestPhotos/a.png"), 10);
    write_png(&old.join("Documents/TestPhotos/b.png"), 20);
    write_png(&root.join("elsewhere/c.png"), 30);

    let mut s = Session::new().with_fs();
    s.open_library(root.join("lib"), false).unwrap();
    let paths: Vec<String> = ["a.png", "b.png"].iter().map(|f| old.join("Documents/TestPhotos").join(f).to_string_lossy().into_owned()).collect();
    s.execute("library.import", &json!({"paths": paths})).unwrap();
    s.execute("library.import", &json!({"paths": [root.join("elsewhere/c.png").to_string_lossy()]})).unwrap();
    assert_eq!(s.catalog.photos().count(), 3);

    // nothing moved: nothing to do
    assert_eq!(s.relink_after_container_change(&old).unwrap(), 0);

    // the reinstall: the whole container folder appears under a new name
    std::fs::rename(&old, &new).unwrap();
    let missing =
        |s: &Session| s.catalog.photos().filter(|p| matches!(&p.source, Source::File { path } if !std::path::Path::new(path).exists())).count();
    assert_eq!(missing(&s), 2, "the two in-container photos are offline before the relink");
    assert_eq!(s.relink_after_container_change(&new).unwrap(), 2);
    assert_eq!(missing(&s), 0, "and found again after it");
    assert!(s.catalog.photos().any(|p| matches!(&p.source, Source::File { path } if path.contains("NEW-UUID") && path.ends_with("a.png"))));
    // a second pass has nothing left to do
    assert_eq!(s.relink_after_container_change(&new).unwrap(), 0);
    let _ = std::fs::remove_dir_all(&root);
}
