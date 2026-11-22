//! Software updates, background check at startup, and startup prompt modal.

use serde_json::Value;

use crate::LightcraftApp;

#[cfg(not(target_arch = "wasm32"))]
use craft_update::dialog::{CheckResultDialog, CheckResultResponse};
#[cfg(not(target_arch = "wasm32"))]
use craft_update::updater::{Action, Status, Updater};

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
            updater: Updater::new(&crate::UPDATE_APP, env!("CARGO_PKG_VERSION")),
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
    ///
    /// Distro packages set `LIGHTCRAFT_DISABLE_UPDATES`, which keeps this off: the package
    /// manager owns the update flow, so the startup prompt would only nag.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_startup_update_check(&mut self, on: bool) {
        self.updates_rt.startup_check = on && !updates_disabled();
    }

    /// Starts the helper of a ready update, which installs it once the process has exited. Call it
    /// in `on_exit` when the process ends there without dropping the app.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn start_pending_update(&mut self) {
        self.updates_rt.updater.start_pending_update();
    }
}

/// Whether every update check and prompt is disabled for this process.
///
/// Distro packages (NixOS, deb/rpm, …) own the update flow: the system state updates the
/// software, so LightCraft's own startup prompt and manual check would only nag. Setting
/// `LIGHTCRAFT_DISABLE_UPDATES` to `1`/`on`/`true`/`yes` turns all of them off; `Help ▸ Check
/// for Updates…` is greyed out and reports why when run another way.
pub(crate) fn updates_disabled() -> bool {
    disabled_by_value(std::env::var("LIGHTCRAFT_DISABLE_UPDATES").ok().as_deref())
}

/// [`updates_disabled`] for a known variable value, without reading the environment (tests).
fn disabled_by_value(value: Option<&str>) -> bool {
    value.is_some_and(|v| matches!(v.trim(), "1" | "on" | "true" | "yes"))
}

/// The update prompt owns the keyboard while it is open: shortcuts must not reach the library behind it.
pub(crate) fn blocks_input(app: &LightcraftApp) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        !updates_disabled() && app.updates_rt.updater.prompt_open()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = app;
        false
    }
}

pub(crate) fn check(app: &mut LightcraftApp) -> Result<Value, String> {
    if updates_disabled() {
        return Err(crate::i18n::tr("Updates are disabled (LIGHTCRAFT_DISABLE_UPDATES is set; your package manager should handle updates).").to_owned());
    }

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
    if updates_disabled() {
        return;
    }

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
        if updates_disabled() {
            return;
        }

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
            let dialog = CheckResultDialog::new(&crate::UPDATE_APP, env!("CARGO_PKG_VERSION"));
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

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn startup_prompt_shows_and_closes() {
        let mut app = LightcraftApp::new(Session::new(), Default::default());
        let ctx = egui::Context::default();
        let release = craft_update::Release {
            tag: "v99.0.0".into(),
            version: craft_update::parse_version("99.0.0"),
            name: "LightCraft 99.0.0".into(),
            html_url: "https://github.com/storytold/lightcraft/releases/tag/v99.0.0".into(),
            ..Default::default()
        };
        // A missing pins file means stable releases, whatever the launcher on this machine chose.
        let no_pins = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("no-launcher-pins.json");
        app.updates_rt.updater = Updater::new(&crate::UPDATE_APP, env!("CARGO_PKG_VERSION")).with_pins_path(no_pins);
        offer_and_wait(&mut app.updates_rt.updater, release);

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

    #[test]
    fn disable_updates_parsing() {
        for on in ["1", "on", "true", "yes", "  yes "] {
            assert!(disabled_by_value(Some(on)), "{on:?}");
        }

        for off in [None, Some(""), Some("  "), Some("0"), Some("false"), Some("off"), Some("no"), Some("2")] {
            assert!(!disabled_by_value(off), "{off:?}");
        }
    }

    /// The menu entry follows the environment, whatever it is (tests never mutate it:
    /// `std::env::set_var` is unsafe and forbidden here, so run the suite both with and
    /// without `LIGHTCRAFT_DISABLE_UPDATES=1`).
    #[test]
    fn check_for_updates_entry_follows_the_env() {
        let app = LightcraftApp::new(Session::new(), Default::default());
        assert_eq!(crate::menus::ui_enabled(&app, "app.checkForUpdates"), !updates_disabled());
    }

    /// Only runs with the variable set; the default suite covers the enabled path.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn disabled_check_reports_and_tick_starts_nothing() {
        if !updates_disabled() {
            return;
        }

        let mut app = LightcraftApp::new(Session::new(), Default::default());
        assert!(check(&mut app).is_err(), "a disabled manual check reports instead of checking");
        assert!(!app.updates_rt.check_requested);

        app.set_startup_update_check(true);
        assert!(!app.updates_rt.startup_check, "the setter keeps the startup check off");

        app.updates_rt.check_requested = true;
        tick(&mut app, &egui::Context::default());
        assert!(app.updates_rt.check_requested, "a disabled tick must not consume the request");
        assert_eq!(app.updates_rt.updater.status(), &Status::Idle);
    }
}
