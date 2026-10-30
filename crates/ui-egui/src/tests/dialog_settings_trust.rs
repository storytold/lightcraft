//! Settings does not do what cannot be taken back, or takes long to make again, on one click.
//!
//! Before: Clear Cache and a model's Remove acted at once, and deleting the smart previews already
//! built was a choice made beforehand, which a later press of Choose Folder then carried out
//! without a word.

use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// A folder under the system's temporary one, removed again when the test ends, passed or not.
struct TempFolder(std::path::PathBuf);

impl TempFolder {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("lc-ui-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, rest: &str) -> String {
        self.0.join(rest).to_string_lossy().into_owned()
    }
}

impl Drop for TempFolder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn headless(session: lightcraft_engine::Session, services: Services) -> Headless {
    let mut h = Headless::new(LightcraftApp::new(session, services), [1200.0, 900.0], 1.0);
    // let the first renders finish (a render still running at exit can crash in GPU teardown)
    h.settle(T);
    h
}

fn demo() -> Headless {
    headless(lightcraft_engine::Session::with_demo(), Services::default())
}

fn show(h: &mut Headless, tab: &str) {
    let r = h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": tab}}), T);
    assert_eq!(r["ok"], true, "app.settings {tab}: {r}");
    h.settle(T);
    for _ in 0..4 {
        h.step();
    }
}

fn steps(h: &mut Headless) {
    for _ in 0..4 {
        h.step();
    }
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "click {id}: {r}");
    steps(h);
}

