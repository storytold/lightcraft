//! Settings ▸ Slideshow ▸ Titles (Lightroom Classic's Slideshow Titles panel): the intro and
//! ending screens, each on or off, with a background colour and, centred on it, the identity plate
//! of the Overlays section or a line of text, at a scale, for its own duration.
//!
//! Every control goes through `slideshow.titles` (validated there, saved with the library), like
//! the Layout and Overlays sections beside it. Changing a screen's settings switches the live
//! preview ([`super::slide_layout::preview`]) to that screen; its Photo / Intro / Ending buttons
//! pick what it shows ([`preview_shows`]).

use lightcraft_engine::slide_overlay::{PLATE_SIZE_MAX, SIZE_MIN};
use lightcraft_engine::slide_title::{TitleContent, TitleScreen};
use lightcraft_engine::slideshow::{SLIDE_MAX, SLIDE_MIN, Title};
use serde_json::{Value, json};

use super::settings::{check, choices, heading, hint, row};
use crate::LightcraftApp;
use crate::text_field::TextField;
use crate::theme::Tokens;
use crate::widgets::register;

/// Where the preview's choice lives (egui memory): `None` = the photo's slide.
fn preview_key() -> egui::Id {
    egui::Id::unique("settingsSlideshowPreviewShows")
}

/// What the Settings preview shows: a title screen, or (`None`) the active photo's slide.
pub fn preview_shows(ctx: &egui::Context) -> Option<Title> {
    ctx.data(|d| d.get_temp::<Option<Title>>(preview_key())).flatten()
}

pub fn set_preview_shows(ctx: &egui::Context, t: Option<Title>) {
    ctx.data_mut(|d| d.insert_temp(preview_key(), t));
}

/// The Photo / Intro / Ending buttons over the preview (`button:settingsSlideshowPreviewShows-{0,1,2}`).
pub fn preview_choice(ui: &mut egui::Ui) {
    let mut v = preview_shows(ui.ctx());
    let opts = [(None, "Photo"), (Some(Title::Intro), "Intro"), (Some(Title::Ending), "Ending")];
    ui.horizontal(|ui| {
        if choices(ui, "settingsSlideshowPreviewShows", &opts, &mut v) {
            set_preview_shows(ui.ctx(), v);
        }
    });
}

/// Change `key` of screen `t` (and show that screen in the preview).
fn set(app: &mut LightcraftApp, ctx: &egui::Context, t: Title, key: &str, v: Value) {
    let _ = app.run("slideshow.titles", json!({ t.id(): { key: v } }));
    set_preview_shows(ctx, Some(t));
}

/// A slider (`slider:{id}`) for `key` of screen `t`; `k` scales the value for display.
#[allow(clippy::too_many_arguments)]
fn slider(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    id: &str,
    t: Title,
    key: &str,
    value: f64,
    range: std::ops::RangeInclusive<f64>,
    k: f64,
    suffix: &str,
) {
    let mut v = value * k;
    let r = ui.add(egui::Slider::new(&mut v, (range.start() * k)..=(range.end() * k)).suffix(suffix).max_decimals(1));
    register(ui, format!("slider:{id}"), r.rect);
    let p = json!({ t.id(): { key: v / k } });
    if r.changed() {
        if let Ok(next) = app.session.slide_titles.merged(&p) {
            app.session.slide_titles = next;
        }
        set_preview_shows(ui.ctx(), Some(t));
    }
    if r.drag_stopped() || (r.changed() && !r.dragged()) {
        let _ = app.run("slideshow.titles", p);
    }
}

/// A colour button (`button:{id}`) for `key` of screen `t`.
fn colour(app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, t: Title, key: &str, value: [u8; 3]) {
    let mut c = value;
    let r = egui::color_picker::color_edit_button_srgb(ui, &mut c);
    register(ui, format!("button:{id}"), r.rect);
    if r.changed() {
        set(app, ui.ctx(), t, key, json!(c));
    }
}

