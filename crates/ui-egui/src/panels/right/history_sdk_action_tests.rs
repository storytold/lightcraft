//! Real egui input drives the candidate panel and its SDK action hit owners.
//! Authored for the existing integrator’s normal test consumer; execution results are reported separately.

use super::*;
use std::sync::Arc;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Ui};
use crate::headless::HeadlessView;
use crate::widgets::{WidgetHit, take_registries};

#[derive(Clone, Copy)]
enum Pass { Real, Measure, Sizing }

struct Frames {
    ctx: egui::Context,
    time: f64,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, WidgetHit)>,
    pass: Pass,
    enabled: bool,
    key_trace: Vec<String>,
    publication: Vec<String>,
}

impl Frames {
    fn new() -> Self {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        let spec = craft::ui::InstallSpec::new(craft::host::target::ShellInfo::default(), Arc::new(craft::window::system::NoWindows));
        craft::ui::install(&ctx, spec).unwrap();
        Self { ctx, time: 0.0, rects: vec![], hits: vec![], pass: Pass::Real, enabled: true, key_trace: vec![], publication: vec![] }
    }

    fn frame(&mut self, events: Vec<Event>, mut draw: impl FnMut(&mut Ui)) {
        let requested: Vec<_> = events.iter().filter_map(|event| match event {
            Event::Key { key, pressed, .. } => Some((*key, *pressed)),
            _ => None,
        }).collect();
        let mut live = Vec::new();
        let mut published = (vec![], vec![]);
        let mut publication = Vec::new();
        let _ = take_registries(&self.ctx);
        self.time += 0.04;
        let mut raw = HeadlessView::raw_input(vec2(1000.0, 900.0), 1.0, self.time, events);
        raw.events.insert(0, Event::ModifiersChanged(Modifiers::NONE));
        let pass = self.pass;
        let enabled = self.enabled;
        let mut output = self.ctx.run_ui(raw, |ui| {
            if !requested.is_empty() {
                let events: Vec<_> = ui.input(|input| input.events.iter().filter_map(|event| match event {
                    Event::Key { key, pressed, .. } => Some((*key, *pressed)),
                    _ => None,
                }).collect());
                live.push((ui.ctx().cumulative_pass_nr(), events));
            }
            ui.set_max_width(350.0);
            ui.add_enabled_ui(enabled, |ui| match pass {
                Pass::Real => draw(ui),
                Pass::Measure => {
                    let ctx = ui.ctx().clone();
                    craft::ui::runtime::measure::run(&ctx, || draw(ui));
                }
                Pass::Sizing => {
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                    draw(&mut child);
                }
            });
            // The app drains once per ui pass and publishes only the nondiscarded one.
            // Do not merge discarded-pass locators into the live automation targets.
            let (rects, hits) = take_registries(ui.ctx());
            let discarded = ui.ctx().will_discard();
            if publication.len() < 8 {
                let menus: Vec<_> = hits.iter().filter(|(name, _)| name.contains("Menu:")).map(|(name, hit)| (name.clone(), hit.id)).collect();
                publication.push(format!("pass={} discarded={discarded} sdk_keys_open_last_pass={} native_popup={} menu_owners={menus:?}", ui.ctx().cumulative_pass_nr(), craft::ui::runtime::overlay::any_open(ui.ctx(), craft::ui::runtime::overlay::OpenFilter::Keys), egui::Popup::is_any_open(ui.ctx())));
            }
            published = if discarded { (vec![], vec![]) } else { (rects, hits) };
        });
        output.textures_delta.clear();
        (self.rects, self.hits) = published;
        self.publication = publication;
        if !requested.is_empty() {
            let native = self.ctx.memory(|memory| memory.focused());
            let sdk = craft::ui::runtime::focus::focused_rec(&self.ctx).map(|rec| (rec.stop.id, rec.stop.root, rec.stop.group));
            let owners: Vec<_> = self.hits.iter().filter(|(name, _)| name.contains("Menu:")).map(|(name, hit)| (name, hit.id)).collect();
            if self.key_trace.len() == 64 {
                self.key_trace.remove(0);
            }
            self.key_trace.push(format!("requested={requested:?} live_after_hook={live:?} native={native:?} sdk={sdk:?} native_popup={} owners={owners:?}", egui::Popup::is_any_open(&self.ctx)));
        }
    }

    fn settle(&mut self, mut draw: impl FnMut(&mut Ui)) {
        for _ in 0..3 {
            self.frame(vec![], &mut draw);
        }
    }

    fn rect(&self, id: &str) -> Rect {
        self.rects.iter().find(|(name, _)| name == id).unwrap_or_else(|| panic!("missing {id}: {:?}", self.rects)).1
    }

    fn click(&mut self, at: Pos2, button: PointerButton, mut draw: impl FnMut(&mut Ui)) {
        self.frame(vec![Event::PointerMoved(at)], &mut draw);
        for pressed in [true, false] {
            self.frame(vec![Event::PointerButton { pos: at, button, pressed, modifiers: Modifiers::NONE }], &mut draw);
        }
        self.settle(draw);
    }

