//! Things the user must not miss (issue #103): settings files that were damaged or unreadable when
//! the library opened (and host warnings such as a damaged `ui.json`), and quitting while changes
//! are only in memory or while tasks that would be cut short are running (issue #345).

use egui::{Align2, RichText, vec2};

use crate::theme::Tokens;
use crate::widgets::register;
use crate::{LightcraftApp, QuitBackup, QuitPrompt};

/// Collect the library's settings-file warnings (each is shown once), and the no-GPU notice once the GPU device has
/// been tried.
pub fn logic(app: &mut LightcraftApp) {
    let new = app.session.take_library_warnings();
    app.notices.extend(new);
    use lightcraft_engine::gpu;
    if app.gpu_notice && app.gpu_applied == Some(true) && gpu::ready() {
        app.gpu_notice = false;
        if !gpu::available() {
            app.notices.extend(gpu_notice(gpu::unavailable_reason().as_deref(), app.ui.settings.gpu));
        }
    }
}

/// What to tell the user when GPU rendering is on (`enabled`) but the GPU could not be used for `reason`: only when no
/// graphics adapter was found at all ("no GPU adapter among …", e.g. no Vulkan loader on Linux), which makes every
/// render much slower without saying so. Nothing when it was turned off (preference, `LIGHTCRAFT_GPU`), a software
/// adapter was skipped, or it failed later; Settings ▸ Performance and Help ▸ System Info show those.
pub fn gpu_notice(reason: Option<&str>, enabled: bool) -> Option<String> {
    if !enabled || !reason?.starts_with("no GPU adapter") {
        return None;
    }
    let mut text =
        crate::i18n::tr("No graphics processor found: LightCraft renders on the CPU, which is much slower. Details in Help → System Info.")
            .to_string();
    if cfg!(target_os = "linux") {
        text.push(' ');
        text.push_str(crate::i18n::tr("On NixOS, start it from `nix develop`."));
    }
    Some(text)
}

/// The window may close now (`true`), or the quit prompt is shown (`false`). Running tasks that
/// can be cancelled (an import, an export…) are asked about first; then the library is backed
/// up where that was chosen for quitting; then changes that couldn't be written are retried —
/// the log, then a snapshot of everything in memory.
pub fn may_close(app: &mut LightcraftApp) -> bool {
    if app.quit_confirmed {
        return true;
    }
    // before the library is touched: a Cancel here keeps working with it
    let running = tasks_quitting_would_stop(app);
    if !running.is_empty() {
        let names = running.iter().map(|t| crate::i18n::tr(&t.label)).collect::<Vec<_>>().join(", ");
        app.quit_prompt = Some(QuitPrompt::Tasks(if running.len() == 1 {
            crate::i18n::tr_format!("1 task is still running: {names}. Quitting stops it.", names = names)
        } else {
            crate::i18n::tr_format!("{n} tasks are still running: {names}. Quitting stops them.", n = running.len(), names = names)
        }));
        return false;
    }
    if !backed_up_for_quitting(app) {
        return false;
    }
    if app.session.unsaved().is_none() {
        return true;
    }
    let _ = app.session.persist();
    if app.session.unsaved().is_some()
        && let Err(e) = app.session.close_library()
    {
        log::error!("quit: library not saved: {e}");
    }
    match app.session.unsaved() {
        None => true,
        Some((n, e)) => {
            app.quit_prompt = Some(QuitPrompt::Unsaved(crate::i18n::tr_format!(
                "{n} change{} couldn't be written to disk: {e}\nIf you quit now, {} lost.",
                if n == 1 { "" } else { "s" },
                if n == 1 { "it is" } else { "they are" },
                n = n,
                e = e
            )));
            false
        }
    }
}

/// The library has a backup to make when LightCraft quits (`library.backupPreferences`,
/// `onClose`), and this quit has not made, skipped or given it up yet.
fn backup_is_ahead(app: &LightcraftApp) -> bool {
    use lightcraft_engine::backup_schedule::BackupOnClose;
    app.quit_backup != QuitBackup::Settled
        && app.session.backup_schedule.on_close != BackupOnClose::Never
        // (a library that is not a folder on disk has no backup to make)
        && lightcraft_engine::cmd::backup::backup_folder(&app.session).is_some()
}

/// The tasks quitting would cut short. A backup of the open library that is under way is not
/// one of them while this quit has its own backup ahead: it is the one the quit waits for. (A
/// backup of another library, still running after a switch, is asked about like any task.)
fn tasks_quitting_would_stop(app: &LightcraftApp) -> Vec<lightcraft_engine::activity::TaskInfo> {
    let mut running = app.session.activity.running_cancellable();
    if backup_is_ahead(app) && backup_under_way(app) {
        let awaited = lightcraft_engine::cmd::backup::status(&app.session).and_then(|b| app.session.activity.id_of(&b.cancel));
        running.retain(|t| Some(t.id) != awaited);
    }
    running
}

