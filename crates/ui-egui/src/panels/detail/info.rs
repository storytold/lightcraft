use super::geometry::focus_box;
use super::*;

/// Draw the camera's focus box over a photo drawn in `img` (View ▸ Focus Point): green corner
/// brackets with a dark keyline, the way cameras show their AF frame, never smaller than 14 pt.
/// `key` names it for tests and the control channel (`canvas:focusPoint`, `compare:focusPoint:<id>`).
pub(crate) fn draw_focus_point(p: &egui::Painter, img: Rect, photo: &lightcraft_catalog::Photo, key: &str) {
    let Some(r) = focus_box(photo) else { return };
    let at = |x: f64, y: f64| pos2(img.left() + x as f32 * img.width(), img.top() + y as f32 * img.height());
    let b = Rect::from_two_pos(at(r.x0, r.y0), at(r.x1, r.y1));
    let b = Rect::from_center_size(b.center(), b.size().max(vec2(14.0, 14.0)));
    let arm = (b.width().min(b.height()) * 0.3).max(5.0);
    let green = Color32::from_rgb(90, 230, 110);
    for (c, dx, dy) in [(b.left_top(), 1.0, 1.0), (b.right_top(), -1.0, 1.0), (b.left_bottom(), 1.0, -1.0), (b.right_bottom(), -1.0, -1.0)] {
        let pts = vec![c + vec2(dx * arm, 0.0), c, c + vec2(0.0, dy * arm)];
        p.add(egui::Shape::line(pts.clone(), Stroke::new(4.0, Color32::from_black_alpha(170))));
        p.add(egui::Shape::line(pts, Stroke::new(2.0, green)));
    }
    let c = b.center();
    p.circle_filled(c, 2.5, Color32::from_black_alpha(170));
    p.circle_filled(c, 1.5, green);
    register_geometry(p.ctx(), key, b);
}

/// View Options' Show Briefly When Photo Changes: note when the loupe changes to photo `id`, and
/// whether the brief info overlay shows this frame (until [`BRIEF_INFO_SECS`] have passed; a
/// repaint is asked for then, so it hides without input).
///
/// [`BRIEF_INFO_SECS`]: crate::state::BRIEF_INFO_SECS
pub(super) fn brief_info(app: &mut LightcraftApp, ctx: &egui::Context, id: PhotoId) {
    let now = ctx.input(|i| i.time);
    let brief = &mut app.ui.brief_info;
    if brief.photo != Some(id) {
        brief.photo = Some(id);
        brief.since = now;
    }
    let left = crate::state::BRIEF_INFO_SECS - (now - brief.since);
    brief.shown = app.ui.settings.loupe_info.briefly != crate::state::InfoOverlay::Off && left > 0.0;
    if brief.shown {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(left));
    }
}

/// The dark pill behind a label drawn over the photo (`Before`, a hovered preset's name): at the
/// photo's top left, inset 8 points, with the `text` size plus padding. Zoomed in, the photo
/// reaches past its `pane` (the canvas): the pill then sits at the pane's top left, so the label
/// stays in view (issue #2267: at 100 % it left the screen with the photo's corner), below the
/// `top_inset` the filter pill takes in that corner, and never past the pane's right edge.
pub(super) fn label_badge_rect(photo: Rect, pane: Rect, text: egui::Vec2, top_inset: f32) -> Rect {
    let size = text + vec2(16.0, 8.0);
    let left = photo.left().max(pane.left()).min(pane.right() - size.x - 16.0).max(pane.left());
    let corner = pos2(left, photo.top().max(pane.top() + top_inset));
    Rect::from_min_size(corner + vec2(8.0, 8.0), size)
}

