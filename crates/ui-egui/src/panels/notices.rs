//! Things the user must not miss (issue #103): settings files that were damaged or unreadable when
//! the library opened (and host warnings such as a damaged `ui.json`), and quitting while changes
//! are only in memory or while tasks that would be cut short are running (issue #345).

use egui::{Align2, RichText, vec2};

use crate::theme::Tokens;
use crate::{LccApp, QuitPrompt};

/// Collect the library's settings-file warnings (each is shown once), the no-GPU notice once the GPU device has
/// been tried, and the missing-fonts notice once.
pub fn logic(app: &mut LccApp) {
    let new = app.session.take_library_warnings();
    app.notices.extend(new);
    let notices = app.session.take_notices();
    app.notices.extend(notices.iter().map(notice_text));
    if let Some(problems) = app.font_problems.take() {
        app.notices.extend(font_notice(problems));
    }
    use lcc_engine::gpu;
    if app.gpu_notice && app.gpu_applied == Some(true) && gpu::ready() {
        app.gpu_notice = false;
        if !gpu::available() {
            app.notices.extend(gpu_notice(gpu::unavailable_reason().as_deref(), app.ui.settings.gpu));
        }
    }
}

/// An engine notice in the user's language.
pub fn notice_text(n: &lcc_engine::notice::Notice) -> String {
    use lcc_engine::notice::Notice;
    match n {
        Notice::SettingsSetAside { file, kept } => crate::i18n::tr_format!(
            "{file} was damaged. It was moved aside as {kept}, and the settings of a new library are used.",
            file = file,
            kept = kept
        ),
        Notice::SettingsUnreadable { file, reason } => crate::i18n::tr_format!(
            "{file} couldn't be read ({reason}). The settings of a new library are used for now, and the file is not overwritten.",
            file = file,
            reason = reason
        ),
        Notice::SyncHeldFuture { device, minutes } => crate::i18n::tr_format!(
            "Changes from {device} are dated {minutes} minutes in the future, so they are held back until then. Set the clock on {device} to the right time.",
            device = device,
            minutes = minutes
        ),
    }
}

/// What to tell the user when the recent list of older versions (`recent-catalogs.json`) couldn't
/// be carried over (`why`, also in the log).
pub fn recent_catalogs_notice(why: &str) -> String {
    crate::i18n::tr_format!(
        "The recent library list of an older version couldn't be carried over ({why}). Open those libraries with File → Open Catalog…",
        why = why
    )
}

/// What to tell the user when fonts the package carries could not be read (`problems`, also in the log): which ones,
/// and that reinstalling brings them back. Text in their scripts shows as empty boxes; Latin text never needs them.
/// Nothing when every font loaded, or when they are built into the program.
pub fn font_notice(problems: &[lcc_engine::fonts::FontProblem]) -> Option<String> {
    if problems.is_empty() {
        return None;
    }
    let fonts = problems.iter().map(|problem| problem.what.as_str()).collect::<Vec<_>>().join(", ");
    Some(crate::i18n::tr_format!(
        "Some of {app}'s fonts are missing or damaged ({fonts}), so some text may show as empty boxes. Reinstall {app} to restore them.",
        fonts = fonts
    ))
}

/// What to tell the user when GPU rendering is on (`enabled`) but the GPU could not be used for `reason`: only when no
/// graphics adapter was found at all ("no GPU adapter among …", e.g. no Vulkan loader on Linux), which makes every
/// render much slower without saying so. Nothing when it was turned off (preference, `<PREFIX>_GPU`), a software
/// adapter was skipped, or it failed later; Settings ▸ Performance and Help ▸ System Info show those.
pub fn gpu_notice(reason: Option<&str>, enabled: bool) -> Option<String> {
    if !enabled || !reason?.starts_with("no GPU adapter") {
        return None;
    }
    let mut text =
        crate::i18n::tr("No graphics processor found: {app} renders on the CPU, which is much slower. Details in Help → System Info.").to_string();
    if cfg!(target_os = "linux") {
        text.push(' ');
        text.push_str(crate::i18n::tr("On NixOS, start it from `nix develop`."));
    }
    Some(text)
}

