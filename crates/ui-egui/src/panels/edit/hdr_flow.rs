//! The Edit panel's flow between SDR and HDR: pressing HDR moves the histogram and the HDR settings
//! to their new layout instead of swapping it in one frame.

use lightcraft_raster::HdrBins;

use super::{Color32, Histogram, Mesh, PhotoId, Pos2, Rect, Stroke, Vertex, hex, pos2, vec2};

/// How long the histogram and the HDR settings take to flow between their SDR and HDR layouts.
const FLOW_SECS: f32 = 0.45;

pub(super) fn hdr_histogram_key(id: PhotoId) -> egui::Id {
    egui::Id::unique(("hdrHistogram", id))
}

/// The photo's last SDR histogram, kept to draw the way back from the HDR layout and into it.
pub(super) fn sdr_histogram_key(id: PhotoId) -> egui::Id {
    egui::Id::unique(("sdrHistogram", id))
}

/// The histogram on its way between its layouts this frame. Entering HDR squeezes the SDR curve into
/// the HDR histogram's SDR section while the HDR section opens beside it (and the legend row slides
/// open under the plot); leaving stretches it back. The last histogram of each kind is kept so both
/// ends can be drawn on the way.
pub(super) struct LayoutFlow {
    /// 0 (the SDR layout) to 1 (the HDR layout), eased.
    pub morph: f32,
    bins: Option<HdrBins>,
    sdr: Option<Histogram>,
    hdr: Option<Histogram>,
}

impl LayoutFlow {
    /// The flow towards the layout of `hist`, the histogram shown this frame.
    pub fn of(ctx: &egui::Context, id: PhotoId, hist: Option<&Histogram>) -> Self {
        let showing_hdr = hist.is_some_and(|h| h.hdr.is_some());
        let morph =
            egui::emath::easing::cubic_in_out(ctx.animate_bool_with_time(egui::Id::unique(("histogramHdrLayout", id)), showing_hdr, FLOW_SECS));
        let hdr = if showing_hdr {
            hist.cloned()
        } else if morph > 0.0 {
            ctx.data(|d| d.get_temp::<Histogram>(hdr_histogram_key(id)))
        } else {
            None
        };
        let sdr = if !showing_hdr {
            hist.cloned()
        } else if morph < 1.0 {
            ctx.data(|d| d.get_temp::<Histogram>(sdr_histogram_key(id)))
        } else {
            None
        };
        let bins = hdr.as_ref().and_then(|h| h.hdr);
        Self { morph: if bins.is_some() { morph } else { 0.0 }, bins, sdr, hdr }
    }

    /// The HDR section's bins, while any of it is showing.
    pub fn hdr_bins(&self) -> Option<HdrBins> {
        self.bins.filter(|_| self.morph > 0.0)
    }

    /// The histogram being left, drawn alone mid-way when there is none of the new kind yet.
    pub fn leaving(&self) -> Option<Histogram> {
        self.hdr.clone().filter(|_| self.morph > 0.0)
    }

    /// The first HDR bin: where the HDR histogram's SDR section ends.
    fn from(&self) -> f32 {
        self.bins.map_or(255.0, |b| b.from as f32)
    }

    /// SDR white across `plot`: its right edge in SDR, the HDR layout's split in HDR.
    pub fn sdr_white_x(&self, plot: Rect) -> f32 {
        plot.left() + plot.width() * sdr_white_at(self.from(), self.morph)
    }

    /// The curve, one through both layouts: the SDR section's heights morph from the SDR histogram's
    /// to the HDR histogram's SDR bins as it narrows to SDR white, and the HDR bins fill the rest.
    /// `shown` decides greyscale, whose three channels coincide: one neutral line is drawn.
    pub fn paint_curve(&self, p: &egui::Painter, plot: Rect, shown: &Histogram) {
        let sdr = self.sdr.as_ref().map(histogram_heights);
        let hdr = self.hdr.as_ref().filter(|_| self.bins.is_some()).map(histogram_heights);
        let pts = morph_curve(sdr.as_ref(), hdr.as_ref(), self.from(), self.morph);
        let to = |x: f32, v: f32| pos2(plot.left() + plot.width() * x, plot.bottom() - plot.height() * v);
        // luminance fill
        let mut mesh = Mesh::default();
        for (i, (x, hs)) in pts.iter().enumerate() {
            let top = to(*x, hs[0]);
            mesh.vertices.push(Vertex { pos: top, uv: Pos2::ZERO, color: Color32::from_gray(120).gamma_multiply(0.35) });
            mesh.vertices.push(Vertex { pos: pos2(top.x, plot.bottom()), uv: Pos2::ZERO, color: Color32::from_gray(120).gamma_multiply(0.35) });
            if i > 0 {
                let k = (i as u32) * 2;
                mesh.indices.extend([k - 2, k - 1, k, k - 1, k + 1, k]);
            }
        }
        p.add(mesh);
        let line = |j: usize, c: Color32| egui::Shape::line(pts.iter().map(|(x, hs)| to(*x, hs[j])).collect(), Stroke::new(1.5, c));
        if is_greyscale(shown) {
            p.add(line(2, Color32::from_gray(200)));
        } else {
            for (j, c) in [(1, hex("#df3939")), (2, hex("#44b072")), (3, hex("#3b6fe0"))] {
                p.add(line(j, c));
            }
        }
    }
}

