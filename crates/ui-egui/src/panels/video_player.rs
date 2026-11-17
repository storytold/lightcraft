//! Video playback in the loupe (LR-VID-PLAY, KEY-VIDEO-PLAY): the frame the engine's player is on,
//! drawn upright in place of the poster, and the transport bar under it (play / pause, frame
//! steps, the scrub bar with the current time and the duration). Every control runs a `video.*`
//! command; [`tick`] advances the player once per frame and stops it when the loupe leaves the clip.
//!
//! A clip's audio is not played (LightCraft has no audio output yet): the bar says so, quietly,
//! when the clip has an audio track.

use std::sync::Arc;

use egui::{Align2, Color32, ColorImage, Id, Mesh, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, pos2, vec2};
use lightcraft_catalog::PhotoId;
use lightcraft_geom::Orientation;
use serde_json::json;

use crate::LightcraftApp;
use crate::icons::Icon;
use crate::state::ViewMode;
use crate::theme::Tokens;
use crate::widgets::{format_duration, icon_button, register};

/// The uploaded frame: whose, which ([`lightcraft_engine::video_play::VideoPlayer::frame`]'s
/// serial) and the texture. Kept in egui's memory, dropped with the player.
#[derive(Clone)]
struct FrameTexture {
    photo: PhotoId,
    serial: u64,
    tex: TextureHandle,
}

fn texture_id() -> Id {
    Id::unique("lc-video-frame")
}

/// The last frame's pixels as handed to egui: once egui has uploaded them (by the next frame) the
/// buffer is filled again instead of allocating 33 MB per 4K frame on the UI thread.
#[derive(Clone, Default)]
struct UploadBuffer(Option<Arc<ColorImage>>);

fn upload_id() -> Id {
    Id::unique("lc-video-upload")
}

/// Whether a frame of `size` fits the GPU's textures (`max_side` 0: unknown, no limit). The
/// player scales bigger pictures down or reports why it can't; this keeps one from ever reaching
/// the renderer, which would fail on it.
fn uploadable(size: [usize; 2], max_side: usize) -> bool {
    size[0] > 0 && size[1] > 0 && (max_side == 0 || (size[0] <= max_side && size[1] <= max_side))
}

/// Whether the loupe shows `app`'s active photo and it is a video: the clip Space plays.
pub(crate) fn loupe_video(app: &LightcraftApp) -> Option<PhotoId> {
    let loupe = app.ui.fullscreen || app.ui.view == ViewMode::Detail;
    let id = app.session.active()?;
    (loupe && app.ui.slideshow.is_none() && app.session.catalog.photo(id).is_some_and(|p| p.is_video())).then_some(id)
}

/// Once per frame, before the panels draw: stop playback when the loupe no longer shows the clip
/// (another photo, the grid, a slideshow), advance the player to the frame due now and ask for the
/// next repaint when that frame is due.
pub fn tick(app: &mut LightcraftApp, ctx: &egui::Context) {
    // frames are made to fit the GPU's textures, from the first one on
    app.session.video.max_side = app.max_texture_side;
    if let Some(p) = app.session.video.player.as_mut() {
        p.set_max_side(app.max_texture_side);
    }
    if app.session.video.player.is_none() {
        forget_texture(ctx);
        return;
    }
    if app.session.video.player.as_ref().map(|p| p.photo()) != loupe_video(app) {
        app.session.video_stop();
        forget_texture(ctx);
        return;
    }
    if let Some(wait) = app.session.video_tick() {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait.clamp(0.0, 0.25)));
    }
}

fn forget_texture(ctx: &egui::Context) {
    ctx.data_mut(|d| {
        d.remove::<FrameTexture>(texture_id());
        d.remove::<UploadBuffer>(upload_id());
    });
}

