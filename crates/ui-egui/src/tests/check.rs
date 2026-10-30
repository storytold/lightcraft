//! The tick box and the radio: what a click and Space do, that a disabled one ignores both, what
//! screen readers hear, that they read in every theme, and that migrated controls are reachable by
//! their automation ids.

use std::time::Duration;

use egui::{Color32, Pos2, Rect, vec2};
use serde_json::json;

use crate::button::{State, contrast};
use crate::check::{Check, box_look, check, radio};
use crate::headless::Headless;
use crate::theme::{ThemeKind, Tokens};
use crate::{LightcraftApp, Services};

/// A context kept across frames, so focus and pointer state carry from one to the next.
struct Rig {
    ctx: egui::Context,
}

impl Rig {
    fn new() -> Self {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.enable_accesskit();
        Rig { ctx }
    }

    /// One frame with `events`; returns what `draw` made and the AccessKit nodes' (role, name, toggled); a Label's text is its value, reported as the name.
    fn frame<T>(&self, events: Vec<egui::Event>, draw: impl FnOnce(&mut egui::Ui) -> T) -> (T, Vec<(String, String, Option<String>)>) {
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))), events, ..Default::default() };
        let mut out_val = None;
        let mut draw = Some(draw);
        let mut out = self.ctx.run_ui(raw, |ui| out_val = draw.take().map(|d| d(ui)));
        out.textures_delta.clear();
        let nodes = out
            .platform_output
            .accesskit_update
            .take()
            .map(|u| {
                u.nodes
                    .iter()
                    .map(|(_, n)| {
                        (
                            format!("{:?}", n.role()),
                            n.label().or_else(|| n.value()).unwrap_or_default().to_string(),
                            n.toggled().map(|t| format!("{t:?}")),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        (out_val.expect("the frame ran"), nodes)
    }
}

fn click_at(at: Pos2) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(at),
        egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() },
        egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() },
    ]
}

fn space() -> Vec<egui::Event> {
    vec![egui::Event::Key {
        key: egui::Key::Space,
        physical_key: Some(egui::Key::Space),
        pressed: true,
        repeat: false,
        modifiers: Default::default(),
    }]
}

#[test]
fn a_click_toggles_a_check_and_the_response_says_changed() {
    let rig = Rig::new();
    let mut on = false;
    let (r, _) = rig.frame(Vec::new(), |ui| check(ui, "gpu", &mut on, "Use the GPU"));
    let (r, _) = rig.frame(click_at(r.rect.center()), |ui| check(ui, "gpu", &mut on, "Use the GPU"));
    assert!(r.changed() && on, "a click turns it on");
    let (r, _) = rig.frame(click_at(r.rect.center()), |ui| check(ui, "gpu", &mut on, "Use the GPU"));
    assert!(r.changed() && !on, "and another turns it off");
}

#[test]
fn space_toggles_the_focused_check_and_radio() {
    let rig = Rig::new();
    let (mut on, mut choice) = (false, 1);
    let draw = |ui: &mut egui::Ui, on: &mut bool, choice: &mut i32| {
        let c = check(ui, "a", on, "Alpha");
        let r = radio(ui, "b-2", choice, 2, "Two");
        (c, r)
    };
    let ((c, r), _) = rig.frame(Vec::new(), |ui| draw(ui, &mut on, &mut choice));
    rig.ctx.memory_mut(|m| m.request_focus(c.id));
    rig.frame(Vec::new(), |ui| draw(ui, &mut on, &mut choice));
    rig.frame(space(), |ui| draw(ui, &mut on, &mut choice));
    assert!(on, "Space turned the focused check on");
    assert_eq!(choice, 1, "and left the radio alone");
    rig.ctx.memory_mut(|m| m.request_focus(r.id));
    rig.frame(Vec::new(), |ui| draw(ui, &mut on, &mut choice));
    rig.frame(space(), |ui| draw(ui, &mut on, &mut choice));
    assert_eq!(choice, 2, "Space picked the focused radio");
}