/// The info overlay at the canvas' top left (`view.infoOverlay`): the lines View Options chose for
/// Info 1 or Info 2 (`view.viewOptions`).
pub(super) fn info_overlay(app: &LightcraftApp, p: &egui::Painter, canvas: Rect, photo: &lightcraft_catalog::Photo) {
    let Some(lines) = info_overlay_lines(app, photo) else { return };
    let t = Tokens::get(p.ctx());
    let mut y = canvas.top() + info_overlay_top(app, &t);
    for (i, l) in lines.iter().enumerate() {
        // laid out uncoloured: `galley` only fills placeholder text with its colour, so a white
        // layout made the shadow a second white copy (#1690)
        let g = p.layout_no_wrap(l.clone(), info_overlay_font(&t, i), Color32::PLACEHOLDER);
        let at = pos2(canvas.left() + 14.0, y);
        // a soft shadow keeps the text readable on bright photos
        p.galley(at + vec2(1.0, 1.0), g.clone(), Color32::from_black_alpha(200));
        p.galley(at, g.clone(), Color32::WHITE);
        y += g.size().y + INFO_OVERLAY_GAP;
    }
    y = overlay_badges(p, &t, pos2(canvas.left() + 14.0, y), photo);
    register_geometry(p.ctx(), "canvas:infoOverlay", Rect::from_min_max(canvas.min, pos2(canvas.left() + 320.0, y)));
}

/// Where the info overlay's text starts below the canvas' top (without window controls over it),
/// and the gap after each line.
const INFO_OVERLAY_TOP: f32 = 12.0;

const INFO_OVERLAY_GAP: f32 = 3.0;

/// The info overlay's font for line `i` (the file name is the first, larger line).
fn info_overlay_font(t: &Tokens, i: usize) -> egui::FontId {
    if i == 0 { t.semibold(15.0) } else { t.font(12.5) }
}

/// How far below the canvas' top the info overlay's text and badges reach (0 when it is off), so a
/// label drawn in that corner can sit below it.
pub(super) fn info_overlay_extent(app: &LightcraftApp, p: &egui::Painter, photo: &lightcraft_catalog::Photo) -> f32 {
    let Some(lines) = info_overlay_lines(app, photo) else { return 0.0 };
    let t = Tokens::get(p.ctx());
    let text = lines.iter().enumerate().fold(info_overlay_top(app, &t), |y, (i, l)| {
        y + p.layout_no_wrap(l.clone(), info_overlay_font(&t, i), Color32::PLACEHOLDER).size().y + INFO_OVERLAY_GAP
    });
    text + if has_overlay_badges(photo) { OVERLAY_BADGES_H } else { 0.0 }
}

/// Where the info overlay's text starts below the canvas' top: the full-screen preview has no top
/// bar, so on macOS it starts below the window controls that sit over the photo there (#2460).
fn info_overlay_top(app: &LightcraftApp, t: &Tokens) -> f32 {
    let controls = app.ui.fullscreen && app.integrated_titlebar && !app.window_is_fullscreen;
    if controls { t.top_bar_h } else { INFO_OVERLAY_TOP }
}

/// The info overlay's lines for `photo`, or `None` when it is off.
fn info_overlay_lines(app: &LightcraftApp, photo: &lightcraft_catalog::Photo) -> Option<Vec<String>> {
    use crate::state::InfoOverlay;
    // with the overlay off, Show Briefly When Photo Changes shows its Info for a moment
    let brief = app.ui.info_overlay == InfoOverlay::Off && app.ui.brief_info.shown;
    let mode = if brief { app.ui.settings.loupe_info.briefly } else { app.ui.info_overlay };
    let fields = match mode {
        InfoOverlay::Off => return None,
        InfoOverlay::Basic => &app.ui.settings.loupe_info.info1,
        InfoOverlay::Exposure => &app.ui.settings.loupe_info.info2,
    };
    let lines: Vec<String> = fields.iter().filter_map(|f| info_line(*f, photo)).collect();
    if lines.is_empty() {
        return None;
    }
    Some(lines)
}

/// The height of the info overlay's rating and flag row.
const OVERLAY_BADGES_H: f32 = 17.0;

