//! Settings ▸ AI Denoise ▸ AI masks: the button that removes the SAM 3 model shows its size, runs
//! `segment.model.remove` and goes away with the files. No model is downloaded or loaded; the files are stand-ins.

use std::time::Duration;

use lightcraft_engine::segment::Segmenter;
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// The model folder of a test, removed again when the test ends, passed or not.
struct Folder(std::path::PathBuf);

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

#[test]
fn settings_removes_the_mask_model_and_the_button_shows_its_size() {
    let dir = Folder(std::env::temp_dir().join(format!("lc-ui-sam-remove-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).unwrap();
    for name in ["model.safetensors", "vocab.json", "merges.txt"] {
        std::fs::write(dir.0.join(name), vec![0u8; 2_000_000]).unwrap();
    }
    std::fs::write(dir.0.join("notes.txt"), b"mine").unwrap();
    let mut session = lightcraft_engine::Session::with_demo();
    session.segmenter.dir = Some(dir.0.clone());
    let mut h = Headless::new(LightcraftApp::new(session, Services::default()), [1200.0, 800.0], 1.0);
    h.settle(T);
    let open = h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": "denoise"}}), T);
    assert_eq!(open["ok"], true, "{open}");
    h.settle(T);
    for _ in 0..4 {
        h.step();
    }
    if !Segmenter::AVAILABLE {
        // a build without AI masks offers nothing to remove
        assert!(!has(&h, "settings:sam3Remove"));
        return;
    }
    // Given the model's files, the button names their size
    assert!(has(&h, "settings:sam3Remove"), "{:?}", h.app.widgets);
    assert!(h.painted_text().iter().any(|t| t.contains("Remove SAM 3 (6.0 MB)")), "{:?}", h.painted_text());
    // When it is pressed
    let r = h.request("ui.clickWidget", json!({"id": "settings:sam3Remove"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
    for _ in 0..4 {
        h.step();
    }
    // Then the model's files are gone, the others stay, and the button with them
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    assert!(dir.0.join("notes.txt").is_file());
    assert!(!has(&h, "settings:sam3Remove"));
}
