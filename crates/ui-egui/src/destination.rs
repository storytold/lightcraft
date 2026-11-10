//! How a drop's destination is named. A name alone is enough until two rows share it (two folders
//! called `Home` on different disks, an album and another album of that name in a folder): then
//! the label carries where the row sits, so the person can tell which one a drop would reach in
//! the dragged label, the confirmation and the toast.

use lightcraft_catalog::{Album, AlbumId, Catalog, FolderNode};

/// Longest parent path shown, in characters: longer ones keep their last names behind `…/`.
const MAX_PARENT: usize = 40;

/// The last names of `parent` that fit [`MAX_PARENT`], behind `…/` when names were left out.
fn shorten(parent: &str) -> String {
    if parent.chars().count() <= MAX_PARENT {
        return parent.to_string();
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut len = 2; // the `…/`
    for name in parent.rsplit(['/', '\\']).filter(|n| !n.is_empty()) {
        let add = name.chars().count() + usize::from(!kept.is_empty());
        if len + add > MAX_PARENT && !kept.is_empty() {
            break;
        }
        len += add;
        kept.push(name);
    }
    kept.reverse();
    format!("…/{}", kept.join("/"))
}

/// The last name of a folder's path.
fn base(path: &str) -> Option<&str> {
    path.rsplit(['/', '\\']).find(|n| !n.is_empty())
}

/// The name a Folders row shows: a disk's own (`photos on tokyo`, where its path ends in a mount token such as
/// `smb-share:server=tokyo,share=photos`), any other folder's last name.
fn shown(n: &FolderNode) -> &str {
    if n.volume { &n.name } else { base(&n.path).unwrap_or(&n.name) }
}

/// The row of the Folders tree at `path`.
fn node_at<'a>(nodes: &'a [FolderNode], path: &str) -> Option<&'a FolderNode> {
    nodes.iter().find_map(|n| if n.path == path { Some(n) } else { node_at(&n.children, path) })
}

/// The paths of the other rows of the Folders tree that are folders shown as `name`.
fn same_named<'a>(nodes: &'a [FolderNode], path: &str, name: &str, out: &mut Vec<&'a str>) {
    for n in nodes {
        if n.selectable && n.path != path && shown(n) == name {
            out.push(&n.path);
        }
        same_named(&n.children, path, name, out);
    }
}

/// The folder a path sits in, without the trailing separator; `None` at the root of a disk.
fn parent(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches(['/', '\\']);
    trimmed.rfind(['/', '\\']).filter(|at| *at > 0).map(|at| &trimmed[..at])
}

/// How the folder at `path` is called as a destination: the name its row shows, and, when another row of the
/// Folders tree has that name, the folder it is in (`Home (/Users/me/Pictures)`). A long parent is
/// cut to its last names, unless that would make it read like another folder's of the same name
/// (one the cut path is the end of, or that is cut to the same names): then it is given whole.
pub fn folder(tree: &[FolderNode], path: &str) -> String {
    let name = node_at(tree, path).map_or_else(|| base(path).unwrap_or(path), shown);
    let mut others = Vec::new();
    same_named(tree, path, name, &mut others);
    if others.is_empty() {
        return name.to_string();
    }
    let Some(parent) = parent(path) else { return name.to_string() };
    let short = shorten(parent);
    // cut, it would read like another folder's path that ends in the same names (a mirror on another disk;
    // compared with one kind of separator, as `shorten` writes it)
    let tail = short.strip_prefix("…/");
    let clash = tail.is_some_and(|tail| {
        others.iter().filter_map(|o| self::parent(o)).any(|o| {
            let o = o.replace('\\', "/");
            o != parent.replace('\\', "/") && (o == tail || o.strip_suffix(tail).is_some_and(|head| head.ends_with('/')))
        })
    });
    format!("{name} ({})", if clash { parent } else { &short })
}

/// The folders an album sits in, outermost first (`Travel 2026 › Trips`); empty at the top level.
fn album_parents(cat: &Catalog, album: &Album) -> String {
    let mut names = Vec::new();
    let mut at: Option<AlbumId> = album.parent;
    // (a cycle can't be saved, but a walk must end whatever the data holds)
    while let Some(id) = at.filter(|_| names.len() < 64) {
        let Some(parent) = cat.album(id) else { break };
        names.push(parent.name.as_str());
        at = parent.parent;
    }
    names.reverse();
    names.join(" › ")
}

