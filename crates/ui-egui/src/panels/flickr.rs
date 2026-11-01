//! Settings → Accounts: connecting LightCraft to a Flickr account, one step at a time, then the
//! connected account. The steps lead into each other: saving the API key starts the sign-in
//! (Flickr's approval page opens in the browser), and pasting the code Flickr shows finishes it.
//! Network steps run in the background ([`crate::flickr`]); this tab only shows where they are.

use egui::RichText;
use serde_json::{Value, json};

use super::settings::{heading, hint, row};
use crate::LightcraftApp;
use crate::text_field::TextField;
use crate::theme::Tokens;
use crate::widgets::register;

fn button(ui: &mut egui::Ui, id: &str, label: &str, enabled: bool) -> bool {
    let r = ui.add_enabled(enabled, egui::Button::new(crate::i18n::tr(label)));
    register(ui, format!("button:{id}"), r.rect);
    r.clicked()
}

/// Run a Flickr command from the tab; its error shows under the tab. Whether it worked.
fn run(app: &mut LightcraftApp, id: &str, p: Value) -> bool {
    app.flickr.error = None;
    match app.run(id, p) {
        Ok(_) => true,
        Err(e) => {
            app.flickr.error = Some(e);
            false
        }
    }
}

fn open(app: &mut LightcraftApp, url: &str) {
    if let Err(e) = crate::links::open(app, url) {
        app.flickr.error = Some(e);
    }
}

/// A numbered step: its number and title, then what it asks for.
fn step(ui: &mut egui::Ui, t: &Tokens, n: u8, title: &str, done: bool) {
    ui.add_space(6.0);
    let mark = if done { "✓".to_string() } else { n.to_string() };
    let color = if done { t.text_dim } else { t.text };
    ui.label(RichText::new(format!("{mark}  {}", crate::i18n::tr(title))).font(t.semibold(12.5)).color(color));
}

/// A complete Flickr code (`123-456-789`, spaces ignored).
pub fn complete_code(code: &str) -> bool {
    let c: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    let parts: Vec<&str> = c.split('-').collect();
    parts.len() == 3 && parts.iter().all(|p| p.len() == 3 && p.chars().all(|c| c.is_ascii_digit()))
}

pub fn settings_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    if cfg!(target_arch = "wasm32") {
        heading(ui, t, "Flickr");
        hint(ui, t, "Uploading to Flickr needs the desktop app.");
        return;
    }
    let status = app.session.execute("flickr.status", &json!({})).unwrap_or_default();
    // filled from the stored key when first shown, and again whenever that key changes (a
    // command, Forget Key): Save Key must never send a stale key, which would sign out
    let stored = status["apiKey"].as_str().unwrap_or_default();
    if !app.flickr.loaded || app.flickr.loaded_key != stored {
        app.flickr.key = stored.to_string();
        app.flickr.loaded_key = stored.to_string();
        app.flickr.secret.clear();
        app.flickr.loaded = true;
    }
    // (an upload keeps the sign-in it started with: no signing out or in until it ends)
    let busy = app.flickr.busy.or(app.flickr.uploading().then_some("Uploading to Flickr"));
    let (signed_in, pending, has_key) = (status["signedIn"] == true, status["pending"] == true, status["hasKey"] == true);

    if signed_in {
        connected(app, ui, t, &status);
    } else {
        heading(ui, t, "Connect to Flickr");
        hint(ui, t, "Upload chosen photos to your Flickr account. LightCraft asks Flickr for permission to upload; it never sees your password.");
        if !has_key {
            key_step(app, ui, t, busy);
            step(ui, t, 3, "Approve LightCraft on Flickr", false);
        } else {
            step(ui, t, 1, "API key", true);
            approve_step(app, ui, t, busy, pending);
        }
    }
    if let Some(b) = busy {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new(crate::i18n::tr(b)).color(t.text_dim));
        });
    }
    for e in [status["problem"].as_str().map(str::to_string), app.flickr.error.clone()].into_iter().flatten() {
        ui.add(egui::Label::new(RichText::new(e).color(t.caution)).wrap());
    }
    // the key, once set up, is out of the way
    if has_key {
        ui.add_space(8.0);
        egui::CollapsingHeader::new(RichText::new(crate::i18n::tr("API key")).color(t.text_dim)).id_salt("flickrKeySection").show(ui, |ui| {
            if status["builtinKey"] == true && status["ownKey"] != true {
                hint(ui, t, "This build has its own Flickr API key. Enter yours only to use it instead.");
            }
            key_fields(app, ui, t, &status);
            ui.horizontal(|ui| {
                let typed = !app.flickr.key.trim().is_empty() && !app.flickr.secret.trim().is_empty();
                if button(ui, "flickrSaveKey", "Save Key", busy.is_none() && typed) && save_key(app) {
                    app.flickr.secret.clear();
                }
                if status["ownKey"] == true && button(ui, "flickrForgetKey", "Forget Key", busy.is_none()) {
                    run(app, "flickr.setApiKey", json!({"key": "", "secret": ""}));
                    app.flickr.key.clear();
                    app.flickr.secret.clear();
                }
            });
        });
    }
    if status["saved"] == false {
        hint(ui, t, "This session keeps the sign-in until LightCraft quits.");
    }
}

