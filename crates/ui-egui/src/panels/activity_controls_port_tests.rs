//! Fake in-memory task rows only: no worker, filesystem operation, or real job is started/cancelled.
use super::{CROSS, MAX_ROWS, SHOW_AFTER_MS, WIDTH, cancel, count_text, detail_line, more_control, row, show};
use crate::LightcraftApp;
use crate::headless::HeadlessView;
use crate::theme::Tokens;
use egui::vec2;
use lightcraft_engine::activity::{Cancel, TaskGuard};
use std::time::Duration;

#[derive(Clone, Copy)]
enum Pass {
    Real,
    Measure,
    Sizing,
    Owners,
    Disabled,
    OwnerMeasure,
}

struct Harness {
    app: LightcraftApp,
    view: HeadlessView,
    time: f64,
    owners: std::collections::HashMap<String, (crate::widgets::WidgetHit, egui::Sense, bool, bool)>,
    nodes: std::collections::HashMap<egui::accesskit::NodeId, egui::accesskit::Node>,
    rects: Vec<(String, egui::Rect)>,
    components: Vec<(String, String)>,
}

impl Harness {
    fn new(sdk: bool) -> Self {
        let app = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services { png: None, ..Default::default() });
        let view = HeadlessView::new();
        view.ctx.enable_accesskit();
        if sdk {
            app.install_framework(&view.ctx);
        }
        Self { app, view, owners: Default::default(), nodes: Default::default(), time: 0.0, rects: Vec::new(), components: Vec::new() }
    }

    fn tasks(&self, count: usize) -> Vec<TaskGuard> {
        (0..count).map(|_| self.app.session.activity.start("export", "Duplicate label", Cancel::Yes)).collect()
    }

    fn visible(&mut self) {
        // Only the task registry's display delay passes. These tasks own no worker or real job.
        std::thread::sleep(Duration::from_millis(SHOW_AFTER_MS + 50));
        for _ in 0..4 {
            self.frame(Vec::new(), Pass::Real);
        }
    }

    fn frame(&mut self, events: Vec<egui::Event>, pass: Pass) {
        let raw = HeadlessView::raw_input(vec2(800.0, 800.0), 1.0, self.time, events);
        self.time += 1.0 / 60.0;
        let app = &mut self.app;
        let mut components = Vec::new();
        let mut rects = Vec::new();
        let mut owners = std::collections::HashMap::new();
        let mut out = self.view.ctx.run_ui(raw, |ui| {
            crate::widgets::take_registries(ui.ctx());
            let (_, record) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("activity-test-inspect"), |ui| match pass {
                Pass::Real => show(app, ui.ctx()),
                Pass::Measure => craft::ui::runtime::measure::run(&ui.ctx().clone(), || show(app, ui.ctx())),
                Pass::Owners | Pass::Disabled | Pass::OwnerMeasure => {
                    if matches!(pass, Pass::Disabled) {
                        ui.disable();
                    }
                    let mut draw = |ui: &mut egui::Ui| {
                        let task = app.session.activity.list().into_iter().next().unwrap();
                        let tokens = Tokens::get(ui.ctx());
                        if row(ui, &tokens, &task, WIDTH - 18.0) {
                            cancel(app, task.id);
                        }
                        if more_control(ui, &tokens, "+2 more") {
                            app.activity_expanded = !app.activity_expanded;
                        }
                    };
                    if matches!(pass, Pass::OwnerMeasure) {
                        craft::ui::runtime::measure::run(&ui.ctx().clone(), || draw(ui));
                    } else {
                        draw(ui);
                    }
                }
                Pass::Sizing => {
                    let mut child = ui.new_child(egui::UiBuilder::new().sizing_pass().invisible());
                    let tokens = Tokens::get(child.ctx());
                    for task in app.session.activity.list() {
                        assert!(!row(&mut child, &tokens, &task, WIDTH - 18.0), "sizing cannot activate a Cancel");
                    }
                    assert!(!more_control(&mut child, &tokens, "+2 more"));
                }
            });
            components = record
                .as_ref()
                .map(|record| {
                    record
                        .elements
                        .iter()
                        .map(|element| {
                            (element.component.to_string(), element.facts.as_ref().and_then(|facts| facts.name.clone()).unwrap_or_default())
                        })
                        .collect()
                })
                .unwrap_or_default();
            let (geometry, hits) = crate::widgets::take_registries(ui.ctx());
            rects = geometry;
            owners.clear();
            for (name, hit) in hits.into_iter().filter(|(name, _)| name == "activity:more" || name.starts_with("activity:cancel:")) {
                let raw = ui.ctx().read_response(hit.id).expect("just registered in this pass");
                assert_eq!((hit.layer_id, hit.interact_rect), (raw.layer_id, raw.interact_rect));
                assert_eq!(rects.iter().find(|(n, _)| n == &name).unwrap().1, raw.rect);
                assert!(owners.insert(name, (hit, raw.sense, raw.enabled(), raw.is_pointer_button_down_on())).is_none());
            }
        });
        self.nodes.clear();
        if let Some(update) = out.platform_output.accesskit_update {
            self.nodes.extend(update.nodes);
        }
        out.textures_delta.clear();
        self.owners = owners;
        self.rects = rects;
        self.components = components;
    }

    fn check_owner(&self, name: &str, id: egui::Id, role: egui::accesskit::Role, label: &str, enabled: bool) {
        let (hit, sense, raw_enabled, _) = self.owners[name];
        assert_eq!(hit.id, id);
        assert!(hit.interact_rect.contains(self.rect(name).center()));
        assert_eq!(sense.senses_click(), enabled, "raw enabled={raw_enabled}");
        assert_eq!(raw_enabled, enabled);
        let n = self.nodes.get(&id.accesskit_id()).expect("current control AX node");
        assert_eq!(n.role(), role);
        assert_eq!(n.label(), Some(label));
        assert_eq!(n.is_disabled(), !enabled);
    }

    fn key(&mut self, key: egui::Key) {
        for pressed in [true, false] {
            self.frame(vec![egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE }], Pass::Real);
        }
        self.frame(Vec::new(), Pass::Real);
    }

    fn rect(&self, id: &str) -> egui::Rect {
        self.rects.iter().find(|(found, _)| found == id).map(|(_, rect)| *rect).unwrap_or_else(|| panic!("missing {id}: {:?}", self.rects))
    }

    fn has(&self, id: &str) -> bool {
        self.rects.iter().any(|(found, _)| found == id)
    }

    fn rows(&self) -> Vec<String> {
        self.rects.iter().filter(|(id, _)| id.starts_with("activity:row:")).map(|(id, _)| id.clone()).collect()
    }

    fn click(&mut self, id: &str) {
        let at = self.rect(id).center();
        self.frame(vec![egui::Event::PointerMoved(at)], Pass::Real);
        for pressed in [true, false] {
            self.frame(
                vec![egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }],
                Pass::Real,
            );
        }
        self.frame(Vec::new(), Pass::Real);
    }
}

