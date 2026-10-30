//! A folder tree patched from the changes to a library's folder counts is the tree built from every
//! folder, after every step (see [`crate::library_tally::LibraryTally::patch_folder_tree`]).
//!
//! * Given a library imported batch by batch, the tree after each batch is what a fresh build gives.
//! * That holds for new nested folders, a folder emptied, photos moved between folders and a folder
//!   spelled in two ways.
//! * What can't be patched exactly (a new disk, a spelling that differs from the row's) is built
//!   again, never patched wrongly.

use crate::library_tally::{FolderUpkeep, LibraryTally};
use crate::*;

/// A small deterministic generator (xorshift), so a failure names its step.
struct Dice(u64);

impl Dice {
    fn roll(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n.max(1)
    }
}

/// Folders as people's libraries hold them: nested and sibling folders, a folder spelled in two
/// ways, case variants, disks (one of them in two spellings of its drive letter), a folder that
/// holds another disk, a share, and the root.
const FOLDERS: &[&str] = &[
    "/Users/me/Pictures",
    "/Users/me/Pictures/2024",
    "/Users/me/Pictures/2024/Italy/Rome",
    "/Users/me/Pictures/2024/italy",
    "/Users/me/Pictures//2024/Italy/",
    "/Users/me/Pictures\\2024\\Spain",
    "/Users/me/Documents",
    "/Volumes/nas/Entities/Z",
    "/Volumes/nas/entities/z",
    "/Volumes/nas/LG/007",
    "/Volumes/nas",
    "/Volumes/usb/a",
    "/Volumes",
    "/mnt/b/x/y/z",
    "C:/Photos/2024",
    "c:\\Photos\\2024\\May",
    "D:/old",
    "d:/old/new",
    "//server/share/a/b",
    "",
    "/Users/me/Pictures/2024/Italy/Rome/Vatican",
    "/media/me/usb/b",
    "/./run/media/me/usb/a",
    "/run/media/me/usb/c",
];

fn dice_dir(d: &mut Dice) -> &'static str {
    FOLDERS[d.roll(FOLDERS.len() as u64) as usize]
}

fn add_op(c: &mut Catalog, dir: &str) -> Op {
    let id = c.alloc_photo_id();
    let name = format!("IMG_{}.jpg", id.0);
    let p = Photo::new(id, Source::File { path: format!("{dir}/{name}") }, &name, "JPEG", 60, 40, "2026-01-01T10:00:00");
    Op::AddPhoto { photo: Box::new(p) }
}

fn any_photo(c: &Catalog, d: &mut Dice) -> Option<PhotoId> {
    let ids: Vec<PhotoId> = c.photos().map(|p| p.id).collect();
    (!ids.is_empty()).then(|| ids[d.roll(ids.len() as u64) as usize])
}

/// One edit of the kinds an import and a library see, chosen by the dice.
fn edit(c: &mut Catalog, d: &mut Dice) -> Op {
    let Some(id) = any_photo(c, d) else { return add_op(c, dice_dir(d)) };
    match d.roll(10) {
        // a batch of an import: a few photos, mostly in one folder
        0..=2 => {
            let dir = dice_dir(d);
            Op::Batch { ops: (0..1 + d.roll(6)).map(|_| add_op(c, dir)).collect() }
        }
        3 => add_op(c, dice_dir(d)),
        4 | 5 => Op::RemovePhoto { id },
        6 | 7 => {
            let dir = dice_dir(d);
            Op::Relink { id, file_name: "moved.jpg".into(), source: Source::File { path: format!("{dir}/moved_{}.jpg", id.0) }, format: None }
        }
        8 => Op::SetLocal { id, local: c.photo(id).is_some_and(|p| !p.local) },
        _ => c.folder_label_op(dice_dir(d), (d.roll(2) == 0).then_some(ColorLabel::Red)),
    }
}

/// The tree the sidebar keeps, brought up to `c` the way the app does it: kept, patched or built again.
struct Kept {
    tally: LibraryTally,
    tree: Vec<FolderNode>,
    upkeep: FolderUpkeep,
    revision: u64,
    patched: usize,
    rebuilt: usize,
}

impl Kept {
    fn new(c: &Catalog) -> Kept {
        let tally = LibraryTally::of(c);
        let (tree, upkeep) = tally.folder_tree_kept(c);
        Kept { tally, tree, upkeep, revision: c.revision, patched: 0, rebuilt: 0 }
    }

    /// `true` when the tree was patched (or nothing changed it), `false` when it was built again.
    fn follow(&mut self, c: &Catalog) -> bool {
        let before = self.tally.folders_stamp();
        if self.tally.follow(c).is_none() {
            *self = Kept { patched: self.patched, rebuilt: self.rebuilt + 1, ..Kept::new(c) };
            return false;
        }
        let tree = std::mem::take(&mut self.tree);
        let only_photos = c.aspects_since(self.revision).is_some();
        self.revision = c.revision;
        if only_photos {
            if self.tally.folders_stamp() == before {
                self.tree = tree;
                return true;
            }
            if let Some(tree) = self.tally.patch_folder_tree(c, tree, &mut self.upkeep) {
                self.tree = tree;
                self.patched += 1;
                return true;
            }
        }
        (self.tree, self.upkeep) = self.tally.folder_tree_kept(c);
        self.rebuilt += 1;
        false
    }
}

