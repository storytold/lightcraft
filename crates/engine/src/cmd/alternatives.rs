//! Show Alternatives: a picked photo that turns out not to work, while editing in the library.
//! `photo.showAlternatives` shows the frames around it in its folder on disk (the neighbours of a
//! burst, imported or not) as a folder view; `photo.importAlternatives` brings the chosen ones into
//! the library (with the original's edit, if asked) and goes back to the view the user came from;
//! `photo.closeAlternatives` goes back without importing.

use std::path::Path;

use lcc_catalog::{Filter, Op, PhotoId, Source};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd};
use crate::import::{ImportMode, ImportOptions, import_with, is_supported};
use crate::{Browse, LibrarySource, Result, Selection, Session};

/// Frames either side of the photo, by default.
pub const AROUND: usize = 5;
/// At most this many either side.
const MAX_AROUND: usize = 50;

/// Where the user was when they asked for alternatives, and to which photo.
#[derive(Clone, Debug)]
pub struct Alternatives {
    pub original: PhotoId,
    back: (LibrarySource, Filter, Option<Browse>, Option<String>, Selection),
}

/// The same file, whatever the case of its path on a case-insensitive file system.
fn same_path(a: &str, b: &str) -> bool {
    if cfg!(any(windows, target_os = "macos")) { a.eq_ignore_ascii_case(b) } else { a == b }
}

/// The supported files of `dir`, by name (as a folder view lists them).
fn files_of(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|f| f.is_file() && is_supported(f) && !f.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
                .map(|f| f.to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| lcc_catalog::query::cmp_name(a, b));
    v
}

/// The catalog's photo for file `path` (not one in Recently Deleted).
fn photo_of(s: &Session, path: &str) -> Option<PhotoId> {
    s.catalog.photos().find(|p| !p.deleted && matches!(&p.source, Source::File { path: q } if same_path(q, path))).map(|p| p.id)
}

fn show(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "photo.showAlternatives";
    let id = match p.get("id") {
        Some(v) => PhotoId(v.as_u64().ok_or_else(|| bad(C, "`id` must be a photo id"))?),
        None => s.active().ok_or_else(|| bad(C, "no photo"))?,
    };
    let around = match p.get("around") {
        None | Some(Value::Null) => AROUND,
        Some(v) => usize::try_from(v.as_u64().ok_or_else(|| bad(C, "`around` must be a whole number"))?).unwrap_or(MAX_AROUND).clamp(1, MAX_AROUND),
    };
    let photo = s.catalog.photo(id).ok_or_else(|| bad(C, "no such photo"))?;
    let Source::File { path } = &photo.source else { return Err(bad(C, "this photo has no file on disk")) };
    let path = path.clone();
    let dir = Path::new(&path).parent().ok_or_else(|| bad(C, "the photo's folder can't be found"))?.to_path_buf();
    if !dir.is_dir() {
        return Err(bad(C, format!("{}: the folder is offline or gone", dir.display())));
    }
    let files = files_of(&dir);
    let at = files.iter().position(|f| same_path(f, &path)).ok_or_else(|| bad(C, "the photo's file is not in its folder any more"))?;
    let window: Vec<String> = files.iter().skip(at.saturating_sub(around)).take(around.saturating_mul(2).saturating_add(1)).cloned().collect();
    // the frames not known yet are looked at in place, as browsing the folder would
    let unknown: Vec<String> = window.iter().filter(|f| photo_of(s, f).is_none()).cloned().collect();
    if !unknown.is_empty() {
        let opts = ImportOptions { mode: ImportMode::Add, local: true, on_deleted: crate::import::OnDeleted::Skip, ..Default::default() };
        import_with(s, &unknown, &opts)?;
    }
    let ids: Vec<PhotoId> = window.iter().filter_map(|f| photo_of(s, f)).collect();
    let dir_s = dir.to_string_lossy().trim_end_matches(['/', '\\']).to_string();
    if s.alternatives.is_none() {
        let back = (s.source, s.filter.clone(), s.browse.clone(), s.library_folder.clone(), s.selection.clone());
        s.alternatives = Some(Alternatives { original: id, back });
    } else if let Some(a) = s.alternatives.as_mut() {
        a.original = id;
    }
    s.browse = Some(Browse { path: dir_s, subfolders: false, not_imported: false });
    s.source = LibrarySource::Folder;
    s.filter = Filter { only: ids.clone(), ..Default::default() };
    s.selection = Selection::single(id);
    let shown: Vec<Value> =
        ids.iter().filter_map(|i| s.catalog.photo(*i).map(|p| json!({"id": i.0, "file": p.file_name, "inLibrary": !p.local}))).collect();
    Ok(json!({"original": id.0, "photos": shown}))
}

/// The view the user came from, and the original photo selected (with `also`).
fn go_back(s: &mut Session, also: &[PhotoId]) -> Option<PhotoId> {
    let a = s.alternatives.take()?;
    let (source, filter, browse, library_folder, selection) = a.back;
    s.source = source;
    s.filter = filter;
    s.browse = browse;
    s.library_folder = library_folder;
    s.selection = selection;
    if !also.is_empty() {
        let mut ids = vec![a.original];
        ids.extend(also.iter().copied().filter(|x| *x != a.original));
        s.selection = Selection { ids, active: also.first().copied() };
    }
    s.keep_active_in_view();
    Some(a.original)
}