#[test]
fn a_radio_picks_its_value_once() {
    let rig = Rig::new();
    let mut choice = "a";
    let draw =
        |ui: &mut egui::Ui, choice: &mut &str| ui.horizontal(|ui| (radio(ui, "r-a", choice, "a", "A"), radio(ui, "r-b", choice, "b", "B"))).inner;
    let ((_, b), _) = rig.frame(Vec::new(), |ui| draw(ui, &mut choice));
    let ((_, b), _) = rig.frame(click_at(b.rect.center()), |ui| draw(ui, &mut choice));
    assert!(b.changed());
    assert_eq!(choice, "b");
    let ((_, b), _) = rig.frame(click_at(b.rect.center()), |ui| draw(ui, &mut choice));
    assert!(!b.changed(), "clicking the chosen one again changes nothing");
    assert_eq!(choice, "b");
}

#[test]
fn a_disabled_check_and_radio_ignore_clicks_and_keys() {
    let rig = Rig::new();
    let (mut on, mut choice) = (false, 0);
    let draw = |ui: &mut egui::Ui, on: &mut bool, choice: &mut i32| {
        let c = Check::new("d", on, "Off").enabled(false).tooltip("Needs a model").show(ui);
        let r = crate::check::Radio::new("e", choice, 1, "Radio").enabled(false).show(ui);
        (c, r)
    };
    let ((c, r), _) = rig.frame(Vec::new(), |ui| draw(ui, &mut on, &mut choice));
    for target in [c.rect.center(), r.rect.center()] {
        rig.frame(click_at(target), |ui| draw(ui, &mut on, &mut choice));
    }
    rig.ctx.memory_mut(|m| m.request_focus(c.id));
    rig.frame(space(), |ui| draw(ui, &mut on, &mut choice));
    assert!(!on && choice == 0, "nothing changed");
    // and a screen reader is told so
    let (_, nodes) = rig.frame(Vec::new(), |ui| draw(ui, &mut on, &mut choice));
    assert!(nodes.iter().any(|n| n.1 == "Off"), "still announced: {nodes:?}");
}

#[test]
fn a_disabled_check_and_radio_say_why_on_hover() {
    let rig = Rig::new();
    rig.ctx.all_styles_mut(|s| s.interaction.tooltip_delay = 0.0);
    let (mut on, mut choice) = (false, 0);
    let draw = |ui: &mut egui::Ui, on: &mut bool, choice: &mut i32| {
        let c = Check::new("d", on, "Off").enabled(false).tooltip("Needs a model").show(ui);
        let r = crate::check::Radio::new("e", choice, 1, "Radio").enabled(false).tooltip("Not now").show(ui);
        (c, r)
    };
    let ((c, r), _) = rig.frame(Vec::new(), |ui| draw(ui, &mut on, &mut choice));
    for target in [c.rect.center(), r.rect.center()] {
        let hover = vec![egui::Event::PointerMoved(target)];
        for _ in 0..3 {
            rig.frame(hover.clone(), |ui| draw(ui, &mut on, &mut choice));
        }
        let ((c, r), _) = rig.frame(Vec::new(), |ui| draw(ui, &mut on, &mut choice));
        assert!(c.is_tooltip_open() || r.is_tooltip_open(), "the reason shows on hover at {target:?}");
    }
}

/// A reason for being off (the face switch without a model) is for the disabled box only: the
/// same box, on and enabled, shows nothing.
#[test]
fn a_disabled_reason_shows_only_while_disabled() {
    for (enabled, shown) in [(false, true), (true, false)] {
        let rig = Rig::new();
        rig.ctx.all_styles_mut(|s| s.interaction.tooltip_delay = 0.0);
        let mut on = true;
        let draw = |ui: &mut egui::Ui, on: &mut bool| Check::new("d", on, "Faces").enabled(enabled).disabled_reason("Choose a model").show(ui);
        let (c, _) = rig.frame(Vec::new(), |ui| draw(ui, &mut on));
        let hover = vec![egui::Event::PointerMoved(c.rect.center())];
        for _ in 0..3 {
            rig.frame(hover.clone(), |ui| draw(ui, &mut on));
        }
        let (c, _) = rig.frame(Vec::new(), |ui| draw(ui, &mut on));
        assert_eq!(c.is_tooltip_open(), shown, "enabled = {enabled}");
    }
}