/// The texture of the player's frame for `id`, uploaded when the frame changed; `None` while the
/// player has no frame of this clip (the poster or the card shows then).
fn frame_texture(app: &LightcraftApp, ctx: &egui::Context, id: PhotoId) -> Option<(TextureHandle, [usize; 2])> {
    let player = app.session.video.player.as_ref().filter(|p| p.photo() == id)?;
    let (frame, serial) = player.frame()?;
    let size = [frame.width as usize, frame.height as usize];
    let cached = ctx.data(|d| d.get_temp::<FrameTexture>(texture_id()));
    if let Some(c) = cached.as_ref().filter(|c| c.photo == id && c.serial == serial) {
        return Some((c.tex.clone(), size));
    }
    if !uploadable(size, app.max_texture_side) || frame.rgba.len() != size[0].saturating_mul(size[1]).saturating_mul(4) {
        return None;
    }
    // the previous upload's buffer, when egui is done with it
    let reused = ctx.data_mut(|d| d.remove_temp::<UploadBuffer>(upload_id())).and_then(|b| b.0).and_then(|a| Arc::try_unwrap(a).ok());
    let mut pixels = reused.map(|i| i.pixels).unwrap_or_default();
    pixels.clear();
    // frames are opaque: a plain copy into egui's pixels
    pixels.extend(frame.rgba.chunks_exact(4).map(|p| Color32::from_rgb(p[0], p[1], p[2])));
    let image = Arc::new(ColorImage { size, source_size: vec2(size[0] as f32, size[1] as f32), pixels });
    ctx.data_mut(|d| d.insert_temp(upload_id(), UploadBuffer(Some(image.clone()))));
    let tex = match cached {
        Some(mut c) if c.tex.size() == size => {
            c.tex.set(image, TextureOptions::LINEAR);
            c.tex
        }
        _ => ctx.load_texture("video-frame", image, TextureOptions::LINEAR),
    };
    ctx.data_mut(|d| d.insert_temp(texture_id(), FrameTexture { photo: id, serial, tex: tex.clone() }));
    Some((tex, size))
}

/// The point of the stored picture (0..1) that display point `d` (0..1 of the upright rectangle)
/// shows: the inverse of the orientation (a horizontal flip, then quarter turns clockwise).
fn source_uv(o: Orientation, d: Pos2) -> Pos2 {
    let (flip, turns) = o.to_parts();
    let mut p = d;
    for _ in 0..turns {
        // undo one clockwise quarter turn
        p = pos2(p.y, 1.0 - p.x);
    }
    if flip { pos2(1.0 - p.x, p.y) } else { p }
}

/// The largest rectangle of the upright frame's aspect centred in `area`.
fn fit(area: Rect, size: [usize; 2], o: Orientation) -> Rect {
    let (w, h) = if o.swaps_axes() { (size[1], size[0]) } else { (size[0], size[1]) };
    let aspect = w.max(1) as f32 / h.max(1) as f32;
    let (aw, ah) = (area.width().max(1.0), area.height().max(1.0));
    let s = if aw / ah > aspect { vec2(ah * aspect, ah) } else { vec2(aw, aw / aspect) };
    Rect::from_center_size(area.center(), s)
}