    fn escape(&mut self, mut draw: impl FnMut(&mut Ui)) {
        let events = [true, false].map(|pressed| Event::Key {
            key: Key::Escape, physical_key: Some(Key::Escape), pressed, repeat: false, modifiers: Modifiers::NONE,
        }).to_vec();
        self.frame(events, &mut draw);
        self.settle(draw);
    }

    fn key(&mut self, key: Key, mut draw: impl FnMut(&mut Ui)) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE }], &mut draw);
        }
        self.settle(draw);
    }

    fn hit(&self, name: &str) -> WidgetHit {
        let matches: Vec<_> = self.hits.iter().filter(|(key, _)| key == name).collect();
        assert_eq!(matches.len(), 1, "one actual hit owner for {name}; publication={:?}", self.publication);
        matches[0].1
    }

    fn tab_to(&mut self, name: &str, mut draw: impl FnMut(&mut Ui)) {
        // Tab enters the real one-stop menu group; arrows travel its actual members. Never request focus.
        let mut trace = Vec::new();
        for _ in 0..16 {
            let native = self.ctx.memory(|memory| memory.focused());
            let focused = craft::ui::runtime::focus::focused_rec(&self.ctx).map(|rec| rec.stop.id);
            if focused == Some(self.hit(name).id) {
                assert_eq!(native, focused, "the actual SDK hit owner must hold real egui focus, not a manufactured parallel focus");
                return;
            }
            let menu_focus = self.hits.iter().find(|(key, hit)| key.contains("Menu:") && Some(hit.id) == focused);
            trace.push((focused, menu_focus.map(|(key, _)| key.clone())));
            self.key(if menu_focus.is_some() { Key::ArrowDown } else { Key::Tab }, &mut draw);
        }
        panic!("Natural Tab/arrow travel never reached the actual popup owner {name}: {trace:?}; key frames: {:?}", self.key_trace);
    }

    fn assert_dismissed(&self) {
        assert!(!self.rects.iter().any(|(id, _)| id.starts_with("versionMenu:") || id.starts_with("historyMenu:")), "action popup must be dismissed; sdk_keys_open={} sdk_top={:?} native_popup={} publication={:?}", craft::ui::runtime::overlay::any_open(&self.ctx, craft::ui::runtime::overlay::OpenFilter::Keys), craft::ui::runtime::overlay::top(&self.ctx), egui::Popup::is_any_open(&self.ctx), self.publication);
    }
}

fn fixture() -> (LightcraftApp, PhotoId) {
    let mut session = lightcraft_engine::Session::with_demo();
    let id = session.active().unwrap();
    session.clock = Box::new(|| "2026-11-21T00:00:00".to_owned());
    let other = session.catalog.photos().find(|p| p.id != id).unwrap().id;
    session.execute("library.select", &json!({"ids": [id.0, other.0], "active": id.0})).unwrap();
    session.catalog.apply(lightcraft_catalog::Op::SetVersions { id, versions: vec![] }).unwrap();
    for value in [1.0, 2.0, 3.0] {
        session.execute("develop.set", &json!({"control": "light.exposure", "value": value})).unwrap();
        session.execute("version.create", &json!({"name": format!("Look {value}")})).unwrap();
    }
    let mut versions = session.catalog.photo(id).unwrap().versions.clone();
    versions[1].auto = true;
    session.catalog.apply(lightcraft_catalog::Op::SetVersions { id, versions }).unwrap();
    session.execute("develop.set", &json!({"control": "light.exposure", "value": 4.0})).unwrap();
    (LightcraftApp::new(session, crate::Services::default()), id)
}

fn photo(app: &LightcraftApp, id: PhotoId) -> lightcraft_catalog::Photo {
    (**app.session.catalog.photo(id).unwrap()).clone()
}

fn state(app: &LightcraftApp, id: PhotoId) -> (lightcraft_catalog::Photo, Option<Arc<lightcraft_develop::DevelopSettings>>, usize) {
    (photo(app, id), app.session.before.get(&id).cloned(), app.session.undo.len())
}

fn version_menu(f: &mut Frames, app: &mut LightcraftApp, id: PhotoId, index: usize) {
    f.settle(|ui| versions(app, ui, id));
    let at = f.rect(&format!("version:{index}")).center();
    f.click(at, PointerButton::Secondary, |ui| versions(app, ui, id));
    assert!(f.rects.iter().any(|(name, _)| name == &format!("versionMenu:{index}:restore")));
}

fn history_menu(f: &mut Frames, app: &mut LightcraftApp, id: PhotoId, index: usize) {
    f.settle(|ui| activity(app, ui, id));
    let at = f.rect(&format!("history:{index}")).center();
    f.click(at, PointerButton::Secondary, |ui| activity(app, ui, id));
    assert!(f.rects.iter().any(|(name, _)| name == &format!("historyMenu:{index}:before")));
}

