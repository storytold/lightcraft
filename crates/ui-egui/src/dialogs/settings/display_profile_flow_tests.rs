//! Actual Monitor profile section with controlled service callbacks; no chooser or profile file is opened.
use super::*;
use crate::headless::HeadlessView;
use egui::{Event, Key, Modifiers, PointerButton, Rect};
use lightcraft_catalog::{MemStore, Op, Photo, Source};
use lightcraft_engine::{Session, library::LibraryStores};
use std::collections::VecDeque;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

const NATIVE_PRESENTATION: bool = false;
const CHOOSE: &str = "button:settingsDisplayProfile";
const RESET: &str = "button:settingsDisplayProfileNone";
const ORIGINAL: &str = "/controlled-only/original.icc";
const CHOSEN: &str = "/controlled-only/chosen.icm";
struct Fixture {
    app: LightcraftApp,
    ctx: egui::Context,
    files: MemStore,
    tick: u32,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    record: Option<Arc<craft::ui::runtime::inspect::Recorded>>,
    native: bool,
    disabled: bool,
    measure: bool,
    sizing: bool,
    original_locale: crate::i18n::Locale,
    picks: Arc<AtomicUsize>,
    screens: Arc<AtomicUsize>,
    queue: Arc<Mutex<VecDeque<Vec<String>>>>,
}
impl Fixture {
    fn new() -> Self {
        let original_locale = crate::i18n::language();
        crate::i18n::set_language(crate::i18n::Locale::En);
        let files = MemStore::new();
        let mut s = Session::new();
        s.open_library_in(
            LibraryStores {
                dir: "memory-monitor-profile".into(),
                catalog: Box::new(MemStore::new()),
                files: Box::new(files.clone()),
                on_disk: false,
            },
            false,
        )
        .unwrap();
        for name in ["A", "B"] {
            let id = s.catalog.alloc_photo_id();
            s.catalog
                .apply(Op::AddPhoto { photo: Box::new(Photo::new(id, Source::Demo { scene: 0 }, name, "JPEG", 8, 6, "2026-11-22T00:00:00")) })
                .unwrap();
        }
        let id = s.catalog.photos().next().unwrap().id;
        s.execute("library.select", &json!({"ids":[id]})).unwrap();
        let mut develop = (*s.develop_of(id).unwrap()).clone();
        develop.light.exposure = 0.5;
        s.set_develop(id, develop, "Unrelated edit").unwrap();
        let mut app = LightcraftApp::new(s, crate::Services { local_places: Some(vec![]), png: None, ..Default::default() });
        app.ui.settings.display_profile = ORIGINAL.into();
        app.display_error = Some("Controlled diagnostic; no profile load".into());
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut f = Self {
            app,
            ctx,
            files,
            tick: 0,
            rects: vec![],
            hits: vec![],
            record: None,
            native: NATIVE_PRESENTATION,
            disabled: false,
            measure: false,
            sizing: false,
            original_locale,
            picks: Arc::new(AtomicUsize::new(0)),
            screens: Arc::new(AtomicUsize::new(0)),
            queue: Arc::new(Mutex::new(VecDeque::new())),
        };
        f.picker(true);
        f.idle();
        f
    }
    fn picker(&mut self, available: bool) {
        let count = self.picks.clone();
        let queue = self.queue.clone();
        self.app.services.pick_display_profile = available.then(|| {
            Box::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
                queue.lock().unwrap().pop_front().unwrap_or_default()
            }) as crate::services::PickFiles
        });
    }
    fn screen(&mut self, available: bool) {
        let count = self.screens.clone();
        self.app.services.screen_profile = available.then(|| {
            Box::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
                None
            }) as crate::services::ScreenProfileFn
        });
    }
    fn offer(&mut self, paths: &[&str]) {
        self.queue.lock().unwrap().push_back(paths.iter().map(|s| (*s).to_owned()).collect());
    }
    fn calls(&self) -> usize {
        self.picks.load(Ordering::SeqCst)
    }
    fn safe(&self) {
        assert!(self.app.session.library_folder.is_none());
        assert_eq!(self.app.session.catalog.len(), 2);
        assert!(self.app.session.catalog.photos().all(|p| matches!(p.source, Source::Demo { .. })));
        assert!(self.app.session.media.rendered.disk().is_none());
        assert!(self.app.renderer.display().is_none());
        assert!(self.app.display_applied.is_none() && self.app.screen_profile.is_none() && self.app.screen_profile_read.is_none());
        assert_eq!(self.screens.load(Ordering::SeqCst), 0, "the real screen callback must not be invoked by settings");
        assert!(self.app.services.picker.is_none() && self.app.services.pick_files.is_none());
    }
    fn state(&self) -> Value {
        let s = &self.app.session;
        json!({"settings":self.app.ui.settings,"catalog":s.catalog,"selection":s.selection,"filter":s.filter,"undo":format!("{:?}",s.undo),"redo":format!("{:?}",s.redo),"commits":s.commits(),"journal":s.journal,"prefs":self.files.get("prefs.json"),"renderer":self.app.renderer.display().map(|d|d.describe()),"displayApplied":self.app.display_applied,"displayError":self.app.display_error,"screenProfile":self.app.screen_profile,"screenRead":self.app.screen_profile_read})
    }
    fn only_profile(&self, before: &Value, path: &str, calls: usize) {
        let mut expected = before.clone();
        expected["settings"]["displayProfile"] = json!(path);
        assert_eq!(self.state(), expected);
        assert_eq!(self.calls(), calls);
        self.safe();
    }

    fn frame(&mut self, mut events: Vec<Event>) {
        self.safe();
        crate::widgets::take_registries(&self.ctx);
        self.tick += 1;
        let modifiers = events
            .iter()
            .find_map(|e| match e {
                Event::Key { modifiers, .. } | Event::PointerButton { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        events.insert(0, Event::ModifiersChanged(modifiers));
        let raw = HeadlessView::raw_input(egui::vec2(1200.0, 1000.0), 1.0, f64::from(self.tick) / 60.0, events);
        let (native, disabled, measure, sizing) = (self.native, self.disabled, self.measure, self.sizing);
        let app = &mut self.app;
        let ctx = self.ctx.clone();
        let mut last = None;
        let mut out = ctx.run_ui(raw, |root| {
            let (_, record) = craft::ui::runtime::inspect::record(root, egui::Id::unique("monitor-profile-workflow-record"), |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.set_max_width(620.0);
                    let mut draw = |ui: &mut egui::Ui| {
                        let t = Tokens::get(ui.ctx());
                        if native {
                            (|| display_tab(app, ui, &t))();
                        } else {
                            display_tab(app, ui, &t);
                        }
                    };
                    ui.add_enabled_ui(!disabled, |ui| {
                        if sizing {
                            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.available_rect_before_wrap()).sizing_pass().invisible());
                            draw(&mut child);
                        } else if measure {
                            craft::ui::runtime::measure::run(&ctx, || draw(ui));
                        } else {
                            draw(ui);
                        }
                    });
                });
            });
            let registry = crate::widgets::take_registries(&ctx);
            for (name, _) in &registry.0 {
                assert_eq!(registry.0.iter().filter(|(other, _)| other == name).count(), 1, "duplicate alias in one actual callback: {name}");
            }
            last = Some((registry, record));
        });
        let ((rects, hits), record) = last.unwrap();
        self.rects = rects;
        self.hits = hits;
        self.record = record;
        out.textures_delta.clear();
        out.platform_output.commands.clear();
        out.viewport_output.clear();
        out.shapes.clear();
        self.safe();
    }

    fn idle(&mut self) {
        for _ in 0..3 {
            self.frame(vec![]);
        }
    }

    fn rect(&self, alias: &str) -> Rect {
        self.rects
            .iter()
            .find(|(n, _)| n == alias)
            .unwrap_or_else(|| panic!("missing {alias}; actual {:?}", self.rects.iter().map(|(n, _)| n).collect::<Vec<_>>()))
            .1
    }

    fn has(&self, alias: &str) -> bool {
        self.rects.iter().any(|(n, _)| n == alias)
    }

    fn pointer(&mut self, at: egui::Pos2, button: PointerButton, pressed: bool) {
        self.frame(vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button, pressed, modifiers: Modifiers::NONE }]);
    }

    fn click_button(&mut self, alias: &str, button: PointerButton) {
        self.tick += 30;
        let at = self.rect(alias).center();
        self.frame(vec![Event::PointerMoved(at)]);
        self.pointer(at, button, true);
        self.pointer(at, button, false);
        self.idle();
    }

    fn click(&mut self, alias: &str) {
        self.click_button(alias, PointerButton::Primary);
    }

    fn key_mod(&mut self, key: Key, modifiers: Modifiers) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }]);
        }
        self.idle();
    }

    fn key(&mut self, key: Key) {
        self.key_mod(key, Modifiers::NONE);
    }

    fn focused(&self, alias: &str) -> bool {
        let Some(native) = self.ctx.memory(|m| m.focused()).and_then(|id| self.ctx.read_response(id)) else {
            return false;
        };
        if !native.has_focus() {
            return false;
        }
        if let Some(rec) = craft::ui::runtime::focus::focused_rec(&self.ctx) {
            let Some(hit) = self.hits.iter().find(|(n, _)| n == alias).map(|(_, h)| h) else {
                return false;
            };
            return rec.stop.part.is_none()
                && rec.stop.id == hit.id
                && rec.stop.root == native.id
                && native.layer_id == hit.layer_id
                && self.record.as_ref().is_some_and(|r| {
                    r.elements
                        .iter()
                        .filter(|e| e.id == hit.id && e.part.is_none() && e.stop.is_some() && e.facts.as_ref().is_some_and(|a| !a.disabled))
                        .count()
                        == 1
                });
        }
        if alias.starts_with("field:") {
            return native.id == crate::text_field::id(alias);
        }
        native.rect == self.rect(alias) && native.enabled()
    }

    fn tab_to(&mut self, alias: &str) {
        for _ in 0..64 {
            if self.focused(alias) {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("original bounded natural Tab did not reach {alias}");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        crate::i18n::set_language(self.original_locale);
    }
}
#[test]
fn monitor_profile_real_choose_uses_controlled_first_path_and_changes_only_the_original_setting() {
    let mut f = Fixture::new();
    let before = f.state();
    f.offer(&[CHOSEN]);
    f.click(CHOOSE);
    f.only_profile(&before, CHOSEN, 1);
}
#[test]
fn monitor_profile_real_cancel_preserves_every_setting_renderer_history_and_preferences() {
    let mut f = Fixture::new();
    let before = f.state();
    f.offer(&[]);
    f.click(CHOOSE);
    assert_eq!(f.state(), before);
    assert_eq!(f.calls(), 1);
}
#[test]
fn monitor_profile_multiple_results_keep_the_original_first_path_without_trimming_or_loading() {
    let mut f = Fixture::new();
    let before = f.state();
    let first = "  /controlled-only/space profile.icc  ";
    f.offer(&[first, CHOSEN]);
    f.click(CHOOSE);
    f.only_profile(&before, first, 1);
}
#[test]
fn monitor_profile_empty_first_result_is_not_replaced_by_a_later_path() {
    let mut f = Fixture::new();
    let before = f.state();
    f.offer(&["", CHOSEN]);
    f.click(CHOOSE);
    f.only_profile(&before, "", 1);
}
#[test]
fn monitor_profile_screen_reset_clears_only_the_override_without_querying_the_screen() {
    let mut f = Fixture::new();
    f.screen(true);
    f.idle();
    let before = f.state();
    f.click(RESET);
    f.only_profile(&before, "", 0);
}
#[test]
fn monitor_profile_srgb_reset_clears_only_the_override_without_any_host_callback() {
    let mut f = Fixture::new();
    f.screen(false);
    let before = f.state();
    f.click(RESET);
    f.only_profile(&before, "", 0);
}
#[test]
fn monitor_profile_unavailable_choose_is_inert_and_reset_remains_naturally_keyboard_reachable() {
    let mut f = Fixture::new();
    f.picker(false);
    f.idle();
    let before = f.state();
    f.click(CHOOSE);
    assert_eq!(f.state(), before);
    assert_eq!(f.calls(), 0);
    f.tab_to(RESET);
    f.key(Key::Enter);
    f.only_profile(&before, "", 0);
}
#[test]
fn monitor_profile_empty_override_reset_is_disabled_then_fresh_choose_still_works() {
    let mut f = Fixture::new();
    f.app.ui.settings.display_profile.clear();
    f.idle();
    let before = f.state();
    f.click(RESET);
    assert_eq!(f.state(), before);
    assert_eq!(f.calls(), 0);
    f.offer(&[CHOSEN]);
    f.tab_to(CHOOSE);
    f.key(Key::Enter);
    f.only_profile(&before, CHOSEN, 1);
}
#[test]
fn monitor_profile_both_unavailable_controls_ignore_real_pointer_tab_and_enter_then_new_input_works() {
    let mut f = Fixture::new();
    f.app.ui.settings.display_profile.clear();
    f.picker(false);
    f.idle();
    let before = f.state();
    f.click(CHOOSE);
    f.click(RESET);
    for _ in 0..6 {
        f.key(Key::Tab);
        f.key(Key::Enter);
    }
    assert_eq!(f.state(), before);
    assert_eq!(f.calls(), 0);
    f.picker(true);
    f.offer(&[CHOSEN]);
    f.idle();
    assert_eq!(f.state(), before);
    assert_eq!(f.calls(), 0);
    f.tab_to(CHOOSE);
    f.key(Key::Enter);
    f.only_profile(&before, CHOSEN, 1);
}
fn keyboard_choose(key: Key) {
    let mut f = Fixture::new();
    let before = f.state();
    f.offer(&[CHOSEN]);
    f.tab_to(CHOOSE);
    assert!(f.focused(CHOOSE));
    f.key(key);
    f.only_profile(&before, CHOSEN, 1);
}
#[test]
fn monitor_profile_natural_tab_enter_activates_the_real_chooser_button() {
    keyboard_choose(Key::Enter);
}
#[test]
fn monitor_profile_natural_tab_space_activates_the_real_chooser_button() {
    keyboard_choose(Key::Space);
}
fn keyboard_reset(screen: bool, key: Key) {
    let mut f = Fixture::new();
    f.screen(screen);
    f.idle();
    let before = f.state();
    f.tab_to(RESET);
    assert!(f.focused(RESET));
    f.key(key);
    f.only_profile(&before, "", 0);
}
#[test]
fn monitor_profile_natural_tab_enter_resets_to_screen_without_calling_its_service() {
    keyboard_reset(true, Key::Enter);
}
#[test]
fn monitor_profile_natural_tab_space_resets_to_srgb() {
    keyboard_reset(false, Key::Space);
}
#[test]
fn monitor_profile_disabled_parent_blocks_pointer_and_keyboard_without_delayed_chooser() {
    let mut f = Fixture::new();
    f.offer(&[CHOSEN]);
    let before = f.state();
    f.disabled = true;
    f.idle();
    f.click(CHOOSE);
    f.click(RESET);
    for _ in 0..6 {
        f.key(Key::Tab);
        f.key(Key::Enter);
    }
    assert_eq!(f.state(), before);
    assert_eq!(f.calls(), 0);
    f.disabled = false;
    f.idle();
    assert_eq!(f.state(), before);
    assert_eq!(f.calls(), 0);
    f.tab_to(CHOOSE);
    f.key(Key::Enter);
    f.only_profile(&before, CHOSEN, 1);
}
fn speculative_choose(sizing: bool) {
    let mut f = Fixture::new();
    let before = f.state();
    f.offer(&[CHOSEN]);
    let at = f.rect(CHOOSE).center();
    f.measure = !sizing;
    f.sizing = sizing;
    f.frame(vec![Event::PointerMoved(at)]);
    f.pointer(at, PointerButton::Primary, true);
    f.pointer(at, PointerButton::Primary, false);
    assert_eq!(f.state(), before);
    assert_eq!(f.calls(), 0);
    f.measure = false;
    f.sizing = false;
    f.idle();
    assert_eq!(f.state(), before);
    assert_eq!(f.calls(), 0);
    f.click(CHOOSE);
    f.only_profile(&before, CHOSEN, 1);
}
#[test]
fn monitor_profile_measured_choose_is_inert_and_never_replays_a_host_callback() {
    speculative_choose(false);
}
#[test]
fn monitor_profile_sizing_choose_is_inert_and_never_replays_a_host_callback() {
    speculative_choose(true);
}
fn speculative_reset(sizing: bool) {
    let mut f = Fixture::new();
    f.screen(true);
    f.idle();
    let before = f.state();
    let at = f.rect(RESET).center();
    f.measure = !sizing;
    f.sizing = sizing;
    f.frame(vec![Event::PointerMoved(at)]);
    f.pointer(at, PointerButton::Primary, true);
    f.pointer(at, PointerButton::Primary, false);
    assert_eq!(f.state(), before);
    assert_eq!(f.calls(), 0);
    f.measure = false;
    f.sizing = false;
    f.idle();
    assert_eq!(f.state(), before);
    f.click(RESET);
    f.only_profile(&before, "", 0);
}
#[test]
fn monitor_profile_measured_reset_is_inert_and_does_not_clear_the_override_later() {
    speculative_reset(false);
}
#[test]
fn monitor_profile_sizing_reset_is_inert_and_does_not_clear_the_override_later() {
    speculative_reset(true);
}
#[test]
fn monitor_profile_hover_never_opens_a_chooser_or_changes_the_override() {
    let mut f = Fixture::new();
    let before = f.state();
    for alias in [CHOOSE, RESET] {
        let at = f.rect(alias).center();
        f.frame(vec![Event::PointerMoved(at)]);
        f.idle();
    }
    assert_eq!(f.state(), before);
    assert_eq!(f.calls(), 0);
}
#[test]
fn monitor_profile_choose_keeps_real_document_undo_and_redo_independent_of_the_override() {
    let mut f = Fixture::new();
    f.offer(&[CHOSEN]);
    f.click(CHOOSE);
    let id = f.app.session.active().unwrap();
    assert_eq!(f.app.session.develop_of(id).unwrap().light.exposure, 0.5);
    f.app.run("edit.undo", json!({})).unwrap();
    assert_eq!(f.app.session.develop_of(id).unwrap().light.exposure, 0.0);
    assert_eq!(f.app.ui.settings.display_profile, CHOSEN);
    f.app.run("edit.redo", json!({})).unwrap();
    assert_eq!(f.app.session.develop_of(id).unwrap().light.exposure, 0.5);
    assert_eq!(f.app.ui.settings.display_profile, CHOSEN);
    assert_eq!(f.calls(), 1);
}

impl Fixture {
    fn sdk_owner(&self, alias: &str, label: &str, disabled: bool) -> egui::Id {
        assert_eq!(self.rects.iter().filter(|(n, _)| n == alias).count(), 1);
        let hits = self.hits.iter().filter(|(n, _)| n == alias).collect::<Vec<_>>();
        assert_eq!(hits.len(), 1);
        let hit = hits[0].1;
        let nodes = self.record.as_ref().unwrap().elements.iter().filter(|e| e.id == hit.id && e.part.is_none()).collect::<Vec<_>>();
        assert_eq!(nodes.len(), 1);
        let e = nodes[0];
        assert_eq!(e.component, "Button");
        let a = e.facts.as_ref().unwrap();
        assert_eq!(a.role, "Button");
        assert_eq!(a.name.as_deref(), Some(label));
        assert_eq!(a.disabled, disabled);
        if !disabled {
            assert!(a.interactive && e.stop.is_some());
        }
        let native = self.ctx.read_response(hit.id).unwrap();
        assert_eq!(native.layer_id, hit.layer_id);
        hit.id
    }
}
#[test]
fn monitor_profile_sdk_buttons_have_unique_public_owners_roles_names_and_real_stops() {
    let f = Fixture::new();
    assert!(!f.native);
    let a = f.sdk_owner(CHOOSE, "Choose Profile…", false);
    let b = f.sdk_owner(RESET, "Use sRGB", false);
    assert_ne!(a, b);
}
#[test]
fn monitor_profile_sdk_reset_identity_survives_screen_capability_and_real_locale_changes() {
    let mut f = Fixture::new();
    assert!(!f.native);
    let before = f.state();
    let a = f.sdk_owner(CHOOSE, "Choose Profile…", false);
    let b = f.sdk_owner(RESET, "Use sRGB", false);
    f.screen(true);
    f.idle();
    assert_eq!(f.sdk_owner(RESET, "Use Screen Profile", false), b);
    crate::i18n::set_language(crate::i18n::Locale::De);
    f.idle();
    assert_eq!(f.sdk_owner(CHOOSE, crate::i18n::tr("Choose Profile…"), false), a);
    assert_eq!(f.sdk_owner(RESET, crate::i18n::tr("Use Screen Profile"), false), b);
    crate::i18n::set_language(crate::i18n::Locale::En);
    f.screen(false);
    f.idle();
    assert_eq!(f.sdk_owner(RESET, "Use sRGB", false), b);
    assert_eq!(f.state(), before);
    assert_eq!(f.calls(), 0);
}
#[test]
fn monitor_profile_sdk_unavailable_actions_publish_disabled_semantics_without_a_focus_owner() {
    let mut f = Fixture::new();
    assert!(!f.native);
    f.picker(false);
    f.app.ui.settings.display_profile.clear();
    f.idle();
    let a = f.sdk_owner(CHOOSE, "Choose Profile…", true);
    let b = f.sdk_owner(RESET, "Use sRGB", true);
    for _ in 0..6 {
        f.key(Key::Tab);
        if let Some(rec) = craft::ui::runtime::focus::focused_rec(&f.ctx) {
            assert_ne!(rec.stop.id, a);
            assert_ne!(rec.stop.id, b);
        }
    }
    assert_eq!(f.calls(), 0);
}
#[test]
fn monitor_profile_sdk_speculative_draw_has_no_live_alias_or_stop_and_restores_the_same_owners() {
    let mut f = Fixture::new();
    assert!(!f.native);
    let before = f.state();
    let a = f.sdk_owner(CHOOSE, "Choose Profile…", false);
    let b = f.sdk_owner(RESET, "Use sRGB", false);
    for sizing in [false, true] {
        f.measure = !sizing;
        f.sizing = sizing;
        f.frame(vec![]);
        assert!(f.rects.is_empty() && f.hits.is_empty());
        assert!(f.record.as_ref().is_none_or(|r| r.elements.iter().all(|e| e.stop.is_none() && e.facts.as_ref().is_none_or(|a| !a.interactive))));
        assert_eq!(f.state(), before);
        assert_eq!(f.calls(), 0);
        f.measure = false;
        f.sizing = false;
        f.idle();
        assert_eq!(f.sdk_owner(CHOOSE, "Choose Profile…", false), a);
        assert_eq!(f.sdk_owner(RESET, "Use sRGB", false), b);
    }
    f.offer(&[CHOSEN]);
    f.tab_to(CHOOSE);
    f.key(Key::Enter);
    f.only_profile(&before, CHOSEN, 1);
}
