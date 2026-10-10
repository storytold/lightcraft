//! Export as Catalog / Import from Another Catalog.
//!
//! **Export** writes a subset of the library (photos with their settings, History, Versions,
//! metadata, remote links, the keyword list, the records of their folders and, optionally, their previews index entries and original files) as a
//! new catalog folder, with the albums, smart albums and stacks that concern them.
//!
//! **Import** reads another catalog without modifying it ([`load_readonly`]), matches its photos
//! to this library's by file (path and virtual-copy name), and plans the change ([`plan_import`]:
//! what is new, what differs and how). [`ImportPlan::ops`] turns the plan and a
//! [`ConflictRule`] into one undoable op the caller applies and logs like any other.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

use crate::db::{CatalogDb, DB_FILE};
use crate::journal::{LOG, SNAPSHOT, decode_record};
use crate::library;
use crate::query::folder_within;
use crate::store::{MemStore, Store};
use crate::{Album, AlbumId, Catalog, CatalogError, Journal, Op, Photo, PhotoId, Result, Source, Stack};

fn ioe(e: std::io::Error) -> CatalogError {
    CatalogError::Io(e.to_string())
}

// ---- export

/// What to export.
#[derive(Clone, Debug, Default)]
pub struct ExportOptions {
    /// The photos (their virtual copies are not added implicitly).
    pub photos: Vec<PhotoId>,
    /// Copy the original files into `Originals/` in the new catalog folder and point the
    /// exported photos at the copies.
    pub include_originals: bool,
    /// Keep the previews index entries (the preview files themselves live in the cache).
    pub include_previews: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportReport {
    /// The new catalog's entry point.
    pub entry: PathBuf,
    pub photos: usize,
    pub albums: usize,
    pub stacks: usize,
    pub originals_copied: usize,
    /// Originals that couldn't be copied (missing, unreadable): path and reason. Their photos are
    /// exported pointing at the original location.
    pub originals_failed: Vec<(String, String)>,
}

/// The subset of `cat` that `opts` selects, as a standalone catalog (same ids).
pub fn subset(cat: &Catalog, opts: &ExportOptions) -> Catalog {
    let mut out = Catalog::new();
    let keep: std::collections::BTreeSet<PhotoId> = opts.photos.iter().copied().filter(|id| cat.photo(*id).is_some()).collect();
    for id in &keep {
        if let Some(p) = cat.photo(*id) {
            let mut p = (**p).clone();
            // a virtual copy whose master stays behind becomes a photo of its own
            if p.copy_of.is_some_and(|m| !keep.contains(&m)) {
                p.copy_of = None;
            }
            out.photos.insert(*id, Arc::new(p));
        }
    }
    // albums: manual ones that hold an exported photo (with only those), every smart album, and
    // the album folders above them
    let mut albums: BTreeMap<AlbumId, Album> = BTreeMap::new();
    for a in cat.albums() {
        if a.folder {
            continue;
        }
        if a.is_smart() {
            albums.insert(a.id, a.clone());
            continue;
        }
        let photos: Vec<PhotoId> = a.photos.iter().copied().filter(|p| keep.contains(p)).collect();
        if !photos.is_empty() {
            let mut a = a.clone();
            a.cover = a.cover.filter(|c| keep.contains(c));
            a.photos = photos;
            albums.insert(a.id, a);
        }
    }
    let mut parents: Vec<AlbumId> = albums.values().filter_map(|a| a.parent).collect();
    let mut guard = 0;
    while let Some(pid) = parents.pop() {
        guard += 1;
        if guard > 100_000 || albums.contains_key(&pid) {
            continue;
        }
        if let Some(f) = cat.album(pid) {
            if let Some(gp) = f.parent {
                parents.push(gp);
            }
            albums.insert(pid, f.clone());
        }
    }
    out.albums = albums;
    for s in cat.stacks() {
        let photos: Vec<PhotoId> = s.photos.iter().copied().filter(|p| keep.contains(p)).collect();
        if photos.len() >= 2 {
            out.stacks.insert(s.id, Stack { id: s.id, photos, collapsed: s.collapsed });
        }
    }
    out.remote = cat.remote.iter().filter(|r| keep.contains(&r.photo_id)).cloned().collect();
    if opts.include_previews {
        out.previews = cat.previews.iter().filter(|(id, _)| keep.contains(id)).map(|(k, v)| (*k, v.clone())).collect();
    }
    out.label_names = cat.label_names.clone();
    // the keyword list is the library's vocabulary: all of it goes along, like the label names
    out.keyword_list = cat.keyword_list.clone();
    // what the library keeps about the folders the exported photos are in (and those above them)
    let files: Vec<&str> = out
        .photos
        .values()
        .filter_map(|p| match &p.source {
            Source::File { path } => Some(path.as_str()),
            _ => None,
        })
        .collect();
    out.folder_records =
        cat.folder_records.iter().filter(|(k, _)| files.iter().any(|f| folder_within(f, k))).map(|(k, r)| (k.clone(), r.clone())).collect();
    out.next_photo = cat.next_photo;
    out.next_album = cat.next_album;
    out.next_stack = cat.next_stack;
    out
}

/// A file name that is unique in `dir` (`a.jpg`, `a-2.jpg`, …).
fn unique_in(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s, format!(".{e}")),
        None => (name, String::new()),
    };
    (2..10_000).map(|n| dir.join(format!("{stem}-{n}{ext}"))).find(|p| !p.exists()).unwrap_or(first)
}

