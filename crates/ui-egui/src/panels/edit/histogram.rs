use super::{
    Align2, Color32, Histogram, Icon, LightcraftApp, Mesh, PhotoId, Pos2, Rect, RichText, RightPanel, Sense, Slot, Stroke, Tokens, Vertex, divider,
    hex, json, paint, pos2, register, vec2,
};
use crate::menu_item::MenuItem;

// ------------------------------------------------------------------------------ histogram

/// The current photo's existing preview histogram, shared by Histogram and Tone Curve.
pub(super) fn preview_histogram(app: &LightcraftApp, id: PhotoId) -> Option<lightcraft_raster::Histogram> {
    app.renderer
        .textures
        .get(&Slot::Main)
        .filter(|t| t.photo == id)
        .and_then(|t| t.histogram.clone())
        .or_else(|| app.renderer.textures.get(&Slot::Thumb(id)).and_then(|t| t.histogram.clone()))
}

/// An HDR edit's histogram stays an HDR histogram: some renders carry only their 8-bit image's
/// (a drag's drafts, a cached thumbnail or stand-in), and showing those would swap the panel to
/// the SDR layout and back while a slider moves. Every render with HDR bins replaces the one kept for the photo, so
/// the histogram still follows the edit live; an SDR edit shows what it gets.
fn steady_hdr_histogram(ctx: &egui::Context, id: PhotoId, hdr_edit: bool, hist: Option<Histogram>) -> Option<Histogram> {
    let key = egui::Id::unique(("hdrHistogram", id));
    match hist {
        Some(h) if h.hdr.is_some() => {
            ctx.data_mut(|d| d.insert_temp(key, h.clone()));
            Some(h)
        }
        h if hdr_edit => ctx.data(|d| d.get_temp::<Histogram>(key)).or(h),
        h => h,
    }
}