#[test]
fn sdk_history_restore_uses_model_index_through_named_reverse_and_auto_filter_and_undo() {
    for index in [2, 1] {
        let (mut app, id) = fixture();
        let before = photo(&app, id);
        let undo = app.session.undo.len();
        let mut f = Frames::new();
        f.ctx.data_mut(|data| data.insert_temp(egui::Id::unique("versions-tab"), index == 1));
        f.settle(|ui| versions(&mut app, ui, id));
        if index == 2 {
            assert!(f.rect("version:2").top() < f.rect("version:0").top(), "newest first");
            assert!(!f.rects.iter().any(|(name, _)| name == "version:1"), "auto version filtered, not reindexed");
        }
        version_menu(&mut f, &mut app, id, index);
        let at = f.rect(&format!("versionMenu:{index}:restore")).center();
        f.click(at, PointerButton::Primary, |ui| versions(&mut app, ui, id));
        assert_eq!(photo(&app, id).develop, before.versions[index].settings);
        assert_eq!(photo(&app, id).versions, before.versions, "restore must not rewrite snapshot identity or ordering");
        assert_eq!(app.session.undo.len(), undo + 1, "one restore, not one per layout pass");
        f.assert_dismissed();
        app.run("edit.undo", json!({})).unwrap();
        assert_eq!(photo(&app, id), before, "Undo restores settings, history and cursor together");
    }
}

#[test]
fn sdk_history_update_changes_only_target_settings_and_undo_keeps_snapshot_identity() {
    let (mut app, id) = fixture();
    let before = photo(&app, id);
    let undo = app.session.undo.len();
    let mut f = Frames::new();
    version_menu(&mut f, &mut app, id, 0);
    let at = f.rect("versionMenu:0:update").center();
    f.click(at, PointerButton::Primary, |ui| versions(&mut app, ui, id));
    let after = photo(&app, id);
    let mut expected = before.versions.clone();
    expected[0].settings = before.develop.clone();
    assert_eq!(after.versions, expected, "name, created, auto, count and model order unchanged");
    assert_eq!(after.develop, before.develop);
    assert_eq!(after.history, before.history);
    assert_eq!(after.history_cursor, before.history_cursor);
    assert_eq!(app.session.undo.len(), undo + 1);
    f.assert_dismissed();
    app.run("edit.undo", json!({})).unwrap();
    assert_eq!(photo(&app, id), before);
}

#[test]
fn sdk_history_set_snapshot_before_does_not_restore_or_add_undo() {
    let (mut app, id) = fixture();
    let before = photo(&app, id);
    let undo = app.session.undo.len();
    let mut f = Frames::new();
    version_menu(&mut f, &mut app, id, 0);
    let at = f.rect("versionMenu:0:before").center();
    f.click(at, PointerButton::Primary, |ui| versions(&mut app, ui, id));
    assert_eq!(app.session.before.get(&id), Some(&before.versions[0].settings));
    assert_eq!(photo(&app, id), before);
    assert_eq!(app.session.undo.len(), undo, "Before is session display state, not an edit");
    f.assert_dismissed();
}

#[test]
fn sdk_history_copy_step_to_before_uses_original_index_without_moving_cursor() {
    let (mut app, id) = fixture();
    let before = photo(&app, id);
    let index = before.history.len() - 3;
    let undo = app.session.undo.len();
    let mut f = Frames::new();
    f.settle(|ui| activity(&mut app, ui, id));
    assert!(f.rect(&format!("history:{}", before.history.len() - 1)).top() < f.rect(&format!("history:{index}")).top());
    history_menu(&mut f, &mut app, id, index);
    let at = f.rect(&format!("historyMenu:{index}:before")).center();
    f.click(at, PointerButton::Primary, |ui| activity(&mut app, ui, id));
    assert_eq!(app.session.before.get(&id), Some(&before.history[index].settings));
    assert_eq!(photo(&app, id), before, "copying an older step must not restore it or prune later steps");
    assert_eq!(app.session.undo.len(), undo);
    f.assert_dismissed();
}

#[test]
fn sdk_history_both_popup_cancellation_paths_leave_model_before_and_undo_untouched() {
    for history in [false, true] {
        for escape in [false, true] {
            let (mut app, id) = fixture();
            let before = state(&app, id);
            let mut f = Frames::new();
            if history {
                history_menu(&mut f, &mut app, id, before.0.history.len() - 2);
                if escape {
                    f.escape(|ui| activity(&mut app, ui, id));
                } else {
                    f.click(pos2(950.0, 850.0), PointerButton::Primary, |ui| activity(&mut app, ui, id));
                }
            } else {
                version_menu(&mut f, &mut app, id, 0);
                if escape {
                    f.escape(|ui| versions(&mut app, ui, id));
                } else {
                    f.click(pos2(950.0, 850.0), PointerButton::Primary, |ui| versions(&mut app, ui, id));
                }
            }
            assert_eq!(state(&app, id), before);
            f.assert_dismissed();
        }
    }
}

#[test]
fn sdk_history_current_version_row_click_is_still_a_no_op() {
    let (mut app, id) = fixture();
    app.run("version.update", json!({"index": 2})).unwrap();
    let before = state(&app, id);
    let mut f = Frames::new();
    f.settle(|ui| versions(&mut app, ui, id));
    let at = f.rect("version:2").center();
    f.click(at, PointerButton::Primary, |ui| versions(&mut app, ui, id));
    assert_eq!(state(&app, id), before);
}

#[test]
fn sdk_history_disabled_action_does_not_run_even_when_its_real_rect_is_clicked() {
    let mut f = Frames::new();
    let mut activations = 0;
    let mut draw = |ui: &mut Ui| {
        ui.add_enabled_ui(false, |ui| {
            if history_action(ui, ("version", 999, 0, "restore"), "Restore") {
                activations += 1;
            }
        });
    };
    f.settle(&mut draw);
    let at = f.rect("versionMenu:0:restore").center();
    f.click(at, PointerButton::Primary, &mut draw);
    drop(draw);
    assert_eq!(activations, 0);
}

