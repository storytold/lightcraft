use super::*;

/// Size of the Navigator mini map (points).
const NAV_W: f32 = 180.0;

/// The Navigator: while zoomed in, a mini map of the photo at the canvas' bottom right with the
/// visible region outlined; click or drag on it to pan. Returns the area it covers.
pub(super) fn navigator(app: &mut LightcraftApp, ui: &mut egui::Ui, canvas: Rect, img: Rect, id: PhotoId) -> Option<Rect> {
    let zoomed = img.width() > canvas.width() + 1.0 || img.height() > canvas.height() + 1.0;
    if !app.ui.navigator || !zoomed || app.ui.fullscreen {
        return None;
    }
    let tex = app.renderer.thumb(id).or_else(|| app.renderer.textures.get(&Slot::Main).filter(|t| t.photo == id));
    let aspect = img.width() / img.height().max(1.0);
    let (w, h) = if aspect >= 1.0 { (NAV_W, NAV_W / aspect) } else { (NAV_W * aspect, NAV_W) };
    let frame = Rect::from_min_size(pos2(canvas.right() - w - 16.0, canvas.bottom() - h - 16.0), vec2(w, h));
    let p = ui.painter_at(canvas);
    p.rect_filled(frame.expand(5.0), 4.0, Color32::from_black_alpha(190));
    match tex {
        Some(t) => p.image(t.tex.id(), frame, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE),
        None => p.rect_filled(frame, 0.0, Tokens::get(ui.ctx()).skeleton),
    };
    // the visible part of the photo, in normalized image coordinates
    let vis = canvas.intersect(img);
    let n = |q: Pos2| pos2((q.x - img.left()) / img.width(), (q.y - img.top()) / img.height());
    let (a, b) = (n(vis.min), n(vis.max));
    let to_nav = |q: Pos2| pos2(frame.left() + q.x * frame.width(), frame.top() + q.y * frame.height());
    let view = Rect::from_min_max(to_nav(a), to_nav(b));
    p.rect_stroke(view, 0.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
    p.rect_stroke(view.expand(1.5), 0.0, Stroke::new(1.0, Color32::from_black_alpha(160)), StrokeKind::Outside);
    register(ui, "canvas:navigator", frame);
    let resp = ui.interact(frame.expand(5.0), egui::Id::unique("navigator"), Sense::click_and_drag());
    crate::widgets::announce(&resp, crate::i18n::tr("Navigator"));
    if (resp.clicked() || resp.dragged())
        && let Some(q) = resp.interact_pointer_pos()
    {
        // centre the view on the point under the pointer
        let u = ((q.x - frame.left()) / frame.width()).clamp(0.0, 1.0);
        let v = ((q.y - frame.top()) / frame.height()).clamp(0.0, 1.0);
        app.ui.pan = (u, v);
    }
    Some(frame.expand(5.0))
}

// ------------------------------------------------------------------------ filmstrip

/// A filmstrip cell's file-name label: names longer than 14 characters are cut to 13 and an
/// ellipsis. Counts characters, not bytes, so a CJK name is never cut inside a character (#266).
pub(super) fn film_label(name: &str) -> String {
    if name.chars().nth(14).is_some() { format!("{}…", name.chars().take(13).collect::<String>()) } else { name.to_string() }
}

/// Draw the horizontal filmstrip in `r`: selectable thumbnail cells with names and format when
/// film names are on, badges when film badges are on, and stack counts, following the active
/// photo and scrolling sideways with the wheel. Clicks select: Command toggles and Shift extends
/// the range; in Compare they adjust the pair's selection, and a plain click replaces the active
/// side.
pub(crate) fn filmstrip(app: &mut LightcraftApp, ui: &mut egui::Ui, r: Rect) {
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_filled(r, 0.0, t.canvas);
    ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(r.width(), 4.0)), 0.0, Color32::from_gray(0x20));
    let ids = app.session.visible_cloned();
    let active = app.session.selection.active;
    let cell_w = 120.0;
    let ppp = ui.ctx().pixels_per_point();
    if ids.is_empty() {
        let why = if app.session.filter != Default::default() { "No photos match the filters (View → Clear Filters)" } else { "No photos" };
        ui.painter().text(r.center(), Align2::CENTER_CENTER, why, t.font(12.5), t.text_dim);
        return;
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink2(vec2(0.0, 4.0))));
    // a vertical mouse wheel scrolls the strip sideways (horizontal trackpad scrolls still work)
    child.style_mut().always_scroll_the_only_direction = true;
    // only a new active photo (or the strip appearing) scrolls it; see `grid::follow_active`
    let follow = crate::panels::grid::follow_active(child.ctx(), egui::Id::unique("film-follow-active"), active, app.session.source);
    // a wheel notch scrolls a third of its width; see `grid::wheel_step`
    register(ui, "filmstrip", child.max_rect());
    let wheel = crate::panels::grid::wheel_step(&child, egui::Id::unique("film-wheel"), child.max_rect(), child.max_rect().width(), true);
    egui::ScrollArea::horizontal().id_salt("filmstrip").auto_shrink([false, false]).show_viewport(&mut child, |ui, vp| {
        if wheel != egui::Vec2::ZERO {
            ui.scroll_with_delta_animation(wheel, egui::style::ScrollAnimation::none());
        }
        let (area, _) = ui.allocate_exact_size(vec2(ids.len() as f32 * cell_w, r.height() - 16.0), Sense::hover());
        app.film_scroll = Some(vp.left());
        for (i, id) in ids.iter().enumerate() {
            let cr = Rect::from_min_size(pos2(area.left() + i as f32 * cell_w, area.top()), vec2(cell_w, area.height()));
            let local = Rect::from_min_size(pos2(i as f32 * cell_w, 0.0), cr.size());
            // centred when it is (partly) off screen; a click on a visible thumbnail leaves the strip alone
            if follow && Some(*id) == active && !(local.left() >= vp.left() && local.right() <= vp.right()) {
                ui.scroll_to_rect(cr, Some(egui::Align::Center));
            }
            if !local.intersects(vp.expand2(vec2(cell_w * 4.0, 0.0))) {
                continue;
            }
            let resp = ui.interact(cr, egui::Id::unique(("film", id.0)), Sense::click());
            let failed = app.renderer.thumb(*id).is_none() && app.renderer.failure(Slot::Thumb(*id)).is_some();
            crate::widgets::announce(&resp, film_spoken(app.session.catalog.photo(*id).map_or("", |p| p.file_name.as_str()), failed));
            register(ui, format!("film:{}", id.0), cr);
            let state = app.session.selection.state_of(*id);
            let sel = state == lightcraft_engine::SelectionState::Active;
            let p = ui.painter();
            let label = app.session.catalog.photo(*id).and_then(|ph| ph.label);
            let selected = state != lightcraft_engine::SelectionState::NotSelected;
            let base = if selected {
                t.cell_selected
            } else if resp.hovered() {
                t.cell_selected.gamma_multiply(0.6)
            } else {
                t.canvas
            };
            p.rect_filled(cr, 0.0, crate::theme::label_background(base, label, selected));
            let names = app.ui.settings.film_names;
            if let Some(ph) = app.session.catalog.photo(*id).filter(|_| names) {
                let name = ph.file_name.rsplit_once('.').map(|(n, _)| n).unwrap_or(&ph.file_name);
                p.text(pos2(cr.left() + 8.0, cr.top() + 10.0), Align2::LEFT_CENTER, film_label(name), t.font(10.0), t.text_dim);
                p.text(pos2(cr.right() - 8.0, cr.top() + 10.0), Align2::RIGHT_CENTER, &ph.format, t.semibold(8.5), t.text_dim);
            }
            let img_area = Rect::from_min_max(cr.min + vec2(10.0, 22.0), cr.max - vec2(10.0, 8.0));
            crate::panels::grid::request_thumb(app, *id, (256.0 * ppp.min(2.0) / 2.0) as usize * 2, 8);
            let thumb = app.renderer.thumb(*id);
            if thumb.is_none() {
                p.rect_filled(img_area, 0.0, t.skeleton);
                if failed {
                    p.text(img_area.center(), Align2::CENTER_CENTER, "!", t.semibold(14.0), t.text);
                }
            }
            if let Some(tex) = thumb {
                let [tw, th] = tex.size;
                let s = (img_area.width() / tw as f32).min(img_area.height() / th as f32);
                let fr = Rect::from_center_size(img_area.center(), vec2(tw as f32 * s, th as f32 * s));
                p.image(tex.tex.id(), fr, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                if sel {
                    p.rect_stroke(fr, 0.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Outside);
                } else if state == lightcraft_engine::SelectionState::Selected {
                    p.rect_stroke(fr, 0.0, Stroke::new(1.5, Color32::from_gray(170)), StrokeKind::Outside);
                }
                if app.ui.settings.film_badges
                    && let Some(ph) = app.session.catalog.photo(*id)
                {
                    film_badges(p, &t, fr, ph);
                }
                if let Some(st) = app.session.catalog.stack_of(*id) {
                    let collapsed = st.collapsed;
                    let text =
                        if collapsed { st.photos.len().to_string() } else { format!("{}/{}", st.position(*id).unwrap_or(0) + 1, st.photos.len()) };
                    let g = p.layout_no_wrap(text, t.semibold(9.5), Color32::WHITE);
                    let br = Rect::from_min_size(fr.min + vec2(3.0, 3.0), vec2(g.size().x + 22.0, 15.0));
                    // like the grid's badge: a click expands / collapses the stack
                    let badge = ui.interact(br, egui::Id::unique(("film-stack-badge", id.0)), Sense::click()).on_hover_text(if collapsed {
                        "Stack — click to expand"
                    } else {
                        "Stack — click to collapse"
                    });
                    crate::widgets::announce(&badge, crate::i18n::tr(if collapsed { "Expand Stack" } else { "Collapse Stack" }));
                    register(ui, format!("filmstack:{}", id.0), br);
                    let p = ui.painter();
                    let fill = if badge.hovered() { Color32::from_black_alpha(230) } else { Color32::from_black_alpha(170) };
                    p.rect_filled(br, 7.5, fill);
                    crate::icons::paint(p, Rect::from_min_size(br.min + vec2(4.0, 2.0), vec2(11.0, 11.0)), crate::icons::Icon::Stack, Color32::WHITE);
                    p.galley(pos2(br.min.x + 17.0, br.center().y - g.size().y / 2.0), g, Color32::WHITE);
                    if badge.clicked() {
                        crate::panels::grid::toggle_stack(app, ui.ctx(), *id);
                    }
                }
            }
            if resp.clicked() {
                let m = ui.input(|i| i.modifiers);
                let compare = app.ui.view == crate::state::ViewMode::Compare;
                let r = if compare && !m.command && !m.shift {
                    // Compare: the clicked photo replaces the active side (or swaps with the other one)
                    app.run("compare.set", json!({"id": id.0}))
                } else if compare {
                    // ⌘/Ctrl- and Shift-clicks change the selection; the pair on screen stays (#187)
                    crate::panels::compare::select_with_modifier(app, *id, !m.command)
                } else {
                    let mode = if m.command {
                        "toggle"
                    } else if m.shift {
                        "range"
                    } else {
                        "replace"
                    };
                    app.run("library.select", json!({"ids": [id.0], "mode": mode}))
                };
                if compare && let Err(e) = r {
                    app.toast(ui.ctx(), e);
                }
            }
            // the same photo actions as the grid and loupe (Restore / Delete Permanently in Recently Deleted)
            crate::widgets::context_menu(&resp, |ui| crate::panels::grid::context_menu(app, ui, *id));
        }
    });
}