pub(super) fn histogram(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    let hdr_edit = app.session.develop_of(id).is_some_and(|d| d.hdr.enabled);
    // a drag of an HDR edit: its drafts have no HDR histogram, so a small render beside them
    // gives one (at its own pace: the drafts don't wait for it)
    let interacting = app.session.interaction.is_some();
    if interacting && hdr_edit {
        let edge = lightcraft_pipeline::HDR_HISTOGRAM_DRAG_EDGE;
        if let Some(job) = app.session.render_job(id, edge, edge, false, app.ui.right != RightPanel::Crop)
            && app.renderer.live_hdr_histogram.as_ref().is_none_or(|l| l.1 != job.key)
        {
            app.renderer.request(Slot::HdrHistogram, job, 95);
        }
    }
    let live = app.renderer.live_hdr_histogram.as_ref().filter(|l| interacting && l.0 == id).map(|l| l.2.clone());
    // else the loupe render of this photo, else its thumbnail (grid view before the loupe has
    // rendered it)
    let hist = live.or_else(|| preview_histogram(app, id));
    let hist = steady_hdr_histogram(ui.ctx(), id, hdr_edit, hist);
    // an HDR edit's histogram (`pipeline::hdr_histogram`) gets a colour bar and an SDR / HDR label
    // row under the plot
    let hdr = hist.as_ref().and_then(|h| h.hdr);
    let legend = if hdr.is_some() { 26.0 } else { 0.0 };
    let (r, resp) = ui.allocate_exact_size(vec2(w, 118.0 + legend), Sense::click_and_drag());
    crate::widgets::announce(&resp, crate::i18n::tr("Histogram"));
    register(ui, "histogram", r);
    let p = ui.painter();
    p.rect_filled(r, 0.0, t.button);
    let plot = Rect::from_min_max(r.min + vec2(0.0, 6.0), pos2(r.right(), r.bottom() - 26.0 - legend));
    // Lightroom Classic's tonal zones (Blacks / Shadows / Exposure / Highlights / Whites across the
    // plot): a drag over one moves its control (the band and the readout show which and how far), a
    // double-click puts it back to its default, and Shift drags a tenth as far
    let zone = crate::panels::histogram::interact(app, ui, &resp, plot, id);
    if let Some(a) = &zone {
        crate::panels::histogram::paint_zone(p, plot, a, &t);
    }
    if let Some(b) = hdr {
        // white for SDR, then one band per stop above SDR white: white too, or (Visualize HDR
        // range on) in its colours
        let x0 = plot.left() + plot.width() * b.from as f32 / 255.0;
        let bar = |a: f32, z: f32, c: Color32| p.rect_filled(Rect::from_min_max(pos2(a, plot.bottom() + 2.0), pos2(z, plot.bottom() + 7.0)), 0.0, c);
        let white = Color32::from_rgb(228, 228, 230);
        bar(plot.left(), x0 - 1.0, white);
        let stops = b.stops.round().max(1.0) as usize;
        let bands = lightcraft_pipeline::visualize::HDR_BANDS;
        let colours = app.ui.hdr_visualize;
        for k in 0..stops {
            let c = if colours { bands.get(k).or(bands.last()).copied().unwrap_or([255, 255, 255]) } else { [228, 228, 230] };
            let (a, z) = (x0 + (plot.right() - x0) * k as f32 / stops as f32, x0 + (plot.right() - x0) * (k + 1) as f32 / stops as f32);
            bar(a + 1.0, z - 1.0, Color32::from_rgb(c[0], c[1], c[2]));
        }
        let y = plot.bottom() + 18.0;
        p.text(pos2((plot.left() + x0) / 2.0, y), Align2::CENTER_CENTER, crate::i18n::tr("SDR"), t.font(12.5), t.text_label);
        p.text(pos2((x0 + plot.right()) / 2.0, y), Align2::CENTER_CENTER, crate::i18n::tr("HDR"), t.font(12.5), t.text_label);
        let info = Rect::from_center_size(pos2(plot.right() - 16.0, y), vec2(14.0, 14.0));
        paint(p, info, Icon::Info, t.text_label);
        ui.interact(info, egui::Id::unique("hdrHistogramInfo"), Sense::hover()).on_hover_text(crate::i18n::tr(
            "Left: the standard (SDR) range up to SDR white. Right: the HDR range, one band per stop above SDR white (the orange line: the HDR headroom). With Visualize HDR range on, the bands take its colours.",
        ));
    }
    let clipping = hist.as_ref().map(|h| h.clipping());
    if let Some(h) = hist {
        let smooth = |v: &[u32]| -> Vec<f32> {
            (0..256)
                .map(|i| {
                    let (a, b) = (i.max(2) - 2, (i + 3).min(256));
                    v[a..b].iter().map(|x| *x as f32).sum::<f32>() / (b - a) as f32
                })
                .collect()
        };
        // HDR: SDR on the left half, a solid line at SDR white and dashed ones between the stops
        if let Some(b) = h.hdr {
            let x0 = plot.left() + plot.width() * b.from as f32 / 255.0;
            let stops = b.stops.round().max(1.0) as usize;
            for k in 1..stops {
                let x = x0 + (plot.right() - x0) * k as f32 / stops as f32;
                let mut y = plot.top();
                while y < plot.bottom() {
                    p.line_segment([pos2(x, y), pos2(x, (y + 5.0).min(plot.bottom()))], Stroke::new(1.0, Color32::from_gray(150)));
                    y += 9.0;
                }
            }
            p.line_segment([pos2(x0, plot.top()), pos2(x0, plot.bottom())], Stroke::new(1.5, Color32::from_gray(160)));
            // the photo's headroom limit on the fixed scale: marked, the range past it dimmed
            let limit = app.session.develop_of(id).map_or(b.stops, |d| d.hdr.max_ev as f32);
            if limit.is_finite() && limit >= 0.0 && limit < b.stops - 0.05 {
                let x = x0 + (plot.right() - x0) * (limit / b.stops).clamp(0.0, 1.0);
                p.rect_filled(Rect::from_min_max(pos2(x, plot.top()), plot.max), 0.0, Color32::from_black_alpha(70));
                p.line_segment([pos2(x, plot.top()), pos2(x, plot.bottom())], Stroke::new(1.0, Color32::from_rgb(230, 150, 60)));
            }
        }
        let chans = [(smooth(&h.r), hex("#df3939")), (smooth(&h.g), hex("#44b072")), (smooth(&h.b), hex("#3b6fe0"))];
        let peak = chans.iter().flat_map(|(v, _)| v.iter().skip(2).take(252)).fold(1.0f32, |a, b| a.max(*b));
        let to = |i: usize, v: f32| pos2(plot.left() + plot.width() * i as f32 / 255.0, plot.bottom() - plot.height() * (v / peak).sqrt().min(1.0));
        // luminance fill
        let lum = smooth(&h.luma);
        let mut mesh = Mesh::default();
        for i in 0..256 {
            let top = to(i, lum[i]);
            mesh.vertices.push(Vertex { pos: top, uv: Pos2::ZERO, color: Color32::from_gray(120).gamma_multiply(0.35) });
            mesh.vertices.push(Vertex { pos: pos2(top.x, plot.bottom()), uv: Pos2::ZERO, color: Color32::from_gray(120).gamma_multiply(0.35) });
            if i > 0 {
                let k = (i as u32) * 2;
                mesh.indices.extend([k - 2, k - 1, k, k - 1, k + 1, k]);
            }
        }
        p.add(mesh);
        // greyscale images: the three channels coincide — draw one neutral line
        let spread: f32 = (0..256).map(|i| (chans[0].0[i] - chans[1].0[i]).abs() + (chans[1].0[i] - chans[2].0[i]).abs()).sum();
        let total: f32 = chans[1].0.iter().sum::<f32>().max(1.0);
        if spread / total < 0.02 {
            let pts: Vec<Pos2> = (0..256).map(|i| to(i, chans[1].0[i])).collect();
            p.add(egui::Shape::line(pts, Stroke::new(1.5, Color32::from_gray(200))));
        } else {
            for (v, c) in &chans {
                let pts: Vec<Pos2> = (0..256).map(|i| to(i, v[i])).collect();
                p.add(egui::Shape::line(pts, Stroke::new(1.5, *c)));
            }
        }
    }
    let mut clicked = resp.clicked();
    // the panel's floating scroll bar widens over the right edge when hovered: both buttons keep
    // clear of it, the left one too so they stay symmetric (#1640)
    let [shadow_rect, highlight_rect] = crate::panels::histogram::clipping_rects(ui, plot);
    for (name, rect, label, clipped) in [
        ("shadows", shadow_rect, "Shadow clipping", clipping.map(|(lo, _)| lo > 0.003)),
        ("highlights", highlight_rect, "Highlight clipping", clipping.map(|(_, hi)| hi > 0.003)),
    ] {
        let response = ui.interact(rect, ui.scope_id().with(("histogram-clipping", name)), Sense::click());
        register(ui, format!("histogram:{name}"), rect);
        response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, ui.is_enabled(), app.ui.show_clipping, crate::i18n::tr(label)));
        clicked |= response.clicked();
        let active = app.ui.show_clipping;
        // Keep the hit target comfortable, with a smaller box and clear space to the plot edges.
        let box_rect = rect.shrink2(vec2(2.0, 2.0));
        p.rect_filled(
            box_rect,
            3.0,
            if active {
                t.pressed
            } else if response.hovered() {
                t.hover
            } else {
                Color32::from_gray(60)
            },
        );
        if active {
            p.rect_stroke(box_rect, 3.0, Stroke::new(1.0, t.text), egui::StrokeKind::Inside);
        }
        paint(
            p,
            Rect::from_center_size(rect.center(), vec2(16.0, 16.0)),
            Icon::ChevronDown,
            if clipped == Some(true) || active { t.text } else { t.text_dim },
        );
        let status = match clipped {
            Some(true) => "Clipping detected in this preview.",
            Some(false) => "No clipping detected in this preview.",
            None => "Histogram not yet available.",
        };
        response.on_hover_text(format!(
            "{}\n{}\n{}",
            crate::i18n::tr(label),
            crate::i18n::tr(if active {
                "Click to hide clipping warnings for shadows and highlights."
            } else {
                "Click to show clipping warnings for shadows and highlights."
            }),
            crate::i18n::tr(status)
        ));
    }
    if clicked {
        let _ = app.run("view.clipping", json!({}));
    }
    // the tonal zone the pointer is on replaces the EXIF strip while it is there
    if let Some(text) = zone.as_ref().and_then(|a| a.readout()) {
        register(ui, "histogramReadout", r);
        p.text(pos2(r.left() + r.width() / 2.0, r.bottom() - 13.0), Align2::CENTER_CENTER, text, t.font(12.5), t.text_label);
    } else if let Some(ph) = app.session.catalog.photo(id) {
        let m = &ph.meta;
        let parts: Vec<String> = [
            m.iso.map(|i| format!("ISO {i}")),
            m.focal_mm.map(|f| format!("{f:.0}mm")),
            m.aperture.map(|a| format!("f/{a:.1}")),
            if m.shutter.is_empty() { None } else { Some(m.shutter.clone()) },
        ]
        .into_iter()
        .flatten()
        .collect();

        if !parts.is_empty() {
            let text = parts.join(" · ");
            let y = r.bottom() - 13.0;
            let x = r.left() + r.width() / 2.0;
            p.text(pos2(x, y), Align2::CENTER_CENTER, text, t.font(12.5), t.text_label);
        }
    }
}