/// SDR white across the plot (0..1), `morph` of the way to the HDR layout where it sits at bin `from`.
fn sdr_white_at(from: f32, morph: f32) -> f32 {
    1.0 + (from / 255.0 - 1.0) * morph
}

/// A histogram's display heights per bin for luma, R, G and B: smoothed over 5 bins, then
/// square-root scaled against the R/G/B peak (the end bins left out) and capped at 1.
fn histogram_heights(h: &Histogram) -> [Vec<f32>; 4] {
    let smooth = |v: &[u32]| -> Vec<f32> {
        (0..256)
            .map(|i: usize| {
                let (a, b) = (i.saturating_sub(2), (i + 3).min(v.len()));
                let w = v.get(a..b).unwrap_or_default();
                w.iter().map(|x| *x as f32).sum::<f32>() / w.len().max(1) as f32
            })
            .collect()
    };
    let [l, r, g, b] = [smooth(&h.luma), smooth(&h.r), smooth(&h.g), smooth(&h.b)];
    let peak = [&r, &g, &b].iter().flat_map(|v| v.iter().skip(2).take(252)).fold(1.0f32, |a, b| a.max(*b));
    [l, r, g, b].map(|v| v.into_iter().map(|x| (x / peak).sqrt().min(1.0)).collect())
}

/// `v` at fractional bin `x`, linearly interpolated (clamped to its ends).
fn sample(v: &[f32], x: f32) -> f32 {
    let x = x.clamp(0.0, v.len().saturating_sub(1) as f32);
    let i = x.floor() as usize;
    let (a, b) = (v.get(i).copied().unwrap_or(0.0), v.get(i + 1).or(v.get(i)).copied().unwrap_or(0.0));
    a + (b - a) * (x - i as f32)
}

/// A greyscale image's histogram: its three channels coincide.
fn is_greyscale(h: &Histogram) -> bool {
    let spread: f32 = h.r.iter().zip(&h.g).zip(&h.b).map(|((r, g), b)| (*r as f32 - *g as f32).abs() + (*g as f32 - *b as f32).abs()).sum();
    spread / (h.g.iter().map(|x| *x as f32).sum::<f32>().max(1.0)) < 0.02
}

/// The histogram curve `morph` of the way from the SDR layout (0) to the HDR one (1), as
/// (x across the plot 0..1, heights for luma, R, G, B). Left of SDR white ([`sdr_white_at`]), the SDR
/// histogram's heights morph into the HDR histogram's SDR bins (`0..from`); right of it, the HDR
/// bins fill what has opened. Either histogram may be missing: the other is drawn alone.
fn morph_curve(sdr: Option<&[Vec<f32>; 4]>, hdr: Option<&[Vec<f32>; 4]>, from: f32, morph: f32) -> Vec<(f32, [f32; 4])> {
    let x0 = sdr_white_at(from, morph);
    let mut pts: Vec<(f32, [f32; 4])> = (0..=255)
        .map(|k| {
            let u = k as f32 / 255.0;
            let hs = std::array::from_fn(|j| match (sdr, hdr) {
                (Some(s), Some(c)) => {
                    let (a, z) = (sample(&s[j], u * 255.0), sample(&c[j], u * from));
                    a + (z - a) * morph
                }
                (Some(s), None) => sample(&s[j], u * 255.0),
                (None, Some(c)) => sample(&c[j], u * from),
                (None, None) => 0.0,
            });
            (x0 * u, hs)
        })
        .collect();
    if let Some(c) = hdr.filter(|_| morph > 0.0) {
        let n = (255.0 - from).max(1.0) as usize;
        pts.extend((1..=n).map(|k| {
            let u = k as f32 / n as f32;
            (x0 + (1.0 - x0) * u, std::array::from_fn(|j| sample(&c[j], from + u * (255.0 - from))))
        }));
    }
    pts
}

