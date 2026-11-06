//! The library's folder tree: where the imported photos live on disk (see [`crate::folders`]).
//!
//! Scenarios, in the words of someone browsing their library:
//!
//! * Given photos imported from several folders, the tree lists those folders and no others,
//!   each with how many photos it holds, under the disk (volume) they are on.
//! * A folder's count includes the folders inside it.
//! * Every folder is a row of its own, the ones above the imported photos included.
//! * Photos only browsed in Local, deleted photos and demo scenes are not "imported from" anywhere.
//! * However a folder is spelled, it is one folder.
//! * Choosing a folder shows what was imported from it and from the folders inside it — exactly
//!   as many photos as the row says.

use crate::*;

fn add(c: &mut Catalog, path: &str) -> PhotoId {
    add_with(c, path, |_| {})
}

fn add_with(c: &mut Catalog, path: &str, tweak: impl FnOnce(&mut Photo)) -> PhotoId {
    let id = c.alloc_photo_id();
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let mut p = Photo::new(id, Source::File { path: path.into() }, name, "JPEG", 60, 40, "2026-01-01T10:00:00");
    tweak(&mut p);
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

fn library(paths: &[&str]) -> Catalog {
    let mut c = Catalog::new();
    paths.iter().for_each(|p| {
        add(&mut c, p);
    });
    c
}

/// `(name, count, own)` of each node, depth first.
fn outline(nodes: &[FolderNode]) -> Vec<(String, usize, usize)> {
    fn walk(n: &FolderNode, out: &mut Vec<(String, usize, usize)>) {
        out.push((n.name.clone(), n.count, n.own));
        n.children.iter().for_each(|c| walk(c, out));
    }
    let mut out = Vec::new();
    nodes.iter().for_each(|n| walk(n, &mut out));
    out
}

fn row(name: &str, count: usize, own: usize) -> (String, usize, usize) {
    (name.to_string(), count, own)
}

fn rows(paths: &[&str]) -> Vec<(String, usize, usize)> {
    outline(&library(paths).folder_tree())
}

#[test]
fn an_empty_library_has_no_folders() {
    assert!(Catalog::new().folder_tree().is_empty());
}

#[test]
fn only_folders_with_imported_photos_are_listed_with_their_counts() {
    let c = library(&["/pics/trip/a.jpg", "/pics/trip/b.jpg", "/pics/home/c.jpg"]);
    let tree = c.folder_tree();
    assert_eq!(outline(&tree), vec![row("/", 3, 0), row("pics", 3, 0), row("home", 1, 1), row("trip", 2, 2)]);
    assert!(tree[0].volume && !tree[0].children[0].volume);
    assert_eq!(tree[0].children[0].children[1].path, "/pics/trip", "a row knows its folder, for choosing it");
}

#[test]
fn a_folders_count_includes_the_folders_inside_it() {
    let r = rows(&["/pics/trip/a.jpg", "/pics/trip/day1/b.jpg", "/pics/trip/day1/c.jpg", "/pics/trip/day2/d.jpg"]);
    assert_eq!(r, vec![row("/", 4, 0), row("pics", 4, 0), row("trip", 4, 1), row("day1", 2, 2), row("day2", 1, 1)]);
}

#[test]
fn every_folder_is_a_row_of_its_own_even_when_it_holds_nothing_itself() {
    let c = library(&["/Users/me/Pictures/2024/a.jpg", "/Users/me/Pictures/2025/b.jpg", "/Volumes/nas/photos/c.jpg"]);
    let tree = c.folder_tree();
    assert_eq!(
        outline(&tree),
        vec![
            row("/", 2, 0),
            row("Users", 2, 0),
            row("me", 2, 0),
            row("Pictures", 2, 0),
            row("2024", 1, 1),
            row("2025", 1, 1),
            row("nas", 1, 0),
            row("photos", 1, 1)
        ]
    );
    assert_eq!(tree[0].children[0].children[0].children[0].path, "/Users/me/Pictures", "a row is exactly the folder it names");
}

#[test]
fn a_folder_with_photos_of_its_own_is_not_folded_into_its_child() {
    assert_eq!(rows(&["/pics/a.jpg", "/pics/trip/b.jpg"]), vec![row("/", 2, 0), row("pics", 2, 1), row("trip", 1, 1)]);
}

#[test]
fn browsed_deleted_and_demo_photos_are_not_imported_from_anywhere() {
    let mut c = Catalog::new();
    add(&mut c, "/pics/kept/a.jpg");
    add_with(&mut c, "/pics/browsed/b.jpg", |p| p.local = true);
    add_with(&mut c, "/pics/gone/c.jpg", |p| p.deleted = true);
    let id = c.alloc_photo_id();
    let demo = Photo::new(id, Source::Demo { scene: 1 }, "demo", "JPEG", 60, 40, "2026-01-01T10:00:00");
    c.apply(Op::AddPhoto { photo: Box::new(demo) }).unwrap();
    assert_eq!(outline(&c.folder_tree()), vec![row("/", 1, 0), row("pics", 1, 0), row("kept", 1, 1)]);
}

#[test]
fn however_a_folder_is_spelled_it_is_one_folder() {
    let r = rows(&["/pics/trip/a.jpg", "/pics//trip/./b.jpg", "/pics\\trip\\c.jpg", "/pics/x/../trip/d.jpg"]);
    assert_eq!(r, vec![row("/", 4, 0), row("pics", 4, 0), row("trip", 4, 4)]);
}

#[test]
fn each_disk_is_a_volume_of_its_own() {
    // macOS, Linux and Windows habits for where disks appear
    let r = rows(&["/Volumes/nas/p/1.jpg", "/media/me/usb/2.jpg", "/run/media/me/card/3.jpg", "/mnt/disk/4.jpg"]);
    assert_eq!(r, vec![row("card", 1, 1), row("disk", 1, 1), row("nas", 1, 0), row("p", 1, 1), row("usb", 1, 1)]);
    let r = rows(&["D:\\Photos\\2024\\a.jpg", "D:\\Photos\\2025\\b.jpg", "C:\\x\\c.jpg"]);
    assert_eq!(r, vec![row("C:", 1, 0), row("x", 1, 1), row("D:", 2, 0), row("Photos", 2, 0), row("2024", 1, 1), row("2025", 1, 1)]);
}

#[test]
fn a_network_share_is_a_volume_and_two_spellings_are_one() {
    let r = rows(&["\\\\srv\\s1\\a\\1.jpg", "\\\\srv\\s2\\b\\2.jpg"]);
    assert_eq!(r, vec![row("//srv/s1", 1, 0), row("a", 1, 1), row("//srv/s2", 1, 0), row("b", 1, 1)]);
    // the verbatim form Windows APIs give is the same share
    let r = rows(&["\\\\?\\UNC\\srv\\s1\\a\\1.jpg", "\\\\srv\\s1\\a\\2.jpg"]);
    assert_eq!(r, vec![row("//srv/s1", 2, 0), row("a", 2, 2)]);
}

#[test]
fn a_samba_share_mounted_on_a_mac_is_a_volume_named_after_the_share() {
    // Finder mounts every share under /Volumes, and a second mount of the same share gets a suffix
    let r = rows(&["/Volumes/tokyo/Photos/2024/a.jpg", "/Volumes/tokyo/Photos/2025/b.jpg", "/Volumes/tokyo-1/old/c.jpg", "/Volumes/東京 NAS/d.jpg"]);
    assert_eq!(
        r,
        vec![
            row("tokyo", 2, 0),
            row("Photos", 2, 0),
            row("2024", 1, 1),
            row("2025", 1, 1),
            row("tokyo-1", 1, 0),
            row("old", 1, 1),
            row("東京 NAS", 1, 1)
        ]
    );
    let tree = library(&["/Volumes/tokyo/Photos/a.jpg"]).folder_tree();
    assert_eq!((tree[0].path.as_str(), tree[0].volume), ("/Volumes/tokyo", true), "choosing the share shows everything on it");
}

#[test]
fn shares_the_other_systems_mount_are_volumes_too() {
    // Linux desktops: gvfs mounts per user, with the server and share in the mount's name
    let gvfs = "/run/user/1000/gvfs/smb-share:server=tokyo,share=photos";
    let r = rows(&[&format!("{gvfs}/2024/a.jpg"), "/run/user/1000/gvfs/sftp:host=nas/b.jpg"]);
    assert_eq!(r, vec![row("photos on tokyo", 1, 0), row("2024", 1, 1), row("sftp:host=nas", 1, 1)]);
    // NFS automounts: Linux /net/<host>, macOS /Network/Servers/<host>
    let r = rows(&["/net/tokyo/export/a.jpg", "/Network/Servers/osaka/vol/b.jpg"]);
    assert_eq!(r, vec![row("osaka", 1, 0), row("vol", 1, 1), row("tokyo", 1, 0), row("export", 1, 1)]);
    // Windows: a share by name or by address, the WSL file system, a mapped drive
    let r = rows(&[r"\\tokyo.local\photos\a.jpg", r"\\192.168.1.5\scans\b.jpg", r"\\wsl$\Ubuntu\home\me\c.jpg", r"Z:\d.jpg"]);
    assert_eq!(
        r,
        vec![
            row("//192.168.1.5/scans", 1, 1),
            row("//tokyo.local/photos", 1, 1),
            row("//wsl$/Ubuntu", 1, 0),
            row("home", 1, 0),
            row("me", 1, 1),
            row("Z:", 1, 1)
        ]
    );
}

#[test]
fn a_share_that_is_not_mounted_costs_nothing() {
    // the tree is made from the catalog's own records: it never looks at a disk, so a share that
    // is asleep or gone lists (and counts) exactly like one that is there
    let c = library(&["/Volumes/definitely-not-mounted/a/b.jpg", r"\\unreachable.invalid\share\c.jpg"]);
    let tree = c.folder_tree();
    assert_eq!(tree.iter().map(|n| n.count).sum::<usize>(), 2);
}

#[test]
fn a_photo_in_the_root_of_a_disk_counts_for_the_disk() {
    assert_eq!(rows(&["/x.jpg", "/d/y.jpg"]), vec![row("/", 2, 1), row("d", 1, 1)]);
    assert_eq!(rows(&["C:\\x.jpg"]), vec![row("C:", 1, 1)]);
}

#[test]
fn photos_with_no_folder_are_left_out() {
    assert!(rows(&["x.jpg", ""]).is_empty());
}

#[test]
fn folders_are_listed_by_name_ignoring_case() {
    let names: Vec<String> = rows(&["/p/b/1.jpg", "/p/A/2.jpg", "/p/c/3.jpg"]).into_iter().skip(2).map(|r| r.0).collect();
    assert_eq!(names, ["A", "b", "c"]);
}

#[test]
fn absurdly_deep_paths_neither_crash_nor_lose_photos() {
    let deep = format!("/{}/a.jpg", vec!["d"; 5000].join("/"));
    let tree = library(&[&deep]).folder_tree();
    assert_eq!(tree.iter().map(|n| n.count).sum::<usize>(), 1);
    assert!(outline(&tree).len() <= crate::folders::MAX_DEPTH + 2);
}

#[test]
fn choosing_a_folder_shows_what_was_imported_from_it_and_below() {
    let mut c = Catalog::new();
    let trip = add(&mut c, "/pics/trip/a.jpg");
    let day = add(&mut c, "/pics/trip/day1/b.jpg");
    let sibling = add(&mut c, "/pics/trip2/c.jpg"); // shares the name's start, not the folder
    let browsed = add_with(&mut c, "/pics/trip/d.jpg", |p| p.local = true);
    let f = Filter { library_folder: Some("/pics//trip/".into()), ..Default::default() };
    let shown = |id: PhotoId| f.matches(c.photo(id).unwrap(), &c);
    assert!(shown(trip) && shown(day));
    assert!(!shown(sibling), "/pics/trip2 is another folder");
    assert!(!shown(browsed), "browsed photos are not part of the library's folders");
    assert_eq!(f.describe(), "folder /pics//trip/");
}

#[test]
fn recently_deleted_shows_the_deleted_photos_of_the_chosen_folder() {
    let mut c = Catalog::new();
    let gone = add_with(&mut c, "/pics/trip/a.jpg", |p| p.deleted = true);
    let kept = add(&mut c, "/pics/trip/b.jpg");
    let f = Filter { library_folder: Some("/pics/trip".into()), deleted: true, ..Default::default() };
    assert!(f.matches(c.photo(gone).unwrap(), &c));
    assert!(!f.matches(c.photo(kept).unwrap(), &c));
}

#[test]
fn a_deleted_browsed_photo_shows_in_recently_deleted_and_comes_back() {
    let mut c = Catalog::new();
    let browsed = add_with(&mut c, "/pics/trip/a.jpg", |p| p.local = true);
    let recently_deleted = Filter { deleted: true, ..Default::default() };
    let library = Filter::default();
    assert!(!recently_deleted.matches(c.photo(browsed).unwrap(), &c));
    assert!(!library.matches(c.photo(browsed).unwrap(), &c), "browsed photos stay out of the library view");

    c.apply(Op::SetDeleted { id: browsed, deleted: true }).unwrap();
    assert_eq!(c.query(&recently_deleted, &Sort::default()), vec![browsed], "#1430: it was invisible everywhere");
    assert!(c.query(&library, &Sort::default()).is_empty());

    c.apply(Op::SetDeleted { id: browsed, deleted: false }).unwrap();
    assert!(c.query(&recently_deleted, &Sort::default()).is_empty());
}

#[test]
fn an_empty_folder_choice_is_no_choice() {
    let c = library(&["/pics/a.jpg"]);
    let f = Filter { library_folder: Some("  ".into()), ..Default::default() };
    assert_eq!(c.query(&f, &Sort::default()).len(), 1);
}

#[test]
fn every_row_shows_exactly_as_many_photos_as_it_says() {
    let c = library(&[
        "/Users/me/Pictures/2024/a.jpg",
        "/Users/me/Pictures/2024/b.jpg",
        "/Users/me/Pictures/2025/c.jpg",
        "/Users/me/Pictures/d.jpg",
        "/Volumes/nas/photos/e.jpg",
        "/Volumes/nas/f.jpg",
        "/x.jpg",
        "/a/../b/g.jpg",
        "D:\\Photos\\2024\\h.jpg",
        "d:/Photos/i.jpg",
        "\\\\srv\\s1\\a\\j.jpg",
        "\\\\?\\UNC\\srv\\s2\\k.jpg",
        "/pics/trip/l.jpg",
        "/pics/trip2/m.jpg",
        "/Volumes/tokyo/Photos/2024/n.jpg",
        "/Volumes/tokyo/o.jpg",
        "/run/user/1000/gvfs/smb-share:server=tokyo,share=photos/p.jpg",
        "/net/osaka/export/q.jpg",
        r"\\wsl$\Ubuntu\home\me\r.jpg",
        // odd spellings the tree and the filter must read the same way
        "//server/share/../other/1.jpg",
        "//server/share/ok/2.jpg",
        "//a b//A/f.jpg",
        r"\\wsl$\Ubuntu\..\a\1.jpg",
        // a folder that holds other disks as well as photos of its own
        "/Volumes/1.jpg",
        "/mnt/2.jpg",
        "/mnt/disk/3.jpg",
    ]);
    fn check(c: &Catalog, nodes: &[FolderNode], seen: &mut usize) {
        for n in nodes {
            // the startup volume's row only opens: its path "/" would cover every other disk
            if n.selectable {
                let f = Filter { library_folder: Some(n.path.clone()), ..Default::default() };
                assert_eq!(c.query(&f, &Sort::default()).len(), n.count, "{} ({})", n.name, n.path);
                *seen += 1;
            }
            check(c, &n.children, seen);
        }
    }
    let mut seen = 0;
    check(&c, &c.folder_tree(), &mut seen);
    assert!(seen > 10, "{seen} rows checked");
}

fn find<'a>(nodes: &'a [FolderNode], path: &str) -> Option<&'a FolderNode> {
    nodes.iter().find_map(|n| if n.path == path { Some(n) } else { find(&n.children, path) })
}

