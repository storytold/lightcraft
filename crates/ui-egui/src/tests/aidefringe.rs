//! Headless tests of AI Defringe's controls: the section at the top of Optics ▸ Defringe and Settings ▸ AI Defringe
//! (docs/ai-defringe.md). The model is a synthetic network that asks for the same change everywhere.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-ui-adf-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A synthetic defringe model and its manifest → the model file.
fn model_file(dir: &Path) -> String {
    let bytes = lightcraft_denoise::synthetic::defringe_onnx([-3.0, 3.0, 10.0]);
    let sha = lightcraft_denoise::hash::sha256_hex(&bytes);
    std::fs::write(dir.join("push.onnx"), &bytes).unwrap();
    let m = json!({"id": "push", "name": "Test push", "version": "2", "licence": "MIT", "file": "push.onnx", "sha256": sha, "contract": "lightcraftDefringe1", "notice": "Trained on nothing at all."});
    std::fs::write(dir.join("defringe-model.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    dir.join("push.onnx").to_string_lossy().into_owned()
}

/// Detail view of the demo photo with Optics ▸ Defringe open, its model folder in `dir`.
fn defringe_open(dir: &Path) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let mut session = lightcraft_engine::Session::with_demo();
    session.set_aidefringe_models_dir(Some(dir.join("defringe-models")));
    let app = LightcraftApp::new(session, services);
    let mut h = Headless::new(app, [1200.0, 1600.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.right = crate::state::RightPanel::Edit;
    if !h.app.ui.section_open("optics") {
        h.app.ui.toggle_section("optics");
    }
    if !h.app.ui.flyout_open("defringe") {
        h.app.ui.toggle_flyout("defringe");
    }
    h.step();
    h.step();
    h
}

fn on_screen(h: &mut Headless, id: &str) -> bool {
    let r = h.request("ui.widgets", json!({"filter": "aidefringe"}), T);
    r["result"].as_array().is_some_and(|ws| ws.iter().any(|w| w["id"] == id))
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
    h.step();
}

#[test]
fn without_a_model_the_section_points_to_settings() {
    let dir = temp("none");
    let mut h = defringe_open(&dir);
    assert!(!on_screen(&mut h, "aidefringe:enabled"));
    assert!(on_screen(&mut h, "aidefringe:settings"));
    click(&mut h, "aidefringe:settings");
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "aidefringe".into() }));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_model_is_installed_from_settings_after_its_terms_are_shown() {
    let dir = temp("install");
    let mut h = defringe_open(&dir);
    let picked = model_file(&dir);
    h.app.services.pick_denoise_model = Some(Box::new(move || vec![picked.clone()]));
    h.app.ui.dialog = Some(Dialog::Settings { tab: "aidefringe".into() });
    h.step();
    h.step();
    click(&mut h, "aidefringe:installFile");
    let painted = h.painted_text();
    assert!(painted.iter().any(|s| s == "Trained on nothing at all."), "the notice is shown: {painted:?}");
    assert!(h.app.session.execute("aiDefringe.status", &json!({})).unwrap()["model"].is_null(), "nothing installed before accepting");
    click(&mut h, "aidefringe:accept");
    assert_eq!(h.app.session.execute("aiDefringe.status", &json!({})).unwrap()["model"]["name"], "Test push");
    click(&mut h, "aidefringe:remove");
    assert!(h.app.session.execute("aiDefringe.status", &json!({})).unwrap()["model"].is_null());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_switch_turns_it_on_and_the_correction_arrives() {
    let dir = temp("switch");
    let mut h = defringe_open(&dir);
    let path = model_file(&dir);
    h.app.session.execute("aiDefringe.models.install", &json!({"path": path, "acknowledged": true})).unwrap();
    h.app.caches.aidefringe = Default::default();
    h.step();
    h.step();
    assert!(on_screen(&mut h, "aidefringe:enabled"));
    click(&mut h, "aidefringe:enabled");
    let id = h.app.session.active().unwrap();
    let d = h.app.session.develop_of(id).unwrap();
    assert!(d.optics.ai_defringe_enabled());
    assert_eq!(d.optics.ai_defringe, 100.0);
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        h.step();
        let s = h.app.session.execute("aiDefringe.status", &json!({})).unwrap();
        match s["photo"]["state"].as_str() {
            Some("ready") => break,
            Some("failed") => panic!("{s}"),
            _ => {}
        }
        assert!(std::time::Instant::now() < deadline, "the correction never arrived");
        std::thread::sleep(Duration::from_millis(20));
    }
    // switched off, the Amount stays
    click(&mut h, "aidefringe:enabled");
    let d = h.app.session.develop_of(id).unwrap();
    assert!(!d.optics.ai_defringe_enabled());
    assert_eq!(d.optics.ai_defringe, 100.0);
    let _ = std::fs::remove_dir_all(&dir);
}
