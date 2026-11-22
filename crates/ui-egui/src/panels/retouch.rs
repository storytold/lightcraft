//! Portrait retouching (ported from OneSlips): the Edit panel's **Retouch** section, one
//! sub-section per part of a person (Skin, Blemishes, Teeth), each with its own eye for a
//! before/after. They run `skin.*`, `spot.findBlemishes` and `retouch.*`; the open MediaPipe models they need are offered in
//! the AI Models dialog when missing (nothing is downloaded without the person's go-ahead).

use egui::{Align2, RichText, Sense, pos2, vec2};
use lcc_catalog::PhotoId;
use lcc_develop::{DevelopSettings, SkinArea, SkinSmoothing};
use serde_json::{Value, json};

use crate::LccApp;
use crate::i18n::tr;
use crate::icons::{Icon, paint};
use crate::theme::Tokens;
use crate::widgets::{register, segmented, text_button};

/// The open models Retouch needs.
const MODELS: [&str; 2] = ["people", "face"];

/// The Retouch section's body: its sub-sections, folded or open.
pub fn panel(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings, id: PhotoId) {
    if sub_header(app, ui, d, "skin", "Skin", d.skin.active(), true) {
        smooth_skin(app, ui, d, id);
    }
    if sub_header(app, ui, d, "blemishes", "Blemishes", false, false) {
        padded(ui, |ui| {
            note(ui, "Heals small dark or red spots on the skin of the faces (and, if you like, of the body).");
            ui.add_space(4.0);
            find_blemishes(app, ui);
        });
        ui.add_space(4.0);
    }
    for (key, title, intro, missing) in PARTS {
        if sub_header(app, ui, d, key, title, d.retouch.section_active(key), true) {
            part_body(app, ui, d, id, key, intro, missing);
        }
    }
    ui.add_space(4.0);
}

/// The sections beyond Skin and Blemishes: (key, title, what it does, what to say when its part
/// isn't in the photo).
const PARTS: &[(&str, &str, &str, &str)] =
    &[("teeth", "Teeth", "Whiter, brighter teeth: the yellow is taken out gently.", "No teeth were found in this photo.")];

/// Where a section stands on this photo, in words (`None`: its parts were found, or it is off).
fn part_status(app: &LccApp, d: &DevelopSettings, id: PhotoId, key: &str, missing: &str) -> Option<String> {
    if !d.retouch.section_active(key) {
        return None;
    }
    let a = d.retouch_analysis.as_ref().filter(|a| a.orientation == d.orientation);
    let parts = d.retouch.section_parts(key);
    let found: Vec<Option<bool>> = parts.iter().map(|r| a.and_then(|a| a.mask(*r)).map(|m| m.seg.is_some())).collect();
    if found.contains(&Some(true)) {
        return None;
    }
    if found.iter().all(Option::is_some) {
        let person = parts.first().map_or(0, |r| r.person);
        return Some(if person > 0 { tr("This person wasn't found in this photo.").to_string() } else { tr(missing).to_string() });
    }
    Some(if app.session.retouch_pending(id) {
        tr("Finding teeth…").to_string()
    } else if !lcc_engine::segment::Segmenter::AVAILABLE {
        return None;
    } else if missing_model(app).is_some() {
        tr("Needs the MediaPipe people and face models (one-time downloads, Apache-2.0).").to_string()
    } else {
        tr("The teeth haven't been found yet.").to_string()
    })
}

/// A part's sub-section body: what it does or where it stands, its sliders, the person, Turn Off.
fn part_body(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings, id: PhotoId, key: &str, intro: &str, missing: &str) {
    let status = part_status(app, d, id, key, missing);
    let pending = app.session.retouch_pending(id);
    padded(ui, |ui| match &status {
        Some(s) => {
            note(ui, s);
            if !pending && lcc_engine::segment::Segmenter::AVAILABLE {
                let need = missing_model(app);
                let (label, wid) =
                    if need.is_some() { ("Download…", format!("retouchDownload-{key}")) } else { ("Find Again", format!("retouchFind-{key}")) };
                if text_button(ui, &wid, label, false).clicked() {
                    if need.is_some() {
                        offer_models(app);
                    } else {
                        let r = app.run_typed("retouch.find", json!({}));
                        report(app, ui.ctx(), r);
                    }
                }
            }
        }
        None => note(ui, intro),
    });
    if pending {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }
    for (sec, _, control) in lcc_develop::retouch::SLIDERS {
        if *sec != key {
            continue;
        }
        let before = d.retouch.section_active(key);
        super::edit::control(app, ui, d, control, true);
        // the first move of a slider needs the models: offer them when they are missing
        let now = app.session.develop_of(id).is_some_and(|n| n.retouch.section_active(key));
        if !before && now && lcc_engine::segment::Segmenter::AVAILABLE && missing_model(app).is_some() {
            offer_models(app);
        }
    }
    super::edit::sub_title(ui, "Person");
    let people = [("All", "0"), ("1", "1"), ("2", "2"), ("3", "3"), ("4", "4")];
    let person = lcc_develop::controls::get(d, &lcc_develop::retouch::person_control(key)).unwrap_or(0.0).clamp(0.0, 4.0) as usize;
    let mut chosen = None;
    padded(ui, |ui| chosen = segmented(ui, &format!("retouchPerson-{key}"), &people, Some(person), 5));
    if let Some(i) = chosen {
        let r = app.run_typed("retouch.set", json!({"section": key, "person": i}));
        report(app, ui.ctx(), r);
    }
    padded(ui, |ui| {
        note(ui, "People are counted from the left, by their face.");
        if d.retouch.section_active(key) {
            ui.add_space(4.0);
            let r = ui.button(tr("Turn Off"));
            register(ui.ctx(), format!("button:retouchOff-{key}"), r.rect);
            if r.clicked() {
                let r = app.run_typed("retouch.off", json!({"section": key}));
                report(app, ui.ctx(), r);
            }
        }
    });
    ui.add_space(6.0);
}

