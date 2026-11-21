//! Keyword normalization is the same for import presets and import-dialog keywords.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use lightcraft_engine::Session;
use serde_json::json;

struct Input(PathBuf);

impl Input {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!("lc-import-keywords-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir(&path).unwrap();
        let image = lightcraft_raster::Rgba8 { width: 2, height: 1, data: vec![[20, 40, 60, 255], [80, 100, 120, 255]] };
        let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &lightcraft_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(path.join("photo.png"), bytes).unwrap();
        Self(path)
    }

    fn photo(&self) -> PathBuf {
        self.0.join("photo.png")
    }

    fn sidecar(&self) {
        std::fs::write(
            self.0.join("photo.xmp"),
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:lr="http://ns.adobe.com/lightroom/1.0/"><dc:subject><rdf:Bag><rdf:li>Été</rdf:li><rdf:li>Portrait</rdf:li><rdf:li>Original</rdf:li></rdf:Bag></dc:subject><lr:hierarchicalSubject><rdf:Bag><rdf:li>Été|Portrait</rdf:li><rdf:li>Original</rdf:li></rdf:Bag></lr:hierarchicalSubject></rdf:Description></rdf:RDF></x:xmpmeta>"#,
        )
        .unwrap();
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn keywords(s: &Session) -> Vec<String> {
    assert_eq!(s.catalog.len(), 1);
    s.catalog.photos().next().unwrap().meta.keywords.clone()
}

fn import(s: &mut Session, path: &Path, mut options: serde_json::Value) {
    options["paths"] = json!([path.to_string_lossy()]);
    let report = s.execute("library.import", &options).unwrap();
    assert_eq!(report["imported"].as_array().unwrap().len(), 1, "{report}");
    assert_eq!(report["failed"], json!([]), "{report}");
}

#[test]
fn default_metadata_preset_cleans_keywords_and_ignores_unicode_duplicates() {
    let input = Input::new();
    let mut s = Session::new().with_fs();
    s.execute(
        "metadata.savePreset",
        &json!({"name": "Default", "fields": {"creator": "Studio", "keywords": [
            " || ", " Été | Portrait ", "été|portrait", "Ärger", "ärger", "studio", "STUDIO"
        ]}}),
    )
    .unwrap();
    s.execute("library.preferences", &json!({"import": {"metadataPreset": "Default"}})).unwrap();

    import(&mut s, &input.photo(), json!({}));
    assert_eq!(keywords(&s), ["Été|Portrait", "Ärger", "studio"]);
    assert_eq!(s.catalog.photos().next().unwrap().meta.creator, "Studio");
}

#[test]
fn explicit_preset_and_import_keywords_keep_first_sidecar_spellings_and_order() {
    let input = Input::new();
    input.sidecar();
    let mut s = Session::new().with_fs();
    s.execute("metadata.savePreset", &json!({"name": "Import", "fields": {"keywords": [" été | portrait ", " | ", "Ärger", "ärger"]}})).unwrap();

    import(
        &mut s,
        &input.photo(),
        json!({"metadataPreset": "Import", "keywords": ["ÉTÉ|PORTRAIT", " ÄRGER ", " New | Child ", "new|child", " || "]}),
    );
    let expected = ["Été|Portrait", "Original", "Ärger", "New|Child"];
    assert_eq!(keywords(&s), expected);
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.catalog.is_empty(), "the whole import is one undo step");
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(keywords(&s), expected);
}

#[test]
fn import_keywords_without_a_preset_use_unicode_keyword_identity() {
    let input = Input::new();
    input.sidecar();
    let mut s = Session::new().with_fs();
    import(&mut s, &input.photo(), json!({"keywords": [" été | portrait ", "Ärger", "ärger", " New || Child ", "new|child", " || "]}));
    assert_eq!(keywords(&s), ["Été|Portrait", "Original", "Ärger", "New|Child"]);
}
