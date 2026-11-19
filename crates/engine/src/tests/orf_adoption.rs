//! Shared ORF adoption at the app boundary: import, develop, offline proxies and damaged-sensor fallback.
//! Sensor known answers and the 23-file reference manifest live with craft-raw in craft-libs#2356.

use std::path::{Path, PathBuf};

use lightcraft_catalog::PhotoId;
use serde_json::json;

use crate::Session;
use crate::export::{ExportFormat, ExportOptions, Resize, export_photo};
use crate::media::SourceLevel;

fn corpus(name: &str) -> Option<PathBuf> {
    let root = std::env::var_os("LIGHTCRAFT_CORPUS").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
    let file = root.join("raw").join(name);
    if !file.is_file() {
        lightcraft_testkit::skip!(Corpus, "{} absent; cargo xtask corpus --download", file.display());
        return None;
    }
    Some(file)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-orf-app-{name}-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    dir
}

#[test]
fn shared_orfs_import_render_and_keep_sensor_facts_while_offline() {
    for name in ["orf-olympus-em1.orf", "orf-olympus-em10iii.orf"] {
        let Some(original) = corpus(name) else { continue };
        let dir = scratch(name);
        let file = dir.join(name);
        std::fs::copy(original, &file).unwrap();
        let mut s = Session::new().with_fs();
        s.media.smart_dir = Some(dir.join("proxies"));
        let imported = s.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
        assert!(imported["failed"].as_array().unwrap().is_empty(), "{imported}");
        let id = PhotoId(imported["imported"][0].as_u64().unwrap());
        let photo = s.catalog.photo(id).unwrap().clone();
        assert_eq!(photo.format, "ORF");
        assert!(photo.develops_raw() && photo.preview_only.is_none());
        let source = s.media.source_ref(&photo, SourceLevel::Thumb).load_source().unwrap();
        assert!(source.info.unwrap().raw && !source.from_proxy);
        assert!(source.image.data.iter().flatten().all(|v| v.is_finite()));
        let (low, high) = source.image.data.iter().flatten().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
        assert!(high - low > 0.01, "the app produced image detail");
        s.execute("develop.set", &json!({"id": id.0, "control": "light.exposure", "value": 0.4})).unwrap();
        let edited = s.catalog.photo(id).unwrap().clone();
        let options = ExportOptions { format: ExportFormat::Png, resize: Some(Resize::long_edge(320)), ..Default::default() };
        let online = export_photo(&mut s, id, &options, 1).unwrap();
        assert_eq!(online.width.max(online.height), 320);
        assert!(lightcraft_codecs::decode(&online.bytes, Default::default()).is_ok());
        assert_eq!(s.execute("library.smartPreviews", &json!({"id": id.0})).unwrap()["built"], 1);
        std::fs::remove_file(&file).unwrap();
        s.media.forget(id);
        let source = s.media.source_ref(&edited, SourceLevel::Preview).load_source().unwrap();
        assert!(source.from_proxy && source.info.unwrap().raw);
        let offline = export_photo(&mut s, id, &options, 1).unwrap();
        assert_eq!((offline.width, offline.height), (online.width, online.height));
        assert!(lightcraft_codecs::decode(&offline.bytes, Default::default()).is_ok());
        assert_eq!(s.catalog.photo(id).unwrap().develop, edited.develop);
        drop(s);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn a_damaged_orf_sensor_still_imports_and_renders_its_embedded_preview() {
    let Some(original) = corpus("orf-olympus-em1.orf") else { return };
    let mut bytes = std::fs::read(original).unwrap();
    let tiff = craft_tiff::Tiff::parse(&bytes).unwrap();
    let width = tiff.ifds.first().unwrap().get(craft_tiff::tags::IMAGE_WIDTH).unwrap();
    assert_eq!(width.count(), 1);
    let at = usize::try_from(width.offset).unwrap();
    // Damage only the raw geometry; the camera's preview and its references remain intact.
    bytes[at..at + width.field_type().size()].fill(0);
    let dir = scratch("damaged-sensor");
    let file = dir.join("damaged.orf");
    std::fs::write(&file, bytes).unwrap();
    let mut s = Session::new().with_fs();
    let imported = s.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
    assert!(imported["failed"].as_array().unwrap().is_empty(), "{imported}");
    let id = PhotoId(imported["imported"][0].as_u64().unwrap());
    let photo = s.catalog.photo(id).unwrap().clone();
    assert_eq!(photo.format, "ORF");
    assert!(photo.preview_only.is_some() && !photo.develops_raw());
    let source = s.media.source_ref(&photo, SourceLevel::Thumb).load_source().unwrap();
    assert!(!source.info.unwrap().raw, "camera JPEG pixels retain rendered-image semantics");
    let rendered =
        export_photo(&mut s, id, &ExportOptions { format: ExportFormat::Png, resize: Some(Resize::long_edge(320)), ..Default::default() }, 1)
            .unwrap();
    assert_eq!(rendered.width.max(rendered.height), 320);
    assert!(lightcraft_codecs::decode(&rendered.bytes, Default::default()).is_ok());
    drop(s);
    std::fs::remove_dir_all(dir).unwrap();
}
