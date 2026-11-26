//! Actual appearance-button owner, installed SDK, and raw headless pointer/keyboard input.
//! Dialog values witness successful UI effects, not raw app.run attempts: UI commands bypass the engine journal.
use super::appearance_button;
use crate::LightcraftApp;
use crate::state::{AppearanceMode, Dialog};
use crate::theme::{ThemeKind, Tokens};
use crate::widgets::{WidgetHit, take_registries};
use egui::{Context, Event, Key, Modifiers, PointerButton, Pos2, Rect, Vec2, pos2, vec2};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const SETTINGS: &str = "menu:appearanceSettings";

#[derive(Clone, Copy, Debug)]
enum Mode {
    Normal,
    Disabled,
    Sizing,
    Measure,
}

struct Frames {
    ctx: Context,
    app: LightcraftApp,
    mode: Mode,
    time: f64,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, WidgetHit)>,
    settings_response: Option<(egui::Id, egui::Sense, bool)>,
    nodes: HashMap<egui::accesskit::NodeId, egui::accesskit::Node>,
    asked: Arc<Mutex<Vec<crate::pick::PickRequest>>>,
    history: (usize, usize, usize, u64),
    favorite: String,
}

impl Frames {
    fn new() -> Self {
        let asked = Arc::new(Mutex::new(Vec::<crate::pick::PickRequest>::new()));
        let observed = Arc::clone(&asked);
        // All picker services are cancel-only before construction or the first draw.
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
        let favorite = ThemeKind::ALL.iter().find(|kind| !kind.is_dark()).unwrap().id().to_owned();
        app.ui.settings.favorite_themes = vec![favorite.clone()];
        app.ui.settings.appearance_mode = AppearanceMode::Dark;
        let history = (app.session.journal.len(), app.session.undo.len(), app.session.catalog.photos().count(), app.session.catalog.revision);
        let ctx = Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        ctx.enable_accesskit();
        app.install_framework(&ctx);
        let mut f = Self {
            ctx,
            app,
            mode: Mode::Normal,
            time: 0.0,
            rects: Vec::new(),
            hits: Vec::new(),
            settings_response: None,
            nodes: HashMap::new(),
            asked,
            history,
            favorite,
        };
        f.settle();
        f
    }

    fn settings(&self) -> Value {
        serde_json::to_value(&self.app.ui.settings).unwrap()
    }

    fn frame(&mut self, events: Vec<Event>) {
        let ctx = self.ctx.clone();
        self.time += 0.02;
        let raw = crate::headless::HeadlessView::raw_input(Vec2::new(600.0, 360.0), 1.0, self.time, events);
        let mode = self.mode;
        let mut output = ctx.run_ui(raw, |ui| {
            let _ = take_registries(&ctx);
            self.settings_response = None;
            let mut draw = |ui: &mut egui::Ui| {
                let t = Tokens::get(ui.ctx());
                let full = Rect::from_min_size(ui.max_rect().min, vec2(400.0, 40.0));
                // This is the same owner called by topbar::show, including its untouched favourite loop.
                appearance_button(&mut self.app, ui, &t, full, full.left() + 60.0);
            };
            match mode {
                Mode::Normal => draw(ui),
                Mode::Disabled => {
                    ui.disable();
                    draw(ui);
                }
                Mode::Sizing => {
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                    assert!(child.is_sizing_pass());
                    draw(&mut child);
                }
                Mode::Measure => craft::ui::runtime::measure::run(&ctx, || draw(ui)),
            }
            // read_response prefers this_pass. Read after the real row registers, before end_pass swaps the widget tables.
            (self.rects, self.hits) = take_registries(&ctx);
            self.settings_response = self.hits.iter().find(|(name, _)| name == SETTINGS).map(|(_, hit)| {
                let raw = ctx.read_response(hit.id).expect("current registered Settings row");
                (raw.id, raw.sense, raw.enabled())
            });
        });
        output.textures_delta.clear();
        self.nodes.clear();
        if let Some(update) = output.platform_output.accesskit_update {
            self.nodes.extend(update.nodes);
        }
    }