/// The first open model Retouch needs that is not installed.
fn missing_model(app: &LccApp) -> Option<&'static str> {
    MODELS.into_iter().find(|m| !app.session.segmenter.open_model_installed(m))
}

/// Offer the missing models (the AI Models dialog, which names their size and licence).
fn offer_models(app: &mut LccApp) {
    if app.ui.dialog.is_none() {
        app.ui.dialog = Some(crate::state::Dialog::Settings { tab: "ai".into() });
    }
}

/// A sub-section's header: a chevron, its title, a dot while it changes the photo, and (`eye`) its
/// eye, which switches it off for a before/after. Returns whether it is open.
fn sub_header(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings, key: &str, title: &str, active: bool, eye: bool) -> bool {
    let t = Tokens::get(ui.ctx());
    let flyout = format!("retouch.{key}");
    let open = app.ui.flyout_open(&flyout);
    let title = tr(title);
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::CollapsingHeader, true, open, title.to_string()));
    register(ui.ctx(), format!("retouch:{key}"), r);
    let p = ui.painter();
    let chev = egui::Rect::from_center_size(pos2(r.left() + 18.0, r.center().y), vec2(12.0, 12.0));
    paint(p, chev, if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.text_label);
    let galley = p.text(pos2(r.left() + 30.0, r.center().y), Align2::LEFT_CENTER, title, t.semibold(12.0), t.text);
    let on = d.section_enabled(key);
    if active {
        // a dot: this part is being retouched
        p.circle_filled(pos2(galley.right() + 8.0, r.center().y), 3.0, if on { t.accent } else { t.text_disabled });
    }
    if eye && (active || !on) {
        let er = egui::Rect::from_center_size(pos2(r.right() - 22.0, r.center().y), vec2(16.0, 16.0));
        let e = ui.interact(er, ui.id().with(("retouchEye", key)), Sense::click()).on_hover_text(tr("Show / hide"));
        register(ui.ctx(), format!("retouchEye:{key}"), er);
        paint(ui.painter(), er, if on { Icon::Eye } else { Icon::EyeOff }, if on { t.icon } else { t.text_disabled });
        if e.clicked() {
            let _ = app.run("develop.sectionEnabled", json!({"section": key, "enabled": !on}));
            return open;
        }
    }
    if resp.clicked() {
        app.ui.toggle_flyout(&flyout);
    }
    open
}

fn padded(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 4, bottom: 4 }).show(ui, add);
}

fn note(ui: &mut egui::Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    ui.add(egui::Label::new(RichText::new(tr(text)).size(11.0).color(t.text_dim)).wrap());
}

/// A command's error, or the `error` its reply carries: when an open model is missing, the dialog
/// that offers it; otherwise a toast.
fn report(app: &mut LccApp, ctx: &egui::Context, r: Result<Value, crate::CommandError>) {
    let e = match r {
        Ok(v) => match v.get("error").and_then(Value::as_str) {
            Some(e) => e.to_string(),
            None => return,
        },
        Err(e) if matches!(e.kind, lcc_engine::ErrorKind::ModelMissing(_)) => return offer_models(app),
        Err(e) => e.message,
    };
    if missing_model(app).is_some() && lcc_engine::segment::Segmenter::AVAILABLE {
        offer_models(app);
    } else {
        app.toast_error(ctx, e);
    }
}

/// Where Smooth Skin stands on this photo, in words (`None`: it is working on the skin found).
fn status(app: &LccApp, d: &DevelopSettings, id: PhotoId) -> Option<String> {
    let found = d.skin_analysis.as_ref().filter(|a| a.is_for(&d.skin, d.orientation));
    match found {
        Some(a) if a.seg.logits().is_some_and(|l| l.iter().any(|v| *v > 0.0)) => None,
        Some(_) if d.skin.person > 0 => Some(tr("This person wasn't found in this photo.").to_string()),
        Some(_) => Some(tr("No skin was found in this photo.").to_string()),
        None if app.session.skin_pending(id) => Some(tr("Finding the skin…").to_string()),
        None if !lcc_engine::segment::Segmenter::AVAILABLE => None,
        None if missing_model(app).is_some() => Some(tr("Needs the MediaPipe people and face models (one-time downloads, Apache-2.0).").to_string()),
        None => Some(tr("The skin hasn't been found yet.").to_string()),
    }
}

