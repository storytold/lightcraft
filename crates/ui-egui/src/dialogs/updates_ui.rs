//! Software updates, background check at startup, and startup prompt modal.
//!
//! Nightly is updated by the ArtCraft Launcher, never by this app's stable release feed: every entry
//! point below returns early on [`crate::channel::is_nightly`], so no check runs, no result dialog
//! opens and a ready stable update is never installed under a Nightly build.

use serde_json::Value;

use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _};
use craft::ui::dialog::Dialog;
#[cfg(not(target_arch = "wasm32"))]
use craft::ui::display::Spinner;
use craft::ui::display::Text;
#[cfg(not(target_arch = "wasm32"))]
use craft::ui::sections::{DialogRole, FooterButton};

use crate::LightcraftApp;

#[cfg(not(target_arch = "wasm32"))]
use craft_update::dialog::CheckResultResponse;
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
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_startup_update_check(&mut self, on: bool) {
        self.updates_rt.startup_check = on;
    }

    /// Starts the helper of a ready update, which installs it once the process has exited. Call it
    /// in `on_exit` when the process ends there without dropping the app.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn start_pending_update(&mut self) {
        if crate::channel::is_nightly() {
            return;
        }
        self.updates_rt.updater.start_pending_update();
    }
}

/// Modal update prompts and results own the keyboard; the installed-version notice stays modeless.
pub(crate) fn blocks_input(app: &LightcraftApp) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        !crate::channel::is_nightly()
            && (app.updates_rt.updater.prompt_open() || !matches!(app.updates_rt.updater.status(), Status::Idle | Status::Updated(_)))
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
        if crate::channel::is_nightly() {
            // Also keeps a command-palette or shortcut invocation from asking the stable feed.
            return Err("LightCraft Nightly is updated by the ArtCraft Launcher".into());
        }
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
    if craft::ui::runtime::measure::active(ctx) {
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        if crate::channel::is_nightly() {
            return;
        }
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

/// The startup offer stays with craft-update. Only the app's requested-check result is adapted
/// here: retries queue a check for `tick`, and a restart still follows the updater's unsaved guard.
#[cfg(not(target_arch = "wasm32"))]
fn show_result(ctx: &egui::Context, status: &Status) -> CheckResultResponse {
    use craft::ui::contract::overlay::Outside;

    if matches!(status, Status::Idle) || craft::ui::runtime::measure::active(ctx) {
        return CheckResultResponse::None;
    }
    let choices = result_choices(status);
    let buttons = choices.iter().map(|(label, _, _)| FooterButton::new((*label).to_owned(), DialogRole::Other));
    let notice = matches!(status, Status::Updated(_));
    let mut answer = CheckResultResponse::None;
    crate::dialogs::host_ui(ctx, egui::Id::unique("lightcraft.updates.host"), |ui| {
        let (output, record) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("lightcraft.updates.inspect"), |ui| {
            let dialog = Dialog::new(crate::i18n::tr("LightCraft Updates").to_owned())
                .id_salt("lightcraft.updates.result")
                .opened(Bind::given(true))
                .buttons(buttons)
                .overlay(|mut overlay| {
                    // The old result card dismissed on an outside click, except the modeless
                    // installed-version notice. No update operation is canceled by dismissing it.
                    overlay.dismiss = overlay.dismiss.outside(Outside::Keep);
                    overlay
                });
            let dialog = if notice { dialog.modeless() } else { dialog };
            let shown = dialog.show(ui, |ui, _| result_body(ui, status));
            if !ui.is_sizing_pass() {
                if let Some((_, _, choice)) = shown.output.picked.and_then(|at| choices.get(at)) {
                    answer = *choice;
                } else if shown.output.closed.is_some() || (!notice && outside_click(ui, &shown.output.response)) {
                    answer = CheckResultResponse::Close;
                }
                crate::widgets::register_response("dialog:updates", shown.output.response.rect, &shown.output.response);
            }
            shown.output
        });
        // Footer hit areas come from real SDK controls, including their clip and layer. They
        // are not invisible proxy widgets, and measurement publishes no automation targets.
        if !ui.is_sizing_pass()
            && let Some(record) = record
        {
            for element in &record.elements {
                let Some(facts) = &element.facts else {
                    continue;
                };
                if facts.role != "Button" {
                    continue;
                }
                let Some((_, id, _)) = choices.iter().find(|(label, _, _)| facts.name.as_deref() == Some(*label)) else {
                    continue;
                };
                let Some(hit) = record.hits.iter().find(|hit| hit.owner == element.id && hit.part == element.part) else {
                    continue;
                };
                let mut response = output.response.clone();
                response.id = element.id;
                response.rect = hit.rect;
                response.interact_rect = hit.rect;
                crate::widgets::register_response(*id, hit.rect, &response);
            }
        }
    });
    answer
}

/// A primary click outside the result dialog: pressed outside it (the framework's modal takes that press from the
/// window below and reports it to the dialog) and released in a later pass, as the old result card closed.
#[cfg(not(target_arch = "wasm32"))]
fn outside_click(ui: &egui::Ui, dialog: &egui::Response) -> bool {
    let key = egui::Id::unique("lightcraft.updates.outside-press");
    let rect = dialog.rect;
    let pressed = craft::ui::runtime::overlay::pressed_outside(ui.ctx(), dialog.id)
        || ui.input(|input| input.pointer.primary_pressed() && input.pointer.press_origin().is_some_and(|at| !rect.contains(at)));
    if pressed {
        ui.data_mut(|d| d.insert_temp(key, true));
    }
    let released = ui.input(|input| {
        input.events.iter().any(|event| matches!(event, egui::Event::PointerButton { button: egui::PointerButton::Primary, pressed: false, pos, .. } if !rect.contains(*pos)))
    });
    released && !pressed && ui.data_mut(|d| d.remove_temp::<bool>(key)).unwrap_or(false)
}

