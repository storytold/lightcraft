//! Finite More-menu regressions. No native picker, file read/write, renderer or GPU harness.
//! Prepared source only under external-light-preset-options-20261119T2318; execution is delegated.

use super::*;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::headless::HeadlessView;
use crate::widgets::{WidgetHit, take_registries};
use egui::{Event, Key, Modifiers, PointerButton, Pos2};

#[derive(Clone, Copy)]
enum Pass {
    Real,
    Measure,
    Sizing,
}

struct Frames {
    ctx: egui::Context,
    app: LightcraftApp,
    time: f64,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, WidgetHit)>,
    nodes: HashMap<String, egui::accesskit::Node>,
    imports: Rc<Cell<usize>>,
    exports: Rc<Cell<usize>>,
}

impl Frames {
    fn new(user: bool) -> Self {
        let imports = Rc::new(Cell::new(0));
        let exports = Rc::new(Cell::new(0));
        let import_count = imports.clone();
        let export_count = exports.clone();
        // Synchronous cancellation counts every dispatch (no pending-picker deduplication).
        let services = crate::Services {
            pick_preset_files: Some(Box::new(move || {
                import_count.set(import_count.get() + 1);
                vec![]
            })),
            save_preset_file: Some(Box::new(move |_| {
                export_count.set(export_count.get() + 1);
                None
            })),
            write: Some(Box::new(|_, _| panic!("cancelled chooser must never write"))),
            ..Default::default()
        };
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), services);
        assert!(app.session.presets.iter().all(|p| p.builtin));
        if user {
            app.session.add_presets(vec![Preset {
                id: "user.external-preset-options-20261119".into(),
                name: "Menu-only fixture".into(),
                group: "Private menu fixture".into(),
                settings: json!({"light": {"exposure": 0.25}}),
                favorite: false,
                builtin: false,
            }]);
        }
        app.ui.preset_thumbs = false;
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        ctx.enable_accesskit();
        app.install_framework(&ctx);
        let mut frames = Self { ctx, app, time: 0.0, rects: vec![], hits: vec![], nodes: HashMap::new(), imports, exports };
        frames.settle();
        frames
    }

    fn frame(&mut self, events: Vec<Event>, pass: Pass) {
        let _ = take_registries(&self.ctx);
        self.time += 0.04;
        let mut raw = HeadlessView::raw_input(vec2(360.0, 600.0), 1.0, self.time, events);
        raw.events.insert(0, Event::ModifiersChanged(Modifiers::NONE));
        let app = &mut self.app;
        let mut out = self.ctx.run_ui(raw, |ui| match pass {
            Pass::Real => super::show(app, ui),
            Pass::Measure => {
                let ctx = ui.ctx().clone();
                let focus = ctx.memory(|m| m.focused());
                craft::ui::runtime::measure::run(&ctx, || super::show(app, ui));
                assert_eq!(ctx.memory(|m| m.focused()), focus, "SDK measurement moved focus");
            }
            Pass::Sizing => {
                let focus = ui.memory(|m| m.focused());
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                assert!(child.is_sizing_pass());
                assert!(!craft::ui::runtime::measure::active(child.ctx()), "exercise native sizing independently");
                super::show(app, &mut child);
                assert_eq!(ui.memory(|m| m.focused()), focus, "native sizing moved focus");
            }
        });
        out.textures_delta.clear();
        if let Some(update) = out.platform_output.accesskit_update {
            for (_, node) in update.nodes {
                if let Some(label) = node.label() {
                    self.nodes.insert(label.to_owned(), node);
                }
            }
        }
        (self.rects, self.hits) = take_registries(&self.ctx);
    }

    fn settle(&mut self) {
        for _ in 0..8 {
            self.frame(vec![], Pass::Real);
        }
    }

    fn rect(&self, id: &str) -> Rect {
        self.rects.iter().find(|(name, _)| name == id).unwrap_or_else(|| panic!("missing {id}: {:?}", self.rects)).1
    }

    fn hit(&self, id: &str) -> WidgetHit {
        let hits: Vec<_> = self.hits.iter().filter(|(name, _)| name == id).collect();
        assert_eq!(hits.len(), 1, "one real response registration for {id}");
        hits[0].1
    }

    fn visible(&self) -> bool {
        self.hits.iter().any(|(name, _)| name == "presetMenu:thumbnails")
    }

    fn click_at(&mut self, pos: Pos2, hold: f64) {
        self.frame(vec![Event::PointerMoved(pos)], Pass::Real);
        self.frame(vec![pointer(pos, true)], Pass::Real);
        if hold > 0.0 {
            self.time += hold;
            self.frame(vec![], Pass::Real);
        }
        self.frame(vec![pointer(pos, false)], Pass::Real);
        self.settle();
    }

    fn click(&mut self, id: &str) {
        self.click_at(self.rect(id).center(), 0.0);
    }

    fn key(&mut self, key: Key) {
        self.frame(key_events(key), Pass::Real);
        self.settle();
    }

    fn focus(&mut self, id: &str) {
        // Natural keyboard travel into the real menu group: Tab enters it and the arrows move between its rows.
        // No focus request: the SDK's own focus record must name the row, or its key activation is not the row's.
        let wanted = self.hit(id).id;
        let mut path = Vec::new();
        for _ in 0..16 {
            let focused = craft::ui::runtime::focus::focused_rec(&self.ctx).map(|rec| rec.stop.id);
            if focused == Some(wanted) {
                return;
            }
            path.push(focused);
            let in_menu = self.hits.iter().any(|(name, hit)| name.starts_with("presetMenu:") && Some(hit.id) == focused);
            self.key(if in_menu { Key::ArrowDown } else { Key::Tab });
        }
        panic!("natural Tab/arrow travel never reached {id}: {path:?}");
    }

    fn snapshot(&self) -> serde_json::Value {
        json!({
            "commits": self.app.session.commits(),
            "undo": self.app.session.undo.len(),
            "redo": self.app.session.redo.len(),
            "presets": self.app.session.presets.iter().map(|p| (&p.id, &p.name, &p.group, &p.settings, p.builtin, p.favorite)).collect::<Vec<_>>(),
            "active": self.app.session.active(),
            "selected": self.app.session.selection.ids,
        })
    }

    fn assert_no_file_requests(&self) {
        assert_eq!(self.imports.get(), 0);
        assert_eq!(self.exports.get(), 0);
        assert!(self.app.pending_picks.is_empty());
    }
}

