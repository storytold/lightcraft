//! A library with no photos yet: what the grid and the Detail view show instead of a dead end,
//! with Import Photos… and Import Lightroom Catalog… as buttons and a row of sample photos to try
//! LightCraft on, and the view it opens on. A library just created asks this as its welcome: it
//! starts empty. While it is empty the welcome has the window: the Tools panel steps aside until
//! photos arrive.

use egui::{Rect, Sense, pos2, vec2};
use serde_json::json;

use super::EmptyAction;
use crate::LightcraftApp;
use crate::shortcuts::Keymap;
use crate::state::{RightPanel, ViewMode};
use crate::theme::Tokens;

/// The library holds no photos: none, or only ones in Recently Deleted or browsed under Local.
pub fn is_empty(session: &lightcraft_engine::Session) -> bool {
    !session.catalog.photos().any(|p| p.in_library())
}

/// What is shown is the empty library: not a folder browsed under Local nor Recently Deleted,
/// whose photos aren't in the library but are on screen.
pub fn shown(session: &lightcraft_engine::Session) -> bool {
    use lightcraft_engine::LibrarySource;
    !matches!(session.source, LibrarySource::Folder | LibrarySource::RecentlyDeleted) && is_empty(session)
}

/// The welcome is what the view shows: the empty library in a grid with no filter hiding
/// anything (else the grid says the filter does), or in Detail with no photo open. The window around it steps back then: the grid's photo
/// count, the bottom bar (view modes and thumbnail size, with nothing to look at yet) and
/// Detail's empty filmstrip aren't drawn, and the sidebar's empty sections fold into one line.
pub fn in_view(app: &LightcraftApp) -> bool {
    let view = match app.ui.view {
        ViewMode::PhotoGrid | ViewMode::SquareGrid => app.session.filter == Default::default(),
        ViewMode::Detail => app.session.active().is_none(),
        _ => false,
    };
    view && shown(&app.session)
}

/// At launch, an empty library opens on a grid with the sidebar, where the way in is (Detail had
/// only "Choose a photo in the grid" to say). Square Grid stays as it is; a library with photos
/// opens as left. The desktop host calls this before it notes what its settings file holds, so
/// the switch isn't saved for its own sake.
pub fn open_on_empty_library(app: &mut LightcraftApp) {
    if is_empty(&app.session) {
        if app.ui.view != ViewMode::SquareGrid {
            app.ui.view = ViewMode::PhotoGrid;
        }
        app.ui.left_panel = true;
    }
}

/// The lines under the buttons: dropping works anywhere in the window, with the shortcut Import
/// Photos… is bound to now (written as menus write it); and, on the desktop while Local is in
/// sight (`local`), that a folder can be looked through in place there without importing it (the
/// way in for a library too large to import at once).
pub fn hint(keymap: &Keymap, mac: bool, local: bool) -> String {
    // the browser build reads dropped files, not folders, and has no Local
    let drop = if cfg!(target_arch = "wasm32") {
        crate::i18n::tr("or drop photos into this window")
    } else {
        crate::i18n::tr("or drop photos and folders into this window")
    };
    let first = match crate::shortcuts::shortcut_of(keymap, "file.addPhotos") {
        Some(sc) => format!("{drop} · {}", crate::menubar::shortcut_text(sc, mac)),
        None => drop.to_string(),
    };
    if cfg!(target_arch = "wasm32") || !local {
        first
    } else {
        format!("{first}\n{}", crate::i18n::tr("or look through a folder without importing it, under Local in the sidebar"))
    }
}

/// "Welcome to LightCraft" for a library just created (it never held a photo), else "Your
/// library is empty". A temporary session (the library couldn't be opened) is never welcomed.
pub fn title(session: &lightcraft_engine::Session, temporary: bool) -> &'static str {
    if welcomes(session, temporary) { "Welcome to LightCraft" } else { "Your library is empty" }
}

fn welcomes(session: &lightcraft_engine::Session, temporary: bool) -> bool {
    !temporary && session.catalog.never_held_photos()
}

const SAMPLES: EmptyAction = EmptyAction { command: "library.addSamples", label: "Try Sample Photos", widget: "button:emptySamples" };
const IMPORT_PHOTOS: EmptyAction = EmptyAction { command: "file.addPhotos", label: "Import Photos…", widget: "button:emptyImportPhotos" };
const IMPORT_LIGHTROOM: EmptyAction =
    EmptyAction { command: "file.importLightroom", label: "Import Lightroom Catalog…", widget: "button:emptyImportLightroom" };