#[test]
fn activity_controls_port_cancel_targets_id_hides_stopping_and_non_cancellable() {
    for sdk in [false, true] {
        let mut h = Harness::new(sdk);
        let guards = h.tasks(2);
        let non = h.app.session.activity.start("faces", "Duplicate label", Cancel::No);
        h.visible();
        assert!(!h.has(&format!("activity:cancel:{}", non.id())));
        let row_rect = h.rect(&format!("activity:row:{}", guards[1].id()));
        let cancel_rect = h.rect(&format!("activity:cancel:{}", guards[1].id()));
        assert_eq!(cancel_rect.size(), vec2(CROSS, CROSS), "preserve the 16-point hit target");
        assert!(cancel_rect.right() >= row_rect.right() - 2.0);
        h.click(&format!("activity:cancel:{}", guards[1].id()));
        assert!(!guards[0].is_cancelled() && guards[1].is_cancelled());
        assert!(!h.has(&format!("activity:cancel:{}", guards[1].id())), "stopping has no Cancel");
        assert!(h.has(&format!("activity:row:{}", guards[1].id())), "stopping row stays until the task drops");
        assert!(!h.app.session.activity.list().iter().find(|task| task.id == non.id()).unwrap().cancelling);
    }
}

#[test]
fn activity_controls_port_natural_tab_space_enter_cancel_only_selected_duplicate() {
    for key in [egui::Key::Space, egui::Key::Enter] {
        let mut h = Harness::new(true);
        let guards = h.tasks(2);
        h.visible();
        h.key(egui::Key::Tab);
        h.key(egui::Key::Tab);
        let focused = craft::ui::runtime::focus::focused_rec(&h.view.ctx).expect("Tab reaches the second task");
        assert_eq!(focused.stop.id, egui::Id::unique(("activity-cancel", guards[1].id())));
        h.key(key);
        assert!(!guards[0].is_cancelled() && guards[1].is_cancelled(), "{key:?} affects only its focused task");
    }
}