#[test]
fn sdk_history_missing_photo_is_a_control_and_command_no_op() {
    let (mut app, active) = fixture();
    let before = state(&app, active);
    let mut f = Frames::new();
    f.settle(|ui| {
        versions(&mut app, ui, PhotoId(u64::MAX));
        activity(&mut app, ui, PhotoId(u64::MAX));
    });
    assert!(f.rects.is_empty());
    assert_eq!(state(&app, active), before);
}


#[derive(Clone, Copy)]
enum Panel { Snapshots, History }

fn draw_panel(panel: Panel, app: &mut LightcraftApp, ui: &mut Ui, id: PhotoId) {
    match panel { Panel::Snapshots => versions(app, ui, id), Panel::History => activity(app, ui, id) }
}

fn open_panel_menu(panel: Panel, f: &mut Frames, app: &mut LightcraftApp, id: PhotoId, index: usize) {
    match panel { Panel::Snapshots => version_menu(f, app, id, index), Panel::History => history_menu(f, app, id, index) }
}

fn document(app: &LightcraftApp) -> serde_json::Value {
    let mut before: Vec<_> = app.session.before.iter().map(|(id, settings)| (id.0, settings.clone())).collect();
    before.sort_by_key(|(id, _)| *id);
    json!({"catalog": app.session.catalog, "selection": app.session.selection, "before": before,
        "undo": app.session.undo.len(), "redo": app.session.redo.len(), "journal": app.session.journal})
}

fn other_photos(app: &LightcraftApp, id: PhotoId) -> Vec<lightcraft_catalog::Photo> {
    app.session.catalog.photos().filter(|p| p.id != id).map(|p| (**p).clone()).collect()
}

fn assert_one_command(app: &LightcraftApp, before: usize, command: &str, params: serde_json::Value) {
    assert_eq!(app.session.journal.len(), before + 1, "exactly one command, never one per layout");
    assert_eq!(app.session.journal.last(), Some(&(command.to_owned(), params)));
}

#[test]
fn snapshots_two_distinct_versions_restore_then_undo_through_both_full_photos() {
    let (mut app, id) = fixture();
    let initial = photo(&app, id);
    assert_ne!(initial.versions[0].settings, initial.versions[2].settings);
    let selection = app.session.selection.clone();
    let others = other_photos(&app, id);
    let undo = app.session.undo.len();
    let mut restored = Vec::new();
    let mut f = Frames::new();
    for (step, index) in [0, 2].into_iter().enumerate() {
        let journal = app.session.journal.len();
        version_menu(&mut f, &mut app, id, index);
        let at = f.rect(&format!("versionMenu:{index}:restore")).center();
        f.click(at, PointerButton::Primary, |ui| versions(&mut app, ui, id));
        assert_eq!(photo(&app, id).develop, initial.versions[index].settings);
        assert_eq!(photo(&app, id).versions, initial.versions);
        assert_eq!(other_photos(&app, id), others);
        assert_eq!(app.session.selection, selection);
        assert_eq!(app.session.undo.len(), undo + step + 1);
        assert_one_command(&app, journal, "version.restore", json!({"index": index}));
        f.assert_dismissed();
        restored.push(photo(&app, id));
    }
    app.run("edit.undo", json!({})).unwrap();
    assert_eq!(photo(&app, id), restored[0]);
    app.run("edit.undo", json!({})).unwrap();
    assert_eq!(photo(&app, id), initial);
    assert_eq!(other_photos(&app, id), others);
    assert_eq!(app.session.selection, selection);
    assert_eq!(app.session.undo.len(), undo);
}

#[test]
fn snapshots_update_and_before_use_both_distinct_indices_without_restore_or_retarget() {
    let (mut app, id) = fixture();
    let initial = photo(&app, id);
    let selection = app.session.selection.clone();
    let others = other_photos(&app, id);
    let undo = app.session.undo.len();
    let mut f = Frames::new();
    // Before must distinguish the original two looks; it is not a restore or a model edit.
    for index in [0, 2] {
        let journal = app.session.journal.len();
        version_menu(&mut f, &mut app, id, index);
        let at = f.rect(&format!("versionMenu:{index}:before")).center();
        f.click(at, PointerButton::Primary, |ui| versions(&mut app, ui, id));
        assert_eq!(app.session.before.get(&id), Some(&initial.versions[index].settings));
        assert_eq!(photo(&app, id), initial);
        assert_eq!(other_photos(&app, id), others);
        assert_eq!(app.session.selection, selection);
        assert_eq!(app.session.undo.len(), undo);
        assert_one_command(&app, journal, "beforeAfter.setBefore", json!({"source":"version", "name": initial.versions[index].name}));
        f.assert_dismissed();
    }
    for index in [0, 2] {
        let journal = app.session.journal.len();
        version_menu(&mut f, &mut app, id, index);
        let at = f.rect(&format!("versionMenu:{index}:update")).center();
        f.click(at, PointerButton::Primary, |ui| versions(&mut app, ui, id));
        let mut expected = initial.clone();
        expected.versions[index].settings = initial.develop.clone();
        assert_eq!(photo(&app, id), expected, "only this snapshot's settings change; metadata, order, history and cursor survive");
        assert_eq!(other_photos(&app, id), others);
        assert_eq!(app.session.selection, selection);
        assert_eq!(app.session.undo.len(), undo + 1);
        assert_one_command(&app, journal, "version.update", json!({"index": index}));
        f.assert_dismissed();
        app.run("edit.undo", json!({})).unwrap();
        assert_eq!(photo(&app, id), initial);
        assert_eq!(app.session.selection, selection);
    }
}

