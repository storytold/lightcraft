//! Source-only preparation: run only in the parent's separately admitted test job.
//! Uses the actual bottom-bar Sort opener and its framework menu rows.

use super::*;
use crate::headless::HeadlessView;
use crate::widgets::{WidgetHit, take_registries};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Response};
use lightcraft_catalog::{Op, Photo, PhotoId, Source};

const NOW: &str = "2026-11-19T12:00:00";

fn fixture(random: bool, held: bool) -> (LightcraftApp, PhotoId) {
    let mut s = lightcraft_engine::Session::new();
    s.clock = Box::new(|| NOW.to_owned());
    for n in 1..=8 {
        let mut photo = Photo::new(PhotoId(n), Source::Demo { scene: 0 }, &format!("popup-{n}.jpg"), "JPEG", 2, 2, NOW);
        photo.rating = 4;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    }
    s.execute("library.sort", &json!({"key": "fileName", "group": "none"})).unwrap();
    if random {
        s.execute("library.shuffle", &json!({"seed": 11})).unwrap();
    }
    s.execute("library.filter", &json!({"rating": 3})).unwrap();
    s.execute("library.holdView", &json!({"on": held})).unwrap();
    let before = s.visible_cloned();
    assert_eq!(before.len(), 8);
    let removed = before[2];
    s.execute("library.select", &json!({"ids": [before[0].0, before[1].0]})).unwrap();
    s.execute("photo.rate", &json!({"ids": [removed.0], "rating": 1})).unwrap();
    assert_eq!(s.visible_cloned().contains(&removed), held);
    s.journal.clear();
    (LightcraftApp::new(s, Default::default()), removed)
}

#[derive(Clone, Copy)]
enum Pass { Real, Measure, Sizing }

struct Frames {
    ctx: egui::Context,
    app: LightcraftApp,
    time: f64,
    scale: f32,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, WidgetHit)>,
    rows: Vec<(String, Response)>,
}

impl Frames {
    fn new(random: bool, held: bool, scale: f32) -> Self {
        let (app, _) = fixture(random, held);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        ctx.enable_accesskit();
        app.install_framework(&ctx);
        let mut f = Self { ctx, app, time: 0.0, scale, rects: vec![], hits: vec![], rows: vec![] };
        f.settle();
        f
    }

    fn frame(&mut self, events: Vec<Event>, pass: Pass) {
        let _ = take_registries(&self.ctx);
        self.time += 0.04;
        let raw = HeadlessView::raw_input(vec2(1000.0, 700.0), self.scale, self.time, events);
        self.rows.clear();
        let mut output = self.ctx.run_ui(raw, |ui| {
            let (_, record) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("sort-popup-test-inspection"), |ui| {
                match pass {
                    Pass::Real => show(&mut self.app, ui),
                    Pass::Measure => {
                        let ctx = ui.ctx().clone();
                        craft::ui::runtime::measure::run(&ctx, || show(&mut self.app, ui));
                    }
                    Pass::Sizing => {
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                        show(&mut self.app, &mut child);
                    }
                }
            });
            if let Some(record) = record {
                for element in &record.elements {
                    if element.component == "MenuItem" && element.part.is_none()
                        && let Some(name) = element.facts.as_ref().and_then(|facts| facts.name.as_ref())
                        && let Some(response) = ui.ctx().read_response(element.id)
                    {
                        self.rows.push((name.clone(), response));
                    }
                }
            }
        });
        output.textures_delta.clear();
        (self.rects, self.hits) = take_registries(&self.ctx);
    }

    fn settle(&mut self) {
        for _ in 0..8 { self.frame(vec![], Pass::Real); }
    }

    fn row(&self, label: &str) -> &Response {
        let rows: Vec<_> = self.rows.iter().filter(|(name, _)| name == label).collect();
        assert_eq!(rows.len(), 1, "one actual SDK row for {label}");
        &rows[0].1
    }

    fn hit(&self, id: &str) -> WidgetHit {
        let hits: Vec<_> = self.hits.iter().filter(|(name, _)| name == id).collect();
        assert_eq!(hits.len(), 1, "one actual locator response for {id}");
        hits[0].1
    }

    fn pointer(&mut self, pos: Pos2, pressed: bool) {
        self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }], Pass::Real);
    }

    fn click_at(&mut self, pos: Pos2, long: bool) {
        self.frame(vec![Event::PointerMoved(pos)], Pass::Real);
        self.pointer(pos, true);
        if long { self.time += 2.0; self.frame(vec![], Pass::Real); }
        self.pointer(pos, false);
        self.settle();
    }

    fn key(&mut self, key: Key) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE }], Pass::Real);
        }
        self.settle();
    }

    /// The Sort menu is drawn: its Ascending row is always there while it is open.
    fn is_open(&self) -> bool {
        self.rows.iter().any(|(name, _)| name == "Ascending")
    }

    fn open(&mut self) {
        let opener = self.hit("icon:sort");
        self.click_at(opener.interact_rect.center(), false);
        assert!(self.is_open(), "actual framework menu opened");
    }

    fn snapshot(&mut self) -> serde_json::Value {
        json!({
            "visible": self.app.session.visible_cloned(),
            "seed": self.app.session.sort.seed,
            "key": format!("{:?}", self.app.session.sort.key),
            "selected": self.app.session.selection.ids,
            "active": self.app.session.active(),
            "journal": self.app.session.journal,
            "undo": self.app.session.undo.len(),
            "redo": self.app.session.redo.len(),
        })
    }
}

