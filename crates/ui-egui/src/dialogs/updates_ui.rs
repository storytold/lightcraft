//! Software updates, background check at startup, and startup prompt modal.

use serde_json::Value;

use crate::LightcraftApp;

#[cfg(not(target_arch = "wasm32"))]
use craft_update::dialog::{CheckResultDialog, CheckResultResponse};
#[cfg(not(target_arch = "wasm32"))]
use craft_update::updater::{Action, Status, Updater};

/// craft-update's words in the interface language. Its English text is the catalog key, as for
/// [`crate::i18n::tr`]; the `{app}`-style placeholders are filled after the lookup, so a
/// translation may put them anywhere in its sentence.
#[cfg(not(target_arch = "wasm32"))]
fn translate(text: &str, args: &[(&str, &str)]) -> String {
    craft_update::translate::fill(crate::i18n::tr(text), args)
}

pub(crate) struct Runtime {
    #[cfg(not(target_arch = "wasm32"))]
    updater: Updater,
    /// The desktop app turns the check at start on; tests and the web build leave it off.
    #[cfg(not(target_arch = "wasm32"))]
    startup_check: bool,
    #[cfg(not(target_arch = "wasm32"))]
    check_requested: bool,
    /// The context of the last frame, to wake the UI when a check is queued from a menu callback.
    #[cfg(not(target_arch = "wasm32"))]
    repaint: Option<egui::Context>,
    /// A requested check's result dialog is showing.
    #[cfg(target_arch = "wasm32")]
    pub(crate) manual_window_open: bool,
    #[cfg(target_arch = "wasm32")]
    pub(crate) status_message: Option<String>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            updater: Updater::new(&crate::UPDATE_APP, env!("CARGO_PKG_VERSION")).with_translator(translate),
            #[cfg(not(target_arch = "wasm32"))]
            startup_check: false,
            #[cfg(not(target_arch = "wasm32"))]
            check_requested: false,
            #[cfg(not(target_arch = "wasm32"))]
            repaint: None,
            #[cfg(target_arch = "wasm32")]
            manual_window_open: false,
            #[cfg(target_arch = "wasm32")]
            status_message: None,
        }
    }
}

impl LightcraftApp {
    /// Whether the app asks for a newer release when it starts (off until the desktop app turns it on).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_startup_update_check(&mut self, on: bool) {
        self.updates_rt.startup_check = on;
    }

    /// Starts the helper of a ready update, which installs it once the process has exited. Call it
    /// in `on_exit` when the process ends there without dropping the app.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn start_pending_update(&mut self) {
        self.updates_rt.updater.start_pending_update();
    }
}

/// The update prompt owns the keyboard while it is open: shortcuts must not reach the library behind it.
pub(crate) fn blocks_input(app: &LightcraftApp) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        app.updates_rt.updater.prompt_open()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = app;
        false
    }
}

pub(crate) fn check(app: &mut LightcraftApp) -> Result<Value, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        app.updates_rt.check_requested = true;
        if let Some(ctx) = &app.updates_rt.repaint {
            ctx.request_repaint();
        }
        Ok(Value::Null)
    }
    #[cfg(target_arch = "wasm32")]
    {
        app.updates_rt.manual_window_open = true;
        app.updates_rt.status_message = Some("Update check is not supported in the web browser.".into());
        Ok(Value::Null)
    }
}

pub(crate) fn tick(app: &mut LightcraftApp, ctx: &egui::Context) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let rt = &mut app.updates_rt;
        rt.repaint = Some(ctx.clone());
        if std::mem::take(&mut rt.check_requested) {
            rt.updater.check_now(ctx);
        } else if rt.startup_check {
            rt.updater.check_at_start(ctx);
        }
        rt.updater.tick(ctx);
    }
    #[cfg(target_arch = "wasm32")]
    let _ = (app, ctx);
}

/// Whether the result dialog shows: for every status but idle (a requested check, and the first
/// start after an update, which reports that it installed or why it failed), and never over the
/// startup prompt.
#[cfg(not(target_arch = "wasm32"))]
fn result_window_shows(status: &Status, prompt_open: bool) -> bool {
    !prompt_open && !matches!(status, Status::Idle)
}