#[test]
fn history_before_on_both_sides_of_an_older_cursor_never_restores_or_prunes() {
    let (mut app, id) = fixture();
    let initial = photo(&app, id);
    let cursor = initial.history.len() - 2;
    let mut f = Frames::new();
    f.settle(|ui| activity(&mut app, ui, id));
    let at = f.rect(&format!("history:{cursor}")).center();
    f.click(at, PointerButton::Primary, |ui| activity(&mut app, ui, id));
    let baseline = photo(&app, id);
    assert_eq!(baseline.history, initial.history);
    assert_eq!(baseline.history_index(), Some(cursor));
    let selection = app.session.selection.clone();
    let others = other_photos(&app, id);
    let undo = app.session.undo.len();
    let indices = [cursor - 1, baseline.history.len() - 1];
    assert_ne!(baseline.history[indices[0]].settings, baseline.history[indices[1]].settings);
    for index in indices {
        let journal = app.session.journal.len();
        history_menu(&mut f, &mut app, id, index);
        let at = f.rect(&format!("historyMenu:{index}:before")).center();
        f.click(at, PointerButton::Primary, |ui| activity(&mut app, ui, id));
        assert_eq!(app.session.before.get(&id), Some(&baseline.history[index].settings));
        assert_eq!(photo(&app, id), baseline, "Before cannot restore an older or undone step, move the cursor or prune future history");
        assert_eq!(other_photos(&app, id), others);
        assert_eq!(app.session.selection, selection);
        assert_eq!(app.session.undo.len(), undo);
        assert_one_command(&app, journal, "beforeAfter.setBefore", json!({"source":"history", "index":index}));
        f.assert_dismissed();
    }
}

#[test]
fn history_actions_unforced_tab_enter_space_cancel_exit_and_real_reopen() {
    for action in ["restore", "update", "before", "history-before"] {
        for key in [Key::Enter, Key::Space] {
            let (mut app, id) = fixture();
            let initial = photo(&app, id);
            let others = other_photos(&app, id);
            let selection = app.session.selection.clone();
            let baseline = document(&app);
            let undo = app.session.undo.len();
            let journal = app.session.journal.len();
            let panel = if action == "history-before" { Panel::History } else { Panel::Snapshots };
            let index = if action == "history-before" { initial.history.len() - 3 } else { 0 };
            let target = if action == "history-before" { format!("historyMenu:{index}:before") } else { format!("versionMenu:{index}:{action}") };
            let mut f = Frames::new();
            open_panel_menu(panel, &mut f, &mut app, id, index);
            f.tab_to(&target, |ui| draw_panel(panel, &mut app, ui, id));
            assert_eq!(document(&app), baseline, "natural keyboard entry is not activation");
            f.key(Key::Escape, |ui| draw_panel(panel, &mut app, ui, id));
            f.assert_dismissed();
            assert_eq!(document(&app), baseline);
            open_panel_menu(panel, &mut f, &mut app, id, index);
            f.tab_to(&target, |ui| draw_panel(panel, &mut app, ui, id));
            assert_eq!(document(&app), baseline, "real reopen cannot replay a canceled action");
            f.key(key, |ui| draw_panel(panel, &mut app, ui, id));
            f.assert_dismissed();
            match action {
                "restore" => {
                    assert_eq!(photo(&app, id).develop, initial.versions[0].settings);
                    assert_eq!(photo(&app, id).versions, initial.versions);
                    assert_eq!(app.session.undo.len(), undo + 1);
                    assert_one_command(&app, journal, "version.restore", json!({"index":0}));
                    app.run("edit.undo", json!({})).unwrap();
                    assert_eq!(photo(&app, id), initial);
                }
                "update" => {
                    let mut expected = initial.clone();
                    expected.versions[0].settings = initial.develop.clone();
                    assert_eq!(photo(&app, id), expected);
                    assert_eq!(app.session.undo.len(), undo + 1);
                    assert_one_command(&app, journal, "version.update", json!({"index":0}));
                    app.run("edit.undo", json!({})).unwrap();
                    assert_eq!(photo(&app, id), initial);
                }
                _ => {
                    let settings = if action == "history-before" { &initial.history[index].settings } else { &initial.versions[0].settings };
                    assert_eq!(app.session.before.get(&id), Some(settings));
                    assert_eq!(photo(&app, id), initial);
                    assert_eq!(app.session.undo.len(), undo);
                    let params = if action == "history-before" { json!({"source":"history", "index":index}) } else { json!({"source":"version", "name":initial.versions[0].name}) };
                    assert_one_command(&app, journal, "beforeAfter.setBefore", params);
                }
            }
            assert_eq!(other_photos(&app, id), others);
            assert_eq!(app.session.selection, selection);
        }
    }
}