/// The screen's text field (`field:{id}`), saved when the edit ends.
fn text(app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, t: Title, value: &str) {
    let mem = egui::Id::unique(("slideshow-title-field", id));
    let mut s: String = ui.data(|d| d.get_temp(mem)).unwrap_or_else(|| value.to_string());
    let widget = format!("field:{id}");
    let r = TextField::singleline(&widget, &mut s).hint(crate::i18n::tr("Title text")).width(ui.available_width().clamp(80.0, 260.0)).show(ui);
    if r.editing {
        ui.data_mut(|d| d.insert_temp(mem, s.clone()));
    } else {
        ui.data_mut(|d| d.remove::<String>(mem));
    }
    if r.committed() && s != value {
        set(app, ui.ctx(), t, "text", json!(s));
    }
}

/// One screen's controls; `name` prefixes the widget ids (`slideshowIntro…`).
fn screen(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, title: Title, s: &TitleScreen) {
    let (name, content_id, label) = match title {
        Title::Intro => ("slideshowIntro", "settingsSlideshowIntroContent", "Intro screen"),
        Title::Ending => ("slideshowEnding", "settingsSlideshowEndingContent", "Ending screen"),
    };
    let mut on = s.enabled;
    if check(ui, &format!("settings.{name}"), &mut on, label) {
        set(app, ui.ctx(), title, "enabled", json!(on));
    }
    if !on {
        return;
    }
    row(ui, t, "Colour", |ui| colour(app, ui, &format!("{name}Color"), title, "color", s.color));
    row(ui, t, "Shows", |ui| {
        let mut c = s.content;
        let opts = [(TitleContent::IdentityPlate, "Identity plate"), (TitleContent::Text, "Text"), (TitleContent::None, "Colour only")];
        if choices(ui, content_id, &opts, &mut c) {
            set(app, ui.ctx(), title, "content", json!(c.id()));
        }
    });
    if s.content == TitleContent::None {
        return;
    }
    if s.content == TitleContent::Text {
        row(ui, t, "Text", |ui| text(app, ui, &format!("{name}Text"), title, &s.text));
    }
    row(ui, t, "Scale", |ui| slider(app, ui, &format!("{name}Scale"), title, "scale", s.scale, SIZE_MIN..=PLATE_SIZE_MAX, 100.0, " %"));
    ui.horizontal(|ui| {
        let mut over = s.override_color;
        if check(ui, &format!("settings.{name}Override"), &mut over, "Override colour") {
            set(app, ui.ctx(), title, "overrideColor", json!(over));
        }
        if over {
            colour(app, ui, &format!("{name}TextColor"), title, "textColor", s.text_color);
        }
    });
    let mut own = s.duration.is_some();
    if check(ui, &format!("settings.{name}OwnDuration"), &mut own, "Own duration (off: the slide duration)") {
        let d = if own { json!(app.session.slideshow.slide_duration) } else { Value::Null };
        set(app, ui.ctx(), title, "duration", d);
    }
    if let Some(d) = s.duration {
        row(ui, t, "Duration", |ui| slider(app, ui, &format!("{name}Duration"), title, "duration", d, SLIDE_MIN..=SLIDE_MAX, 1.0, " s"));
    }
}

/// The Titles section.
pub fn sections(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let s = app.session.slide_titles.clone();
    heading(ui, t, "Titles");
    screen(app, ui, t, Title::Intro, &s.intro);
    screen(app, ui, t, Title::Ending, &s.ending);
    hint(
        ui,
        t,
        "The intro plays once before the first photo; the ending after the last photo when Repeat is off. The identity plate is the one set under Overlays. Titles fade like slides and random order never moves them.",
    );
    let warnings = s.warnings(&app.session.slide_overlays);
    for w in warnings {
        ui.label(egui::RichText::new(w).size(11.0).color(t.caution));
    }
}
