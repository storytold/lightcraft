//! The library's folder tree: where on disk the imported photos live.
//!
//! A *folder* here is a folder holding at least one photo of the library (imported, not deleted,
//! not merely browsed in Local; demo scenes have no folder). The tree lists those folders and the
//! ones above them, never a folder that holds nothing of the library, with how many photos each
//! holds — its subfolders included, like a year in By Date counts its months.
//!
//! Every folder is a row of its own, the ones above the imported photos included
//! (`/Users` → `me` → `Pictures`), so a row is exactly the folder it names. A folder spelled two
//! ways (`/` or `\`, repeated or trailing separators, `.`) is one folder (see
//! [`crate::query::folder_key`]).
//!
//! Each disk is a *volume*, a top-level row of its own: a drive letter (`D:`), a Windows share
//! (`//server/share`), a mounted disk or share (macOS `/Volumes/tokyo`, Linux `/media/me/usb`,
//! `/mnt/disk`, `/run/user/1000/gvfs/smb-share:…`, `/net/host`) or, for everything else, the
//! startup disk (`/`). Only the spelling of the stored paths tells them apart: no disk is
//! touched, so a share that is asleep or gone lists and counts like one that is there, and the
//! sidebar never waits on the network.
//!
//! Limits of reading only the stored spelling of a path: file names are compared as written
//! (Windows folds case, other systems do not), so on a case-insensitive disk `/Volumes/tokyo` and
//! `/Volumes/TOKYO` are two volumes here; and one place reached by two spellings
//! (`/Volumes/Macintosh HD/Users/me` and `/Users/me`, `/media/me/usb` and `/run/media/me/usb`) is
//! two rows. Each row is still consistent: choosing it shows exactly the photos it counts.
//!
//! A folder is otherwise only where photos are, but the library can also keep a
//! [`FolderRecord`] about it (its colour label), stored under the folder's identity
//! ([`crate::query::folder_key`]). A record follows its folder when the folder is renamed or moved
//! here ([`Catalog::folder_records_follow`]); one about a folder that no longer holds a photo is
//! kept (undo can bring the photos back) and shows again when the folder does.
//!
//! This is the library's own view of its photos; browsing any folder on disk without importing
//! is Local's job (see [`crate::local`]).

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::query::{folder_key, folder_rest, folder_within, key_within, split_path};
use crate::{Catalog, ColorLabel, Op, Source};

/// Folder levels kept per photo. No real path comes near it; a deeper one is counted at its
/// ancestor this many levels down, which bounds the tree's depth (and so every walk of it)
/// whatever a catalog holds.
pub const MAX_DEPTH: usize = 64;

/// One row of the library's folder tree.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FolderNode {
    /// What the row says: the folder's name; for a volume, its name (`nas`, `D:`,
    /// `//server/share`, `/` for the startup disk).
    pub name: String,
    /// The folder the row stands for, written out from the names of the photos' paths (forward
    /// slashes, no verbatim prefix); hand it back to [`crate::Filter::library_folder`] to see its
    /// photos.
    pub path: String,
    /// Photos in this folder or any folder inside it.
    pub count: usize,
    /// Photos directly in this folder.
    pub own: usize,
    /// A disk rather than a folder in one; its `path` is where it is mounted.
    pub volume: bool,
    /// Whether choosing the row (`Filter::library_folder`) shows exactly its `count` photos. Not
    /// so for the startup disk and for folders that hold other disks (`/Volumes`, `/mnt`…): their
    /// path covers those disks' photos too, so choosing them is not offered.
    pub selectable: bool,
    /// The folder's colour label (see [`FolderRecord`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<ColorLabel>,
    pub children: Vec<FolderNode>,
}

/// What the library keeps about one of its folders beyond the photos in it. A record with
/// nothing in it is not kept.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderRecord {
    /// A colour label, as a photo has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<ColorLabel>,
}

impl FolderRecord {
    /// Whether the record says nothing (and so is not kept).
    pub fn is_empty(&self) -> bool {
        self.label.is_none()
    }
}

/// The identity a folder's record is kept under: its [`folder_key`]; `None` for a path that
/// names no folder (empty, relative, `.`).
pub(crate) fn record_key(path: &str) -> Option<String> {
    let key = folder_key(path);
    (split_path(path, true).absolute && !key.is_empty()).then_some(key)
}

