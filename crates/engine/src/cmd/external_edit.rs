//! The round trip to an external editor around `photo.editExternal` (the command itself is in
//! [`super::convert`]):
//!
//! - [`stack_edit`]: the edit copy is stacked like Lightroom's Edit In: directly above the photo
//!   it was made from, inside that photo's stack when it has one (an edit of an edit joins the
//!   same stack; an edit of an unstacked virtual copy stacks with the copy and its master).
//! - [`link`]: the TIFF carries `xmpMM:DocumentID` / `InstanceID` / `DerivedFrom`, so importing a
//!   folder stacks copies on their sources again ([`link::stack_derived`]).
//! - [`editor`]: PhotoCraft's `--wait` / `--single-instance` (feature-detected with
//!   `--version --json`) and a modification-time poll of the files out for editing.
//!
//! Sources: own design.

use lightcraft_catalog::{Op, PhotoId};

use crate::{Result, Session};

pub mod editor;
pub mod link;

/// Stack the edit copy `new` directly above `on` (see the module docs), expanded so the versions
/// show.
pub(crate) fn stack_edit(s: &mut Session, new: PhotoId, on: PhotoId) -> Result<()> {
    // already directly above `on` in an expanded stack (the import placed it from the DerivedFrom link):
    // nothing to do, and no no-op undo step
    if let Some(st) = s.catalog.stack_of(on)
        && !st.collapsed
        && st.photos.iter().position(|p| *p == on).is_some_and(|i| i > 0 && st.photos.get(i - 1) == Some(&new))
    {
        return Ok(());
    }
    let master = s.catalog.photo(on).and_then(|p| p.copy_of).filter(|m| s.catalog.photo(*m).is_some());
    let op = match (s.catalog.stack_of(on).is_some(), master) {
        // an unstacked virtual copy: the edit and the copy go directly above the master, in the
        // master's stack or a new one
        (false, Some(m)) => match s.catalog.stack_of(m).cloned() {
            Some(st) => {
                let mut photos: Vec<PhotoId> = st.photos.iter().copied().filter(|p| *p != new && *p != on).collect();
                let at = photos.iter().position(|p| *p == m).unwrap_or(0);
                photos.splice(at..at, [new, on]);
                let mut ops = s.catalog.remove_from_stacks_ops(&[new]);
                ops.retain(|o| !matches!(o, Op::SetStack { id, .. } | Op::RemoveStack { id } if *id == st.id));
                ops.push(Op::SetStack { id: st.id, photos, collapsed: false });
                Some(Op::Batch { ops })
            }
            None => s.catalog.group_ops(new, &[new, on, m], false),
        },
        _ => s.catalog.insert_above_ops(new, on, false),
    };
    match op {
        Some(op) => s.commit("Stack Edit", op),
        None => Ok(()),
    }
}

/// Open `path` in `app` with `--wait` and block until the editor exits, with a row in the
/// activity stack meanwhile; then reload `id`. → `{waited, saved, reloaded}`
pub(crate) fn wait_and_reload(s: &mut Session, app: &str, path: &std::path::Path, id: u64, cmd: &str) -> Result<serde_json::Value> {
    let caps = editor::capabilities(app);
    if !caps.wait {
        return Err(super::bad(cmd, format!("{app} can't be waited for (it doesn't take --wait)")));
    }
    let line = editor::wait_command_line(app, path, caps);
    let task = s.activity.start("externalEdit", "Editing in the external editor", crate::activity::Cancel::No);
    task.detail(&path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
    let saved = editor::run_and_wait(&line).map_err(|e| super::bad(cmd, format!("couldn't start the editor: {e}")));
    drop(task);
    let saved = saved?;
    let r = s.execute("photo.reload", &serde_json::json!({"ids": [id]}))?;
    Ok(serde_json::json!({"waited": true, "saved": saved, "reloaded": r["reloaded"]}))
}