/// The backup the library has when LightCraft quits, if it has one: `true` once there is
/// nothing more to wait or ask for. Asked about, or started, here; [`quit_after_backup`] goes on
/// with the quit when the backup has ended.
fn backed_up_for_quitting(app: &mut LightcraftApp) -> bool {
    use lightcraft_engine::backup_schedule::BackupOnClose;
    if !backup_is_ahead(app) {
        return true;
    }
    match app.quit_backup {
        QuitBackup::Settled => true,
        QuitBackup::Running => {
            waiting_for_the_backup(app);
            false
        }
        QuitBackup::Undecided if app.session.backup_schedule.on_close == BackupOnClose::Ask => {
            app.quit_prompt = Some(QuitPrompt::BackUp(crate::i18n::tr("Back up the library before LightCraft quits?").to_string()));
            false
        }
        QuitBackup::Undecided => {
            back_up_then_quit(app);
            false
        }
    }
}

/// A backup of the open library that is running and was not told to stop.
fn backup_under_way(app: &LightcraftApp) -> bool {
    use std::sync::atomic::Ordering;
    lightcraft_engine::cmd::backup::status(&app.session).is_some_and(|b| !b.finished.load(Ordering::Relaxed) && !b.cancel.load(Ordering::Relaxed))
}

/// Wait for the backup LightCraft quits after: one that runs already, else one started now. One
/// that can't be started is a backup that failed ([`backup_failed`]).
fn back_up_then_quit(app: &mut LightcraftApp) {
    if !backup_under_way(app)
        && let Err(e) = app.run("library.backup", serde_json::json!({}))
    {
        backup_failed(app, &e);
        return;
    }
    app.quit_backup = QuitBackup::Running;
    waiting_for_the_backup(app);
}

fn waiting_for_the_backup(app: &mut LightcraftApp) {
    app.quit_prompt = Some(QuitPrompt::BackingUp(crate::i18n::tr("Backing up the library. LightCraft quits when the backup is made.").to_string()));
}

/// The backup before quitting was not made: the person is asked whether to quit all the same.
/// (Never a quit that can't be made because a drive is unplugged, and never one that goes ahead
/// as if the library had been backed up.)
fn backup_failed(app: &mut LightcraftApp, why: &str) {
    app.quit_backup = QuitBackup::Undecided;
    app.quit_prompt = Some(QuitPrompt::BackupFailed(crate::i18n::tr_format!("The library was not backed up: {}", why)));
}

/// The quit was called off: the next one starts over, with its own question about the backup.
fn quit_called_off(app: &mut LightcraftApp) {
    app.quit_prompt = None;
    app.quit_backup = QuitBackup::Undecided;
}

/// Go on with a quit that waits for its backup ([`QuitBackup::Running`]) once the backup has
/// ended: made, LightCraft quits (after whatever else quitting asks); not made, the person is
/// asked ([`backup_failed`]).
pub fn quit_after_backup(app: &mut LightcraftApp) {
    if app.quit_backup != QuitBackup::Running {
        return;
    }
    let Some(job) = lightcraft_engine::cmd::backup::status(&app.session).cloned() else {
        // no backup at all: the library was closed under the quit
        quit_called_off(app);
        return;
    };
    let Some(outcome) = job.outcome() else { return };
    app.quit_prompt = None;
    match outcome {
        Ok(_) => {
            app.quit_backup = QuitBackup::Settled;
            if may_close(app) {
                app.ui.quit = true;
            }
        }
        Err(e) => backup_failed(app, &e),
    }
}

enum Choice {
    Retry,
    QuitAnyway,
    Cancel,
    Ok,
    BackUp,
    Skip,
}

fn window(ctx: &egui::Context, id: &str, title: &str, text: &str, buttons: &[(&str, &str, Choice)]) -> Option<usize> {
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::new((id, "dim"))).order(egui::Order::Middle).fixed_pos(screen.min).show(ctx, |ui| {
        ui.allocate_rect(screen, egui::Sense::click_and_drag());
        ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(140));
    });
    let mut chosen = None;
    let frame = egui::Frame::window(&ctx.global_style()).inner_margin(egui::Margin::symmetric(18, 14));
    egui::Window::new(crate::i18n::tr(title))
        .id(egui::Id::new(id))
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .frame(frame)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(440.0)
        .show(ctx, |ui| {
            // a fixed width: a wrapped label in an auto-sized, centred window would move it every frame
            ui.set_width(420.0);
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.add(egui::Label::new(RichText::new(text).color(t.text_label)).wrap());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                for (i, (bid, label, _)) in buttons.iter().enumerate() {
                    let r = ui.add(egui::Button::new(crate::i18n::tr(label)).min_size(vec2(0.0, 26.0)));
                    register(ui.ctx(), format!("button:{bid}"), r.rect);
                    if r.clicked() {
                        chosen = Some(i);
                    }
                }
            });
        });
    chosen
}

/// Quit without the backup this quit had ahead (skipped, not waited for, or not made): nothing
/// more is asked about it, and the rest of quitting goes on.
fn quit_without_the_backup(app: &mut LightcraftApp) {
    app.quit_backup = QuitBackup::Settled;
    if may_close(app) {
        app.ui.quit = true;
    }
}