#[test]
fn a_row_that_would_cover_other_disks_is_not_offered_for_choosing() {
    let tree = library(&["/Users/me/a.jpg", "/Volumes/1.jpg", "/Volumes/tokyo/x.jpg", "/mnt/2.jpg"]).folder_tree();
    let sel = |p: &str| find(&tree, p).map(|n| n.selectable);
    assert_eq!(sel("/"), Some(false), "the startup disk's path covers every disk");
    assert_eq!(sel("/Volumes"), Some(false), "it holds the tokyo disk");
    assert_eq!(sel("/Volumes/tokyo"), Some(true));
    assert_eq!(sel("/Users/me"), Some(true));
    assert_eq!(sel("/mnt"), Some(true), "no disk is mounted below it in this library");
}

#[test]
fn the_tree_and_the_filter_read_odd_spellings_the_same_way() {
    // `..` cannot climb above a share, and repeated separators between server and share are one
    let tree = library(&["//server/share/../other/1.jpg", "//a b//A/f.jpg"]).folder_tree();
    assert_eq!(outline(&tree), vec![row("//a b/A", 1, 1), row("//server/share", 1, 0), row("other", 1, 1)]);
}

#[test]
fn a_path_that_names_no_folder_chooses_nothing() {
    let c = library(&["/pics/a.jpg", "C:\\b\\c.jpg"]);
    for dot in [".", "./", "a/..", "../.."] {
        let f = Filter { library_folder: Some(dot.into()), ..Default::default() };
        assert_eq!(c.query(&f, &Sort::default()).len(), 0, "{dot:?} is no folder, not everything");
    }
}

