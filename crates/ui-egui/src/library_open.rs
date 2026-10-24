//! Opening a library from the menus or the Open Library dialog (issue #2718). A large library
//! takes a while to open, and opening it in the frame of the click left that frame's menu on screen
//! with no word of what was happening, as if the app had hung. From those places the open waits:
//! the window first shows "Opening library “…”…" over everything, with the menu closed, and the
//! library opens at the start of a later frame. `app.openLibrary` itself (CLI, MCP, the control
//! channel) still opens the library before it answers.

use serde_json::Value;

use crate::LightcraftApp;

/// A library to open once its message has been on screen.
pub struct Pending {
    /// The `app.openLibrary` parameters (`path` set).
    pub params: Value,
    /// "Opening library “name”…", in the UI language.
    pub message: String,
    /// Frames drawn with the message.
    painted: u8,
}

/// Frames the message is drawn before the library opens: the frame of the click still draws the
/// menu that was clicked, the next one only the message.
const FRAMES_SHOWN: u8 = 2;

/// `app.openLibrary {path}` from a menu item or a folder dialog's answer: wait to open it until the
/// window says so → `Some(Ok(null))`. `None` for any other command (run it as usual).
pub fn later(app: &mut LightcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let path = params.get("path").and_then(Value::as_str).filter(|_| id == "app.openLibrary")?;
    let name = crate::panels::settings::library_name(std::path::Path::new(path));
    let message = crate::i18n::tr_format!("Opening library “{name}”…", name = name);
    app.opening_library = Some(Pending { params: params.clone(), message, painted: 0 });
    Some(Ok(Value::Null))
}

/// At the start of a frame: open the waiting library once its message has been shown; a refusal
/// or failure is a toast (the current library stays open).
pub fn run_pending(app: &mut LightcraftApp, ctx: &egui::Context) {
    if !app.opening_library.as_ref().is_some_and(|p| p.painted >= FRAMES_SHOWN) {
        return;
    }
    let Some(pending) = app.opening_library.take() else { return };
    if let Err(e) = app.run("app.openLibrary", pending.params) {
        app.toast_error(ctx, e);
    }
    ctx.request_repaint();
}

/// Draw the message of a waiting open over the whole window (which takes no input meanwhile).
pub fn show(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(pending) = app.opening_library.as_mut() else { return };
    let t = crate::theme::Tokens::get(ctx);
    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::new("library-opening")).order(egui::Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
        let (all, _) = ui.allocate_exact_size(screen.size(), egui::Sense::click_and_drag());
        let p = ui.painter();
        p.rect_filled(all, 0.0, egui::Color32::from_black_alpha(120));
        let galley = p.layout(pending.message.clone(), t.font(15.0), t.text, (all.width() - 80.0).clamp(200.0, 720.0));
        let r = egui::Rect::from_center_size(all.center(), egui::vec2(galley.size().x + 48.0, galley.size().y + 32.0));
        p.rect_filled(r, 8.0, t.chrome);
        p.rect_stroke(r, 8.0, egui::Stroke::new(1.0, t.divider), egui::StrokeKind::Inside);
        p.galley(r.center() - galley.size() / 2.0, galley, t.text);
        crate::widgets::register(ctx, "libraryOpening", r);
    });
    pending.painted = pending.painted.saturating_add(1);
    ctx.request_repaint();
}
