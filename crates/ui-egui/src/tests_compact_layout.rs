//! The Compact layout (Settings ▸ General ▸ Layout, `view.compactLayout`): Classic's panels drawn
//! denser. Switching between Classic, Compact and Modern keeps the photo's edit and the open
//! panel; Classic draws exactly as without Compact.

use serde_json::json;

use crate::headless::Headless;
use crate::tests_classic::{T, run};
use crate::{LccApp, Services};

fn develop() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    h.settle(std::time::Duration::from_secs(120));
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    h
}

fn rect(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn exposure(h: &Headless) -> f64 {
    let id = h.app.session.active().expect("a photo");
    h.app.session.develop_of(id).expect("its edit").light.exposure
}

/// One session, several steps; each step was a test of its own and says what it checks.
#[test]
fn compact_is_denser_and_switching_keeps_the_edit_the_panel_and_a_dragged_width() {
    let mut h = develop();
    // compact is denser and switching keeps the edit and the panel
    {
        run(&mut h, "develop.set", json!({"control": "light.exposure", "value": 0.7}));
        let panel = h.app.ui.right;
        let classic_style = h.view.ctx.global_style();
        let classic = rect(&h, "section:light").expect("the Light section header");

        assert_eq!(run(&mut h, "view.compactLayout", json!({"on": true}))["on"], true);
        h.step();
        let compact = rect(&h, "section:light").expect("the Light section header, compact");
        assert!(compact.height() < classic.height(), "compact header {compact:?} vs classic {classic:?}");
        assert!(crate::theme::compact(&h.view.ctx));
        assert_eq!(exposure(&h), 0.7, "the edit stays");
        assert_eq!(h.app.ui.right, panel, "the panel stays");

        // Modern overrides Compact; Compact again turns Modern off
        run(&mut h, "view.modernLayout", json!({"on": true}));
        assert!(!h.app.compact() && !crate::theme::compact(&h.view.ctx));
        run(&mut h, "view.compactLayout", json!({"on": true}));
        assert!(!h.app.ui.modern.enabled && h.app.compact());

        // back to Classic: the very same style and header as before
        run(&mut h, "view.compactLayout", json!({"on": false}));
        h.step();
        assert_eq!(*h.view.ctx.global_style(), *classic_style, "Classic's style is untouched");
        assert_eq!(rect(&h, "section:light").map(|r| r.height()), Some(classic.height()));
        assert_eq!(exposure(&h), 0.7);
        assert_eq!(h.app.ui.right, panel);
    }
    // compact right panel default width
    // The right panel opens at 318 points in Compact while it was at Classic's default, and goes back;
    // a width the user dragged to stays.
    {
        use crate::state::{COMPACT_RIGHT_WIDTH, RIGHT_WIDTH};
        assert_eq!(h.app.ui.right_width, RIGHT_WIDTH.default);
        run(&mut h, "view.compactLayout", json!({"on": true}));
        assert_eq!(h.app.ui.right_width, COMPACT_RIGHT_WIDTH);
        run(&mut h, "view.compactLayout", json!({"on": false}));
        assert_eq!(h.app.ui.right_width, RIGHT_WIDTH.default);
        h.app.ui.right_width = 300.0;
        run(&mut h, "view.compactLayout", json!({"on": true}));
        assert_eq!(h.app.ui.right_width, 300.0, "a dragged width stays");
        for w in [f32::NAN, -1.0, 1e9] {
            let _ = crate::state::follow_compact_width(w, true);
        }
    }
}

/// Draws a few passes so every name column has seen all of its rows.
fn redraw(h: &mut Headless) {
    for _ in 0..3 {
        h.step();
    }
}

#[test]
fn compact_name_columns_fit_each_section_and_a_flyout_never_moves_the_tracks_around_it() {
    let mut h = develop();
    h.app.ui.open_sections = vec!["light".to_string(), "optics".to_string()];
    redraw(&mut h);
    let classic = rect(&h, "slider:light.exposure").expect("Exposure");
    run(&mut h, "view.compactLayout", json!({"on": true}));
    redraw(&mut h);
    let compact = rect(&h, "slider:light.exposure").expect("Exposure, compact");
    // Basic's column fits its own names, shorter than the widest name of every control
    assert!(compact.width() >= classic.width(), "compact track {compact:?} vs classic {classic:?}");
    let before = rect(&h, "slider:optics.distortion").expect("Distortion");
    h.app.ui.toggle_flyout("defringe");
    redraw(&mut h);
    assert!(rect(&h, "slider:optics.defringePurple").is_some(), "Defringe is open");
    let after = rect(&h, "slider:optics.distortion").expect("Distortion");
    assert!((after.left() - before.left()).abs() < 0.01 && (after.width() - before.width()).abs() < 0.01, "{before:?} → {after:?}");
}

#[test]
fn shaded_tracks_run_dark_to_light_and_survive_hostile_positions() {
    use lcc_develop::Shade;
    let t = crate::theme::Tokens::default();
    let lum = |c: egui::Color32| u32::from(c.r()) + u32::from(c.g()) + u32::from(c.b());
    let ramp = |s, x| lum(crate::widgets::shade_color(s, &t, x));
    assert!(ramp(Shade::Ramp, 0.0) < ramp(Shade::Ramp, 1.0));
    assert!(ramp(Shade::ReverseRamp, 0.0) > ramp(Shade::ReverseRamp, 1.0));
    assert!(ramp(Shade::CentreLight, 0.5) > ramp(Shade::CentreLight, 0.0));
    for x in [f32::NAN, f32::INFINITY, -5.0, 9.0] {
        let _ = crate::widgets::shade_color(Shade::Tinted, &t, x);
    }
    assert_eq!(lcc_develop::controls::find("light.exposure").map(|c| c.shade), Some(Shade::Ramp));
}

/// Settings ▸ Generative Fill (where the Remove panel's Generative tool sends you for an API key) opens the section
/// that holds it, with the services' key fields in view, in Classic and in Compact alike.
#[test]
fn settings_open_on_generative_fill_in_classic_and_compact() {
    for compact in [false, true] {
        let mut h = develop();
        run(&mut h, "view.compactLayout", json!({"on": compact}));
        run(&mut h, "app.settings", json!({"tab": "genfill"}));
        h.settle(std::time::Duration::from_secs(60));
        let section = match &h.app.ui.dialog {
            Some(crate::state::Dialog::Settings { tab }) => crate::settings_screen::section_of(tab),
            other => panic!("Settings is open: {other:?}"),
        };
        assert_ne!(section, "general", "compact {compact}: Generative Fill has a section of its own to open");
        let rows = rect(&h, "settings:rows").expect("Settings' rows");
        let key = h.app.widgets.iter().find(|(w, _)| w.contains("genfillKey-")).map(|(_, r)| *r);
        let key = key.unwrap_or_else(|| panic!("compact {compact}: a service's key field is drawn"));
        assert!(rows.intersects(key), "compact {compact}: the key field {key:?} is in view in {rows:?}");
    }
}

/// Develop at the Compact layout (or Classic), Basic and Lens Corrections open, Defringe open, every
/// name column settled.
fn develop_at(compact: bool) -> Headless {
    let mut h = develop();
    run(&mut h, "view.compactLayout", json!({"on": compact}));
    h.app.ui.open_sections = vec!["light".to_string(), "optics".to_string()];
    h.app.ui.toggle_flyout("defringe");
    redraw(&mut h);
    h
}

/// A widget's rect, by its id or an icon button's (`icon:` / `button:` prefixes).
fn button(h: &Headless, id: &str) -> Option<egui::Rect> {
    rect(h, &format!("icon:{id}")).or_else(|| rect(h, &format!("button:{id}")))
}

/// One session, several steps; each step was a test of its own and says what it checks.
#[test]
fn compact_edit_panel_rows_columns_and_tools_line_up() {
    let mut h = develop_at(true);
    // compact starts with basic and its treatment row
    // LAYOUT-DENSE (OneSlips' 23e2bec5c, 703ccb4f9): Compact has no "Edit" header above the sections:
    // Basic's banded header comes first (under the tool profile's row), and Auto · B&W | HDR and white balance
    // are its first rows; Browse Profiles is in the tool row above the panels, as in Classic.
    {
        let basic = rect(&h, "section:light").expect("Basic");
        let auto = button(&h, "auto").expect("Auto");
        let profile = button(&h, "profileBrowser").expect("Browse Profiles");
        assert!(auto.top() >= basic.bottom() - 0.5, "Auto {auto:?} is inside Basic {basic:?}");
        assert!(profile.bottom() <= basic.top() + 0.5, "Browse Profiles {profile:?} is above Basic {basic:?}");
        let wb = button(&h, "wbPicker").expect("the white balance selector");
        assert!(wb.top() >= auto.bottom() - 0.5, "white balance {wb:?} under Auto {auto:?}");
    }
    // compact treatment buttons share one right aligned row
    // Auto, B&W and HDR stand on one row, right-aligned on the rows' right margin.
    {
        let ids = ["auto", "treatment-bw", "hdr"];
        let rects: Vec<egui::Rect> = ids.iter().map(|id| button(&h, id).unwrap_or_else(|| panic!("{id}"))).collect();
        for (id, r) in ids.iter().zip(&rects) {
            assert!((r.center().y - rects[0].center().y).abs() < 1.0, "{id} {r:?} is on Auto's row {:?}", rects[0]);
        }
        assert!(rects[0].right() < rects[1].left() && rects[1].right() < rects[2].left(), "in order: {rects:?}");
        let basic = rect(&h, "section:light").expect("Basic");
        let right = basic.right() - crate::widgets::PAD_R;
        assert!((rects[2].right() - right).abs() <= 1.5, "HDR {:?} ends at the right margin {right}", rects[2]);
    }
    // compact section headers outrank sub tool rows
    // Compact section headers are the loud rows (a band, taller than a sub-tool row) and sub-tool rows
    // the quiet ones; in Classic the boxed sub-tool row is as tall as a section header.
    {
        let (sec, fly) = (rect(&h, "section:optics").expect("Lens Corrections"), rect(&h, "flyout:defringe").expect("Defringe"));
        assert!(sec.height() > fly.height(), "the section header {sec:?} is taller than the sub-tool row {fly:?}");
        // the band stands off the chrome in every theme, so the header reads louder than the rows
        for kind in crate::theme::ThemeKind::ALL {
            let t = crate::theme::Tokens::for_kind(kind);
            assert_ne!(t.section_band, t.chrome, "{kind:?}");
        }
    }
    // compact slider names and tracks share one column
    // A Compact section's slider names end on one column and its tracks start on one, from Basic's
    // first slider to its last; Lens Corrections' rows start on the same left margin.
    {
        let ids = ["light.exposure", "light.contrast", "light.blacks", "color.vibrance"];
        let labels: Vec<egui::Rect> = ids.iter().map(|c| rect(&h, &format!("sliderLabel:{c}")).unwrap_or_else(|| panic!("{c}"))).collect();
        let tracks: Vec<egui::Rect> = ids.iter().map(|c| rect(&h, &format!("slider:{c}")).unwrap_or_else(|| panic!("{c}"))).collect();
        for (l, t) in labels.iter().zip(&tracks) {
            assert!((l.right() - labels[0].right()).abs() < 0.5, "{l:?} vs {:?}", labels[0]);
            assert!((t.left() - tracks[0].left()).abs() < 0.5, "{t:?} vs {:?}", tracks[0]);
            assert!(t.left() > l.right(), "the name {l:?} clears its track {t:?}");
        }
        let basic = rect(&h, "section:light").expect("Basic");
        let left = basic.left() + crate::widgets::PAD_L;
        assert!((labels[0].left() - left).abs() < 0.5, "names start on the left margin: {:?} vs {left}", labels[0]);
        let distortion = rect(&h, "sliderLabel:optics.distortion").expect("Distortion");
        assert!((distortion.left() - left).abs() < 0.5, "{distortion:?}");
    }
    // compact original photo box toggles the unedited view
    // The Compact histogram's Original Photo box shows the unedited photo while ticked
    // (`view.showOriginal`); Classic has no such box.
    {
        use crate::state::BeforeAfter;
        assert!(rect(&h, "check:originalPhoto").is_some(), "the box is under the histogram");
        assert_eq!(h.app.ui.before_after, BeforeAfter::Off);
        let r = h.request("ui.clickWidget", json!({"id": "check:originalPhoto"}), T);
        assert_eq!(r["ok"], true, "{r}");
        redraw(&mut h);
        assert_eq!(h.app.ui.before_after, BeforeAfter::Original, "ticked: the original");
        run(&mut h, "view.showOriginal", json!({"on": true}));
        assert_eq!(h.app.ui.before_after, BeforeAfter::Original, "on stays on");
        let r = h.request("ui.clickWidget", json!({"id": "check:originalPhoto"}), T);
        assert_eq!(r["ok"], true, "{r}");
        redraw(&mut h);
        assert_eq!(h.app.ui.before_after, BeforeAfter::Off, "unticked: the edit again");
    }
    // compact tool row spaces the tools evenly and ends with reset
    // The tool row (OneSlips' fe1eb033d): in Compact every tool and Reset share one evenly spaced row
    // under the histogram, and Reset resets the photo; Classic's row has no Reset of its own.
    {
        let ids = ["toolsEdit", "crop", "remove", "redeye", "masking", "profileBrowser", "toolsReset"];
        let rects: Vec<egui::Rect> = ids.iter().map(|id| button(&h, id).unwrap_or_else(|| panic!("{id}"))).collect();
        let step = rects[1].center().x - rects[0].center().x;
        for pair in rects.windows(2) {
            assert!((pair[0].center().y - pair[1].center().y).abs() < 0.5, "one row: {pair:?}");
            assert!(((pair[1].center().x - pair[0].center().x) - step).abs() < 0.6, "evenly spaced: {rects:?}");
        }
        run(&mut h, "develop.set", json!({"control": "light.exposure", "value": 0.7}));
        let id = if rect(&h, "icon:toolsReset").is_some() { "icon:toolsReset" } else { "button:toolsReset" };
        let r = h.request("ui.clickWidget", json!({"id": id}), T);
        assert_eq!(r["ok"], true, "{r}");
        redraw(&mut h);
        assert_eq!(exposure(&h), 0.0, "Reset reset the edit");
    }
}

/// Classic's Edit panel is as it was without Compact (Auto · B&W | HDR at the top of Basic, as there), the boxed sub-tool row as tall
/// as a section header, no Original Photo box under the histogram and no Reset in the tool row.
#[test]
fn classic_keeps_its_edit_panel_layout() {
    let classic = develop_at(false);
    let basic = rect(&classic, "section:light").expect("Basic");
    let auto = button(&classic, "auto").expect("Auto");
    assert!(auto.top() >= basic.bottom() - 0.5, "Classic too: Auto {auto:?} inside Basic {basic:?} (no Edit line above it)");
    let (sec, fly) = (rect(&classic, "section:optics").expect("Lens Corrections"), rect(&classic, "flyout:defringe").expect("Defringe"));
    assert!(fly.height() >= sec.height() - 10.0, "Classic keeps its boxed sub-tool row: {fly:?} vs {sec:?}");
    assert!(rect(&classic, "check:originalPhoto").is_none(), "Classic's histogram is as it was");
    assert!(button(&classic, "toolsReset").is_none(), "Classic's tool row has no Reset");
}

/// The Compact histogram draws RGB Overlap as three filled areas that add up toward grey where
/// they overlap, not as lines.
#[test]
fn compact_histogram_draws_filled_areas() {
    use lcc_raster::{Histogram, HistogramScale};
    let samples: Vec<[f32; 3]> = (0..4000)
        .map(|i| {
            let x = (i % 100) as f32 / 100.0;
            [x, (x * 0.7 + 0.1).min(1.0), (1.0 - x) * 0.8]
        })
        .collect();
    let hist = Histogram::of_hdr(&samples, 0.0);
    let plot = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(300.0, 120.0));
    let meshes = crate::panels::histogram::soft_fills(plot, &hist, HistogramScale::default());
    assert_eq!(meshes.len(), 3, "one filled area per channel");
    for (m, fill) in meshes.iter().zip(crate::panels::histogram::SOFT_FILLS) {
        assert!(m.indices.len() >= 6, "the area has triangles");
        assert!(m.vertices.iter().all(|v| v.color == fill && v.color.a() == 0), "the fills add up (premultiplied, no alpha)");
        assert!(m.vertices.iter().all(|v| plot.expand(0.5).contains(v.pos)), "inside the plot");
        // a filled area: points on the plot's floor as well as above it
        assert!(m.vertices.iter().any(|v| (v.pos.y - plot.bottom()).abs() < 0.01) && m.vertices.iter().any(|v| v.pos.y < plot.bottom() - 1.0));
    }
    // where all three overlap they add up toward a light grey, as Lightroom Classic's histogram
    let sum = |k: fn(egui::Color32) -> u8| crate::panels::histogram::SOFT_FILLS.iter().map(|c| u32::from(k(*c))).sum::<u32>();
    let (r, g, b) = (sum(|c| c.r()), sum(|c| c.g()), sum(|c| c.b()));
    assert!(r > 160 && g > 160 && b > 160 && r.abs_diff(g) < 30 && g.abs_diff(b) < 30, "{r} {g} {b}");
}

