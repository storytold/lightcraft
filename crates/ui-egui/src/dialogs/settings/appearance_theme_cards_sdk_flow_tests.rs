//! Native-first physical Appearance page/card/favorite workflow, not desktop/window acceptance.
//! Real AppSettings and memory catalog/Undo; no app logic, picker, image decoding or worker.
use crate::state::{AppearanceMode, DarkTheme, LightTheme};
use crate::theme::ThemeKind;
use crate::{LightcraftApp, Services, headless::HeadlessView};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use lightcraft_catalog::{HistoryStep, Op, Photo, PhotoId, Source, Version};
use lightcraft_develop::DevelopSettings;
use lightcraft_engine::{Selection, Session};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};
const CLOCK: &str = "2026-11-22T00:00:00Z";
#[derive(Clone, Copy, Debug, PartialEq)]
enum Host {
    Page,
    NativePage,
    SettingsCallback,
}
#[derive(Clone, Copy, Debug)]
enum Pass {
    Real,
    Disabled,
    Measure,
    Sizing,
}
struct Drain(egui::FullOutput);
impl Drop for Drain {
    fn drop(&mut self) {
        self.0.textures_delta.clear();
        self.0.platform_output.commands.clear();
        self.0.viewport_output.clear();
        self.0.shapes.clear();
    }
}
#[derive(Clone, Debug)]
struct Node {
    id: u64,
    role: String,
    name: Option<String>,
    bounds: Option<Rect>,
}
struct Fixture {
    app: LightcraftApp,
    ctx: egui::Context,
    host: Host,
    pass: Pass,
    tick: u32,
    width: f32,
    shown: bool,
    tab: String,
    system: Arc<AtomicU8>,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    nodes: Vec<Node>,
    trace: VecDeque<String>,
    previous_locale: crate::i18n::Locale,
}
impl Fixture {
    fn new(host: Host) -> Self {
        let previous_locale = crate::i18n::language();
        crate::i18n::set_language(crate::i18n::Locale::En);
        let mut session = Session::new();
        session.clock = Box::new(|| CLOCK.into());
        session.xmp.auto_write = false;
        for index in 0..2 {
            let id = PhotoId(101 + index);
            let mut photo = Photo::new(id, Source::Demo { scene: index as u32 }, &format!("Preserved {index}"), "JPEG", 8, 6, CLOCK);
            let mut d = DevelopSettings::default();
            d.light.contrast = index as f64 * 7.0;
            photo.develop = Arc::new(d);
            photo.meta.title = format!("Title {index}");
            photo.meta.caption = format!("Caption {index}");
            photo.meta.keywords = vec![format!("keep-{index}")];
            photo.history = vec![HistoryStep { label: format!("Old {index}"), settings: photo.develop.clone() }];
            photo.versions = vec![Version { name: format!("Saved {index}"), created: CLOCK.into(), settings: photo.develop.clone(), auto: false }];
            session.commit("Fixture", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        }
        session.selection = Selection::single(PhotoId(101));
        let system = Arc::new(AtomicU8::new(2));
        let read_system = system.clone();
        let mut app = LightcraftApp::new(
            session,
            Services {
                png: None,
                local_places: Some(vec![]),
                system_theme: Some(Box::new(move |_| match read_system.load(Ordering::SeqCst) {
                    1 => Some(egui::Theme::Light),
                    2 => Some(egui::Theme::Dark),
                    _ => None,
                })),
                ..Default::default()
            },
        );
        app.set_simulated_clock(Some(0.0));
        app.run("library.select", json!({"ids":[101,102],"active":101})).unwrap();
        app.run("develop.merge", json!({"settings":{"light":{"exposure":0.5}},"label":"Preserved redo"})).unwrap();
        app.run("edit.undo", json!({})).unwrap();
        assert!(!app.session.undo.is_empty() && !app.session.redo.is_empty());
        app.ui.settings.appearance_mode = AppearanceMode::Dark;
        app.ui.settings.light_theme = LightTheme::Silver;
        app.ui.settings.dark_theme = DarkTheme::Charcoal;
        app.ui.settings.favorite_themes = vec!["unknown-preserved".into(), "silver".into(), "charcoal".into()];
        app.ui.settings.gpu = false;
        app.ui.settings.external_editor = "untouched editor preference".into();
        app.ui.settings.film_names = false;
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        ctx.enable_accesskit();
        app.sync_theme(&ctx);
        let mut f = Self {
            app,
            ctx,
            host,
            pass: Pass::Real,
            tick: 0,
            width: 560.0,
            shown: true,
            tab: "interface".into(),
            system,
            rects: vec![],
            hits: vec![],
            nodes: vec![],
            trace: VecDeque::new(),
            previous_locale,
        };
        let before = f.state();
        f.settle();
        assert_eq!(f.state(), before, "initial Appearance page is passive");
        f
    }
    fn history(entries: &[lightcraft_engine::UndoEntry]) -> Value {
        json!(entries.iter().map(|entry| json!({"label":entry.label,"op":entry.op,"folder":format!("{:?}",entry.folder)})).collect::<Vec<_>>())
    }
    fn document(&self) -> Value {
        let s = &self.app.session;
        json!({"catalog":s.catalog,"revision":s.catalog.revision,"selection":s.selection,"previousActive":s.previous_active,
            "undo":Self::history(&s.undo),"redo":Self::history(&s.redo),"journal":s.journal,"commits":s.commits(),
            "interaction":format!("{:?}",s.interaction),"autoSync":s.auto_sync,"clipboard":s.clipboard,"copyGroups":s.copy_groups,
            "source":s.source,"filter":s.filter,"sort":s.sort,"activeMask":s.active_mask,"activeSpot":s.active_spot,
            "toolEffects":s.mask_tool_effects,"lastMerge":s.last_merge,"imports":s.import_defaults,"xmp":s.xmp,
            "lastExport":s.last_export,"presets":s.presets.iter().cloned().collect::<Vec<_>>(),"print":s.print,
            "templates":s.print_templates,"effects":s.mask_effects,"keywordSets":s.keyword_sets,"keywordSet":s.keyword_set})
    }
    fn state(&self) -> Value {
        json!({"settings":self.app.ui.settings,"document":self.document(),"shownTheme":crate::theme::current(&self.ctx).map(|kind|kind.id()),"system":self.system.load(Ordering::SeqCst),"hdr":self.app.ui.hdr_visualize,"epoch":self.app.caches.denoise.epoch,"workers":self.app.renderer.live_workers(),"inFlight":self.app.renderer.in_flight(),"segmenting":self.app.session.segmenter.busy()})
    }
    fn expected_theme(mut state: Value) -> Value {
        let mode = state["settings"]["appearanceMode"].as_str().unwrap();
        let light = mode == "light" || (mode == "auto" && state["system"] == 1);
        state["shownTheme"] = state["settings"][if light { "lightTheme" } else { "darkTheme" }].clone();
        state
    }
    fn safe(&self) {
        assert!(self.app.session.library.is_none() && self.app.session.library_folder.is_none());
        assert!(self.app.session.media.rendered.disk().is_none());
        assert_eq!(self.app.session.catalog.photos().count(), 2);
        assert!(!self.app.session.denoise_busy());
        assert_eq!(self.app.renderer.live_workers(), 0);
        assert_eq!(self.app.renderer.in_flight(), 0);
        assert!(!self.app.session.segmenter.busy());
        assert!(
            self.app.services.png.is_none()
                && self.app.services.picker.is_none()
                && self.app.services.pick_files.is_none()
                && self.app.services.pick_folder.is_none()
        );
    }
    fn frame(&mut self, mut events: Vec<Event>) {
        self.safe();
        self.tick += 1;
        self.app.sync_theme(&self.ctx);
        let mods = events
            .iter()
            .rev()
            .find_map(|event| match event {
                Event::Key { modifiers, .. } | Event::PointerButton { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or(Modifiers::NONE);
        events.insert(0, Event::ModifiersChanged(mods));
        let input = format!("{events:?}");
        let raw = HeadlessView::raw_input(egui::vec2((self.width + 64.0).max(400.0), 1800.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let host = self.host;
        let pass = self.pass;
        let width = self.width;
        let shown = self.shown;
        let tab = &mut self.tab;
        let context = self.ctx.clone();
        let mut registry = (vec![], vec![]);
        let output = Drain(self.ctx.run_ui(raw, |root| {
            crate::widgets::take_registries(root.ctx());
            registry = (vec![], vec![]);
            egui::CentralPanel::default().show(root, |ui| {
                ui.allocate_ui_with_layout(egui::vec2(width, 1600.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_min_width(width);
                    ui.set_max_width(width);
                    let mut draw = |ui: &mut egui::Ui| {
                        if !shown {
                            return;
                        }
                        if host == Host::SettingsCallback {
                            super::super::window(app, ui, tab, 1500.0);
                        } else {
                            let tokens = crate::theme::Tokens::get(ui.ctx());
                            let system = app.system_theme(ui.ctx());
                            super::appearance(ui, &tokens, &mut app.ui.settings, system);
                        }
                    };
                    let mut present = || match pass {
                        Pass::Real => draw(ui),
                        Pass::Disabled => {
                            ui.add_enabled_ui(false, |ui| draw(ui));
                        }
                        Pass::Measure => craft::ui::runtime::measure::run(&context, || draw(ui)),
                        Pass::Sizing => {
                            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                            draw(&mut child);
                        }
                    };
                    if host == Host::NativePage {
                        (present)();
                    } else {
                        present();
                    }
                });
            });
            registry = crate::widgets::take_registries(root.ctx());
        }));
        self.nodes.clear();
        if let Some(update) = &output.0.platform_output.accesskit_update {
            for (id, node) in &update.nodes {
                self.nodes.push(Node {
                    id: id.0,
                    role: format!("{:?}", node.role()),
                    name: node.label().map(str::to_owned),
                    bounds: node
                        .bounds()
                        .map(|rect| Rect::from_min_max(egui::pos2(rect.x0 as f32, rect.y0 as f32), egui::pos2(rect.x1 as f32, rect.y1 as f32))),
                });
            }
        }
        (self.rects, self.hits) = registry;
        self.trace.push_back(format!(
            "frame={} host={host:?} pass={pass:?} input={input} selected={:?}/{:?} favorites={:?} focus={:?} sdk={:?} targets={:?}",
            self.tick,
            self.app.ui.settings.light_theme,
            self.app.ui.settings.dark_theme,
            self.app.ui.settings.favorite_themes,
            self.ctx.memory(|m| m.focused()),
            craft::ui::runtime::focus::focused_rec(&self.ctx),
            self.rects.iter().filter(|(key, _)| key.starts_with("radio:theme-") || key.starts_with("button:themeFavorite-")).collect::<Vec<_>>()
        ));
        while self.trace.len() > 14 {
            self.trace.pop_front();
        }
        self.safe();
    }
    fn settle(&mut self) {
        for _ in 0..4 {
            self.frame(vec![]);
        }
    }
    fn rect(&self, key: &str) -> Rect {
        self.rects.iter().find(|(name, _)| name == key).unwrap_or_else(|| panic!("missing {key}; trace={:#?}", self.trace)).1
    }
    fn has(&self, key: &str) -> bool {
        self.rects.iter().any(|(name, _)| name == key)
    }
    fn radio(kind: ThemeKind) -> String {
        format!("radio:theme-{}", kind.id())
    }
    fn favorite(kind: ThemeKind) -> String {
        format!("button:themeFavorite-{}", kind.id())
    }
    fn node(&self, key: &str) -> &Node {
        let kind = ThemeKind::ALL.iter().copied().find(|kind| key.ends_with(&format!("-{}", kind.id()))).unwrap();
        let radio = key.starts_with("radio:");
        let role = if radio { "RadioButton" } else { "Button" };
        let nodes =
            self.nodes
                .iter()
                .filter(|node| {
                    node.role == role
                        && node.name.as_deref().is_some_and(|name| {
                            if radio { name == crate::i18n::tr(kind.label()) } else { name.ends_with(crate::i18n::tr(kind.label())) }
                        })
                })
                .collect::<Vec<_>>();
        assert_eq!(nodes.len(), 1, "one named real owner for {key}; nodes={:?}; trace={:#?}", self.nodes, self.trace);
        let node = nodes[0];
        assert!(node.bounds.is_some_and(|bounds| bounds.intersects(self.rect(key))));
        node
    }
    fn focused(&self, key: &str) -> bool {
        let id = craft::ui::runtime::focus::focused_rec(&self.ctx).map(|focus| focus.stop.id).or_else(|| self.ctx.memory(|memory| memory.focused()));
        id.is_some_and(|id| id.accesskit_id().0 == self.node(key).id)
    }
    fn pointer(&mut self, at: Pos2, pressed: bool) {
        self.frame(vec![
            Event::PointerMoved(at),
            Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
        ]);
    }
    fn hover(&mut self, key: &str) {
        let before = self.state();
        let at = self.rect(key).center();
        self.frame(vec![Event::PointerMoved(at)]);
        self.settle();
        assert_eq!(self.state(), before, "hover is passive");
    }
    fn click(&mut self, key: &str) {
        self.tick += 30;
        self.hover(key);
        let at = self.rect(key).center();
        self.frame(vec![Event::PointerMoved(at)]);
        if key.starts_with("radio:") || key.starts_with("button:themeFavorite-") {
            self.node(key);
        }
        self.pointer(at, true);
        self.pointer(at, false);
        self.settle();
    }
    fn key_mod(&mut self, key: Key, mods: Modifiers) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: mods }]);
        }
        self.frame(vec![]);
    }
    fn key(&mut self, key: Key) {
        self.key_mod(key, Modifiers::NONE);
    }
    fn tab_to(&mut self, key: &str) {
        if key.starts_with("radio:") {
            self.frame(vec![Event::PointerGone]);
        }
        for _ in 0..96 {
            if self.focused(key) {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("natural Tab never acquired {key}; trace={:#?}", self.trace);
    }
    fn verify(&self, wanted: Value) {
        assert_eq!(self.state(), Self::expected_theme(wanted), "only the requested preference changed; trace={:#?}", self.trace);
    }
    fn pick(&mut self, kind: ThemeKind) {
        let mut wanted = self.state();
        wanted["settings"][if kind.is_dark() { "darkTheme" } else { "lightTheme" }] = json!(kind.id());
        self.click(&Self::radio(kind));
        self.verify(wanted);
    }
    fn toggle_favorite(&mut self, kind: ThemeKind) {
        let mut wanted = self.state();
        let values = wanted["settings"]["favoriteThemes"].as_array_mut().unwrap();
        if values.iter().any(|value| value == kind.id()) {
            values.retain(|value| value != kind.id());
        } else {
            values.push(json!(kind.id()));
        }
        self.click(&Self::favorite(kind));
        self.verify(wanted);
    }
    fn mode(&mut self, index: usize, value: &str) {
        let mut wanted = self.state();
        wanted["settings"]["appearanceMode"] = json!(value);
        self.click(&format!("button:settingsAppearance-{index}"));
        self.verify(wanted);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.safe();
            let raw = HeadlessView::raw_input(egui::vec2(1000.0, 1800.0), 1.0, f64::from(self.tick + 1) / 60.0, vec![]);
            let _output = Drain(self.ctx.run_ui(raw, |ui| {
                crate::widgets::take_registries(ui.ctx());
            }));
            crate::widgets::take_registries(&self.ctx);
        }
        crate::i18n::set_language(self.previous_locale);
    }
}

#[test]
fn appearance_sdk_page_pointer_family_selections_reselection_and_modes_preserve_everything() {
    let mut f = Fixture::new(Host::Page);
    for kind in [ThemeKind::Paper, ThemeKind::Midnight, ThemeKind::Silver, ThemeKind::Charcoal, ThemeKind::Paper, ThemeKind::Midnight] {
        f.pick(kind);
        f.pick(kind);
    }
    for (index, value) in [(2, "light"), (1, "dark"), (0, "auto")] {
        f.mode(index, value);
    }
    assert_eq!(f.app.ui.settings.light_theme, LightTheme::Paper);
    assert_eq!(f.app.ui.settings.dark_theme, DarkTheme::Midnight);
}
#[test]
fn appearance_sdk_page_favorites_toggle_without_picking_or_reordering_other_ids() {
    let mut f = Fixture::new(Host::Page);
    for kind in [ThemeKind::Silver, ThemeKind::Paper, ThemeKind::Charcoal, ThemeKind::Midnight, ThemeKind::Silver, ThemeKind::Paper] {
        f.toggle_favorite(kind);
    }
    f.mode(2, "light");
    f.mode(1, "dark");
}
#[test]
fn appearance_sdk_scoped_pointer_and_natural_keyboard_favorites_and_choices() {
    native_journey(Host::NativePage);
}
#[test]
fn appearance_sdk_settings_callback_pointer_and_keyboard_use_existing_native_scope() {
    native_journey(Host::SettingsCallback);
}
fn native_journey(host: Host) {
    let mut f = Fixture::new(host);
    f.pick(ThemeKind::Paper);
    f.tab_to(&Fixture::radio(ThemeKind::Silver));
    let mut wanted = f.state();
    wanted["settings"]["lightTheme"] = json!("silver");
    f.key(Key::Space);
    f.settle();
    f.verify(wanted);
    let key = Fixture::favorite(ThemeKind::Charcoal);
    f.tab_to(&key);
    let mut wanted = f.state();
    wanted["settings"]["favoriteThemes"].as_array_mut().unwrap().retain(|v| v != "charcoal");
    f.key(Key::Enter);
    f.settle();
    f.verify(wanted);
    f.hover(&key);
    f.tab_to(&key);
    let mut wanted = f.state();
    wanted["settings"]["favoriteThemes"].as_array_mut().unwrap().push(json!("charcoal"));
    f.key(Key::Space);
    f.settle();
    f.verify(wanted);
    f.key_mod(Key::Tab, Modifiers { shift: true, ..Modifiers::NONE });
    f.toggle_favorite(ThemeKind::Paper);
}
#[test]
fn appearance_sdk_page_roving_radio_keyboard_and_favorite_preserve_both_families() {
    let mut f = Fixture::new(Host::Page);
    f.tab_to(&Fixture::radio(ThemeKind::Silver));
    let before = f.state();
    f.key(Key::ArrowDown);
    assert!(f.focused(&Fixture::radio(ThemeKind::Paper)), "arrow acquires the next family member; trace={:#?}", f.trace);
    assert_eq!(f.state(), before);
    let mut wanted = before;
    wanted["settings"]["lightTheme"] = json!("paper");
    f.key(Key::Space);
    f.settle();
    f.verify(wanted);
    let key = Fixture::favorite(ThemeKind::Charcoal);
    f.tab_to(&key);
    let mut wanted = f.state();
    wanted["settings"]["favoriteThemes"].as_array_mut().unwrap().retain(|v| v != "charcoal");
    f.key(Key::Enter);
    f.settle();
    f.verify(wanted);
}
#[test]
fn appearance_sdk_page_auto_system_changes_and_external_preferences_are_authoritative() {
    let mut f = Fixture::new(Host::Page);
    f.pick(ThemeKind::Paper);
    f.pick(ThemeKind::Midnight);
    f.mode(0, "auto");
    for system in [1, 2] {
        f.system.store(system, Ordering::SeqCst);
        let wanted = f.state();
        f.settle();
        f.verify(wanted);
    }
    f.app.ui.settings.light_theme = LightTheme::Silver;
    f.app.ui.settings.dark_theme = DarkTheme::Charcoal;
    f.app.ui.settings.favorite_themes = vec!["preserved-external".into(), "paper".into()];
    let wanted = f.state();
    f.settle();
    f.verify(wanted);
    f.pick(ThemeKind::Paper);
    f.toggle_favorite(ThemeKind::Paper);
}
#[test]
fn appearance_sdk_page_disappearance_cancels_old_press_then_reentry_and_fresh_edit() {
    for favorite in [false, true] {
        let mut f = Fixture::new(Host::Page);
        let key = if favorite { Fixture::favorite(ThemeKind::Paper) } else { Fixture::radio(ThemeKind::Paper) };
        f.hover(&key);
        let at = f.rect(&key).center();
        f.pointer(at, true);
        f.shown = false;
        f.settle();
        assert!(!f.has(&key));
        let before = f.state();
        f.pointer(at, false);
        f.shown = true;
        f.settle();
        assert_eq!(f.state(), before);
        if favorite {
            f.toggle_favorite(ThemeKind::Paper);
        } else {
            f.pick(ThemeKind::Paper);
        }
    }
}
#[test]
fn appearance_sdk_page_narrow_cards_keep_both_choices_and_favorites() {
    let mut f = Fixture::new(Host::Page);
    f.width = 340.0;
    let before = f.state();
    f.settle();
    assert_eq!(f.state(), before);
    assert!(f.rect(&Fixture::radio(ThemeKind::Charcoal)).top() > f.rect(&Fixture::radio(ThemeKind::Paper)).bottom());
    f.pick(ThemeKind::Paper);
    f.pick(ThemeKind::Midnight);
    f.toggle_favorite(ThemeKind::Midnight);
}
fn nonreal_flow(pass: Pass, host: Host) {
    for favorite in [false, true] {
        for real_press in [false, true] {
            let mut f = Fixture::new(host);
            let key = if favorite { Fixture::favorite(ThemeKind::Paper) } else { Fixture::radio(ThemeKind::Paper) };
            f.hover(&key);
            let at = f.rect(&key).center();
            let before = f.state();
            if !real_press {
                f.pass = pass;
            }
            f.pointer(at, true);
            assert_eq!(f.state(), before);
            if !real_press && matches!(pass, Pass::Measure | Pass::Sizing) {
                assert!(!f.has(&key), "speculation publishes no theme/favorite alias");
            }
            f.pass = if real_press { pass } else { Pass::Real };
            f.pointer(at, false);
            assert_eq!(f.state(), before);
            if real_press && matches!(pass, Pass::Measure | Pass::Sizing) {
                assert!(!f.has(&key));
            }
            f.pass = Pass::Real;
            f.settle();
            assert_eq!(f.state(), before);
            f.frame(vec![]);
            assert_eq!(f.state(), before);
            if favorite {
                f.toggle_favorite(ThemeKind::Paper);
            } else {
                f.pick(ThemeKind::Paper);
            }
        }
    }
}
#[test]
fn appearance_sdk_page_disabled_both_physical_directions_no_replay_and_fresh_actions() {
    nonreal_flow(Pass::Disabled, Host::Page);
}
#[test]
fn appearance_sdk_page_sdk_measure_both_directions_no_alias_replay_and_fresh_actions() {
    nonreal_flow(Pass::Measure, Host::Page);
}
#[test]
fn appearance_sdk_page_egui_sizing_both_directions_no_alias_replay_and_fresh_actions() {
    nonreal_flow(Pass::Sizing, Host::Page);
}
#[test]
fn appearance_sdk_fallback_nonreal_radio_and_favorite_paths_are_inert() {
    for pass in [Pass::Disabled, Pass::Measure, Pass::Sizing] {
        nonreal_flow(pass, Host::NativePage);
    }
}
