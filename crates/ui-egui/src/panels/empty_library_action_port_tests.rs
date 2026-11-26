//! Empty-library actions: real pointer/keyboard against actual empty_state and its owner.
//! All fixtures install cancel-only fake picker services before app creation/drawing/measurement.
//! No OS dialog, filesystem-backed photo, import worker, capture, or forced keyboard focus.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy)]
enum Mode {
    Returns,
    Owner,
    Disabled,
    Measure,
    Sizing,
    NoActionGrid,
}

struct Fixture {
    app: LightcraftApp,
    mode: Mode,
    returned: Vec<&'static str>,
    asked: Arc<Mutex<Vec<crate::pick::PickRequest>>>,
    _busy: Option<Box<dyn std::any::Any>>,
}

fn fixture(width: f32, mode: Mode, busy: bool) -> Harness<'static, Fixture> {
    let asked = Arc::new(Mutex::new(Vec::<crate::pick::PickRequest>::new()));
    let observed = Arc::clone(&asked);
    // Install every relevant fake before even constructing the app, not after a first pass.
    let services = crate::Services {
        picker: Some(Box::new(move |request| {
            observed.lock().unwrap().push(request);
            let (tx, rx) = std::sync::mpsc::channel();
            tx.send(Vec::new()).unwrap();
            Ok(rx)
        })),
        pick_files: Some(Box::new(Vec::new)),
        pick_lightroom_catalog: Some(Box::new(|| None)),
        png: None,
        ..Default::default()
    };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), services);
    app.ui.view = crate::state::ViewMode::PhotoGrid;
    if matches!(mode, Mode::NoActionGrid) {
        app.session.source = lightcraft_engine::LibrarySource::RecentlyDeleted;
    }
    #[cfg(not(target_arch = "wasm32"))]
    let blocked = busy.then(|| crate::lightroom_import::stall_import(&mut app));
    #[cfg(target_arch = "wasm32")]
    let blocked = {
        assert!(!busy);
        None
    };
    let state = Fixture { app, mode, returned: Vec::new(), asked, _busy: blocked };
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    crate::theme::apply(&ctx);
    state.app.install_framework(&ctx);
    let mut h = Harness::builder().with_context(ctx).with_size(vec2(width, 650.0)).build_ui_state(
        |ui, state: &mut Fixture| {
            crate::widgets::take_registries(ui.ctx());
            // Poll only the in-memory cancellation channels. The fixture never yields paths.
            crate::pick::poll(&mut state.app, ui.ctx());
            match state.mode {
                Mode::Owner => empty_library::show(&mut state.app, ui, ui.max_rect()),
                Mode::NoActionGrid => grid::show(&mut state.app, ui),
                Mode::Measure => craft::ui::runtime::measure::run(&ui.ctx().clone(), || draw_return(ui, state)),
                Mode::Sizing => {
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                    draw_return(&mut child, state);
                }
                Mode::Disabled => {
                    ui.add_enabled_ui(false, |ui| draw_return(ui, state));
                }
                Mode::Returns => draw_return(ui, state),
            }
            (state.app.widgets, state.app.widget_hits) = crate::widgets::take_registries(ui.ctx());
        },
        state,
    );
    h.run_steps(5);
    h
}

fn draw_return(ui: &mut egui::Ui, state: &mut Fixture) {
    let actions = [
        EmptyAction { command: "file.addPhotos", label: "Import Photos…", widget: "button:emptyImportPhotos" },
        EmptyAction { command: "file.importLightroom", label: "Import Lightroom Catalog…", widget: "button:emptyImportLightroom" },
    ];
    if let Some(command) = empty_state(ui, ui.max_rect(), "Your library is empty", "Import photos to get started", &actions, None) {
        state.returned.push(command);
    }
}

