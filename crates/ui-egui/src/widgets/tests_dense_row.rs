//! The one-line slider row: its parts, its height, and real egui frames
//! for reset, typing, nudges and hostile values.
use super::{SliderOut, row_parts, slider, take_registry};
use crate::headless::HeadlessView;
use egui::{Event, Key, Modifiers, PointerButton, Rect, pos2, vec2};
use lightcraft_develop::{ControlSpec, controls};

struct Row {
    view: HeadlessView,
    spec: ControlSpec,
    value: f64,
    time: f64,
    width: f32,
}

impl Row {
    fn new(id: &str, value: f64) -> Self {
        let mut r = Row { view: HeadlessView::new(), spec: *controls::find(id).unwrap(), value, time: 0.0, width: 300.0 };
        for _ in 0..2 {
            r.frame(vec![]);
        }
        r
    }
    fn frame(&mut self, events: Vec<Event>) -> SliderOut {
        self.time += 0.02;
        let mut out = SliderOut::default();
        let (spec, value) = (self.spec, self.value);
        self.view.run(HeadlessView::raw_input(vec2(self.width, 100.0), 1.0, self.time, events), |ui| {
            out = slider(ui, &spec, value, true, None);
        });
        if let Some(v) = out.value {
            self.value = v;
        }
        out
    }
    fn rect(&self, part: &str) -> Rect {
        let name = format!("{part}:{}", self.spec.id);
        take_registry(&self.view.ctx).into_iter().find(|(id, _)| *id == name).map(|(_, r)| r).unwrap_or_else(|| panic!("no {name}"))
    }
    fn parts(&mut self) -> (Rect, Rect, Rect) {
        self.frame(vec![]);
        let registry = take_registry(&self.view.ctx);
        let find = |p: &str| registry.iter().find(|(id, _)| *id == format!("{p}:{}", self.spec.id)).unwrap().1;
        (find("sliderLabel"), find("slider"), find("sliderValue"))
    }
    fn click(&mut self, at: egui::Pos2, count: usize) -> Vec<SliderOut> {
        let mut outs = vec![self.frame(vec![Event::PointerMoved(at)])];
        for _ in 0..count {
            for pressed in [true, false] {
                outs.push(self.frame(vec![Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]));
            }
        }
        outs.push(self.frame(vec![]));
        outs
    }
    fn key(&mut self, key: Key) -> SliderOut {
        self.frame(vec![Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }])
    }
}

#[test]
fn a_slider_is_one_row_of_the_dense_height() {
    let mut r = Row::new("light.exposure", 0.0);
    let (label, track, value) = r.parts();
    let h = crate::theme::Tokens::default().slider_row_h;
    assert!((18.0..=22.0).contains(&h), "row height {h}");
    for (name, part) in [("name", label), ("track", track), ("value", value)] {
        assert!((part.height() - h).abs() < 0.5, "{name} spans the row: {part:?}");
        assert!((part.top() - label.top()).abs() < 0.5, "{name} on the same line: {part:?}");
    }
    assert!(label.right() < track.left() && track.right() < value.left(), "name | track | value: {label:?} {track:?} {value:?}");
}

#[test]
fn row_parts_never_overlap_or_vanish_at_any_width() {
    for w in [0.0, 1.0, 10.0, 40.0, 120.0, 250.0, 270.0, 520.0, 4000.0] {
        for name in [0.0, 40.0, 1e9, f32::NAN, f32::INFINITY, -5.0] {
            let p = row_parts(Rect::from_min_size(pos2(0.0, 0.0), vec2(w, 20.0)), name);
            if w >= 1.0 {
                assert!(p.track.width() >= 1.0 - 1e-3, "{w}: a track to divide by: {p:?}");
            }
            assert!(p.label.right() <= p.track.left(), "{w}: {p:?}");
            // every part stays inside the row, and the track never runs into the value
            assert!(p.track.right() <= p.value.left(), "{w}: the track runs into the value: {p:?}");
            for r in [p.label, p.track, p.value] {
                assert!(r.left() >= -1e-3 && r.right() <= w + 1e-3, "{w}: outside the row: {p:?}");
            }
            for r in [p.label, p.track, p.value] {
                assert!(r.min.x.is_finite() && r.max.x.is_finite() && r.width() >= 0.0, "{w}: {p:?}");
            }
        }
    }
}

#[test]
fn double_clicking_the_name_resets() {
    let mut r = Row::new("light.contrast", 40.0);
    let (label, _, _) = r.parts();
    let outs = r.click(label.left_center() + vec2(6.0, 0.0), 2);
    assert!(outs.iter().any(|o| o.reset), "a reset");
    assert_eq!(r.value, 0.0);
}