/// The window may close now (`true`), or the quit prompt is shown (`false`). Running tasks that
/// can be cancelled (an import, an export…) are asked about first; then changes that couldn't be
/// written are retried — the log, then a snapshot of everything in memory.
pub fn may_close(app: &mut LccApp) -> bool {
    if app.quit_confirmed {
        return true;
    }
    // before the library is touched: a Cancel here keeps working with it
    let running = app.session.activity.running_cancellable();
    if !running.is_empty() {
        let names = running.iter().map(crate::panels::activity::label_text).collect::<Vec<_>>().join(", ");
        app.quit_prompt = Some(QuitPrompt::Tasks(if running.len() == 1 {
            crate::i18n::tr_format!("1 task is still running: {names}. Quitting stops it.", names = names)
        } else {
            crate::i18n::tr_format!("{n} tasks are still running: {names}. Quitting stops them.", n = running.len(), names = names)
        }));
        return false;
    }
    // a catalog backup due: asked once, as Lightroom Classic asks when it closes (the web keeps no catalog folder);
    // nothing changed since the last backup, nothing to ask
    #[cfg(not(target_arch = "wasm32"))]
    if app.exit_backup.is_none() && app.session.catalog_backup_wanted() {
        app.quit_prompt =
            Some(QuitPrompt::Backup(crate::i18n::tr("A backup of the catalog is due. Back it up now, before {app} quits?").to_string()));
        return false;
    }
    if app.session.unsaved().is_none() {
        return true;
    }
    let _ = app.session.persist();
    if app.session.unsaved().is_some()
        && let Err(e) = app.session.close_library()
    {
        log::error!("quit: catalog not saved: {e}");
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

enum Choice {
    Retry,
    QuitAnyway,
    Cancel,
    Ok,
}

/// Where the backup schedule is set, as the menu bar shows it ("Disable backups in Edit ▸ Catalog Settings…"),
/// translated; `None` if the command has no menu path.
fn backup_hint() -> Option<String> {
    let (_, label, _, menu) = crate::menus::ui_commands().find(|c| c.0 == "dialog.catalogSettings")?;
    if menu.is_empty() {
        return None;
    }
    let path: Vec<&str> = menu.split('>').chain([*label]).map(crate::i18n::tr).collect();
    Some(crate::i18n::tr_format!("Disable backups in {path}", path = path.join(" ▸ ")))
}

/// A modal card: `text`, then `hint` (dimmer) when given, then the buttons.
fn window(ctx: &egui::Context, id: &str, title: &str, text: &str, hint: Option<&str>, buttons: &[(&str, &str, Choice)]) -> Option<usize> {
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::new((id, "dim"))).order(egui::Order::Middle).fixed_pos(screen.min).show(ctx, |ui| {
        ui.allocate_rect(screen, egui::Sense::click_and_drag());
        ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(140));
    });
    let mut chosen = None;
    let frame = crate::dialog_frame::card_frame(ctx);
    let window_id = egui::Id::new(id);
    let shown = egui::Window::new(crate::i18n::tr(title))
        .id(window_id)
        .title_bar(false)
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
            crate::dialog_frame::card_header(ui, crate::i18n::tr(title), None);
            let mut layout = crate::dialog_frame::ModalLayout::begin(ui, window_id);
            layout.body(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(text).color(t.text_label)).wrap());
                if let Some(hint) = hint {
                    ui.add(egui::Label::new(RichText::new(hint).color(t.text_dim)).wrap());
                }
            });
            layout.footer(ui, |ui| {
                ui.add_space(4.0);
                // (wrapping on a narrow window)
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                    for (i, (bid, label, _)) in buttons.iter().enumerate() {
                        if crate::button::Button::new(label).widget(format!("button:{bid}")).show(ui).clicked() {
                            chosen = Some(i);
                        }
                    }
                });
            });
        });
    crate::dialog_frame::ModalLayout::finish(ctx, window_id, shown.as_ref().map(|w| w.response.rect));
    chosen
}