fn rect(h: &Harness<'_, Fixture>, id: &str) -> Rect {
    let targets: Vec<_> = h.state().app.widgets.iter().filter(|(name, _)| name == id).collect();
    assert_eq!(targets.len(), 1, "one visible real hit for {id}");
    targets[0].1
}

fn click_at(h: &mut Harness<'_, Fixture>, pos: egui::Pos2) {
    h.event(egui::Event::PointerMoved(pos));
    h.run_steps(2);
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run_steps(3);
}

fn click(h: &mut Harness<'_, Fixture>, id: &str) {
    let at = rect(h, id).center();
    click_at(h, at);
}

fn key(h: &mut Harness<'_, Fixture>, key: egui::Key, shift: bool) {
    let modifiers = if shift { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE };
    h.event(egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers });
    h.run_steps(1);
    h.event(egui::Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers });
    h.run_steps(2);
}

fn tab_to(h: &mut Harness<'_, Fixture>, id: &str) {
    let wanted = rect(h, id);
    let mut path = Vec::new();
    for _ in 0..30 {
        let focus = h.ctx.memory(|memory| memory.focused());
        let current = focus.and_then(|id| h.ctx.read_response(id)).map(|response| response.rect);
        path.push((focus, current));
        if current == Some(wanted) {
            return;
        }
        key(h, egui::Key::Tab, false);
    }
    panic!("natural Tab did not reach {id}: {path:?}");
}

#[test]
fn empty_action_pointer_returns_each_exact_command_once() {
    for (id, command) in [("button:emptyImportPhotos", "file.addPhotos"), ("button:emptyImportLightroom", "file.importLightroom")] {
        let mut h = fixture(1000.0, Mode::Returns, false);
        assert!(h.state().returned.is_empty());
        click(&mut h, id);
        assert_eq!(h.state().returned, [command]);
        h.run_steps(12);
        assert_eq!(h.state().returned, [command], "idle frames do not repeat activation");
        assert!(h.state().asked.lock().unwrap().is_empty(), "return-only test does not dispatch");
    }
}

#[test]
fn empty_action_natural_tab_enter_and_space_return_once() {
    for (id, command, activation) in
        [("button:emptyImportPhotos", "file.addPhotos", egui::Key::Enter), ("button:emptyImportLightroom", "file.importLightroom", egui::Key::Space)]
    {
        let mut h = fixture(1000.0, Mode::Returns, false);
        h.event(egui::Event::PointerGone);
        h.run_steps(2);
        tab_to(&mut h, id);
        key(&mut h, activation, false);
        assert_eq!(h.state().returned, [command]);
        h.run_steps(12);
        assert_eq!(h.state().returned, [command]);
    }
}

#[test]
fn empty_action_layout_retains_horizontal_or_stacked_thirty_point_targets() {
    for (width, stacked) in [(1000.0, false), (300.0, true)] {
        let h = fixture(width, Mode::Returns, false);
        let photos = rect(&h, "button:emptyImportPhotos");
        let catalog = rect(&h, "button:emptyImportLightroom");
        assert_eq!(photos.height(), 30.0);
        assert_eq!(catalog.height(), 30.0);
        assert!(!photos.intersects(catalog));
        if stacked {
            assert_eq!(photos.center().x, catalog.center().x);
            assert_eq!(catalog.top() - photos.bottom(), 8.0);
        } else {
            assert_eq!(photos.top(), catalog.top());
            assert_eq!(catalog.left() - photos.right(), 8.0);
        }
        for target in [photos, catalog] {
            assert!(target.left() >= 0.0 && target.right() <= width);
            assert!(target.top() >= 0.0 && target.bottom() <= 650.0);
        }
    }
}