#[test]
fn history_already_open_disabled_popup_refuses_pointer_and_keys_then_recovers() {
    for panel in [Panel::Snapshots, Panel::History] {
        let (mut app, id) = fixture();
        let initial = photo(&app, id);
        let index = match panel { Panel::Snapshots => 0, Panel::History => initial.history.len() - 3 };
        let target = match panel { Panel::Snapshots => "versionMenu:0:restore".to_owned(), Panel::History => format!("historyMenu:{index}:before") };
        let mut f = Frames::new();
        open_panel_menu(panel, &mut f, &mut app, id, index);
        f.tab_to(&target, |ui| draw_panel(panel, &mut app, ui, id));
        let at = f.rect(&target).center();
        let baseline = document(&app);
        f.enabled = false;
        f.click(at, PointerButton::Primary, |ui| draw_panel(panel, &mut app, ui, id));
        f.key(Key::Enter, |ui| draw_panel(panel, &mut app, ui, id));
        f.key(Key::Space, |ui| draw_panel(panel, &mut app, ui, id));
        assert_eq!(document(&app), baseline);
        f.enabled = true;
        f.key(Key::Escape, |ui| draw_panel(panel, &mut app, ui, id));
        f.assert_dismissed();
        open_panel_menu(panel, &mut f, &mut app, id, index);
        assert_eq!(document(&app), baseline);
        let at = f.rect(&target).center();
        f.click(at, PointerButton::Primary, |ui| draw_panel(panel, &mut app, ui, id));
        match panel {
            Panel::Snapshots => assert_eq!(photo(&app, id).develop, initial.versions[0].settings),
            Panel::History => assert_eq!(app.session.before.get(&id), Some(&initial.history[index].settings)),
        }
        f.assert_dismissed();
    }
}

#[test]
fn history_measured_release_is_inert_then_the_same_row_and_action_recover() {
    for panel in [Panel::Snapshots, Panel::History] {
        for pass in [Pass::Sizing, Pass::Measure] {
            let (mut app, id) = fixture();
            let initial = photo(&app, id);
            let index = match panel { Panel::Snapshots => 0, Panel::History => initial.history.len() - 3 };
            let target = match panel { Panel::Snapshots => "versionMenu:0:restore".to_owned(), Panel::History => format!("historyMenu:{index}:before") };
            let row = match panel { Panel::Snapshots => "version:0".to_owned(), Panel::History => format!("history:{index}") };
            let mut f = Frames::new();
            open_panel_menu(panel, &mut f, &mut app, id, index);
            let owner = f.hit(&row).id;
            let at = f.rect(&target).center();
            let baseline = document(&app);
            f.frame(vec![Event::PointerMoved(at)], |ui| draw_panel(panel, &mut app, ui, id));
            f.frame(vec![Event::PointerButton { pos:at, button:PointerButton::Primary, pressed:true, modifiers:Modifiers::NONE }], |ui| draw_panel(panel, &mut app, ui, id));
            assert_eq!(document(&app), baseline, "ordinary action press is not its release");
            f.pass = pass;
            f.frame(vec![Event::PointerButton { pos:at, button:PointerButton::Primary, pressed:false, modifiers:Modifiers::NONE }], |ui| draw_panel(panel, &mut app, ui, id));
            assert_eq!(document(&app), baseline);
            assert!(!f.rects.iter().any(|(id, _)| id.starts_with("version:") || id.starts_with("history:") || id.contains("Menu:")));
            assert!(!f.hits.iter().any(|(id, _)| id.starts_with("version:") || id.starts_with("history:") || id.contains("Menu:")));
            f.pass = Pass::Real;
            f.settle(|ui| draw_panel(panel, &mut app, ui, id));
            assert_eq!(document(&app), baseline, "speculative release cannot replay later");
            assert_eq!(f.hit(&row).id, owner);
            f.key(Key::Escape, |ui| draw_panel(panel, &mut app, ui, id));
            open_panel_menu(panel, &mut f, &mut app, id, index);
            assert_eq!(f.hit(&row).id, owner);
            let at = f.rect(&target).center();
            let journal = app.session.journal.len();
            let undo = app.session.undo.len();
            f.click(at, PointerButton::Primary, |ui| draw_panel(panel, &mut app, ui, id));
            match panel {
                Panel::Snapshots => {
                    assert_eq!(photo(&app, id).develop, initial.versions[0].settings);
                    assert_eq!(app.session.undo.len(), undo + 1);
                    assert_one_command(&app, journal, "version.restore", json!({"index":0}));
                    app.run("edit.undo", json!({})).unwrap();
                    assert_eq!(photo(&app, id), initial);
                }
                Panel::History => {
                    assert_eq!(photo(&app, id), initial);
                    assert_eq!(app.session.before.get(&id), Some(&initial.history[index].settings));
                    assert_eq!(app.session.undo.len(), undo);
                    assert_one_command(&app, journal, "beforeAfter.setBefore", json!({"source":"history", "index":index}));
                }
            }
            f.assert_dismissed();
        }
    }
}