/// Export `opts` of `cat` as a new catalog `parent/name`.
pub fn export_catalog(cat: &Catalog, opts: &ExportOptions, parent: &Path, name: &str) -> Result<ExportReport> {
    let mut sub = subset(cat, opts);
    let entry = library::create(parent, name)?;
    let dir = entry.parent().map(Path::to_path_buf).ok_or_else(|| CatalogError::Invalid("catalog without a folder".into()))?;
    let mut report = ExportReport { entry: entry.clone(), ..Default::default() };
    if opts.include_originals {
        let originals = dir.join("Originals");
        std::fs::create_dir_all(&originals).map_err(ioe)?;
        // one copy per file (virtual copies share it)
        let mut copied: HashMap<String, String> = HashMap::new();
        let ids: Vec<PhotoId> = sub.photos.keys().copied().collect();
        for id in ids {
            let Some(p) = sub.photos.get_mut(&id) else { continue };
            let Source::File { path } = &p.source else { continue };
            let path = path.clone();
            let new = match copied.get(&path) {
                Some(n) => n.clone(),
                None => {
                    let file = Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "original".into());
                    let dest = unique_in(&originals, &file);
                    match std::fs::copy(&path, &dest) {
                        Ok(_) => {
                            report.originals_copied += 1;
                            let n = dest.to_string_lossy().into_owned();
                            copied.insert(path.clone(), n.clone());
                            n
                        }
                        Err(e) => {
                            report.originals_failed.push((path.clone(), e.to_string()));
                            continue;
                        }
                    }
                }
            };
            Arc::make_mut(p).source = Source::File { path: new };
        }
    }
    report.photos = sub.photos.len();
    report.albums = sub.albums.values().filter(|a| !a.folder).count();
    report.stacks = sub.stacks.len();
    let (mut j, _, _) = library::open(&entry)?;
    j.snapshot(&sub)?;
    drop(j);
    Ok(report)
}

// ---- import

/// Read the catalog at `path` (entry point or folder) without changing anything there: the v4
/// store read-only, the log replayed in memory; a v3 library read from copies of its files.
pub fn load_readonly(path: &Path) -> Result<Catalog> {
    let dir = library::resolve(path)?;
    let db_path = dir.join(DB_FILE);
    if db_path.exists() {
        let mut db = CatalogDb::open(&db_path)?;
        let (mut cat, seq, _) = db.load()?;
        drop(db);
        let log = std::fs::read(dir.join(LOG)).unwrap_or_default();
        for line in log.split(|b| *b == b'\n') {
            let Ok(line) = std::str::from_utf8(line) else { continue };
            if let Some((s, op)) = decode_record(line.trim_end_matches('\r'))
                && s > seq
            {
                let _ = cat.apply(op);
            }
        }
        cat.revision = 0;
        return Ok(cat);
    }
    let m = MemStore::new();
    for f in [SNAPSHOT, LOG] {
        if let Ok(b) = std::fs::read(dir.join(f)) {
            m.set(f, b);
        }
    }
    let (_, cat, _) = Journal::open_json(Box::new(m) as Box<dyn Store>, false)?;
    Ok(cat)
}