/// A folder while the tree is being built (children by identity key).
struct Level {
    name: String,
    path: String,
    own: usize,
    volume: bool,
    children: Vec<String>,
}

/// Where a stored folder path lies: its volume and the folder names below it.
struct Placed {
    mount: String,
    volume: String,
    names: Vec<String>,
}

/// A gvfs mount's name made readable: `smb-share:server=tokyo,share=photos` → `photos on tokyo`;
/// any other (`sftp:host=nas`) as it is.
fn mount_name(n: &str) -> String {
    let field = |key: &str| {
        let rest = n.strip_prefix("smb-share:")?;
        rest.split(',').find_map(|kv| kv.strip_prefix(key)?.strip_prefix('=')).filter(|v| !v.is_empty())
    };
    match (field("share"), field("server")) {
        (Some(share), Some(server)) => format!("{share} on {server}"),
        _ => n.to_string(),
    }
}

/// The volume `dir` lies on and the folder names below it, read the way [`folder_key`] reads a
/// path (so a row and the filter that chooses it never disagree). `None` for a path that is not
/// absolute or names no share.
fn place(dir: &str) -> Option<Placed> {
    let sp = split_path(dir, false);
    let names: Vec<&str> = sp.parts.iter().map(String::as_str).collect();
    let named = |mount: String, volume: &str, used: usize| (mount, volume.to_string(), used);
    let (mount, volume, used) = if sp.prefix.starts_with("//") {
        // `//server/share`: a server alone (no share) is no place to keep photos
        if sp.prefix.ends_with('/') {
            return None;
        }
        named(sp.prefix.clone(), &sp.prefix, 0)
    } else if !sp.prefix.is_empty() {
        named(sp.prefix.clone(), &sp.prefix, 0)
    } else if sp.absolute {
        match names.as_slice() {
            ["Volumes", n, ..] => named(format!("/Volumes/{n}"), n, 2),
            ["run", "media", u, n, ..] => named(format!("/run/media/{u}/{n}"), n, 4),
            ["media", u, n, ..] => named(format!("/media/{u}/{n}"), n, 3),
            ["mnt", n, ..] => named(format!("/mnt/{n}"), n, 2),
            // Linux desktops mount network places per user, naming server and share
            ["run", "user", uid, "gvfs", n, ..] => named(format!("/run/user/{uid}/gvfs/{n}"), &mount_name(n), 5),
            // NFS automounts: Linux `/net/<host>`, macOS `/Network/Servers/<host>`
            ["net", host, ..] => named(format!("/net/{host}"), host, 2),
            ["Network", "Servers", host, ..] => named(format!("/Network/Servers/{host}"), host, 3),
            _ => named("/".to_string(), "/", 0),
        }
    } else {
        return None;
    };
    Some(Placed { mount, volume, names: names.iter().skip(used).take(MAX_DEPTH).map(|n| n.to_string()).collect() })
}

/// The disk or share (`/Volumes/nas`, `D:`, `/` for the startup disk) a file-backed photo is on,
/// from its stored path alone.
pub fn volume_of(p: &crate::Photo) -> Option<String> {
    let Source::File { path } = &p.source else { return None };
    let dir = path.rfind(['/', '\\']).and_then(|i| path.get(..i.max(1)))?;
    place(dir).map(|pl| folder_key(&pl.mount))
}

/// Whether `path` is a whole disk or share (`/`, `C:\`, `\\srv\share`, `/Volumes/nas`), not a
/// folder in one.
pub fn is_disk_root(path: &str) -> bool {
    place(path).is_some_and(|p| p.names.is_empty())
}

/// Whether `path` is the startup disk's root (`/`), whose path also covers every other disk.
pub fn is_startup_disk(path: &str) -> bool {
    place(path).is_some_and(|p| p.names.is_empty() && p.mount == "/")
}

/// A folder named by its last two names (`photos/travel`), so two folders called `travel` are
/// told apart; the path itself when it has no names (`/`).
pub fn folder_label(path: &str) -> String {
    let names: Vec<&str> = path.split(['/', '\\']).filter(|n| !n.is_empty()).collect();
    if names.is_empty() { path.to_string() } else { names.iter().skip(names.len().saturating_sub(2)).copied().collect::<Vec<_>>().join("/") }
}

