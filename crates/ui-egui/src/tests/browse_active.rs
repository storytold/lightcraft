//! Showing a Local folder picks the active photo the way every other source does (issue #1447):
//! the current one when the folder shows it, else the folder's first photo, so Photo Grid marks
//! it, the info and histogram describe it, and Detail opens on it instead of a photo from
//! another source.

use std::time::Duration;

use lightcraft_catalog::PhotoId;
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::headless::Headless;
use crate::state::ViewMode;

const T: Duration = Duration::from_secs(10);
const SETTLE: Duration = Duration::from_secs(120);

fn demo() -> Headless {
    let services = crate::Services { png: None, ..Default::default() };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    app.ui.view = ViewMode::PhotoGrid;
    Headless::new(app, [1300.0, 900.0], 1.0)
}

fn exec(h: &mut Headless, command: &str, params: Value) -> Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command} {params}: {r}");
    h.settle(SETTLE);
    r
}

/// A folder of `n` small PNGs (different bytes each) in the temporary folder.
fn folder(name: &str, n: u8) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-ui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
    for i in 0..n {
        let img = lightcraft_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 12) as u8, 40 * i, 255]);
        std::fs::write(dir.join(format!("p{i}.png")), lightcraft_engine::export::encode_image(&img, &o).unwrap()).unwrap();
    }
    dir
}

fn active(h: &Headless) -> Option<PhotoId> {
    h.app.session.selection.active
}

#[test]
fn browsing_a_local_folder_makes_its_first_photo_active() {
    let mut h = demo();
    h.settle(SETTLE);
    let dir = folder("browse-active", 3);
    // a library photo is active: the folder doesn't show it
    let before = h.app.session.visible_cloned()[0];
    exec(&mut h, "library.select", json!({"ids": [before.0]}));
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    let vis = h.app.session.visible_cloned();
    assert_eq!(vis.len(), 3);
    assert_eq!(active(&h), Some(vis[0]), "the folder's first photo is active, not a photo the grid doesn't show");
    assert_eq!(h.app.session.selection.ids, vec![vis[0]], "and selected");
    // the grid marks it, and Detail opens on it
    assert!(h.app.widgets.iter().any(|(w, _)| *w == format!("thumb:{}", vis[0].0)));
    h.request("ui.set", json!({"view": "detail"}), T);
    h.settle(SETTLE);
    assert_eq!(active(&h), Some(vis[0]));

    // the folder shows the current photo again: it stays active
    h.request("ui.set", json!({"view": "photoGrid"}), T);
    exec(&mut h, "library.select", json!({"ids": [vis[2].0]}));
    exec(&mut h, "library.source", json!({"kind": "all"}));
    assert_ne!(active(&h), Some(vis[2]), "All Photos doesn't list Local photos");
    exec(&mut h, "library.select", json!({"ids": [vis[2].0]}));
    exec(&mut h, "library.browse", json!({"path": dir.to_string_lossy()}));
    assert_eq!(active(&h), Some(vis[2]), "a photo the folder shows stays active");
    let _ = std::fs::remove_dir_all(&dir);
}

/// An album behaves the same (it already did: kept here as the rule's reference).
#[test]
fn choosing_an_album_keeps_or_picks_the_active_photo() {
    let mut h = demo();
    h.settle(SETTLE);
    let album = h.app.session.catalog.albums().find(|a| !a.folder && !a.is_smart() && a.photos.len() > 1).map(|a| (a.id, a.photos.clone()));
    let (album, photos) = album.expect("the demo library has an album");
    let outside = h.app.session.visible_cloned().into_iter().find(|p| !photos.contains(p)).expect("a photo outside the album");
    exec(&mut h, "library.select", json!({"ids": [outside.0]}));
    exec(&mut h, "library.source", json!({"kind": "album", "id": album.0}));
    let vis = h.app.session.visible_cloned();
    assert_eq!(active(&h), vis.first().copied());
    exec(&mut h, "library.select", json!({"ids": [vis[1].0]}));
    exec(&mut h, "library.source", json!({"kind": "all"}));
    exec(&mut h, "library.source", json!({"kind": "album", "id": album.0}));
    assert_eq!(active(&h), Some(vis[1]));
}
