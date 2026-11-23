//! Production show_in_window and installed topbar; no substitute menu, logic loop, OS host or file services.
use super::{LightcraftApp, MENUS, MenuNode, menu_bar, show_in_window};
use crate::headless::HeadlessView;
use egui::{Event, Key, Modifiers, PointerButton, Rect};
use serde_json::{Value, json};

struct Output(egui::FullOutput);
impl Drop for Output {
    fn drop(&mut self) {
        self.0.textures_delta.clear();
        self.0.platform_output.commands.clear();
        self.0.viewport_output.clear();
        self.0.shapes.clear();
    }
}

// 0 = actual full presenter; 2 = installed production topbar. Native Settings scope remains private and untouched.
struct Fixture {
    app: LightcraftApp,
    ctx: egui::Context,
    route: u8,
    enabled: bool,
    narrow: bool,
    tick: u32,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    painted: Vec<String>,
    old_language: crate::i18n::Locale,
}
impl Fixture {
    fn new(route: u8) -> Self {
        let mut app = LightcraftApp::new(
            lightcraft_engine::Session::with_demo(),
            crate::Services { local_places: Some(vec![]), png: None, ..Default::default() },
        );
        app.native_menu = false; // Fixture host has an in-window bar, never an OS menu service.
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let old_language = crate::i18n::language();
        let mut f = Self { app, ctx, route, enabled: true, narrow: false, tick: 0, rects: vec![], hits: vec![], painted: vec![], old_language };
        for _ in 0..3 {
            f.frame(vec![], 0);
        }
        f
    }
    fn memory_only(&self) {
        assert!(self.app.session.library.is_none() && self.app.session.library_folder.is_none());
        assert!(self.app.session.smart_previews_dir.is_none() && self.app.session.media.rendered.disk().is_none());
        assert!(self.app.services.picker.is_none() && self.app.services.pick_folder.is_none() && self.app.services.pick_files.is_none());
    }
    fn frame(&mut self, events: Vec<Event>, probe: u8) {
        self.memory_only();
        crate::widgets::take_registries(&self.ctx);
        self.tick += 1;
        let raw = HeadlessView::raw_input(egui::vec2(2400.0, 1800.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let ctx = self.ctx.clone();
        let (route, enabled, narrow) = (self.route, self.enabled, self.narrow);
        let mut final_pass = None;
        let output = Output(self.ctx.run_ui(raw, |ui| {
            ui.add_enabled_ui(enabled, |ui| {
                let mut draw = |ui: &mut egui::Ui| {
                    if route == 2 {
                        crate::panels::topbar::show(app, ui);
                    } else {
                        egui::Panel::top("title-flow-bar").exact_size(40.0).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                show_in_window(app, ui, if narrow { 1.0 } else { 2000.0 });
                            });
                        });
                    }
                };
                match probe {
                    1 => craft::ui::runtime::measure::run(&ctx, || draw(ui)),
                    2 => {
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                        draw(&mut child);
                    }
                    _ => draw(ui),
                }
            });
            final_pass = Some(crate::widgets::take_registries(&ctx));
        }));
        fn text(shape: &egui::Shape, labels: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => labels.push(t.galley.job.text.clone()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        text(shape, labels);
                    }
                }
                _ => {}
            }
        }
        self.painted.clear();
        for shape in &output.0.shapes {
            text(&shape.shape, &mut self.painted);
        }
        (self.rects, self.hits) = final_pass.unwrap();
        drop(output);
        self.memory_only();
    }
    fn rect(&self, id: &str) -> Rect {
        let found: Vec<_> = self.rects.iter().filter(|(name, _)| name == id).collect();
        assert_eq!(found.len(), 1, "one presented locator for {id}, route={}", self.route);
        found[0].1
    }
    fn click_at(&mut self, at: egui::Pos2) {
        for pressed in [true, false] {
            self.frame(
                vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }],
                0,
            );
        }
        self.frame(vec![], 0);
    }
    fn click(&mut self, id: &str) {
        self.click_at(self.rect(id).center());
    }
    fn hover(&mut self, id: &str) {
        self.frame(vec![Event::PointerMoved(self.rect(id).center())], 0);
        self.frame(vec![], 0);
    }
    fn key(&mut self, key: Key) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE }], 0);
        }
        self.frame(vec![], 0);
    }
    fn open_title(&self) -> Option<String> {
        if !egui::Popup::is_any_open(&self.ctx) {
            return None;
        }
        let level = self.rect("menu-level:1");
        let matches: Vec<_> = MENUS.iter().filter(|title| (self.rect(&format!("menu:{title}")).left() - level.left()).abs() < 12.0).collect();
        assert_eq!(matches.len(), 1, "one title anchors the live popup");
        let max_bottom = MENUS.iter().map(|title| self.rect(&format!("menu:{title}")).bottom()).fold(f32::MIN, f32::max);
        assert!(level.top() >= max_bottom - 0.5, "the anchored menu does not cover its titles");
        assert!(level.bottom() <= 1800.5, "menu stays inside the viewport");
        Some(matches[0].to_string())
    }
    fn tab_to_title(&mut self, title: &str) {
        let id = self.hits.iter().find(|(name, _)| name == &format!("menu:{title}")).unwrap().1.id;
        self.frame(vec![Event::PointerGone], 0);
        for _ in 0..96 {
            if craft::ui::runtime::focus::focused_rec(&self.ctx).is_some_and(|rec| rec.stop.id == id) {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("natural Tab did not reach title {title}, route={}", self.route);
    }
    fn arrow_to_native_item(&mut self, id: &str) {
        self.frame(vec![Event::PointerGone], 0);
        let mut trace = Vec::new();
        for _ in 0..96 {
            let native = self.ctx.memory(|memory| memory.focused());
            let focused = native.and_then(|id| self.ctx.read_response(id));
            let sdk = craft::ui::runtime::focus::focused_rec(&self.ctx).map(|rec| rec.stop.id);
            trace.push((native, focused.as_ref().map(|response| response.rect), sdk));
            assert!(egui::Popup::is_any_open(&self.ctx), "native popup closed during natural navigation: {trace:?}");
            if focused.is_some_and(|response| response.rect == self.rect(id)) {
                return;
            }
            // Native MenuState::handle_keyboard accepts cardinal directions, not Tab's
            // Next/Previous direction. Enter from the opener and traverse real rows with Down.
            self.key(Key::ArrowDown);
        }
        panic!("native popup ArrowDown did not reach {id}, route={}; native/SDK focus trace: {trace:?}; no forced focus fallback", self.route);
    }
    fn document(&self) -> Value {
        let s = &self.app.session;
        json!({"photos":s.catalog.photos().map(|photo|serde_json::to_value(photo).unwrap()).collect::<Vec<_>>(),
            "revision":s.catalog.revision,"undo":s.undo.len(),"redo":s.redo.len(),"commits":s.commits(),
            "settings":self.app.ui.settings,"imports":s.import_defaults,"xmp":s.xmp})
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        crate::i18n::set_language(self.old_language);
        if !std::thread::panicking() {
            self.memory_only();
            let raw = HeadlessView::raw_input(egui::vec2(2400.0, 1800.0), 1.0, f64::from(self.tick + 1) / 60.0, vec![]);
            drop(Output(self.ctx.run_ui(raw, |_| {})));
            crate::widgets::take_registries(&self.ctx);
        }
    }
}

#[test]
fn in_window_titles_pointer_hover_switch_confirmation_and_dismissal_keep_one_menu() {
    for route in [0, 2] {
        let mut f = Fixture::new(route);
        let before = f.document();
        let journal = f.app.session.journal.clone();
        f.hover("menu:Edit");
        assert_eq!(f.open_title(), None, "hover alone cannot open a closed bar");
        f.click("menu:File");
        assert_eq!(f.open_title().as_deref(), Some("File"));
        for title in ["Edit", "View", "Photo", "Window", "Help"] {
            f.hover(&format!("menu:{title}"));
            assert_eq!(f.open_title().as_deref(), Some(title));
        }
        f.click("menu:Help");
        assert_eq!(f.open_title().as_deref(), Some("Help"), "click confirms a hover-opened menu");
        f.frame(vec![Event::PointerMoved(egui::pos2(1800.0, 900.0))], 0);
        f.click("menu:Edit");
        assert_eq!(f.open_title().as_deref(), Some("Edit"), "one click switches an existing menu");
        f.click("menu:Edit");
        assert_eq!(f.open_title(), None, "clicking the confirmed title closes it");
        f.click("menu:Photo");
        f.key(Key::Escape);
        assert_eq!(f.open_title(), None);
        f.click("menu:File");
        f.click_at(egui::pos2(1800.0, 900.0));
        assert_eq!(f.open_title(), None);
        assert_eq!(f.app.session.journal, journal, "title activation is not command execution");
        assert_eq!(f.document(), before);
    }
}

#[test]
fn in_window_titles_opening_never_executes_and_one_real_item_executes_exactly_once() {
    for route in [0, 2] {
        let mut f = Fixture::new(route);
        f.app.run("library.selectAll", json!({})).unwrap();
        let before = f.document();
        let active = f.app.session.selection.active;
        let journal = f.app.session.journal.len();
        f.click("menu:Edit");
        assert_eq!(f.app.session.journal.len(), journal, "opening is not executing a command");
        f.click("menuitem:library.selectNone");
        assert_eq!(f.app.session.journal.get(journal..).unwrap(), &[("library.selectNone".into(), json!({}))]);
        assert_eq!(f.app.session.selection.active, active);
        assert_eq!(f.app.session.selection.ids.iter().copied().collect::<Vec<_>>(), active.into_iter().collect::<Vec<_>>());
        assert_eq!(f.document(), before, "only the non-destructive selection command runs");
        assert!(!egui::Popup::is_any_open(&f.ctx));
        for _ in 0..3 {
            f.frame(vec![], 0);
        }
        assert_eq!(f.app.session.journal.len(), journal + 1);
    }
}

#[test]
fn in_window_titles_natural_tab_enter_space_open_then_native_keyboard_executes_once() {
    for route in [0, 2] {
        for activation in [Key::Enter, Key::Space] {
            let mut f = Fixture::new(route);
            let before = f.document();
            let journal = f.app.session.journal.len();
            f.tab_to_title("Edit");
            f.key(activation);
            assert_eq!(f.open_title().as_deref(), Some("Edit"));
            assert_eq!(f.app.session.journal.len(), journal, "keyboard title only opens");
            f.arrow_to_native_item("menuitem:library.selectNone");
            f.key(Key::Enter);
            assert_eq!(f.app.session.journal.get(journal..).unwrap(), &[("library.selectNone".into(), json!({}))]);
            assert_eq!(f.document(), before);
            assert!(!egui::Popup::is_any_open(&f.ctx));
        }
    }
}

#[test]
fn in_window_titles_live_localized_titles_and_selection_labels_keep_semantic_identity() {
    let mut f = Fixture::new(2);
    f.click("menu:Photo");
    f.app.run("library.selectAll", json!({})).unwrap();
    f.frame(vec![], 0);
    let bar = menu_bar(&f.app);
    let photo = &bar.iter().find(|(title, _)| title == "Photo").unwrap().1;
    let label = photo
        .iter()
        .find_map(|node| match node {
            MenuNode::Item { id, label, .. } if id == "photo.delete" => Some(label),
            _ => None,
        })
        .unwrap();
    assert!(f.painted.iter().any(|text| text.trim() == label.trim()), "open menu refreshes its real live label");
    f.key(Key::Escape);
    crate::i18n::set_language(crate::i18n::Locale::parse_tag("fr").unwrap());
    f.frame(vec![], 0);
    for title in MENUS {
        f.rect(&format!("menu:{title}"));
        assert!(f.painted.iter().any(|text| text == crate::i18n::tr(title)), "translated title {title}");
    }
    f.click("menu:Edit");
    assert_eq!(f.open_title().as_deref(), Some("Edit"));
}

#[test]
fn in_window_titles_disabled_sizing_and_outside_release_do_not_open_or_execute() {
    for route in [0, 2] {
        let mut f = Fixture::new(route);
        let before = f.document();
        let journal = f.app.session.journal.clone();
        f.enabled = false;
        f.frame(vec![], 0);
        f.click("menu:Edit");
        assert!(!egui::Popup::is_any_open(&f.ctx));
        assert_eq!(f.app.session.journal, journal);
        f.enabled = true;
        f.frame(vec![], 0);
        let at = f.rect("menu:Edit").center();
        f.frame(
            vec![
                Event::PointerMoved(at),
                Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE },
            ],
            0,
        );
        let outside = egui::pos2(1800.0, 900.0);
        f.frame(
            vec![
                Event::PointerMoved(outside),
                Event::PointerButton { pos: outside, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE },
            ],
            0,
        );
        assert!(!egui::Popup::is_any_open(&f.ctx));
        assert_eq!(f.document(), before);
        assert_eq!(f.app.session.journal, journal);
    }
    for probe in [1, 2] {
        let mut f = Fixture::new(0);
        let at = f.rect("menu:Edit").center();
        let journal = f.app.session.journal.clone();
        for pressed in [true, false] {
            f.frame(
                vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }],
                probe,
            );
            assert!(!f.rects.iter().any(|(name, _)| name.starts_with("menu:")));
        }
        f.frame(vec![], 0);
        assert!(!egui::Popup::is_any_open(&f.ctx));
        assert_eq!(f.app.session.journal, journal);
    }
}

#[test]
fn in_window_titles_narrow_overflow_remains_native_and_full_width_keeps_title_gaps() {
    let mut f = Fixture::new(0);
    for pair in MENUS.windows(2) {
        assert!(f.rect(&format!("menu:{}", pair[1])).left() - f.rect(&format!("menu:{}", pair[0])).right() >= 23.5);
    }
    f.narrow = true;
    f.frame(vec![], 0);
    f.rect("menu:all");
    assert!(!f.rects.iter().any(|(name, _)| name == "menu:File"));
    let journal = f.app.session.journal.clone();
    f.click("menu:all");
    assert!(egui::Popup::is_any_open(&f.ctx));
    f.key(Key::Escape);
    assert!(!egui::Popup::is_any_open(&f.ctx));
    assert_eq!(f.app.session.journal, journal);
}