    fn settle(&mut self) {
        for _ in 0..3 {
            self.frame(Vec::new());
        }
    }

    fn idle(&mut self) {
        self.frame(vec![Event::PointerGone]);
        for _ in 0..12 {
            self.frame(Vec::new());
        }
    }

    fn rect(&self, id: &str) -> Rect {
        let targets: Vec<_> = self.rects.iter().filter(|(name, _)| name == id).collect();
        assert_eq!(targets.len(), 1, "one current target for {id}: {:?}", self.rects);
        targets[0].1
    }

    fn sdk_target(&self) -> WidgetHit {
        let hits: Vec<_> = self.hits.iter().filter(|(name, _)| name == SETTINGS).collect();
        assert_eq!(hits.len(), 1, "one real SDK response, not an automation hover probe");
        let hit = hits[0].1;
        let response = self.ctx.read_response(hit.id).unwrap();
        assert!(response.sense.senses_click());
        assert_eq!(response.rect, self.rect(SETTINGS));
        assert_eq!(response.layer_id, hit.layer_id);
        assert_eq!(response.interact_rect, hit.interact_rect);
        let label = crate::i18n::tr("Appearance Settings…");
        assert!(
            self.nodes.get(&hit.id.accesskit_id()).is_some_and(|node| node.role() == egui::accesskit::Role::MenuItem && node.label() == Some(label)),
            "installed SDK menu-item role/name, not the old egui button"
        );
        hit
    }

    fn click(&mut self, at: Pos2, button: PointerButton) {
        self.frame(vec![Event::PointerMoved(at)]);
        self.frame(vec![pointer(at, button, true)]);
        self.frame(vec![pointer(at, button, false)]);
        self.settle();
    }

    fn is_open(&self) -> bool {
        self.ctx.data(|d| d.get_temp::<bool>(egui::Id::unique(("top", "appearance")).with("appearance-menu-open"))).unwrap_or(false)
    }

    fn open(&mut self) {
        let before = self.settings();
        self.click(self.rect("icon:appearance").center(), PointerButton::Secondary);
        assert!(self.is_open());
        self.sdk_target();
        self.assert_no_action(&before);
    }

    fn key(&mut self, key: Key) {
        self.frame(vec![Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: Modifiers::NONE }]);
        self.frame(vec![Event::Key { key, physical_key: Some(key), pressed: false, repeat: false, modifiers: Modifiers::NONE }]);
        self.settle();
    }

    fn tab_to_settings(&mut self) {
        let wanted = self.sdk_target().id;
        self.frame(vec![Event::PointerGone]);
        // Always send a real Tab, even if opening the popup selected its first row.
        // No focus request, synthetic response, menu invocation, or app.run shortcut.
        let mut path = Vec::new();
        for _ in 0..4 {
            self.key(Key::Tab);
            let focused = craft::ui::runtime::focus::focused_rec(&self.ctx).map(|rec| rec.stop.id);
            path.push((focused, self.ctx.memory(|memory| memory.focused())));
            if focused == Some(wanted) {
                return;
            }
        }
        panic!("natural Tab did not reach Appearance Settings: {path:?}");
    }

    fn assert_history(&self) {
        assert_eq!(
            (
                self.app.session.journal.len(),
                self.app.session.undo.len(),
                self.app.session.catalog.photos().count(),
                self.app.session.catalog.revision,
            ),
            self.history
        );
        assert!(self.asked.lock().unwrap().is_empty(), "no picker or import side effect");
    }

    fn assert_no_action(&self, before: &Value) {
        assert_eq!(self.app.ui.dialog, None);
        assert_eq!(&self.settings(), before, "opening/dismissal/probes do not select a theme or cycle appearance");
        self.assert_history();
    }

    fn assert_settings(&self, before: &Value) {
        assert_eq!(self.app.ui.dialog, Some(Dialog::Settings { tab: "interface".into() }));
        assert_eq!(&self.settings(), before, "Settings is not view.appearance or view.theme");
        assert!(!self.is_open(), "the bound SDK menu closes on activation");
        self.assert_history();
    }
}

fn pointer(pos: Pos2, button: PointerButton, pressed: bool) -> Event {
    Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE }
}

