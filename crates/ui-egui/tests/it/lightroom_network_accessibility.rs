//! Keyboard focus must follow a network source when virtual scrolling replaces its row.
//! The existing pointer-scrolling test cannot catch focus jumping to a different share.
use egui::accesskit::{Role, Toggled};
use egui_kittest::{Harness, kittest::NodeT};
use lightcraft_ui_egui::{LightcraftApp, Services, state::Dialog};

fn harness() -> Harness<'static, LightcraftApp> {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services { png: None, ..Default::default() });
    app.headless_host = true;
    app.ui.dialog = Some(Dialog::LightroomDestination {
        catalog: "Example.lrcat".into(),
        library: "Example".into(),
        photos: 0,
        new_library: false,
        name: "Imported".into(),
        location: "Example".into(),
        dont_ask: false,
        network_sources: (0..40).map(|i| format!(r"\\nas\source-{i:02}")).collect(),
        network_shares: Vec::new(),
        generation: app.session.library_generation(),
    });
    let mut h = Harness::builder().with_size(egui::vec2(1500.0, 950.0)).build_ui_state(
        |ui, app: &mut LightcraftApp| {
            app.logic(&ui.ctx().clone());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(6);
    h
}

fn focused_source(h: &Harness<'_, LightcraftApp>) -> Option<(String, Option<Toggled>)> {
    h.root()
        .children_recursive()
        .map(|n| n.accesskit_node())
        .find(|n| n.is_focused() && n.role() == Role::CheckBox && n.label().is_some_and(|s| s.starts_with(r"\\nas\")))
        .map(|n| (n.label().unwrap(), n.toggled()))
}

/// Home/End and arrows reach offscreen sources without losing focus or changing consent;
/// Space toggles only the focused share, and Escape cancels without starting an import.
#[test]
fn keyboard_reaches_offscreen_network_sources_and_preserves_their_identity() {
    let mut h = harness();
    assert!(h.root().children_recursive().map(|n| n.accesskit_node()).any(|n| n.role() == Role::Dialog && n.data().is_modal()));
    for _ in 0..20 {
        h.key_press(egui::Key::Tab);
        h.run_steps(1);
        if focused_source(&h).is_some() {
            break;
        }
    }
    assert_eq!(focused_source(&h), Some((r"\\nas\source-00".into(), Some(Toggled::False))));
    h.key_press(egui::Key::ArrowDown);
    h.run_steps(2);
    assert_eq!(focused_source(&h), Some((r"\\nas\source-01".into(), Some(Toggled::False))));
    h.key_press(egui::Key::ArrowUp);
    h.run_steps(2);
    assert_eq!(focused_source(&h), Some((r"\\nas\source-00".into(), Some(Toggled::False))));
    h.key_press(egui::Key::End);
    h.run_steps(4);
    assert_eq!(focused_source(&h), Some((r"\\nas\source-39".into(), Some(Toggled::False))));
    h.key_press(egui::Key::Space);
    h.run_steps(2);
    assert_eq!(focused_source(&h), Some((r"\\nas\source-39".into(), Some(Toggled::True))));
    h.key_press(egui::Key::Home);
    h.run_steps(4);
    h.key_press(egui::Key::ArrowDown);
    h.run_steps(4);
    assert_eq!(focused_source(&h), Some((r"\\nas\source-01".into(), Some(Toggled::False))));
    assert!(matches!(&h.state().ui.dialog, Some(Dialog::LightroomDestination { network_shares, .. }) if network_shares == &[r"\\nas\source-39"]));
    // At the limit, navigation still reaches selected rows so consent can be removed, but
    // cannot focus a disabled source. Freeing a slot makes the next offscreen row reachable.
    if let Some(Dialog::LightroomDestination { network_sources, network_shares, .. }) = &mut h.state_mut().ui.dialog {
        *network_shares = network_sources.iter().take(32).cloned().collect();
    }
    h.run_steps(2);
    h.key_press(egui::Key::End);
    h.run_steps(4);
    assert_eq!(focused_source(&h), Some((r"\\nas\source-31".into(), Some(Toggled::True))));
    h.key_press(egui::Key::ArrowDown);
    h.run_steps(2);
    assert_eq!(focused_source(&h), Some((r"\\nas\source-31".into(), Some(Toggled::True))));
    h.key_press(egui::Key::Space);
    h.run_steps(2);
    h.key_press(egui::Key::ArrowDown);
    h.run_steps(4);
    assert_eq!(focused_source(&h), Some((r"\\nas\source-32".into(), Some(Toggled::False))));
    for _ in 0..12 {
        h.key_press(egui::Key::Tab);
        h.run_steps(1);
        if focused_source(&h).is_none() {
            break;
        }
    }
    assert!(
        h.root().children_recursive().map(|n| n.accesskit_node()).any(|n| n.is_focused() && !n.label().is_some_and(|s| s.starts_with(r"\\nas\")))
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.state().ui.dialog.is_none());
    assert!(h.state().lightroom.is_none());
}