#[test]
fn a_patched_folder_tree_is_the_tree_built_from_every_folder_after_every_step() {
    let (mut patched, mut rebuilt) = (0, 0);
    for seed in [3u64, 7, 99, 2026, 0x5eed, 0xfeed_beef] {
        let mut d = Dice(seed);
        let mut c = Catalog::new();
        for _ in 0..30 {
            let op = add_op(&mut c, dice_dir(&mut d));
            c.apply(op).unwrap();
        }
        let mut kept = Kept::new(&c);
        assert_eq!(kept.tree, c.folder_tree(), "seed {seed}: at the start");
        for step in 1..=500 {
            let op = edit(&mut c, &mut d);
            let _ = c.apply(op);
            kept.follow(&c);
            assert_eq!(kept.tree, c.folder_tree(), "seed {seed}, step {step}");
        }
        (patched, rebuilt) = (patched + kept.patched, rebuilt + kept.rebuilt);
    }
    // (the rest are built again: a label edit, a disk coming or going, a folder spelled two ways, two disks of one
    // name; on Windows more folders are one folder in two spellings)
    let share = if cfg!(windows) { 2 } else { 1 };
    assert!(share * patched > rebuilt, "{patched} patched, {rebuilt} built again");
}

/// Adds the photos to a library that already has some in every folder of `FOLDERS` but `skip`.
fn library_without(skip: &[&str]) -> Catalog {
    let mut c = Catalog::new();
    for dir in FOLDERS.iter().filter(|f| !skip.contains(f)) {
        let op = add_op(&mut c, dir);
        c.apply(op).unwrap();
    }
    c
}

fn step(c: &mut Catalog, kept: &mut Kept, op: Op) -> bool {
    c.apply(op).unwrap();
    let patched = kept.follow(c);
    assert_eq!(kept.tree, c.folder_tree());
    patched
}

#[test]
fn an_import_batch_is_patched_into_the_rows_it_changes() {
    let mut c = library_without(&["/Users/me/Pictures/2024/Italy/Rome/Vatican"]);
    let mut kept = Kept::new(&c);
    // into a folder that is a row already; into one below a row (new rows, nested two deep)
    let op = add_op(&mut c, "/Users/me/Pictures/2024");
    assert!(step(&mut c, &mut kept, op));
    let op = add_op(&mut c, "/Users/me/Pictures/2024/Italy/Rome/Vatican");
    assert!(step(&mut c, &mut kept, op));
    let op = add_op(&mut c, "/Users/me/Pictures/Brand New/Deeper/Still");
    assert!(step(&mut c, &mut kept, op));
    assert_eq!(kept.rebuilt, 0);
}

#[test]
fn a_folder_that_is_emptied_leaves_the_tree_and_comes_back_in_its_place() {
    let mut c = Catalog::new();
    for dir in ["/Users/me/a", "/Users/me/b/deep", "/Users/me/c"] {
        let op = add_op(&mut c, dir);
        c.apply(op).unwrap();
    }
    let mut kept = Kept::new(&c);
    let deep = c.photos().find(|p| matches!(&p.source, Source::File { path } if path.contains("deep"))).unwrap().id;
    assert!(step(&mut c, &mut kept, Op::RemovePhoto { id: deep }));
    // `b` and `deep` are gone, not left as rows of nothing
    assert!(kept.tree.iter().all(|v| v.children.iter().all(|u| u.children.iter().all(|n| n.name != "b"))));
    let op = add_op(&mut c, "/Users/me/b/deep");
    assert!(step(&mut c, &mut kept, op));
    assert_eq!(kept.rebuilt, 0);
}

#[test]
fn a_photo_moved_between_folders_moves_its_count() {
    let mut c = library_without(&[]);
    let mut kept = Kept::new(&c);
    // (from folders spelled one way only: on Windows a case variant is a second spelling of one folder)
    let from = ["/Users/me/Documents", "/Volumes/nas/LG/007", "/Users/me/Pictures", "/Users/me/Pictures/2024"];
    let to = ["/Users/me/Pictures/2024", "/Volumes/nas/LG/008", "/Users/me/new place/x", "/Users/me/Pictures"];
    for (i, (from, to)) in from.into_iter().zip(to).enumerate() {
        let id = c.photos().find(|p| matches!(&p.source, Source::File { path } if path.starts_with(&format!("{from}/IMG_")))).unwrap().id;
        let op = Op::Relink { id, file_name: "m.jpg".into(), source: Source::File { path: format!("{to}/m.jpg") }, format: None };
        assert!(step(&mut c, &mut kept, op), "move {i} from {from} to {to}");
    }
}

