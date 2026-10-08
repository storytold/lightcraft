//! URI source adapters shared between Android and provider contract tests.
#![forbid(unsafe_code)]
use lightcraft_engine::{Session, merge::ByteReader};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
pub type Names = Arc<Mutex<HashMap<String, String>>>;

pub fn configure(session: &mut Session, read: ByteReader, names: Names) {
    // Bound simultaneous decode/probe allocations independently of Rayon's thread count.
    let gate = Arc::new(Mutex::new(()));
    let (reader, serial) = (read.clone(), gate.clone());
    session.media.file_loader = Some(Arc::new(move |uri, edge| {
        let _lease = serial.lock().map_err(|_| "Source decoder lock failed")?;
        lightcraft_engine::files::load_vec(reader(uri)?, edge)
    }));
    let (reader, serial) = (read.clone(), gate.clone());
    session.media.file_probe = Some(Arc::new(move |uri| {
        let _lease = serial.lock().map_err(|_| "Source decoder lock failed")?;
        let name = names.lock().map_err(|_| "Document index lock failed")?.get(uri).cloned().unwrap_or_else(|| uri.into());
        lightcraft_engine::files::probe_bytes(&name, &reader(uri)?)
    }));
    let reader = read.clone();
    session.media.preview_loader = Some(Arc::new(move |uri, edge| {
        let _lease = gate.lock().ok()?;
        lightcraft_engine::files::embedded_preview_srgb(&reader(uri).ok()?, edge)
    }));
    session.media.file_bytes = Some(read);
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_engine::{
        export::{ExportOptions, encode_image},
        import::{ImportJob, ImportOptions, commit_prepared},
    };
    use serde_json::json;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn uri_photo_edits_render_persist_and_export_without_changing_original() {
        let mut image = lightcraft_raster::Rgba8::new(96, 64);
        for (i, p) in image.data.iter_mut().enumerate() {
            *p = [40 + (i % 96) as u8, 80, 90, 255];
        }
        let original = Arc::new(encode_image(&image, &ExportOptions::default()).unwrap());
        let before_bytes = (*original).clone();
        let uri = "content://test.documents/tree/photos/document/opaque%3A42";
        let names: Names = Arc::new(Mutex::new(HashMap::from([(uri.into(), "camera.jpg".into())])));
        let reader: ByteReader = Arc::new(move |key| {
            assert_eq!(key, uri);
            Ok((*original).clone())
        });
        let dir = std::env::temp_dir().join(format!("lightcraft-android-contract-{}", std::process::id()));
        let mut s = Session::new().with_system_clock();
        s.open_library(&dir, false).unwrap();
        configure(&mut s, reader.clone(), names.clone());
        let opts = ImportOptions::default();
        let mut job = ImportJob::new(&mut s, opts.clone()).unwrap();
        let now = job.now().to_owned();
        let prepared = job.prepare_files(vec![uri.into()], &AtomicBool::new(false));
        let report = commit_prepared(&mut s, &opts, &now, prepared).unwrap();
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        let id = report.imported[0];
        s.execute("library.select", &json!({"ids":[id], "active":id})).unwrap();
        let before = s.render_now(s.active().unwrap(), 96, 64).unwrap().image;
        s.execute("develop.set", &json!({"control":"light.exposure", "value":1.0})).unwrap();
        let after = s.render_now(s.active().unwrap(), 96, 64).unwrap().image;
        assert_ne!(before.data, after.data, "exposure must change pixels");
        s.execute("develop.set", &json!({"control":"color.vibrance", "value":30})).unwrap();
        s.execute("crop.aspect", &json!({"aspect":"1x1"})).unwrap();
        s.execute("mask.add", &json!({"kind":"radial", "center":[0.5,0.5]})).unwrap();
        s.execute("mask.adjust", &json!({"values":{"exposure":0.3}})).unwrap();
        let settings = s.develop_of(s.active().unwrap()).unwrap().clone();
        s.close_library().unwrap();
        drop(s);
        let mut reopened = Session::new();
        reopened.open_library(&dir, false).unwrap();
        configure(&mut reopened, reader.clone(), names);
        assert_eq!(reopened.develop_of(lightcraft_catalog::PhotoId(id)).unwrap(), settings);
        let mut job = ImportJob::new(&mut reopened, opts.clone()).unwrap();
        let now = job.now().to_owned();
        let prepared = job.prepare_files(vec![uri.into()], &AtomicBool::new(false));
        let repeated = commit_prepared(&mut reopened, &opts, &now, prepared).unwrap();
        assert!(repeated.imported.is_empty(), "reopening the same URI must not duplicate it");
        let rendered = reopened.render_now(lightcraft_catalog::PhotoId(id), 96, 96).unwrap();
        assert_eq!(rendered.image.width, rendered.image.height);
        assert!(!encode_image(&rendered.image, &ExportOptions::default()).unwrap().is_empty());
        assert_eq!(reader(uri).unwrap(), before_bytes);
        reopened.close_library().unwrap();
        drop(reopened);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