fn has_overlay_badges(photo: &lightcraft_catalog::Photo) -> bool {
    photo.rating > 0 || photo.flag != lightcraft_catalog::Flag::None
}

/// The star rating and flag under the info overlay's lines, when the photo has either; returns
/// the y below them.
fn overlay_badges(p: &egui::Painter, t: &Tokens, at: Pos2, photo: &lightcraft_catalog::Photo) -> f32 {
    use crate::icons::{Icon, paint};
    use lightcraft_catalog::Flag;
    let flag = match photo.flag {
        Flag::Pick => Some((Icon::FlagPickFilled, Color32::WHITE)),
        Flag::Reject => Some((Icon::FlagReject, t.reject)),
        Flag::None => None,
    };
    if !has_overlay_badges(photo) {
        return at.y;
    }
    let stars = photo.rating.min(5);
    let mut icons: Vec<(Icon, Color32)> = vec![(Icon::StarFilled, Color32::WHITE); stars as usize];
    icons.extend(flag);
    let side = 12.0;
    let mut x = at.x;
    for (i, (icon, color)) in icons.into_iter().enumerate() {
        // the flag stands a little apart from the stars
        if i > 0 && i == stars as usize {
            x += 5.0;
        }
        let r = Rect::from_min_size(pos2(x, at.y + 1.0), vec2(side, side));
        paint(p, r.translate(vec2(1.0, 1.0)), icon, Color32::from_black_alpha(200));
        paint(p, r, icon, color);
        x += side + 2.0;
    }
    register_geometry(p.ctx(), "canvas:infoOverlayBadges", Rect::from_min_max(at, pos2(x, at.y + side + 2.0)));
    at.y + OVERLAY_BADGES_H
}

/// The filter pill's inset from the canvas' top-left corner and its height (the same inset from
/// the left as from the top, so the pill sits in the corner, #1645).
pub(super) const FILTER_PILL_INSET: f32 = 3.0;

pub(super) const FILTER_PILL_HEIGHT: f32 = 20.0;

/// Whether [`filter_pill`] has something to show (a filter is active).
pub(super) fn filter_pill_shown(app: &LightcraftApp) -> bool {
    !lightcraft_engine::filter_chips(&app.session.filter, &app.session.catalog).is_empty()
}