#[test]
fn activity_controls_port_more_less_preserves_three_rows_count_order_and_keyboard() {
    let mut h = Harness::new(true);
    let guards = h.tasks(5);
    h.visible();
    let expected: Vec<_> = guards.iter().map(|task| format!("activity:row:{}", task.id())).collect();
    assert_eq!(h.rows(), expected[..MAX_ROWS]);
    assert!(h.components.iter().any(|(component, name)| component == "Link" && name == "+2 more"), "{:?}", h.components);
    for _ in 0..4 {
        h.key(egui::Key::Tab);
    }
    assert_eq!(craft::ui::runtime::focus::focused_rec(&h.view.ctx).unwrap().stop.id, egui::Id::unique("activity-more"));
    h.key(egui::Key::Space);
    assert!(h.app.activity_expanded);
    assert_eq!(h.rows(), expected);
    assert!(h.components.iter().any(|(component, name)| component == "Link" && name == "Show less"));
    h.key(egui::Key::Enter);
    assert!(!h.app.activity_expanded);
    assert_eq!(h.rows(), expected[..MAX_ROWS]);
    assert!(guards.iter().all(|task| !task.is_cancelled()));
}

#[test]
fn activity_controls_port_pointer_more_less_and_empty_reset_in_legacy_and_sdk() {
    for sdk in [false, true] {
        let mut h = Harness::new(sdk);
        let guards = h.tasks(5);
        h.visible();
        h.click("activity:more");
        assert!(h.app.activity_expanded && h.rows().len() == 5);
        h.click("activity:more");
        assert!(!h.app.activity_expanded && h.rows().len() == MAX_ROWS);
        h.click("activity:more");
        drop(guards);
        h.frame(Vec::new(), Pass::Real);
        assert!(!h.app.activity_expanded && h.rows().is_empty());
    }
}

fn pointer_event(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
}