/// What to do with a photo both catalogs have, when they differ.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictRule {
    /// Keep this library's version.
    #[default]
    Keep,
    /// Take the other catalog's develop settings, History and Versions.
    ReplaceSettings,
    /// Take its settings and its metadata (rating, flag, label, descriptive metadata).
    ReplaceSettingsAndMetadata,
    /// Take its metadata only.
    ReplaceMetadata,
}

/// One photo both catalogs have, and how they differ.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Changed {
    /// Its id in the other catalog and in this library.
    pub source: PhotoId,
    pub target: PhotoId,
    pub file_name: String,
    pub settings_differ: bool,
    pub metadata_differ: bool,
}

/// The change preview.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPlan {
    /// Photos only the other catalog has (its ids).
    pub new_photos: Vec<PhotoId>,
    /// Photos both have that differ.
    pub changed: Vec<Changed>,
    /// Photos both have, identical.
    pub unchanged: usize,
    /// Albums it has that this library doesn't (by name and folder path), and albums both have
    /// that it would add photos to.
    pub new_albums: Vec<String>,
    pub extended_albums: Vec<String>,
}

/// How photos are matched: the file, and the virtual copy's name.
fn match_key(p: &Photo) -> Option<(String, Option<String>)> {
    match &p.source {
        Source::File { path } => Some((crate::query::folder_key(path), p.copy_name.clone())),
        Source::Demo { .. } => None,
    }
}

fn metadata_differs(a: &Photo, b: &Photo) -> bool {
    a.rating != b.rating || a.flag != b.flag || a.label != b.label || a.meta != b.meta
}

fn settings_differ(a: &Photo, b: &Photo) -> bool {
    a.develop != b.develop || a.history != b.history || a.versions != b.versions
}

/// An album's place: its folder names from the top, and its own name.
fn album_path(cat: &Catalog, a: &Album) -> Vec<String> {
    let mut path = vec![a.name.clone()];
    let mut at = a.parent;
    let mut guard = 0;
    while let Some(id) = at {
        guard += 1;
        let Some(f) = cat.album(id) else { break };
        if guard > 1000 {
            break;
        }
        path.push(f.name.clone());
        at = f.parent;
    }
    path.reverse();
    path
}

/// Compare `other` with `cat`: the preview of an import.
pub fn plan_import(cat: &Catalog, other: &Catalog) -> ImportPlan {
    let here: HashMap<(String, Option<String>), PhotoId> = cat.photos().filter_map(|p| match_key(p).map(|k| (k, p.id))).collect();
    let mut plan = ImportPlan::default();
    for p in other.photos() {
        match match_key(p).and_then(|k| here.get(&k)).and_then(|id| cat.photo(*id)) {
            None => plan.new_photos.push(p.id),
            Some(t) => {
                let (s, m) = (settings_differ(t, p), metadata_differs(t, p));
                if s || m {
                    plan.changed.push(Changed { source: p.id, target: t.id, file_name: p.file_name.clone(), settings_differ: s, metadata_differ: m });
                } else {
                    plan.unchanged += 1;
                }
            }
        }
    }
    let here_albums: HashMap<Vec<String>, &Album> = cat.albums().map(|a| (album_path(cat, a), a)).collect();
    for a in other.albums().filter(|a| !a.folder) {
        let path = album_path(other, a);
        let name = path.join(" / ");
        match here_albums.get(&path) {
            None => plan.new_albums.push(name),
            Some(h) if !h.is_smart() && !a.is_smart() && !a.photos.is_empty() => plan.extended_albums.push(name),
            Some(_) => {}
        }
    }
    plan
}

