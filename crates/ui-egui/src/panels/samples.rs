//! The banner under the top bar while the library holds the sample photos: it says what they
//! are, and offers the next step. With only samples: Import Photos… or Remove Samples. Once photos
//! of the person's own are in: Remove Samples or Keep Them. Closed for good with Hide / Keep Them
//! (until samples are added again). After landing on a sample it walks the first edit through:
//! drag a slider, then compare with the original.

use lightcraft_catalog::Source;
use serde_json::json;

use crate::LightcraftApp;
use crate::theme::Tokens;
use crate::widgets::register;

#[derive(Clone, Copy)]
enum Click {
    Import,
    Remove,
    Hide,
}

/// The library holds photos of the person's own beside the samples. It stops at the first one, and
/// is asked once a frame, only while the banner is up (the library then holds a sample).
fn own_photos(app: &LightcraftApp) -> bool {
    app.session.catalog.photos().any(|p| p.in_library() && !matches!(p.source, Source::Demo { .. }))
}

/// Whether the banner is up: not hidden, a sample in the library (all in Recently Deleted: the
/// grid says the library is empty), and not an in-memory demo session (a demo by design).
fn shown(app: &LightcraftApp) -> bool {
    !app.ui.samples_banner_hidden && app.session.catalog.sample_photos_in_library() > 0 && app.session.library.is_some()
}

/// The first edit on a sample, from landing on one ([`crate::panels::empty_library::land_on_samples`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tour {
    /// The sample open has its own settings: the banner invites dragging a slider.
    #[default]
    Invited,
    /// It is edited: the banner says so, and how to see it beside the original.
    Edited,
    /// Comparing it with the original: the banner says what is on screen.
    Comparing,
    /// Compared and back: the usual message from here on.
    Done,
}

/// The settings of the photo open as it became the open one, to tell what the person has edited
/// from what it came with (a few samples come edited): its library generation, its id, its settings.
pub(crate) type TourBase = (u64, lightcraft_catalog::PhotoId, std::sync::Arc<lightcraft_develop::DevelopSettings>);

/// Keep the settings the photo open came with: taken when it becomes the open one.
fn note_base(app: &mut LightcraftApp) {
    let generation = app.session.library_generation();
    let active = app.session.active();
    let kept = matches!((&app.samples_tour_base, active), (Some((g, id, _)), Some(a)) if *g == generation && *id == a);
    if !kept {
        app.samples_tour_base = active.and_then(|id| app.session.develop_of(id).map(|d| (generation, id, d)));
    }
}

/// Whether the photo open has been edited since it became the open one (before that is known,
/// whether it differs from the default settings).
fn edited(app: &LightcraftApp) -> bool {
    let Some(id) = app.session.active() else { return false };
    let Some(now) = app.session.develop_of(id) else { return false };
    match &app.samples_tour_base {
        Some((generation, base_id, base)) if *generation == app.session.library_generation() && *base_id == id => *now != **base,
        _ => *now != lightcraft_develop::DevelopSettings::default(),
    }
}

/// Where the first edit stands now, from the sample open (edited or not: a rating or a keyword
/// isn't an edit, and another sample or an undo can take the edit away again) and whether it is
/// compared with the original. Comparing once ends it. `None` before samples landed here.
fn tour(app: &LightcraftApp) -> Option<Tour> {
    if app.samples_landed != Some(app.session.library_generation()) {
        return None;
    }
    // the walk is over: nothing to compare, every frame
    if app.samples_tour == Tour::Done {
        return Some(Tour::Done);
    }
    let edited = edited(app);
    // a Before / After layout: Show Original alone shows nothing beside the edit
    let comparing = app.ui.before_after.is_layout();
    Some(match app.samples_tour {
        Tour::Done => Tour::Done,
        Tour::Comparing if comparing => Tour::Comparing,
        Tour::Comparing => Tour::Done,
        _ if edited && comparing => Tour::Comparing,
        _ if edited => Tour::Edited,
        _ => Tour::Invited,
    })
}

/// What the banner says: `own` is whether photos of the person's own are in beside the samples.
/// On a sample open in Detail after landing there, it walks the first edit through: an
/// invitation to drag a slider, then (once one is dragged) how to compare with the original,
/// with the key Before/After Side by Side is bound to, then what the comparison shows.
fn words(app: &LightcraftApp, own: bool, mac: bool) -> String {
    use crate::i18n::{tr, tr_format};
    if own {
        return tr("Your photos are in. The sample photos are still here too.").to_string();
    }
    let usual = || tr("These are sample photos, to try LightCraft on. Import your own whenever you're ready.").to_string();
    let on_a_sample = app.ui.view == crate::state::ViewMode::Detail
        && app.session.active().and_then(|id| app.session.catalog.photo(id)).is_some_and(|p| matches!(p.source, Source::Demo { .. }));
    let key = crate::shortcuts::shortcut_of(&app.ui.settings.keymap, "view.beforeAfter").map(|sc| crate::menubar::shortcut_text(sc, mac));
    match tour(app).filter(|_| on_a_sample) {
        Some(Tour::Invited) => {
            let first = tr("Drag any slider: the original never changes.");
            match key {
                Some(key) => format!("{first} {}", tr_format!("Press {key} to compare before and after.", key = key)),
                None => first.to_string(),
            }
        }
        Some(Tour::Edited) => {
            let how = match key {
                Some(key) => tr_format!("Press {key} to see it beside the original.", key = key),
                None => tr("View → Before/After Side by Side shows it beside the original.").to_string(),
            };
            format!("{} {how}", tr("That's your first edit."))
        }
        Some(Tour::Comparing) => tr("The original beside your edit: the file itself never changes.").to_string(),
        Some(Tour::Done) | None => usual(),
    }
}