#[test]
fn external_sort_popup_action_visibility_and_real_refresh_locator() {
    for scale in [1.0, 1.5, 2.0] {
        for random in [false, true] {
            for held in [false, true] {
                let mut f = Frames::new(random, held, scale);
                let before = f.snapshot();
                f.open();
                assert_eq!(f.rows.iter().any(|(name, _)| name == "Reshuffle"), random);
                assert_eq!(f.rows.iter().any(|(name, _)| name == "Refresh View"), held);
                assert_eq!(f.rects.iter().any(|(name, _)| name == "button:refreshView"), held);
                if random { assert!(f.row("Reshuffle").interact_rect.is_positive()); }
                if held {
                    let row = f.row("Refresh View");
                    let hit = f.hit("button:refreshView");
                    assert_eq!(hit.id, row.id, "locator names the SDK row, not a hover probe");
                    assert_eq!(hit.layer_id, row.layer_id);
                    assert_eq!(hit.interact_rect, row.interact_rect);
                    assert_ne!(hit.layer_id, f.hit("icon:sort").layer_id, "the menu owns its layer");
                }
                assert_eq!(f.snapshot(), before, "opening and idle must not dispatch");
            }
        }
    }
}

#[test]
fn external_sort_popup_actions_dispatch_exactly_once_with_deterministic_state() {
    for (label, command) in [("Reshuffle", "library.shuffle"), ("Refresh View", "library.refresh")] {
        for key in [None, Some(Key::Enter), Some(Key::Space)] {
            for scale in [1.0, 1.5, 2.0] {
                let mut f = Frames::new(true, true, scale);
                let (mut oracle, removed) = fixture(true, true);
                let before = f.app.session.visible_cloned();
                let selection = f.app.session.selection.ids.clone();
                let active = f.app.session.active();
                let seed = f.app.session.sort.seed;
                oracle.run(command, json!({})).unwrap();
                f.open();
                let response = f.row(label).clone();
                if let Some(key) = key {
                    f.ctx.memory_mut(|memory| memory.request_focus(response.id));
                    f.frame(vec![], Pass::Real);
                    f.key(key);
                } else {
                    f.click_at(response.interact_rect.center(), true);
                }
                assert!(!f.is_open(), "action closes the menu");
                assert_eq!(f.app.session.journal, vec![(command.to_owned(), json!({}))], "single caller dispatch, empty params");
                assert_eq!(f.app.session.sort.seed, oracle.session.sort.seed, "one deterministic transition, not chance order inequality");
                assert_eq!(f.app.session.visible_cloned(), oracle.session.visible_cloned());
                assert!(!f.app.session.visible_cloned().contains(&removed));
                assert_eq!(f.app.session.selection.ids, selection);
                assert_eq!(f.app.session.active(), active);
                if command == "library.refresh" {
                    assert_eq!(f.app.session.sort.seed, seed, "refresh is not a shuffle");
                    assert_eq!(f.app.session.visible_cloned(), before.into_iter().filter(|id| *id != removed).collect::<Vec<_>>());
                } else {
                    assert_ne!(f.app.session.sort.seed, seed, "deterministic seed changes even if an order happened to match");
                }
                let after = f.snapshot();
                f.settle();
                assert_eq!(f.snapshot(), after, "no idle/release replay");
            }
        }
    }
}

