//! Issue #1303: originals a Lightroom import committed without reading are read in the background.

use std::path::{Path, PathBuf};
use std::time::Duration;

use lcc_catalog::{Op, PhotoId};
use serde_json::json;

use crate::LccApp;

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An app on a library at `dir/lib` whose photos (PNG files) have not had their originals read: no content hash, as
/// a Lightroom import that reads them later leaves them. The last photo's file is gone (an offline drive).
fn app_with_unread_photos(dir: &Path, count: usize) -> (LccApp, Vec<PhotoId>) {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir.join("files")).unwrap();
    let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
    let files: Vec<_> = (0..count)
        .map(|i| {
            let img = lcc_raster::Rgba8::from_fn(16, 12, |x, y| [(x * 9) as u8, (y * 11) as u8, i as u8, 255]);
            let file = dir.join("files").join(format!("photo-{i}.png"));
            std::fs::write(&file, lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();
            file
        })
        .collect();
    let mut session = lcc_engine::Session::new().with_fs();
    session.open_library(dir.join("lib"), false).unwrap();
    let r = session.execute("library.import", &json!({"paths": files.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>()})).unwrap();
    let ids: Vec<_> = r["imported"].as_array().unwrap().iter().map(|id| PhotoId(id.as_u64().unwrap())).collect();
    for id in &ids {
        let p = session.catalog.photo(*id).unwrap().clone();
        session
            .catalog
            .apply(Op::SetContent {
                id: *id,
                width: p.width,
                height: p.height,
                file_size: 0,
                content_hash: None,
                preview_only: None,
                preview_checked: p.preview_checked.clone(),
            })
            .unwrap();
    }
    std::fs::remove_file(files.last().unwrap()).unwrap();
    (LccApp::new(session, Default::default()), ids)
}

#[test]
fn unread_originals_are_read_in_the_background_and_an_offline_one_is_not_retried() {
    let dir = Dir(std::env::temp_dir().join(format!("lc-ui-read-originals-{}", std::process::id())));
    let (mut app, ids) = app_with_unread_photos(&dir.0, 3);
    let ctx = egui::Context::default();
    crate::read_originals::tick(&mut app);
    let rows = app.session.activity.list();
    assert!(rows.iter().any(|r| r.kind == "readOriginals" && r.label == crate::read_originals::LABEL && r.cancellable), "{rows:?}");
    assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(10)));
    assert!(app.session.activity.list().iter().all(|r| r.kind != "readOriginals"), "the row goes with the task");
    for id in &ids[..2] {
        let p = app.session.catalog.photo(*id).unwrap();
        assert!(p.content_hash.is_some() && p.file_size > 0, "photo {id:?} was read");
    }
    assert!(app.session.catalog.photo(ids[2]).unwrap().content_hash.is_none(), "the offline original stays unread");
    // while the library stays open, the offline original isn't tried again every frame
    crate::read_originals::tick(&mut app);
    assert!(app.tasks.is_empty());
    crate::read_originals::wake(&mut app);
    crate::read_originals::tick(&mut app);
    assert!(app.tasks.is_empty(), "an import's wake-up doesn't retry it either");
}

#[test]
fn a_stopped_run_does_not_start_again_by_itself() {
    let dir = Dir(std::env::temp_dir().join(format!("lc-ui-read-originals-stop-{}", std::process::id())));
    let (mut app, _ids) = app_with_unread_photos(&dir.0, 2);
    let ctx = egui::Context::default();
    crate::read_originals::tick(&mut app);
    // the user stops the row
    let row = app.session.activity.list().into_iter().find(|r| r.kind == "readOriginals").expect("a row");
    app.session.activity.cancel(row.id).unwrap();
    assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(10)));
    crate::read_originals::tick(&mut app);
    assert!(app.tasks.is_empty(), "a stopped run doesn't start again by itself");
}