/// The folder a photo's file is in, as its path spells it (`None` for a photo that is no file).
/// A photo straight in the root has an empty folder part: the root itself.
pub(crate) fn photo_folder(p: &crate::Photo) -> Option<&str> {
    match &p.source {
        Source::File { path } => path.rfind(['/', '\\']).and_then(|i| path.get(..i.max(1))),
        _ => None,
    }
}

impl Catalog {
    /// What the library keeps about folder `path`, however it is spelled.
    pub fn folder_record(&self, path: &str) -> Option<&FolderRecord> {
        record_key(path).and_then(|k| self.folder_records.get(&k))
    }

    /// The colour label of folder `path`, however it is spelled.
    pub fn folder_color_label(&self, path: &str) -> Option<ColorLabel> {
        self.folder_record(path).and_then(|r| r.label)
    }

    /// The op that gives folder `path` colour label `label` (`None`: takes it off), keeping the
    /// rest of its record. Applying it checks the path.
    pub fn folder_label_op(&self, path: &str, label: Option<ColorLabel>) -> Op {
        let mut record = self.folder_record(path).cloned().unwrap_or_default();
        record.label = label;
        Op::SetFolderRecord { folder: path.to_string(), record: Some(record) }
    }

    /// The ops that carry the records of folder `from` and the folders inside it over to `to`
    /// (the same place below it), for a folder renamed or moved on disk: commit them with the
    /// photos' relinks so undo puts both back. Records already at or below `to` are left from a
    /// folder that is no longer there and are dropped (undo restores them). Empty when there is
    /// nothing to carry or drop.
    pub fn folder_records_follow(&self, from: &str, to: &str) -> Vec<Op> {
        self.folder_records_carried(from, to, true)
    }

    /// [`Catalog::folder_records_follow`] for a folder located where it already is on disk
    /// (`folder.updateLocation`): nothing arrived at `to`, the library only learned that the
    /// folder is there. So what it knows about folders at or below `to` (other folders of the
    /// library there) stays; a record carried over replaces the one at the same place.
    pub fn folder_records_located(&self, from: &str, to: &str) -> Vec<Op> {
        self.folder_records_carried(from, to, false)
    }

    fn folder_records_carried(&self, from: &str, to: &str, clear_destination: bool) -> Vec<Op> {
        let (Some(root), Some(dest)) = (record_key(from), record_key(to)) else { return Vec::new() };
        if root == dest {
            return Vec::new();
        }
        // a folder located below itself: what is already at or below `to` belongs to the folders
        // of the library there, not to the folder located, so it is neither carried nor cleared
        // (the record of `from` itself still lands at `to`, replacing the one there)
        let nested = !clear_destination && key_within(&dest, &root);
        let carried = |k: &str| key_within(k, &root) && !(nested && key_within(k, &dest));
        let moved: Vec<(String, FolderRecord)> = self
            .folder_records
            .iter()
            .filter(|(k, _)| carried(k))
            .filter_map(|(k, r)| {
                let rest = folder_rest(k, &root)?;
                let base = dest.trim_end_matches('/');
                let path = if rest.is_empty() { dest.clone() } else { format!("{base}/{}", rest.join("/")) };
                Some((folder_key(&path), r.clone()))
            })
            .collect();
        // what was known at the old place goes, and so does what is left at the new one (also
        // when the moved folder carries nothing: the folder now there is not the one it was about)
        let stale = self.folder_records.keys().filter(|k| carried(k) || (clear_destination && key_within(k, &dest)));
        let mut ops: Vec<Op> = stale.map(|k| Op::SetFolderRecord { folder: k.clone(), record: None }).collect();
        ops.extend(moved.into_iter().map(|(k, r)| Op::SetFolderRecord { folder: k, record: Some(r) }));
        ops
    }

    /// The library's volumes with their folders and photo counts: volumes by name
    /// (case-insensitive), each folder with its subfolders the same way.
    pub fn folder_tree(&self) -> Vec<FolderNode> {
        // photos per containing folder, as spelled (sorted, so the spelling that names a folder
        // never depends on hash order)
        let mut dirs: BTreeMap<&str, usize> = BTreeMap::new();
        for p in self.photos().filter(|p| p.in_library()) {
            if let Some(dir) = photo_folder(p) {
                *dirs.entry(dir).or_default() += 1;
            }
        }
        self.folder_tree_of(dirs)
    }