/// A small pill at the loupe's top left naming the active filters: the filmstrip and Next / Previous
/// follow them, so a filter must never be invisible here. A click clears them all. It sits in the
/// canvas margin, so the photo does not move.
pub(super) fn filter_pill(app: &mut LightcraftApp, ui: &mut egui::Ui, canvas: Rect) {
    let chips = lightcraft_engine::filter_chips(&app.session.filter, &app.session.catalog);
    if chips.is_empty() {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let p = ui.painter_at(canvas);
    let label =
        chips.iter().map(|c| crate::panels::chips::display_label(c, &app.session.filter, &app.session.catalog)).collect::<Vec<_>>().join("  ·  ");
    let g = p.layout_no_wrap(crate::i18n::tr_format!("Filtered: {label}", label = label), t.font(12.0), Color32::from_gray(225));
    let inset = FILTER_PILL_INSET;
    let w = (g.size().x + 40.0).min((canvas.width() - 2.0 * inset).max(60.0));
    let r = Rect::from_min_size(canvas.min + vec2(inset, inset), vec2(w, FILTER_PILL_HEIGHT));
    let resp = ui.interact(r, egui::Id::unique("loupe-filter-pill"), Sense::click());
    crate::widgets::announce(&resp, crate::i18n::tr("Filters"));
    register(ui, "loupe:filters", r);
    p.rect_filled(r, 10.0, Color32::from_black_alpha(if resp.hovered() { 225 } else { 185 }));
    let text_clip = Rect::from_min_max(r.min, pos2(r.right() - 24.0, r.max.y));
    p.with_clip_rect(text_clip).galley(pos2(r.left() + 10.0, r.center().y - g.size().y / 2.0), g, Color32::from_gray(225));
    let (c, m) = (pos2(r.right() - 13.0, r.center().y), 3.0);
    let cross = Stroke::new(1.3, if resp.hovered() { Color32::WHITE } else { Color32::from_gray(190) });
    p.line_segment([c - vec2(m, m), c + vec2(m, m)], cross);
    p.line_segment([c - vec2(m, -m), c + vec2(m, -m)], cross);
    if resp.on_hover_text(crate::i18n::tr("Click to clear these filters")).clicked() {
        let _ = app.run("library.clearFilter", json!({}));
    }
}

/// A raw shown from its embedded JPEG (issue #10): a pill at the canvas' top centre saying so;
/// hovering it explains (registered as `notice:previewOnly:loupe`).
pub(super) fn preview_only_pill(ui: &mut egui::Ui, canvas: Rect, reason: &str) {
    let t = Tokens::get(ui.ctx());
    let p = ui.painter_at(canvas);
    let text = crate::i18n::tr_format!("Preview only — editing the camera's embedded JPEG ({})", crate::widgets::preview_only_variant(reason));
    let g = p.layout_no_wrap(text, t.font(12.0), Color32::WHITE);
    let size = vec2(g.size().x + 40.0, 26.0);
    let r = Rect::from_min_size(pos2(canvas.center().x - size.x / 2.0, canvas.top() + 12.0), size);
    p.rect_filled(r, 13.0, Color32::from_black_alpha(185));
    crate::icons::paint(&p, Rect::from_center_size(pos2(r.left() + 16.0, r.center().y), vec2(15.0, 15.0)), crate::icons::Icon::Info, t.caution);
    p.galley(pos2(r.left() + 29.0, r.center().y - g.size().y / 2.0), g, Color32::WHITE);
    register(ui, "notice:previewOnly:loupe", r);
    ui.interact(r, egui::Id::unique("preview-only-pill"), Sense::hover()).on_hover_text(crate::widgets::preview_only_explanation(reason));
}

/// One line of the info overlay for `photo`; `None` when the field is None or the photo has
/// nothing to show for it.
fn info_line(field: crate::state::LoupeInfoField, photo: &lightcraft_catalog::Photo) -> Option<String> {
    use crate::state::LoupeInfoField as F;
    let m = &photo.meta;
    let text = match field {
        F::None => return None,
        F::FileName => photo.file_name.clone(),
        F::CaptureDate => pretty_date(photo.captured.as_deref().unwrap_or(&photo.imported)),
        F::Dimensions => format!("{} × {}", photo.width, photo.height),
        F::Exposure => {
            let mut exp = Vec::new();
            if !m.shutter.is_empty() {
                exp.push(format!("{} s", m.shutter));
            }
            if let Some(a) = m.aperture {
                exp.push(format!("f/{a:.1}"));
            }
            if let Some(i) = m.iso {
                exp.push(format!("ISO {i}"));
            }
            if let Some(f) = m.focal_mm {
                exp.push(format!("{f:.0} mm"));
            }
            if exp.is_empty() { "No exposure information".into() } else { exp.join("  ") }
        }
        F::Camera => m.camera.clone(),
        F::Lens => m.lens.clone(),
        F::CameraAndLens => [m.camera.as_str(), m.lens.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" · "),
    };
    (!text.is_empty()).then_some(text)
}

/// `2026-09-30T12:00:00` → `2026-09-30 12:00`.
fn pretty_date(iso: &str) -> String {
    let d = iso.replacen('T', " ", 1);
    d.get(..16).map(str::to_string).unwrap_or(d)
}

/// How long a render may be on its way, with nothing new on screen, before the loupe says
/// "Rendering…" (s): as long as the activity stack waits before it shows a task.
pub(super) const RENDERING_AFTER_SECS: f64 = 0.5;

/// What [`rendering_overdue`] keeps between frames while a render is coming: the texture keys on
/// screen, since when, and whether the pill is up.
pub(crate) type RenderingWait = Option<([Option<u64>; 4], f64, bool)>;

/// Whether the loupe says "Rendering…": a render has been `coming` for [`RENDERING_AFTER_SECS`] with
/// no new texture `keys` on screen. The wait starts when the render starts coming (an idle app draws
/// no frames, so the last one may be long ago), and once up the pill stays until nothing is coming:
/// drafts slower than the wait don't make it blink.
pub(crate) fn rendering_overdue(wait: &mut RenderingWait, keys: [Option<u64>; 4], coming: bool, now: f64) -> bool {
    if !coming {
        *wait = None;
        return false;
    }
    let (seen, since, up) = wait.get_or_insert((keys, now, false));
    if !*up && *seen != keys {
        (*seen, *since) = (keys, now);
    }
    *up |= now - *since > RENDERING_AFTER_SECS;
    *up
}

/// A pill over the photo, 12 pt above the bottom of `over` or just above the Navigator when that
/// is in the way, registered as `id`; `caution` adds the preview-only pill's caution icon. The
/// loupe's "Rendering…" says the pixels on screen aren't final, so it isn't an activity row (see
/// docs/background-tasks.md); "This photo can't be opened" says the stand-in on screen is all
/// there will be.
pub(crate) fn loupe_pill(ui: &egui::Ui, canvas: Rect, over: Rect, navigator: Option<Rect>, id: &str, text: &str, caution: bool) -> Rect {
    let t = Tokens::get(ui.ctx());
    let p = ui.painter_at(canvas);
    let g = p.layout_no_wrap(text.to_string(), t.font(12.0), Color32::from_gray(225));
    let inset = if caution { 29.0 } else { 10.0 };
    let size = vec2(g.size().x + inset + 10.0, 20.0);
    let mut r = Rect::from_center_size(pos2(over.center().x, over.bottom() - 12.0 - size.y / 2.0), size);
    if let Some(nav) = navigator.filter(|n| n.expand(8.0).intersects(r)) {
        r = Rect::from_min_size(pos2(nav.right() - size.x, nav.top() - 8.0 - size.y), size);
    }
    p.rect_filled(r, 10.0, Color32::from_black_alpha(185));
    if caution {
        crate::icons::paint(&p, Rect::from_center_size(pos2(r.left() + 16.0, r.center().y), vec2(13.0, 13.0)), crate::icons::Icon::Info, t.caution);
    }
    p.galley(pos2(r.left() + inset, r.center().y - g.size().y / 2.0), g, Color32::from_gray(225));
    register(ui, id, r);
    r
}

/// The caution pill saying a render failed behind the stand-in on screen (see [`loupe_pill`]):
/// "Original offline · no smart preview" when that is why, else "This photo can't be opened", with
/// the error on hover and for screen readers.
#[allow(clippy::too_many_arguments)]
pub(crate) fn failed_pill(
    ui: &egui::Ui,
    app: &LightcraftApp,
    photo: &lightcraft_catalog::Photo,
    canvas: Rect,
    over: Rect,
    navigator: Option<Rect>,
    id: &str,
    error: &str,
) {
    let offline = matches!(&photo.source, lightcraft_catalog::Source::File { path } if app.session.media.availability.is_offline(path));
    let text = crate::i18n::tr(if offline { "Original offline · no smart preview" } else { "This photo can't be opened" });
    let r = loupe_pill(ui, canvas, over, navigator, id, text, true);
    let resp = ui.interact(r, egui::Id::unique(("render-failed", id)), Sense::hover()).on_hover_text(error);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, format!("{text}: {error}")));
}

