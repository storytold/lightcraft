//! Library ▸ Review Comments: the comments people left on published photos (Immich: the album's
//! activity), per photo with author, date and text. The network asks run on a worker thread
//! (`tasks::spawn`, an activity row while they run); Refresh asks again. A service without
//! comments (Hard Drive, SFTP) says so. Fork-owned.
#![allow(clippy::indexing_slicing, reason = "not yet converted to checked indexing: tracked in docs/gaps.md (indexing lint)")]

use chrono::DateTime;
use egui::{RichText, vec2};
use lcc_engine::cmd::publish::{CommentsReview, comments_job};
use serde_json::{Value, json};

use crate::LccApp;
use crate::i18n::tr;
use crate::menus::UiCommand;
use crate::theme::Tokens;
use crate::widgets::register;

pub const OPEN: &str = "dialog.reviewComments";

pub const COMMANDS: &[UiCommand] = &[(OPEN, "Review Comments", None, "")];

/// The task's label (one review at a time).
const TASK: &str = "Loading comments";
/// The activity row's kind.
const KIND: &str = "comments";

/// What the window shows.
#[derive(Default)]
pub struct View {
    pub open: bool,
    /// The chosen published collection (an album id).
    pub collection: Option<u64>,
    pub state: State,
}

#[derive(Default)]
pub enum State {
    #[default]
    Idle,
    Loading,
    Loaded {
        collection: u64,
        review: CommentsReview,
    },
    Failed(String),
}

/// One published collection to choose from.
struct Choice {
    album: u64,
    label: String,
    comments: bool,
}

/// The published collections, with whether their service has comments.
fn choices(app: &mut LccApp) -> Vec<Choice> {
    let list = app.session.execute("publish.services", &json!({})).unwrap_or(Value::Null);
    let mut out = Vec::new();
    for svc in list["services"].as_array().map(Vec::as_slice).unwrap_or_default() {
        let comments = svc["capabilities"]["comments"].as_bool().unwrap_or(false);
        let service = svc["name"].as_str().unwrap_or_default();
        for c in svc["collections"].as_array().map(Vec::as_slice).unwrap_or_default() {
            let Some(album) = c["album"].as_u64() else { continue };
            let name = c["name"].as_str().unwrap_or_default();
            let mut label = format!("{name} · {service}");
            if !comments {
                label.push_str(&format!(" ({})", tr("no comments")));
            }
            out.push(Choice { album, label, comments });
        }
    }
    out
}

/// A server's ISO 8601 date as `2026-11-01 09:00 +00:00`, the text as it came when it isn't one.
pub fn format_date(iso: &str) -> String {
    match DateTime::parse_from_rfc3339(iso.trim()) {
        Ok(d) => d.format("%Y-%m-%d %H:%M %:z").to_string(),
        Err(_) => iso.chars().take(40).collect(),
    }
}

/// Ask the service again for the chosen collection, on a worker thread.
pub fn refresh(app: &mut LccApp) -> Result<(), String> {
    let Some(album) = app.review_comments.collection else { return Err(tr("No published collections yet").to_string()) };
    if app.tasks.is_running(TASK) {
        return Ok(());
    }
    let job = comments_job(&mut app.session, &json!({"collection": album}), None, "publish.reviewComments").map_err(|e| e.to_string())?;
    app.review_comments.state = State::Loading;
    let work = move || job.fetch().map_err(|e| e.to_string());
    let done = move |app: &mut LccApp, ctx: &egui::Context, r: Result<CommentsReview, String>| {
        // the user may have chosen another collection meanwhile: that answer is stale
        if app.review_comments.collection != Some(album) {
            if let Err(e) = refresh(app) {
                app.review_comments.state = State::Failed(e);
            }
            return;
        }
        app.review_comments.state = match r {
            Ok(review) => State::Loaded { collection: album, review },
            Err(e) => State::Failed(e),
        };
        ctx.request_repaint();
    };
    crate::tasks::spawn(app, TASK, Some(KIND), work, done)
}

/// Choose a collection (the first with comments by default) and ask.
fn choose(app: &mut LccApp, album: Option<u64>) -> Result<(), String> {
    let all = choices(app);
    let pick = album.filter(|a| all.iter().any(|c| c.album == *a)).or_else(|| all.iter().find(|c| c.comments).or(all.first()).map(|c| c.album));
    app.review_comments.collection = pick;
    app.review_comments.state = State::Idle;
    if pick.is_some() { refresh(app) } else { Ok(()) }
}

pub fn enabled(app: &LccApp, id: &str) -> Option<bool> {
    (id == OPEN).then(|| app.session.library.as_ref().is_some_and(|l| l.on_disk))
}

pub fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if id != OPEN {
        return None;
    }
    app.review_comments.open = true;
    let want = p.get("collection").and_then(Value::as_u64).or(app.review_comments.collection);
    if let Err(e) = choose(app, want) {
        app.review_comments.state = State::Failed(e);
    }
    Some(Ok(json!({"collection": app.review_comments.collection})))
}

/// A photo's file name (its id when it left the catalog).
fn photo_name(app: &LccApp, id: lcc_catalog::PhotoId) -> String {
    app.session.catalog.photo(id).map(|p| p.file_name.clone()).unwrap_or_else(|| format!("#{}", id.0))
}