#[test]
fn dragging_the_name_moves_the_value_from_where_it_is() {
    let mut r = Row::new("light.contrast", 10.0);
    let (label, track, _) = r.parts();
    let from = label.center();
    r.frame(vec![Event::PointerMoved(from)]);
    r.frame(vec![Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
    // a quarter of the track to the right: a quarter of the range (200) up, from 10, not a jump
    let to = from + vec2(track.width() / 4.0, 0.0);
    for i in 1..=10 {
        r.frame(vec![Event::PointerMoved(from + (to - from) * (i as f32 / 10.0))]);
    }
    r.frame(vec![Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }]);
    assert!((r.value - 60.0).abs() <= 3.0, "10 + 50: {}", r.value);
}

#[test]
fn typing_a_value_into_the_row_sets_it() {
    let mut r = Row::new("light.exposure", 0.0);
    let (_, _, value) = r.parts();
    r.click(value.center(), 1);
    r.frame(vec![Event::Text("1.5".into())]);
    let out = r.key(Key::Enter);
    assert_eq!(out.value, Some(1.5));
    assert_eq!(r.value, 1.5);
}

#[test]
fn arrow_keys_and_the_wheel_nudge_a_hovered_or_clicked_row() {
    let mut r = Row::new("light.contrast", 0.0);
    let (_, track, _) = r.parts();
    r.frame(vec![Event::PointerMoved(track.center())]);
    r.key(Key::ArrowUp);
    assert_eq!(r.value, 1.0, "↑ over the row");
    // a click arms the wheel; a notch down is a step down (the click itself set the value there)
    r.click(track.center(), 1);
    let before = r.value;
    r.frame(vec![Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, -120.0),
        phase: egui::TouchPhase::Move,
        modifiers: Modifiers::NONE,
        source: egui::MouseWheelSource::Unknown,
    }]);
    for _ in 0..40 {
        r.frame(vec![]);
    }
    assert!(r.value < before, "the wheel nudged it down: {before} → {}", r.value);
}

#[test]
fn hostile_values_and_widths_never_panic() {
    for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e300, -1e300] {
        for w in [0.0f32, 3.0, 300.0] {
            let mut r = Row::new("wb.temp", 6500.0);
            r.width = w.max(1.0);
            r.value = v;
            let out = r.frame(vec![]);
            assert!(out.value.is_none_or(|x| x.is_finite()), "{v} at {w}: {out:?}");
            // a press and drag over whatever is there
            let at = pos2(w / 2.0, 10.0);
            r.frame(vec![Event::PointerMoved(at)]);
            r.frame(vec![Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
            let out = r.frame(vec![Event::PointerMoved(at + vec2(30.0, 0.0))]);
            assert!(out.value.is_none_or(|x| x.is_finite()), "{v} at {w}: {out:?}");
            r.frame(vec![Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }]);
        }
    }
    // the registry is still there after all that
    let r = Row::new("light.exposure", 0.0);
    let _ = r.rect("slider");
}