#[test]
fn appearance_sdk_secondary_release_then_pointer_opens_exact_interface_settings() {
    let mut f = Frames::new();
    let before = f.settings();
    let anchor = f.rect("icon:appearance").center();
    f.frame(vec![Event::PointerMoved(anchor), pointer(anchor, PointerButton::Secondary, true)]);
    f.assert_no_action(&before);
    assert!(!f.is_open(), "a secondary press alone does not open the menu");
    f.frame(vec![pointer(anchor, PointerButton::Secondary, false)]);
    f.settle();
    f.sdk_target();
    f.assert_no_action(&before);
    f.click(f.rect(SETTINGS).center(), PointerButton::Primary);
    f.assert_settings(&before);
    // Consume the observed dialog, then watch the same owner for delayed/repeated effects.
    // This witnesses reopen effects, not multiple raw calls within one frame.
    f.app.ui.dialog = None;
    f.idle();
    f.assert_no_action(&before);
}

#[test]
fn appearance_sdk_natural_tab_enter_and_space_keep_settings_payload_and_dismissal() {
    for activation in [Key::Enter, Key::Space] {
        let mut f = Frames::new();
        let before = f.settings();
        f.open();
        f.tab_to_settings();
        f.key(activation);
        f.assert_settings(&before);
        assert_eq!(f.ctx.memory(|mem| mem.focused()), Some(egui::Id::unique(("top", "appearance"))), "keyboard close restores the opener");
        f.app.ui.dialog = None;
        f.idle();
        f.assert_no_action(&before);
    }
}

#[test]
fn appearance_sdk_escape_outside_and_cancelled_primary_press_never_open_settings() {
    for dismissal in ["escape", "outside", "drag-out"] {
        let mut f = Frames::new();
        let before = f.settings();
        f.open();
        match dismissal {
            "escape" => f.key(Key::Escape),
            "outside" => f.click(pos2(500.0, 300.0), PointerButton::Primary),
            _ => {
                let at = f.rect(SETTINGS).center();
                let outside = pos2(500.0, 300.0);
                f.frame(vec![Event::PointerMoved(at), pointer(at, PointerButton::Primary, true)]);
                f.frame(vec![Event::PointerMoved(outside)]);
                f.frame(vec![pointer(outside, PointerButton::Primary, false)]);
            }
        }
        f.idle();
        f.assert_no_action(&before);
        if dismissal == "drag-out" {
            assert!(f.is_open(), "cancelling an inside press is not an outside click");
            f.sdk_target();
            f.key(Key::Escape);
            f.idle();
            f.assert_no_action(&before);
        }
        assert!(!f.is_open(), "{dismissal}: actual close");
    }
}

#[test]
fn appearance_sdk_favorites_and_primary_icon_keep_their_existing_command_ownership() {
    let mut f = Frames::new();
    let before = f.settings();
    f.open();
    let favorite = format!("menu:themeFavorite-{}", f.favorite);
    f.click(f.rect(&favorite).center(), PointerButton::Primary);
    f.idle();
    assert_eq!(f.app.ui.dialog, None, "a favourite is not Appearance Settings");
    assert_eq!(f.app.ui.settings.appearance_mode, AppearanceMode::Light);
    assert_eq!(f.app.ui.settings.theme(f.app.system_theme(&f.ctx)).id(), f.favorite);
    assert_ne!(f.settings(), before, "the untouched favourite really selected a theme");
    f.assert_history();
    let expected = f.app.ui.settings.appearance_mode.next();
    f.click(f.rect("icon:appearance").center(), PointerButton::Primary);
    assert_eq!(f.app.ui.settings.appearance_mode, expected, "primary icon still owns view.appearance");
    assert_eq!(f.app.ui.dialog, None);
    f.assert_history();
}

