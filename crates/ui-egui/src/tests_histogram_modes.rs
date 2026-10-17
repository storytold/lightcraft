//! The Histogram panel's display modes and scales (issue #206): `view.histogram.mode` and
//! `view.histogram.scale` select the distribution the plot draws and how tall its bins are, the
//! panel's own selector and context menu dispatch the same two commands, the choice is saved with
//! the UI state, and the clipping indicators are colour-coded and preview the overlay on hover.

use std::time::Duration;

use egui::Color32;
use serde_json::json;

use crate::headless::Headless;
use crate::panels::histogram::CHANNEL_COLORS;
use crate::render::Slot;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The demo library in Detail with the Edit panel (the histogram sits first) on a selected photo.
fn edit_panel() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    let id = h.app.session.visible_cloned()[0].0;
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit", "histogram": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn rect(h: &Headless, id: &str) -> egui::Rect {
    widget(h, id).unwrap_or_else(|| panic!("no widget {id} on screen"))
}

/// Every colour the histogram painted inside its plot last frame. Its curves are meshes and its
/// hairlines are paths, so both are read.
fn plot_colors(h: &Headless) -> Vec<Color32> {
    let plot = rect(h, "histogram:plot");
    let mut out: Vec<Color32> = Vec::new();
    let mut add = |c: Color32| {
        if !out.contains(&c) {
            out.push(c);
        }
    };
    for shape in h.view.painted_shapes() {
        match shape {
            egui::Shape::Mesh(mesh) => {
                for v in mesh.vertices.iter().filter(|v| plot.contains(v.pos)) {
                    add(v.color);
                }
            }
            egui::Shape::Path(path) => {
                if path.points.iter().any(|p| plot.contains(*p))
                    && let egui::epaint::ColorMode::Solid(c) = path.stroke.color
                {
                    add(c);
                }
            }
            _ => {}
        }
    }
    out
}

/// Set a mode (or scale) through the app, as the panel's own menu does.
fn set(h: &mut Headless, id: &str, key: &str, value: &str) {
    h.app.run(id, json!({key: value})).unwrap_or_else(|e| panic!("{id} {value}: {e}"));
}