#[test]
fn external_sort_popup_escape_outside_and_reopen_do_not_dispatch() {
    let mut f = Frames::new(true, true, 1.0);
    let before = f.snapshot();
    f.open();
    f.key(Key::Escape);
    assert!(!f.is_open());
    assert_eq!(f.snapshot(), before);
    f.open();
    f.click_at(pos2(990.0, 10.0), false);
    assert!(!f.is_open());
    assert_eq!(f.snapshot(), before);
    f.open();
    assert_eq!(f.snapshot(), before);
}

#[test]
fn external_sort_popup_measure_and_native_sizing_cannot_dispatch_actions() {
    for pass in [Pass::Measure, Pass::Sizing] {
        for label in ["Reshuffle", "Refresh View"] {
            let mut f = Frames::new(true, true, 1.0);
            f.open();
            let response = f.row(label).clone();
            let before = f.snapshot();
            let pos = response.interact_rect.center();
            for pressed in [true, false] {
                f.frame(vec![Event::PointerMoved(pos), Event::PointerButton {
                    pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE,
                }], pass);
            }
            for key in [Key::Enter, Key::Space] {
                f.ctx.memory_mut(|memory| memory.request_focus(response.id));
                f.frame(vec![Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: Modifiers::NONE }], pass);
            }
            assert_eq!(f.snapshot(), before);
            assert!(!f.hits.iter().any(|(name, _)| name == "button:refreshView"), "measurement publishes no action hit");
        }
    }
}


#[test]
fn library_sort_choices_change_order_direction_and_group_without_photo_edits() {
    let mut f = Frames::new(false, false, 1.0);
    let (mut oracle, _) = fixture(false, false);
    let selection = f.app.session.selection.clone();
    let revision = f.app.session.catalog.revision;
    let undo = f.app.session.undo.len();
    for (id, label, params) in [
        ("sort:key:rating", "Rating", json!({"key":"rating"})),
        ("sort:descending", "Descending", json!({"ascending":false})),
        ("sort:group:month", "Month", json!({"group":"month"})),
        ("sort:key:random", "Random", json!({"key":"random"})),
        ("sort:key:fileName", "File Name", json!({"key":"fileName"})),
        ("sort:ascending", "Ascending", json!({"ascending":true})),
    ] {
        if !f.is_open() { f.open(); }
        let row = f.row(label).clone();
        assert_eq!(f.hit(id).id, row.id);
        assert!(row.enabled() && row.sense.is_focusable());
        oracle.run("library.sort", params.clone()).unwrap();
        f.click_at(row.interact_rect.center(), false);
        assert_eq!(f.app.session.sort, oracle.session.sort);
        assert_eq!(f.app.session.visible_cloned(), oracle.session.visible_cloned());
        assert_eq!(f.app.session.journal, oracle.session.journal);
        assert_eq!(f.app.session.selection, selection);
        assert_eq!(f.app.session.catalog.revision, revision);
        assert_eq!(f.app.session.undo.len(), undo);
        if id == "sort:key:random" {
            if !f.is_open() { f.open(); }
            let dir = f.row("Ascending").clone();
            assert!(!dir.enabled());
            f.click_at(dir.interact_rect.center(), false);
            assert_eq!(f.app.session.sort, oracle.session.sort);
            assert_eq!(f.app.session.journal, oracle.session.journal);
        }
    }
}


#[test]
fn library_sort_choices_measuring_release_is_inert_then_same_owner_changes_order() {
    for pass in [Pass::Measure, Pass::Sizing] {
        let mut f = Frames::new(false, true, 1.0);
        f.open();
        let target = "sort:key:rating";
        let hit = f.hit(target);
        let at = hit.interact_rect.center();
        let before = f.snapshot();
        f.frame(vec![Event::PointerMoved(at)], Pass::Real);
        f.pointer(at, true);
        assert!(f.ctx.read_response(hit.id).unwrap().is_pointer_button_down_on());
        f.frame(vec![Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }], pass);
        assert!(!f.hits.iter().any(|(n, _)| n.starts_with("sort:")), "measurement published a sort hit");
        f.settle();
        assert_eq!(f.snapshot(), before, "measuring release changed the library or replayed");
        if !f.is_open() { f.open(); }
        let recovered = f.hit(target);
        assert_eq!(recovered.id, hit.id);
        f.click_at(recovered.interact_rect.center(), false);
        assert_eq!(f.app.session.sort.key, lightcraft_catalog::SortKey::Rating);
        assert_eq!(f.app.session.journal, [("library.sort".to_owned(), json!({"key":"rating"}))]);
    }
}