/// Each frame: when the welcome comes on screen (the library becomes or opens empty) the Tools
/// panel steps aside, so the welcome is the one thing asking for attention, and the panel it was
/// is remembered (in `ui.json` too, so quitting while empty doesn't lose it). Once the welcome
/// goes (photos arrive, or a folder under Local or Recently Deleted is shown, whose photos can be
/// edited) it comes back as it was: an editing tool (Crop, Masking…) as the Edit panel, as the
/// photo it was used on is gone. Samples that land from the welcome open the Edit panel instead
/// ([`land_on_samples`]). Only the welcome coming puts it aside: a panel the person opens
/// meanwhile is theirs, and so is closing it again.
pub fn follow(app: &mut LightcraftApp) {
    // the welcome wasn't drawn last frame (another view, a filter's message, photos in): its
    // sample thumbnails go, and their worker stops at its next one
    if !std::mem::take(&mut app.welcome_drawn) {
        app.welcome_samples = None;
    }
    let empty = shown(&app.session);
    if empty {
        if app.welcome_seen != Some(true) {
            if app.ui.right != RightPanel::None {
                app.ui.right_before_welcome = Some(app.ui.right);
                app.ui.right = RightPanel::None;
            }
        } else if app.ui.right != RightPanel::None {
            // opened by the person while the welcome shows: theirs from now on
            app.ui.right_before_welcome = None;
        }
    } else if let Some(right) = app.ui.right_before_welcome.take()
        && app.ui.right == RightPanel::None
    {
        app.ui.right = if right.is_edit_tool() { RightPanel::Edit } else { right };
    }
    app.welcome_seen = Some(empty);
}

/// The welcome's sample photos: their scenes, and their thumbnails as they arrive from the worker
/// that draws them (they take a moment each, never on the UI thread).
pub struct SampleStrip {
    tiles: Vec<Tile>,
    /// How many sample photos come in (not just the ones shown).
    count: usize,
    rx: Option<std::sync::mpsc::Receiver<(u32, lightcraft_raster::Rgba8)>>,
}

struct Tile {
    scene: u32,
    title: String,
    texture: Option<egui::TextureHandle>,
}

/// The long side the thumbnails are drawn at: sharp at 2× on the widest tile.
const THUMB_EDGE: usize = 320;
const TILE_GAP: f32 = 10.0;
const TILE_MIN_W: f32 = 96.0;
const TILE_MAX_W: f32 = 150.0;
const CAPTION_H: f32 = 26.0;

impl SampleStrip {
    /// Start drawing the thumbnails (desktop: a worker thread; the browser has none to spare, and
    /// keeps the Try Sample Photos button instead).
    fn start() -> SampleStrip {
        let scenes = lightcraft_engine::demo::welcome_scenes();
        let tiles = scenes.iter().map(|(scene, title)| Tile { scene: *scene, title: title.clone(), texture: None }).collect();
        let (tx, rx) = std::sync::mpsc::channel();
        let ids: Vec<u32> = scenes.iter().map(|(id, _)| *id).collect();
        let spawned = std::thread::Builder::new().name("welcome-samples".into()).spawn(move || {
            for id in ids {
                let Some(img) = lightcraft_engine::demo::scene_preview(id, THUMB_EDGE) else { continue };
                // the welcome went away (photos came in): nobody to draw them for
                if tx.send((id, img)).is_err() {
                    return;
                }
            }
        });
        if let Err(e) = spawned {
            log::warn!("welcome sample thumbnails: {e}");
        }
        SampleStrip { tiles, count: lightcraft_engine::demo::sample_count(), rx: Some(rx) }
    }

    /// The worker is still drawing thumbnails.
    pub(crate) fn drawing(&self) -> bool {
        self.rx.is_some()
    }

    /// At least one thumbnail has arrived (tests wait for it).
    #[cfg(test)]
    pub(crate) fn has_thumbnail(&self) -> bool {
        self.tiles.iter().any(|t| t.texture.is_some())
    }

    /// Thumbnails that arrived become textures; the frame after the last, the worker is done.
    fn receive(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.rx else { return };
        loop {
            match rx.try_recv() {
                Ok((scene, img)) => {
                    let bytes = img.as_bytes();
                    // egui insists the pixels fill the size: skip one that doesn't (never expected)
                    if bytes.len() != img.width.saturating_mul(img.height).saturating_mul(4) {
                        continue;
                    }
                    let image = egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &bytes);
                    if let Some(tile) = self.tiles.iter_mut().find(|t| t.scene == scene) {
                        tile.texture = Some(ctx.load_texture(format!("welcome-sample-{scene}"), image, egui::TextureOptions::LINEAR));
                    }
                }
                // still drawing (about a tenth of a second for all five): keep frames coming until
                // the last arrives, so none shows as a placeholder longer than it must (and a
                // screenshot that waits for the window to settle waits for them)
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint();
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.rx = None;
                    return;
                }
            }
        }
    }
}