/// What the banner says now, or `None` while it isn't shown.
pub fn message(app: &LightcraftApp, mac: bool) -> Option<String> {
    shown(app).then(|| words(app, own_photos(app), mac))
}

/// The banner's height with one line of words, and with two.
const BANNER_H: f32 = 34.0;
const BANNER_TALL: f32 = 50.0;

/// The banner's band: the accent, faintly, over the chrome, so it reads as a note apart from the
/// top bar above it (its words stay at AA on it in every theme).
pub(crate) fn fill(t: &Tokens) -> egui::Color32 {
    toward_accent(t, 0.2)
}

/// The line under the band.
pub(crate) fn edge(t: &Tokens) -> egui::Color32 {
    t.accent
}

/// The chrome taken `share` of the way to the accent.
fn toward_accent(t: &Tokens, share: f32) -> egui::Color32 {
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * share).round().clamp(0.0, 255.0) as u8;
    egui::Color32::from_rgb(mix(t.chrome.r(), t.accent.r()), mix(t.chrome.g(), t.accent.g()), mix(t.chrome.b(), t.accent.b()))
}

pub fn banner(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    // cached by the photo set, not by every edit (see `Session::has_samples`)
    let present = app.session.has_samples();
    // samples that newly appear bring the banner back, however they were added
    if present && app.samples_present == Some(false) {
        app.ui.samples_banner_hidden = false;
    }
    app.samples_present = Some(present);
    if !shown(app) {
        return;
    }
    note_base(app);
    if let Some(now) = tour(app) {
        app.samples_tour = now;
    }
    let own = own_photos(app);
    let text = words(app, own, ui.ctx().os() == egui::os::OperatingSystem::Mac);
    let t = Tokens::get(ui.ctx());
    use crate::button::{Button, Kind};
    // right to left: Hide (or Keep Them) at the edge
    let (hide, hide_widget) = if own { ("Keep Them", "button:samplesKeep") } else { ("Hide", "button:samplesHide") };
    let mut buttons =
        vec![(hide, hide_widget, Kind::Ghost, Click::Hide), ("Remove Sample Photos", "button:samplesRemove", Kind::Secondary, Click::Remove)];
    if !own {
        buttons.push(("Import Photos…", "button:samplesImport", Kind::Primary, Click::Import));
    }
    let button = |(label, widget, kind, _): &(&'static str, &'static str, Kind, Click)| Button::new(label).kind(*kind).small().widget(*widget);
    // the room the words have beside the buttons: a long message (or language) takes a second
    // line before it is shortened
    let font = t.font(13.0);
    let gap = ui.spacing().item_spacing.x;
    let buttons_w: f32 = buttons.iter().map(|b| button(b).desired_size(ui).x + gap).sum();
    let room = (ui.available_width() - 24.0 - buttons_w - 8.0).max(80.0);
    let lines = ui.painter().layout(text.clone(), font.clone(), t.text, room).rows.len();
    let height = if lines > 1 { BANNER_TALL } else { BANNER_H };
    let mut click = None;
    let resp = egui::Panel::top("samples-banner")
        .exact_size(height)
        .frame(egui::Frame::NONE.fill(fill(&t)).inner_margin(egui::Margin::symmetric(12, 0)))
        .show(ui, |ui| {
            // the buttons first, from the right edge, so a long translation wraps or shortens the
            // message instead of pushing them out of the window
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                for b in &buttons {
                    let resp = button(b).show(ui);
                    // (a focused one keeps only the keys that press it, as the welcome's do)
                    crate::widgets::plain_button(ui.ctx(), resp.id);
                    if resp.clicked() {
                        click = Some(b.3);
                    }
                }
                ui.add_space(8.0);
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let mut job = egui::text::LayoutJob::simple(text.clone(), font.clone(), t.text, ui.available_width());
                    job.wrap.max_rows = 2;
                    job.wrap.overflow_character = Some('…');
                    // (shortened past two lines, egui shows the whole on hover)
                    let words = ui.add(egui::Label::new(job).wrap());
                    // read out when it appears or changes (each step of the first edit), without
                    // taking the screen reader from what it was reading
                    ui.ctx().accesskit_node_builder(words.id, |node| node.set_live(egui::accesskit::Live::Polite));
                });
            });
        });
    register(ui, "banner:samples", resp.response.rect);
    // its lower edge in the accent, so the band reads as a note apart from the window
    let r = resp.response.rect;
    ui.painter().hline(r.x_range(), r.bottom() - 0.5, egui::Stroke::new(1.0, edge(&t)));
    match click {
        Some(Click::Import) => {
            if let Err(e) = app.run("file.addPhotos", json!({})) {
                app.toast_error(ui.ctx(), e);
            }
        }
        Some(Click::Remove) => match app.run("library.removeSamples", json!({})) {
            Ok(r) => {
                let n = r["removed"].as_u64().unwrap_or(0);
                let text = crate::i18n::tr_format!("Removed {n} sample photos. Edit → Undo brings them back.", n = n);
                app.toast(ui.ctx(), text);
            }
            Err(e) => app.toast_error(ui.ctx(), e),
        },
        Some(Click::Hide) => app.ui.samples_banner_hidden = true,
        None => {}
    }
}