/// A short name ends next to its own track, not at the left edge.
#[test]
fn names_are_right_aligned_next_to_their_track() {
    let label = Rect::from_min_max(pos2(14.0, 0.0), pos2(80.0, 20.0));
    assert_eq!(super::name_left(label, 30.0), 50.0);
    assert_eq!(super::name_left(label, 200.0), 14.0, "too wide: starts at the column's left edge");
    for w in [f32::NAN, f32::INFINITY, -5.0] {
        let x = super::name_left(label, w);
        assert!(x.is_finite() && x >= label.left() && x <= label.right(), "{w}: {x}");
    }
    // two rows sharing one name column: the short and the long name end at the same x
    let mut view = HeadlessView::new();
    let specs = [*controls::find("light.exposure").unwrap(), *controls::find("light.whites").unwrap()];
    for i in 0..4 {
        view.run(HeadlessView::raw_input(vec2(320.0, 100.0), 1.0, 0.02 * f64::from(i), vec![]), |ui| {
            for s in &specs {
                slider(ui, s, s.default, true, None);
            }
        });
    }
    let right_of = |text: &str| {
        view.painted_shapes()
            .into_iter()
            .find_map(|s| match s {
                egui::epaint::Shape::Text(t) if t.galley.job.text == text => Some(t.pos.x + t.galley.size().x),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{text} not painted"))
    };
    let (long, short) = (right_of(specs[0].label), right_of(specs[1].label));
    assert!((long - short).abs() < 1.0, "names end together: {} at {long}, {} at {short}", specs[0].label, specs[1].label);
    let track = take_registry(&view.ctx).into_iter().find(|(id, _)| id == "slider:light.whites").unwrap().1;
    assert!(
        track.left() - short <= super::NAME_GAP + 1.0 && track.left() - short >= super::HANDLE_R_HOT,
        "the name ends a small gap before the track, clear of the handle: {short} vs {}",
        track.left()
    );
}

/// A major section's header band stands out from the panel more than a
/// sub-section row (Curve, Color Mixer…), which has no fill of its own at rest, in every theme.
#[test]
fn section_headers_stand_out_more_than_sub_sections() {
    use crate::theme::{ThemeKind, Tokens};
    let dist = |a: egui::Color32, b: egui::Color32| (0..3).map(|i| (i32::from(a[i]) - i32::from(b[i])).abs()).max().unwrap_or(0);
    for kind in ThemeKind::ALL {
        let t = Tokens::for_kind(kind);
        assert!(super::flyout_fill(&t, false).is_none(), "{kind:?}: a sub-section row has no box at rest");
        let band = dist(super::section_fill(&t, false), t.chrome);
        assert!(band >= 10, "{kind:?}: the section band is distinct from the chrome ({band})");
        let hover = super::flyout_fill(&t, true).map_or(0, |c| dist(c, t.chrome));
        assert!(band >= hover, "{kind:?}: even a hovered sub-section stays quieter ({hover} vs {band})");
    }
    const { assert!(super::SECTION_TITLE > super::SUB_TITLE) };
}

/// Corners are at most half of the earlier 4 (controls) and 6 (panels),
/// and egui's own widgets, windows and menus take them from the theme.
#[test]
fn corners_are_at_most_half_the_earlier_radius() {
    use crate::theme::{RADIUS, RADIUS_PANEL, ThemeKind, apply_kind};
    const { assert!(RADIUS <= 2 && RADIUS_PANEL <= 3) };
    for kind in ThemeKind::ALL {
        let ctx = egui::Context::default();
        apply_kind(&ctx, kind);
        let v = ctx.global_style().visuals.clone();
        for w in [v.widgets.inactive, v.widgets.hovered, v.widgets.active, v.widgets.noninteractive] {
            assert!(w.corner_radius.nw <= RADIUS, "{kind:?}: widget corner {:?}", w.corner_radius);
        }
        assert!(v.window_corner_radius.nw <= RADIUS_PANEL && v.menu_corner_radius.nw <= RADIUS_PANEL, "{kind:?}");
    }
}

/// Grey ramps inside the tracks, in every theme.
#[test]
fn shaded_tracks_run_the_way_their_spec_says() {
    use super::shade_color;
    use crate::theme::{ThemeKind, Tokens};
    use lightcraft_develop::Shade;
    let lum = |c: egui::Color32| u32::from(c.r()) + u32::from(c.g()) + u32::from(c.b());
    for kind in ThemeKind::ALL {
        let t = Tokens::for_kind(kind);
        let at = |s: Shade, x: f32| lum(shade_color(s, &t, x).unwrap());
        // "lighter" follows the theme: in the light themes the lighter end is still the lighter grey
        assert!(at(Shade::Ramp, 1.0) > at(Shade::Ramp, 0.0) + 20, "{kind:?}: a ramp is lighter at the right");
        assert!(at(Shade::ReverseRamp, 0.0) > at(Shade::ReverseRamp, 1.0) + 20, "{kind:?}: a reversed ramp is lighter at the left");
        let c = at(Shade::CentreLight, 0.5);
        assert!(c > at(Shade::CentreLight, 0.0) + 20 && c > at(Shade::CentreLight, 1.0) + 20, "{kind:?}: a centre ramp is lightest at the centre");
        let red = shade_color(Shade::Tinted, &t, 1.0).unwrap();
        assert!(red.r() > red.g() + 20, "{kind:?}: the tinted end is red: {red:?}");
        assert!(shade_color(Shade::Plain, &t, 0.3).is_none(), "{kind:?}: a plain track is flat");
        // the centre mark shows on a plain track too (CodeRabbit on #3164: it was the track's colour)
        let tick = lum(super::centre_tick(&t));
        assert!(tick.abs_diff(lum(t.track)) > 60, "{kind:?}: the centre mark stands out on a plain track");
        for x in [f32::NAN, f32::INFINITY, -1.0, 2.0] {
            assert!(shade_color(Shade::Ramp, &t, x).is_some());
        }
    }
    // the kind comes from the develop spec, so mask sliders and new controls follow it
    let shade = |id: &str| lightcraft_develop::controls::find(id).unwrap().shade;
    assert_eq!(shade("light.exposure"), Shade::Ramp);
    assert_eq!(shade("light.contrast"), Shade::ReverseRamp);
    assert_eq!(shade("vignette.amount"), Shade::CentreLight);
    assert_eq!(shade("detail.sharpenAmount"), Shade::Tinted);
    assert_eq!(shade("effects.texture"), Shade::Plain);
    for s in crate::panels::masking::LOCAL {
        let twin = ["light", "effects"].iter().find_map(|g| lightcraft_develop::controls::find(&format!("{g}.{}", s.id)));
        if let Some(twin) = twin {
            assert_eq!(s.shade, twin.shade, "mask {} follows its Edit twin {}", s.id, twin.id);
        }
    }
}

/// Puts the UI language back to English when a test that changed it ends (also when it fails).
struct Language;

impl Language {
    fn set(language: crate::i18n::Locale) -> Self {
        crate::i18n::set_language(language);
        Language
    }
}

impl Drop for Language {
    fn drop(&mut self) {
        crate::i18n::set_language(crate::i18n::Locale::En);
    }
}

/// FredApps on #3155: the name column is as wide as the longest name of its group in the language
/// on screen ("Dunst entfernen" in German, "Dehaze" in English), measured in the row's own font.
#[test]
fn the_name_column_fits_the_longest_name_in_the_current_language() {
    let ids = ["effects.texture", "effects.clarity", "effects.dehaze"];
    let column = |language: crate::i18n::Locale| {
        let _language = Language::set(language);
        let mut view = HeadlessView::new();
        let specs: Vec<ControlSpec> = ids.iter().map(|id| *controls::find(id).unwrap()).collect();
        let mut longest = 0.0f32;
        let mut registry = Vec::new();
        for frame in 0..3 {
            view.run(HeadlessView::raw_input(vec2(320.0, 200.0), 1.0, f64::from(frame) * 0.02, vec![]), |ui| {
                super::begin_slider_group(ui, "effects");
                for spec in &specs {
                    slider(ui, spec, 0.0, true, None);
                }
                // the longest name in this language, in the row's own font
                let font = crate::theme::Tokens::get(ui.ctx()).font(super::ROW_FONT);
                longest = specs
                    .iter()
                    .map(|s| ui.painter().layout_no_wrap(crate::i18n::tr(s.label).to_string(), font.clone(), egui::Color32::WHITE).size().x.ceil())
                    .fold(0.0f32, f32::max);
            });
            // (the registry keeps every frame's rects: the last frame's are the ones to check)
            registry = take_registry(&view.ctx);
        }
        let widths: Vec<f32> = ids
            .iter()
            .map(|id| registry.iter().find(|(r, _)| *r == format!("sliderLabel:{id}")).map(|(_, r)| r.width()).unwrap_or_else(|| panic!("no {id}")))
            .collect();
        for w in &widths {
            assert!((w - longest.max(super::NAME_MIN)).abs() < 0.5, "{language:?}: column {w} for a longest name of {longest}: {widths:?}");
        }
        longest
    };
    let (en, de) = (column(crate::i18n::Locale::En), column(crate::i18n::Locale::De));
    assert!(de > en + 10.0, "the German column is the wider one: {de} against {en}");
}

/// FredApps on #3155: a name too long for its column is cut with an ellipsis, and hovering it
/// shows the whole name.
#[test]
fn a_name_too_long_for_its_column_ends_in_an_ellipsis_and_shows_whole_on_hover() {
    let long = "Highlights Luminance Saturation Balance";
    let spec = *controls::find("light.exposure").unwrap();
    let mut view = HeadlessView::new();
    let mut label: Option<Rect> = None;
    let mut texts = Vec::new();
    let mut cut = None;
    let mut moved = false;
    for frame in 0..60 {
        // the pointer comes to rest on the name once (a move every frame would keep the tooltip away)
        let events = match label {
            Some(l) if !moved => {
                moved = true;
                vec![Event::PointerMoved(l.center())]
            }
            _ => vec![],
        };
        view.run(HeadlessView::raw_input(vec2(220.0, 200.0), 1.0, f64::from(frame) * 0.05, events), |ui| {
            slider(ui, &spec, 0.0, true, Some(long));
        });
        texts = view.painted_text();
        cut = view.painted_shapes().into_iter().find_map(|shape| match shape {
            egui::epaint::Shape::Text(t) if t.galley.job.text == long => Some((t.galley.elided, t.galley.size().x)),
            _ => None,
        });
        label = take_registry(&view.ctx).into_iter().find(|(id, _)| id == "sliderLabel:light.exposure").map(|(_, r)| r);
    }
    let label = label.expect("the name's rect");
    assert!(label.width() < 220.0 * 0.5, "the column is capped: {label:?}");
    let (elided, drawn_w) = cut.expect("the name is drawn");
    assert!(elided && drawn_w <= label.width() + 0.5, "the name is cut with an ellipsis to its column: {drawn_w} in {label:?}");
    assert!(texts.iter().any(|t| t != long && t.contains(long)), "hovering shows the whole name (with the reset hint): {texts:?}");
}