impl ImportPlan {
    /// The op that carries out the import into `cat` (ids allocated from it), as one undo step.
    pub fn ops(&self, cat: &mut Catalog, other: &Catalog, rule: ConflictRule) -> Op {
        let mut ops = Vec::new();
        // ids of the other catalog → this library's
        let mut map: HashMap<PhotoId, PhotoId> = self.changed.iter().map(|c| (c.source, c.target)).collect();
        let here: HashMap<(String, Option<String>), PhotoId> = cat.photos().filter_map(|p| match_key(p).map(|k| (k, p.id))).collect();
        for p in other.photos() {
            if let Some(t) = match_key(p).and_then(|k| here.get(&k)) {
                map.entry(p.id).or_insert(*t);
            }
        }
        for src in &self.new_photos {
            map.insert(*src, cat.alloc_photo_id());
        }
        // masters before their virtual copies
        let mut new: Vec<&Arc<Photo>> = self.new_photos.iter().filter_map(|id| other.photo(*id)).collect();
        new.sort_by_key(|p| p.copy_of.is_some());
        for p in new {
            let Some(id) = map.get(&p.id).copied() else { continue };
            let mut q = (**p).clone();
            q.id = id;
            q.copy_of = p.copy_of.and_then(|m| map.get(&m).copied());
            ops.push(Op::AddPhoto { photo: Box::new(q) });
            for r in other.remote_of(p.id) {
                if cat.photo_of_remote(&r.service, &r.account_id, &r.remote_id).is_none() {
                    let mut r = r.clone();
                    r.photo_id = id;
                    ops.push(Catalog::link_remote_op(r));
                }
            }
        }
        for c in &self.changed {
            let Some(p) = other.photo(c.source) else { continue };
            let settings = matches!(rule, ConflictRule::ReplaceSettings | ConflictRule::ReplaceSettingsAndMetadata) && c.settings_differ;
            let metadata = matches!(rule, ConflictRule::ReplaceMetadata | ConflictRule::ReplaceSettingsAndMetadata) && c.metadata_differ;
            if settings {
                ops.push(Op::SetDevelop { id: c.target, settings: p.develop.clone(), label: "Import from Catalog".into(), edited: p.edited.clone() });
                ops.push(Op::SetHistory { id: c.target, history: p.history.clone() });
                ops.push(Op::SetVersions { id: c.target, versions: p.versions.clone() });
            }
            if metadata {
                ops.push(Op::SetRating { id: c.target, rating: p.rating });
                ops.push(Op::SetFlag { id: c.target, flag: p.flag });
                ops.push(Op::SetLabel { id: c.target, label: p.label });
                ops.push(Op::SetMeta { id: c.target, meta: Box::new(p.meta.clone()) });
            }
        }
        // albums: new ones (with their folders), and photos added to the ones both have
        let mut here_albums: HashMap<Vec<String>, AlbumId> = cat.albums().map(|a| (album_path(cat, a), a.id)).collect();
        let mut others: Vec<&Album> = other.albums().collect();
        others.sort_by_key(|a| album_path(other, a).len());
        for a in others {
            let path = album_path(other, a);
            let photos: Vec<PhotoId> = a.photos.iter().filter_map(|p| map.get(p).copied()).collect();
            match here_albums.get(&path).and_then(|id| cat.album(*id)) {
                Some(h) => {
                    if !h.folder && !h.is_smart() && !a.is_smart() {
                        let mut all = h.photos.clone();
                        all.extend(photos.into_iter().filter(|p| !h.photos.contains(p)));
                        if all.len() != h.photos.len() {
                            ops.push(Op::SetAlbumPhotos { id: h.id, photos: all });
                        }
                    }
                }
                None => {
                    let parent_path = path.get(..path.len().saturating_sub(1)).unwrap_or(&[]).to_vec();
                    let parent = if parent_path.is_empty() { None } else { here_albums.get(&parent_path).copied() };
                    let id = cat.alloc_album_id();
                    let mut n = a.clone();
                    n.id = id;
                    n.parent = parent;
                    n.photos = photos;
                    n.cover = a.cover.and_then(|c| map.get(&c).copied());
                    n.quick = false;
                    here_albums.insert(path, id);
                    ops.push(Op::AddAlbum { album: n });
                }
            }
        }
        // keywords and folder records this library doesn't have yet (what it says already wins)
        for l in other.keyword_list.values() {
            if cat.keyword_info(&l.path).is_none() {
                ops.push(Op::SetKeyword { path: l.path.clone(), info: Some(l.info.clone()) });
            }
        }
        for (folder, r) in &other.folder_records {
            if cat.folder_record(folder).is_none() {
                ops.push(Op::SetFolderRecord { folder: folder.clone(), record: Some(r.clone()) });
            }
        }
        // stacks made only of new photos
        for s in other.stacks() {
            let ids: Vec<PhotoId> = s.photos.iter().filter_map(|p| map.get(p).copied()).collect();
            let all_new = s.photos.iter().all(|p| self.new_photos.contains(p));
            if all_new && ids.len() >= 2 {
                ops.push(Op::AddStack { stack: Stack { id: cat.alloc_stack_id(), photos: ids, collapsed: s.collapsed } });
            }
        }
        Op::Batch { ops }
    }
}