#[test]
fn history_stale_photo_and_other_panel_cannot_retarget_an_existing_popup() {
    let (mut app, id) = fixture();
    let other = app.session.catalog.photos().find(|p| p.id != id).unwrap().id;
    let template = photo(&app, id);
    app.session.catalog.apply(lightcraft_catalog::Op::SetVersions { id:other, versions:template.versions.clone() }).unwrap();
    app.session.catalog.apply(lightcraft_catalog::Op::SetHistory { id:other, history:template.history.clone() }).unwrap();
    let mut f = Frames::new();
    version_menu(&mut f, &mut app, id, 0);
    let owner = f.hit("version:0").id;
    let at = f.rect("versionMenu:0:restore").center();
    app.run("library.select", json!({"ids":[other.0,id.0], "active":other.0})).unwrap();
    let baseline = document(&app);
    f.click(at, PointerButton::Primary, |ui| versions(&mut app, ui, id));
    f.key(Key::Enter, |ui| versions(&mut app, ui, id));
    assert_eq!(document(&app), baseline, "a stale panel cannot run an active-only command on the new target");
    f.key(Key::Escape, |ui| versions(&mut app, ui, id));
    f.settle(|ui| activity(&mut app, ui, other));
    f.assert_dismissed();
    assert_eq!(document(&app), baseline);
    f.settle(|ui| versions(&mut app, ui, other));
    assert!(!f.hits.iter().any(|(name, hit)| name.starts_with("version:") && hit.id == owner));
    f.assert_dismissed();
    assert_eq!(document(&app), baseline, "tab/photo changes cannot carry an old row action to another photo");

    app.run("library.select", json!({"ids":[id.0,other.0], "active":id.0})).unwrap();
    let baseline = document(&app);
    version_menu(&mut f, &mut app, id, 0);
    f.settle(|ui| activity(&mut app, ui, id)); // Leave while the snapshot popup is still open.
    f.assert_dismissed();
    assert_eq!(document(&app), baseline);
    f.settle(|ui| versions(&mut app, ui, id));
    f.assert_dismissed();
    assert_eq!(document(&app), baseline, "returning to a tab cannot reopen or activate its old row action");
    version_menu(&mut f, &mut app, id, 2); // An ordinary new pointer entry must still work.
    f.key(Key::Escape, |ui| versions(&mut app, ui, id));
    f.assert_dismissed();
    assert_eq!(document(&app), baseline);
}


#[test]
fn history_sdk_context_layer_keeps_carry_move_and_no_settle_escape_reopen_on_the_same_target() {
    for panel in [Panel::Snapshots, Panel::History] {
        let (mut app, id) = fixture();
        let initial = photo(&app, id);
        let others = other_photos(&app, id);
        let selection = app.session.selection.clone();
        let baseline = document(&app);
        let undo = app.session.undo.len();
        let journal = app.session.journal.len();
        let index = match panel { Panel::Snapshots => 0, Panel::History => initial.history.len() - 3 };
        let row = match panel { Panel::Snapshots => "version:0".to_owned(), Panel::History => format!("history:{index}") };
        let target = match panel { Panel::Snapshots => "versionMenu:0:restore".to_owned(), Panel::History => format!("historyMenu:{index}:before") };
        let mut f = Frames::new();
        f.settle(|ui| draw_panel(panel, &mut app, ui, id));
        let bounds = f.rect(&row);
        let left = bounds.left_center() + vec2(8.0, 0.0);
        let right = bounds.right_center() - vec2(8.0, 0.0);

        crate::widgets::set_carrying(&f.ctx, true);
        f.click(right, PointerButton::Secondary, |ui| draw_panel(panel, &mut app, ui, id));
        f.assert_dismissed();
        assert_eq!(document(&app), baseline, "a carried photo cannot open or run a row action");
        crate::widgets::set_carrying(&f.ctx, false);
        f.click(right, PointerButton::Secondary, |ui| draw_panel(panel, &mut app, ui, id));
        let before_move = f.rect(&target);
        assert!(!egui::Popup::is_any_open(&f.ctx), "the SDK must own the popup boundary, not just its rows");
        f.click(left, PointerButton::Secondary, |ui| draw_panel(panel, &mut app, ui, id));
        assert!(f.rect(&target).left() < before_move.left() - 40.0, "another right-click moves the still-open context menu");
        assert_eq!(document(&app), baseline);
        f.key(Key::Tab, |ui| draw_panel(panel, &mut app, ui, id));
        f.tab_to(&target, |ui| draw_panel(panel, &mut app, ui, id));
        eprintln!("History first-Tab boundary {target}: {:?}", f.key_trace);

        // No idle/settle frame between cancel and the next genuine secondary press/release.
        f.frame(vec![Event::Key { key: Key::Escape, physical_key: Some(Key::Escape), pressed: true, repeat: false, modifiers: Modifiers::NONE }], |ui| draw_panel(panel, &mut app, ui, id));
        assert_eq!(document(&app), baseline, "Escape cannot replay the focused action");
        f.frame(vec![
            Event::Key { key: Key::Escape, physical_key: Some(Key::Escape), pressed: false, repeat: false, modifiers: Modifiers::NONE },
            Event::PointerMoved(right),
            Event::PointerButton { pos: right, button: PointerButton::Secondary, pressed: true, modifiers: Modifiers::NONE },
        ], |ui| draw_panel(panel, &mut app, ui, id));
        f.frame(vec![Event::PointerButton { pos: right, button: PointerButton::Secondary, pressed: false, modifiers: Modifiers::NONE }], |ui| draw_panel(panel, &mut app, ui, id));
        assert!(!egui::Popup::is_any_open(&f.ctx));
        assert_eq!(document(&app), baseline, "immediate reopening is not activation or restoration");
        f.tab_to(&target, |ui| draw_panel(panel, &mut app, ui, id));
        f.key(Key::Enter, |ui| draw_panel(panel, &mut app, ui, id));
        f.assert_dismissed();
        match panel {
            Panel::Snapshots => {
                assert_eq!(photo(&app, id).develop, initial.versions[0].settings);
                assert_eq!(photo(&app, id).versions, initial.versions);
                assert_eq!(app.session.undo.len(), undo + 1);
                assert_one_command(&app, journal, "version.restore", json!({"index": 0}));
                app.run("edit.undo", json!({})).unwrap();
                assert_eq!(photo(&app, id), initial);
            }
            Panel::History => {
                assert_eq!(photo(&app, id), initial);
                assert_eq!(app.session.before.get(&id), Some(&initial.history[index].settings));
                assert_eq!(app.session.undo.len(), undo);
                assert_one_command(&app, journal, "beforeAfter.setBefore", json!({"source": "history", "index": index}));
            }
        }
        assert_eq!(other_photos(&app, id), others);
        assert_eq!(app.session.selection, selection);
    }
}


