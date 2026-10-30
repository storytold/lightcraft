use super::display::duration_slider;
use super::widgets::{check, choices, heading, hint, row};
use super::{LightcraftApp, Tokens, Value, json, register};

/// The Slideshow tab: the slide preview, then the settings, in one column the width of the page
/// (the page scrolls; a column beside the preview would be wider than the window). Without
/// `with_preview` the preview is left out: [`window`] has pinned it above the page.
pub(super) fn slideshow_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, with_preview: bool) {
    if with_preview {
        crate::panels::slide_layout::preview(app, ui, t, f32::INFINITY);
        ui.add_space(6.0);
    }
    crate::panels::slide_templates::section(app, ui, t);
    ui.add_space(6.0);
    playback_section(app, ui, t);
    ui.add_space(6.0);
    crate::panels::slide_layout::sections(app, ui, t);
    ui.add_space(6.0);
    crate::panels::slide_overlays::sections(app, ui, t);
    ui.add_space(6.0);
    crate::panels::slide_titles::sections(app, ui, t);
    ui.add_space(6.0);
    export_section(app, ui, t);
}

/// Export: PDF, JPEG sequence or video of the slideshow (View ▸ Export Slideshow…).
fn export_section(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    heading(ui, t, crate::i18n::tr("Export"));
    let running = app.session.slideshow_export.as_ref().is_some_and(|r| r.running());
    ui.horizontal_wrapped(|ui| {
        for (id, label, format) in [
            ("slideshowExportPdf", "Export PDF…", "pdf"),
            ("slideshowExportJpeg", "Export JPEG…", "jpeg"),
            ("slideshowExportVideo", "Export Video…", "video"),
        ] {
            let r = ui.add_enabled(!running, egui::Button::new(crate::i18n::tr(label)));
            register(ui, format!("button:{id}"), r.rect);
            if r.clicked() {
                let _ = app.run("dialog.slideshowExport", json!({"format": format}));
            }
        }
    });
    hint(ui, t, crate::i18n::tr("The photos in view as slides, with the settings above. Progress shows in the activity stack."));
}

fn playback_section(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    use lightcraft_engine::slideshow::{FADE_MAX, SLIDE_MAX, SLIDE_MIN};
    heading(ui, t, crate::i18n::tr("Playback"));
    let s = app.session.slideshow.clone();
    let mut v = s.manual;
    if check(ui, "settings.slideshowManual", &mut v, "Manual slideshow") {
        let _ = app.run("slideshow.playback", json!({"manual": v}));
    }
    hint(ui, t, crate::i18n::tr("A manual slideshow shows each slide until → or ← changes it. Exports still use the slide duration."));
    row(ui, t, crate::i18n::tr("Slide duration"), |ui| {
        duration_slider(app, ui, "slideshowSlide", "slideDuration", s.slide_duration, SLIDE_MIN..=SLIDE_MAX)
    });
    row(ui, t, crate::i18n::tr("Fade duration"), |ui| {
        duration_slider(app, ui, "slideshowFade", "fadeDuration", s.fade_duration, 0.0..=FADE_MAX.min(s.slide_duration))
    });
    hint(ui, t, crate::i18n::tr("A slide stays up for its duration, its fade in included. A fade of 0 cuts."));
    row(ui, t, crate::i18n::tr("Fade"), |ui| {
        #[derive(Clone, Copy, PartialEq)]
        enum Fade {
            Cross,
            Black,
            White,
            Color,
        }
        let mut f = match s.fade_color {
            None => Fade::Cross,
            Some([0, 0, 0]) => Fade::Black,
            Some([255, 255, 255]) => Fade::White,
            Some(_) => Fade::Color,
        };
        let opts = [(Fade::Cross, "Crossfade"), (Fade::Black, "Through black"), (Fade::White, "Through white"), (Fade::Color, "Through a colour")];
        if choices(ui, "settingsSlideshowFade", &opts, &mut f) {
            let c = match f {
                Fade::Cross => Value::Null,
                Fade::Black => json!("#000000"),
                Fade::White => json!("#ffffff"),
                Fade::Color => json!([64, 64, 64]),
            };
            let _ = app.run("slideshow.playback", json!({"fadeColor": c}));
        }
        if f == Fade::Color
            && let Some(mut c) = s.fade_color
        {
            let r = egui::color_picker::color_edit_button_srgb(ui, &mut c);
            register(ui, "button:slideshowFadeColor", r.rect);
            if r.changed() {
                let _ = app.run("slideshow.playback", json!({"fadeColor": c}));
            }
        }
    });
    let mut v = s.random_order;
    if check(ui, "settings.slideshowRandom", &mut v, "Random order") {
        let _ = app.run("slideshow.playback", json!({"randomOrder": v}));
    }
    let mut v = s.repeat;
    if check(ui, "settings.slideshowRepeat", &mut v, "Repeat (off: the slideshow ends after the last photo)") {
        let _ = app.run("slideshow.playback", json!({"repeat": v}));
    }
    let mut v = s.prepare_previews;
    if check(ui, "settings.slideshowPrepare", &mut v, "Prepare previews in advance") {
        let _ = app.run("slideshow.playback", json!({"preparePreviews": v}));
    }
    hint(
        ui,
        t,
        crate::i18n::tr(
            "Renders the next slides while the current one is up, so fades never wait. Off: each slide renders when it is due (less memory and work).",
        ),
    );
    hint(
        ui,
        t,
        crate::i18n::tr("Slideshow in the View menu plays the photos in view from the selected one. These settings are saved with the library."),
    );
}
