//! `photo.trashFiles`: photos leave the library for good and their files go to the system Trash
//! (see [`crate::trash`]). The one command that touches a photo's file on the way out. The work
//! itself is [`crate::trash_job`], which this command runs on the session's thread.

use std::collections::HashSet;
use std::sync::atomic::AtomicBool;

use lightcraft_catalog::PhotoId;
use serde_json::Value;

use super::{CommandSpec, always, bad, cmd};
use crate::trash_job::TrashPlan;
use crate::{Result, Session};

/// What `noTrash` says to do with files on a disk without a Trash.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NoTrash {
    /// The photo and its file stay; the answer names them.
    Keep,
    /// Their files are erased for good.
    Erase,
}

fn no_trash_param(p: &Value, c: &str) -> Result<NoTrash> {
    match p.get("noTrash") {
        None | Some(Value::Null) => Ok(NoTrash::Keep),
        Some(v) => match v.as_str() {
            Some("keep") => Ok(NoTrash::Keep),
            Some("delete") => Ok(NoTrash::Erase),
            _ => Err(bad(c, "`noTrash` is \"keep\" (the default) or \"delete\"")),
        },
    }
}

/// Take `ids` out of the library for good and move their files to the Trash, as one undo step,
/// on this thread. Everything that can go goes; what can't is named in the answer and stays,
/// photo and file.
pub(crate) fn trash_files(s: &mut Session, ids: &[PhotoId], no_trash: NoTrashParam, c: &str) -> Result<Value> {
    let mut plan = TrashPlan::new(s, ids, c)?;
    let never = AtomicBool::new(false);
    let survey = plan.survey(&never, None);
    plan.resolve(s, &survey)?;
    let erase: HashSet<PhotoId> = if no_trash.0 == NoTrash::Erase { plan.file_ids() } else { HashSet::new() };
    plan.work(survey, &erase, &never, None, &crate::rename::RealFs).finish(s, c)
}

/// `noTrash` as a command read it.
pub(crate) struct NoTrashParam(NoTrash);

impl NoTrashParam {
    pub(crate) fn read(p: &Value, c: &str) -> Result<NoTrashParam> {
        no_trash_param(p, c).map(NoTrashParam)
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "photo.trashFiles",
        "Move Files to Trash",
        [],
        None,
        "{ids?, noTrash?: keep|delete} → {removed, trashed, erased, missing, shared, sidecarsLeft, noTrash: [ids], failed: [{id, path, why}], skipped, stopped} — the photos (the selection, or `ids`; in the library or in Recently Deleted) leave the library for good and their files, with their XMP sidecars, go to the system Trash; one undo step, and undo moves the files back when the Trash says where they are. `trashed` and `erased` count files. A file another photo in the same folder is (another spelling of its name, or that folder reached through a link) stays with it (`shared`); before a file is erased, every photo of that name in the library is checked, links included. A sidecar goes only when it is the photo's alone: one named for the stem (`IMG_1.xmp`) stays while any other file of that stem is beside it or a photo of that stem stays in the library, and the sidecars of erased files always stay (`sidecarsLeft`). A file gone from a folder that is there is `missing` and its photo leaves; a photo whose folder can't be reached (a disk that is away) stays and is in `failed`. Photos on a disk without a Trash stay, file and all, and are named in `noTrash`: only a call with `noTrash: \"delete\"` erases their files for good (undo then brings the photos back as missing). Photos on a disk the system says has no Trash are named in `noTrash` without their files being looked at. A photo whose file can't be moved stays and is in `failed`; when nothing could go, that is the error. `stopped` is 0 here: only a run that is cancelled leaves photos unreached. The files move on the session's thread, so nothing else runs meanwhile. Photos only browsed in Local are skipped (an error when all of those named are). Not available on Windows or in the browser yet. Delete, Delete Permanently, Empty Recently Deleted and Remove Folder from Library never touch a file",
        // (not `has_selection`: the photos are named by `ids` as often as selected, and what is
        // there to do is decided, and said, below)
        always,
        |s, p| {
            const C: &str = "photo.trashFiles";
            let no_trash = NoTrashParam::read(p, C)?;
            if let Some(v) = p.get("ids").filter(|v| !v.is_null()) {
                let list = v.as_array().ok_or_else(|| bad(C, "`ids` must be a list of photo ids"))?;
                if let Some(x) = list.iter().find(|x| x.as_u64().is_none()) {
                    return Err(bad(C, format!("`ids` must be a list of photo ids, not {x}")));
                }
            }
            let ids = s.targets(p)?;
            trash_files(s, &ids, no_trash, C)
        }
    )]
}
