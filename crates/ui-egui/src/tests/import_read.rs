//! What an import reads from the photos themselves, in the app: chosen for the library in
//! Settings ▸ Import ▸ Read from photos, and for one import in the Import Photos review, which
//! opens with the library's choice.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-ui-importread-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A photo with a sidecar that rates it four stars and gives it a keyword.
fn a_rated_photo_with_a_keyword(dir: &Path, name: &str, seed: u8) {
    let (w, h) = (48usize, 32usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 5) as u8, (i / w * 7) as u8, seed, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(dir.join(format!("{name}.png")), bytes).unwrap();
    std::fs::write(
        dir.join(format!("{name}.xmp")),
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmp:Rating="4">
 <dc:subject><rdf:Bag><rdf:li>fjord</rdf:li></rdf:Bag></dc:subject>
</rdf:Description></rdf:RDF></x:xmpmeta>"#,
    )
    .unwrap();
}

fn app() -> Headless {
    let session = lightcraft_engine::Session::new().with_fs();
    let mut h = Headless::new(LightcraftApp::new(session, Services::default()), [1400.0, 1000.0], 1.0);
    h.step();
    h
}

fn click(h: &mut Headless, id: &str) {
    h.step();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    for _ in 0..4 {
        h.step();
    }
}

fn read(h: &mut Headless) -> Value {
    h.app.run("library.preferences", json!({})).unwrap()["import"]["read"].clone()
}

/// The photo named `name` in the library: its rating and keywords.
fn imported(h: &Headless, name: &str) -> (u8, Vec<String>) {
    let p = h.app.session.catalog.photos().find(|p| p.file_name.starts_with(name)).unwrap_or_else(|| panic!("{name} was imported"));
    (p.rating, p.meta.keywords.clone())
}

/// Open the import review for a folder, and wait for it.
fn review(h: &mut Headless, dir: &Path) {
    h.app.run("file.addPhotos", json!({"paths": [dir.to_string_lossy()]})).unwrap();
    assert!(h.step_until(T, |h| matches!(h.app.ui.dialog, Some(Dialog::Import { .. }))), "the review opens");
    h.step();
}

fn import_what_is_reviewed(h: &mut Headless, photos: usize) {
    click(h, "button:dialogOk");
    assert!(h.step_until(T, |h| h.app.session.catalog.len() == photos && h.app.import.is_none()), "{} photos", h.app.session.catalog.len());
}

/// Given Settings ▸ Import, then everything is read from photos until chosen otherwise; when
/// Keywords is unticked there, the library's imports leave keywords in the files, and ticking
/// it again reads them again. The other kinds stay as they were.
#[test]
fn what_imports_read_from_photos_is_chosen_in_settings() {
    let mut h = app();
    assert_eq!(read(&mut h), json!({"keywords": true, "ratings": true, "descriptions": true, "location": true, "develop": true}));
    assert_eq!(h.request("ui.key", json!({"key": ",", "cmd": true}), T)["ok"], true);
    click(&mut h, "button:settingsTab-import");
    let r = h.request("ui.widgets", json!({"filter": "check:settings.read."}), T);
    assert_eq!(r["result"].as_array().map(Vec::len), Some(5), "one for each kind: {r}");
    click(&mut h, "check:settings.read.keywords");
    assert_eq!(read(&mut h), json!({"keywords": false, "ratings": true, "descriptions": true, "location": true, "develop": true}));
    click(&mut h, "check:settings.read.location");
    click(&mut h, "check:settings.read.keywords");
    assert_eq!(read(&mut h), json!({"keywords": true, "ratings": true, "descriptions": true, "location": false, "develop": true}));
}

/// Given a library that does not read keywords from photos, when a folder is imported through
/// the review as it opens, then its photos come in rated and without their keywords. When, in
/// the review of a second folder, Keywords is ticked and Ratings and labels unticked for once,
/// then those photos have their keywords and no rating, and the library's own choice is as it was.
#[test]
fn the_import_review_opens_with_the_librarys_choice_and_can_differ_for_once() {
    let (first, second) = (temp("first"), temp("second"));
    a_rated_photo_with_a_keyword(&first, "one", 1);
    a_rated_photo_with_a_keyword(&second, "two", 2);
    let mut h = app();
    h.app.run("library.preferences", json!({"import": {"read": {"keywords": false}}})).unwrap();

    review(&mut h, &first);
    let Some(Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("the review") };
    assert!(!opts.read.keywords && opts.read.ratings, "as the library has it: {:?}", opts.read);
    let r = h.request("ui.widgets", json!({"filter": "check:importRead."}), T);
    assert_eq!(r["result"].as_array().map(Vec::len), Some(5), "{r}");
    import_what_is_reviewed(&mut h, 1);
    assert_eq!(imported(&h, "one"), (4, Vec::new()));

    review(&mut h, &second);
    click(&mut h, "check:importRead.keywords");
    click(&mut h, "check:importRead.ratings");
    import_what_is_reviewed(&mut h, 2);
    assert_eq!(imported(&h, "two"), (0, vec!["fjord".to_string()]));
    assert_eq!(read(&mut h)["keywords"], json!(false), "for that import only");
    assert_eq!(imported(&h, "one"), (4, Vec::new()), "and what was imported before is as it was");
    for d in [first, second] {
        let _ = std::fs::remove_dir_all(d);
    }
}