#[test]
fn a_folder_is_labelled_by_its_last_two_names() {
    use crate::folders::folder_label;
    assert_eq!(folder_label("/Volumes/tokyo/photos/travel"), "photos/travel");
    assert_eq!(folder_label("/Volumes/tokyo/"), "Volumes/tokyo");
    assert_eq!(folder_label(r"C:\Photos\2024"), "Photos/2024");
    assert_eq!(folder_label("/"), "/");
    assert_eq!(folder_label(""), "");
}

#[test]
fn disk_roots_are_told_from_folders() {
    use crate::folders::{is_disk_root, is_startup_disk};
    for p in ["/", "C:", "C:\\", r"\\?\C:\", r"\\srv\share", "/Volumes/nas", "/mnt/disk", "/net/host"] {
        assert!(is_disk_root(p), "{p}");
    }
    for p in ["/Volumes", "/Users", "C:\\Photos", r"\\srv\share\x", "/Volumes/nas/p", "."] {
        assert!(!is_disk_root(p), "{p}");
    }
    assert!(is_startup_disk("/") && is_startup_disk("//") && !is_startup_disk("/Volumes/nas") && !is_startup_disk("C:"));
}

#[test]
fn a_folder_name_with_spaces_at_its_edge_is_its_own_folder() {
    // legal on macOS and Linux: `/p/shoot ` is not `/p/shoot`
    let c = library(&["/p/shoot /a.jpg", "/p/shoot/b.jpg", "/p/other/c.jpg"]);
    let tree = c.folder_tree();
    let shoot_space = find(&tree, "/p/shoot ").expect("its own row");
    let shoot = find(&tree, "/p/shoot").expect("and the other");
    for (node, want) in [(shoot_space, "a.jpg"), (shoot, "b.jpg")] {
        let f = Filter { library_folder: Some(node.path.clone()), ..Default::default() };
        let ids = c.query(&f, &Sort::default());
        let names: Vec<&str> = ids.iter().filter_map(|id| c.photo(*id)).map(|p| p.file_name.as_str()).collect();
        assert_eq!(names, [want], "{:?} shows its own photo", node.path);
    }
}

