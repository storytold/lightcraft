//! Local's built-in places are the computer's own folders under a translated label: the menu
//! offers no Rename or Move for them, and a folder the user keeps in Local still renames by its
//! real name.
use egui::{PointerButton, vec2};
use egui_kittest::{Harness, kittest::Queryable};
use lightcraft_ui_egui::{LightcraftApp, Services, i18n::Locale, state::Dialog};
use serde_json::json;
use std::path::{Path, PathBuf};

/// A scratch home with Pictures and a folder kept with Browse Folder…; removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("lightcraft-local-places-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["home/Pictures", "kept/Trips"] {
            std::fs::create_dir_all(dir.join(sub)).expect("scratch folders");
        }
        Self(dir)
    }
    fn path(&self, sub: &str) -> String {
        self.0.join(sub).to_string_lossy().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The app with Local listing `places` (what the host passes in place of the machine's home) and
/// `kept` folders kept with Browse Folder….
fn harness(places: Vec<(String, String)>, kept: Vec<String>) -> Harness<'static, LightcraftApp> {
    let services = Services { local_places: Some(places), pick_folder: Some(Box::new(|| None)), ..Default::default() };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), services);
    app.ui.left_panel = true;
    app.ui.local_roots = kept;
    let mut h = Harness::builder().with_size(vec2(1000.0, 800.0)).build_ui_state(
        |ui, app: &mut LightcraftApp| {
            app.logic(&ui.ctx().clone());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(8);
    h
}

fn open_menu(h: &mut Harness<'_, LightcraftApp>, row: &str) {
    h.get_by_label(row).click_button(PointerButton::Secondary);
    h.run_steps(4);
}

#[test]
fn a_built_in_place_offers_no_rename_or_move() {
    // the label is the translation of "Pictures"; the folder on disk is still called Pictures
    let bilder = Locale::De.tr("Pictures");
    assert_ne!(bilder, "Pictures", "the German label differs from the folder's name");
    let scratch = Scratch::new("builtin");
    let pictures = scratch.path("home/Pictures");
    let places = vec![(bilder.to_string(), pictures.clone()), ("Home".to_string(), scratch.path("home"))];
    let mut h = harness(places, Vec::new());
    for label in [bilder, "Home"] {
        open_menu(&mut h, label);
        assert!(h.query_by_label("Remove from Local").is_some(), "{label}: the row's menu is open");
        assert!(
            h.query_by_label("Rename Folder…").is_none(),
            "{label}: Rename Folder… must not be offered (prompt would start with {:?})",
            h.state().ui.dialog
        );
        assert!(h.query_by_label("Move Folder To…").is_none(), "{label}: Move Folder To… must not be offered");
        h.key_press(egui::Key::Escape);
        h.run_steps(3);
    }
    assert!(Path::new(&pictures).is_dir(), "nothing was renamed on disk");
}

#[test]
fn a_kept_folder_renames_from_its_real_name() {
    let scratch = Scratch::new("kept");
    let kept = scratch.path("kept/Trips");
    let mut h = harness(vec![("Home".to_string(), scratch.path("home"))], vec![kept.clone()]);
    open_menu(&mut h, "Trips");
    h.get_by_label("Rename Folder…").click();
    h.run_steps(4);
    let Some(Dialog::TextPrompt { value, command, params, key, .. }) = h.state().ui.dialog.clone() else {
        panic!("Rename Folder… opens a prompt: {:?}", h.state().ui.dialog)
    };
    assert_eq!((value.as_str(), command.as_str()), ("Trips", "folder.rename"));
    // confirmed as it stands, the prompt renames nothing
    let mut p = params;
    p[key.as_str()] = json!(value);
    assert!(h.state_mut().run(&command, p).is_err(), "the folder already has that name");
    assert!(Path::new(&kept).is_dir());
}