fn pointer(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
}

fn key_events(key: Key) -> Vec<Event> {
    [true, false].map(|pressed| Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE }).to_vec()
}

#[test]
fn external_preset_options_real_header_owner_long_release_keys_escape_and_outside() {
    for activation in [None, Some(Key::Enter), Some(Key::Space)] {
        let mut f = Frames::new(false);
        let before = f.snapshot();
        let opener = f.hit("icon:presetMore");
        if let Some(key) = activation {
            f.focus("icon:presetMore");
            f.key(key);
        } else {
            f.click_at(f.rect("icon:presetMore").center(), 2.0);
        }
        assert!(f.visible(), "{activation:?}");
        assert_eq!(f.hit("icon:presetMore").id, opener.id);
        assert_eq!(f.hit("icon:presetMore").layer_id, opener.layer_id);
        let mut owners = std::collections::HashSet::new();
        for id in ["presetMenu:thumbnails", "presetMenu:import", "presetMenu:export"] {
            let row = f.hit(id);
            assert!(owners.insert(row.id), "rows must not alias");
            assert_ne!(row.id, opener.id);
            assert_ne!(row.layer_id, opener.layer_id, "popup must own a separate layer");
            assert!(row.interact_rect.contains(f.rect(id).center()));
        }
        f.key(Key::Escape);
        assert!(!f.visible());
        assert_eq!(f.ctx.memory(|m| m.focused()), Some(opener.id), "Escape returns to the actual hdr response");
        f.click("icon:presetMore");
        assert!(f.visible());
        f.click_at(egui::pos2(5.0, 590.0), 0.0);
        assert!(!f.visible(), "outside click dismisses");
        assert_eq!(f.snapshot(), before);
        f.assert_no_file_requests();
    }
}