/// What a screen reader says for a filmstrip thumbnail: the file, and why there is no picture
/// when the thumbnail failed (the painted `!` has no name of its own). The name is empty when the
/// catalog no longer has the photo (or its file name is blank); the reason then stands alone.
pub(crate) fn film_spoken(file_name: &str, failed: bool) -> String {
    match (file_name.is_empty(), failed) {
        (_, false) => file_name.to_string(),
        (true, true) => crate::i18n::tr("This photo can't be opened").to_string(),
        (false, true) => format!("{file_name}, {}", crate::i18n::tr("This photo can't be opened")),
    }
}

/// Rating, flag and edited badges along a filmstrip thumbnail's bottom edge (Settings → Interface).
fn film_badges(p: &egui::Painter, t: &Tokens, fr: Rect, ph: &lightcraft_catalog::Photo) {
    use crate::icons::{Icon, paint};
    use lightcraft_catalog::Flag;
    let edited = ph.is_edited();
    if ph.rating == 0 && ph.flag == Flag::None && !edited {
        return;
    }
    let bar = Rect::from_min_max(pos2(fr.left(), fr.bottom() - 15.0), fr.right_bottom());
    p.rect_filled(bar, 0.0, Color32::from_black_alpha(130));
    let y = bar.center().y;
    let mut x = bar.left() + 3.0;
    x += crate::rating::paint_rating(p, pos2(x, y), ph.rating, crate::rating::RatingSize::Small, t.star) + 2.0;
    match ph.flag {
        Flag::Pick => paint(p, Rect::from_min_size(pos2(x, y - 5.0), vec2(10.0, 10.0)), Icon::FlagPickFilled, t.pick),
        Flag::Reject => paint(p, Rect::from_min_size(pos2(x, y - 5.0), vec2(10.0, 10.0)), Icon::FlagReject, t.reject),
        Flag::None => {}
    }
    if edited {
        paint(p, Rect::from_min_size(pos2(bar.right() - 13.0, y - 5.0), vec2(10.0, 10.0)), Icon::Sliders, t.text_label);
    }
}