/// Develop at the Compact layout with `sections` open, every name column settled.
fn compact_with(sections: &[&str]) -> Headless {
    let mut h = develop();
    run(&mut h, "view.compactLayout", json!({"on": true}));
    h.app.ui.open_sections = sections.iter().map(|s| s.to_string()).collect();
    redraw(&mut h);
    h
}

/// The right edge of the panel's value column: where the slider values of the open sections end.
fn value_column(h: &Headless) -> f32 {
    h.app.widgets.iter().filter(|(w, _)| w.starts_with("sliderValue:")).map(|(_, r)| r.right()).fold(f32::NEG_INFINITY, f32::max)
}

/// One session, several steps; each step was a test of its own and says what it checks.
#[test]
fn compact_rows_stay_in_the_value_column_whatever_is_open() {
    let mut h = compact_with(&["mixer"]);
    // compact mixer adjust row stays in the column
    // The Color Mixer's Adjust row: its caption and HSL | Color fit the panel's column, the track ending on the value
    // column like the sliders under it (the caption pushed it past the column's right edge).
    {
        let hsl = button(&h, "mixerAdjust-hsl").expect("HSL");
        let color = button(&h, "mixerAdjust-color").expect("Color");
        let right = value_column(&h);
        assert!(right.is_finite(), "the mixer's sliders are drawn");
        assert!(color.right() <= right + 0.5, "Color {color:?} ends on the value column {right}");
        assert!(hsl.left() < color.left() && hsl.width() > 20.0, "HSL {hsl:?} then Color {color:?}");
    }
    // compact treatment row ends on the value column with a scroll bar
    // With every section open (the panel scrolls, so it has a scroll bar) Auto · B&W | HDR still end on the value
    // column, not past it.
    {
        h.app.ui.open_sections =
            ["light", "curve", "mixer", "grading", "detail", "optics", "geometry", "effects", "calibration"].map(str::to_string).to_vec();
        redraw(&mut h);
        let hdr = button(&h, "hdr").expect("HDR");
        let exposure = rect(&h, "sliderValue:light.exposure").expect("Exposure's value");
        assert!(hdr.right() <= exposure.right() + 0.5, "HDR {hdr:?} ends on the value column {exposure:?}");
    }
    // compact upright caption is no larger than the slider names
    // The Transform section's Upright caption reads as a caption: no larger than the slider names, in Compact.
    {
        h.app.ui.open_sections = vec!["geometry".to_string()];
        redraw(&mut h);
        let caption = rect(&h, "label:upright").expect("the Upright caption");
        let name = h.app.widgets.iter().find(|(w, _)| w.starts_with("sliderLabel:geometry.")).map(|(_, r)| *r).expect("a transform slider's name");
        assert!(caption.height() <= name.height() + 0.5, "Upright {caption:?} vs a slider name {name:?}");
    }
}
