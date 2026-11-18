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
fn the_mode_and_scale_commands_select_step_and_reject_junk(h: &mut Headless) {
    assert_eq!(h.app.ui.histogram_mode, "rgb", "the panel opens on the target app's own overlay");
    assert_eq!(h.app.ui.histogram_scale, "linear");

    for mode in ["luminance", "channels", "chroma", "vectorscope", "waveform", "parade", "rgb"] {
        let out = h.app.run("view.histogram.mode", json!({"mode": mode})).expect("a known mode");
        assert_eq!(out["mode"], mode);
        assert_eq!(h.app.ui.histogram_mode, mode);
        h.step();
    }
    // no parameter: the next mode, wrapping at the end
    for (from, to) in [
        ("rgb", "luminance"),
        ("luminance", "channels"),
        ("channels", "chroma"),
        ("chroma", "vectorscope"),
        ("vectorscope", "waveform"),
        ("waveform", "parade"),
        ("parade", "rgb"),
    ] {
        set(h, "view.histogram.mode", "mode", from);
        let out = h.app.run("view.histogram.mode", json!({})).expect("a step");
        assert_eq!(out["mode"], to, "stepping from {from}");
        assert_eq!(h.app.ui.histogram_mode, to);
    }
    for scale in ["logarithmic", "linear"] {
        let out = h.app.run("view.histogram.scale", json!({"scale": scale})).expect("a known scale");
        assert_eq!(out["scale"], scale);
        assert_eq!(h.app.ui.histogram_scale, scale);
    }
    set(h, "view.histogram.scale", "scale", "linear");
    assert_eq!(h.app.run("view.histogram.scale", json!({})).expect("a step")["scale"], "logarithmic");
    assert_eq!(h.app.run("view.histogram.scale", json!({})).expect("a step")["scale"], "linear");

    // anything else is an error naming the values it takes, and changes nothing
    for bad in ["LUMINANCE", "waves", "", "rgb "] {
        let e = h.app.run("view.histogram.mode", json!({"mode": bad})).expect_err("an unknown mode");
        assert!(e.contains("rgb|luminance|channels|chroma|vectorscope|waveform|parade"), "{bad}: {e}");
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
    set(h, "view.histogram.mode", "mode", "rgb");
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
/// the vectorscope draws its circular graticule, six bracketed targets (`R`, `Mg`, `B`, `Cy`, `G`, `Yl`)
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
            CHANNEL_COLORS.iter().flat_map(|cc| [*cc, cc.gamma_multiply(0.38)]).any(|e| e.r() == c.r() && e.g() == c.g() && e.b() == c.b()),
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
    assert!(circles >= 5, "the disc's well and its four saturation rings are drawn: {circles}");
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
fn the_mode_and_scale_survive_the_settings(h: &mut Headless) {
    set(h, "view.histogram.mode", "mode", "channels");
    set(h, "view.histogram.scale", "scale", "logarithmic");
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
    assert!(!plot_colors(h).is_empty());
}

#[test]
fn the_mode_and_scale_commands_select_step_reject_junk_and_survive_the_settings() {
    let mut h = edit_panel();
    the_mode_and_scale_commands_select_step_and_reject_junk(&mut h);
    the_mode_and_scale_survive_the_settings(&mut h);
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
    // the pointer rests there a moment first
    h.time += crate::panels::edit::CLIPPING_PREVIEW_REST_S;
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

/// The Waveform overlays the loupe photo's red, green and blue columns left to right in one track, the RGB
/// Parade puts the three channels in three side-by-side tracks of their own colours; only the loupe's histogram keeps the
/// waveform planes, thumbnails' don't.
#[test]
fn the_waveform_views_draw_the_photo_by_column() {
    let mut h = edit_panel();
    let main = h.app.renderer.textures.get(&Slot::Main).and_then(|t| t.histogram.clone()).expect("the loupe's histogram");
    assert!(main.has_waveform(), "the loupe keeps its waveform");
    let id = h.app.session.active().expect("a photo");
    if let Some(thumb) = h.app.renderer.textures.get(&Slot::Thumb(id)).and_then(|t| t.histogram.clone()) {
        assert!(!thumb.has_waveform(), "a thumbnail drops it");
    }
    set(&mut h, "view.histogram.mode", "mode", "waveform");
    h.settle(SETTLE);
    let plot = rect(&h, "histogram:plot");
    let overlay = crate::panels::histogram::waveform_tracks(plot, lcc_raster::HistogramMode::Waveform);
    assert_eq!(overlay.len(), 3, "red, green and blue");
    assert!(overlay.iter().all(|t| t.0 == plot), "overlaid in one track");
    assert_eq!(scope_pictures(&h), vec![plot], "one picture fills the track");
    // the picture holds a trace of each channel
    let planes: Vec<_> = overlay.iter().map(|(_, c, rgb)| (*c, *rgb)).collect();
    let picture = crate::panels::histogram::waveform_image(&main, &planes, lcc_raster::HistogramScale::Linear);
    for (k, dominant) in [(0usize, "red"), (1, "green"), (2, "blue")] {
        assert!(
            picture.pixels.iter().any(|c| {
                let v = [c.r(), c.g(), c.b()];
                (0..3).all(|j| j == k || v[k] > v[j])
            }),
            "the waveform draws a {dominant} trace",
        );
    }

    set(&mut h, "view.histogram.mode", "mode", "parade");
    h.settle(SETTLE);
    let plot = rect(&h, "histogram:plot");
    let tracks = crate::panels::histogram::waveform_tracks(plot, lcc_raster::HistogramMode::Parade);
    assert_eq!(tracks.len(), 3);
    assert!(tracks.windows(2).all(|w| w[0].0.right() < w[1].0.left()), "side by side");
    assert_eq!(scope_pictures(&h), tracks.iter().map(|t| t.0).collect::<Vec<_>>(), "a picture per track");
    // each track's picture is its channel's alone
    for (k, (_, plane, rgb)) in tracks.iter().enumerate() {
        let picture = crate::panels::histogram::waveform_image(&main, &[(*plane, *rgb)], lcc_raster::HistogramScale::Linear);
        let lit: Vec<_> = picture.pixels.iter().filter(|c| c.r() > 0 || c.g() > 0 || c.b() > 0).collect();
        assert!(!lit.is_empty(), "track {k} has a trace");
        assert!(
            lit.iter().all(|c| {
                let v = [c.r(), c.g(), c.b()];
                (0..3).all(|j| v[k] >= v[j])
            }),
            "track {k} is its own channel"
        );
    }
}

/// Where the scope pictures (textured meshes) were drawn last frame, left to right.
fn scope_pictures(h: &Headless) -> Vec<egui::Rect> {
    let plot = rect(h, "histogram:plot");
    let mut out: Vec<egui::Rect> = h
        .view
        .painted_shapes()
        .iter()
        .filter_map(|s| match s {
            egui::Shape::Mesh(m) if m.texture_id != egui::TextureId::default() && m.vertices.iter().all(|v| plot.expand(0.5).contains(v.pos)) => {
                Some(egui::Rect::from_points(&m.vertices.iter().map(|v| v.pos).collect::<Vec<_>>()))
            }
            _ => None,
        })
        .collect();
    out.sort_by(|a, b| a.left().total_cmp(&b.left()));
    out
}

/// In the Library grid the histogram and the scopes show the selected photo, not only the one last opened in the
/// loupe: a grid thumbnail keeps no waveform (and a cached one may carry no histogram), so the panel renders its
/// own small picture of the photo when what it has can't draw the mode.
#[test]
fn the_grid_shows_the_scopes_of_the_selected_photo() {
    let mut h = edit_panel();
    let loupe = h.app.session.active().expect("a photo");
    // back to the grid and on to a photo the loupe never showed
    let other = h.app.session.visible_cloned().iter().map(|p| p.0).find(|&p| p != loupe.0).expect("a second photo");
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [other]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.set", json!({"view": "squareGrid", "histogram": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.session.active().map(|p| p.0), Some(other), "the selected photo is the active one");
    for mode in ["rgb", "waveform", "parade"] {
        set(&mut h, "view.histogram.mode", "mode", mode);
        // the grid as it is with real photos: only thumbnails, no loupe render of the selected photo
        h.app.renderer.textures.remove(&Slot::Main);
        h.app.renderer.panel_histogram = None;
        h.step();
        h.app.renderer.textures.remove(&Slot::Main);
        h.settle(SETTLE);
        assert!(widget(&h, "histogram:plot").is_some(), "{mode}: the histogram is on screen in the grid");
        if mode == "rgb" {
            assert!(!plot_colors(&h).is_empty(), "the histogram draws the selected photo");
        } else {
            assert!(!scope_pictures(&h).is_empty(), "{mode}: the scope draws the selected photo");
            // the loupe's render of the photo if the grid made one again, else the panel's own
            let main = h.app.renderer.textures.get(&Slot::Main).filter(|t| t.photo.0 == other).and_then(|t| t.histogram.clone());
            let own = h.app.renderer.panel_histogram.as_ref().filter(|k| k.0.0 == other).map(|k| k.2.clone());
            let shown = main.or(own).unwrap_or_else(|| panic!("{mode}: no waveform of the selected photo"));
            assert!(shown.has_waveform(), "{mode}: with its waveform");
        }
    }
}

/// An RGB Parade with a Waveform as the second histogram: each draws its own picture (they once
/// shared one texture per track, so the Parade's first track showed the Waveform).
#[test]
fn a_parade_beside_a_waveform_keeps_its_own_pictures() {
    let mut h = edit_panel();
    set(&mut h, "view.histogram.mode", "mode", "parade");
    set(&mut h, "view.histogram.second", "mode", "waveform");
    h.settle(SETTLE);
    let tex = |h: &Headless, mode: &str| {
        h.view.ctx.data_mut(|d| d.get_temp::<egui::TextureHandle>(egui::Id::new(("scope-waveform", mode, 0usize)))).map(|t| t.id())
    };
    let (parade, wave) = (tex(&h, "parade"), tex(&h, "waveform"));
    assert!(parade.is_some() && wave.is_some(), "both scopes drew ({parade:?}, {wave:?})");
    assert_ne!(parade, wave, "a texture each");
}

/// Where the curves inside widget `id` reached last frame: the points of its meshes and lines.
fn curve_points(h: &Headless, id: &str) -> Vec<egui::Pos2> {
    let r = rect(h, id);
    let mut out = Vec::new();
    for shape in h.view.painted_shapes() {
        match shape {
            egui::Shape::Mesh(mesh) => out.extend(mesh.vertices.iter().map(|v| v.pos).filter(|p| r.contains(*p))),
            egui::Shape::Path(path) => out.extend(path.points.iter().copied().filter(|p| r.contains(*p))),
            _ => {}
        }
    }
    out
}

/// A display with HDR headroom: HDR edits show in HDR, and a drag of one sends the loupe only float
/// drafts.
struct FakeHdrDisplay;

impl crate::HdrOutput for FakeHdrDisplay {
    fn headroom(&mut self) -> Option<f32> {
        Some(4.0)
    }
    fn upload(&mut self, view: u8, _: usize, _: usize, _: &[f32]) -> Result<egui::TextureId, String> {
        Ok(egui::TextureId::User(700 + u64::from(view)))
    }
}

/// The demo library on its first photo, made an HDR edit, on an HDR display or an SDR one.
fn hdr_edit(hdr_display: bool, high_quality_previews: bool) -> Headless {
    let hdr_output: Option<Box<dyn crate::HdrOutput>> = if hdr_display { Some(Box::new(FakeHdrDisplay)) } else { None };
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, hdr_output, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    h.app.ui.settings.high_quality_adjustment_previews = high_quality_previews;
    let id = h.app.session.visible_cloned()[0].0;
    for (command, params) in [("library.select", json!({"ids": [id]})), ("develop.hdr", json!({"enabled": true}))] {
        let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h
}

/// Once the loupe shows `h`'s photo (in HDR on an HDR display), an exposure drag in two steps and
/// its release: each of `plots` (widget ids) follows every step, and every frame after the release
/// shows the drag's last histogram or the settled one, never the one from before the drag.
fn drag_and_release(h: &mut Headless, hdr_display: bool, plots: &[&str], what: &str) {
    let hdr_shown = |h: &Headless| h.app.loupe_shown.is_some_and(|(_, s)| s == "hdr");
    if hdr_display {
        assert!(h.step_until(SETTLE, hdr_shown), "{what}: the loupe shows the HDR texture");
    }
    h.settle(SETTLE);
    assert_eq!(hdr_shown(h), hdr_display, "{what}");
    let drawn = |h: &Headless| plots.iter().map(|id| curve_points(h, id)).collect::<Vec<_>>();
    let before = drawn(h);
    assert!(before.iter().all(|p| !p.is_empty()), "{what}: every plot is drawn");
    h.app.run("develop.beginInteraction", json!({"label": "Exposure"})).unwrap();
    for value in [1.5, 3.0] {
        let step = drawn(h);
        h.app.run("develop.set", json!({"values": {"light.exposure": value}})).unwrap();
        assert!(h.settle(SETTLE), "{what}, exposure {value}: the drag's renders land");
        assert!(h.app.session.interaction.is_some());
        for (k, (now, was)) in drawn(h).iter().zip(&step).enumerate() {
            assert!(now != was, "{what}, exposure {value}: {} follows the drag", plots[k]);
        }
    }
    let last = drawn(h);
    h.app.run("develop.endInteraction", json!({})).unwrap();
    // every frame until the released render has landed and nothing is left to do
    let mut released = Vec::new();
    let (t0, mut quiet) = (std::time::Instant::now(), 0);
    while quiet < 12 && t0.elapsed() < SETTLE {
        h.step();
        released.push(drawn(h));
        quiet = if h.busy() { 0 } else { quiet + 1 };
        std::thread::sleep(Duration::from_millis(1));
    }
    let settled = drawn(h);
    for (k, id) in plots.iter().enumerate() {
        assert!(settled[k] != before[k], "{what}: {id} settles on the new exposure");
        for (i, frame) in released.iter().enumerate() {
            assert!(frame[k] != before[k], "{what}: {id}, frame {i} after the release shows the histogram from before the drag");
            assert!(frame[k] == last[k] || frame[k] == settled[k], "{what}: {id}, frame {i} after the release shows a third histogram");
        }
    }
}

/// A slider drag of an HDR edit: both histograms follow the drag's live HDR histogram, and the
/// release goes from the drag's last one to the released render's. On an HDR display the loupe's
/// float drafts leave its 8-bit render (and that render's histogram) as it was before the drag: the
/// second histogram once drew only that one, standing still until the release.
#[test]
fn both_histograms_follow_an_hdr_drag_and_hold_on_release() {
    for (hdr_display, hq) in [(false, false), (true, false), (true, true)] {
        let mut h = hdr_edit(hdr_display, hq);
        let r = h.request("ui.set", json!({"view": "detail", "right": "edit", "histogram": true}), T);
        assert_eq!(r["ok"], true, "{r}");
        set(&mut h, "view.histogram.mode", "mode", "luminance");
        set(&mut h, "view.histogram.second", "mode", "luminance");
        let what = format!("HDR display {hdr_display}, high quality previews {hq}");
        drag_and_release(&mut h, hdr_display, &["histogram:plot", "histogram2"], &what);
    }
}

/// The Modern layout's floating histogram follows a drag of an HDR edit as the panel's do (it
/// stays while the rest of the chrome fades). It once drew only the loupe's 8-bit render, which on
/// an HDR display stands still for the whole drag.
#[test]
fn the_modern_histogram_follows_an_hdr_drag_and_holds_on_release() {
    for hdr_display in [false, true] {
        let mut h = hdr_edit(hdr_display, false);
        for (command, params) in [("view.modernLayout", json!({"on": true})), ("module.develop", json!({})), ("view.detail", json!({}))] {
            let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
            assert_eq!(r["ok"], true, "{command}: {r}");
        }
        set(&mut h, "view.histogram.mode", "mode", "luminance");
        drag_and_release(&mut h, hdr_display, &["modern:histogram"], &format!("Modern, HDR display {hdr_display}"));
    }
}

/// Gliding the pointer across the histogram passes over its clipping triangles: that must not
/// preview the clipping overlay (each preview re-rendered the photo, every tile at 100 %: the
/// interface stuttered and the graphics card filled up, CF-ZOOM-FREEZE 2026-11-10). Only a pointer
/// resting on a triangle previews it.
#[test]
fn gliding_over_the_histogram_never_previews_clipping() {
    let mut h = edit_panel();
    let id = h.app.session.active().expect("an active photo");
    h.step();
    let d = (*h.app.session.develop_of(id).unwrap_or_default()).clone();
    let rect = |h: &Headless, id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).expect("the widget");
    let plot = rect(&h, "histogram:plot");
    let (lo, hi) = (rect(&h, "histogram:shadows"), rect(&h, "histogram:highlights"));
    // back and forth along the top of the plot, through both triangles, one point a frame
    for pass in 0..2 {
        for i in 0..=24 {
            let f = if pass % 2 == 0 { i as f32 / 24.0 } else { 1.0 - i as f32 / 24.0 };
            let x = plot.left() + f * plot.width();
            let r = h.request("ui.move", json!({"x": x, "y": lo.center().y.min(hi.center().y)}), T);
            assert_eq!(r["ok"], true, "{r}");
            h.step();
            assert!(!h.app.clipping_preview, "a pointer passing over a triangle previews nothing (x {x})");
            assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, false), lcc_pipeline::Overlay::None);
        }
    }
    // resting on one: it previews, and stops as the pointer leaves
    let r = h.request("ui.move", json!({"x": hi.center().x, "y": hi.center().y}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(!h.app.clipping_preview, "not at once");
    h.time += crate::panels::edit::CLIPPING_PREVIEW_REST_S;
    h.step();
    assert!(h.app.clipping_preview, "resting on the triangle previews it");
    let r = h.request("ui.move", json!({"x": plot.center().x, "y": plot.center().y}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(!h.app.clipping_preview);
    // coming back after a while starts the rest over
    h.time += 5.0;
    let r = h.request("ui.move", json!({"x": lo.center().x, "y": lo.center().y}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(!h.app.clipping_preview, "an old hover doesn't count as a rest");
}
