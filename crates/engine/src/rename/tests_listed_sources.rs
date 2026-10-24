//! Moving photos out of a folder asks the disk for their sidecars only where a listing of the
//! folder leaves room for one: on a network share each name asked about is a round trip.
use super::*;
use std::cell::Cell;

/// The disk, counting the questions about `.xmp` names and the listings.
struct Counting {
    can_list: bool,
    xmp_asked: Cell<usize>,
    listings: Cell<usize>,
}

impl Counting {
    fn new(can_list: bool) -> Self {
        Counting { can_list, xmp_asked: Cell::new(0), listings: Cell::new(0) }
    }
}

impl MoveFs for Counting {
    fn exists(&self, p: &Path) -> bool {
        RealFs.exists(p)
    }
    fn is_file(&self, p: &Path) -> bool {
        if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("xmp")) {
            self.xmp_asked.set(self.xmp_asked.get() + 1);
        }
        RealFs.is_file(p)
    }
    fn same_file(&self, a: &Path, b: &Path) -> bool {
        RealFs.same_file(a, b)
    }
    fn rename_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        RealFs.rename_no_replace(a, b)
    }
    fn copy_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        RealFs.copy_no_replace(a, b)
    }
    fn remove_file(&self, p: &Path) -> std::io::Result<()> {
        RealFs.remove_file(p)
    }
    fn stem_sibling(&self, sidecar: &Path) -> Option<String> {
        RealFs.stem_sibling(sidecar)
    }
    fn list_dir(&self, dir: &Path) -> Option<Vec<std::ffi::OsString>> {
        self.listings.set(self.listings.get() + 1);
        if self.can_list { RealFs.list_dir(dir) } else { None }
    }
}

fn scratch(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("lc-listed-sources-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (from, to) = (root.join("from"), root.join("to"));
    std::fs::create_dir_all(&from).unwrap();
    std::fs::create_dir_all(&to).unwrap();
    (from, to)
}

/// `n` photos `p00.jpg`… in `from`, moved to `to` under the same names.
fn moves(from: &Path, to: &Path, n: usize) -> Vec<(String, String)> {
    (0..n)
        .map(|i| {
            let name = format!("p{i:02}.jpg");
            (from.join(&name).to_string_lossy().to_string(), to.join(name).to_string_lossy().to_string())
        })
        .collect()
}

fn names_lower(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_lowercase()).collect();
    v.sort();
    v
}

/// Given 50 photos in a folder without sidecars, when they are moved, then no sidecar name is
/// asked of the disk (it was eight questions for each photo), and the folder is listed once.
#[test]
fn photos_leaving_a_listed_folder_without_sidecars_ask_the_disk_nothing() {
    let (from, to) = scratch("none");
    for (src, _) in moves(&from, &to, 50) {
        std::fs::write(src, b"x").unwrap();
    }
    let fs = Counting::new(true);
    move_all(&fs, &moves(&from, &to, 50)).unwrap();
    assert_eq!((fs.xmp_asked.get(), fs.listings.get()), (0, 1));
    assert_eq!(names_lower(&to).len(), 50);
    assert!(names_lower(&from).is_empty());
    let _ = std::fs::remove_dir_all(from.parent().unwrap());
}

/// Given sidecars in every case and naming among the photos, when they are moved and moved back
/// (undo), then each goes with its photo each way, and only the photos that have one are asked
/// about.
#[test]
fn sidecars_in_the_listing_move_with_their_photos_and_back() {
    let (from, to) = scratch("cases");
    let all = moves(&from, &to, 12);
    for (src, _) in &all {
        std::fs::write(src, b"x").unwrap();
    }
    // stem and full names, in lower and upper case (the spellings a move looks for)
    for sc in ["p00.xmp", "p01.XMP", "p02.jpg.xmp", "p03.jpg.XMP"] {
        std::fs::write(from.join(sc), b"<x/>").unwrap();
    }
    let before = names_lower(&from);
    let fs = Counting::new(true);
    move_all(&fs, &all).unwrap();
    assert!(names_lower(&from).is_empty(), "everything left, sidecars with their photos: {:?}", names_lower(&from));
    assert_eq!(names_lower(&to), before);
    // (the four photos with a sidecar are asked about; the eight others are not)
    assert!(fs.xmp_asked.get() > 0 && fs.xmp_asked.get() < 12 * 8, "{}", fs.xmp_asked.get());

    let back: Vec<(String, String)> = all.iter().map(|(a, b)| (b.clone(), a.clone())).collect();
    move_all(&fs, &back).unwrap();
    assert_eq!(names_lower(&from), before);
    assert!(names_lower(&to).is_empty());
    let _ = std::fs::remove_dir_all(from.parent().unwrap());
}

/// A folder that can't be listed, and one with too few photos to list, keep the probes: a
/// sidecar there is still moved with its photo.
#[test]
fn a_folder_that_cannot_be_listed_or_is_too_small_is_probed_photo_by_photo() {
    for (can_list, photos) in [(false, 12), (true, LIST_FOLDER_AT - 1)] {
        let (from, to) = scratch("probed");
        let all = moves(&from, &to, photos);
        for (src, _) in &all {
            std::fs::write(src, b"x").unwrap();
        }
        std::fs::write(from.join("p00.XMP"), b"<x/>").unwrap();
        let fs = Counting::new(can_list);
        move_all(&fs, &all).unwrap();
        assert!(names_lower(&to).contains(&"p00.xmp".to_string()), "{can_list} {photos}");
        assert!(fs.xmp_asked.get() >= photos * 4, "each photo is probed: {}", fs.xmp_asked.get());
        assert_eq!(fs.listings.get(), usize::from(can_list && photos >= LIST_FOLDER_AT) + usize::from(!can_list), "{can_list} {photos}");
        let _ = std::fs::remove_dir_all(from.parent().unwrap());
    }
}