#[test]
fn two_disks_of_one_name_are_ordered_as_a_rebuild_orders_them() {
    let mut c = Catalog::new();
    for dir in ["/./run/media/me/usb/a", "/media/me/usb/b", "/run/media/me/usb/c"] {
        let op = add_op(&mut c, dir);
        c.apply(op).unwrap();
    }
    let mut kept = Kept::new(&c);
    let gone = c.photos().next().unwrap().id;
    step(&mut c, &mut kept, Op::RemovePhoto { id: gone });
    let op = add_op(&mut c, "/./run/media/me/usb/a");
    step(&mut c, &mut kept, op);
}

/// `n` photos in each of `dirs`, in batches the tally can follow.
fn grow(c: &mut Catalog, t: &mut LibraryTally, dirs: &[String], n: usize) {
    let ops: Vec<Op> = dirs.iter().flat_map(|d| (0..n).map(move |_| d)).map(|d| add_op(c, d)).collect();
    for batch in ops.chunks(2000) {
        c.apply(Op::Batch { ops: batch.to_vec() }).unwrap();
        assert!(t.follow(c).is_some());
    }
}

#[test]
fn a_tree_whose_changes_were_trimmed_from_the_log_is_built_again() {
    let mut c = library_without(&[]);
    let mut t = LibraryTally::of(&c);
    let (tree, mut upkeep) = t.folder_tree_kept(&c);
    // more changes than the log keeps, to one folder, without a patch between
    grow(&mut c, &mut t, &["/Users/me/Documents".to_string()], 10_000);
    assert!(t.patch_folder_tree(&c, tree, &mut upkeep).is_none());
    let (tree, mut upkeep) = t.folder_tree_kept(&c);
    assert_eq!(tree, c.folder_tree());
    // and a tree kept after the trim is patched again
    grow(&mut c, &mut t, &["/Users/me/Documents".to_string()], 3);
    assert_eq!(t.patch_folder_tree(&c, tree, &mut upkeep).unwrap(), c.folder_tree());
}

#[test]
fn a_change_to_very_many_folders_is_built_again() {
    let mut c = library_without(&[]);
    let mut t = LibraryTally::of(&c);
    let (tree, mut upkeep) = t.folder_tree_kept(&c);
    let dirs: Vec<String> = (0..600).map(|i| format!("/Users/me/Many/{i}")).collect();
    grow(&mut c, &mut t, &dirs, 1);
    assert!(t.patch_folder_tree(&c, tree, &mut upkeep).is_none());
    assert_eq!(t.folder_tree_kept(&c).0, c.folder_tree());
}

#[test]
fn a_case_variant_of_a_folder_is_one_row_and_is_patched_as_one() {
    let mut c = Catalog::new();
    for dir in ["/Volumes/nas/Entities/Z", "/Volumes/nas/other"] {
        let op = add_op(&mut c, dir);
        c.apply(op).unwrap();
    }
    let mut kept = Kept::new(&c);
    // the same folder spelled the other way (`//` and a trailing `/`): one row, counted twice
    let op = add_op(&mut c, "/Volumes/nas//Entities/Z/");
    assert!(step(&mut c, &mut kept, op));
    // (case alone makes another folder on this system, and on Windows the same one: both are
    // the tree a rebuild gives, patched or built again)
    let op = add_op(&mut c, "/Volumes/nas/entities/z");
    step(&mut c, &mut kept, op);
}

#[test]
fn a_new_disk_is_built_again_not_patched() {
    let mut c = library_without(&["/mnt/b/x/y/z"]);
    let mut kept = Kept::new(&c);
    let op = add_op(&mut c, "/mnt/b/x/y/z");
    assert!(!step(&mut c, &mut kept, op), "a disk coming in changes which rows can be chosen");
    // (and the folder that holds a disk is not choosable, as a rebuild says)
    let op = add_op(&mut c, "/Volumes/nas/LG/more");
    assert!(step(&mut c, &mut kept, op));
}

#[test]
fn a_drive_letter_written_two_ways_is_one_disk_whichever_goes_first() {
    for first in ["D:/old", "d:/old"] {
        let mut c = Catalog::new();
        for dir in [first, "D:/old/new", "d:/old/new"] {
            let op = add_op(&mut c, dir);
            c.apply(op).unwrap();
        }
        let mut kept = Kept::new(&c);
        // the photo whose folder gave the disk its name goes: the name may come from another
        let gone = c.photos().next().unwrap().id;
        step(&mut c, &mut kept, Op::RemovePhoto { id: gone });
        let gone = c.photos().next().unwrap().id;
        step(&mut c, &mut kept, Op::RemovePhoto { id: gone });
    }
}

#[test]
fn a_tree_cannot_be_patched_from_another_counting() {
    let mut c = library_without(&[]);
    let a = LibraryTally::of(&c);
    let (tree, mut upkeep) = a.folder_tree_kept(&c);
    let op = add_op(&mut c, "/Users/me/Documents");
    c.apply(op).unwrap();
    let mut b = LibraryTally::of(&c);
    assert!(b.follow(&c).is_some());
    assert!(b.patch_folder_tree(&c, tree, &mut upkeep).is_none());
}