/// How `album` is called as a destination: its name, and, when another album or album folder has
/// that name somewhere else, the folders it is in (`Garden (Travel 2026)`). Twins in the same place
/// have nothing to tell them apart by, and keep the plain name; so does one at the top level.
pub fn album(cat: &Catalog, album: &Album) -> String {
    if !cat.albums().any(|o| o.id != album.id && o.name == album.name && o.parent != album.parent) {
        return album.name.clone();
    }
    let parents = album_parents(cat, album);
    if parents.is_empty() { album.name.clone() } else { format!("{} ({parents})", album.name) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(path: &str, children: Vec<FolderNode>) -> FolderNode {
        FolderNode {
            name: base(path).unwrap_or("/").into(),
            path: path.into(),
            count: 1,
            own: 1,
            volume: false,
            selectable: true,
            label: None,
            children,
        }
    }

    #[test]
    fn a_folder_name_nobody_shares_stays_plain() {
        let tree = vec![node("/a", vec![node("/a/Home", vec![]), node("/a/Trip", vec![])])];
        assert_eq!(folder(&tree, "/a/Home"), "Home");
    }

    #[test]
    fn two_folders_with_one_name_are_told_apart_by_where_they_are() {
        let tree = vec![node("/a", vec![node("/a/Home", vec![])]), node("/b/pics", vec![node("/b/pics/Home", vec![])])];
        assert_eq!(folder(&tree, "/a/Home"), "Home (/a)");
        assert_eq!(folder(&tree, "/b/pics/Home"), "Home (/b/pics)");
    }

    #[test]
    fn a_folder_of_the_same_name_that_cannot_be_dropped_on_does_not_count() {
        let mut other = node("/b/Home", vec![]);
        other.selectable = false;
        let tree = vec![node("/a/Home", vec![]), other];
        assert_eq!(folder(&tree, "/a/Home"), "Home");
    }

    #[test]
    fn two_disks_with_one_name_are_told_apart_by_where_they_are_mounted() {
        let mut a = node("/Volumes/photos", vec![]);
        let mut b = node("/mnt/photos", vec![]);
        a.volume = true;
        b.volume = true;
        let tree = vec![a, b];
        assert_eq!(folder(&tree, "/Volumes/photos"), "photos (/Volumes)");
        assert_eq!(folder(&tree, "/mnt/photos"), "photos (/mnt)");
    }

    #[test]
    fn a_disk_is_named_as_its_row_shows_it_not_by_its_mount_token() {
        let mut gvfs = node("/run/user/1000/gvfs/smb-share:server=tokyo,share=photos", vec![]);
        gvfs.volume = true;
        gvfs.name = "photos on tokyo".into();
        let tree = vec![gvfs, node("/a/Home", vec![])];
        assert_eq!(folder(&tree, "/run/user/1000/gvfs/smb-share:server=tokyo,share=photos"), "photos on tokyo");
        // two rows showing that name still say where each is
        let mut other = node("/mnt/photos on tokyo", vec![]);
        other.volume = true;
        let mut again = tree[0].clone();
        again.children.clear();
        let tree = vec![again, other];
        assert_eq!(folder(&tree, "/mnt/photos on tokyo"), "photos on tokyo (/mnt)");
    }

    #[test]
    fn a_long_parent_keeps_its_last_names() {
        let deep = "/Volumes/tokyo/vault/photos/Asian/Japanese/Archive/2019";
        let tree = vec![node("/x/Home", vec![]), node(&format!("{deep}/Home"), vec![])];
        let label = folder(&tree, &format!("{deep}/Home"));
        assert_eq!(label, "Home (…/photos/Asian/Japanese/Archive/2019)");
        assert!(shorten("/a/b").len() < 10);
        // one name longer than the room is kept whole rather than dropped
        let long = "x".repeat(60);
        assert_eq!(shorten(&format!("/p/{long}")), format!("…/{long}"));
    }

    #[test]
    fn two_long_parents_that_differ_only_near_the_start_are_given_whole() {
        let a = "/Volumes/NAS-A/Photography/Archive/Lightroom/2019";
        let b = "/Volumes/NAS-B/Photography/Archive/Lightroom/2019";
        let tree = vec![node(&format!("{a}/Home"), vec![]), node(&format!("{b}/Home"), vec![])];
        // cut to its last names the two would read the same
        assert_eq!(shorten(a), shorten(b));
        assert_eq!(folder(&tree, &format!("{a}/Home")), format!("Home ({a})"));
        assert_eq!(folder(&tree, &format!("{b}/Home")), format!("Home ({b})"));
    }

    #[test]
    fn a_long_parent_is_given_whole_when_a_short_path_ends_in_the_same_names() {
        let mirror = "/Volumes/Backup/Users/me/Pictures/Lightroom/2019";
        let home = "/Users/me/Pictures/Lightroom/2019";
        let tree = vec![node(&format!("{mirror}/Home"), vec![]), node(&format!("{home}/Home"), vec![])];
        assert_eq!(folder(&tree, &format!("{mirror}/Home")), format!("Home ({mirror})"));
        // (the short one is shown whole already)
        assert_eq!(folder(&tree, &format!("{home}/Home")), format!("Home ({home})"));
    }

    #[test]
    fn a_long_windows_parent_is_cut_at_its_backslashes() {
        let deep = r"C:\Users\me\Pictures\Lightroom\Archive\2019";
        let tree = vec![node("/x/Home", vec![]), node(&format!("{deep}\\Home"), vec![])];
        assert_eq!(folder(&tree, &format!("{deep}\\Home")), "Home (…/me/Pictures/Lightroom/Archive/2019)");
    }

    #[test]
    fn two_long_windows_parents_that_end_in_the_same_names_are_given_whole() {
        let backup = r"D:\Backup\Users\me\Pictures\Lightroom\Archive\2019";
        let home = r"C:\Users\me\Pictures\Lightroom\Archive\2019";
        let tree = vec![node(&format!("{backup}\\Home"), vec![]), node(&format!("{home}\\Home"), vec![])];
        assert_eq!(folder(&tree, &format!("{backup}\\Home")), format!("Home ({backup})"));
        assert_eq!(folder(&tree, &format!("{home}\\Home")), format!("Home ({home})"));
    }

    #[test]
    fn a_folder_at_the_root_of_a_disk_has_no_parent_to_show() {
        let tree = vec![node("/Home", vec![]), node("/b/Home", vec![])];
        assert_eq!(folder(&tree, "/Home"), "Home");
    }

    fn album_in(cat: &mut Catalog, name: &str, parent: Option<AlbumId>, folder: bool) -> AlbumId {
        let id = cat.alloc_album_id();
        let mut a = Album::new(id, name);
        a.parent = parent;
        a.folder = folder;
        cat.apply(lightcraft_catalog::Op::AddAlbum { album: a }).unwrap();
        id
    }

    #[test]
    fn albums_with_one_name_in_different_folders_say_which() {
        let mut cat = Catalog::new();
        let trips = album_in(&mut cat, "Trips", None, true);
        let a = album_in(&mut cat, "Garden", Some(trips), false);
        let b = album_in(&mut cat, "Garden", None, false);
        let solo = album_in(&mut cat, "Portfolio", Some(trips), false);
        assert_eq!(album(&cat, cat.album(a).unwrap()), "Garden (Trips)");
        // the one at the top level is the plain name, so the two differ
        assert_eq!(album(&cat, cat.album(b).unwrap()), "Garden");
        assert_eq!(album(&cat, cat.album(solo).unwrap()), "Portfolio");
    }

    #[test]
    fn albums_with_one_name_in_the_same_place_cannot_be_told_apart_and_stay_plain() {
        let mut cat = Catalog::new();
        let a = album_in(&mut cat, "Garden", None, false);
        album_in(&mut cat, "Garden", None, false);
        assert_eq!(album(&cat, cat.album(a).unwrap()), "Garden");
    }

    #[test]
    fn albums_with_one_name_twice_in_one_folder_stay_plain_but_say_so_against_another_folder() {
        let mut cat = Catalog::new();
        let trips = album_in(&mut cat, "Trips", None, true);
        let a = album_in(&mut cat, "Garden", Some(trips), false);
        album_in(&mut cat, "Garden", Some(trips), false);
        assert_eq!(album(&cat, cat.album(a).unwrap()), "Garden");
        let elsewhere = album_in(&mut cat, "Garden", None, false);
        assert_eq!(album(&cat, cat.album(a).unwrap()), "Garden (Trips)");
        assert_eq!(album(&cat, cat.album(elsewhere).unwrap()), "Garden");
    }
}
