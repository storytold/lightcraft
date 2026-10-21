//! Import, source refresh and disposable general metadata search through public commands.

use lightcraft_catalog::{Filter, Op, PhotoId, Sort};
use serde_json::json;

use crate::Session;

fn png(prompt: Option<&str>, workflow: Option<&str>) -> Vec<u8> {
    let fields: Vec<_> = prompt.map(|v| ("prompt", v)).into_iter().chain(workflow.map(|v| ("workflow", v))).collect();
    text_png(&fields)
}

fn text_png(fields: &[(&str, &str)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut bytes, 2, 1);
        enc.set_color(png::ColorType::Rgb);
        for (key, value) in fields {
            enc.add_text_chunk((*key).into(), (*value).into()).unwrap();
        }
        let mut writer = enc.write_header().unwrap();
        writer.write_image_data(&[10, 20, 30, 40, 50, 60]).unwrap();
    }
    bytes
}

#[test]
fn general_cache_is_disposable_and_never_changes_catalog_format_or_originals() {
    let dir = crate::tests_xmp::temp_dir("general-metadata-cache");
    let path = dir.join("ordinary.png");
    let library = dir.join("library");
    let original = text_png(&[("parameters", "Cat sitting on a chair, stained glass church"), ("Software", "External Tagger")]);
    std::fs::write(&path, &original).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    s.execute("library.import", &json!({"paths": [path]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    for text in ["cat chair", "CHURCH", "meta:cat meta:chair", "meta:software=external", "meta:\"parameters=stained glass\"", "cat rating:0"] {
        assert_eq!(query(&s, text), vec![id], "{text}");
    }
    assert!(s.catalog.photo(id).unwrap().meta.keywords.is_empty());
    assert!(query(&s, "keyword:cat").is_empty());
    s.close_library().unwrap();
    drop(s);
    let snapshot = std::fs::read(library.join("catalog.snap")).unwrap();
    let snapshot_text = String::from_utf8_lossy(&snapshot);
    assert!(snapshot_text.contains("\"version\":9"));
    assert!(!snapshot_text.contains("metadata_search"));
    assert!(!snapshot_text.contains("stained glass"));
    assert!(!snapshot_text.contains("\"comfy\""));
    let cache = library.join(crate::metadata_cache::FILE);
    assert!(cache.is_file());
    // Missing, malformed and unknown-version caches all remain safe to delete and rebuild.
    for broken in [None, Some("{broken cache\n"), Some("{\"version\":999}\n")] {
        if let Some(broken) = broken {
            std::fs::write(&cache, broken).unwrap();
        } else {
            std::fs::remove_file(&cache).unwrap();
        }
        let mut s = Session::new().with_fs();
        s.open_library(&library, false).unwrap();
        assert!(query(&s, "stained glass").is_empty());
        s.execute("photo.reload", &json!({"ids": [id.0]})).unwrap();
        assert_eq!(query(&s, "cat chair").len(), 1);
        assert_eq!(std::fs::read(library.join("catalog.snap")).unwrap(), snapshot);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(!path.with_extension("xmp").exists());
    }
    // Searches and reopen work with offline originals; no probe is needed.
    std::fs::remove_file(&path).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    s.media.file_probe = Some(std::sync::Arc::new(|_| Err("search must never probe".into())));
    assert_eq!(query(&s, "cat chair"), vec![id]);
    drop(s);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cache_write_failure_does_not_block_catalog_edits_and_retries() {
    let dir = crate::tests_xmp::temp_dir("metadata-cache-write-failure");
    let path = dir.join("image.png");
    let library = dir.join("library");
    std::fs::write(&path, text_png(&[("Comment", "Cat and chair")])).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    let cache = library.join(crate::metadata_cache::FILE);
    // A directory at the cache name makes atomic replacement fail on every supported OS.
    std::fs::create_dir(&cache).unwrap();
    s.execute("library.import", &json!({"paths": [path]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    s.execute("photo.rate", &json!({"ids": [id.0], "rating": 5})).unwrap();
    assert_eq!(s.unsaved(), None);
    assert_eq!(query(&s, "cat chair rating:5"), vec![id]);
    std::fs::remove_dir(&cache).unwrap();
    s.persist().unwrap();
    assert!(cache.is_file());
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    assert_eq!(query(&s, "cat chair rating:5"), vec![id]);
    drop(s);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cache_append_recovers_after_a_torn_tail_and_switching_libraries_does_not_leak_entries() {
    let dir = crate::tests_xmp::temp_dir("metadata-cache-torn-tail");
    let path = dir.join("image.png");
    let library = dir.join("library");
    std::fs::write(&path, text_png(&[("Comment", "Cat and chair")])).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    s.execute("library.import", &json!({"paths": [path]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    drop(s);
    let cache = library.join(crate::metadata_cache::FILE);
    use std::io::Write;
    std::fs::OpenOptions::new().append(true).open(&cache).unwrap().write_all(b"{\"id\":").unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    assert_eq!(query(&s, "cat chair"), vec![id]);
    let second = dir.join("second.png");
    std::fs::write(&second, text_png(&[("Comment", "Dog and sofa")])).unwrap();
    s.execute("library.import", &json!({"paths": [second]})).unwrap();
    assert_eq!(query(&s, "dog sofa").len(), 1);
    s.open_library(dir.join("other"), false).unwrap();
    assert!(query(&s, "cat chair").is_empty());
    s.open_library(&library, false).unwrap();
    assert_eq!(query(&s, "cat chair"), vec![id]);
    assert_eq!(query(&s, "dog sofa").len(), 1);
    drop(s);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn source_refresh_and_relink_replace_cached_values_without_overwriting_user_edits() {
    let dir = crate::tests_xmp::temp_dir("general-metadata-replace");
    let path = dir.join("image.png");
    let library = dir.join("library");
    std::fs::write(&path, text_png(&[("parameters", "Cat on chair")])).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    s.execute("library.import", &json!({"paths": [path]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    s.execute("photo.setMeta", &json!({"ids": [id.0], "title": "My own title"})).unwrap();
    drop(s);
    std::fs::write(&path, text_png(&[("parameters", "Dog on sofa")])).unwrap();
    let file = std::fs::File::options().write(true).open(&path).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3))).unwrap();
    drop(file);
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    assert_eq!(query(&s, "cat chair"), vec![id], "external changes wait for explicit refresh");
    assert_eq!(query(&s, "my own title"), vec![id]);
    s.execute("photo.reload", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(query(&s, "dog sofa"), vec![id]);
    assert!(query(&s, "cat").is_empty());
    assert_eq!(s.catalog.photo(id).unwrap().meta.title, "My own title");
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    assert_eq!(query(&s, "dog sofa"), vec![id]);
    let replacement = dir.join("replacement.png");
    std::fs::write(&replacement, text_png(&[("Comment", "Forest lake")])).unwrap();
    s.execute("photo.relink", &json!({"id": id.0, "path": replacement})).unwrap();
    assert_eq!(query(&s, "forest lake"), vec![id]);
    assert!(query(&s, "dog sofa").is_empty());
    assert_eq!(s.catalog.photo(id).unwrap().meta.title, "My own title");
    drop(s);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn custom_xmp_tags_search_without_keywords_and_refresh_after_external_or_automatic_writes() {
    let dir = crate::tests_xmp::temp_dir("general-metadata-sidecar");
    let path = dir.join("image.png");
    let sidecar = path.with_extension("xmp");
    let library = dir.join("library");
    let packet = r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:ai="https://example.org/ai/">
<ai:tags><rdf:Bag><rdf:li>Cat</rdf:li><rdf:li>Chair</rdf:li></rdf:Bag></ai:tags></rdf:Description></rdf:RDF>"#;
    std::fs::write(&path, text_png(&[("parameters", "Moonlit forest")])).unwrap();
    std::fs::write(&sidecar, packet).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    s.execute("library.import", &json!({"paths": [path]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    assert_eq!(query(&s, "cat chair forest"), vec![id]);
    assert!(s.catalog.photo(id).unwrap().meta.keywords.is_empty());
    drop(s);
    std::fs::write(&sidecar, packet.replace("Chair", "Sofa")).unwrap();
    let file = std::fs::File::options().write(true).open(&sidecar).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3))).unwrap();
    drop(file);
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    assert_eq!(query(&s, "chair"), vec![id], "external sidecar edits wait for explicit refresh");
    assert_eq!(query(&s, "forest"), vec![id]);
    s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(query(&s, "cat sofa"), vec![id]);
    assert!(query(&s, "chair").is_empty());
    assert!(query(&s, "keyword:cat").is_empty());
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    s.execute("photo.setMeta", &json!({"ids": [id.0], "title": "My title"})).unwrap();
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    assert_eq!(query(&s, "cat sofa forest"), vec![id]);
    assert!(s.catalog.photo(id).unwrap().meta.keywords.is_empty());
    drop(s);
    std::fs::remove_dir_all(dir).unwrap();
}

fn query(s: &Session, text: &str) -> Vec<PhotoId> {
    s.catalog.query(&Filter { text: text.into(), ..Default::default() }, &Sort::default())
}

#[test]
fn sidecar_saves_refresh_virtual_copy_search_and_persist_without_overwriting_copy_edits() {
    let dir = crate::tests_xmp::temp_dir("metadata-copy-sidecar");
    let path = dir.join("image.png");
    let sidecar = path.with_extension("xmp");
    let library = dir.join("library");
    let original = text_png(&[("parameters", "Moonlit forest")]);
    let packet = r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:ai="https://example.org/ai/">
<ai:tags><rdf:Bag><rdf:li>Cat</rdf:li><rdf:li>Chair</rdf:li></rdf:Bag></ai:tags></rdf:Description></rdf:RDF>"#;
    std::fs::write(&path, &original).unwrap();
    std::fs::write(&sidecar, packet).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    s.execute("library.import", &json!({"paths": [path]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    let made = s.execute("photo.virtualCopy", &json!({"ids": [id.0]})).unwrap();
    let copy = PhotoId(made["ids"][0].as_u64().unwrap());
    s.execute("photo.setMeta", &json!({"ids": [copy.0], "title": "Copy title", "keywords": ["Personal"]})).unwrap();
    assert_eq!(query(&s, "meta:tags=chair").len(), 2);

    for (auto, tag, stale) in [(false, "Sofa", "Chair"), (true, "Table", "Sofa")] {
        std::fs::write(&sidecar, packet.replace("Chair", tag)).unwrap();
        if auto {
            s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
            s.execute("photo.setMeta", &json!({"ids": [id.0], "title": "Original title"})).unwrap();
        } else {
            let saved = s.execute("photo.saveMetadataToFile", &json!({"ids": [id.0]})).unwrap();
            assert_eq!(saved["failed"], json!([]));
        }
        assert_eq!(query(&s, &format!("meta:tags={tag} forest")).len(), 2);
        assert!(query(&s, &format!("meta:tags={stale}")).is_empty());
        assert_eq!(s.catalog.photo(copy).unwrap().meta.title, "Copy title");
        assert_eq!(s.catalog.photo(copy).unwrap().meta.keywords, ["Personal"]);
    }
    s.persist().unwrap();
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    assert_eq!(query(&s, "meta:tags=table forest").len(), 2);
    assert!(query(&s, "meta:tags=chair").is_empty());
    assert_eq!(s.catalog.photo(copy).unwrap().meta.title, "Copy title");
    assert_eq!(s.catalog.photo(copy).unwrap().meta.keywords, ["Personal"]);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    drop(s);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn external_xmp_keywords_import_refresh_and_search_with_generation_metadata() {
    let dir = crate::tests_xmp::temp_dir("comfy-external-keywords");
    let file = dir.join("generated.png");
    let sidecar = file.with_extension("xmp");
    let library = dir.join("library");
    let original = png(Some(include_str!("../../meta/tests/fixtures/comfy-api.json")), None);
    std::fs::write(&file, &original).unwrap();
    let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:subject><rdf:Bag><rdf:li>cat</rdf:li><rdf:li>chair</rdf:li><rdf:li>living room</rdf:li></rdf:Bag></dc:subject>
</rdf:Description></rdf:RDF></x:xmpmeta>"#;
    std::fs::write(&sidecar, xmp).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    s.execute("library.import", &json!({"paths": [file]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    for text in ["cat chair", "keyword:cat keyword:chair", "CAT CHAIR", "comfy:true model:flux \"living room\""] {
        assert_eq!(query(&s, text), vec![id], "{text}");
    }

    let updated = xmp.replace("chair", "sofa");
    std::fs::write(&sidecar, &updated).unwrap();
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    let result = s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(result["failed"], json!([]));
    assert_eq!(result["read"].as_array().unwrap().len(), 1);
    assert!(query(&s, "cat chair").is_empty());
    assert_eq!(query(&s, "comfy:true model:flux keyword:cat keyword:sofa"), vec![id]);
    assert_eq!(std::fs::read(&file).unwrap(), original);
    assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), updated);
    s.persist().unwrap();
    drop(s);
    let mut reopened = Session::new().with_fs();
    reopened.open_library(&library, false).unwrap();
    assert_eq!(query(&reopened, "comfy:true cat sofa"), vec![id]);
    assert!(query(&reopened, "keyword:chair").is_empty());
    drop(reopened);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn all_metadata_includes_raw_png_generation_fields_and_readable_summary() {
    let dir = crate::tests_xmp::temp_dir("comfy-viewer");
    let file = dir.join("generated.png");
    let bytes =
        png(Some(include_str!("../../meta/tests/fixtures/comfy-api.json")), Some(include_str!("../../meta/tests/fixtures/comfy-workflow.json")));
    std::fs::write(&file, &bytes).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths":[file.to_string_lossy()]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    let rows = s.execute("photo.allMetadata", &json!({"id":id.0})).unwrap();
    assert!(rows["exif"].as_array().unwrap().is_empty());
    assert_eq!(rows["pngText"].as_array().unwrap().len(), 2);
    assert!(rows["pngText"].as_array().unwrap().iter().any(|r| r["name"] == "workflow"));
    assert!(rows["generation"].as_array().unwrap().iter().any(|r| r["name"] == "node" && r["value"].as_str().unwrap().contains("MyCustomNode")));
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
}

#[test]
fn comfy_import_reindex_reopen_and_replace_sources() {
    let dir = crate::tests_xmp::temp_dir("comfy-lifecycle");
    let path = dir.join("generated.png");
    let library = dir.join("library");
    let original = png(Some(include_str!("../../meta/tests/fixtures/comfy-api.json")), None);
    std::fs::write(&path, &original).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    let imported = s.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    assert_eq!(imported["imported"].as_array().unwrap().len(), 1, "{imported}");
    let id = s.catalog.photos().next().unwrap().id;
    assert_eq!(query(&s, "prompt:\"neon city\" model:flux lora:detail seed:18446744073709551614"), vec![id]);
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.execute("photo.setMeta", &json!({"title": "Keep my title", "keywords": ["portfolio"]})).unwrap();
    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.5})).unwrap();

    // A persisted record from before generation metadata was extracted, with unchanged source bytes.
    s.persist().unwrap();
    let hash = s.catalog.photo(id).unwrap().content_hash.clone();
    drop(s);
    std::fs::remove_file(library.join(crate::metadata_cache::FILE)).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    assert!(query(&s, "comfy:true").is_empty());
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    let r = s.execute("photo.reload", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(r["reloaded"], json!([id.0]));
    assert_eq!(r["failed"], json!([]));
    assert_eq!(s.catalog.photo(id).unwrap().content_hash, hash);
    let p = s.catalog.photo(id).unwrap();
    assert_eq!(p.meta.title, "Keep my title");
    assert_eq!(p.rating, 4);
    assert_eq!(p.develop.light.exposure, 0.5);
    assert_eq!(query(&s, "comfy:true portfolio"), vec![id]);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(!path.with_extension("xmp").exists(), "reindex never auto-writes sidecars");
    let r = s.execute("photo.reload", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(r["reloaded"], json!([]), "unchanged sources and index are not reported twice");
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    assert_eq!(query(&s, "comfy:true cfg:>5"), vec![id]);

    std::fs::remove_file(&path).unwrap();
    assert_eq!(query(&s, "sampler:euler"), vec![id], "offline search only reads catalog data");
    let r = s.execute("photo.reload", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(r["failed"].as_array().unwrap().len(), 1);
    assert_eq!(query(&s, "comfy:true"), vec![id], "unreadable sources keep their previous metadata");

    let updated = png(None, Some(include_str!("../../meta/tests/fixtures/comfy-workflow.json")));
    std::fs::write(&path, &updated).unwrap();
    s.execute("photo.reload", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(query(&s, "comfy:true steps:28 seed:123456"), vec![id]);
    assert!(query(&s, "seed:18446744073709551614").is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), updated);

    std::fs::write(&path, png(None, None)).unwrap();
    s.execute("photo.reload", &json!({"ids": [id.0]})).unwrap();
    assert!(query(&s, "comfy:true").is_empty());
    assert_eq!(query(&s, "comfy:false portfolio"), vec![id]);
    assert_eq!(s.catalog.photo(id).unwrap().meta.title, "Keep my title");
    assert!(!path.with_extension("xmp").exists());
    drop(s);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn comfy_reload_updates_virtual_copies_and_is_undoable() {
    let mut s = Session::new();
    let id = s.catalog.alloc_photo_id();
    let mut p =
        lightcraft_catalog::Photo::new(id, lightcraft_catalog::Source::File { path: "/generated.png".into() }, "old.png", "PNG", 2, 1, "2026-10-18");
    p.meta.title = "Original".into();
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p.clone()) }).unwrap();
    let copy = s.catalog.alloc_photo_id();
    p.id = copy;
    p.copy_of = Some(id);
    p.meta.title = "Copy's title".into();
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let info = crate::files::probe_bytes("generated.png", &png(Some(include_str!("../../meta/tests/fixtures/comfy-api.json")), None)).unwrap();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = calls.clone();
    s.media.file_probe = Some(std::sync::Arc::new(move |_| {
        counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(info.clone())
    }));
    s.execute("library.select", &json!({"ids": [copy.0, id.0]})).unwrap();
    let r = s.execute("photo.reload", &json!({"ids": [copy.0, id.0]})).unwrap();
    assert_eq!(r["reloaded"], json!([id.0, copy.0]));
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1, "a shared source is probed once");
    assert_eq!(query(&s, "comfy:true").len(), 2);
    assert_eq!(s.catalog.photo(copy).unwrap().meta.title, "Copy's title");
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(query(&s, "comfy:true").is_empty());
}

#[test]
fn comfy_uses_existing_library_filter_and_catalog_query_commands() {
    let mut s = Session::new();
    let info = crate::files::probe_bytes("generated.png", &png(Some(include_str!("../../meta/tests/fixtures/comfy-api.json")), None)).unwrap();
    let id = s.catalog.alloc_photo_id();
    let mut p = lightcraft_catalog::Photo::new(id, lightcraft_catalog::Source::Demo { scene: 0 }, "generated.png", "PNG", 2, 1, "2026-10-18");
    p.meta = info.meta;
    p.metadata_search = info.metadata_search;
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let text = "comfy:true prompt:\"NEON CITY\" model:flux steps:>20 cfg:>5";
    s.execute("library.filter", &json!({"text": text})).unwrap();
    assert_eq!(s.visible_cloned(), vec![id]);
    let r = s.execute("catalog.query", &json!({})).unwrap();
    assert_eq!(r["total"], 1);
    let r = s.execute("catalog.query", &json!({"filter": {"text": "seed:18446744073709551614"}})).unwrap();
    assert_eq!(r["total"], 1);
    s.execute("library.filter", &json!({"text": "prompt:missing"})).unwrap();
    assert!(s.visible_cloned().is_empty());
}

#[test]
fn comfy_reindexes_a_large_existing_library_in_one_batch() {
    const COUNT: usize = 20_000;
    let mut s = Session::new();
    let info = crate::files::probe_bytes("generated.png", &png(Some(include_str!("../../meta/tests/fixtures/comfy-api.json")), None)).unwrap();
    let mut ids = Vec::new();
    for i in 0..COUNT {
        let id = s.catalog.alloc_photo_id();
        let p = lightcraft_catalog::Photo::new(
            id,
            lightcraft_catalog::Source::File { path: format!("/images/{i}.png") },
            "generated.png",
            "PNG",
            2,
            1,
            "2026-10-18",
        );
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        ids.push(id);
    }
    s.media.file_probe = Some(std::sync::Arc::new(move |_| Ok(info.clone())));
    s.selection = crate::Selection { active: ids.first().copied(), ids };
    let started = std::time::Instant::now();
    let r = s.execute("photo.reload", &json!({})).unwrap();
    assert_eq!(r["reloaded"].as_array().unwrap().len(), COUNT);
    assert_eq!(r["failed"], json!([]));
    assert_eq!(query(&s, "comfy:true model:flux prompt:\"neon city\"").len(), COUNT);
    eprintln!("ComfyUI catalog update and query over 20,000 existing records (probes supplied): {:?}", started.elapsed());
}