    /// [`Catalog::folder_tree`] from the photos counted per containing folder, as spelled, in
    /// order of spelling ([`photo_folder`]).
    pub(crate) fn folder_tree_of<'a>(&self, dirs: impl IntoIterator<Item = (&'a str, usize)>) -> Vec<FolderNode> {
        self.folder_tree_and_mixed_of(dirs).0
    }

    /// [`Catalog::folder_tree_of`], and the keys of the folders some of whose folders'
    /// spellings differ (`C:` and `c:`; see [`Catalog::patch_folder_tree_of`]).
    pub(crate) fn folder_tree_and_mixed_of<'a>(&self, dirs: impl IntoIterator<Item = (&'a str, usize)>) -> (Vec<FolderNode>, HashSet<String>) {
        let mut levels: HashMap<String, Level> = HashMap::new();
        let mut roots: Vec<String> = Vec::new();
        let mut mixed: HashSet<String> = HashSet::new();
        for (dir, n) in dirs {
            let Some(placed) = place(dir) else { continue };
            let mut parent: Option<String> = None;
            for_levels(&placed, |key, name, path, volume| {
                match levels.get(&key) {
                    Some(l) => {
                        // (the first spelling names the folder: another one is worth knowing)
                        if l.name != name || l.path != path {
                            mixed.insert(key.clone());
                        }
                    }
                    None => {
                        levels.insert(key.clone(), Level { name: name.to_string(), path: path.to_string(), own: 0, volume, children: Vec::new() });
                        match parent.as_ref().and_then(|p| levels.get_mut(p)) {
                            Some(l) => l.children.push(key.clone()),
                            None => roots.push(key.clone()),
                        }
                    }
                }
                parent = Some(key);
            });
            if let Some(l) = parent.and_then(|p| levels.get_mut(&p)) {
                l.own += n;
            }
        }
        let mut tree: Vec<FolderNode> = roots.iter().filter_map(|k| build(k, &levels, &self.folder_records)).collect();
        sort(&mut tree);
        // a folder that holds another disk's mount covers that disk's photos too
        let mounts = mounts_of(&tree);
        mark_selectable(&mut tree, &mounts);
        (tree, mixed)
    }

    /// `tree` (what [`Catalog::folder_tree_and_mixed_of`] gave, with `mixed`) brought up to date
    /// from how many photos each folder gained (`changes`: folder as spelled, photos gained or,
    /// negative, lost) instead of building it again: only the rows on the way to a folder that
    /// changed are touched. The tree is exactly the one building it again gives, or `None` when
    /// that is not so cheap to promise: a disk came or went, a folder's spelling is not the one
    /// its row has, a folder left whose row's spelling might come from it, two rows would be
    /// named alike, or a count would go below nothing. Folder labels are as the records were
    /// when `tree` was built: the caller builds again when they changed.
    pub(crate) fn patch_folder_tree_of(
        &self,
        mut tree: Vec<FolderNode>,
        mixed: &mut HashSet<String>,
        changes: &[(&str, isize)],
    ) -> Option<Vec<FolderNode>> {
        let mounts = mounts_of(&tree);
        // (what comes in first, so that a row emptied by one folder and refilled by another stays)
        let mut order: Vec<&(&str, isize)> = changes.iter().filter(|(_, d)| *d != 0).collect();
        order.sort_by_key(|(_, d)| std::cmp::Reverse(*d));
        for (dir, delta) in order {
            let Some(placed) = place(dir) else { continue };
            let mut chain: Vec<Lvl> = Vec::new();
            for_levels(&placed, |key, name, path, _| chain.push(Lvl { key, name: name.to_string(), path: path.to_string() }));
            let at = Ctx { records: &self.folder_records, mounts: &mounts };
            patch_level(&mut tree, &chain, *delta, true, &at, mixed)?;
        }
        Some(tree)
    }
}

/// One level of a folder's path while it is being walked (see [`for_levels`]).
struct Lvl {
    key: String,
    name: String,
    path: String,
}

/// What a patch of the tree reads besides the rows.
struct Ctx<'a> {
    records: &'a BTreeMap<String, FolderRecord>,
    mounts: &'a [String],
}

/// The levels of a folder, its volume first: (identity key, name, path as the row says it,
/// whether it is the volume). One walk for building the tree and for patching it, so a row is
/// the same whichever way it came.
fn for_levels(placed: &Placed, mut f: impl FnMut(String, &str, &str, bool)) {
    f(folder_key(&placed.mount), &placed.volume, &placed.mount, true);
    let mut prefix = if placed.mount == "/" { String::new() } else { placed.mount.clone() };
    for name in &placed.names {
        prefix.push('/');
        prefix.push_str(name);
        f(folder_key(&prefix), name, &prefix, false);
    }
}