#[test]
fn history_popup_publication_tracks_completed_pass_on_pointer_cancel_and_activate() {
    for panel in [Panel::Snapshots, Panel::History] {
        let (mut app, id) = fixture();
        let initial = photo(&app, id);
        let baseline = document(&app);
        let others = other_photos(&app, id);
        let selection = app.session.selection.clone();
        let undo = app.session.undo.len();
        let journal = app.session.journal.len();
        let index = match panel { Panel::Snapshots => 0, Panel::History => initial.history.len() - 3 };
        let target = match panel { Panel::Snapshots => "versionMenu:0:restore".to_owned(), Panel::History => format!("historyMenu:{index}:before") };
        let mut f = Frames::new();
        open_panel_menu(panel, &mut f, &mut app, id, index);
        assert!(craft::ui::runtime::overlay::any_open(&f.ctx, craft::ui::runtime::overlay::OpenFilter::Keys));
        assert!(!egui::Popup::is_any_open(&f.ctx));
        let mut passes = 0;
        f.frame(vec![], |ui| {
            draw_panel(panel, &mut app, ui, id);
            passes += 1;
            if passes == 1 {
                ui.ctx().request_discard("History publication: discard an actual rendered popup pass");
            }
        });
        assert!(passes >= 2, "the real host lifecycle must repeat the discarded pass");
        f.hit(&target); // Exactly one current owner, not one copy per discarded render.
        assert_eq!(document(&app), baseline);
        eprintln!("History publication open {target}: {:?}", f.publication);

        f.escape(|ui| draw_panel(panel, &mut app, ui, id));
        eprintln!("History publication cancelled {target}: sdk_keys_open={} passes={:?}", craft::ui::runtime::overlay::any_open(&f.ctx, craft::ui::runtime::overlay::OpenFilter::Keys), f.publication);
        f.assert_dismissed();
        assert!(!craft::ui::runtime::overlay::any_open(&f.ctx, craft::ui::runtime::overlay::OpenFilter::Keys));
        assert_eq!(document(&app), baseline, "cancel may neither edit nor replay an action");

        open_panel_menu(panel, &mut f, &mut app, id, index);
        f.hit(&target);
        let at = f.rect(&target).center();
        f.click(at, PointerButton::Primary, |ui| draw_panel(panel, &mut app, ui, id));
        match panel {
            Panel::Snapshots => {
                assert_eq!(photo(&app, id).develop, initial.versions[index].settings);
                assert_eq!(photo(&app, id).versions, initial.versions);
                assert_eq!(app.session.undo.len(), undo + 1);
                assert_one_command(&app, journal, "version.restore", json!({"index": index}));
            }
            Panel::History => {
                assert_eq!(photo(&app, id), initial);
                assert_eq!(app.session.before.get(&id), Some(&initial.history[index].settings));
                assert_eq!(app.session.undo.len(), undo);
                assert_one_command(&app, journal, "beforeAfter.setBefore", json!({"source": "history", "index": index}));
            }
        }
        eprintln!("History publication activated {target}: sdk_keys_open={} passes={:?}", craft::ui::runtime::overlay::any_open(&f.ctx, craft::ui::runtime::overlay::OpenFilter::Keys), f.publication);
        f.assert_dismissed();
        assert!(!craft::ui::runtime::overlay::any_open(&f.ctx, craft::ui::runtime::overlay::OpenFilter::Keys));
        assert_eq!(other_photos(&app, id), others);
        assert_eq!(app.session.selection, selection);
        if matches!(panel, Panel::Snapshots) {
            app.run("edit.undo", json!({})).unwrap();
            assert_eq!(photo(&app, id), initial);
            assert_eq!(app.session.undo.len(), undo);
        }
    }
}
