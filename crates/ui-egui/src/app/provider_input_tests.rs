//! Native regressions exercise actual slider buffers and the guard used before web staging.

use egui::{Context, Event, Id, Key, Modifiers, PointerButton, Pos2, Rect, Vec2};
use serde_json::json;

use super::LightcraftApp;
use crate::file_providers::{Command, Host, Resource};
use crate::tests::fixtures::SyntheticLibrary;
use crate::widgets::{self, SliderOut};

fn fixture(tag: &str) -> (SyntheticLibrary, LightcraftApp, Host, Context) {
    let photos = SyntheticLibrary::new(tag);
    let mut app = photos.app();
    let ctx = Context::default();
    crate::theme::install_fonts(&ctx);
    app.tasks.repaint = Some(ctx.clone());
    app.fonts_ready = true;
    let host = Host::default();
    host.set_session("light-library-admission-0001".into()).unwrap();
    let connection = host.connect("storage".into(), "Storage".into(), "account".into()).unwrap();
    host.set_resources(&connection, vec![Resource { id: "other".into(), name: "Other.zip".into(), updated_at: String::new() }]).unwrap();
    app.file_provider = Some(host.clone());
    app.file_provider_document = 1;
    (photos, app, host, ctx)
}

fn request(app: &mut LightcraftApp, host: &Host) -> String {
    let reply = app.run("file.providerRecent", json!({"resourceId":"other"})).unwrap();
    let request = reply["requestId"].as_str().unwrap().to_string();
    assert_eq!(host.take_events().len(), 1);
    assert!(host.busy());
    request
}

fn deliver(host: &Host, request: &str) {
    host.open(request.into(), "Other.zip".into(), vec![1, 2, 3]).unwrap();
    let commands = host.take_commands();
    assert_eq!(commands.len(), 1);
    assert!(matches!(&commands[0], Command::Open { request: delivered, .. } if delivered == request));
    assert!(host.busy(), "this read is still owned until its admission outcome is delivered");
}

fn key(key: Key) -> Event {
    Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }
}

fn pointer(at: Pos2, pressed: bool) -> Event {
    Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
}

fn frame(app: &mut LightcraftApp, ctx: &Context, events: Vec<Event>, before_ui: Option<&mut bool>) -> Id {
    let mut raw = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 120.0))),
        time: Some(ctx.cumulative_pass_nr() as f64 / 60.0),
        events,
        ..Default::default()
    };
    app.raw_input_hook(ctx, &mut raw);
    let spec = lightcraft_develop::controls::find("light.exposure").unwrap();
    let mut draft_id = Id::unique("unrendered-slider-draft");
    let mut before_ui = before_ui;
    ctx.run_ui(raw, |ui| {
        if let Some(allowed) = before_ui.as_deref_mut() {
            *allowed = app.can_admit_provider_library(ui.ctx()).is_ok();
        }
        draft_id = ui.scope_id().with(spec.id).with("typing");
        let id = app.session.active().unwrap();
        let settings = app.session.develop_of(id).unwrap();
        let value = lightcraft_develop::controls::get(&settings, spec.id).unwrap();
        let out: SliderOut = widgets::slider(ui, spec, value, true, None);
        if let Some(value) = out.value {
            app.run("develop.set", json!({"control":spec.id,"value":value})).unwrap();
        }
        app.finish_ui_pass(ui.ctx());
    })
    .textures_delta
    .clear();
    draft_id
}

fn readout(ctx: &Context) -> Pos2 {
    widgets::take_registry(ctx).into_iter().find(|(name, _)| name == "sliderValue:light.exposure").unwrap().1.center()
}

#[test]
fn pending_provider_read_preserves_a_real_numeric_draft_including_when_parked() {
    let (_photos, mut app, host, ctx) = fixture("provider-draft");
    frame(&mut app, &ctx, vec![], None);
    frame(&mut app, &ctx, vec![], None);
    let at = readout(&ctx);
    let pending = request(&mut app, &host);
    frame(&mut app, &ctx, vec![Event::PointerMoved(at), pointer(at, true)], None);
    frame(&mut app, &ctx, vec![pointer(at, false)], None);
    frame(&mut app, &ctx, vec![], None);
    let draft_id = frame(&mut app, &ctx, vec![Event::Text("not-a-number".into())], None);
    let before = ctx.data(|data| data.get_temp::<(String, u8, bool)>(draft_id)).unwrap();
    assert_eq!(before.0, "not-a-number");
    assert!(widgets::library_input::has_draft(&ctx));
    let active = app.session.active();
    let generation = app.session.library_generation();
    let settings = app.session.develop_of(active.unwrap()).unwrap();
    deliver(&host, &pending);
    assert!(app.can_admit_provider_library(&ctx).is_err());
    assert!(app.run("file.providerRecent", json!({"resourceId":"other"})).is_err());
    assert_eq!(ctx.data(|data| data.get_temp::<(String, u8, bool)>(draft_id)), Some(before.clone()));
    assert_eq!(app.session.active(), active);
    assert_eq!(app.session.library_generation(), generation);
    assert_eq!(app.session.develop_of(active.unwrap()).unwrap(), settings);
    // A hidden panel/focus change cannot make its resident invalid text disappear from the guard.
    ctx.memory_mut(|memory| memory.surrender_focus(crate::text_field::id("sliderField:light.exposure")));
    assert!(!ctx.text_edit_focused(), "the parked-draft check must not rely on active text focus");
    assert!(app.can_admit_provider_library(&ctx).is_err());
    assert_eq!(ctx.data(|data| data.get_temp::<(String, u8, bool)>(draft_id)), Some(before));
    // Only the user's explicit cancellation consumes it, not an attempted library replacement.
    ctx.memory_mut(|memory| memory.request_focus(crate::text_field::id("sliderField:light.exposure")));
    frame(&mut app, &ctx, vec![key(Key::Escape)], None);
    assert!(!widgets::library_input::has_draft(&ctx));
    assert!(app.can_admit_provider_library(&ctx).is_ok(), "its own pending read must not block settled admission");
    host.opened(&pending, None, "fixture complete");
}