/// A label is an English key that the box translates once; one that is already translated (a
/// `tr_format!` result) is drawn and announced as it is, not looked up again.
#[test]
fn a_translated_label_is_not_looked_up_again() {
    let rig = Rig::new();
    crate::i18n::set_language(crate::i18n::Locale::De);
    let mut on = false;
    let (_, nodes) = rig.frame(Vec::new(), |ui| {
        Check::new("key", &mut on, "Auto Crop").show(ui);
        // "Auto Crop" stands for text that was translated already and happens to be a catalog key.
        Check::new("ready", &mut on, "Auto Crop").translated().show(ui);
    });
    crate::i18n::set_language(crate::i18n::Locale::En);
    let names: Vec<&str> = nodes.iter().filter(|n| n.0 == "CheckBox").map(|n| n.1.as_str()).collect();
    let german = crate::i18n::Locale::De.tr("Auto Crop");
    assert!(names.contains(&german), "an English key is translated: {names:?}");
    assert!(names.contains(&"Auto Crop"), "a translated label is kept as it is: {names:?}");
}

#[test]
fn screen_readers_hear_a_checkbox_and_a_radio_with_their_state() {
    let rig = Rig::new();
    let mut choice = 3;
    let mut on = true;
    let (_, nodes) = rig.frame(Vec::new(), |ui| {
        check(ui, "x", &mut on, "Auto Crop");
        radio(ui, "y-3", &mut choice, 3, "Spherical");
        radio(ui, "y-4", &mut choice, 4, "Cylindrical");
    });
    let find = |name: &str| nodes.iter().find(|n| n.1 == name).cloned().unwrap_or_default();
    let (check_node, picked, other) = (find("Auto Crop"), find("Spherical"), find("Cylindrical"));
    assert_eq!(check_node.0, "CheckBox", "{nodes:?}");
    assert_eq!(check_node.2.as_deref(), Some("True"));
    assert_eq!(picked.0, "RadioButton");
    assert_eq!(picked.2.as_deref(), Some("True"));
    assert_eq!(other.2.as_deref(), Some("False"));
}

/// The frame a click turns a row on or off already reports the new state, not the one before it.
#[test]
fn screen_readers_hear_the_new_state_in_the_frame_a_click_changes_it() {
    let rig = Rig::new();
    let (mut on, mut choice) = (false, 3);
    let draw = |ui: &mut egui::Ui, on: &mut bool, choice: &mut i32| {
        let c = check(ui, "x", on, "Auto Crop");
        let r4 = radio(ui, "y-4", choice, 4, "Cylindrical");
        (c, r4)
    };
    let ((c, r4), _) = rig.frame(Vec::new(), |ui| draw(ui, &mut on, &mut choice));
    let toggled = |nodes: &[(String, String, Option<String>)], name: &str| nodes.iter().find(|n| n.1 == name).and_then(|n| n.2.clone());
    let (_, nodes) = rig.frame(click_at(c.rect.center()), |ui| draw(ui, &mut on, &mut choice));
    assert!(on);
    assert_eq!(toggled(&nodes, "Auto Crop").as_deref(), Some("True"), "ticked by this very click: {nodes:?}");
    let (_, nodes) = rig.frame(click_at(c.rect.center()), |ui| draw(ui, &mut on, &mut choice));
    assert!(!on);
    assert_eq!(toggled(&nodes, "Auto Crop").as_deref(), Some("False"), "and cleared by the next: {nodes:?}");
    let (_, nodes) = rig.frame(click_at(r4.rect.center()), |ui| draw(ui, &mut on, &mut choice));
    assert_eq!(choice, 4);
    assert_eq!(toggled(&nodes, "Cylindrical").as_deref(), Some("True"), "chosen by this very click: {nodes:?}");
}

/// A choice button (one of a row of Format or Color space buttons) reports itself chosen in the frame of the click that chose it.
#[test]
fn a_choice_button_says_it_is_chosen_in_the_frame_of_its_click() {
    let rig = Rig::new();
    let mut chosen = false;
    let draw = |ui: &mut egui::Ui, chosen: &mut bool| {
        let r = crate::widgets::choice_button(ui, "format-jpeg", "Format JPEG", *chosen);
        if r.clicked() {
            *chosen = true;
        }
        r
    };
    let (r, _) = rig.frame(Vec::new(), |ui| draw(ui, &mut chosen));
    let (_, nodes) = rig.frame(click_at(r.rect.center()), |ui| draw(ui, &mut chosen));
    assert!(chosen);
    let toggled = nodes.iter().find(|n| n.1 == "Format JPEG").and_then(|n| n.2.clone());
    assert_eq!(toggled.as_deref(), Some("True"), "chosen by this very click: {nodes:?}");
}