fn import(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "photo.importAlternatives";
    let original = s.alternatives.as_ref().map(|a| a.original).ok_or_else(|| bad(C, "no alternatives are shown (photo.showAlternatives)"))?;
    let chosen: Vec<PhotoId> = s.targets(p)?.into_iter().filter(|id| *id != original && s.catalog.photo(*id).is_some()).collect();
    if chosen.is_empty() {
        return Err(bad(C, "choose the frames to bring in (not the photo itself)"));
    }
    let same_edit = bool_or(p, "sameEdit", true);
    let mut ops: Vec<Op> =
        chosen.iter().filter(|id| s.catalog.photo(**id).is_some_and(|ph| ph.local)).map(|id| Op::SetLocal { id: *id, local: false }).collect();
    let added = ops.len();
    if same_edit && let Some(edit) = s.develop_of(original) {
        // the edit, not what belongs to the original's own pixels (spots, red eye)
        let mut edit = (*edit).clone();
        edit.spots.clear();
        edit.red_eye.clear();
        for id in &chosen {
            if let Some(op) = s.develop_op(*id, edit.clone(), "Apply the Same Edit") {
                ops.push(op);
            }
        }
    }
    if !ops.is_empty() {
        s.commit("Import Alternatives", Op::Batch { ops })?;
    }
    go_back(s, &chosen);
    Ok(json!({"imported": added, "chosen": chosen.iter().map(|i| i.0).collect::<Vec<_>>(), "sameEdit": same_edit}))
}

fn close(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(json!({"original": go_back(s, &[]).map(|i| i.0)}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "photo.showAlternatives",
            "Show Alternatives",
            ["Photo", "Stack"],
            None,
            "{id?, around?: n (default 5)} → {original, photos: [{id, file, inLibrary}]}: the frames around the photo in its folder on disk (a burst's neighbours, imported or not), shown as a view of that folder with the photo selected; photo.importAlternatives brings chosen ones in, photo.closeAlternatives goes back",
            always,
            show
        ),
        cmd!(
            "photo.importAlternatives",
            "Import Alternatives",
            [],
            None,
            "{ids?, sameEdit?: bool (default true)} → {imported, chosen, sameEdit}: the chosen frames (default: the selection) join the library, with the original's edit (not its spot removal or red eye) when sameEdit; one undo step; then back to the view the alternatives were asked from, the original and the new ones selected",
            always,
            import
        ),
        cmd!("photo.closeAlternatives", "Close Alternatives", [], None, "{}: back to the view the alternatives were asked from", always, close),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` photos in a Temp folder of the test's own, removed when the test ends.
    fn folder(tag: &str, n: usize) -> (lcc_testkit::TempDir, Vec<String>) {
        let dir = lcc_testkit::TempDir::new(&format!("lc-engine-alternatives-{tag}"));
        let paths: Vec<String> = (0..n)
            .map(|i| {
                let p = dir.join(format!("IMG_{i:02}.jpg"));
                let img = lcc_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 13) as u8, (i * 20) as u8, 255]);
                std::fs::write(&p, crate::export::encode_image(&img, &crate::export::ExportOptions::default()).unwrap()).unwrap();
                p.to_string_lossy().into_owned()
            })
            .collect();
        (dir, paths)
    }

    /// The burst around a picked photo comes up from its folder (imported or not); the chosen
    /// frames join the library with the same edit, and the view goes back.
    #[test]
    fn alternatives_come_from_the_folder_and_join_with_the_same_edit() {
        let (dir, paths) = folder("burst", 12);
        let mut s = Session::new().with_fs();
        s.execute("library.import", &json!({"paths": [paths[6]]})).unwrap();
        let original = s.visible_cloned()[0];
        s.execute("library.select", &json!({"ids": [original.0]})).unwrap();
        s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.7})).unwrap();
        let r = s.execute("photo.showAlternatives", &json!({"around": 3})).unwrap();
        let shown = r["photos"].as_array().unwrap();
        assert_eq!(shown.len(), 7, "three either side and the photo: {r}");
        assert_eq!(shown.iter().filter(|p| p["inLibrary"] == true).count(), 1);
        assert_eq!(s.visible_cloned().len(), 7, "the view shows them");
        assert_eq!(s.active(), Some(original));
        let pick = PhotoId(shown[2]["id"].as_u64().unwrap());
        let r = s.execute("photo.importAlternatives", &json!({"ids": [pick.0]})).unwrap();
        assert_eq!(r["imported"], 1);
        assert_eq!(s.source, LibrarySource::All, "back where the user was");
        assert!(s.visible_cloned().contains(&pick) && s.visible_cloned().contains(&original));
        assert!((s.develop_of(pick).unwrap().light.exposure - 0.7).abs() < 1e-9, "the same edit");
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(!s.visible_cloned().contains(&pick), "one undo step");
        // asked again and closed: nothing changes
        s.execute("photo.showAlternatives", &json!({"id": original.0})).unwrap();
        assert!(s.execute("photo.importAlternatives", &json!({"ids": [original.0]})).is_err());
        s.execute("photo.closeAlternatives", &json!({})).unwrap();
        assert!(s.alternatives.is_none());
        assert_eq!(s.visible_cloned(), vec![original]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