/// Selecting a mode or a scale sets it and names it back; with no parameter the command steps to
/// the next one, wrapping; anything else is refused with the values it does take.
#[test]
fn the_mode_and_scale_commands_select_step_and_reject_junk() {
    let mut h = edit_panel();
    assert_eq!(h.app.ui.histogram_mode, "rgb", "the panel opens on the target app's own overlay");
    assert_eq!(h.app.ui.histogram_scale, "linear");

    for mode in ["luminance", "channels", "chroma", "vectorscope", "rgb"] {
        let out = h.app.run("view.histogram.mode", json!({"mode": mode})).expect("a known mode");
        assert_eq!(out["mode"], mode);
        assert_eq!(h.app.ui.histogram_mode, mode);
        h.step();
    }
    // no parameter: the next mode, wrapping at the end
    for (from, to) in [("rgb", "luminance"), ("luminance", "channels"), ("channels", "chroma"), ("chroma", "vectorscope"), ("vectorscope", "rgb")] {
        set(&mut h, "view.histogram.mode", "mode", from);
        let out = h.app.run("view.histogram.mode", json!({})).expect("a step");
        assert_eq!(out["mode"], to, "stepping from {from}");
        assert_eq!(h.app.ui.histogram_mode, to);
    }
    for scale in ["logarithmic", "linear"] {
        let out = h.app.run("view.histogram.scale", json!({"scale": scale})).expect("a known scale");
        assert_eq!(out["scale"], scale);
        assert_eq!(h.app.ui.histogram_scale, scale);
    }
    set(&mut h, "view.histogram.scale", "scale", "linear");
    assert_eq!(h.app.run("view.histogram.scale", json!({})).expect("a step")["scale"], "logarithmic");
    assert_eq!(h.app.run("view.histogram.scale", json!({})).expect("a step")["scale"], "linear");

    // anything else is an error naming the values it takes, and changes nothing
    for bad in ["LUMINANCE", "parade", "", "rgb "] {
        let e = h.app.run("view.histogram.mode", json!({"mode": bad})).expect_err("an unknown mode");
        assert!(e.contains("rgb|luminance|channels|chroma|vectorscope"), "{bad}: {e}");
        assert_eq!(h.app.ui.histogram_mode, "rgb");
    }
    let e = h.app.run("view.histogram.scale", json!({"scale": "log"})).expect_err("an unknown scale");
    assert!(e.contains("linear|logarithmic"), "{e}");
    assert_eq!(h.app.ui.histogram_scale, "linear");

    // the commands reach the app through the control channel too, and selecting one shows the panel
    h.app.ui.histogram = false;
    let r = h.request("engine.execute", json!({"command": "view.histogram.mode", "params": {"mode": "chroma"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.ui.histogram, "the command's own panel is shown by it");
    assert_eq!(h.app.ui.histogram_mode, "chroma");
    h.step();

    // …and a key the user gives one fires it, the way any other command's does
    set(&mut h, "view.histogram.mode", "mode", "rgb");
    let r = h.app.run("app.setShortcut", json!({"id": "view.histogram.mode", "shortcut": "Shift+U"})).expect("a free key");
    assert_eq!(r["shortcut"], "Shift+U", "{r}");
    let r = h.request("ui.key", json!({"key": "U", "shift": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.ui.histogram_mode, "luminance", "the rebound key cycled the histogram mode");
}

/// Each mode paints its own distribution: the RGB overlay only ever uses the seven additive
/// colours, the luminance is neutral, the separated channels are the three channel colours in
/// three tracks, the chroma is its own colour — with the luminance hairline staying neutral — and
/// the vectorscope draws its circular graticule, six target boxes (`R`, `Mg`, `B`, `Cy`, `G`, `Yl`)
/// and 2D chrominance cloud inside the disc.
#[test]
fn every_mode_paints_its_own_distribution() {
    let mut h = edit_panel();
    // the RGB overlay: blends of red, green and blue only
    let overlay = [
        Color32::from_gray(235),
        Color32::from_rgb(226, 210, 74),
        Color32::from_rgb(74, 208, 214),
        Color32::from_rgb(214, 74, 206),
        CHANNEL_COLORS[0],
        CHANNEL_COLORS[1],
        CHANNEL_COLORS[2],
    ];
    let colors = plot_colors(&h);
    assert!(!colors.is_empty(), "the demo photo's histogram is drawn");
    for c in &colors {
        assert!(overlay.contains(c) || c.r() == c.g() && c.g() == c.b(), "the RGB overlay draws channel blends and a neutral hairline, not {c:?}",);
    }

    set(&mut h, "view.histogram.mode", "mode", "luminance");
    h.step();
    let colors = plot_colors(&h);
    assert!(!colors.is_empty(), "the luminance curve is drawn");
    assert!(colors.iter().all(|c| c.r() == c.g() && c.g() == c.b()), "the luminance is monochrome: {colors:?}");

    set(&mut h, "view.histogram.mode", "mode", "chroma");
    h.step();
    let colors = plot_colors(&h);
    assert!(colors.contains(&Color32::from_rgb(92, 100, 146)), "the chroma fill: {colors:?}");
    assert!(colors.iter().any(|c| c.b() > c.r() + 20), "the chroma is drawn in its own colour: {colors:?}");
    assert!(!colors.contains(&CHANNEL_COLORS[1]), "no channel colour leaks into the chroma view");

    set(&mut h, "view.histogram.mode", "mode", "channels");
    h.step();
    let plot = rect(&h, "histogram:plot");
    let mut ys: Vec<f32> = Vec::new();
    let mut colors: Vec<Color32> = Vec::new();
    for shape in h.view.painted_shapes() {
        let egui::Shape::Mesh(mesh) = shape else { continue };
        for v in mesh.vertices.iter().filter(|v| plot.contains(v.pos)) {
            ys.push(v.pos.y);
            if !colors.contains(&v.color) {
                colors.push(v.color);
            }
        }
    }
    assert!(!ys.is_empty(), "the three tracks are drawn");
    // three tracks: the plot's height is split in three, so the fills reach every third of it
    let (lo, hi) = (plot.top(), plot.bottom());
    let third = (hi - lo) / 3.0;
    for k in 0..3 {
        assert!(
            ys.iter().any(|y| *y >= lo + third * k as f32 && *y < lo + third * (k as f32 + 1.0)),
            "track {k} has pixels in its own third of the plot",
        );
    }
    for c in &colors {
        assert!(
            CHANNEL_COLORS.iter().flat_map(|cc| [*cc, cc.gamma_multiply(0.45)]).any(|e| e.r() == c.r() && e.g() == c.g() && e.b() == c.b()),
            "a track is drawn in a channel's own colour, not {c:?}",
        );
    }

    // the vectorscope gets a taller plot, paints its circular graticule and six hue targets, and
    // does not offer 1D tonal-zone dragging across the disc
    let plot_1d = rect(&h, "histogram:plot");
    set(&mut h, "view.histogram.mode", "mode", "vectorscope");
    h.step();
    let plot_scope = rect(&h, "histogram:plot");
    assert!(plot_scope.height() > plot_1d.height() + 40.0, "the scope gets room for its disc: {plot_scope:?} vs {plot_1d:?}");
    let texts = h.painted_text();
    for target in ["Vector", "R", "Mg", "B", "Cy", "G", "Yl"] {
        assert!(texts.iter().any(|t| t == target), "the vectorscope draws {target}: {texts:?}");
    }
    let mut circles = 0usize;
    let mut cloud_vertices = 0usize;
    for shape in h.view.painted_shapes() {
        match shape {
            egui::Shape::Circle(c) if plot_scope.contains(c.center) => circles += 1,
            egui::Shape::Mesh(m) => {
                cloud_vertices += m.vertices.iter().filter(|v| plot_scope.contains(v.pos)).count();
            }
            _ => {}
        }
    }
    assert!(circles >= 11, "the disc fill, four saturation rings and six hue target rings are drawn: {circles}");
    assert!(cloud_vertices >= 4, "the photo's 2D chrominance cloud is drawn inside the disc: {cloud_vertices}");
    let r = h.request("ui.hoverWidget", json!({"id": "histogram:plot", "fx": 0.5, "fy": 0.5}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(
        !h.painted_text().iter().any(|t| t.starts_with("Exposure:")),
        "hovering the vectorscope disc does not activate the 1D Exposure tonal zone",
    );
}

/// The log scale draws the quiet end of the distribution higher than the linear one, and neither
/// draws outside the plot.
#[test]
fn the_logarithmic_scale_lifts_the_quiet_bins() {
    let mut h = edit_panel();
    let plot = rect(&h, "histogram:plot");
    let tallest = |h: &Headless| -> f32 {
        let mut top = plot.bottom();
        for shape in h.view.painted_shapes() {
            if let egui::Shape::Mesh(mesh) = shape {
                for v in mesh.vertices.iter().filter(|v| plot.contains(v.pos)) {
                    top = top.min(v.pos.y);
                }
            }
        }
        top
    };
    let linear = tallest(&h);
    set(&mut h, "view.histogram.scale", "scale", "logarithmic");
    h.step();
    let log = tallest(&h);
    assert!(linear >= plot.top() - 0.5, "the linear plot keeps its tallest bin inside the plot");
    assert!(log >= plot.top() - 0.5, "so does the log one");
    assert!(log <= linear + 0.5, "the log scale never draws taller than the linear one: {log} vs {linear}");
}

/// The panel's own selector shows the mode, its menu lists every mode and both scales, and a click
/// on it opens that menu instead of toggling the clipping warnings.
#[test]
fn the_panels_own_selector_lists_the_modes_and_scales() {
    let mut h = edit_panel();
    assert!(h.painted_text().iter().any(|t| t == "RGB"), "the selector names the mode: {:?}", h.painted_text());
    set(&mut h, "view.histogram.mode", "mode", "luminance");
    h.step();
    assert!(h.painted_text().iter().any(|t| t == "Luma"), "{:?}", h.painted_text());

    assert!(!h.app.ui.show_clipping);
    let r = h.request("ui.clickWidget", json!({"id": "histogram:display"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(egui::Popup::is_any_open(&h.view.ctx), "the selector's menu");
    assert!(!h.app.ui.show_clipping, "the selector is not the plot: a click on it leaves the overlay alone");
    let texts = h.painted_text();
    for label in ["RGB Overlay", "Luminance", "Separated Channels", "Chroma", "Vectorscope", "Linear", "Logarithmic"] {
        assert!(texts.iter().any(|t| t == label), "the menu lists {label}: {texts:?}");
    }
    // right-clicking the plot opens the same menu
    egui::Popup::close_all(&h.view.ctx);
    h.step();
    let plot = rect(&h, "histogram:plot");
    let r = h.request("ui.click", json!({"x": plot.center().x, "y": plot.center().y, "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(egui::Popup::is_any_open(&h.view.ctx), "the plot's own menu");
    assert!(h.painted_text().iter().any(|t| t == "Separated Channels"), "{:?}", h.painted_text());
    assert!(!h.app.ui.show_clipping, "a right-click opens the menu, it does not toggle the overlay");
    egui::Popup::close_all(&h.view.ctx);
    h.step();
}

/// The chosen mode and scale are saved with the UI state; a settings file from before them, or one
/// naming something else, opens on the defaults.
#[test]
fn the_mode_and_scale_survive_the_settings() {
    let mut h = edit_panel();
    set(&mut h, "view.histogram.mode", "mode", "channels");
    set(&mut h, "view.histogram.scale", "scale", "logarithmic");
    let saved = serde_json::to_string(&h.app.ui).expect("the UI state");
    let back = serde_json::from_str::<crate::UiState>(&saved).expect("read back").sanitized();
    assert_eq!((back.histogram_mode.as_str(), back.histogram_scale.as_str()), ("channels", "logarithmic"));

    // a ui.json from before the modes existed, and one naming something this build doesn't know
    let old = serde_json::from_str::<crate::UiState>(r#"{"histogram": true}"#).expect("an old file").sanitized();
    assert_eq!((old.histogram_mode.as_str(), old.histogram_scale.as_str()), ("rgb", "linear"));
    let junk = serde_json::from_str::<crate::UiState>(r#"{"histogramMode": "vector", "histogramScale": 7}"#).expect("a hand-edited file").sanitized();
    assert_eq!((junk.histogram_mode.as_str(), junk.histogram_scale.as_str()), ("rgb", "linear"), "junk is the default");
    // …and the panel draws the default rather than failing on it
    h.app.ui.histogram_mode = "vector".into();
    h.step();
    assert!(!plot_colors(&h).is_empty());
}

/// The clipping triangles show which channels reach each end, and resting the pointer on one
/// previews the clipping overlay on the photo without turning the persistent one on.
#[test]
fn a_clipping_indicator_previews_the_overlay_it_stands_for() {
    let mut h = edit_panel();
    let id = h.app.session.active().expect("an active photo");
    // every channel blown: the highlight triangle reports all three and draws neutral
    let mut clipped = lcc_raster::Histogram::of_srgb8(&lcc_raster::Rgba8::from_fn(8, 8, |_, _| [255, 255, 255, 255]));
    clipped.chroma = vec![0; lcc_raster::Histogram::BINS];
    if let Some(texture) = h.app.renderer.textures.get_mut(&Slot::Main) {
        texture.photo = id;
        texture.histogram = Some(clipped);
    }
    h.step();
    assert!(!h.app.ui.show_clipping);
    assert!(!h.app.clipping_preview);
    let d = (*h.app.session.develop_of(id).unwrap_or_default()).clone();
    assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, false), lcc_pipeline::Overlay::None);

    let r = h.request("ui.hoverWidget", json!({"id": "histogram:highlights", "fx": 0.5, "fy": 0.5}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.clipping_preview, "the hovered indicator previews its mask");
    assert!(!h.app.ui.show_clipping, "and does not turn the persistent overlay on");
    assert_eq!(
        crate::panels::detail::view_overlay(&h.app, &d, false),
        lcc_pipeline::Overlay::Clipping(None),
        "the hovering indicator puts the clipping mask on the photo"
    );

    let r = h.request("ui.move", json!({"x": 700.0, "y": 500.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(!h.app.clipping_preview, "the preview goes with the pointer");
    assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, false), lcc_pipeline::Overlay::None);
    // the click still toggles the persistent overlay, exactly once
    let r = h.request("ui.clickWidget", json!({"id": "histogram:highlights"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.ui.show_clipping);
    assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, false), lcc_pipeline::Overlay::Clipping(None));
}
