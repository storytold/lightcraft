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

use crate::LightcraftApp;
use crate::panels::settings::{check, choices, heading, hint, row};
use crate::theme::Tokens;

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
    label: &str,
    t: Title,
    key: &str,
    value: f64,
    range: std::ops::RangeInclusive<f64>,
    k: f64,
    suffix: &str,
) {
    use super::controls::{self, Shape, Step};
    let shape = Shape { label, lo: range.start() * k, hi: range.end() * k, unit: suffix.trim(), step: 0.1, log: false };
    for step in controls::slider(ui, controls::owner(id), id, &shape, value * k) {
        match step {
            Step::Begin(_) => {}
            Step::Preview(v) => {
                if let Ok(next) = app.session.slide_titles.merged(&json!({ t.id(): { key: v / k } })) {
                    app.session.slide_titles = next;
                }
                set_preview_shows(ui.ctx(), Some(t));
            }
            Step::Save(v) => {
                let _ = app.run("slideshow.titles", json!({ t.id(): { key: v / k } }));
            }
        }
    }
}

/// A colour well (`button:{id}`) for `key` of screen `t`.
fn colour(app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, label: &str, t: Title, key: &str, value: [u8; 3]) {
    use super::controls;
    if let Some(c) = controls::colour(ui, controls::owner(id), id, label, value) {
        set(app, ui.ctx(), t, key, json!(c));
    }
}

/// Only a presented, enabled title control can commit a library preference.
fn title_input(ui: &egui::Ui) -> bool {
    ui.is_enabled() && !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx())
}

#[derive(Clone)]
struct TitleDraft {
    text: String,
    model: String,
    pass: u64,
}

/// The screen's text field (`field:{id}`), saved when the edit ends.
fn text(app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, t: Title, value: &str) {
    use craft::ui::contract::{Bind, Props as _, Show as _};

    let mem = egui::Id::unique(("slideshow-title-field", id));
    let pass = ui.ctx().cumulative_pass_nr();
    // A hidden/rebound field starts from its own authoritative screen, never a stale draft.
    let remembered = ui
        .data(|d| d.get_temp::<TitleDraft>(mem))
        .filter(|d| d.model == value && (d.pass == pass || d.pass.checked_add(1) == Some(pass)));
    let mut draft = remembered.map_or_else(|| value.to_owned(), |d| d.text);
    let widget = format!("field:{id}");
    let width = ui.available_width().clamp(80.0, 260.0);
    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    let committed = {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 24.0), egui::Sense::hover());
        let out = craft::ui::text::TextField::new(crate::i18n::tr("Title text"), Bind::bound(&mut draft))
            .id(crate::text_field::id(&widget))
            .placeholder(crate::i18n::tr("Title text"))
            .limit(None) // Keep engine-side text truncation at commit, not while typing.
            .submits()
            .show_at(ui, rect);
        if real {
            crate::widgets::register_response(widget, out.response.rect, &out.response);
            if out.escaped {
                crate::widgets::take_escape(ui.ctx());
            }
            if out.events.ended().is_some() || !ui.is_enabled() {
                ui.data_mut(|d| d.remove::<TitleDraft>(mem));
            } else {
                ui.data_mut(|d| d.insert_temp(mem, TitleDraft { text: draft, model: value.to_owned(), pass }));
            }
        }
        out.events.committed().cloned()
    };
    if title_input(ui)
        && let Some(next) = committed.filter(|next| next != value)
    {
        set(app, ui.ctx(), t, "text", json!(next));
    }
}

/// Enable is independent for each title screen; disabled controls never commit.
fn screen_enable(app: &mut LightcraftApp, ui: &mut egui::Ui, title: Title, s: &TitleScreen, name: &str, label: &str) -> bool {
    let mut on = s.enabled;
    if check(ui, &format!("settings.{name}"), &mut on, label) && title_input(ui) {
        set(app, ui.ctx(), title, "enabled", json!(on));
    } else {
        on = s.enabled;
    }
    if !on && title_input(ui) {
        ui.data_mut(|d| d.remove::<TitleDraft>(egui::Id::unique(("slideshow-title-field", format!("{name}Text")))));
    }
    on
}

/// Content and its optional editor are one workflow. Colour/scale/duration stay with `screen`.
fn screen_content(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, title: Title, s: &TitleScreen, name: &str, content_id: &str) {
    row(ui, t, "Shows", |ui| {
        let mut c = s.content;
        let opts = [(TitleContent::IdentityPlate, "Identity plate"), (TitleContent::Text, "Text"), (TitleContent::None, "Colour only")];
        if choices(ui, content_id, &opts, &mut c) && title_input(ui) {
            set(app, ui.ctx(), title, "content", json!(c.id()));
        }
    });
    if s.content == TitleContent::Text {
        row(ui, t, "Text", |ui| text(app, ui, &format!("{name}Text"), title, &s.text));
    } else if title_input(ui) {
        ui.data_mut(|d| d.remove::<TitleDraft>(egui::Id::unique(("slideshow-title-field", format!("{name}Text")))));
    }
}

/// One screen's controls; `name` prefixes the widget ids (`slideshowIntro…`).
fn screen(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, title: Title, s: &TitleScreen) {
    let (name, content_id, label) = match title {
        Title::Intro => ("slideshowIntro", "settingsSlideshowIntroContent", "Intro screen"),
        Title::Ending => ("slideshowEnding", "settingsSlideshowEndingContent", "Ending screen"),
    };
    if !screen_enable(app, ui, title, s, name, label) {
        return;
    }
    row(ui, t, "Colour", |ui| colour(app, ui, &format!("{name}Color"), "Colour", title, "color", s.color));
    screen_content(app, ui, t, title, s, name, content_id);
    if s.content == TitleContent::None {
        return;
    }
    row(ui, t, "Scale", |ui| slider(app, ui, &format!("{name}Scale"), "Scale", title, "scale", s.scale, SIZE_MIN..=PLATE_SIZE_MAX, 100.0, "%"));
    ui.horizontal(|ui| {
        let mut over = s.override_color;
        if check(ui, &format!("settings.{name}Override"), &mut over, "Override colour") && title_input(ui) {
            set(app, ui.ctx(), title, "overrideColor", json!(over));
        }
        if over {
            colour(app, ui, &format!("{name}TextColor"), "Text colour", title, "textColor", s.text_color);
        }
    });
    let mut own = s.duration.is_some();
    if check(ui, &format!("settings.{name}OwnDuration"), &mut own, "Own duration (off: the slide duration)") && title_input(ui) {
        let d = if own { json!(app.session.slideshow.slide_duration) } else { Value::Null };
        set(app, ui.ctx(), title, "duration", d);
    }
    if let Some(d) = s.duration {
        row(ui, t, "Duration", |ui| slider(app, ui, &format!("{name}Duration"), "Duration", title, "duration", d, SLIDE_MIN..=SLIDE_MAX, 1.0, "s"));
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

#[cfg(test)]
#[path = "slide_title_screen_flow_tests.rs"]
mod slide_title_screen_flow_tests;