pub fn show(app: &mut LightcraftApp, ctx: &egui::Context) {
    if let Some(QuitPrompt::BackUp(text)) = app.quit_prompt.clone() {
        let buttons = [("quitBackUp", "Back Up", Choice::BackUp), ("quitSkipBackup", "Skip", Choice::Skip), ("quitCancel", "Cancel", Choice::Cancel)];
        let Some(i) = window(ctx, "quit-backup", "Back up before quitting?", &text, &buttons) else { return };
        app.quit_prompt = None;
        match buttons[i].2 {
            Choice::BackUp => back_up_then_quit(app),
            Choice::Skip => quit_without_the_backup(app),
            _ => quit_called_off(app),
        }
        return;
    }
    if let Some(QuitPrompt::BackingUp(text)) = app.quit_prompt.clone() {
        let buttons = [("quitNoWait", "Quit Without Waiting", Choice::QuitAnyway), ("quitCancel", "Cancel", Choice::Cancel)];
        let Some(i) = window(ctx, "quit-backing-up", "Backing up before quitting", &text, &buttons) else { return };
        app.quit_prompt = None;
        match buttons[i].2 {
            Choice::QuitAnyway => {
                // (the backup is stopped, and leaves nothing half made)
                if let Some(b) = lightcraft_engine::cmd::backup::status(&app.session) {
                    b.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                quit_without_the_backup(app);
            }
            // back to work: the backup goes on, and the quit is called off
            _ => quit_called_off(app),
        }
        return;
    }
    if let Some(QuitPrompt::BackupFailed(text)) = app.quit_prompt.clone() {
        let buttons = [("quitAnyway", "Quit Anyway", Choice::QuitAnyway), ("quitCancel", "Cancel", Choice::Cancel)];
        let Some(i) = window(ctx, "quit-backup-failed", "Quit without a backup?", &text, &buttons) else { return };
        app.quit_prompt = None;
        match buttons[i].2 {
            Choice::QuitAnyway => quit_without_the_backup(app),
            _ => quit_called_off(app),
        }
        return;
    }
    if let Some(QuitPrompt::Tasks(text)) = app.quit_prompt.clone() {
        let buttons = [("quitAnyway", "Quit Anyway", Choice::QuitAnyway), ("quitCancel", "Cancel", Choice::Cancel)];
        let Some(i) = window(ctx, "quit-tasks", "Quit while tasks are running?", &text, &buttons) else { return };
        app.quit_prompt = None;
        if matches!(buttons[i].2, Choice::QuitAnyway) {
            // (not the backup this quit waits for: see `tasks_quitting_would_stop`)
            for task in tasks_quitting_would_stop(app) {
                // (one that ended meanwhile has nothing left to stop)
                let _ = app.session.activity.cancel(task.id);
            }
            // the backup and unsaved changes still get their own questions
            if may_close(app) {
                app.ui.quit = true;
            }
        } else {
            quit_called_off(app);
        }
        return;
    }
    if let Some(QuitPrompt::Unsaved(text)) = app.quit_prompt.clone() {
        let buttons = [
            ("quitRetry", "Try Saving Again", Choice::Retry),
            ("quitAnyway", "Quit Anyway", Choice::QuitAnyway),
            ("quitCancel", "Cancel", Choice::Cancel),
        ];
        let Some(i) = window(ctx, "quit-unsaved", "Quit with unsaved changes?", &text, &buttons) else { return };
        match buttons[i].2 {
            Choice::Retry => {
                app.quit_prompt = None;
                if may_close(app) {
                    app.ui.quit = true;
                }
            }
            Choice::QuitAnyway => {
                app.quit_prompt = None;
                app.quit_confirmed = true;
                app.ui.quit = true;
            }
            Choice::Cancel | Choice::Ok | Choice::BackUp | Choice::Skip => quit_called_off(app),
        }
        return;
    }
    if let Some(text) = app.notices.first().cloned() {
        let buttons = [("noticeOk", "OK", Choice::Ok)];
        if window(ctx, "settings-notice", "Settings file problem", &text, &buttons).is_some() {
            app.notices.remove(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::gpu_notice;

    #[test]
    fn the_gpu_notice_is_only_for_a_missing_adapter() {
        let none = "no GPU adapter among Backends(VULKAN | GL) (No suitable graphics adapter found)";
        let text = gpu_notice(Some(none), true).expect("no adapter: say so");
        assert!(text.starts_with("No graphics processor found") && text.contains("System Info"), "{text}");
        assert_eq!(text.contains("nix develop"), cfg!(target_os = "linux"), "{text}");
        assert_eq!(gpu_notice(Some(none), false), None, "the preference is off");
        for reason in [
            "disabled by LIGHTCRAFT_GPU=0",
            "disabled by LIGHTCRAFT_GPU_BACKEND=off",
            "disabled by the GPU rendering preference (app.gpu)",
            "software adapter (llvmpipe (LLVM 19.1.7, 256 bits)) skipped: the CPU pipeline is faster",
            "stopped after a GPU failure: device lost",
        ] {
            assert_eq!(gpu_notice(Some(reason), true), None, "{reason}");
        }
        assert_eq!(gpu_notice(None, true), None, "the GPU is in use");
    }
}
