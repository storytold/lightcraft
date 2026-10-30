//! Move Files to Trash…: the questions before `photo.trashFiles` (see `lightcraft_engine::trash`).
//! Taking a photo out of the library never touches its file, except here, and here it is always
//! asked first: once for the Trash, and once more before a file on a disk without a Trash is
//! erased for good. The work runs in the background ([`crate::trash_run`]).

use lightcraft_catalog::PhotoId;
use serde_json::Value;

use crate::LightcraftApp;
use crate::state::Dialog;

/// What is being asked.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TrashAsk {
    /// Move these photos' files to the Trash? (`emptying`: they are all of Recently Deleted.)
    /// `since`: when it took another question's place (the Delete question's second answer):
    /// its button waits a moment then, as the Erase question's always does. `library`: the
    /// library it was asked in (`Session::library_generation`): ids are numbered per library, so
    /// in another one they name other photos, whose files must not go.
    Trash { ids: Vec<u64>, emptying: bool, since: Option<f64>, library: u64 },
    /// These photos are on a disk without a Trash: erase their files for good? `since`: when it
    /// came up (its button waits a moment, so a second click meant for the question before it
    /// does not answer this one). `library`: as for `Trash`.
    Erase { ids: Vec<u64>, since: f64, library: u64 },
}

/// How long the Erase question's button stays unavailable after it came up (s).
pub const ERASE_SETTLE_SECONDS: f64 = 1.0;

/// Whether the question's button can be pressed yet.
pub(crate) fn armed(app: &LightcraftApp, ctx: &egui::Context, ask: &TrashAsk) -> bool {
    match ask {
        TrashAsk::Trash { since: None, .. } => true,
        TrashAsk::Trash { since: Some(since), .. } | TrashAsk::Erase { since, .. } => {
            let left = since + ERASE_SETTLE_SECONDS - app.now();
            if left > 0.0 {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(left));
            }
            left <= 0.0
        }
    }
}

/// Whether this build can move files to a Trash at all (not on Windows or in the browser yet).
pub fn available() -> bool {
    lightcraft_engine::trash::available()
}

/// Open the first question, for the selected photos or (`recentlyDeleted`) for every photo in
/// Recently Deleted. Photos only browsed in Local are left out: they are not the library's.
pub fn open(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    if !available() {
        return Err(crate::i18n::tr("Moving files to the Trash is not available here yet").to_string());
    }
    if app.trash_run.is_some() {
        return Err(crate::i18n::tr("Files are already being moved to the Trash").to_string());
    }
    let emptying = p.get("recentlyDeleted").and_then(Value::as_bool).unwrap_or(false);
    let cat = &app.session.catalog;
    let ids: Vec<u64> = if emptying {
        cat.photos().filter(|p| p.deleted).map(|p| p.id.0).collect()
    } else {
        app.session.targets(p).map_err(|e| e.to_string())?.into_iter().filter(|id| cat.photo(*id).is_some_and(|p| !p.local)).map(|id| id.0).collect()
    };
    if ids.is_empty() {
        return Err(crate::i18n::tr(if emptying { "Recently Deleted is empty" } else { "No photos of the library are selected" }).to_string());
    }
    // the command would refuse this only after the person said yes
    if let Some(why) = lightcraft_engine::trash::too_many(ids.len()) {
        return Err(why);
    }
    // (in place of another question: a second click meant for that one must not answer this)
    let since = p.get("settle").and_then(Value::as_bool).unwrap_or(false).then(|| app.now());
    let library = app.session.library_generation();
    app.ui.dialog = Some(Dialog::TrashFiles(TrashAsk::Trash { ids, emptying, since, library }));
    Ok(Value::Null)
}

pub(crate) fn title(ask: &TrashAsk) -> &'static str {
    match ask {
        TrashAsk::Trash { .. } => "Move Files to Trash",
        TrashAsk::Erase { .. } => "Erase Files",
    }
}

pub(crate) fn button(ask: &TrashAsk) -> &'static str {
    match ask {
        TrashAsk::Trash { .. } => "Move to Trash",
        TrashAsk::Erase { .. } => "Erase Files",
    }
}

/// What the question says: what goes where, and what Undo can and can't do about it.
pub(crate) fn body(app: &LightcraftApp, ui: &mut egui::Ui, ask: &TrashAsk) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let dim = |ui: &mut egui::Ui, text: &str| {
        ui.label(egui::RichText::new(crate::i18n::tr(text)).color(t.text_dim));
    };
    ui.set_max_width(460.0);
    match ask {
        TrashAsk::Trash { ids, .. } => {
            ui.label(match ids.len() {
                1 => crate::i18n::tr("Move this photo's file to the Trash?").to_string(),
                n => crate::i18n::tr_format!("Move the files of {n} photos to the Trash?", n = n),
            });
            names(app, ui, ids);
            dim(ui, "The photos leave the library, also Recently Deleted. Their files go to the system Trash, with their XMP sidecars.");
            dim(ui, "Undo brings the photos back and moves the files back, as long as they are still in the Trash.");
        }
        TrashAsk::Erase { ids, .. } => {
            ui.label(match ids.len() {
                1 => crate::i18n::tr("This photo is on a disk that has no Trash. Erase its file for good?").to_string(),
                n => crate::i18n::tr_format!("{n} photos are on a disk that has no Trash. Erase their files for good?", n = n),
            });
            names(app, ui, ids);
            ui.label(
                egui::RichText::new(crate::i18n::tr("This cannot be undone: the files are not kept anywhere."))
                    .color(crate::theme::label_color(lightcraft_catalog::ColorLabel::Red)),
            );
            dim(ui, "Only these files are erased; XMP sidecars beside them stay.");
            dim(ui, "Cancel leaves these photos in the library and their files where they are.");
        }
    }
}