pub(crate) fn show(app: &mut LightcraftApp, ctx: &egui::Context) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        // The close for a ready update waits until nothing is unsaved.
        let clean = app.session.unsaved().is_none();
        match app.updates_rt.updater.show_if(ctx, clean) {
            Action::OpenUrl(url) => ctx.open_url(egui::OpenUrl::new_tab(url)),
            Action::Close => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Action::None => {}
        }
        // A found update opens the startup prompt and leaves the status idle, which ends the result dialog.
        let rt = &mut app.updates_rt;
        if result_window_shows(rt.updater.status(), rt.updater.prompt_open()) {
            let dialog = CheckResultDialog::new(&crate::UPDATE_APP, env!("CARGO_PKG_VERSION")).with_translator(translate);
            match dialog.show(ctx, rt.updater.status()) {
                CheckResultResponse::None => {}
                // Closing the dialog dismisses the outcome it showed.
                CheckResultResponse::Close => rt.updater.dismiss_status(),
                CheckResultResponse::CheckAgain => rt.check_requested = true,
                // The next `show_if` asks the window to close once nothing is unsaved.
                CheckResultResponse::RestartToUpdate => rt.updater.restart_to_update(),
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    if app.updates_rt.manual_window_open {
        let mut open = true;
        let shown = egui::Window::new(crate::i18n::tr("LightCraft Updates"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .show(ctx, |ui| {
                if let Some(msg) = &app.updates_rt.status_message {
                    ui.label(msg);
                }
                ui.add_space(8.0);
                if ui.button(crate::i18n::tr("OK")).clicked() {
                    app.updates_rt.manual_window_open = false;
                }
            });
        if let Some(w) = &shown {
            crate::widgets::announce_dialog(ctx, &w.response, crate::widgets::Modal::Window);
        }
        if !open {
            app.updates_rt.manual_window_open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_engine::Session;

    #[test]
    fn default_runtime_is_idle() {
        let rt = Runtime::default();
        #[cfg(target_arch = "wasm32")]
        assert!(!rt.manual_window_open);
        #[cfg(not(target_arch = "wasm32"))]
        {
            assert!(!rt.startup_check);
            assert!(!rt.updater.prompt_open());
            assert_eq!(rt.updater.status(), &Status::Idle);
        }
    }

    /// Offers `release` and returns once its prompt is open. The updater reads the installed
    /// files for an offer on a thread of its own. That thread sends its answer and then asks the
    /// first context the updater was ticked with for a repaint, so the tick after that repaint
    /// reads the answer and opens the prompt. `updater` must not have been ticked before, or the
    /// repaint goes to another context. The test fails when the launcher manages this copy or
    /// when the answered offer opens no prompt.
    #[cfg(not(target_arch = "wasm32"))]
    fn offer_and_wait(updater: &mut Updater, release: craft_update::Release) {
        let ctx = egui::Context::default();
        let (answered, wait) = std::sync::mpsc::channel();
        ctx.set_repaint_observer(move |info| {
            if info.delay.is_zero() {
                let _ = answered.send(());
            }
        });
        // Keeps `ctx` for the offer's thread.
        updater.tick(&ctx);
        updater.offer(release);
        assert!(updater.managed().is_none(), "the launcher manages this copy, so it ignored the offer");
        if !updater.prompt_open() {
            wait.recv().unwrap();
            updater.tick(&ctx);
        }
        assert!(updater.prompt_open(), "the answered offer opened no prompt");
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn release_99() -> craft_update::Release {
        craft_update::Release {
            tag: "v99.0.0".into(),
            version: craft_update::parse_version("99.0.0"),
            name: "LightCraft 99.0.0".into(),
            html_url: "https://github.com/storytold/lightcraft/releases/tag/v99.0.0".into(),
            ..Default::default()
        }
    }

    /// A missing pins file means stable releases, whatever the launcher on this machine chose.
    #[cfg(not(target_arch = "wasm32"))]
    fn no_pins() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("no-launcher-pins.json")
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn startup_prompt_shows_and_closes() {
        let mut app = LightcraftApp::new(Session::new(), Default::default());
        let ctx = egui::Context::default();
        app.updates_rt.updater = Updater::new(&crate::UPDATE_APP, env!("CARGO_PKG_VERSION")).with_pins_path(no_pins());
        offer_and_wait(&mut app.updates_rt.updater, release_99());

        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(&mut app, ui.ctx());
        });
        output.textures_delta.clear();
        assert!(app.updates_rt.updater.prompt_open());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn result_window_shows_every_status_but_idle() {
        let shown = [
            Status::Checking,
            Status::UpToDate,
            Status::Failed("no network".into()),
            Status::Installing,
            Status::Ready,
            Status::Managed,
            Status::Updated("0.2.0".into()),
            Status::InstallFailed("disk full".into()),
        ];
        for status in &shown {
            assert!(result_window_shows(status, false), "{status:?}");
            assert!(!result_window_shows(status, true), "{status:?} over the prompt");
        }
        assert!(!result_window_shows(&Status::Idle, false));
    }

    /// The text [`show`] paints once its card has been sized (egui hides an area's first frame).
    #[cfg(not(target_arch = "wasm32"))]
    fn painted(app: &mut LightcraftApp, view: &mut crate::headless::HeadlessView) -> String {
        for _ in 0..2 {
            view.run(egui::RawInput::default(), |ui| show(app, ui.ctx()));
        }
        view.painted_text().join("\n")
    }

    /// craft-update keeps its words in English; the startup prompt and the result dialog get
    /// them from the catalog, with the app name and version filled in after the lookup.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn the_update_dialogs_are_in_the_ui_language() {
        crate::i18n::set_language(crate::i18n::Locale::De);
        assert_eq!(
            translate("{app} updated to {version}.", &[("app", "LightCraft"), ("version", "0.2.0")]),
            "LightCraft wurde auf 0.2.0 aktualisiert."
        );

        let mut app = LightcraftApp::new(Session::new(), Default::default());
        let mut view = crate::headless::HeadlessView::new();
        // The runtime's own updater, so the test sees the translator `Runtime::default` passes.
        app.updates_rt.updater = Runtime::default().updater.with_pins_path(no_pins());
        offer_and_wait(&mut app.updates_rt.updater, release_99());
        let prompt = painted(&mut app, &mut view);
        assert!(prompt.contains("SPÄTER"), "{prompt}");

        // A requested check, its source standing in for the network and a missing pins file for
        // the launcher's (a version pinned there would end the check at once). `show` never
        // ticks the updater, so the dialog stays on "Checking".
        let source: craft_update::updater::CheckSource = std::sync::Arc::new(|| Ok(None));
        app.updates_rt.updater = Runtime::default().updater.with_source(source).with_pins_path(no_pins());
        app.updates_rt.updater.check_now(&view.ctx);
        let result = painted(&mut app, &mut view);
        crate::i18n::set_language(crate::i18n::Locale::En);
        assert!(result.contains("Suche nach Updates…"), "{result}");
    }
}