/// `body` as a drawer sliding open (`open`) or shut over [`FLOW_SECS`], eased like the histogram: its
/// height grows from 0 to its full height, the rows below move with its edge, and the content slides
/// down from under the row above (clipped there: nothing past the edge is drawn or clickable). Fully
/// shut, `body` doesn't run.
pub(super) fn drawer(ui: &mut egui::Ui, id: egui::Id, open: bool, body: impl FnOnce(&mut egui::Ui)) {
    let t = egui::emath::easing::cubic_in_out(ui.ctx().animate_bool_with_time(id, open, FLOW_SECS));
    if t <= 0.0 {
        return;
    }
    // the height measured last frame (0 before the first: that frame draws nothing and measures)
    let full = ui.ctx().data(|d| d.get_temp::<f32>(id)).unwrap_or(0.0);
    let avail = ui.available_rect_before_wrap();
    let top = avail.top();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(avail.translate(vec2(0.0, -full * (1.0 - t)))).layout(*ui.layout()));
    if t < 1.0 {
        child.shrink_clip_rect(Rect::from_min_max(pos2(avail.left(), top), pos2(avail.right(), top + full * t)));
    }
    body(&mut child);
    let measured = child.min_rect().height();
    ui.ctx().data_mut(|d| d.insert_temp(id, measured));
    ui.advance_cursor_after_rect(Rect::from_min_size(pos2(avail.left(), top), vec2(child.min_rect().width(), measured * t)));
    // the spacing after it grows with it too, so nothing below jumps when it starts or ends
    ui.add_space(-ui.spacing().item_spacing.y * (1.0 - t));
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Sense;

    /// The whole panel, frame by frame: the HDR button's layout change moves the histogram's height
    /// and the sliders below in small steps, both ways, and ends at the same layout it left.
    #[test]
    fn pressing_hdr_moves_the_panel_instead_of_swapping_its_layout() {
        use crate::headless::Headless;
        use crate::{LightcraftApp, Services};
        use serde_json::json;
        use std::time::{Duration, Instant};

        let widget = |h: &Headless, id: &str| h.app.widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| *r);
        // (the histogram's height, the Exposure slider's top) at each frame until both reach `end`
        let run = |h: &mut Headless, on: bool, end: Option<f32>| {
            h.app.run("develop.hdr", json!({"enabled": on})).expect("develop.hdr");
            let (t0, mut trail) = (Instant::now(), Vec::new());
            let mut still = 0;
            while t0.elapsed() < Duration::from_secs(120) && still < 60 {
                h.step();
                std::thread::sleep(Duration::from_millis(1));
                let at = |id| widget(h, id).unwrap_or_else(|| panic!("no widget {id}"));
                let now = (at("histogram").height(), at("slider:light.exposure").top());
                still = if trail.last() == Some(&now) && end.is_none_or(|e| now.0 == e) { still + 1 } else { 0 };
                trail.push(now);
            }
            trail
        };
        let moved = |v: &[f32]| -> (usize, f32) {
            let (lo, hi) = (v.iter().copied().fold(f32::MAX, f32::min), v.iter().copied().fold(f32::MIN, f32::max));
            let steps = v.iter().filter(|x| **x > lo && **x < hi).count();
            (steps, v.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max) / (hi - lo).max(1.0))
        };

        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
        let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
        let r = h.request("ui.set", json!({"view": "detail", "right": "edit", "openSections": ["light"]}), Duration::from_secs(20));
        assert_eq!(r["ok"], true, "{r}");
        h.settle(Duration::from_secs(120));
        let (sdr_h, sdr_y) = (widget(&h, "histogram").expect("histogram").height(), widget(&h, "slider:light.exposure").expect("exposure").top());

        let on = run(&mut h, true, Some(sdr_h + 26.0));
        let off = run(&mut h, false, Some(sdr_h));
        for (name, trail) in [("on", &on), ("off", &off)] {
            let heights: Vec<f32> = trail.iter().map(|t| t.0).collect();
            let tops: Vec<f32> = trail.iter().map(|t| t.1).collect();
            let (h_steps, h_jump) = moved(&heights);
            let (y_steps, y_jump) = moved(&tops);
            assert!(h_steps >= 4 && h_jump < 0.5, "{name}: the histogram's height jumped: {heights:?}");
            assert!(y_steps >= 4 && y_jump < 0.5, "{name}: the sliders jumped: {tops:?}");
        }
        assert_eq!(on.last().map(|t| t.0), Some(sdr_h + 26.0), "the legend row opened");
        assert_eq!(off.last().copied(), Some((sdr_h, sdr_y)), "back where it started");
    }

    #[test]
    fn the_hdr_settings_slide_open_and_shut() {
        let ctx = egui::Context::default();
        let id = egui::Id::unique("drawer");
        // the height the drawer takes in the layout at each frame
        let frame = |time: f64, open: bool| {
            let mut height = 0.0;
            let input = egui::RawInput { time: Some(time), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                let top = ui.cursor().top();
                drawer(ui, id, open, |ui| {
                    ui.allocate_exact_size(vec2(100.0, 40.0), Sense::hover());
                });
                height = ui.cursor().top() - top;
            });
            out.textures_delta.clear();
            height
        };
        // shut from the start: nothing; opened: it grows a little each frame up to its height
        assert_eq!(frame(0.0, false), 0.0);
        let opening: Vec<f32> = (1..=15).map(|k| frame(k as f64 * 0.05, true)).collect();
        assert!(opening.windows(2).all(|w| w[1] >= w[0]), "{opening:?}");
        assert!(opening.iter().filter(|h| **h > 0.0 && **h < 40.0).count() >= 4, "{opening:?}");
        assert!(opening[0] < 3.0, "no jump as it starts: {opening:?}");
        let spaced = 40.0 + ctx.global_style().spacing.item_spacing.y;
        assert_eq!(opening.last(), Some(&spaced));
        // and shrinks back the same way
        let closing: Vec<f32> = (16..=30).map(|k| frame(k as f64 * 0.05, false)).collect();
        assert!(closing.windows(2).all(|w| w[1] <= w[0]), "{closing:?}");
        assert!(closing.iter().filter(|h| **h > 0.0 && **h < 40.0).count() >= 4, "{closing:?}");
        assert_eq!(closing.last(), Some(&0.0));
    }

    #[test]
    fn the_histogram_flows_between_its_sdr_and_hdr_layouts() {
        let flat = |v: f32| -> [Vec<f32>; 4] { std::array::from_fn(|_| vec![v; 256]) };
        let (sdr, hdr) = (flat(0.2), flat(0.6));
        let from = 128.0;
        let x_of = |pts: &[(f32, [f32; 4])]| pts.iter().map(|p| p.0).collect::<Vec<_>>();
        // SDR: the SDR histogram across the whole plot, no HDR section
        let pts = morph_curve(Some(&sdr), Some(&hdr), from, 0.0);
        assert_eq!(pts.len(), 256);
        assert!(pts.iter().all(|p| (p.1[0] - 0.2).abs() < 1e-6));
        assert_eq!(pts.last().map(|p| p.0), Some(1.0));
        // half way: the SDR section squeezed half way to SDR white, its heights half morphed,
        // the HDR section opened beside it, joined without a gap
        let pts = morph_curve(Some(&sdr), Some(&hdr), from, 0.5);
        let x0 = sdr_white_at(from, 0.5);
        assert!((pts[255].0 - x0).abs() < 1e-6);
        assert!((pts[100].1[2] - 0.4).abs() < 1e-6);
        assert!((pts[256].1[2] - 0.6).abs() < 1e-6 && pts[256].0 > x0);
        let xs = x_of(&pts);
        assert!(xs.windows(2).all(|w| w[1] > w[0] && w[1] - w[0] < 0.02), "one continuous curve");
        assert_eq!(xs.last(), Some(&1.0));
        // HDR: the HDR histogram alone, SDR white at its bin
        let pts = morph_curve(Some(&sdr), Some(&hdr), from, 1.0);
        assert!((pts[255].0 - from / 255.0).abs() < 1e-6);
        assert!(pts.iter().all(|p| (p.1[0] - 0.6).abs() < 1e-6));
        // a missing end draws the other alone
        assert!(morph_curve(None, Some(&hdr), from, 0.3).iter().all(|p| (p.1[1] - 0.6).abs() < 1e-6));
        assert_eq!(morph_curve(Some(&sdr), None, from, 0.0).len(), 256);
        assert_eq!((sample(&[0.0, 1.0], 0.25), sample(&[0.0, 1.0], 9.0), sample(&[], 1.0)), (0.25, 1.0, 0.0));
    }
}