// ------------------------------------------------------------------------------ soft proofing

/// The Soft Proofing strip under the histogram: proof profile, gamut warnings, Create Proof Copy.
pub(super) fn soft_proofing(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    use lightcraft_engine::pipeline::OutputSpace;
    let t = Tokens::get(ui.ctx());
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 8, bottom: 6 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(crate::i18n::tr("Soft Proofing")).color(t.text).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut dest = app.ui.proof.dest_warning;
                let r = ui
                    .toggle_value(&mut dest, RichText::new("■").color(Color32::from_rgb(255, 40, 40)))
                    .on_hover_text(crate::i18n::tr("Show destination gamut warning"));
                register(ui, "button:proofDestWarning", r.rect);
                let mut disp = app.ui.proof.display_warning;
                let r2 = ui
                    .toggle_value(&mut disp, RichText::new("■").color(Color32::from_rgb(40, 90, 255)))
                    .on_hover_text(crate::i18n::tr("Show display gamut warning"));
                register(ui, "button:proofDisplayWarning", r2.rect);
                if dest != app.ui.proof.dest_warning || disp != app.ui.proof.display_warning {
                    let _ = app.run("view.softProof", json!({"destWarning": dest, "displayWarning": disp}));
                }
            });
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new(crate::i18n::tr("Profile:")).color(t.text_label));
            let cur = app.ui.proof.space;
            let device = app.ui.proof_profile.as_ref().map(|(_, n)| n.clone());
            let shown = device.clone().unwrap_or_else(|| cur.label().to_string());
            let mut pick = false;
            let combo = crate::widgets::combo_box("proof-space").width(170.0).selected_text(shown).show_ui(ui, |ui| {
                for s in OutputSpace::ALL {
                    if MenuItem::new(s.label()).selected(device.is_none() && cur == s).show(ui).clicked() {
                        let _ = app.run("view.softProof", json!({"space": s}));
                    }
                }
                if let Some(n) = &device {
                    let _ = MenuItem::new(n).selected(true).inert().show(ui);
                }
                if app.services.pick_display_profile.is_some() && MenuItem::picker(crate::i18n::tr("Other…")).show(ui).clicked() {
                    pick = true;
                }
            });
            crate::widgets::name_combo(&combo.response, crate::i18n::tr("Profile"));
            if pick
                && let Some(path) = app.services.pick_display_profile.as_mut().and_then(|pick| pick().into_iter().next())
                && let Err(e) = app.run("view.softProof", json!({"profile": path}))
            {
                app.toast(ui.ctx(), e);
            }
        });
        if app.ui.proof_profile.is_some() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(crate::i18n::tr("Intent:")).color(t.text_label));
                use lightcraft_engine::proof::ProofIntent;
                for (i, label) in [(ProofIntent::Perceptual, "Perceptual"), (ProofIntent::Relative, "Relative")] {
                    let r = ui.selectable_label(app.ui.proof_intent == i, crate::i18n::tr(label));
                    register(ui, format!("button:proofIntent-{}", i.name()), r.rect);
                    if r.clicked() && app.ui.proof_intent != i {
                        let _ = app.run("view.softProof", json!({"intent": i.name()}));
                    }
                }
            });
            let mut paper = app.ui.proof_paper_ink;
            let r = ui.checkbox(&mut paper, crate::i18n::tr("Simulate Paper & Ink"));
            register(ui, "button:proofPaperInk", r.rect);
            if r.changed() {
                let _ = app.run("view.softProof", json!({"paperInk": paper}));
            }
        }
        let r = ui.button(crate::i18n::tr("Create Proof Copy"));
        register(ui, "button:createProofCopy", r.rect);
        if r.clicked() {
            let target = app.ui.proof_profile.as_ref().map_or_else(|| app.ui.proof.space.label().to_string(), |(_, n)| n.clone());
            let name = crate::i18n::tr_format!("Proof Copy ({})", target);
            if app.run("photo.virtualCopy", json!({"ids": [id.0], "name": name})).is_ok() {
                app.toast(ui.ctx(), crate::i18n::tr("Proof copy created"));
            }
        }
    });
    divider(ui);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_hdr_edit_keeps_its_hdr_histogram_through_8_bit_renders() {
        let ctx = egui::Context::default();
        let id = PhotoId(7);
        let sdr = Histogram::of_hdr(&[], 5.0);
        let sdr = Histogram { hdr: None, ..sdr };
        let hdr = |v: f32| Histogram::of_hdr(&[[v, v, v]], 5.0);
        // nothing kept yet: what arrives
        assert_eq!(steady_hdr_histogram(&ctx, id, true, Some(sdr.clone())), Some(sdr.clone()));
        assert_eq!(steady_hdr_histogram(&ctx, id, true, Some(hdr(2.0))), Some(hdr(2.0)));
        // a cached thumbnail's 8-bit histogram (or none) doesn't swap the panel to SDR
        assert_eq!(steady_hdr_histogram(&ctx, id, true, Some(sdr.clone())), Some(hdr(2.0)));
        assert_eq!(steady_hdr_histogram(&ctx, id, true, None), Some(hdr(2.0)));
        // the next HDR render replaces it: the histogram follows the slider
        assert_eq!(steady_hdr_histogram(&ctx, id, true, Some(hdr(4.0))), Some(hdr(4.0)));
        assert_eq!(steady_hdr_histogram(&ctx, id, true, Some(sdr.clone())), Some(hdr(4.0)));
        // HDR turned off: the SDR histogram at once; other photos keep their own
        assert_eq!(steady_hdr_histogram(&ctx, id, false, Some(sdr.clone())), Some(sdr.clone()));
        assert_eq!(steady_hdr_histogram(&ctx, PhotoId(8), true, Some(sdr.clone())), Some(sdr));
    }
}