/// How many thumbnails fit in `width` (five, down to three) and how wide each is; `None` when not
/// three: the column is too narrow, and a button offers the samples instead.
pub(crate) fn strip_layout(width: f32) -> Option<(usize, f32)> {
    (3..=5).rev().find(|n| *n as f32 * TILE_MIN_W + (*n as f32 - 1.0) * TILE_GAP <= width).map(|n| {
        let w = ((width - (n as f32 - 1.0) * TILE_GAP) / n as f32).min(TILE_MAX_W);
        (n, w)
    })
}

/// The sample thumbnails' height in a column `width` wide (0 when they don't fit).
fn strip_height(width: f32) -> f32 {
    strip_layout(width).map_or(0.0, |(_, w)| CAPTION_H + w * 2.0 / 3.0)
}

/// Samples just came into an empty library (the welcome's button or thumbnails, Help ▸ Add
/// Sample Photos, an agent): open the first of the welcome's in Detail, ready to edit, so every
/// way in lands in the same place; and note the library it happened in, so the samples banner
/// invites a first change there.
pub fn land_on_samples(app: &mut LightcraftApp) {
    let first = lightcraft_engine::demo::welcome_scenes().first().map(|(id, _)| *id);
    if let Some(scene) = first {
        select_sample(app, scene);
    }
    // the sliders beside it: the banner invites dragging one
    app.ui.right = RightPanel::Edit;
    app.samples_landed = Some(app.session.library_generation());
    app.samples_tour = super::samples::Tour::Invited;
}

/// Select the sample photo of `scene` and show it in Detail.
fn select_sample(app: &mut LightcraftApp, scene: u32) {
    let photo = app.session.catalog.photos().find(|p| matches!(p.source, lightcraft_catalog::Source::Demo { scene: s } if s == scene)).map(|p| p.id);
    if let Some(id) = photo {
        app.session.selection = lightcraft_engine::Selection::single(id);
    }
    app.ui.view = ViewMode::Detail;
}

/// Add the samples and open the one clicked in Detail, ready to edit (with the Edit panel).
fn open_sample(app: &mut LightcraftApp, ctx: &egui::Context, scene: u32) {
    if let Err(e) = app.run("library.addSamples", json!({})) {
        app.toast_error(ctx, e);
        return;
    }
    // (adding them landed on the first: this one was clicked)
    select_sample(app, scene);
}

/// The row of sample thumbnails in `area`, under a caption. Returns the scene clicked.
fn strip(strip: &mut SampleStrip, ui: &mut egui::Ui, area: Rect) -> Option<u32> {
    let t = Tokens::get(ui.ctx());
    let (n, w) = strip_layout(area.width())?;
    let n = n.min(strip.tiles.len());
    if n == 0 {
        return None;
    }
    strip.receive(ui.ctx());
    let p = ui.painter().clone();
    // (all of them come: the caption says how many)
    let caption = crate::i18n::tr_format!("Or explore LightCraft with {n} sample photos", n = strip.count);
    let caption_rect = p.text(pos2(area.center().x, area.top() + 8.0), egui::Align2::CENTER_CENTER, &caption, t.font(12.0), t.text_label);
    super::readable(ui, caption_rect, "welcome-caption", &caption, false);
    let h = w * 2.0 / 3.0;
    let row_w = n as f32 * w + (n as f32 - 1.0) * TILE_GAP;
    let mut x = area.center().x - row_w / 2.0;
    let mut clicked = None;
    for tile in strip.tiles.iter().take(n) {
        let r = Rect::from_min_size(pos2(x, area.top() + CAPTION_H), vec2(w, h));
        x += w + TILE_GAP;
        let resp = ui.interact(r, ui.scope_id().with(("welcome-sample", tile.scene)), Sense::click());
        let label = crate::i18n::tr_format!("Open the sample photo “{title}”", title = tile.title);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label.clone()));
        crate::widgets::register(ui, format!("button:sample:{}", tile.scene), r);
        crate::widgets::plain_button(ui.ctx(), resp.id);
        match &tile.texture {
            Some(tex) => {
                egui::Image::new((tex.id(), r.size())).corner_radius(6.0).paint_at(ui, r);
            }
            // drawing: a quiet placeholder of the same size, so nothing jumps when it arrives
            None => {
                p.rect_filled(r, 6.0, t.inset);
            }
        }
        if resp.hovered() || resp.has_focus() {
            let width = if resp.has_focus() { 2.0 } else { 1.5 };
            p.rect_stroke(r, 6.0, egui::Stroke::new(width, t.accent), egui::StrokeKind::Outside);
        }
        let resp = resp.on_hover_text(&tile.title).on_hover_cursor(egui::CursorIcon::PointingHand);
        if resp.clicked() {
            clicked = Some(tile.scene);
        }
    }
    clicked
}