#[test]
fn a_gesture_or_hover_preview_started_after_request_is_not_committed_by_admission() {
    let (_photos, mut app, host, ctx) = fixture("provider-gesture");
    let pending = request(&mut app, &host);
    app.run("develop.beginInteraction", json!({"label":"Exposure"})).unwrap();
    app.run("develop.set", json!({"control":"light.exposure","value":1.0})).unwrap();
    app.ui.dragging_control = Some("light.exposure".into());
    let active = app.session.active().unwrap();
    let settings = app.session.develop_of(active).unwrap();
    let original = app.session.interaction.as_ref().unwrap().original.clone();
    let generation = app.session.library_generation();
    assert!(app.session.unsaved().is_none(), "storage-error status is not a gesture guard");
    deliver(&host, &pending);
    assert!(app.can_admit_provider_library(&ctx).is_err());
    assert_eq!(app.session.interaction.as_ref().unwrap().original, original);
    assert_eq!(app.session.develop_of(active).unwrap(), settings);
    assert_eq!(app.session.library_generation(), generation);
    assert_eq!(app.ui.dragging_control.as_deref(), Some("light.exposure"));
    app.run("develop.cancelInteraction", json!({})).unwrap();
    app.ui.dragging_control = None;
    app.hover_preview = Some(crate::HoverPreview { label: "Preview".into(), settings: (*settings).clone() });
    assert!(app.can_admit_provider_library(&ctx).is_err());
    assert!(app.hover_preview.is_some());
    app.hover_preview = None;
    assert!(app.can_admit_provider_library(&ctx).is_ok());
    host.opened(&pending, None, "fixture complete");
}

#[test]
fn same_frame_input_waits_for_widgets_but_idle_hover_and_leave_do_not_block_a_read() {
    let (_photos, mut app, host, ctx) = fixture("provider-raw-input");
    let pending = request(&mut app, &host);
    deliver(&host, &pending);
    let mut allowed = true;
    frame(&mut app, &ctx, vec![key(Key::Delete)], Some(&mut allowed));
    assert!(!allowed, "delivery before this frame's widgets must not retarget a key");
    assert!(app.can_admit_provider_library(&ctx).is_ok(), "the settled UI pass consumes the raw barrier");
    let mut hover_allowed = false;
    frame(&mut app, &ctx, vec![Event::PointerMoved(Pos2::new(390.0, 110.0)), Event::PointerGone], Some(&mut hover_allowed));
    assert!(hover_allowed, "idle pointer movement/leave is not editing input");
    let at = Pos2::new(390.0, 110.0);
    frame(&mut app, &ctx, vec![pointer(at, true)], None);
    assert!(app.can_admit_provider_library(&ctx).is_err(), "a held press survives a settled pass");
    frame(&mut app, &ctx, vec![pointer(at, false)], None);
    assert!(app.can_admit_provider_library(&ctx).is_ok());
    host.opened(&pending, None, "fixture complete");
}

#[test]
fn a_dialog_created_after_the_read_request_is_rejected_without_mutation() {
    let (_photos, mut app, host, ctx) = fixture("provider-dialog");
    let pending = request(&mut app, &host);
    app.ui.dialog = Some(crate::state::Dialog::Settings { tab: "general".into() });
    let ui = serde_json::to_value(&app.ui).unwrap();
    let active = app.session.active();
    let generation = app.session.library_generation();
    deliver(&host, &pending);
    assert!(app.can_admit_provider_library(&ctx).is_err());
    assert_eq!(serde_json::to_value(&app.ui).unwrap(), ui);
    assert_eq!(app.session.active(), active);
    assert_eq!(app.session.library_generation(), generation);
    app.ui.dialog = None;
    assert!(app.can_admit_provider_library(&ctx).is_ok());
    host.opened(&pending, None, "fixture complete");
}