fn key(h: &mut Headless, key: &str) {
    assert_eq!(h.request("ui.key", json!({"key": key}), T)["ok"], true, "{key}");
    steps(h);
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn says(h: &Headless, part: &str) -> bool {
    h.painted_text().iter().any(|text| text.contains(part))
}

fn settings_open(h: &Headless, tab: &str) -> bool {
    h.app.ui.dialog == Some(Dialog::Settings { tab: tab.into() })
}

#[test]
fn clear_cache_asks_first() {
    let mut h = demo();
    show(&mut h, "performance");
    let used = |h: &Headless| h.app.session.media.rendered.mem_usage().0;
    let before = used(&h);
    assert!(before > 0, "the demo's thumbnails are cached");
    // When Clear Cache is pressed
    click(&mut h, "button:settingsClearCache");
    // Then it says what clearing costs and waits for an answer; nothing is cleared yet
    assert!(says(&h, "made again"), "it says what clearing costs: {:?}", h.painted_text().iter().filter(|t| t.contains("again")).collect::<Vec<_>>());
    assert!(has(&h, "button:settingsClearCache:confirm") && has(&h, "button:settingsClearCache:keep"));
    assert_eq!(used(&h), before, "nothing is cleared before the answer");
    // When the answer is to keep it
    click(&mut h, "button:settingsClearCache:keep");
    // Then the question goes and the cache is as it was
    assert!(!has(&h, "button:settingsClearCache:confirm"));
    assert_eq!(used(&h), before);
    // When it is pressed again and confirmed, the cache is cleared
    click(&mut h, "button:settingsClearCache");
    let r = h.request("ui.clickWidget", json!({"id": "button:settingsClearCache:confirm"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(used(&h) < before, "the cache was cleared: {} of {before}", used(&h));
    steps(&mut h);
    assert!(!has(&h, "button:settingsClearCache:confirm"), "and the question is gone");
}

#[test]
fn esc_takes_the_question_away_and_leaves_settings_open() {
    let mut h = demo();
    show(&mut h, "performance");
    let before = h.app.session.media.rendered.mem_usage().0;
    click(&mut h, "button:settingsClearCache");
    assert!(has(&h, "button:settingsClearCache:confirm"));
    key(&mut h, "Escape");
    assert!(!has(&h, "button:settingsClearCache:confirm"), "Esc answers no");
    assert!(settings_open(&h, "performance"), "and Settings stays open");
    assert_eq!(h.app.session.media.rendered.mem_usage().0, before);
}

#[test]
fn removing_a_face_model_asks_first() {
    let dir = TempFolder::new("settings-trust-faces");
    let mut h = demo();
    h.app.session.face_models_dir = Some(dir.0.join("models"));
    if h.app.session.execute("faces.models.list", &json!({})).unwrap()["runtime"] != true {
        lightcraft_testkit::skip!(Model, "this build cannot run recognition models: there is nothing to install or remove");
        return;
    }
    // Given a recognition model installed
    let model = dir.0.join("Mine.onnx");
    std::fs::write(&model, lightcraft_faces::synthetic::tiny_embedder_model(512)).unwrap();
    let installed = h.app.run("faces.models.install", json!({"path": model.to_string_lossy(), "acknowledged": true, "activate": false})).unwrap();
    let id = installed["installed"]["id"].as_str().unwrap().to_string();
    h.app.caches.faces_epoch += 1;
    let is_installed = |h: &mut Headless| h.app.run("faces.models.list", json!({})).unwrap().to_string().contains(&id);
    show(&mut h, "faces");
    let remove = format!("faces:remove:{id}");
    // When Remove is pressed
    click(&mut h, &remove);
    // Then it asks, and the model is still there
    assert!(has(&h, &format!("{remove}:confirm")), "Remove asks first");
    assert!(is_installed(&mut h), "the model is still installed");
    // When the answer is yes, it is removed
    click(&mut h, &format!("{remove}:confirm"));
    assert!(!is_installed(&mut h), "the model is removed");
    assert!(!has(&h, &remove), "and its Remove button is gone with it");
}

/// Given a model whose Remove was answered yes (so the row stops drawing the button), when the same
/// model is installed again, then its new Remove button does not take the focus the question left
/// behind: Space does nothing until the person tabs to it.
#[test]
fn a_model_installed_again_after_remove_does_not_take_the_focus() {
    let dir = TempFolder::new("settings-trust-faces-again");
    let mut h = demo();
    h.app.session.face_models_dir = Some(dir.0.join("models"));
    if h.app.session.execute("faces.models.list", &json!({})).unwrap()["runtime"] != true {
        lightcraft_testkit::skip!(Model, "this build cannot run recognition models: there is nothing to install or remove");
        return;
    }
    let model = dir.0.join("Mine.onnx");
    std::fs::write(&model, lightcraft_faces::synthetic::tiny_embedder_model(512)).unwrap();
    let install = |h: &mut Headless| {
        let r = h.app.run("faces.models.install", json!({"path": model.to_string_lossy(), "acknowledged": true, "activate": false})).unwrap();
        h.app.caches.faces_epoch += 1;
        r["installed"]["id"].as_str().unwrap().to_string()
    };
    let id = install(&mut h);
    show(&mut h, "faces");
    let remove = format!("faces:remove:{id}");
    click(&mut h, &remove);
    click(&mut h, &format!("{remove}:confirm"));
    assert!(!has(&h, &remove), "the model is removed and its Remove button is gone");
    // some frames pass with no Remove button
    steps(&mut h);
    steps(&mut h);
    // When it is installed again
    assert_eq!(install(&mut h), id);
    steps(&mut h);
    assert!(has(&h, &remove), "its Remove button is back");
    // Then nothing has the focus: Space does not open the question
    key(&mut h, "Space");
    assert!(!has(&h, &format!("{remove}:confirm")), "the new Remove button took the focus the old question left behind");
}

fn write_png(path: &str, seed: u8) {
    let (w, h) = (24usize, 16usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 9) as u8, (i / w * 13) as u8, seed, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A library on disk with one photo and its smart preview built, whose folder picker answers `to`.
fn with_smart_preview(dir: &TempFolder, to: &str) -> Headless {
    write_png(&dir.path("trip/a.png"), 3);
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(dir.0.join("library"), false).unwrap();
    session.execute("library.import", &json!({"paths": [dir.path("trip/a.png")]})).unwrap();
    session.execute("library.smartPreviews", &json!({})).unwrap();
    std::fs::create_dir_all(to).unwrap();
    let to = to.to_string();
    headless(session, Services { pick_folder: Some(Box::new(move || Some(to.clone()))), ..Default::default() })
}

fn smart(h: &mut Headless) -> Value {
    h.app.run("library.smartPreviewsLocation", json!({})).unwrap()
}

#[test]
fn deleting_the_smart_previews_is_asked_about_when_the_folder_is_chosen() {
    let dir = TempFolder::new("settings-trust-smart");
    let to = dir.path("elsewhere");
    let mut h = with_smart_preview(&dir, &to);
    assert_eq!(smart(&mut h)["count"], 1);
    show(&mut h, "performance");
    // Given "Delete them" chosen for the previews already built
    click(&mut h, "button:settingsSmartExisting-2");
    // When a new folder is chosen
    click(&mut h, "button:settingsSmartChoose");
    // Then nothing has happened yet: it asks, saying how many previews would go
    let now = smart(&mut h);
    assert_eq!((now["count"].clone(), now["custom"].clone()), (json!(1), json!(false)), "nothing is deleted or moved before the answer: {now}");
    assert!(has(&h, "button:settingsSmartDelete") && has(&h, "button:settingsSmartCancel"), "it asks");
    let question: Vec<String> = h.painted_text().into_iter().filter(|text| text.contains("Delete the smart previews")).collect();
    assert!(question.iter().any(|text| text.contains("1 smart preview")), "it asks, saying how many would go: {question:?}");
    // When the answer is no
    click(&mut h, "button:settingsSmartCancel");
    // Then the folder and its previews are as they were, and the question is gone
    let now = smart(&mut h);
    assert_eq!((now["count"].clone(), now["custom"].clone()), (json!(1), json!(false)));
    assert!(!has(&h, "button:settingsSmartDelete"));
    // And the choice is back at "Move them": deleting is not left armed for next time
    click(&mut h, "button:settingsSmartChoose");
    let moved = smart(&mut h);
    assert_eq!((moved["count"].clone(), moved["custom"].clone()), (json!(1), json!(true)), "the previews moved with the folder: {moved}");
}

#[test]
fn saying_yes_deletes_the_smart_previews_and_changes_the_folder() {
    let dir = TempFolder::new("settings-trust-smart-yes");
    let to = dir.path("elsewhere");
    let mut h = with_smart_preview(&dir, &to);
    show(&mut h, "performance");
    click(&mut h, "button:settingsSmartExisting-2");
    click(&mut h, "button:settingsSmartChoose");
    assert_eq!(smart(&mut h)["count"], 1);
    click(&mut h, "button:settingsSmartDelete");
    let now = smart(&mut h);
    assert_eq!((now["count"].clone(), now["custom"].clone()), (json!(0), json!(true)), "{now}");
    assert_eq!(now["path"], json!(to));
    assert!(!has(&h, "button:settingsSmartDelete"));
}

#[test]
fn moving_the_smart_previews_needs_no_question() {
    let dir = TempFolder::new("settings-trust-smart-move");
    let to = dir.path("elsewhere");
    let mut h = with_smart_preview(&dir, &to);
    show(&mut h, "performance");
    click(&mut h, "button:settingsSmartChoose");
    let now = smart(&mut h);
    assert_eq!((now["count"].clone(), now["custom"].clone()), (json!(1), json!(true)), "{now}");
    assert!(!has(&h, "button:settingsSmartDelete"));
}

#[test]
fn writing_xmp_by_itself_says_what_is_written_where() {
    let mut h = demo();
    show(&mut h, "import");
    assert!(says(&h, "beside the photo"), "{:?}", h.painted_text().iter().filter(|t| t.contains("XMP")).collect::<Vec<_>>());
    // not every change is written (face names, a write that failed and waits to be retried): it doesn't promise so
    assert!(!says(&h, "every change"), "{:?}", h.painted_text().iter().filter(|t| t.contains("XMP")).collect::<Vec<_>>());
    assert!(says(&h, "when possible"), "{:?}", h.painted_text().iter().filter(|t| t.contains("XMP")).collect::<Vec<_>>());
}

/// A second library on disk with a smart preview of its own, in a folder chosen for it.
fn another_library(dir: &TempFolder) -> (String, String) {
    write_png(&dir.path("other/b.png"), 9);
    let (library, folder) = (dir.path("library-b"), dir.path("previews-b"));
    std::fs::create_dir_all(&folder).unwrap();
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(&library, false).unwrap();
    session.execute("library.import", &json!({"paths": [dir.path("other/b.png")]})).unwrap();
    session.execute("library.smartPreviewsLocation", &json!({"path": folder, "existing": "leave"})).unwrap();
    session.execute("library.smartPreviews", &json!({})).unwrap();
    session.close_library().unwrap();
    (library, folder)
}

#[test]
fn a_question_left_unanswered_is_gone_when_settings_is_opened_again() {
    let dir = TempFolder::new("settings-trust-smart-left");
    let to = dir.path("elsewhere");
    let mut h = with_smart_preview(&dir, &to);
    show(&mut h, "performance");
    // Given the question about deleting, left unanswered
    click(&mut h, "button:settingsSmartExisting-2");
    click(&mut h, "button:settingsSmartChoose");
    assert!(has(&h, "button:settingsSmartDelete"));
    // When Settings is closed and opened again
    key(&mut h, "Escape");
    assert!(h.app.ui.dialog.is_none());
    show(&mut h, "performance");
    // Then nothing waits: no question, and the choice is back at "Move them"
    assert!(!has(&h, "button:settingsSmartDelete"), "the question does not outlive Settings");
    click(&mut h, "button:settingsSmartChoose");
    let now = smart(&mut h);
    assert_eq!((now["count"].clone(), now["custom"].clone()), (json!(1), json!(true)), "the previews moved: {now}");
}

#[test]
fn a_question_asked_in_one_library_is_not_there_in_another() {
    let dir = TempFolder::new("settings-trust-smart-other");
    let (library_b, folder_b) = another_library(&dir);
    let to = dir.path("elsewhere");
    let mut h = with_smart_preview(&dir, &to);
    show(&mut h, "performance");
    // Given the question about deleting asked in one library, and left
    click(&mut h, "button:settingsSmartExisting-2");
    click(&mut h, "button:settingsSmartChoose");
    assert!(has(&h, "button:settingsSmartDelete"));
    key(&mut h, "Escape");
    // When another library is opened and its Settings looked at
    h.app.run("app.openLibrary", json!({"path": library_b})).unwrap();
    h.settle(T);
    show(&mut h, "performance");
    // Then that library is not asked anything, and its smart previews are where they were
    assert!(!has(&h, "button:settingsSmartDelete"), "the question belongs to the library it was asked in");
    let now = smart(&mut h);
    assert_eq!((now["count"].clone(), now["path"].clone()), (json!(1), json!(folder_b)), "{now}");
}

#[test]
fn a_change_made_another_way_takes_the_question_away() {
    let dir = TempFolder::new("settings-trust-smart-other-way");
    let to = dir.path("elsewhere");
    let mut h = with_smart_preview(&dir, &to);
    show(&mut h, "performance");
    click(&mut h, "button:settingsSmartExisting-2");
    click(&mut h, "button:settingsSmartChoose");
    assert!(has(&h, "button:settingsSmartDelete"));
    // When "Move them" is chosen instead and the folder chosen again
    click(&mut h, "button:settingsSmartExisting-0");
    assert!(!has(&h, "button:settingsSmartDelete"), "choosing not to delete takes the question away");
    click(&mut h, "button:settingsSmartChoose");
    // Then the previews moved, and no question about deleting them is left standing
    let now = smart(&mut h);
    assert_eq!((now["count"].clone(), now["custom"].clone()), (json!(1), json!(true)), "{now}");
    assert!(!has(&h, "button:settingsSmartDelete"));
}

#[test]
fn use_library_folder_asks_before_deleting_too() {
    let dir = TempFolder::new("settings-trust-smart-reset");
    let to = dir.path("elsewhere");
    let mut h = with_smart_preview(&dir, &to);
    show(&mut h, "performance");
    // Given the previews moved to a chosen folder, then "Delete them" chosen
    click(&mut h, "button:settingsSmartChoose");
    assert_eq!(smart(&mut h)["custom"], true);
    h.settle(T);
    steps(&mut h);
    click(&mut h, "button:settingsSmartExisting-2");
    // When Use Library Folder is pressed, it asks first
    click(&mut h, "button:settingsSmartReset");
    let now = smart(&mut h);
    assert_eq!((now["count"].clone(), now["custom"].clone()), (json!(1), json!(true)), "{now}");
    assert!(has(&h, "button:settingsSmartDelete"));
}

#[test]
fn the_question_opens_with_the_focus_on_keep() {
    let mut h = demo();
    show(&mut h, "performance");
    let before = h.app.session.media.rendered.mem_usage().0;
    click(&mut h, "button:settingsClearCache");
    assert!(has(&h, "button:settingsClearCache:confirm"));
    // When Return is pressed straight away
    key(&mut h, "Enter");
    // Then the answer was Keep: the question is gone and nothing was cleared
    assert!(!has(&h, "button:settingsClearCache:confirm"));
    assert_eq!(h.app.session.media.rendered.mem_usage().0, before);
    assert!(settings_open(&h, "performance"));
}

#[test]
fn a_question_about_a_folder_that_has_changed_since_deletes_nothing() {
    let dir = TempFolder::new("settings-trust-smart-outside");
    let to = dir.path("elsewhere");
    let mut h = with_smart_preview(&dir, &to);
    show(&mut h, "performance");
    click(&mut h, "button:settingsSmartExisting-2");
    click(&mut h, "button:settingsSmartChoose");
    assert!(has(&h, "button:settingsSmartDelete"));
    // Given the folder changed from outside Settings (an agent, say) while the question stands
    let other = dir.path("another");
    std::fs::create_dir_all(&other).unwrap();
    h.app.run("library.smartPreviewsLocation", json!({"path": other, "existing": "move"})).unwrap();
    // When Delete and Change Folder is pressed all the same
    let r = h.request("ui.clickWidget", json!({"id": "button:settingsSmartDelete"}), T);
    assert_eq!(r["ok"], true, "{r}");
    steps(&mut h);
    // Then nothing is deleted and the folder is the one set from outside: the question was about another
    let now = smart(&mut h);
    assert_eq!((now["count"].clone(), now["path"].clone()), (json!(1), json!(other)), "{now}");
    assert!(!has(&h, "button:settingsSmartDelete"));
}

#[test]
fn after_keep_the_focus_is_back_on_the_button() {
    let mut h = demo();
    show(&mut h, "performance");
    click(&mut h, "button:settingsClearCache");
    key(&mut h, "Enter"); // Keep, which has the focus
    assert!(!has(&h, "button:settingsClearCache:confirm"));
    // When Space is pressed again, the question opens again: the focus was on Clear Cache
    key(&mut h, "Space");
    assert!(has(&h, "button:settingsClearCache:confirm"), "the focus went back to Clear Cache");
}

fn focus_is_on_clear_cache(h: &mut Headless) -> bool {
    // When Space is pressed, the focused button is pressed: the question opens again
    key(h, "Space");
    has(h, "button:settingsClearCache:confirm")
}

#[test]
fn after_esc_the_focus_is_back_on_the_button() {
    let mut h = demo();
    show(&mut h, "performance");
    click(&mut h, "button:settingsClearCache");
    key(&mut h, "Escape");
    assert!(!has(&h, "button:settingsClearCache:confirm"));
    assert!(focus_is_on_clear_cache(&mut h), "the focus went back to Clear Cache after Esc");
}

#[test]
fn after_yes_the_focus_is_back_on_the_button() {
    let mut h = demo();
    show(&mut h, "performance");
    click(&mut h, "button:settingsClearCache");
    click(&mut h, "button:settingsClearCache:confirm");
    assert!(!has(&h, "button:settingsClearCache:confirm"));
    assert!(focus_is_on_clear_cache(&mut h), "the focus went back to Clear Cache after yes");
}

#[test]
fn after_yes_by_keyboard_the_focus_is_back_on_the_button() {
    let mut h = demo();
    show(&mut h, "performance");
    click(&mut h, "button:settingsClearCache");
    // When the focus is moved from Keep to the action (Shift+Tab) and Return is pressed
    assert_eq!(h.request("ui.key", json!({"key": "Tab", "shift": true}), T)["ok"], true);
    steps(&mut h);
    key(&mut h, "Enter");
    // Then the question is gone and the focus is back on Clear Cache
    assert!(!has(&h, "button:settingsClearCache:confirm"), "the question closed");
    assert!(focus_is_on_clear_cache(&mut h), "the focus went back to Clear Cache after a keyboard yes");
}

#[test]
fn after_a_click_elsewhere_the_focus_is_back_on_the_button() {
    let mut h = demo();
    show(&mut h, "performance");
    click(&mut h, "button:settingsClearCache");
    let rect = h.app.widgets.iter().find(|(w, _)| w == "button:settingsClearCache").unwrap().1;
    // When a click lands on empty space in the window, to the right of the button
    h.request("ui.click", json!({"x": rect.right() + 120.0, "y": rect.center().y}), T);
    steps(&mut h);
    assert!(!has(&h, "button:settingsClearCache:confirm"), "a click elsewhere answers no");
    assert!(focus_is_on_clear_cache(&mut h), "the focus went back to Clear Cache");
}

#[test]
fn removing_a_denoise_model_asks_first() {
    let dir = TempFolder::new("settings-trust-denoise");
    let mut h = demo();
    h.app.session.set_denoise_models_dir(Some(dir.0.join("denoise")));
    // Given a model installed
    // (laid down as an install leaves it, so the test does not need a build that can run models)
    let source = dir.0.join("denoise/test-smoothing");
    std::fs::create_dir_all(&source).unwrap();
    let bytes = lightcraft_denoise::synthetic::smoothing_onnx();
    std::fs::write(source.join("model.onnx"), &bytes).unwrap();
    let manifest = lightcraft_denoise::manifest::DenoiserManifest {
        id: "test-smoothing".into(),
        name: "Synthetic smoothing".into(),
        version: "1".into(),
        licence: Default::default(),
        source: None,
        sha256: None,
        size_bytes: Some(bytes.len() as u64),
        provenance: "Synthetic tests".into(),
        domain: lightcraft_denoise::manifest::Domain::BayerToRgb,
        tile: 64,
        overlap: 16,
        gain: lightcraft_denoise::manifest::Gain::None,
    };
    std::fs::write(source.join("denoise-model.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
    h.app.caches.denoise.epoch += 1;
    let installed = |h: &mut Headless| h.app.run("denoise.models.list", json!({})).unwrap().to_string().contains("test-smoothing");
    assert!(installed(&mut h));
    show(&mut h, "denoise");
    let remove = "denoise:remove:test-smoothing";
    // When Remove is pressed, it asks, and the model is still there
    click(&mut h, remove);
    assert!(has(&h, &format!("{remove}:confirm")), "Remove asks first");
    assert!(installed(&mut h), "the model is still installed");
    // When the answer is yes, it is removed
    click(&mut h, &format!("{remove}:confirm"));
    assert!(!installed(&mut h), "the model is removed");
}

/// A quit prompt that comes up over Settings, and goes away again, leaves the choice for the
/// smart previews already built as it was: the frame that opens a prompt draws the prompt alone
/// first, and that must not look like Settings having been closed and opened again.
#[test]
fn a_prompt_over_settings_keeps_the_choice_for_the_smart_previews() {
    let dir = TempFolder::new("settings-trust-smart-prompt");
    let to = dir.path("elsewhere");
    let mut h = with_smart_preview(&dir, &to);
    show(&mut h, "performance");
    click(&mut h, "button:settingsSmartExisting-2");
    h.app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
    steps(&mut h);
    assert!(has(&h, "button:quitCancel"), "the prompt is up");
    h.app.quit_prompt = None;
    steps(&mut h);
    click(&mut h, "button:settingsSmartChoose");
    assert!(has(&h, "button:settingsSmartDelete"), "\"Delete them\" is still chosen, so it asks first");
    assert_eq!(smart(&mut h)["count"], 1, "nothing was moved or deleted");
}