/// Draw the player's frame of `id` upright in `area`. Returns where, or `None` when there is no
/// frame of this clip to draw.
pub(crate) fn draw_frame(app: &LightcraftApp, ui: &egui::Ui, p: &egui::Painter, id: PhotoId, area: Rect) -> Option<Rect> {
    let (tex, size) = frame_texture(app, ui.ctx(), id)?;
    let o = app.session.video.player.as_ref().and_then(|pl| pl.info()).map_or(Orientation::Normal, |i| i.orientation);
    let r = fit(area, size, o);
    let mut mesh = Mesh::with_texture(tex.id());
    let corners =
        [(r.left_top(), pos2(0.0, 0.0)), (r.right_top(), pos2(1.0, 0.0)), (r.right_bottom(), pos2(1.0, 1.0)), (r.left_bottom(), pos2(0.0, 1.0))];
    for (pos, d) in corners {
        mesh.vertices.push(egui::epaint::Vertex { pos, uv: source_uv(o, d), color: Color32::WHITE });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    p.add(mesh);
    register(ui, "canvas:videoFrame", r);
    Some(r)
}

/// The Play or Pause button's tooltip (`base`, translated) with the key that does it as bound now.
fn play_tip(keymap: &crate::shortcuts::Keymap, mac: bool, base: &str) -> String {
    let base = crate::i18n::tr(base);
    match crate::shortcuts::video_play_key(keymap) {
        Some(sc) => format!("{base} ({})", crate::menubar::shortcut_text(sc, mac)),
        None => base.to_string(),
    }
}

/// Height of the transport bar, and its distance from the canvas edges.
pub(crate) const BAR_H: f32 = 40.0;
const BAR_INSET: f32 = 12.0;

/// The transport bar along the bottom of the loupe for a video: play / pause, previous and next
/// frame, the scrub bar, the time and duration, the audio note and the player's error.
pub(crate) fn transport(app: &mut LightcraftApp, ui: &mut egui::Ui, canvas: Rect, photo: &lightcraft_catalog::Photo) {
    if !photo.is_video() {
        return;
    }
    let (id, catalog_duration) = (photo.id, photo.duration);
    let t = Tokens::get(ui.ctx());
    let width = (canvas.width() - 2.0 * BAR_INSET).clamp(1.0, 720.0);
    if canvas.height() < BAR_H + 2.0 * BAR_INSET || width < 200.0 {
        return;
    }
    let bar = Rect::from_center_size(pos2(canvas.center().x, canvas.bottom() - BAR_INSET - BAR_H / 2.0), vec2(width, BAR_H));
    let available = app.session.video.available();
    let player = app.session.video.player.as_ref().filter(|p| p.photo() == id);
    let playing = player.is_some_and(|p| p.playing());
    let duration = player.and_then(|p| p.info()).map(|i| i.duration).or(catalog_duration).filter(|d| d.is_finite() && *d > 0.0);
    let position = player.map_or(0.0, |p| p.position());
    let audio = player.and_then(|p| p.info()).and_then(|i| i.audio.clone());
    // a build without video decoding offers no Play and says why (the poster still shows)
    let error = if available {
        player.and_then(|p| p.error())
    } else {
        Some(crate::i18n::tr(lightcraft_engine::video_source::NO_VIDEO_DECODING).to_string())
    };
    let p = ui.painter_at(canvas);
    p.rect_filled(bar, 8.0, Color32::from_black_alpha(190));
    register(ui, "video:transport", bar);
    let mut child =
        ui.new_child(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(6.0, 2.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    child.spacing_mut().item_spacing.x = 2.0;
    let size = vec2(32.0, 32.0);
    let mut fire: Option<(&str, serde_json::Value)> = None;
    if icon_button(&mut child, "video:stepBack", Icon::ChevronLeft, size, false, available, "Previous frame").clicked() {
        fire = Some(("video.step", json!({"frames": -1})));
    }
    let mac = ui.ctx().os() == egui::os::OperatingSystem::Mac;
    let (icon, tip) = if playing {
        (Icon::Pause, play_tip(&app.ui.settings.keymap, mac, "Pause"))
    } else {
        (Icon::Play, play_tip(&app.ui.settings.keymap, mac, "Play"))
    };
    if icon_button(&mut child, "video:playPause", icon, size, false, available, &tip).clicked() {
        fire = Some(("video.playPause", json!({})));
    }
    if icon_button(&mut child, "video:stepForward", Icon::ChevronRight, size, false, available, "Next frame").clicked() {
        fire = Some(("video.step", json!({"frames": 1})));
    }
    // time, scrub bar, duration, audio note: what is left of the bar
    let time = format_duration(position);
    let total = duration.map_or_else(|| "--:--".to_string(), format_duration);
    let font = t.font(12.0);
    let label_w = 52.0;
    let note = audio.as_ref().map(|_| crate::i18n::tr("Audio not played"));
    let note_w = note.as_ref().map_or(0.0, |n| p.layout_no_wrap(n.to_string(), t.font(11.0), Color32::GRAY).size().x + 12.0);
    let rest = child.available_width();
    let scrub_w = (rest - 2.0 * label_w - note_w).max(40.0);
    let (lr, _) = child.allocate_exact_size(vec2(label_w, 32.0), Sense::hover());
    child.painter().text(lr.center(), Align2::CENTER_CENTER, time, font.clone(), Color32::from_gray(230));
    let (sr, resp) = child.allocate_exact_size(vec2(scrub_w, 32.0), if available { Sense::click_and_drag() } else { Sense::hover() });
    register(ui, "video:scrub", sr);
    let track = Rect::from_center_size(sr.center(), vec2(sr.width() - 8.0, 4.0));
    child.painter().rect_filled(track, 2.0, Color32::from_gray(90));
    if let Some(d) = duration {
        let f = (position / d).clamp(0.0, 1.0) as f32;
        let done = Rect::from_min_max(track.min, pos2(track.left() + track.width() * f, track.max.y));
        child.painter().rect_filled(done, 2.0, t.accent);
        child.painter().circle_filled(pos2(done.right(), track.center().y), 6.0, Color32::WHITE);
        if (resp.dragged() || resp.clicked())
            && let Some(at) = resp.interact_pointer_pos()
        {
            let f = ((at.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
            fire = Some(("video.seek", json!({"time": f64::from(f) * d})));
        }
    }
    resp.widget_info(|| egui::WidgetInfo::slider(true, position, crate::i18n::tr("Playhead")));
    let (rr, _) = child.allocate_exact_size(vec2(label_w, 32.0), Sense::hover());
    child.painter().text(rr.center(), Align2::CENTER_CENTER, total, font, Color32::from_gray(200));
    if let (Some(n), Some(a)) = (note, audio.as_ref()) {
        let (nr, nresp) = child.allocate_exact_size(vec2(note_w, 32.0), Sense::hover());
        child.painter().text(nr.center(), Align2::CENTER_CENTER, n, t.font(11.0), Color32::from_gray(150));
        register(ui, "video:audioNote", nr);
        nresp.on_hover_text(crate::i18n::tr_format!(
            "This clip's audio ({track}) is not played: LightCraft has no audio output yet",
            track = a.as_str()
        ));
    }
    // why the clip doesn't play, above the bar (also a build without video decoding, once asked)
    let problem = error.map(|e| crate::i18n::tr_format!("Can't play this clip: {reason}", reason = e.as_str()).to_string());
    if let Some(msg) = problem {
        let g = p.layout(msg, t.font(12.0), Color32::from_gray(235), width - 24.0);
        let r = Rect::from_center_size(pos2(bar.center().x, bar.top() - 8.0 - g.size().y / 2.0 - 6.0), g.size() + vec2(16.0, 12.0));
        p.rect_filled(r, 6.0, Color32::from_black_alpha(200));
        p.rect_stroke(r, 6.0, Stroke::new(1.0, t.accent.gamma_multiply(0.6)), StrokeKind::Inside);
        p.galley(r.min + vec2(8.0, 6.0), g, Color32::from_gray(235));
        register(ui, "video:problem", r);
    }
    if let Some((cmd, params)) = fire {
        if let Err(e) = app.run(cmd, params) {
            app.toast_error(ui.ctx(), e);
        }
        ui.ctx().request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_uv_turns_every_orientation_upright() {
        let tl = pos2(0.0, 0.0);
        // a stored picture turned a quarter clockwise for display: its bottom-left is top-left on screen
        assert_eq!(source_uv(Orientation::Rotate90, tl), pos2(0.0, 1.0));
        assert_eq!(source_uv(Orientation::Rotate180, tl), pos2(1.0, 1.0));
        assert_eq!(source_uv(Orientation::Rotate270, tl), pos2(1.0, 0.0));
        assert_eq!(source_uv(Orientation::Normal, tl), tl);
        assert_eq!(source_uv(Orientation::FlipH, tl), pos2(1.0, 0.0));
        // every orientation maps the four corners onto the four corners
        for exif in 1..=8 {
            let o = Orientation::from_exif(exif);
            let mut seen: Vec<(i32, i32)> = [pos2(0.0, 0.0), pos2(1.0, 0.0), pos2(1.0, 1.0), pos2(0.0, 1.0)]
                .into_iter()
                .map(|d| source_uv(o, d))
                .map(|s| (s.x.round() as i32, s.y.round() as i32))
                .collect();
            seen.sort_unstable();
            assert_eq!(seen, vec![(0, 0), (0, 1), (1, 0), (1, 1)], "{o:?}");
        }
    }

    #[test]
    fn the_play_tooltip_names_the_key_as_bound() {
        let mut keymap = crate::shortcuts::Keymap::new();
        assert_eq!(play_tip(&keymap, false, "Play"), "Play (Space)");
        // its own shortcut wins
        keymap.insert("video.playPause".into(), "K".into());
        assert_eq!(play_tip(&keymap, false, "Pause"), "Pause (K)");
        // Space given to another command: no key plays, so none is named
        keymap.clear();
        keymap.insert("photo.flag".into(), "Space".into());
        assert_eq!(play_tip(&keymap, false, "Play"), "Play");
    }

    #[test]
    fn frames_bigger_than_the_gpu_allows_are_never_uploaded() {
        assert!(uploadable([3840, 2160], 8192));
        assert!(uploadable([8192, 4320], 8192));
        assert!(!uploadable([8704, 4352], 8192));
        assert!(!uploadable([2160, 9000], 8192));
        // an unknown limit (0) blocks nothing but empty frames
        assert!(uploadable([16384, 8640], 0));
        assert!(!uploadable([0, 10], 0));
    }

    #[test]
    fn fit_keeps_the_upright_aspect() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 500.0));
        let r = fit(area, [1920, 1080], Orientation::Normal);
        assert!((r.width() / r.height() - 16.0 / 9.0).abs() < 1e-3);
        // a portrait phone clip stored landscape
        let r = fit(area, [1920, 1080], Orientation::Rotate90);
        assert!((r.height() - 500.0).abs() < 1e-3 && (r.width() / r.height() - 9.0 / 16.0).abs() < 1e-3);
        // degenerate sizes never divide by zero
        let r = fit(area, [0, 0], Orientation::Normal);
        assert!(r.width().is_finite() && r.height().is_finite());
    }
}