#[test]
fn a_check_is_addressable_by_its_id() {
    let rig = Rig::new();
    let mut on = false;
    rig.frame(Vec::new(), |ui| {
        check(ui, "settings.gpu", &mut on, "Use the GPU");
        radio(ui, "theme-paper", &mut 0, 0, "Paper");
    });
    let ids: Vec<String> = crate::widgets::take_registry(&rig.ctx).into_iter().map(|(id, _)| id).collect();
    assert!(ids.contains(&"check:settings.gpu".to_string()) && ids.contains(&"radio:theme-paper".to_string()), "{ids:?}");
}

#[test]
fn the_box_reads_in_every_theme_and_state() {
    for kind in ThemeKind::ALL {
        let t = Tokens::for_kind(kind);
        for state in [State::Idle, State::Hovered, State::Pressed] {
            let on = box_look(&t, true, state);
            assert!(contrast(on.mark, on.fill) >= 4.5, "{kind:?} {state:?}: the tick on the accent {:.2}:1", contrast(on.mark, on.fill));
            let off = box_look(&t, false, state);
            for ground in [t.chrome, t.canvas] {
                assert!(contrast(off.outline, ground) >= 3.0, "{kind:?} {state:?}: the empty box's outline {:.2}:1", contrast(off.outline, ground));
            }
        }
        // and a disabled one is fainter than an idle one
        let (idle, off) = (box_look(&t, true, State::Idle), box_look(&t, true, State::Disabled));
        assert!(off.fill.a() < idle.fill.a() || off.fill != idle.fill);
        assert_ne!(Color32::TRANSPARENT, off.mark);
    }
}

// ------------------------------------------------------------------------ migrated controls

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn ask(h: &mut Headless, method: &str, params: serde_json::Value) {
    let r = h.request(method, params.clone(), T);
    assert_eq!(r["ok"], true, "{method} {params}: {r}");
    h.settle(SETTLE);
}

fn keywords_panel() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    ask(&mut h, "ui.set", json!({"view": "photoGrid", "right": "keywords"}));
    h
}

/// A dialog's checkbox (Edit Keyword Set's "Save as a new set") is reached by `check:…` and a click
/// turns the choice on; the Keywording view switch (a toggle row) by `button:…`.
#[test]
fn a_dialog_checkbox_and_a_toggle_are_reachable_by_id() {
    let mut h = keywords_panel();
    h.app.session.execute("keyword.saveSet", &json!({"name": "Weddings", "keywords": ["ceremony"]})).unwrap();
    h.settle(SETTLE);
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordSetCombo"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordSetMenu:edit"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "check:keywordSetAsNew"}));
    let Some(crate::state::Dialog::KeywordSet { as_new, .. }) = h.app.ui.dialog.clone() else { panic!("{:?}", h.app.ui.dialog) };
    assert!(as_new, "the click ticked the box");
    ask(&mut h, "ui.clickWidget", json!({"id": "button:dialogCancel"}));

    ask(&mut h, "ui.clickWidget", json!({"id": "button:keywordView-containing"}));
    assert_eq!(h.app.ui.keywording_view, crate::state::KeywordingView::Containing);
}

