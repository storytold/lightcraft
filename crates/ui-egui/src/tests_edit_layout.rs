//! The Edit panel at its narrowest with a long profile applied: the profile row fits (#1759).

use egui::{Rect, vec2};
use lightcraft_develop::WbMode;
use serde_json::json;

use crate::{LightcraftApp, Services, headless::HeadlessView};

const LONG_ID: &str = "lut:kodak-portra-400-plus-one-stop-overexposed";
const LONG_NAME: &str = "Kodak Portra 400 Plus One Stop Overexposed";

fn text_shapes(shape: &egui::Shape, out: &mut Vec<egui::epaint::TextShape>) {
    match shape {
        egui::Shape::Text(text) => out.push(text.clone()),
        egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| text_shapes(s, out)),
        _ => {}
    }
}

fn panel(width: f32) -> (Vec<egui::epaint::TextShape>, Vec<(String, Rect)>) {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    app.session.lut_profiles.push(lightcraft_engine::cmd::lut_profiles::LutProfile {
        id: LONG_ID.into(),
        name: LONG_NAME.into(),
        group: "Film Stocks".into(),
        file: String::new(),
    });
    app.run("develop.profile", json!({"id": LONG_ID})).unwrap();
    let photo = app.session.active().unwrap();
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    crate::theme::apply(&ctx);
    let mut texts = Vec::new();
    let mut widgets = Vec::new();
    for frame in 0..3 {
        let raw = HeadlessView::raw_input(vec2(width, 4000.0), 1.0, frame as f64 / 60.0, vec![]);
        let mut out = ctx.run_ui(raw, |ui| {
            egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                crate::panels::edit::show(&mut app, ui, photo);
            });
        });
        out.textures_delta.clear();
        texts.clear();
        for shape in &out.shapes {
            text_shapes(&shape.shape, &mut texts);
        }
        widgets = crate::widgets::take_registry(&ctx);
    }
    (texts, widgets)
}

/// Every widget and text the Color and Effects sections draw, with the Color Mixer open and white balance `mode`, in a
/// `width` pt right panel: drawn through `right::show`, so its border and scroll area take their share of the width as
/// in the app. The last item is the White Balance label's full width, before any cutting short.
fn color_panel(width: f32, mode: WbMode) -> (Vec<(String, Rect)>, Vec<egui::epaint::TextShape>, f32) {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    app.ui.right = crate::state::RightPanel::Edit;
    app.ui.toggle_section("color");
    app.ui.toggle_section("effects");
    app.ui.toggle_flyout("mixer");
    app.run("develop.wb", json!({"mode": serde_json::to_value(mode).unwrap()})).unwrap();
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    crate::theme::apply(&ctx);
    let mut widgets = Vec::new();
    let mut texts = Vec::new();
    for frame in 0..3 {
        let raw = HeadlessView::raw_input(vec2(width, 4000.0), 1.0, frame as f64 / 60.0, vec![]);
        let mut out = ctx.run_ui(raw, |ui| {
            egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                crate::panels::right::show(&mut app, ui);
            });
        });
        out.textures_delta.clear();
        texts.clear();
        for shape in &out.shapes {
            text_shapes(&shape.shape, &mut texts);
        }
        widgets = crate::widgets::take_registry(&ctx);
    }
    let t = crate::theme::Tokens::get(&ctx);
    let painter = egui::Painter::new(ctx.clone(), egui::LayerId::background(), Rect::EVERYTHING);
    let label_w = painter.layout_no_wrap(crate::i18n::tr("White Balance").to_string(), t.font(13.0), t.text_dim).size().x;
    (widgets, texts, label_w)
}