/// The Skin sub-section's body (Smooth Skin and Reduce Shine).
pub fn smooth_skin(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings, id: PhotoId) {
    let k = d.skin;
    if !k.active() {
        padded(ui, |ui| {
            note(ui, "Evens out skin tone and texture while keeping pores: the skin and faces are found by the open MediaPipe models.");
            ui.add_space(4.0);
            if text_button(ui, "skinSmooth", "Smooth Skin", false).on_hover_text(tr("Switch Smooth Skin on with subtle settings")).clicked() {
                let r = app.run_typed("skin.smooth", json!({}));
                report(app, ui.ctx(), r);
            }
        });
        return;
    }
    if let Some(s) = status(app, d, id) {
        padded(ui, |ui| {
            note(ui, &s);
            if !app.session.skin_pending(id) && lcc_engine::segment::Segmenter::AVAILABLE {
                let missing = missing_model(app);
                let (label, wid) = if missing.is_some() { ("Download…", "skinDownload") } else { ("Find Skin", "skinFind") };
                if text_button(ui, wid, label, false).clicked() {
                    if missing.is_some() {
                        offer_models(app);
                    } else {
                        let r = app.run_typed("skin.find", json!({}));
                        report(app, ui.ctx(), r);
                    }
                }
            }
        });
    }
    if app.session.skin_pending(id) {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }
    for c in ["skin.amount", "skin.texture", "skin.evenTone", "skin.redness", "skin.shine"] {
        super::edit::control(app, ui, d, c, true);
    }
    super::edit::sub_title(ui, "Face or Body");
    let areas = [(SkinArea::Face, "Face", "face"), (SkinArea::Body, "Body", "body"), (SkinArea::Both, "Both", "both")];
    let items: Vec<(&str, &str)> = areas.iter().map(|(_, l, k)| (*l, *k)).collect();
    let mut chosen = None;
    padded(ui, |ui| chosen = segmented(ui, "skinArea", &items, areas.iter().position(|(a, _, _)| *a == k.area), 3));
    if let Some(i) = chosen
        && let Some((_, _, name)) = areas.get(i)
    {
        let r = app.run_typed("skin.set", json!({"area": name}));
        report(app, ui.ctx(), r);
    }
    super::edit::sub_title(ui, "Person");
    // (All, not Everyone: the longer word overflowed its segment)
    let people = [("All", "0"), ("1", "1"), ("2", "2"), ("3", "3"), ("4", "4")];
    let mut chosen = None;
    padded(ui, |ui| chosen = segmented(ui, "skinPerson", &people, Some(k.person.min(SkinSmoothing::MAX_PERSON) as usize), 5));
    if let Some(i) = chosen {
        let r = app.run_typed("skin.set", json!({"person": i}));
        report(app, ui.ctx(), r);
    }
    padded(ui, |ui| {
        note(ui, "People are counted from the left, by their face.");
        ui.add_space(4.0);
        let r = ui.button(tr("Turn Off"));
        register(ui.ctx(), "button:skinOff", r.rect);
        if r.clicked() {
            let r = app.run_typed("skin.smooth", json!({"off": true}));
            report(app, ui.ctx(), r);
        }
    });
    ui.add_space(6.0);
}

/// Find Blemishes (in Retouch, next to Smooth Skin): the button and its "Include body skin" option.
pub fn find_blemishes(app: &mut LccApp, ui: &mut egui::Ui) {
    if text_button(ui, "findBlemishes", "Find Blemishes", false)
        .on_hover_text(tr("Heal the small dark or red spots on the faces' skin; eyes, brows, nostrils and lips are left alone"))
        .clicked()
    {
        let body = app.ui.blemish_body;
        match app.run_typed("spot.findBlemishes", json!({"body": body})) {
            Ok(v) if v.get("queued").and_then(Value::as_bool) == Some(true) => app.toast(ui.ctx(), tr("Finding blemishes…")),
            Ok(v) => {
                let text = match v.get("added").and_then(Value::as_u64).unwrap_or(0) {
                    0 => tr("No blemishes found").to_string(),
                    1 => tr("Healed 1 blemish").to_string(),
                    n => crate::i18n::tr_format!("Healed {n} blemishes", n = n),
                };
                app.toast(ui.ctx(), text);
            }
            Err(e) => report(app, ui.ctx(), Err(e)),
        }
    }
    let mut body = app.ui.blemish_body;
    let r = ui.checkbox(&mut body, tr("Include body skin"));
    register(ui.ctx(), "check:blemishBody", r.rect);
    let r = r.on_hover_text(tr("Also look on shoulders, arms and back"));
    if r.changed() {
        app.ui.blemish_body = body;
    }
}