/// The Synchronize dialog draws its count beside the box; screen readers still hear it with the
/// (translated) label, as when the count was part of the label.
#[test]
fn the_synchronize_choices_announce_their_count() {
    use crate::state::{Dialog, SyncCounts};
    let rig = Rig::new();
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut dlg = Dialog::SynchronizeFolder {
        path: "/p".into(),
        name: "p".into(),
        disk: false,
        counts: Some(SyncCounts { new: 7, ..Default::default() }),
        import_new: true,
        relink_moved: false,
        remove_missing: false,
        read_metadata: false,
    };
    crate::i18n::set_language(crate::i18n::Locale::De);
    let (_, nodes) = rig.frame(Vec::new(), |ui| crate::sync::body(&app, ui, &mut dlg));
    crate::i18n::set_language(crate::i18n::Locale::En);
    let names: Vec<&str> = nodes.iter().filter(|n| n.0 == "CheckBox").map(|n| n.1.as_str()).collect();
    let label = {
        crate::i18n::set_language(crate::i18n::Locale::De);
        let l = crate::i18n::tr("Import new photos").to_string();
        crate::i18n::set_language(crate::i18n::Locale::En);
        l
    };
    assert!(names.contains(&format!("{label} (7)").as_str()), "label and count are announced together: {names:?}");
    let separate: Vec<_> = nodes.iter().filter(|n| n.1 == "(7)").collect();
    assert!(separate.is_empty(), "the count is not announced a second time on its own: {separate:?}");
}

/// Every rectangle the frame painted, as (fill, stroke colour, stroke width).
fn painted_rects(out: &egui::FullOutput) -> Vec<(Color32, Color32, f32)> {
    fn walk(shape: &egui::Shape, into: &mut Vec<(Color32, Color32, f32)>) {
        match shape {
            egui::Shape::Rect(r) => into.push((r.fill, r.stroke.color, r.stroke.width)),
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, into)),
            _ => {}
        }
    }
    let mut rects = Vec::new();
    out.shapes.iter().for_each(|c| walk(&c.shape, &mut rects));
    rects
}

/// One frame of `draw`, returning what it made and what it painted.
fn paint_frame<T>(rig: &Rig, events: Vec<egui::Event>, draw: impl FnOnce(&mut egui::Ui) -> T) -> (T, Vec<(Color32, Color32, f32)>) {
    let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))), events, ..Default::default() };
    let mut val = None;
    let mut draw = Some(draw);
    let mut out = rig.ctx.run_ui(raw, |ui| val = draw.take().map(|d| d(ui)));
    out.textures_delta.clear();
    (val.expect("the frame ran"), painted_rects(&out))
}

/// Keyboard focus shows on the buttons that were migrated to `text_button`, `text_button_sized`
/// and `swatch_toggle` (keymap, keyword view, soft proof), whether they are idle, hovered or
/// selected, and an unfocused one paints no ring: the pressed and hover fills are unchanged.
#[test]
fn a_focused_button_swatch_and_icon_button_show_the_focus_ring() {
    type Draw = fn(&mut egui::Ui, bool) -> egui::Response;
    let draws: [(&str, Draw); 4] = [
        ("text_button", |ui, on| crate::widgets::text_button(ui, "a", "Auto", on)),
        ("text_button_sized", |ui, on| crate::widgets::text_button_sized(ui, "b", "Name", on, 80.0, 24.0, None)),
        ("swatch_toggle", |ui, on| crate::widgets::swatch_toggle(ui, "c", Color32::RED, on, "Out of gamut")),
        ("icon_button", |ui, on| crate::widgets::icon_button(ui, "d", crate::icons::Icon::Eye, vec2(32.0, 32.0), on, true, "Eye")),
    ];
    for (name, draw) in draws {
        let rig = Rig::new();
        let t = Tokens::get(&rig.ctx);
        let ring = |rects: &[(Color32, Color32, f32)]| rects.iter().any(|r| r.1 == t.accent && r.2 == 2.0);
        let fill = |rects: &[(Color32, Color32, f32)], c: Color32| rects.iter().any(|r| r.0 == c);
        // idle, unfocused: no ring
        let (resp, rects) = paint_frame(&rig, Vec::new(), |ui| draw(ui, false));
        assert!(!ring(&rects), "{name}: no ring without focus");
        // idle, focused: ring
        rig.ctx.memory_mut(|m| m.request_focus(resp.id));
        let (_, rects) = paint_frame(&rig, Vec::new(), |ui| draw(ui, false));
        assert!(ring(&rects), "{name}: a focused idle button shows the ring");
        // focused and selected: ring on top of the selected fill
        let (_, rects) = paint_frame(&rig, Vec::new(), |ui| draw(ui, true));
        assert!(ring(&rects), "{name}: a focused selected button shows the ring");
        // unfocused and selected: the pressed fill, no ring
        rig.ctx.memory_mut(|m| m.surrender_focus(resp.id));
        let (_, rects) = paint_frame(&rig, Vec::new(), |ui| draw(ui, true));
        assert!(!ring(&rects), "{name}: a selected button without focus has no ring");
        if name != "icon_button" {
            assert!(fill(&rects, t.pressed), "{name}: selected keeps the pressed fill");
            // unfocused and hovered: the hover fill, no ring
            let hover = vec![egui::Event::PointerMoved(resp.rect.center())];
            paint_frame(&rig, hover.clone(), |ui| draw(ui, false));
            let (_, rects) = paint_frame(&rig, hover, |ui| draw(ui, false));
            assert!(fill(&rects, t.hover), "{name}: hover keeps the hover fill");
            assert!(!ring(&rects), "{name}: a hovered button without focus has no ring");
        }
    }
}