pub(super) fn quick_name(q: lightcraft_engine::media::QuickSource) -> &'static str {
    use lightcraft_engine::media::QuickSource;
    match q {
        QuickSource::Cached => "cached",
        QuickSource::Embedded => "embedded",
        QuickSource::Small => "small",
    }
}

#[cfg(test)]
mod filter_pill_tests {
    use super::*;

    /// #1645: the filter pill sits in the loupe's top-left corner as far from the left edge as
    /// from the top one.
    #[test]
    fn the_filter_pill_is_inset_equally_from_both_edges() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        app.session.filter.text = "717".into();
        let ctx = egui::Context::default();
        let canvas = Rect::from_min_size(pos2(52.0, 82.0), vec2(800.0, 500.0));
        let raw = crate::headless::HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        let mut out = ctx.run_ui(raw, |ui| filter_pill(&mut app, ui, canvas));
        out.textures_delta.clear();
        let widgets = crate::widgets::take_registry(&ctx);
        let pill = widgets.iter().find(|(w, _)| w == "loupe:filters").map(|(_, r)| *r).expect("the pill shows with a search");
        let (left, top) = (pill.left() - canvas.left(), pill.top() - canvas.top());
        assert!(top > 0.0, "inside the canvas: {pill:?}");
        assert_eq!(left, top, "left and top insets: {pill:?} in {canvas:?}");
    }
}

