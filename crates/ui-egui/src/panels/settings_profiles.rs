//! Settings ▸ Profiles (ported from OneSlips, LR-PROF-MANAGE / LR-PROF-BODY): each camera body's own
//! camera profile, and the way to the Profile Manager (`panels::profile_manager`).

use serde_json::{Value, json};

use super::settings::{heading, hint, row};
use crate::LccApp;
use crate::theme::Tokens;
use crate::widgets::{register, text_button};

pub fn settings_tab(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    let m = app.session.execute("profiles.manager", &json!({})).unwrap_or_default();
    // first: a long list of bodies or camera profiles below never pushes it out of view
    super::settings::heading_help(ui, t, "Profiles", "profiles");
    hint(
        ui,
        t,
        "The Profile Manager lists every look, LUT profile and camera profile: hide them from the menus, rename and group them, import new ones, and delete your own.",
    );
    if crate::dialog_frame::control_row(ui, |ui| text_button(ui, "profilesOpenManager", crate::i18n::tr("Profile Manager…"), false).clicked()) {
        let _ = app.run("dialog.profileManager", json!({}));
    }
    heading(ui, t, "Camera bodies");
    hint(
        ui,
        t,
        "Raws from a body start with its camera profile when they are imported. Apply to Photos gives it to the body's photos already in the catalog.",
    );
    let bodies: Vec<Value> = m["bodies"].as_array().cloned().unwrap_or_default();
    if bodies.is_empty() {
        hint(ui, t, "No raw photos with a known camera body in this catalog yet.");
    }
    for (i, b) in bodies.iter().enumerate() {
        let Some(key) = b["body"].as_str() else { continue };
        let label = format!("{} · {}", b["camera"].as_str().unwrap_or(""), b["serial"].as_str().unwrap_or(""));
        let current = b["profile"].as_str().unwrap_or("").to_string();
        let mut options = options_for(b["camera"].as_str().unwrap_or(""));
        // (the ones the Profile Manager hides are left out; the body's own stays)
        options.retain(|(o, _)| *o == current || !app.session.profile_hidden.iter().any(|h| h == o));
        row(ui, t, &label, |ui| {
            let shown = options.iter().find(|(id, _)| *id == current).map(|(_, n)| n.clone()).unwrap_or_else(|| current.clone());
            let mut chosen = current.clone();
            let combo = crate::widgets::combo_box(format!("bodyProfile{i}")).width(240.0).selected_text(shown).show_ui(ui, |ui| {
                for (id, name) in &options {
                    ui.selectable_value(&mut chosen, id.clone(), name);
                }
            });
            register(ui.ctx(), format!("combo:bodyProfile:{key}"), combo.response.rect);
            if chosen != current {
                let _ = app.run("profile.setBodyDefault", json!({"id": chosen, "body": key}));
            }
            let photos = b["photos"].as_u64().unwrap_or(0);
            if photos > 0 && !current.is_empty() && text_button(ui, &format!("bodyApply:{key}"), crate::i18n::tr("Apply to Photos"), false).clicked()
            {
                let _ = app.run("profile.applyBodyDefaults", json!({}));
            }
        });
    }
    // the imported camera profiles (.dcp): deleted here, with their copy as the camera's default (OneSlips,
    // LR-PROF-IMPORT `profile.deleteCamera`)
    let camera: Vec<Value> = m["camera"].as_array().cloned().unwrap_or_default();
    if !camera.is_empty() {
        heading(ui, t, "Camera profiles");
        hint(
            ui,
            t,
            "Delete removes an imported camera profile, and its use as its camera's default. Photos that chose it go back to their camera's default.",
        );
        for c in &camera {
            let Some(id) = c["id"].as_str() else { continue };
            let name = c["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(id);
            let mut label = format!("{} · {name}", c["model"].as_str().unwrap_or(""));
            if c["cameraDefault"] == true {
                label = crate::i18n::tr_format!("{label} (camera default)", label = label).to_string();
            }
            row(ui, t, &label, |ui| {
                let used = c["usedBy"].as_u64().unwrap_or(0);
                if used > 0 {
                    ui.label(egui::RichText::new(crate::i18n::tr_format!("{n} photos", n = used)).color(t.text_dim));
                }
                if text_button(ui, &format!("cameraProfileDelete:{id}"), crate::i18n::tr("Delete"), false).clicked() {
                    let _ = app.run("profile.deleteCamera", json!({"id": id}));
                }
            });
        }
    }
}

/// The camera profiles a body of `camera` can start with: the camera's default, the built-in colour,
/// and the imported (.dcp) and calibrated profiles for that camera (as the Profiles panel offers them).
fn options_for(camera: &str) -> Vec<(String, String)> {
    let mut options: Vec<(String, String)> =
        vec![(String::new(), crate::i18n::tr("Camera default").to_string()), ("lc".into(), crate::i18n::tr("Built-in colour").to_string())];
    for (pid, p) in lcc_raw::dcp::library() {
        if lcc_raw::dcp::covers_model(&p.model, camera) {
            let name = if p.name.is_empty() { pid.trim_start_matches("dcp:").to_string() } else { p.name.clone() };
            options.push((pid, name));
        }
    }
    if let Some(cal) = lcc_raw::dcp::calibrated_for_camera(camera) {
        options.extend(cal.looks.iter().map(|(pid, p)| (pid.clone(), p.name.clone())));
    }
    options
}