/// The paths of the disks other than the startup disk.
fn mounts_of(tree: &[FolderNode]) -> Vec<String> {
    tree.iter().filter(|v| v.path != "/").map(|v| v.path.clone()).collect()
}

fn lower_name(n: &FolderNode) -> String {
    n.name.to_lowercase()
}

/// `delta` photos more (fewer, when negative) in the folder `chain` leads to, below `nodes`.
/// `None` when the tree can't be patched (see [`Catalog::patch_folder_tree_of`]).
fn patch_level(nodes: &mut Vec<FolderNode>, chain: &[Lvl], delta: isize, top: bool, at: &Ctx, mixed: &mut HashSet<String>) -> Option<()> {
    let (lvl, rest) = chain.split_first()?;
    let want = lvl.name.to_lowercase();
    let start = nodes.partition_point(|n| lower_name(n) < want);
    let mut found = None;
    let mut i = start;
    while let Some(n) = nodes.get(i).filter(|n| lower_name(n) == want) {
        if folder_key(&n.path) == lvl.key {
            found = Some(i);
            break;
        }
        i += 1;
    }
    let idx = match found {
        Some(i) => i,
        None => {
            // a disk coming in changes which rows are choosable; a row that would sort as equal
            // to another one is put where the first spelling met first, which isn't known here
            if top || delta < 0 || nodes.iter().skip(start).take_while(|n| lower_name(n) == want).any(|n| n.name == lvl.name) {
                return None;
            }
            let at_sorted = start + nodes.iter().skip(start).take_while(|n| lower_name(n) == want && n.name < lvl.name).count();
            let selectable = !at.mounts.iter().any(|m| folder_within(m, &lvl.path));
            let label = at.records.get(&lvl.key).and_then(|r| r.label);
            let node = FolderNode {
                name: lvl.name.clone(),
                path: lvl.path.clone(),
                count: 0,
                own: 0,
                volume: false,
                selectable,
                label,
                children: Vec::new(),
            };
            nodes.insert(at_sorted, node);
            at_sorted
        }
    };
    // two disks of one name (`/media/me/usb`, `/run/media/me/usb`) are ordered by the spelling met first
    if top && nodes.iter().skip(start).take_while(|n| lower_name(n) == want).filter(|n| n.name == lvl.name).count() > 1 {
        return None;
    }
    let node = nodes.get_mut(idx)?;
    if node.name != lvl.name || node.path != lvl.path {
        return None;
    }
    let count = node.count.checked_add_signed(delta)?;
    if delta < 0 && count > 0 && mixed.contains(&lvl.key) {
        // the folder that left may be the one whose spelling the row has
        return None;
    }
    if rest.is_empty() {
        node.own = node.own.checked_add_signed(delta)?;
    } else {
        patch_level(&mut node.children, rest, delta, false, at, mixed)?;
    }
    node.count = count;
    if count == 0 {
        if top {
            return None;
        }
        mixed.remove(&lvl.key);
        nodes.remove(idx);
    }
    Some(())
}

/// The node for `key` with everything below it (at most [`MAX_DEPTH`] deep).
fn build(key: &str, levels: &HashMap<String, Level>, records: &BTreeMap<String, FolderRecord>) -> Option<FolderNode> {
    let l = levels.get(key)?;
    let mut children: Vec<FolderNode> = l.children.iter().filter_map(|k| build(k, levels, records)).collect();
    sort(&mut children);
    let count = l.own + children.iter().map(|c| c.count).sum::<usize>();
    Some(FolderNode {
        name: l.name.clone(),
        path: l.path.clone(),
        count,
        own: l.own,
        volume: l.volume,
        selectable: true,
        label: records.get(key).and_then(|r| r.label),
        children,
    })
}

fn mark_selectable(nodes: &mut [FolderNode], mounts: &[String]) {
    for n in nodes {
        n.selectable = !(n.volume && n.path == "/") && (n.volume || !mounts.iter().any(|m| folder_within(m, &n.path)));
        mark_selectable(&mut n.children, mounts);
    }
}

fn sort(v: &mut [FolderNode]) {
    v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.name.cmp(&b.name)));
}