#[cfg(not(target_arch = "wasm32"))]
fn result_choices(status: &Status) -> Vec<(&'static str, &'static str, CheckResultResponse)> {
    if matches!(status, Status::Checking | Status::Idle) {
        return Vec::new();
    }
    let mut choices = vec![(crate::i18n::tr("OK"), "button:updatesOk", CheckResultResponse::Close)];
    match status {
        Status::Failed(_) | Status::InstallFailed(_) => {
            choices.push((crate::i18n::tr("Check Again"), "button:updatesCheckAgain", CheckResultResponse::CheckAgain))
        }
        Status::Ready => choices.push((crate::i18n::tr("Restart to Update"), "button:updatesRestart", CheckResultResponse::RestartToUpdate)),
        _ => {}
    }
    choices
}

#[cfg(not(target_arch = "wasm32"))]
fn result_body(ui: &mut egui::Ui, status: &Status) {
    ui.set_width(400.0_f32.min(ui.ctx().content_rect().width() - 64.0).max(160.0));
    Text::new(format!("{} v{}", crate::UPDATE_APP.name, env!("CARGO_PKG_VERSION"))).show(ui);
    Text::new(format!("by {}", crate::UPDATE_APP.developer)).show(ui);
    match status {
        Status::Checking | Status::Installing => {
            ui.horizontal(|ui| {
                Spinner::new().show(ui);
                Text::new(
                    crate::i18n::tr(if matches!(status, Status::Checking) { "Checking for updates…" } else { "Installing update…" }).to_owned(),
                )
                .show(ui);
            });
        }
        Status::Failed(error) | Status::InstallFailed(error) => {
            Text::new(
                crate::i18n::tr(if matches!(status, Status::Failed(_)) {
                    "Could not check for updates."
                } else {
                    "The update could not be installed:"
                })
                .to_owned(),
            )
            .show(ui);
            // Keep arbitrary service errors scrollable and copyable; a long error cannot push
            // retry or dismissal off screen. SelectableText is the SDK's read-only control.
            egui::ScrollArea::vertical().id_salt("updatesError").max_height(160.0).show(ui, |ui| {
                craft::ui::text::SelectableText::new("Update error", error).multiline().show(ui);
            });
            if matches!(status, Status::Failed(_)) {
                Text::new(format!("All releases are listed at {}", crate::UPDATE_APP.releases_url())).show(ui);
            }
        }
        Status::Ready => {
            Text::new(craft_update::updater::READY_TEXT.replace("{app}", crate::UPDATE_APP.name)).show(ui);
        }
        Status::Managed => {
            Text::new(format!(
                "{} {} follows the version chosen in the launcher, so it does not update itself.",
                crate::UPDATE_APP.name,
                env!("CARGO_PKG_VERSION")
            ))
            .show(ui);
        }
        Status::Updated(version) => {
            Text::new(format!("{} updated to {version}.", crate::UPDATE_APP.name)).show(ui);
        }
        Status::UpToDate => {
            Text::new(format!("{} {} is the newest version available.", crate::UPDATE_APP.name, env!("CARGO_PKG_VERSION"))).show(ui);
        }
        Status::Idle => {}
    }
}

pub(crate) fn show(app: &mut LightcraftApp, ctx: &egui::Context) {
    // Measurement must not consume keys, dismiss outcomes, or start the retained updater's actions.
    if craft::ui::runtime::measure::active(ctx) {
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        if crate::channel::is_nightly() {
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
            match show_result(ctx, rt.updater.status()) {
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
    if app.updates_rt.manual_window_open && show_web_result(ctx, app.updates_rt.status_message.as_deref()) {
        app.updates_rt.manual_window_open = false;
    }
}

/// The browser's answer to a requested check, in a modeless framework dialog with one OK: true once it is dismissed
/// (OK, Escape or Enter).
#[cfg(target_arch = "wasm32")]
fn show_web_result(ctx: &egui::Context, message: Option<&str>) -> bool {
    use std::cell::Cell;
    let dismissed = Cell::new(false);
    crate::dialogs::host_ui(ctx, egui::Id::unique("lightcraft.updates.host"), |ui| {
        let ok = [crate::dialogs::FooterSpec {
            label: crate::i18n::tr("OK").to_owned(),
            name: "button:updatesWebOk".into(),
            enabled: true,
            primary: true,
        }];
        let output = Dialog::new(crate::i18n::tr("LightCraft Updates").to_owned())
            .id_salt("lightcraft.updates.web")
            .opened(Bind::given(true))
            .footer(|ui, _| {
                if crate::dialogs::footer_buttons(ui, "lightcraft.updates.web", &ok).is_some() {
                    dismissed.set(true);
                }
            })
            .close_on_enter(true)
            // the rest of the application stays reachable, as before
            .modeless()
            .show(ui, |ui, _| {
                if let Some(message) = message {
                    Text::new(message.to_owned()).show(ui);
                }
            })
            .output;
        if !craft::ui::runtime::measure::active(ctx) && output.closed.is_some() {
            dismissed.set(true);
        }
    });
    dismissed.get()
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "updates_external155_tests.rs"]
mod updates_external155_tests;

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
}
