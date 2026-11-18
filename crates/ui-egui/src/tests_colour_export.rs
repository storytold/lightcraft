//! File ▸ More Exports ▸ Export Camera Profile (.dcp)… and Export Look as LUT (.cube)…: offered for the active photo
//! (the profile only for a raw), and with a path they write files that read back.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(60);

fn temp_dir(tag: &str) -> lcc_testkit::TempDir {
    let d = lcc_testkit::TempDir::new(&format!("lc-ui-colour-export-{tag}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn entry(h: &Headless, id: &str) -> crate::menus::MenuEntry {
    crate::menus::menu_entries(&h.app).into_iter().find(|e| e.id == id).unwrap_or_else(|| panic!("{id} is in the menus"))
}

#[test]
fn the_active_raw_exports_its_camera_profile_and_look() {
    let dir = temp_dir("raw");
    let path = dir.join("IMG_1.dng");
    std::fs::write(&path, lcc_merge::synth::bracket_dngs(320, 240, &[0.0]).unwrap().remove(0)).unwrap();
    let mut session = lcc_engine::Session::new().with_fs();
    let r = session.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    let id = r["imported"][0].as_u64().unwrap();

    let mut h = Headless::new(LccApp::new(session, Services { png: None, ..Default::default() }), [1200.0, 800.0], 1.0);
    h.settle(T);
    h.app.session.selection.active = None;
    for id in ["file.exportCameraProfile", "file.exportLookLut"] {
        let e = entry(&h, id);
        assert_eq!(e.menu.first().map(String::as_str), Some("File"), "{id}: {:?}", e.menu);
        assert!(!e.enabled, "{id}: nothing to export without a photo");
    }
    h.app.session.execute("library.select", &json!({"ids": [id], "active": id})).unwrap();
    assert!(entry(&h, "file.exportCameraProfile").enabled && entry(&h, "file.exportLookLut").enabled);

    let (dcp, cube) = (dir.join("IMG_1.dcp"), dir.join("IMG_1.cube"));
    h.app.run("file.exportCameraProfile", json!({"path": dcp.to_string_lossy()})).unwrap();
    assert!(lcc_raw::dcp::parse(&std::fs::read(&dcp).unwrap()).is_ok());
    assert!(h.app.ui.toast().is_some_and(|t| t.0.contains("camera profile")), "{:?}", h.app.ui.toasts);
    h.app.run("file.exportLookLut", json!({"path": cube.to_string_lossy()})).unwrap();
    assert!(lcc_pipeline::lut::Lut3d::parse_cube(&std::fs::read_to_string(&cube).unwrap()).is_ok());
    // a failed export says why and leaves the original alone
    assert!(h.app.run("file.exportCameraProfile", json!({"path": path.to_string_lossy()})).is_err());
    assert!(lcc_raw::decode(&std::fs::read(&path).unwrap()).is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A rendered photo has a look but no camera colour.
#[test]
fn a_rendered_photo_offers_only_the_look() {
    let mut h = Headless::new(LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() }), [1200.0, 800.0], 1.0);
    h.settle(T);
    let first = h.app.session.catalog.photos().find(|p| !p.develops_raw()).map(|p| p.id).unwrap();
    h.app.session.execute("library.select", &json!({"ids": [first.0], "active": first.0})).unwrap();
    assert!(!entry(&h, "file.exportCameraProfile").enabled);
    assert!(entry(&h, "file.exportLookLut").enabled);
}