// Draw the production row and link under a controllable real Ui. The full Area creates its
// own Ui; disabling an unrelated outer Ui would not actually disable either control.
fn inert_owner(mode: Pass, link: bool) {
    let mut h = Harness::new(true);
    let tasks = h.tasks(5);
    let (name, id, role, label) = if link {
        ("activity:more".to_owned(), egui::Id::unique("activity-more"), egui::accesskit::Role::Link, "+2 more".to_owned())
    } else {
        (
            format!("activity:cancel:{}", tasks[0].id()),
            egui::Id::unique(("activity-cancel", tasks[0].id())),
            egui::accesskit::Role::Button,
            crate::i18n::tr("Cancel").to_string(),
        )
    };
    for _ in 0..3 {
        h.frame(vec![], Pass::Owners);
    }
    h.check_owner(&name, id, role, &label, true);
    let at = h.rect(&name).center();
    for _ in 0..if link { 2 } else { 1 } {
        for pressed in [true, false] {
            h.frame(
                vec![egui::Event::Key { key: egui::Key::Tab, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE }],
                Pass::Owners,
            );
        }
    }
    assert_eq!(craft::ui::runtime::focus::focused_rec(&h.view.ctx).unwrap().stop.id, id);
    h.frame(vec![egui::Event::PointerMoved(at)], Pass::Owners);
    h.frame(vec![pointer_event(at, true)], Pass::Owners);
    h.check_owner(&name, id, role, &label, true);
    assert!(h.owners[&name].3, "real enabled owner got the press");
    h.frame(vec![pointer_event(at, false)], mode);
    let check_quiet = |h: &Harness| {
        assert!(!h.app.activity_expanded && tasks.iter().all(|t| !t.is_cancelled()));
        if matches!(mode, Pass::Disabled) {
            h.check_owner(&name, id, role, &label, false);
        } else {
            assert!(h.owners.is_empty() && h.rects.is_empty());
        }
    };
    check_quiet(&h);
    for pressed in [true, false] {
        h.frame(
            vec![egui::Event::Key {
                key: egui::Key::Space,
                physical_key: Some(egui::Key::Space),
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            mode,
        );
        check_quiet(&h);
    }
    for _ in 0..3 {
        h.frame(vec![], Pass::Owners);
        assert!(!h.app.activity_expanded && tasks.iter().all(|t| !t.is_cancelled()));
    }
    h.check_owner(&name, id, role, &label, true);
    let at = h.rect(&name).center();
    h.frame(vec![egui::Event::PointerMoved(at)], Pass::Owners);
    h.frame(vec![pointer_event(at, true)], Pass::Owners);
    h.frame(vec![pointer_event(at, false)], Pass::Owners);
    h.frame(vec![], Pass::Owners);
    assert_eq!(h.app.activity_expanded, link);
    assert_eq!(tasks[0].is_cancelled(), !link);
    assert!(tasks[1..].iter().all(|t| !t.is_cancelled()));
}

#[test]
fn activity_controls_port_measuring_never_registers_or_dispatches() {
    let mut h = Harness::new(true);
    let guards = h.tasks(5);
    h.visible();
    h.key(egui::Key::Tab);
    for mode in [Pass::Disabled, Pass::OwnerMeasure, Pass::Sizing] {
        for link in [false, true] {
            inert_owner(mode, link);
        }
    }
    for pass in [Pass::Measure, Pass::Sizing] {
        h.frame(
            vec![egui::Event::Key { key: egui::Key::Space, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }],
            pass,
        );
        assert!(h.rects.is_empty(), "no automation registrations in a speculative pass");
        assert!(!h.app.activity_expanded && guards.iter().all(|task| !task.is_cancelled()));
    }
}

#[test]
fn activity_controls_port_sdk_owns_controls_and_progress_helpers_are_unchanged() {
    let mut h = Harness::new(true);
    let guards = h.tasks(4);
    guards[0].progress(2, 5);
    guards[0].detail("fake-photo.raw");
    h.visible();
    assert_eq!(h.components.iter().filter(|(component, _)| component == "IconButton").count(), MAX_ROWS);
    assert_eq!(h.components.iter().filter(|(component, _)| component == "Link").count(), 1);
    for g in &guards[..MAX_ROWS] {
        h.check_owner(
            &format!("activity:cancel:{}", g.id()),
            egui::Id::unique(("activity-cancel", g.id())),
            egui::accesskit::Role::Button,
            &crate::i18n::tr("Cancel"),
            true,
        );
    }
    h.check_owner("activity:more", egui::Id::unique("activity-more"), egui::accesskit::Role::Link, "+1 more", true);
    let task = h.app.session.activity.list().remove(0);
    assert_eq!(count_text(&task), "2 of 5");
    assert_eq!(detail_line(&task), "2 of 5 · fake-photo.raw");
}

#[test]
fn activity_controls_port_stale_cancel_is_quiet_without_touching_other_tasks() {
    let mut h = Harness::new(true);
    let stale = h.app.session.activity.start("export", "Duplicate label", Cancel::Yes);
    let guards = h.tasks(1);
    let id = stale.id();
    let old = format!("activity:cancel:{id}");
    let old_id = egui::Id::unique(("activity-cancel", id));
    h.visible();
    h.check_owner(&old, old_id, egui::accesskit::Role::Button, &crate::i18n::tr("Cancel"), true);
    let at = h.rect(&old).center();
    h.frame(vec![egui::Event::PointerMoved(at)], Pass::Real);
    h.frame(vec![pointer_event(at, true)], Pass::Real);
    assert!(h.owners[&old].3);
    drop(stale);
    h.frame(vec![], Pass::Real);
    assert!(!h.has(&old) && !h.owners.contains_key(&old));
    let remaining = format!("activity:cancel:{}", guards[0].id());
    assert!(h.rect(&remaining).contains(at), "survivor moved under the original press");
    h.frame(vec![pointer_event(at, false)], Pass::Real);
    h.frame(vec![], Pass::Real);
    assert!(!guards[0].is_cancelled());
    h.app.ui.status = "Ready".into();
    cancel(&mut h.app, id);
    assert!(!guards[0].is_cancelled());
    assert_eq!(h.app.ui.status, "Ready");
    assert!(h.app.ui.toast.is_none());
}
