//! Photos imported earlier, updated on request (issue #4367): LightCraft records some things about a file when it
//! is imported (which segmentation mattes it carries), so photos imported before it did say "not looked at".
//! Reading them again is offered, never done by itself, the way the lens database is ([`crate::lens_db`]): once per
//! version of the app, when the library holds such photos and the window is free ([`tick`]), with how many there
//! are, Update Now or Not Now; Settings ▸ General can run it any time ([`start`]) and switch the offer off.
//!
//! The pass (`lightcraft_engine::cmd::file_facts`) reads the files on a worker thread, with a row in the activity
//! stack that counts them and can stop, and records only what was missing: not an undo step, and nothing else about
//! the photos changes. Files that can't be read (an unplugged disk) stay for a later pass.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lightcraft_engine::cmd::{file_facts, recheck};
use serde_json::{Value, json};

use crate::LightcraftApp;

/// What the update is for, and what it does and doesn't do: said in the offer and in Settings.
pub const WHY: &str = "Reading them again lets presets and masks tell which photos they work on (the people and sky mattes of iPhone ProRAW photos).";
pub const WHAT: &str = "Only that is recorded: your edits and metadata are not touched. It runs in the background and can be stopped.";
/// Said in the offer only: where to find it again.
pub const LATER: &str = "You can do it later from Settings.";

/// The activity stack's label while the files are read.
pub const LABEL: &str = "Updating photos imported earlier";

/// Called every frame: the offer, once per run.
pub fn tick(app: &mut LightcraftApp) {
    if app.ui.file_facts_checked {
        return;
    }
    // a host without a person to ask (CLI snapshot, MCP) never offers; nor does a person who switched it off
    if !app.session.file_facts_offer || !app.ui.settings.file_facts_offer {
        app.ui.file_facts_checked = true;
        return;
    }
    // (the offer waits for the window to be free: What's New, another dialog, the library chooser)
    if app.ui.dialog.is_some() || crate::panels::library_problem::blocks_dialog_input(app) {
        return;
    }
    app.ui.file_facts_checked = true;
    let version = env!("CARGO_PKG_VERSION");
    if app.ui.settings.file_facts_asked.as_deref() == Some(version) {
        return;
    }
    let count = pending(app);
    if count == 0 {
        return;
    }
    // answered once it is shown, as the lens database's offer: closing it any way but Update Now is "not now", and
    // this version doesn't ask again (a later version, which may record more, does)
    app.ui.settings.file_facts_asked = Some(version.to_string());
    app.ui.dialog = Some(crate::state::Dialog::FileFacts { count });
}

/// How many photos the pass would read.
pub fn pending(app: &LightcraftApp) -> usize {
    file_facts::missing(&app.session.catalog, None).len()
}

/// Whether the pass is reading files now.
pub fn running(app: &LightcraftApp) -> bool {
    app.tasks.is_running(LABEL)
}

/// Start the pass in the background → how many photos it reads (0: nothing to do, nothing started).
pub fn start(app: &mut LightcraftApp) -> Result<usize, String> {
    if running(app) {
        return Err(crate::i18n::tr("Updating photos imported earlier…").to_string());
    }
    let probe = recheck::file_probe(&app.session).ok_or_else(|| crate::i18n::tr("This library has no access to the photos' files").to_string())?;
    let listed = file_facts::missing(&app.session.catalog, None);
    let count = listed.len();
    if count == 0 {
        return Ok(0);
    }
    let library = app.session.library_generation();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let work = move |task: lightcraft_engine::activity::TaskHandle| {
        let total = listed.len() as u64;
        task.progress(0, total);
        file_facts::probe_tracked(&probe, listed, &flag, &|done| task.progress(done as u64, total))
    };
    let done = move |app: &mut LightcraftApp, ctx: &egui::Context, results| {
        // another library was opened while the files were read: its photos are not these
        if app.session.library_generation() != library {
            return;
        }
        app.ui.file_facts_epoch = app.ui.file_facts_epoch.wrapping_add(1);
        match file_facts::apply(&mut app.session, results) {
            Ok(recorded) if recorded.len() == 1 => app.toast(ctx, crate::i18n::tr("1 photo updated")),
            Ok(recorded) if recorded.len() > 1 => app.toast(ctx, crate::i18n::tr_format!("{n} photos updated", n = recorded.len())),
            Ok(_) => app.toast(ctx, crate::i18n::tr("No photo could be updated: their files can't be read right now")),
            Err(e) => app.toast_error(ctx, e.to_string()),
        }
    };
    crate::tasks::spawn_tracked(app, LABEL, "fileFacts", cancel, work, done)?;
    Ok(count)
}

/// `library.recordFileFacts` from the app: the whole library in the background → `{background: true, count}`.
/// `None` hands the command to the engine, which answers when it is done: with `wait: true`, or for chosen `ids`.
pub fn command(app: &mut LightcraftApp, p: &Value) -> Option<Result<Value, String>> {
    if p.get("wait").and_then(Value::as_bool) == Some(true) || p.get("ids").is_some_and(|v| !v.is_null()) {
        return None;
    }
    Some(start(app).map(|count| json!({"background": count > 0, "count": count})))
}

/// The offer's window: how many photos, what the update does and doesn't do, and that Settings has it later.
pub fn offer(ui: &mut egui::Ui, count: usize) {
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(crate::i18n::tr("Update photos imported earlier?")).strong());
    ui.add_space(4.0);
    ui.add(
        egui::Label::new(crate::i18n::tr_format!("{n} photos were imported before LightCraft recorded what their files carry.", n = count)).wrap(),
    );
    ui.add_space(4.0);
    ui.add(egui::Label::new(crate::i18n::tr(WHY)).wrap());
    ui.add_space(4.0);
    let note = format!("{} {}", crate::i18n::tr(WHAT), crate::i18n::tr(LATER));
    ui.add(egui::Label::new(egui::RichText::new(note).color(t.text_dim)).wrap());
}
