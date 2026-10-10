//! Native tethering through the commands, against the simulated PTP camera. The connected
//! camera is process-wide, so everything runs in one test.

use serde_json::json;

use crate::Session;

#[test]
fn simulated_camera_captures_downloads_and_imports() {
    let root = std::env::temp_dir().join(format!("dac-tether-native-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let mut s = Session::new().with_fs();

    // no library on disk: refused before touching a camera
    assert!(s.execute("tether.connect", &json!({"device": "sim"})).is_err());
    s.open_library(root.join("lib"), false).unwrap();

    // an unknown device says to fall back to studio capture
    let e = s.execute("tether.connect", &json!({"device": "nonsense"})).unwrap_err().to_string();
    assert!(e.contains("studio capture"), "{e}");
    assert!(s.execute("tether.capture", &json!({})).is_err());

    let cam = s.execute("tether.connect", &json!({"device": "sim", "session": "Shoot", "sameAsPrevious": true})).unwrap();
    assert_eq!(cam["name"], "Simulated PTP Camera");
    assert_eq!(cam["canCapture"], true);
    assert_eq!(cam["readouts"][0]["label"], "1/250");
    let ses = s.execute("tether.status", &json!({})).unwrap();
    assert_eq!(ses["name"], "Shoot");
    assert!(ses["folder"].as_str().unwrap().contains("Tethered"));

    // settings
    let st = s.execute("tether.camera.set", &json!({"setting": "aperture", "value": "f/8"})).unwrap();
    assert_eq!(st["readouts"][1]["label"], "f/8");
    assert!(s.execute("tether.camera.set", &json!({"setting": "iso", "value": "ISO 7"})).is_err());

    // capture from the app: downloaded, imported, selected
    let r = s.execute("tether.capture", &json!({})).unwrap();
    assert_eq!(r["files"].as_array().unwrap().len(), 1, "{r}");
    let first = dac_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
    assert_eq!(s.selection.active, Some(first));
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.5})).unwrap();

    // the camera's own shutter: picked up by the scans, same develop settings as the previous shot
    s.execute("tether.simShoot", &json!({})).unwrap();
    let mut imported = Vec::new();
    for _ in 0..3 {
        let r = s.execute("tether.scan", &json!({})).unwrap();
        imported.extend(r["imported"].as_array().unwrap().iter().filter_map(|v| v.as_u64()));
    }
    assert_eq!(imported.len(), 1);
    let second = dac_catalog::PhotoId(imported[0]);
    assert!((s.develop_of(second).unwrap().light.exposure - 0.5).abs() < 1e-6);

    // delete from card
    s.execute("tether.camera.set", &json!({"deleteFromCard": true})).unwrap();
    s.execute("tether.capture", &json!({})).unwrap();
    assert_eq!(s.execute("tether.camera", &json!({})).unwrap()["downloaded"], 3);

    // live view: not on a generic camera, yes on the Nikon-like one
    assert!(s.execute("tether.liveView", &json!({})).is_err());
    s.execute("tether.connect", &json!({"device": "sim-nikon"})).unwrap();
    let lv = s.execute("tether.liveView", &json!({})).unwrap();
    assert_eq!((lv["width"].as_u64(), lv["height"].as_u64()), (Some(96), Some(64)));

    s.execute("tether.disconnect", &json!({})).unwrap();
    assert!(s.execute("tether.camera", &json!({})).unwrap().is_null());
    let _ = std::fs::remove_dir_all(&root);
}