/// The empty library's message and buttons, centred in `rect`: Import Photos…, Import Lightroom
/// Catalog… (not in the browser), and the sample photos as a row of thumbnails (as a button where
/// the row doesn't fit, or in the browser). A click runs the command as its menu item does; a
/// refusal shows as a toast.
pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let temporary = app.library_problem.is_some();
    // samples in Recently Deleted are still in the library: adding them again would be refused;
    // nothing done in a temporary session is saved: samples there would only be lost
    let samples = !app.session.has_samples() && !temporary;
    let wrap = (rect.width() - 48.0).max(80.0);
    let mac = ui.ctx().os() == egui::os::OperatingSystem::Mac;
    let local = app.ui.left_panel && app.ui.sidebar.shown().any(|s| s == crate::sidebar::SidebarSection::Local);
    let hint = hint(&app.ui.settings.keymap, mac, local);
    let title = title(&app.session, temporary);
    let body = if welcomes(&app.session, temporary) {
        "Start with photos of your own, or look around with a set of sample photos first. Nothing is copied or moved unless you choose to."
    } else {
        "Photos you import show up here. Nothing is copied or moved unless you choose to."
    };
    // the row needs room both ways: a short view (Detail above its filmstrip, under the samples
    // banner), or a long language wrapping the message, offers the button instead of pushing the
    // welcome past its edges. Measured with the two buttons the row goes with.
    let message = super::empty_state_height(ui, rect, title, body, &[IMPORT_PHOTOS, IMPORT_LIGHTROOM], Some(&hint));
    let room = rect.height() >= message + super::EMPTY_EXTRA_GAP + strip_height(wrap) + 16.0;
    let row = samples && !cfg!(target_arch = "wasm32") && strip_layout(wrap).is_some() && room;
    app.welcome_drawn = true;
    if !row && let Some(samples_row) = app.welcome_samples.as_mut() {
        // the row is out of room for now (a resize): keep what was drawn for when it's back, and
        // take what the worker sends meanwhile, so it finishes rather than waits
        samples_row.receive(ui.ctx());
    }
    // the browser build can't read a Lightroom catalog from disk: only offer what works there
    let actions: &[EmptyAction] = match (cfg!(target_arch = "wasm32"), samples && !row) {
        (true, true) => &[IMPORT_PHOTOS, SAMPLES],
        (true, false) => &[IMPORT_PHOTOS],
        (false, true) => &[IMPORT_PHOTOS, IMPORT_LIGHTROOM, SAMPLES],
        (false, false) => &[IMPORT_PHOTOS, IMPORT_LIGHTROOM],
    };
    let extra = if row { strip_height(wrap) } else { 0.0 };
    // a new library's welcome puts the keyboard on Import Photos… once, so Enter or Space starts;
    // not while a notice, the quit prompt or a dialog is over it, and the one try is kept for when
    // they are gone
    let covered = !app.notices.is_empty() || app.quit_prompt.is_some() || app.ui.dialog.is_some();
    let focus = welcomes(&app.session, temporary) && !covered && !std::mem::replace(&mut app.welcome_focused, true);
    let (clicked, below) = super::empty_state(ui, rect, title, body, actions, Some(&hint), super::EmptyMore { extra, focus_primary: focus });
    // An ordinary dialog doesn't own the input as a prompt does (a screen reader's Click request still
    // reaches the button, and the keyboard stays on it), so Enter, Space or a click would press it
    // behind the dialog: nothing the welcome offers happens while one is up.
    // (A notice, the quit prompt or the launch list owns the input already: the background gets
    // neither keys nor the pointer and loses its focus.)
    let dialog_open = app.ui.dialog.is_some();
    let clicked = clicked.filter(|_| !dialog_open);
    if let Some(command) = clicked
        && let Err(e) = app.run(command, json!({}))
    {
        app.toast_error(ui.ctx(), e);
    }
    if row {
        let mut samples_row = app.welcome_samples.take().unwrap_or_else(SampleStrip::start);
        let scene = strip(&mut samples_row, ui, below).filter(|_| !dialog_open);
        app.welcome_samples = Some(samples_row);
        if let Some(scene) = scene {
            app.welcome_samples = None;
            open_sample(app, ui.ctx(), scene);
        }
    }
}