#[test]
fn appearance_sdk_arrows_move_between_settings_and_the_checked_choice_group() {
    let mut f = Frames::new();
    let before = f.settings();
    f.open();
    f.tab_to_settings();
    let settings = f.sdk_target().id;
    let favorite = format!("menu:themeFavorite-{}", f.favorite);
    let favorite = f.hits.iter().find(|(name, _)| name == &favorite).unwrap().1.id;
    f.key(Key::ArrowDown);
    assert_eq!(craft::ui::runtime::focus::focused_rec(&f.ctx).map(|rec| rec.stop.id), Some(favorite));
    f.key(Key::ArrowUp);
    assert_eq!(craft::ui::runtime::focus::focused_rec(&f.ctx).map(|rec| rec.stop.id), Some(settings));
    f.assert_no_action(&before);
}

fn inert_release(mode: Mode) {
    let mut f = Frames::new();
    let before = f.settings();
    f.open();
    let target = f.sdk_target().id;
    let at = f.rect(SETTINGS).center();
    f.frame(vec![Event::PointerMoved(at)]);
    f.frame(vec![pointer(at, PointerButton::Primary, true)]);
    let (pressed, sense, enabled) = f.settings_response.expect("current press pass");
    assert_eq!(pressed, target);
    assert!(sense.senses_click(), "normal press must advertise CLICK (raw enabled={enabled})");
    f.mode = mode;
    f.frame(vec![pointer(at, PointerButton::Primary, false)]);
    if matches!(mode, Mode::Disabled) && f.hits.iter().any(|(name, _)| name == SETTINGS) {
        let hit = f.hits.iter().find(|(name, _)| name == SETTINGS).unwrap().1;
        assert_eq!(hit.id, target);
        let (release, sense, enabled) = f.settings_response.expect("current release pass");
        assert_eq!(release, target);
        assert!(!sense.senses_click(), "disabled release must not advertise CLICK (raw enabled={enabled})");
        let node = f.nodes.get(&target.accesskit_id()).expect("disabled row from this exact frame");
        assert_eq!(node.role(), egui::accesskit::Role::MenuItem);
        assert_eq!(node.label(), Some(crate::i18n::tr("Appearance Settings…")));
        assert!(node.is_disabled());
    } else {
        assert!(!f.rects.iter().any(|(name, _)| name == SETTINGS));
        assert!(!f.hits.iter().any(|(name, _)| name == SETTINGS));
        assert!(!f.nodes.contains_key(&target.accesskit_id()));
        assert!(f.settings_response.is_none());
        if matches!(mode, Mode::Disabled) {
            assert!(!f.is_open(), "missing disabled row requires actual dismissal");
        }
    }
    f.assert_no_action(&before);
    f.mode = Mode::Normal;
    f.idle();
    f.assert_no_action(&before);
    if f.is_open() {
        f.key(Key::Escape);
        assert!(!f.is_open());
    }
    f.open();
    f.click(f.rect(SETTINGS).center(), PointerButton::Primary);
    f.assert_settings(&before);
}
#[test]
fn appearance_sdk_disabled_release_then_idle_is_inert() {
    inert_release(Mode::Disabled);
}
#[test]
fn appearance_sdk_sizing_release_then_idle_is_inert() {
    inert_release(Mode::Sizing);
}
#[test]
fn appearance_sdk_measure_release_then_idle_is_inert() {
    inert_release(Mode::Measure);
}

#[test]
fn appearance_sdk_secondary_click_repositions_and_carry_blocks_opening() {
    let mut f = Frames::new();
    let before = f.settings();
    let icon = f.rect("icon:appearance");
    let a = icon.right_center() - vec2(2.0, 0.0);
    let b = icon.left_center() + vec2(2.0, 0.0);
    crate::widgets::set_carrying(&f.ctx, true);
    f.click(a, PointerButton::Secondary);
    assert!(!f.is_open());
    crate::widgets::set_carrying(&f.ctx, false);
    f.click(a, PointerButton::Secondary);
    let first = f.sdk_target();
    f.click(b, PointerButton::Secondary);
    let next = f.sdk_target();
    assert_eq!(first.id, next.id);
    assert!((next.interact_rect.left() - first.interact_rect.left() - (b.x - a.x)).abs() <= 2.0, "second click moves the same menu");
    f.assert_no_action(&before);
    f.key(Key::Escape);
    f.idle();
    assert!(!f.is_open());
    f.assert_no_action(&before);
}