/// The first few of the files a question is about, then how many more.
fn names(app: &LightcraftApp, ui: &mut egui::Ui, ids: &[u64]) {
    const SHOWN: usize = 5;
    let t = crate::theme::Tokens::get(ui.ctx());
    for id in ids.iter().take(SHOWN) {
        let Some(p) = app.session.catalog.photo(PhotoId(*id)) else { continue };
        let text = match &p.source {
            lightcraft_catalog::Source::File { path } => path.clone(),
            lightcraft_catalog::Source::Demo { .. } => p.file_name.clone(),
        };
        ui.label(egui::RichText::new(text).color(t.text_dim));
    }
    if ids.len() > SHOWN {
        ui.label(egui::RichText::new(crate::i18n::tr_format!("…and {n} more", n = ids.len() - SHOWN)).color(t.text_dim));
    }
}

/// Do what was asked: the work starts in the background and the question closes. Photos on a
/// disk without a Trash are asked about in the Erase question before anything moves (nothing of
/// theirs is touched before that is answered); what went and what could not go is said in a
/// notice when the work is done ([`report`]).
pub(crate) fn run(app: &mut LightcraftApp, ask: &TrashAsk) -> Result<Value, String> {
    let (TrashAsk::Trash { library, .. } | TrashAsk::Erase { library, .. }) = ask;
    // another library opened meanwhile (from the menu bar, an agent): these numbers are other
    // photos there, and their files would go
    if *library != app.session.library_generation() {
        return Err(crate::i18n::tr("Another library was opened since the question came up: nothing was moved to the Trash or erased").to_string());
    }
    match ask {
        // (the photos the question listed, also when it was about all of Recently Deleted)
        TrashAsk::Trash { ids, .. } => crate::trash_run::start(app, ids.clone(), None)?,
        // asked by the run at work before anything moved: it goes on with them
        TrashAsk::Erase { ids, .. } if crate::trash_run::answer_erase(app, ids) => {}
        // asked after a run (files there were found to have no Trash only as they were tried)
        TrashAsk::Erase { ids, .. } => crate::trash_run::start(app, ids.clone(), Some(ids.clone()))?,
    }
    Ok(Value::Null)
}

/// Say what a run did (`photo.trashFiles`'s answer `r`). Photos found to be on a disk without a
/// Trash only as their files were tried get the Erase question now, unless they were `asked`
/// about before anything moved or this run was `erasing`.
pub(crate) fn report(app: &mut LightcraftApp, r: &Value, asked: &[u64], library: u64, erasing: bool) {
    let n = |key: &str| r[key].as_u64().unwrap_or(0);
    let mut said = Vec::new();
    if n("removed") > 0 {
        said.push(if n("trashed") > 0 && n("erased") > 0 {
            crate::i18n::tr_format!(
                "Files moved to the Trash: {trashed}, files erased: {erased} (photos out of the library: {photos})",
                trashed = n("trashed"),
                erased = n("erased"),
                photos = n("removed")
            )
        } else if erasing || (n("trashed") == 0 && n("erased") > 0) {
            crate::i18n::tr_format!("Files erased: {files} (photos out of the library: {photos})", files = n("erased"), photos = n("removed"))
        } else {
            crate::i18n::tr_format!(
                "Files moved to the Trash: {files} (photos out of the library: {photos})",
                files = n("trashed"),
                photos = n("removed")
            )
        });
    }
    let failed = r["failed"].as_array().map(Vec::as_slice).unwrap_or_default();
    if let Some(first) = failed.first() {
        said.push(crate::i18n::tr_format!(
            "Photos that could not be moved stay in the library ({n}): {why}",
            n = failed.len(),
            why = first["why"].as_str().unwrap_or_default()
        ));
    }
    if n("stopped") > 0 {
        said.push(crate::i18n::tr_format!("Stopped: photos not reached stay in the library: {n}", n = n("stopped")));
    }
    // what left the library without a file being moved, and sidecars that were left
    if n("missing") > 0 {
        said.push(crate::i18n::tr_format!("Photos whose file was already gone: {n}", n = n("missing")));
    }
    if n("shared") > 0 {
        said.push(crate::i18n::tr_format!("Files other photos still use, kept: {n}", n = n("shared")));
    }
    if n("sidecarsLeft") > 0 {
        said.push(crate::i18n::tr_format!("Sidecars left in place: {n}", n = n("sidecarsLeft")));
    }
    let plain = failed.is_empty() && n("stopped") == 0 && n("missing") == 0 && n("sidecarsLeft") == 0 && n("shared") == 0;
    if !said.is_empty() {
        app.say(said.join(" · "), if plain { 4.0 } else { 12.0 });
    }
    // a disk without a Trash found only as its files were tried: asked about separately, and
    // nothing of it was touched. (Another dialog is up: the question waits until it closes.
    // One still waiting from an earlier run, in this library, is asked together with it.)
    let mut no_trash: Vec<u64> =
        r["noTrash"].as_array().map(|a| a.iter().filter_map(Value::as_u64).filter(|id| !asked.contains(id)).collect()).unwrap_or_default();
    if !erasing && !no_trash.is_empty() {
        if let Some((waiting, at)) = app.trash_erase_later.take()
            && at == library
        {
            no_trash.extend(waiting.into_iter().filter(|id| !no_trash.contains(id)).collect::<Vec<_>>());
        }
        app.trash_erase_later = Some((no_trash, library));
        crate::trash_run::ask_erase_later(app);
    }
}
