//! Production show_in_window and installed topbar over the framework menu bar; no substitute menu, logic loop, OS host or file services.
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
    /// Idle frames: a menu that closed or switched fades out for a few frames before it stops being drawn.
    fn settle(&mut self) {
        for _ in 0..20 {
            self.frame(vec![], 0);
        }
    }
    fn click_at(&mut self, at: egui::Pos2) {
        for pressed in [true, false] {
            self.frame(
                vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }],
                0,
            );
        }
        self.settle();
    }
    fn click(&mut self, id: &str) {
        self.click_at(self.rect(id).center());
    }
    fn hover(&mut self, id: &str) {
        self.frame(vec![Event::PointerMoved(self.rect(id).center())], 0);
        self.settle();
    }
    fn key(&mut self, key: Key) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE }], 0);
        }
        self.settle();
    }
    /// A menu is drawn: its level is published while it is open (and not otherwise).
    fn is_open(&self) -> bool {
        self.rects.iter().any(|(name, _)| name.starts_with("menu-level:"))
    }
    fn open_title(&self) -> Option<String> {
        if !self.is_open() {
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
        // The framework bar is one Tab stop; the arrow keys move between its titles (one roving group).
        let id = self.hits.iter().find(|(name, _)| name == &format!("menu:{title}")).unwrap().1.id;
        let part = craft::behavior::PartKey::new("title").at(MENUS.iter().position(|m| *m == title).unwrap() as u64);
        self.frame(vec![Event::PointerGone], 0);
        for _ in 0..96 {
            let rec = craft::ui::runtime::focus::focused_rec(&self.ctx);
            if rec.as_ref().is_some_and(|rec| rec.stop.id == id) {
                for _ in 0..MENUS.len() {
                    if craft::ui::runtime::focus::focused_rec(&self.ctx).is_some_and(|rec| rec.stop.id == id && rec.stop.part == Some(part)) {
                        return;
                    }
                    self.key(Key::ArrowRight);
                }
                panic!("the arrow keys did not reach title {title}, route={}", self.route);
            }
            self.key(Key::Tab);
        }
        panic!("natural Tab did not reach the menu bar for title {title}, route={}", self.route);
    }
    /// ArrowDown through the open menu's rows until the keyboard is on the row published as `id`.
    fn arrow_to_item(&mut self, id: &str) {
        let row = self.hits.iter().find(|(name, _)| name == id).unwrap_or_else(|| panic!("row {id} is published")).1.id;
        self.frame(vec![Event::PointerGone], 0);
        let mut trace = Vec::new();
        for _ in 0..96 {
            let sdk = craft::ui::runtime::focus::focused_rec(&self.ctx).map(|rec| rec.stop.id);
            trace.push(sdk);
            assert!(self.is_open(), "the menu closed during natural navigation: {trace:?}");
            if sdk == Some(row) {
                return;
            }
            self.key(Key::ArrowDown);
        }
        panic!("ArrowDown did not reach {id}, route={}; focus trace: {trace:?}; no forced focus fallback", self.route);
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
        // The framework bar opens a menu on the press: a press on the open title closes it (the native
        // bar's confirmation of a hover-opened menu is gone).
        f.click("menu:Help");
        assert_eq!(f.open_title(), None, "a press on the open title closes its menu");
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
        assert!(!f.is_open());
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
            // Enter opens the focused title's menu; Space stays the app's key (the bar leaves it to the shortcuts)
            if activation == Key::Space {
                assert_eq!(f.open_title(), None, "Space opens no menu");
                f.key(Key::Enter);
            }
            assert_eq!(f.open_title().as_deref(), Some("Edit"));
            assert_eq!(f.app.session.journal.len(), journal, "keyboard title only opens");
            f.arrow_to_item("menuitem:library.selectNone");
            f.key(Key::Enter);
            assert_eq!(f.app.session.journal.get(journal..).unwrap(), &[("library.selectNone".into(), json!({}))]);
            assert_eq!(f.document(), before);
            assert!(!f.is_open());
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
        assert!(!f.is_open());
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
        // The framework bar opens a menu on the press, so a release elsewhere is not what decides it: the menu
        // may stay up. Either way nothing runs, and Escape leaves no menu.
        f.settle();
        assert_eq!(f.document(), before);
        assert_eq!(f.app.session.journal, journal);
        f.key(Key::Escape);
        assert!(!f.is_open());
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
        assert!(!f.is_open());
        assert_eq!(f.app.session.journal, journal);
    }
}

#[test]
fn in_window_titles_narrow_overflow_remains_native_and_full_width_keeps_title_gaps() {
    let mut f = Fixture::new(0);
    // The framework's title face pads each caption on both sides, so the boxes touch and the
    // caption gap is the padding.
    for pair in MENUS.windows(2) {
        assert!(f.rect(&format!("menu:{}", pair[1])).left() - f.rect(&format!("menu:{}", pair[0])).right() >= -0.5);
    }
    for title in MENUS {
        assert!(f.rect(&format!("menu:{title}")).width() >= 30.0, "{title} has room around its caption");
    }
    f.narrow = true;
    // (the bar lays its titles out from the previous pass: it takes a few frames to collapse)
    f.settle();
    f.rect("menu:all");
    assert!(!f.rects.iter().any(|(name, _)| name == "menu:File"));
    let journal = f.app.session.journal.clone();
    f.click("menu:all");
    assert!(f.is_open());
    f.key(Key::Escape);
    assert!(!f.is_open());
    assert_eq!(f.app.session.journal, journal);
}
