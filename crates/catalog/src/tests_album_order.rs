//! Album order: the children of a folder (or of the top level) are listed folders first, then
//! alphabetically, until some of them are given a place of their own (`Album.order`), which mixes
//! folders and albums as they were put; moving an
//! album to another folder drops its place, and undoing the move brings it back.

use crate::*;

fn add(c: &mut Catalog, name: &str, parent: Option<AlbumId>, folder: bool) -> AlbumId {
    let id = c.alloc_album_id();
    c.apply(Op::AddAlbum { album: Album { parent, folder, ..Album::new(id, name) } }).unwrap();
    id
}

fn names(c: &Catalog, parent: Option<AlbumId>) -> Vec<String> {
    c.album_children(parent).iter().map(|a| a.name.clone()).collect()
}

fn place(c: &mut Catalog, id: AlbumId, order: Option<u32>) {
    c.apply(Op::SetAlbumOrder { id, order }).unwrap();
}

/// Given albums that were never ordered, when they are listed, then folders come first and each
/// group is alphabetical regardless of case.
#[test]
fn children_are_folders_first_then_alphabetical() {
    let mut c = Catalog::new();
    add(&mut c, "beta", None, false);
    add(&mut c, "Alpha", None, false);
    add(&mut c, "zoo", None, true);
    add(&mut c, "Archive", None, true);
    assert_eq!(names(&c, None), ["Archive", "zoo", "Alpha", "beta"]);
    assert!(!c.album_children_are_ordered(None));
}

/// Given some children with a place of their own, then they follow it; the others come after,
/// folders first, by name.
#[test]
fn ordered_children_follow_their_order() {
    let mut c = Catalog::new();
    let a = add(&mut c, "A", None, false);
    let b = add(&mut c, "B", None, false);
    let cc = add(&mut c, "C", None, false);
    add(&mut c, "D", None, false);
    let f = add(&mut c, "F", None, true);
    place(&mut c, cc, Some(0));
    place(&mut c, a, Some(1));
    assert_eq!(names(&c, None), ["C", "A", "F", "B", "D"]);
    assert!(c.album_children_are_ordered(None));
    // equal places fall back to the name, so a damaged catalog still lists deterministically
    place(&mut c, b, Some(1));
    assert_eq!(names(&c, None), ["C", "A", "B", "F", "D"]);
    // another container is unaffected
    let inner = add(&mut c, "Z", Some(f), false);
    add(&mut c, "Y", Some(f), false);
    assert_eq!(names(&c, Some(f)), ["Y", "Z"]);
    assert!(!c.album_children_are_ordered(Some(f)));
    place(&mut c, inner, Some(0));
    assert_eq!(names(&c, Some(f)), ["Z", "Y"]);
}

/// Given folders and albums with places of their own, then they are listed by them, mixed: an
/// album can come before a folder.
#[test]
fn placed_albums_and_folders_mix() {
    let mut c = Catalog::new();
    let f = add(&mut c, "Folder", None, true);
    let g = add(&mut c, "Gallery", None, true);
    let a = add(&mut c, "Album", None, false);
    let b = add(&mut c, "Best", None, false);
    for (i, id) in [a, f, b, g].into_iter().enumerate() {
        place(&mut c, id, Some(u32::try_from(i).unwrap()));
    }
    assert_eq!(names(&c, None), ["Album", "Folder", "Best", "Gallery"]);
    let map = c.album_children_by_parent();
    let from_map: Vec<&str> = map[&None].iter().map(|a| a.name.as_str()).collect();
    assert_eq!(from_map, ["Album", "Folder", "Best", "Gallery"]);
}

/// The one-pass map lists every folder's children exactly as `album_children` does.
#[test]
fn the_children_map_matches_the_children_of_each_folder() {
    let mut c = Catalog::new();
    let f = add(&mut c, "F", None, true);
    let g = add(&mut c, "G", Some(f), true);
    for (name, parent) in [("b", None), ("a", None), ("y", Some(f)), ("x", Some(f)), ("q", Some(g))] {
        add(&mut c, name, parent, false);
    }
    let x = c.albums().find(|a| a.name == "x").unwrap().id;
    place(&mut c, x, Some(7));
    let map = c.album_children_by_parent();
    for parent in [None, Some(f), Some(g)] {
        let from_map: Vec<AlbumId> = map.get(&parent).map(|v| v.iter().map(|a| a.id).collect()).unwrap_or_default();
        let direct: Vec<AlbumId> = c.album_children(parent).iter().map(|a| a.id).collect();
        assert_eq!(from_map, direct, "{parent:?}");
    }
    assert!(!map.contains_key(&Some(AlbumId(999))));
}

/// A place is one undo step, and `None` puts the album back among the alphabetical ones.
#[test]
fn a_place_can_be_set_cleared_and_undone() {
    let mut c = Catalog::new();
    let a = add(&mut c, "A", None, false);
    add(&mut c, "B", None, false);
    let inv = c.apply(Op::SetAlbumOrder { id: a, order: Some(5) }).unwrap();
    assert_eq!(c.album(a).unwrap().order, Some(5));
    c.apply(inv).unwrap();
    assert_eq!(c.album(a).unwrap().order, None);
    assert!(c.apply(Op::SetAlbumOrder { id: AlbumId(999), order: Some(1) }).is_err(), "no such album");
}

/// Moving to another folder drops the album's place there (it belonged to the old neighbours),
/// and undoing the move gives it back.
#[test]
fn moving_drops_the_place_and_undo_restores_it() {
    let mut c = Catalog::new();
    let from = add(&mut c, "From", None, true);
    let to = add(&mut c, "To", None, true);
    let a = add(&mut c, "A", Some(from), false);
    place(&mut c, a, Some(3));
    let inv = c.apply(Op::MoveAlbum { id: a, parent: Some(to) }).unwrap();
    assert_eq!((c.album(a).unwrap().parent, c.album(a).unwrap().order), (Some(to), None));
    c.apply(inv).unwrap();
    assert_eq!((c.album(a).unwrap().parent, c.album(a).unwrap().order), (Some(from), Some(3)));
    // "moving" to where it already is changes nothing
    c.apply(Op::MoveAlbum { id: a, parent: Some(from) }).unwrap();
    assert_eq!(c.album(a).unwrap().order, Some(3));
}

/// Catalogs written before albums had an order load as they were; an unset order is not written.
#[test]
fn albums_without_an_order_load_and_save_unchanged() {
    let old = r#"{"id":4,"name":"Old","parent":null,"folder":false,"photos":[],"cover":null}"#;
    let a: Album = serde_json::from_str(old).unwrap();
    assert_eq!(a.order, None);
    assert!(!serde_json::to_string(&a).unwrap().contains("order"));
    let mut a = a;
    a.order = Some(2);
    let back: Album = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
    assert_eq!(back.order, Some(2));
}
