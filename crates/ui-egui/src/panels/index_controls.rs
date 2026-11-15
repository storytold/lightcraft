//! A background indexer's status and controls in Settings: the face scan (Settings ▸ Faces) and semantic search
//! indexing (Settings ▸ AI Models ▸ Semantic search). A status line (off, not started, indexing, paused, complete, with
//! how many photos are done) and Pause / Resume, Clear… and Restart…, each the indexer's own engine command. Clear and
//! Restart throw the index away, so each asks first, in place under the buttons.

use serde_json::{Value, json};

use crate::LccApp;
use crate::i18n::{tr, tr_format};
use crate::theme::Tokens;
use crate::widgets::register;

/// One indexer: the prefix of its widget ids (`button:{key}Pause`…), its commands, and its two questions.
pub struct Indexer {
    pub key: &'static str,
    pub pause: &'static str,
    pub resume: &'static str,
    pub clear: &'static str,
    pub restart: &'static str,
    /// What Clear… asks before it deletes the index.
    pub clear_question: &'static str,
    /// What Restart… asks.
    pub restart_question: &'static str,
}

/// The state the engine reports (`state`), as the status line words it.
pub fn state_text(state: &str) -> &'static str {
    match state {
        "off" => "Off",
        "paused" => "Paused",
        "indexing" => "Indexing",
        "complete" => "Complete",
        _ => "Not started",
    }
}

/// Which question is open under the buttons.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Asking {
    Clear,
    Restart,
}

/// The status line and the buttons. `status` is what the indexer's status command returned (`state`, `done`,
/// `total`, `paused`). Returns whether a command ran (the caller re-reads its cached state).
pub fn show(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, ix: &Indexer, status: &Value) -> bool {
    let state = status["state"].as_str().unwrap_or("idle");
    let (done, total) = (status["done"].as_u64().unwrap_or(0), status["total"].as_u64().unwrap_or(0));
    let paused = status["paused"].as_bool() == Some(true);
    super::settings::row(ui, t, "Status", |ui| {
        let mut text = tr(state_text(state)).to_string();
        if total > 0 && state != "off" {
            text += &format!(" · {}", tr_format!("{done} of {total} photos", done = done.min(total), total = total));
        }
        let r = ui.label(egui::RichText::new(text).color(t.text));
        register(ui.ctx(), format!("{}:status", ix.key), r.rect);
    });
    if total > 0 && matches!(state, "indexing" | "paused") {
        let fraction = (done.min(total) as f64 / total as f64) as f32;
        crate::progress::Progress::new(Some(fraction)).widget(format!("{}:progress", ix.key)).dimmed(paused).show(ui);
    }
    let ask_id = egui::Id::new(("index-controls-asking", ix.key));
    let mut asking: Option<Asking> = ui.data(|d| d.get_temp(ask_id)).flatten();
    let mut ran = false;
    let mut run = |app: &mut LccApp, ui: &egui::Ui, cmd: &str| {
        if let Err(e) = app.run(cmd, json!({})) {
            app.toast(ui.ctx(), e);
        }
        ran = true;
    };
    ui.horizontal_wrapped(|ui| {
        let off = state == "off";
        if paused {
            if crate::button::Button::new("Resume").small().enabled(!off).widget(format!("button:{}Resume", ix.key)).show(ui).clicked() {
                run(app, ui, ix.resume);
            }
        } else if crate::button::Button::new("Pause").small().enabled(!off).widget(format!("button:{}Pause", ix.key)).show(ui).clicked() {
            run(app, ui, ix.pause);
        }
        if crate::button::Button::new("Clear…").small().widget(format!("button:{}Clear", ix.key)).show(ui).clicked() {
            asking = Some(Asking::Clear);
        }
        if crate::button::Button::new("Restart…").small().enabled(!off).widget(format!("button:{}Restart", ix.key)).show(ui).clicked() {
            asking = Some(Asking::Restart);
        }
    });
    if let Some(what) = asking {
        let (question, button, cmd, id) = match what {
            Asking::Clear => (ix.clear_question, "Clear", ix.clear, "ClearConfirm"),
            Asking::Restart => (ix.restart_question, "Restart", ix.restart, "RestartConfirm"),
        };
        ui.add(egui::Label::new(egui::RichText::new(tr(question)).font(t.font(11.5)).color(t.caution)).wrap());
        ui.horizontal(|ui| {
            if crate::button::Button::new(button).small().danger().widget(format!("button:{}{id}", ix.key)).show(ui).clicked() {
                run(app, ui, cmd);
                asking = None;
            }
            if crate::button::Button::new("Cancel").small().widget(format!("button:{}ConfirmCancel", ix.key)).show(ui).clicked() {
                asking = None;
            }
        });
    }
    ui.data_mut(|d| d.insert_temp(ask_id, asking));
    ran
}