pub fn show(app: &mut LccApp, ctx: &egui::Context) {
    if let Some(QuitPrompt::Tasks(text)) = app.quit_prompt.clone() {
        let buttons = [("quitAnyway", "Quit Anyway", Choice::QuitAnyway), ("quitCancel", "Cancel", Choice::Cancel)];
        let Some(i) = window(ctx, "quit-tasks", "Quit while tasks are running?", &text, None, &buttons) else { return };
        app.quit_prompt = None;
        if matches!(buttons[i].2, Choice::QuitAnyway) {
            app.session.activity.cancel_all();
            // unsaved changes still get their own question
            if may_close(app) {
                app.ui.quit = true;
            }
        }
        return;
    }
    if let Some(QuitPrompt::Backup(text)) = app.quit_prompt.clone() {
        let buttons = [
            ("quitBackUp", "Back Up and Quit", Choice::Retry),
            ("quitSkipBackup", "Skip This Time", Choice::QuitAnyway),
            ("quitCancel", "Cancel", Choice::Cancel),
        ];
        let Some(i) = window(ctx, "quit-backup", "Back up the catalog?", &text, backup_hint().as_deref(), &buttons) else { return };
        app.quit_prompt = None;
        match buttons[i].2 {
            Choice::Retry => app.exit_backup = Some(true),
            Choice::QuitAnyway => app.exit_backup = Some(false),
            Choice::Cancel | Choice::Ok => return,
        }
        if may_close(app) {
            app.ui.quit = true;
        }
        return;
    }
    if let Some(QuitPrompt::Unsaved(text)) = app.quit_prompt.clone() {
        let buttons = [
            ("quitRetry", "Try Saving Again", Choice::Retry),
            ("quitAnyway", "Quit Anyway", Choice::QuitAnyway),
            ("quitCancel", "Cancel", Choice::Cancel),
        ];
        let Some(i) = window(ctx, "quit-unsaved", "Quit with unsaved changes?", &text, None, &buttons) else { return };
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
            Choice::Cancel | Choice::Ok => app.quit_prompt = None,
        }
        return;
    }
    if let Some(text) = app.notices.first().cloned() {
        let buttons = [("noticeOk", "OK", Choice::Ok)];
        // an upgrade is news, not a problem: don't title it like one
        let title = if text.starts_with(lcc_engine::library::UPGRADE_NOTICE_START) { "Catalog upgraded" } else { "Settings file problem" };
        if window(ctx, "settings-notice", title, &text, None, &buttons).is_some() {
            app.notices.remove(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{font_notice, gpu_notice};

    /// A font the package lists but couldn't read is named, with what to do; nothing to say when all loaded.
    #[test]
    fn the_font_notice_names_the_fonts_and_says_to_reinstall() {
        use lcc_engine::fonts::FontProblem;
        assert_eq!(font_notice(&[]), None, "every font loaded");
        let problems = [
            FontProblem { what: "Noto Sans Arabic Regular".into(), detail: "missing".into() },
            FontProblem { what: "NanumGothic Regular".into(), detail: "damaged".into() },
        ];
        let text = font_notice(&problems).expect("say which fonts");
        assert!(text.contains("Noto Sans Arabic Regular") && text.contains("NanumGothic Regular"), "{text}");
        assert!(text.contains("Reinstall") && text.contains(lcc_brand::DISPLAY_NAME), "{text}");
    }

    #[test]
    fn the_gpu_notice_is_only_for_a_missing_adapter() {
        let none = "no GPU adapter among Backends(VULKAN | GL) (No suitable graphics adapter found)";
        let text = gpu_notice(Some(none), true).expect("no adapter: say so");
        assert!(text.starts_with("No graphics processor found") && text.contains("System Info"), "{text}");
        assert_eq!(text.contains("nix develop"), cfg!(target_os = "linux"), "{text}");
        assert_eq!(gpu_notice(Some(none), false), None, "the preference is off");
        for reason in [
            format!("disabled by {}=0", lcc_brand::env_var("GPU")),
            format!("disabled by {}=off", lcc_brand::env_var("GPU_BACKEND")),
            "disabled by the GPU rendering preference (app.gpu)".to_string(),
            "software adapter (llvmpipe (LLVM 19.1.7, 256 bits)) skipped: the CPU pipeline is faster".to_string(),
            "stopped after a GPU failure: device lost".to_string(),
        ] {
            assert_eq!(gpu_notice(Some(&reason), true), None, "{reason}");
        }
        assert_eq!(gpu_notice(None, true), None, "the GPU is in use");
    }
}