#[test]
fn a_local_photo_never_counts_as_imported_even_when_a_disk_folder_is_also_shown() {
    let mut c = Catalog::new();
    let browsed = add_with(&mut c, "/pics/trip/b.jpg", |p| p.local = true);
    let f = Filter { folder: Some("/pics/trip".into()), library_folder: Some("/pics/trip".into()), ..Default::default() };
    assert!(!f.matches(c.photo(browsed).unwrap(), &c));
}

#[test]
fn the_quick_check_agrees_with_the_careful_one() {
    use crate::query::{folder_key, key_within, photo_in_root};
    let paths = [
        "/a/b/c.jpg",
        "/a/b/d/e.jpg",
        "/a/bc/x.jpg",
        "/a/./b/x.jpg",
        "/a/../a/b/x.jpg",
        "/a//b/x.jpg",
        "/a\\b\\x.jpg",
        "/a/b ",
        "/a/b /x.jpg",
        "/.hidden/x.jpg",
        "/a/.b/x.jpg",
        "/x.jpg",
        "//srv/share/x.jpg",
        "C:\\a\\b\\x.jpg",
        "relative/a/b/x.jpg",
        "/Volumes/tokyo/a/x.jpg",
        "/日本/写真/x.jpg",
    ];
    let roots = ["/", "/a", "/a/b", "/a/b/", "/a/bc", "/a/b ", "/Volumes/tokyo", "//srv/share", "C:\\a", "/日本", "relative", "/a/../a/b"];
    for p in paths {
        let mut c = Catalog::new();
        let id = add(&mut c, p);
        let photo = c.photo(id).unwrap();
        for r in roots {
            let root = folder_key(r);
            let slow = crate::local::folder_of(photo).is_some_and(|f| key_within(&f, &root));
            assert_eq!(photo_in_root(photo, &root), slow, "{p:?} in {r:?}");
        }
    }
}