pub fn show(app: &mut LccApp, ctx: &egui::Context) {
    if !app.review_comments.open {
        return;
    }
    let t = Tokens::get(ctx);
    let all = choices(app);
    let id = egui::Id::new("review-comments");
    let mut chosen: Option<u64> = None;
    let (mut again, mut closed) = (false, false);
    let current = app.review_comments.collection;
    let shown = egui::Window::new(tr("Review Comments"))
        .id(id)
        .title_bar(false)
        .frame(crate::dialog_frame::card_frame(ctx))
        .collapsible(false)
        .default_width(560.0)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .max_height(crate::dialog_frame::max_window_height(ctx))
        .show(ctx, |ui| {
            closed = crate::dialog_frame::card_header(ui, tr("Review Comments"), Some("review-comments:close"));
            let mut layout = crate::dialog_frame::ModalLayout::begin(ui, id);
            if all.is_empty() {
                ui.add(
                    egui::Label::new(
                        RichText::new(tr("No published collections yet. Publish a collection to an Immich service to see its comments here."))
                            .color(t.text_label),
                    )
                    .wrap(),
                );
            } else {
                ui.horizontal(|ui| {
                    let selected = all.iter().find(|c| Some(c.album) == current).map(|c| c.label.clone()).unwrap_or_default();
                    let r = egui::ComboBox::from_id_salt("reviewCommentsCollection").selected_text(selected).width(320.0).show_ui(ui, |ui| {
                        for c in &all {
                            if ui.selectable_label(Some(c.album) == current, &c.label).clicked() {
                                chosen = Some(c.album);
                            }
                        }
                    });
                    register(ui.ctx(), "combo:reviewComments.collection".to_string(), r.response.rect);
                    let busy = matches!(app.review_comments.state, State::Loading);
                    let b = ui.add_enabled(!busy && current.is_some(), egui::Button::new(tr("Refresh")).min_size(vec2(0.0, 26.0)));
                    register(ui.ctx(), "button:reviewComments.refresh".to_string(), b.rect);
                    again |= b.clicked();
                });
                ui.add_space(6.0);
            }
            layout.body(ui, |ui| match &app.review_comments.state {
                State::Idle => {}
                State::Loading => {
                    ui.add(egui::Label::new(RichText::new(tr("Loading comments…")).color(t.text_dim)));
                }
                State::Failed(e) => {
                    ui.add(egui::Label::new(RichText::new(format!("{}: {e}", tr("Comments couldn't be loaded"))).color(t.text_label)).wrap());
                }
                State::Loaded { review, .. } if !review.supported => {
                    ui.add(egui::Label::new(RichText::new(tr("This service has no comments. Immich albums do.")).color(t.text_label)).wrap());
                }
                State::Loaded { review, .. } => {
                    let shown: Vec<_> = review.photos.iter().filter(|p| !p.comments.is_empty() || p.error.is_some()).collect();
                    if shown.is_empty() {
                        ui.add(egui::Label::new(RichText::new(tr("No comments on the published photos.")).color(t.text_label)));
                    }
                    for (i, p) in shown.iter().enumerate() {
                        ui.add(egui::Label::new(RichText::new(photo_name(app, p.photo)).strong()));
                        if let Some(e) = &p.error {
                            ui.add(egui::Label::new(RichText::new(e).color(t.text_dim).small()).wrap());
                        }
                        for (j, c) in p.comments.iter().enumerate() {
                            let who = match &c.date {
                                Some(d) => format!("{} · {}", c.author, format_date(d)),
                                None => c.author.clone(),
                            };
                            ui.push_id(("review-comment", i, j), |ui| {
                                ui.add(egui::Label::new(RichText::new(who).color(t.text_dim).small()));
                                ui.add(egui::Label::new(RichText::new(&c.text).color(t.text)).wrap());
                            });
                            ui.add_space(2.0);
                        }
                        ui.add_space(8.0);
                    }
                    if review.skipped > 0 {
                        ui.add(egui::Label::new(RichText::new(tr("Only the first published photos were asked.")).color(t.text_dim).small()));
                    }
                }
            });
        });
    crate::dialog_frame::ModalLayout::finish(ctx, id, shown.as_ref().map(|w| w.response.rect));
    if closed {
        app.review_comments.open = false;
    }
    if let Some(a) = chosen.filter(|a| Some(*a) != current)
        && let Err(e) = choose(app, Some(a))
    {
        app.review_comments.state = State::Failed(e);
    }
    if again && let Err(e) = refresh(app) {
        app.review_comments.state = State::Failed(e);
    }
}

#[cfg(test)]
mod tests {
    use super::format_date;

    #[test]
    fn dates_read_as_iso_and_oddities_pass_through_bounded() {
        assert_eq!(format_date("2026-11-01T09:05:00.000Z"), "2026-11-01 09:05 +00:00");
        assert_eq!(format_date("2026-11-01T09:05:00+10:00"), "2026-11-01 09:05 +10:00");
        assert_eq!(format_date("last Tuesday"), "last Tuesday");
        assert!(format_date(&"x".repeat(500)).chars().count() <= 40);
        assert_eq!(format_date(""), "");
    }
}