/// Signed in: who, and what next.
fn connected(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, status: &Value) {
    heading(ui, t, "Flickr account");
    let name = status["fullname"].as_str().filter(|s| !s.trim().is_empty()).or(status["username"].as_str()).unwrap_or_default();
    ui.label(RichText::new(format!("✓  {}", crate::i18n::tr_format!("Signed in as {name}", name = name))).font(t.semibold(12.5)));
    hint(ui, t, "Select photos, then choose File → Share → Flickr….");
    ui.horizontal(|ui| {
        if button(ui, "flickrPhotostream", "Open Your Photostream", true) {
            open(app, &format!("https://www.flickr.com/photos/{}/", status["userNsid"].as_str().unwrap_or("me")));
        }
        if button(ui, "flickrSignOut", "Sign Out", app.flickr.busy.is_none() && !app.flickr.uploading()) {
            run(app, "flickr.signOut", json!({}));
        }
    });
}

fn key_fields(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, status: &Value) -> bool {
    let mut enter = false;
    row(ui, t, "Key", |ui| {
        enter |= TextField::singleline("field:flickrKey", &mut app.flickr.key).width(280.0).show(ui).committed();
    });
    row(ui, t, "Secret", |ui| {
        let hint_text = if status["ownKey"] == true { crate::i18n::tr("(saved; type to replace)").to_string() } else { String::new() };
        enter |= TextField::singleline("field:flickrSecret", &mut app.flickr.secret).password(true).hint(hint_text).width(280.0).show(ui).committed();
    });
    enter
}

fn save_key(app: &mut LightcraftApp) -> bool {
    let (key, secret) = (app.flickr.key.trim().to_string(), app.flickr.secret.trim().to_string());
    run(app, "flickr.setApiKey", json!({"key": key, "secret": secret}))
}

/// Steps 1 and 2: get a key from Flickr, paste it; Continue saves it and starts the sign-in.
fn key_step(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, busy: Option<&'static str>) {
    step(ui, t, 1, "Get a free API key from Flickr", false);
    hint(ui, t, "Flickr asks every app for one. On Flickr's page, choose a non-commercial key; any app name and description will do.");
    if button(ui, "flickrGetKey", "Open Flickr's API Key Page", true) {
        let page = app.session.execute("flickr.status", &json!({})).ok().and_then(|s| s["apiKeyPage"].as_str().map(str::to_string));
        open(app, &page.unwrap_or_default());
    }
    step(ui, t, 2, "Paste the key and its secret", false);
    let status = app.session.execute("flickr.status", &json!({})).unwrap_or_default();
    let enter = key_fields(app, ui, t, &status);
    let typed = !app.flickr.key.trim().is_empty() && !app.flickr.secret.trim().is_empty();
    ui.horizontal(|ui| {
        ui.add_space(super::settings::LABEL_W + 8.0);
        let go = button(ui, "flickrContinue", "Continue", busy.is_none() && typed) || (enter && typed && busy.is_none());
        if go && save_key(app) {
            app.flickr.secret.clear();
            // straight on to the sign-in: the browser opens Flickr's approval page
            run(app, "flickr.signIn", json!({}));
        }
    });
}

/// Step 3: approve LightCraft in the browser, then paste the code Flickr shows (it is sent as
/// soon as it is complete).
fn approve_step(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, busy: Option<&'static str>, pending: bool) {
    step(ui, t, 3, "Approve LightCraft on Flickr", false);
    if !pending {
        hint(ui, t, "Your browser opens Flickr. Sign in there if asked, approve LightCraft, and Flickr shows a code to paste here.");
        if button(ui, "flickrSignIn", "Sign In to Flickr…", busy.is_none()) {
            run(app, "flickr.signIn", json!({}));
        }
        return;
    }
    hint(ui, t, "Flickr opened in your browser. Approve LightCraft there, then paste the code Flickr shows:");
    let mut send = false;
    row(ui, t, "Code", |ui| {
        let field = TextField::singleline("field:flickrCode", &mut app.flickr.code).hint("123-456-789").width(140.0);
        if std::mem::take(&mut app.flickr.focus_code) {
            ui.memory_mut(|m| m.request_focus(crate::text_field::id("field:flickrCode")));
        }
        let r = field.show(ui);
        let code = app.flickr.code.trim().to_string();
        // a whole code is sent at once (once: a refused one waits to be changed)
        let complete = complete_code(&code) && code != app.flickr.submitted_code;
        let clicked = button(ui, "flickrFinishSignIn", "Finish Sign-In", busy.is_none() && !code.is_empty());
        send = busy.is_none() && !code.is_empty() && (clicked || r.committed() || complete);
    });
    if send {
        app.flickr.submitted_code = app.flickr.code.trim().to_string();
        run(app, "flickr.completeSignIn", json!({"code": app.flickr.submitted_code.clone()}));
    }
    ui.horizontal(|ui| {
        ui.add_space(super::settings::LABEL_W + 8.0);
        if let Some(url) = app.flickr.authorize_url.clone()
            && button(ui, "flickrOpenAgain", "Open the Flickr Page Again", true)
        {
            open(app, &url);
        }
        if button(ui, "flickrCancelSignIn", "Start Over", busy.is_none()) {
            run(app, "flickr.cancelSignIn", json!({}));
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn recognises_a_whole_code() {
        assert!(super::complete_code("123-456-789"));
        assert!(super::complete_code(" 123 - 456 - 789 "));
        for partial in ["", "123", "123-456", "123-456-78", "123-456-7890", "abc-def-ghi", "123456789"] {
            assert!(!super::complete_code(partial), "{partial}");
        }
    }
}