#[test]
fn compact_view_sort_changes_rating_order_without_changing_photo_selection() {
    use egui_kittest::kittest::Queryable;
    let (app, _) = fixture(false, false);
    let (mut oracle, _) = fixture(false, false);
    oracle.run("library.sort", json!({"key":"rating"})).unwrap();
    let mut ready = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(320.0, 700.0)).build_ui_state(move |ui, app: &mut LightcraftApp| {
        if !ready {
            crate::theme::install_fonts(ui.ctx());
            crate::theme::apply(ui.ctx());
            app.install_framework(ui.ctx());
            ready = true;
            return;
        }
        let _ = take_registries(ui.ctx());
        show(app, ui);
    }, app);
    h.run_steps(8);
    assert!(h.query_by_label("Sort").is_none(), "narrow layout should not show the wide Sort opener");
    h.get_by_label("View").click();
    h.run_steps(8);
    h.get_by_label_contains("Sort").click();
    h.run_steps(8);
    assert!(h.state().session.journal.is_empty());
    h.get_by_label("Rating").click();
    h.run_steps(8);
    assert_eq!(h.state().session.sort, oracle.session.sort);
    assert_eq!(h.state().session.selection, oracle.session.selection);
    assert_eq!(h.state().session.catalog.revision, oracle.session.catalog.revision);
    assert_eq!(h.state().session.undo.len(), oracle.session.undo.len());
    assert_eq!(h.state_mut().session.visible_cloned(), oracle.session.visible_cloned());
    assert_eq!(h.state().session.journal, oracle.session.journal);
}

/// Matched old/new capture seam: add only this module declaration to the old source.
/// The same fixture and public Sort opener are used in both trees; no SDK-row lookup.
#[test]
fn capture_external_sort_popup_matched_old_new() {
    use egui_kittest::kittest::Queryable;
    let Some(directory) = std::env::var_os("CRAFT_UI_SORT_POPUP_CAPTURES").map(std::path::PathBuf::from) else { return };
    std::fs::create_dir_all(&directory).unwrap();
    for scale in [1.0, 1.5, 2.0] {
        for random in [false, true] {
            for held in [false, true] {
                let (app, _) = fixture(random, held);
                let mut ready = false;
                let mut harness = egui_kittest::Harness::builder().with_size(vec2(1000.0, 700.0)).with_pixels_per_point(scale).wgpu().build_ui_state(
                    move |ui, app: &mut LightcraftApp| {
                        if !ready {
                            crate::theme::install_fonts(ui.ctx());
                            crate::theme::apply(ui.ctx());
                            app.install_framework(ui.ctx());
                            ready = true;
                            return;
                        }
                        let _ = take_registries(ui.ctx());
                        show(app, ui);
                    }, app,
                );
                harness.input_mut().max_texture_side = Some(8192);
                harness.run_steps(8);
                harness.get_by_label("Sort").click();
                harness.run_steps(8);
                assert!(harness.query_by_label("Ascending").is_some());
                assert_eq!(harness.query_by_label("Reshuffle").is_some(), random);
                assert_eq!(harness.query_by_label("Refresh View").is_some(), held);
                assert!(harness.state().session.journal.is_empty());
                let stem = format!("sort-popup-random-{random}-held-{held}-{scale}x");
                harness.render().unwrap().save(directory.join(format!("{stem}.png"))).unwrap();
                let visible = harness.state_mut().session.visible_cloned();
                let state = json!({
                    "screen": [1000, 700], "scale": scale, "random": random, "held": held,
                    "visible": visible,
                    "seed": harness.state().session.sort.seed,
                    "selected": harness.state().session.selection.ids,
                    "active": harness.state().session.active(),
                    "journal": harness.state().session.journal,
                });
                std::fs::write(directory.join(format!("{stem}.json")), serde_json::to_vec_pretty(&state).unwrap()).unwrap();
            }
        }
    }
}
