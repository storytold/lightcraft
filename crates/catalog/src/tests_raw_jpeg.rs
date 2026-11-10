//! RAW+JPEG pairs ([`crate::raw_jpeg`]).

use super::*;

fn add(c: &mut Catalog, path: &str, raw: bool) -> PhotoId {
    let id = c.alloc_photo_id();
    let name = path.rsplit('/').next().unwrap_or(path);
    let format = if raw { "NEF" } else { "JPEG" };
    let mut p = Photo::new(id, Source::File { path: path.into() }, name, format, 6000, 4000, "2026-11-05T10:00:00");
    p.kind = if raw { MediaKind::Raw } else { MediaKind::Image };
    if raw {
        p.set_preview_only(None);
    }
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

fn pairs(c: &Catalog, ids: &[PhotoId]) -> Vec<(PhotoId, PhotoId)> {
    c.raw_jpeg_pairs(ids).into_iter().map(|p| (p.raw, p.jpeg)).collect()
}

#[test]
fn same_folder_and_stem_pair_whatever_the_letter_case() {
    let mut c = Catalog::new();
    let raw = add(&mut c, "/card/DCIM/dsc_0001.nef", true);
    let jpg = add(&mut c, "/card/DCIM/DSC_0001.JPG", false);
    let other = add(&mut c, "/card/DCIM/DSC_0002.JPG", false);
    assert_eq!(pairs(&c, &[raw, jpg, other]), vec![(raw, jpg)], "one pair, listed once");
    assert_eq!(pairs(&c, &[jpg]), vec![(raw, jpg)], "the partner is found outside `ids`");
    assert!(pairs(&c, &[other]).is_empty());
}

#[test]
fn jpeg_and_heif_extensions_are_companions() {
    let mut c = Catalog::new();
    for (i, ext) in ["jpeg", "HIF", "heic", "heif"].iter().enumerate() {
        let raw = add(&mut c, &format!("/s/IMG_{i}.CR3"), true);
        let jpg = add(&mut c, &format!("/s/IMG_{i}.{ext}"), false);
        assert_eq!(pairs(&c, &[raw]), vec![(raw, jpg)], "{ext}");
    }
    let raw = add(&mut c, "/s/IMG_9.CR3", true);
    add(&mut c, "/s/IMG_9.png", false);
    add(&mut c, "/s/IMG_9.tif", false);
    add(&mut c, "/s/IMG_9.jpe", false);
    assert!(pairs(&c, &[raw]).is_empty(), "PNG, TIFF and JPE are not camera companions");
}

#[test]
fn different_folders_never_pair() {
    let mut c = Catalog::new();
    let raw = add(&mut c, "/shoot/DSC_0001.NEF", true);
    let jpg = add(&mut c, "/shoot/exported/DSC_0001.JPG", false);
    assert!(pairs(&c, &[raw, jpg]).is_empty());
}

#[test]
fn a_raw_or_jpeg_without_its_partner_stays_alone() {
    let mut c = Catalog::new();
    let raw = add(&mut c, "/s/DSC_0001.NEF", true);
    let jpg = add(&mut c, "/s/DSC_0002.JPG", false);
    let two = add(&mut c, "/s/DSC_0002.NEF.JPG", false);
    assert!(pairs(&c, &[raw, jpg, two]).is_empty());
}

#[test]
fn ambiguous_groups_stay_apart_but_other_files_do_not_block() {
    let mut c = Catalog::new();
    // two JPEGs for one raw: which is the camera's?
    let raw = add(&mut c, "/s/A.NEF", true);
    add(&mut c, "/s/A.JPG", false);
    add(&mut c, "/s/a.jpeg", false);
    assert!(pairs(&c, &[raw]).is_empty());
    // two raws (a NEF and a DNG made from it) for one JPEG
    let raw_b = add(&mut c, "/s/B.NEF", true);
    add(&mut c, "/s/B.dng", true);
    add(&mut c, "/s/B.JPG", false);
    assert!(pairs(&c, &[raw_b]).is_empty());
    // a TIFF edited elsewhere doesn't stop the pair and stays outside it
    let raw_c = add(&mut c, "/s/C.NEF", true);
    let jpg_c = add(&mut c, "/s/C.JPG", false);
    add(&mut c, "/s/C.tif", false);
    assert_eq!(pairs(&c, &[raw_c]), vec![(raw_c, jpg_c)]);
}

#[test]
fn deleted_copies_stacked_and_demo_photos_are_left_out() {
    let mut c = Catalog::new();
    let raw = add(&mut c, "/s/D.NEF", true);
    let jpg = add(&mut c, "/s/D.JPG", false);
    c.apply(Op::SetDeleted { id: jpg, deleted: true }).unwrap();
    assert!(pairs(&c, &[raw]).is_empty(), "a JPEG in Recently Deleted");
    c.apply(Op::SetDeleted { id: jpg, deleted: false }).unwrap();
    assert_eq!(pairs(&c, &[raw]), vec![(raw, jpg)]);
    // already stacked with a burst: left alone
    let other = add(&mut c, "/s/E.NEF", true);
    let op = c.group_ops(raw, &[other], true).unwrap();
    c.apply(op).unwrap();
    assert!(pairs(&c, &[raw, jpg]).is_empty());
    // demo photos have no folder
    let mut d = Catalog::new();
    let id = d.alloc_photo_id();
    let mut p = Photo::new(id, Source::Demo { scene: 1 }, "X.NEF", "NEF", 4, 3, "2026-01-01");
    p.kind = MediaKind::Raw;
    d.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let id2 = d.alloc_photo_id();
    d.apply(Op::AddPhoto { photo: Box::new(Photo::new(id2, Source::Demo { scene: 2 }, "X.JPG", "JPEG", 4, 3, "2026-01-01")) }).unwrap();
    assert!(pairs(&d, &[id, id2]).is_empty());
}

#[test]
fn stack_ops_put_the_raw_on_top_collapsed_and_undo_in_one_step() {
    let mut c = Catalog::new();
    let raw = add(&mut c, "/s/DSC_0001.NEF", true);
    let jpg = add(&mut c, "/s/DSC_0001.JPG", false);
    let before = c.clone();
    let found = c.raw_jpeg_pairs(&[raw]);
    let ops = c.raw_jpeg_stack_ops(&found);
    let batch = Op::Batch { ops };
    let inverse = c.apply(batch).unwrap();
    let st = c.stack_of(raw).unwrap();
    assert_eq!(st.photos, vec![raw, jpg]);
    assert!(st.collapsed);
    assert_eq!(c.arrange_stacks(&[raw, jpg]), vec![raw], "the grid shows one photo");
    assert!(c.raw_jpeg_stack_ops(&found).is_empty(), "pairing twice changes nothing");
    c.apply(inverse).unwrap();
    assert_eq!(c.stacks().count(), before.stacks().count());
    assert!(c.stack_of(raw).is_none());
}

#[test]
fn a_raw_shown_only_from_its_preview_puts_the_jpeg_on_top() {
    let mut c = Catalog::new();
    let raw = add(&mut c, "/s/DSC_0001.NEF", true);
    let jpg = add(&mut c, "/s/DSC_0001.JPG", false);
    let mut p = Photo::clone(c.photo(raw).unwrap());
    p.set_preview_only(Some("Nikon High Efficiency NEF".into()));
    c.apply(Op::RemovePhoto { id: raw }).unwrap();
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let found = c.raw_jpeg_pairs(&[raw]);
    let ops = c.raw_jpeg_stack_ops(&found);
    c.apply(Op::Batch { ops }).unwrap();
    assert_eq!(c.stack_of(raw).unwrap().photos, vec![jpg, raw]);
}

#[test]
fn local_photos_never_pair() {
    let mut c = Catalog::new();
    // a library raw and a JPEG only browsed in Local
    let raw = add(&mut c, "/s/F.NEF", true);
    let jpg = add(&mut c, "/s/F.JPG", false);
    c.apply(Op::SetLocal { id: jpg, local: true }).unwrap();
    assert!(pairs(&c, &[raw, jpg]).is_empty());
    // both only browsed in Local: a stack would keep the records from being forgotten
    let raw_g = add(&mut c, "/s/G.NEF", true);
    let jpg_g = add(&mut c, "/s/G.JPG", false);
    c.apply(Op::SetLocal { id: raw_g, local: true }).unwrap();
    c.apply(Op::SetLocal { id: jpg_g, local: true }).unwrap();
    assert!(pairs(&c, &[raw_g, jpg_g]).is_empty());
}

#[test]
fn a_stacked_second_member_still_makes_the_name_ambiguous() {
    let mut c = Catalog::new();
    // A.JPG is in a burst stack; A.NEF + a.jpeg would look like a pair if A.JPG were not counted
    let raw = add(&mut c, "/s/A.NEF", true);
    let stacked_jpg = add(&mut c, "/s/A.JPG", false);
    let jpeg = add(&mut c, "/s/a.jpeg", false);
    let other = add(&mut c, "/s/Z.JPG", false);
    let op = c.group_ops(stacked_jpg, &[other], true).unwrap();
    c.apply(op).unwrap();
    assert!(pairs(&c, &[raw, jpeg]).is_empty());
    // B.NEF + B.dng + B.JPG: two raws
    let b = add(&mut c, "/s/B.NEF", true);
    let b_dng = add(&mut c, "/s/B.dng", true);
    let b_jpg = add(&mut c, "/s/B.JPG", false);
    assert!(pairs(&c, &[b, b_dng, b_jpg]).is_empty());
}

#[test]
fn the_stem_is_everything_before_the_last_dot() {
    let mut c = Catalog::new();
    let raw = add(&mut c, "/s/IMG_1.NEF", true);
    let edit = add(&mut c, "/s/IMG_1.edit.jpg", false);
    assert!(pairs(&c, &[raw, edit]).is_empty());
    let raw2 = add(&mut c, "/s/IMG.2.NEF", true);
    let jpg2 = add(&mut c, "/s/img.2.JPG", false);
    assert_eq!(pairs(&c, &[raw2]), vec![(raw2, jpg2)]);
}

#[test]
fn many_stems_at_once_use_the_set_lookup() {
    let mut c = Catalog::new();
    let mut raws = Vec::new();
    for i in 0..80 {
        raws.push(add(&mut c, &format!("/s/DSC_{i:04}.NEF"), true));
        add(&mut c, &format!("/s/dsc_{i:04}.jpg"), false);
    }
    assert_eq!(c.raw_jpeg_pairs(&raws).len(), 80);
}

#[test]
fn retop_follows_a_raw_that_starts_or_stops_decoding() {
    let mut c = Catalog::new();
    let raw = add(&mut c, "/s/DSC_0001.NEF", true);
    let jpg = add(&mut c, "/s/DSC_0001.JPG", false);
    let found = c.raw_jpeg_pairs(&[raw]);
    let ops = c.raw_jpeg_stack_ops(&found);
    c.apply(Op::Batch { ops }).unwrap();
    assert!(c.raw_jpeg_retop_ops(&[raw]).is_empty(), "already right");
    // the raw turns out to be preview only: the JPEG goes on top
    c.apply(Op::SetContent {
        id: raw,
        width: 6000,
        height: 4000,
        file_size: 0,
        content_hash: None,
        preview_only: Some("Nikon High Efficiency NEF".into()),
        preview_checked: String::new(),
    })
    .unwrap();
    let ops = c.raw_jpeg_retop_ops(&[raw]);
    c.apply(Op::Batch { ops }).unwrap();
    assert_eq!(c.stack_of(raw).unwrap().photos, vec![jpg, raw]);
    // other stacks are never re-ordered
    let a = add(&mut c, "/s/X.NEF", true);
    let b = add(&mut c, "/s/Y.JPG", false);
    let op = c.group_ops(b, &[a], true).unwrap();
    c.apply(op).unwrap();
    assert!(c.raw_jpeg_retop_ops(&[a]).is_empty());
}