/// The segmented control's segments take the focus ring too.
#[test]
fn a_focused_segment_shows_the_focus_ring() {
    let rig = Rig::new();
    let t = Tokens::get(&rig.ctx);
    let draw = |ui: &mut egui::Ui| crate::widgets::segmented(ui, "seg", &[("One", "one"), ("Two", "two")], Some(0), 2);
    let (_, rects) = paint_frame(&rig, Vec::new(), draw);
    assert!(!rects.iter().any(|r| r.1 == t.accent && r.2 == 2.0), "no ring without focus");
    // Tab reaches the first segment
    let tab = vec![egui::Event::Key {
        key: egui::Key::Tab,
        physical_key: Some(egui::Key::Tab),
        pressed: true,
        repeat: false,
        modifiers: Default::default(),
    }];
    paint_frame(&rig, tab, draw);
    let (_, rects) = paint_frame(&rig, Vec::new(), draw);
    assert!(rects.iter().any(|r| r.1 == t.accent && r.2 == 2.0), "the focused segment shows the ring");
}

/// The rectangles of the focus rings (2 px accent strokes) a frame painted.
fn ring_rects(rig: &Rig, events: Vec<egui::Event>, draw: impl FnOnce(&mut egui::Ui)) -> Vec<Rect> {
    fn walk(shape: &egui::Shape, accent: Color32, into: &mut Vec<Rect>) {
        match shape {
            egui::Shape::Rect(r) if r.stroke.color == accent && r.stroke.width == 2.0 => into.push(r.rect),
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, accent, into)),
            _ => {}
        }
    }
    let accent = Tokens::get(&rig.ctx).accent;
    let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))), events, ..Default::default() };
    let mut draw = Some(draw);
    let mut out = rig.ctx.run_ui(raw, |ui| draw.take().map(|d| d(ui)).unwrap_or_default());
    out.textures_delta.clear();
    let mut rings = Vec::new();
    out.shapes.iter().for_each(|c| walk(&c.shape, accent, &mut rings));
    rings
}

fn tab() -> Vec<egui::Event> {
    vec![egui::Event::Key { key: egui::Key::Tab, physical_key: Some(egui::Key::Tab), pressed: true, repeat: false, modifiers: Default::default() }]
}

/// Star ratings, drop-down buttons and section headers (with their eye) show the focus ring when
/// Tab reaches them, and not before.
#[test]
fn stars_dropdowns_and_section_headers_show_the_focus_ring() {
    type Draw = fn(&mut egui::Ui);
    let draws: [(&str, Draw); 3] = [
        ("stars", |ui| {
            crate::rating::Rating::new("r", 2).show(ui);
        }),
        ("dropdown", |ui| {
            let t = Tokens::get(ui.ctx());
            crate::widgets::dropdown(ui, "d", "Sort", t.semibold(12.0), t.text);
        }),
        ("section_header", |ui| {
            crate::widgets::section_header(ui, "s", "Light", true, None);
        }),
    ];
    for (name, draw) in draws {
        let rig = Rig::new();
        assert!(ring_rects(&rig, Vec::new(), draw).is_empty(), "{name}: no ring before focus");
        ring_rects(&rig, tab(), draw);
        assert_eq!(ring_rects(&rig, Vec::new(), draw).len(), 1, "{name}: one ring on the focused control");
    }
}