#[test]
fn external_preset_options_thumbnail_is_ui_only_single_toggle_and_keeps_open() {
    for key in [None, Some(Key::Enter), Some(Key::Space)] {
        let mut f = Frames::new(false);
        let before = f.snapshot();
        f.click("icon:presetMore");
        if let Some(key) = key {
            f.focus("presetMenu:thumbnails");
            f.key(key);
        } else {
            f.click("presetMenu:thumbnails");
        }
        assert!(f.app.ui.preset_thumbs, "exactly one toggle, not both ran and response.clicked");
        assert!(f.visible(), "checkbox-equivalent stays open");
        f.settle();
        assert!(f.app.ui.preset_thumbs, "idle passes do not replay activation");
        f.click("presetMenu:thumbnails");
        assert!(!f.app.ui.preset_thumbs);
        assert!(f.visible());
        assert_eq!(f.snapshot(), before);
        f.assert_no_file_requests();
    }
}

#[test]
fn external_preset_options_cancelled_file_commands_dispatch_once_and_close() {
    for (target, importing) in [("presetMenu:import", true), ("presetMenu:export", false)] {
        for key in [None, Some(Key::Enter), Some(Key::Space)] {
            let mut f = Frames::new(true);
            let before = f.snapshot();
            f.click("icon:presetMore");
            if let Some(key) = key {
                f.focus(target);
                f.key(key);
            } else {
                f.click(target);
            }
            assert!(!f.visible(), "a file command dismisses the menu before a chooser");
            assert_eq!(f.imports.get(), usize::from(importing));
            assert_eq!(f.exports.get(), usize::from(!importing));
            f.settle();
            assert_eq!(f.imports.get() + f.exports.get(), 1, "no idle replay or SDK/egui double dispatch");
            assert!(f.app.pending_picks.is_empty(), "all choosers cancel synchronously");
            assert_eq!(f.snapshot(), before);
        }
    }
}

#[test]
fn external_preset_options_builtin_only_export_is_disabled_and_never_dispatches() {
    let mut f = Frames::new(false);
    let before = f.snapshot();
    f.click("icon:presetMore");
    assert!(f.nodes.get("Export User Presets…").expect("named export accessibility node").is_disabled());
    f.click("presetMenu:export");
    assert!(f.visible(), "disabled row does not pick or dismiss");
    // A disabled row cannot own keyboard focus. Its mnemonic must not pick it either.
    f.frame(vec![Event::Text("e".into())], Pass::Real);
    f.settle();
    assert!(f.visible());
    assert_ne!(f.ctx.memory(|m| m.focused()), Some(f.hit("presetMenu:export").id));
    f.assert_no_file_requests();
    assert_eq!(f.snapshot(), before);
}

#[test]
fn external_preset_options_measure_and_native_sizing_have_no_effects_or_targets() {
    for pass in [Pass::Measure, Pass::Sizing] {
        for target in ["presetMenu:thumbnails", "presetMenu:import", "presetMenu:export"] {
            let mut f = Frames::new(true);
            f.click("icon:presetMore");
            let before = f.snapshot();
            let thumbs = f.app.ui.preset_thumbs;
            let pos = f.rect(target).center();
            let hit = f.hit(target);
            f.ctx.memory_mut(|m| m.request_focus(hit.id));
            f.frame(vec![Event::PointerMoved(pos)], pass);
            f.frame(vec![pointer(pos, true)], pass);
            f.frame(vec![pointer(pos, false)], pass);
            f.frame(key_events(Key::Enter), pass);
            f.frame(key_events(Key::Space), pass);
            assert_eq!(f.app.ui.preset_thumbs, thumbs);
            assert_eq!(f.snapshot(), before);
            f.assert_no_file_requests();
            assert!(!f.rects.iter().any(|(id, _)| id.starts_with("presetMenu:")));
            assert!(!f.hits.iter().any(|(id, _)| id.starts_with("presetMenu:")));
        }
    }
}