#[cfg(test)]
mod info_overlay_tests {
    use super::*;

    /// #1690: the info overlay's shadow is a dark copy under the text. Drawn as a second white
    /// copy one point off, each line read doubled.
    #[test]
    fn the_info_overlay_shadow_is_dark() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        app.ui.info_overlay = crate::state::InfoOverlay::Basic;
        let photo = app.session.catalog.photos().next().map(|p| (**p).clone()).expect("the demo catalog has photos");
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let canvas = Rect::from_min_size(pos2(52.0, 82.0), vec2(800.0, 500.0));
        let raw = crate::headless::HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        let mut out = ctx.run_ui(raw, |ui| info_overlay(&app, ui.painter(), canvas, &photo));
        out.textures_delta.clear();
        // the colour a text shape paints its glyphs in, as epaint resolves it
        let ink = |s: &egui::epaint::TextShape| {
            let c = s.galley.job.sections.first().map_or(Color32::PLACEHOLDER, |sec| sec.format.color);
            s.override_text_color.unwrap_or(if c == Color32::PLACEHOLDER { s.fallback_color } else { c })
        };
        fn texts(shape: &egui::epaint::Shape, out: &mut Vec<egui::epaint::TextShape>) {
            match shape {
                egui::epaint::Shape::Text(t) => out.push(t.clone()),
                egui::epaint::Shape::Vec(v) => v.iter().for_each(|s| texts(s, out)),
                _ => {}
            }
        }
        let mut all = Vec::new();
        out.shapes.iter().for_each(|c| texts(&c.shape, &mut all));
        let copies: Vec<Color32> = all.iter().filter(|t| t.galley.job.text == photo.file_name).map(ink).collect();
        assert_eq!(copies.len(), 2, "the file name and its shadow: {copies:?}");
        assert_eq!(copies[1], Color32::WHITE, "the text on top");
        assert!(copies[0].r() < 64 && copies[0].a() > 0, "the shadow under it is dark: {:?}", copies[0]);
    }

    /// View Options' Show Briefly When Photo Changes: with the overlay off, the chosen Info shows
    /// for `BRIEF_INFO_SECS` after the loupe changes photo, then hides; an overlay that is on stays.
    #[test]
    fn show_briefly_shows_the_info_after_the_photo_changes() {
        use crate::state::{BRIEF_INFO_SECS, InfoOverlay};
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        let photos: Vec<lightcraft_catalog::Photo> = app.session.catalog.photos().take(2).map(|p| (**p).clone()).collect();
        assert_eq!(photos.len(), 2, "the demo catalog has two photos");
        let ctx = egui::Context::default();
        // the loupe shows photo `i` at `time`: the lines the overlay paints then
        let at = |app: &mut LightcraftApp, i: usize, time: f64| {
            let raw = crate::headless::HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, time, vec![]);
            let mut out = ctx.run_ui(raw, |ui| brief_info(app, ui.ctx(), photos[i].id));
            out.textures_delta.clear();
            painted_lines(app, &photos[i])
        };
        let name = |i: usize| photos[i].file_name.clone();
        assert!(at(&mut app, 0, 1.0).is_empty(), "off by default");
        app.ui.settings.loupe_info.briefly = InfoOverlay::Exposure;
        let info2 = at(&mut app, 1, 2.0);
        assert_eq!(info2.first(), Some(&name(1)), "Info 2 shows when the photo changes: {info2:?}");
        assert!(!at(&mut app, 1, 2.0 + BRIEF_INFO_SECS - 0.5).is_empty(), "still up");
        assert!(at(&mut app, 1, 2.0 + BRIEF_INFO_SECS + 0.1).is_empty(), "then hidden");
        assert_eq!(at(&mut app, 0, 10.0).first(), Some(&name(0)), "and shown again for the next photo");
        app.ui.settings.loupe_info.briefly = InfoOverlay::Basic;
        let info1 = at(&mut app, 1, 20.0);
        assert_eq!(info1.len(), 3, "Info 1's three lines: {info1:?}");
        assert_ne!(info1, info2, "Info 1, not Info 2");
        app.ui.info_overlay = InfoOverlay::Exposure;
        assert_eq!(at(&mut app, 1, 30.0), info2, "an overlay that is on shows its own Info, and stays");
    }

    /// The lines the overlay paints, top to bottom (each once: the shadow copies left out).
    fn painted_lines(app: &LightcraftApp, photo: &lightcraft_catalog::Photo) -> Vec<String> {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let canvas = Rect::from_min_size(pos2(52.0, 82.0), vec2(800.0, 500.0));
        let raw = crate::headless::HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        let mut out = ctx.run_ui(raw, |ui| info_overlay(app, ui.painter(), canvas, photo));
        out.textures_delta.clear();
        let mut lines = Vec::new();
        for c in &out.shapes {
            if let egui::epaint::Shape::Text(t) = &c.shape
                && t.override_text_color.unwrap_or(t.fallback_color) == Color32::WHITE
            {
                lines.push(t.galley.job.text.clone());
            }
        }
        lines
    }

    /// View ▸ View Options: Info 1 and Info 2 each show the three lines chosen for them, in that
    /// order, without the lines set to None.
    #[test]
    fn the_info_overlay_shows_the_lines_chosen_in_view_options() {
        use crate::state::{InfoOverlay, LoupeInfoField as F};
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        let photo = app.session.catalog.photos().next().map(|p| (**p).clone()).expect("the demo catalog has photos");
        let size = format!("{} × {}", photo.width, photo.height);
        let date = pretty_date(photo.captured.as_deref().unwrap_or(&photo.imported));
        app.ui.settings.loupe_info.info1 = [F::Dimensions, F::None, F::FileName];
        app.ui.settings.loupe_info.info2 = [F::CaptureDate, F::FileName, F::None];
        app.ui.info_overlay = InfoOverlay::Basic;
        assert_eq!(painted_lines(&app, &photo), vec![size.clone(), photo.file_name.clone()], "Info 1");
        app.ui.info_overlay = InfoOverlay::Exposure;
        assert_eq!(painted_lines(&app, &photo), vec![date, photo.file_name.clone()], "Info 2");
        app.ui.settings.loupe_info.info1 = [F::None; 3];
        app.ui.info_overlay = InfoOverlay::Basic;
        assert_eq!(painted_lines(&app, &photo), Vec::<String>::new(), "nothing chosen, nothing shown");
    }

    fn demo_app() -> (LightcraftApp, lightcraft_catalog::Photo) {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        app.ui.info_overlay = crate::state::InfoOverlay::Basic;
        let photo = app.session.catalog.photos().next().map(|p| (**p).clone()).expect("the demo catalog has photos");
        (app, photo)
    }

    /// The overlay's text shapes and the rects it registered, painted on a canvas at the window's corner.
    fn paint_overlay(app: &LightcraftApp, photo: &lightcraft_catalog::Photo) -> (Vec<egui::epaint::TextShape>, Vec<(String, Rect)>) {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let canvas = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 700.0));
        let raw = crate::headless::HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        let mut out = ctx.run_ui(raw, |ui| info_overlay(app, ui.painter(), canvas, photo));
        out.textures_delta.clear();
        fn texts(shape: &egui::epaint::Shape, out: &mut Vec<egui::epaint::TextShape>) {
            match shape {
                egui::epaint::Shape::Text(t) => out.push(t.clone()),
                egui::epaint::Shape::Vec(v) => v.iter().for_each(|s| texts(s, out)),
                _ => {}
            }
        }
        let mut all = Vec::new();
        out.shapes.iter().for_each(|c| texts(&c.shape, &mut all));
        (all, crate::widgets::take_registry(&ctx))
    }

    fn name_top(app: &LightcraftApp, photo: &lightcraft_catalog::Photo) -> f32 {
        let (texts, _) = paint_overlay(app, photo);
        texts.iter().filter(|t| t.galley.job.text == photo.file_name).map(|t| t.pos.y).fold(f32::INFINITY, f32::min)
    }

    /// #2460: in the full-screen preview on macOS the window controls stay at the top-left, so the
    /// file name starts below them; in macOS full screen they are gone and it keeps the corner.
    #[test]
    fn the_file_name_clears_the_macos_window_controls_in_the_full_screen_preview() {
        let (mut app, photo) = demo_app();
        app.ui.fullscreen = true;
        assert_eq!(name_top(&app, &photo), 12.0, "no window controls over the photo");
        app.integrated_titlebar = true;
        let bar = Tokens::get(&egui::Context::default()).top_bar_h;
        assert!(name_top(&app, &photo) >= bar, "below the window controls");
        app.window_is_fullscreen = true;
        assert_eq!(name_top(&app, &photo), 12.0, "macOS full screen hides the window controls");
        app.window_is_fullscreen = false;
        app.ui.fullscreen = false;
        assert_eq!(name_top(&app, &photo), 12.0, "the loupe's canvas starts below the top bar");
    }

    /// #2460: a rated or flagged photo shows its stars and flag under the date and size.
    #[test]
    fn the_info_overlay_shows_the_rating_and_flag_under_the_date() {
        let (_, mut photo) = demo_app();
        let (app, _) = demo_app();
        photo.rating = 0;
        photo.flag = lightcraft_catalog::Flag::None;
        let badges = |photo: &lightcraft_catalog::Photo| {
            paint_overlay(&app, photo).1.into_iter().find(|(id, _)| id == "canvas:infoOverlayBadges").map(|(_, r)| r)
        };
        assert_eq!(badges(&photo), None, "nothing to show");
        photo.rating = 3;
        let stars = badges(&photo).expect("three stars");
        photo.flag = lightcraft_catalog::Flag::Pick;
        let flagged = badges(&photo).expect("three stars and a flag");
        assert!(flagged.width() > stars.width(), "the flag follows the stars: {stars:?} {flagged:?}");
        let (texts, _) = paint_overlay(&app, &photo);
        let date_bottom = texts.iter().map(|t| t.pos.y + t.galley.size().y).fold(0.0, f32::max);
        assert!(flagged.top() >= date_bottom, "under the date and size: {flagged:?}, text ends at {date_bottom}");
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let raw = crate::headless::HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        let mut extent = 0.0;
        ctx.run_ui(raw, |ui| extent = info_overlay_extent(&app, ui.painter(), &photo)).textures_delta.clear();
        assert!(extent >= flagged.bottom(), "a label below the overlay clears the badges: {extent} < {flagged:?}");
        photo.rating = 0;
        photo.flag = lightcraft_catalog::Flag::Reject;
        assert!(badges(&photo).is_some(), "a rejected photo shows its flag alone");
    }
}