/// The drop-down's ring stands clear of its text, which starts at the control's left edge.
#[test]
fn the_dropdown_ring_does_not_cut_the_text() {
    let rig = Rig::new();
    let draw = |ui: &mut egui::Ui| {
        let t = Tokens::get(ui.ctx());
        crate::widgets::dropdown(ui, "d", "Sort", t.semibold(12.0), t.text).rect
    };
    let (mut rect, mut rings) = (Rect::NOTHING, Vec::new());
    ring_rects(&rig, Vec::new(), |ui| rect = draw(ui));
    ring_rects(&rig, tab(), |ui| rect = draw(ui));
    rings.extend(ring_rects(&rig, Vec::new(), |ui| rect = draw(ui)));
    let ring = rings.first().expect("a ring");
    assert!(ring.min.x + 2.0 <= rect.min.x, "the 2 px ring ends before the text: ring {ring:?}, control {rect:?}");
}

/// A section header's eye toggle takes the focus after the header, and shows its own ring.
#[test]
fn a_focused_section_eye_shows_the_focus_ring() {
    let rig = Rig::new();
    let draw = |ui: &mut egui::Ui| {
        crate::widgets::section_header(ui, "s", "Light", true, Some(false));
    };
    ring_rects(&rig, Vec::new(), draw);
    ring_rects(&rig, tab(), draw);
    let header = ring_rects(&rig, Vec::new(), draw);
    assert_eq!(header.len(), 1, "the header has the focus first");
    ring_rects(&rig, tab(), draw);
    let eye = ring_rects(&rig, Vec::new(), draw);
    assert_eq!(eye.len(), 1, "the eye has it next");
    assert!(eye[0].width() < 30.0 && header[0].width() > 100.0, "a small ring on the eye, not the header: {eye:?} / {header:?}");
}

/// A keymap row's shortcut button is 110 pt wide and cuts a long spelled-out shortcut with "…", so
/// pointer users get the whole shortcut on hover. One that fits (an unset "—", "Press keys…" in English) has nothing more to say.
#[test]
fn a_keymap_shortcut_shows_its_full_text_on_hover() {
    let shortcut = "Ctrl+Shift+Alt+Right";
    let bindable = crate::shortcuts::bindable().first().copied().expect("a bindable command");
    let button = format!("button:shortcut-{}", bindable.id);
    // the button's own accessible name is the shortcut too: only a tooltip adds a second node with it
    let nodes_with = |nodes: &[(String, String, Option<String>)], text: &str| nodes.iter().filter(|n| n.1 == text).count();
    let hover = |sc: Option<&str>, recording: bool| {
        let rig = Rig::new();
        rig.ctx.all_styles_mut(|s| s.interaction.tooltip_delay = 0.0);
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
        if recording {
            app.recording_shortcut = Some(bindable.id.to_string());
        }
        let t = Tokens::get(&rig.ctx);
        let mut draw = |events: Vec<egui::Event>| {
            let (_, nodes) = rig.frame(events, |ui| crate::panels::keymap::row(&mut app, ui, &t, &bindable, sc, false));
            nodes
        };
        draw(Vec::new());
        let at = crate::widgets::take_registry(&rig.ctx).into_iter().find(|r| r.0 == button).expect("the shortcut button is registered").1.center();
        let moved = vec![egui::Event::PointerMoved(at)];
        for _ in 0..3 {
            draw(moved.clone());
        }
        draw(Vec::new())
    };
    let nodes = hover(Some(shortcut), false);
    assert_eq!(nodes_with(&nodes, shortcut), 2, "the button's name and the tooltip both say {shortcut}: {nodes:?}");
    let nodes = hover(Some("Ctrl+Z"), false);
    assert_eq!(nodes_with(&nodes, "Ctrl+Z"), 1, "a shortcut that fits has no tooltip: {nodes:?}");
    let nodes = hover(None, false);
    assert_eq!(nodes_with(&nodes, "—"), 1, "an unset shortcut has no tooltip: {nodes:?}");
    let nodes = hover(Some(shortcut), true);
    let press = crate::i18n::tr("Press keys…");
    assert_eq!(nodes_with(&nodes, shortcut), 0, "a shortcut being recorded is not shown: {nodes:?}");
    assert_eq!(nodes_with(&nodes, press), 1, "\"Press keys…\" fits the button and has no tooltip: {nodes:?}");
}