/// A row wider than the panel widens everything below it (egui grows the content to its widest row), so the sliders
/// and buttons run past the right edge. The Color Mixer's Hue / Saturation / Luminance row did that in longer
/// languages, Portuguese *Matiz / Saturação / Luminância* among them, and the White Balance row with a long mode.
#[test]
fn the_color_section_leaves_the_edit_panel_its_width_in_every_language() {
    use crate::i18n::Locale;
    let mut past = Vec::new();
    let mut wrapped = Vec::new();
    for &locale in Locale::ALL {
        crate::i18n::set_language(locale);
        for width in [crate::state::RIGHT_WIDTH.min, crate::state::RIGHT_WIDTH.default] {
            // Custom included: the menu doesn't offer it, but dragging Temp or Tint sets it
            for mode in WbMode::ALL {
                let at = format!("{locale:?} {width} {mode:?}");
                let (widgets, texts, label_full) = color_panel(width, mode);
                let rect = |id: &str| widgets.iter().find(|(w, _)| w == id).unwrap_or_else(|| panic!("{at}: {id} not drawn")).1;
                let text = |s: &str| texts.iter().find(|t| t.galley.job.text == s).unwrap_or_else(|| panic!("{at}: {s} not drawn"));
                for (id, r) in &widgets {
                    if r.right() > width + 0.5 {
                        past.push(format!("{at}: {id} ends at {}", r.right()));
                    }
                }
                // White Balance: label, mode and eyedropper stay on one row whatever the mode, and the label gives way
                // before the mode is cut short
                let (menu, picker) = (rect("dropdown:wbMode"), rect("icon:wbPicker"));
                assert!(menu.right() <= picker.left(), "{at}: the white balance menu {menu:?} runs under its eyedropper {picker:?}");
                let label = text(crate::i18n::tr("White Balance"));
                let label_rect = label.visual_bounding_rect();
                assert!(menu.y_range().contains(label_rect.center().y), "{at}: the label {label_rect:?} left the menu's row {menu:?}");
                assert!(menu.y_range().contains(picker.center().y), "{at}: the eyedropper {picker:?} left the menu's row {menu:?}");
                // the mode's text inside its menu (the Edit header also has an Auto)
                let shown = texts
                    .iter()
                    .find(|t| t.galley.job.text == crate::i18n::tr(mode.label()) && menu.contains_rect(t.visual_bounding_rect()))
                    .unwrap_or_else(|| panic!("{at}: the mode is not drawn inside its menu {menu:?}"));
                // the mode is cut short only once the label is down to its 40 pt floor, and the label only when its whole
                // text, the menu and the eyedropper (6 pt item gaps, `theme::apply`) don't fit the row
                if shown.galley.elided {
                    assert!(label_rect.width() <= 40.5, "{at}: the mode is cut short while its label {label_rect:?} has room to give");
                }
                if label.galley.elided {
                    let needed = label_full + 6.0 + menu.width() + 6.0 + picker.width();
                    let row = picker.right() - label_rect.left();
                    assert!(needed > row - 0.5, "{at}: the label is cut short though {needed} pt fits the {row} pt row");
                }
                if locale == Locale::En && width == crate::state::RIGHT_WIDTH.default {
                    assert!(!shown.galley.elided, "{at}: English modes show whole in the default panel");
                    if mode == WbMode::AsShot {
                        assert!(!label.galley.elided, "{at}: English As Shot keeps its whole label");
                    }
                }
                if mode != WbMode::AsShot {
                    continue;
                }
                // Color Mixer: the targeted-adjustment row wraps, each later row starting after the icon like the first
                let buttons = ["button:tatMixer-hue", "button:tatMixer-sat", "button:tatMixer-lum"].map(rect);
                let first = buttons[0];
                for b in buttons.iter().filter(|b| b.top() > first.top()) {
                    let row_start = buttons.iter().filter(|o| (o.top() - b.top()).abs() < 0.5).map(|o| o.left()).fold(f32::INFINITY, f32::min);
                    assert!(
                        (row_start - first.left()).abs() < 0.5,
                        "{at}: a wrapped row starts at {row_start}, not after the icon at {}",
                        first.left()
                    );
                }
                if buttons[2].top() > first.top() {
                    wrapped.push((locale, width));
                }
            }
        }
    }
    crate::i18n::set_language(Locale::En);
    assert!(past.is_empty(), "widgets past the panel's right edge:\n{}", past.join("\n"));
    // the gaps narrow before a button wraps: English keeps one row in the default panel, Portuguese wraps in the narrowest
    assert!(!wrapped.contains(&(Locale::En, crate::state::RIGHT_WIDTH.default)), "{wrapped:?}");
    assert!(wrapped.contains(&(Locale::PtBr, crate::state::RIGHT_WIDTH.min)), "{wrapped:?}");
}

#[test]
fn a_long_profile_name_leaves_the_edit_panel_its_width() {
    let width = crate::state::RIGHT_WIDTH.min;
    let (texts, widgets) = panel(width);
    let rect = |id: &str| widgets.iter().find(|(w, _)| w == id).unwrap_or_else(|| panic!("{id} not drawn")).1;
    let dropdown = rect("dropdown:profile");
    let browse = rect("icon:profileBrowser");
    assert!(dropdown.right() <= browse.left(), "the profile menu {dropdown:?} runs into Browse Profiles {browse:?}");
    let name = texts.iter().find(|t| t.galley.job.text == LONG_NAME).unwrap();
    assert!(name.galley.elided, "a name wider than the row is cut short");
    assert!(name.visual_bounding_rect().right() <= dropdown.right(), "the name leaves its menu");
    // every slider drawn, from the profile's Amount down
    rect("sliderValue:profile.amount");
    rect("sliderValue:light.exposure");
    for (id, r) in widgets.iter().filter(|(id, _)| id.starts_with("slider")) {
        assert!(r.right() <= width, "{id} {r:?} is cut off by the {width} pt panel");
    }
}