#[test]
fn empty_action_disabled_measure_and_sizing_probes_are_inert() {
    let mut disabled = fixture(1000.0, Mode::Disabled, false);
    click(&mut disabled, "button:emptyImportPhotos");
    key(&mut disabled, egui::Key::Tab, false);
    key(&mut disabled, egui::Key::Enter, false);
    key(&mut disabled, egui::Key::Space, false);
    assert!(disabled.state().returned.is_empty());
    assert!(disabled.state().asked.lock().unwrap().is_empty());
    for mode in [Mode::Measure, Mode::Sizing] {
        let mut h = fixture(1000.0, Mode::Returns, false);
        let at = rect(&h, "button:emptyImportPhotos").center();
        h.state_mut().mode = mode;
        h.run_steps(2);
        click_at(&mut h, at);
        key(&mut h, egui::Key::Enter, false);
        assert!(h.state().returned.is_empty());
        assert!(h.state().asked.lock().unwrap().is_empty());
        assert!(!h.state().app.widgets.iter().any(|(id, _)| id.starts_with("button:emptyImport")));
        h.state_mut().mode = Mode::Returns;
        h.event(egui::Event::PointerGone);
        h.run_steps(12);
        assert!(h.state().returned.is_empty(), "measurement must not defer a command to an ordinary frame");
        assert!(h.state().asked.lock().unwrap().is_empty(), "measurement must not defer a picker request");
    }
}

#[test]
fn empty_action_actual_owner_availability_labels_and_cancel_picker_once() {
    let mut h = fixture(1000.0, Mode::Owner, false);
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, crate::i18n::tr("Import Photos…").as_ref()).is_some());
    assert_eq!(
        h.state().app.widgets.iter().filter(|(id, _)| id == "button:emptyImportLightroom").count(),
        usize::from(!cfg!(target_arch = "wasm32"))
    );
    click(&mut h, "button:emptyImportPhotos");
    h.run_steps(10);
    {
        let asked = h.state().asked.lock().unwrap();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].kind, crate::pick::PickKind::Files);
        assert_eq!(asked[0].filter.as_ref().unwrap().1, lightcraft_engine::import::EXTENSIONS);
    }
    assert!(h.state().app.pending_picks.is_empty());
    assert!(h.state().app.ui.dialog.is_none());
    assert_eq!(h.state().app.session.catalog.photos().count(), 0);
    #[cfg(not(target_arch = "wasm32"))]
    {
        assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, crate::i18n::tr("Import Lightroom Catalog…").as_ref()).is_some());
        click(&mut h, "button:emptyImportLightroom");
        h.run_steps(10);
        let asked = h.state().asked.lock().unwrap();
        assert_eq!(asked.len(), 2);
        assert_eq!(asked[1].kind, crate::pick::PickKind::File);
        assert_eq!(asked[1].filter.as_ref().unwrap().1, ["lrcat", "zip"]);
        assert!(h.state().app.pending_picks.is_empty());
        assert!(h.state().app.ui.dialog.is_none());
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn empty_action_actual_owner_refusal_toasts_without_opening_picker() {
    let mut h = fixture(1000.0, Mode::Owner, true);
    let before = h.state().app.session.catalog.photos().count();
    click(&mut h, "button:emptyImportPhotos");
    assert_eq!(h.state().app.ui.toast.as_ref().unwrap().0, "finish Lightroom catalog import before adding photos");
    assert!(h.state().asked.lock().unwrap().is_empty(), "busy refusal occurs before any picker");
    assert_eq!(h.state().app.session.catalog.photos().count(), before);
    assert!(h.state().app.pending_picks.is_empty());
    assert!(h.state().app.ui.dialog.is_none());
}

#[test]
fn empty_action_no_action_grid_stays_without_buttons_or_commands() {
    let mut h = fixture(1000.0, Mode::NoActionGrid, false);
    assert!(!h.state().app.widgets.iter().any(|(id, _)| id.starts_with("button:emptyImport")));
    key(&mut h, egui::Key::Tab, false);
    key(&mut h, egui::Key::Enter, false);
    key(&mut h, egui::Key::Space, false);
    assert!(h.state().returned.is_empty());
    assert!(h.state().asked.lock().unwrap().is_empty());
}
