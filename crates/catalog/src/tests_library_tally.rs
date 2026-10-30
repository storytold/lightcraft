//! A [`LibraryTally`] followed through edits says what one counted from every photo says.

use crate::keywords::KeywordInfo;
use crate::library_tally::{LibraryCounts, LibraryTally, MAX_FOLLOWED};
use crate::model::*;
use crate::{Catalog, Op};

/// A small deterministic generator (xorshift), so a failure names its step.
struct Dice(u64);

impl Dice {
    fn roll(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n.max(1)
    }
    fn pick<'a>(&mut self, of: &[&'a str]) -> &'a str {
        of[self.roll(of.len() as u64) as usize]
    }
}

/// Folders as people's libraries hold them: a disk, a share, a Windows drive, a folder spelled
/// in two cases, the root itself.
const FOLDERS: &[&str] =
    &["/Volumes/nas/Entities/Z", "/Volumes/nas/entities/z", "/Volumes/nas/LG/007", "/Users/me/Pictures", "C:/Photos/2024", "", "/mnt/b"];
/// Keywords, with case variants of the same keyword, levels shared by two, and spaces and
/// empty levels that are trimmed away.
const KEYWORDS: &[&str] =
    &["Body", "body", "Travel|Italy", "travel|France", "travel|italy|Rome", " Travel | Spain ", "a||b", "People|Ann", "people|ann"];
const DATES: &[&str] = &["2024-03-05T10:00:00", "2024-03-06T10:00:00", "2023-12-31T23:59:59", "2025-01-01", "2024", "ü2024-03-05", ""];

fn photo(c: &mut Catalog, d: &mut Dice) -> Photo {
    let id = c.alloc_photo_id();
    let dir = d.pick(FOLDERS);
    let mut p = Photo::new(id, Source::File { path: format!("{dir}/IMG_{}.CR3", id.0) }, &format!("IMG_{}.CR3", id.0), "CR3", 6, 4, "2026-10-16");
    p.captured = Some(d.pick(DATES).to_string()).filter(|s| !s.is_empty());
    p.meta.keywords = (0..d.roll(4)).map(|_| d.pick(KEYWORDS).to_string()).collect();
    p.flag = if d.roll(3) == 0 { Flag::Pick } else { Flag::None };
    p.local = d.roll(10) == 0;
    if d.roll(12) == 0 {
        p.source = Source::Demo { scene: 1 };
    }
    p
}

fn any_photo(c: &Catalog, d: &mut Dice) -> Option<PhotoId> {
    let ids: Vec<PhotoId> = c.photos().map(|p| p.id).collect();
    (!ids.is_empty()).then(|| ids[d.roll(ids.len() as u64) as usize])
}

/// One edit of the kinds a library sees, chosen by the dice.
fn edit(c: &mut Catalog, d: &mut Dice) -> Op {
    let Some(id) = any_photo(c, d) else { return Op::AddPhoto { photo: Box::new(photo(c, d)) } };
    let p = c.photo(id).cloned().unwrap();
    match d.roll(13) {
        0 => Op::Batch { ops: (0..1 + d.roll(16)).map(|_| Op::AddPhoto { photo: Box::new(photo(c, d)) }).collect() },
        1 => Op::RemovePhoto { id },
        2 => Op::SetDeleted { id, deleted: !p.deleted },
        3 => Op::SetLocal { id, local: !p.local },
        4 => Op::Relink {
            id,
            file_name: "moved.jpg".into(),
            source: Source::File { path: format!("{}/moved_{}.jpg", d.pick(FOLDERS), id.0) },
            format: None,
        },
        5 => Op::SetCaptured { id, captured: (d.roll(3) > 0).then(|| format!("202{}-0{}-1{}T10:00:00", d.roll(6), 1 + d.roll(9), d.roll(10))) },
        6 => Op::SetMeta { id, meta: Box::new(Meta { keywords: (0..d.roll(4)).map(|_| d.pick(KEYWORDS).to_string()).collect(), ..p.meta.clone() }) },
        7 => Op::SetFlag { id, flag: if p.flag == Flag::Pick { Flag::None } else { Flag::Pick } },
        8 => {
            let aid = c.alloc_album_id();
            Op::AddAlbum { album: Album { photos: vec![id], ..Album::new(aid, "Trip") } }
        }
        9 => Op::SetKeyword {
            path: d.pick(&["TRAVEL|Italy", "people|Bob", "Unused|Listed"]).to_string(),
            info: (d.roll(3) > 0).then(KeywordInfo::default),
        },
        10 => c.folder_label_op(d.pick(FOLDERS), (d.roll(2) == 0).then_some(ColorLabel::Red)),
        11 => Op::SetRating { id, rating: d.roll(6) as u8 },
        _ => Op::Batch {
            ops: vec![
                Op::SetFlag { id, flag: Flag::Pick },
                Op::SetMeta { id, meta: Box::new(Meta { keywords: vec![d.pick(KEYWORDS).into()], ..p.meta.clone() }) },
            ],
        },
    }
}

/// What the sidebar shows, from the tally and from every photo.
fn assert_same(t: &LibraryTally, c: &Catalog, step: usize) {
    assert_eq!(t.revision(), c.revision, "step {step}: the tally is of the catalog as it is");
    assert_eq!(t.folder_tree(c), c.folder_tree(), "step {step}: folders");
    assert_eq!(t.date_groups(), c.date_groups(), "step {step}: dates");
    assert_eq!(t.keyword_tree(c), c.keyword_tree(), "step {step}: keywords");
    let mut want = LibraryCounts::default();
    for p in c.photos() {
        if p.in_library() {
            want.total += 1;
            want.picks += usize::from(p.flag == Flag::Pick);
        } else if p.deleted && !p.local {
            want.deleted += 1;
        }
    }
    assert_eq!(t.counts(), want, "step {step}: counts");
}

#[test]
fn a_tally_followed_through_edits_is_the_tally_of_every_photo() {
    for seed in [7u64, 99, 2026, 0x5eed] {
        let mut d = Dice(seed);
        let mut c = Catalog::new();
        for _ in 0..40 {
            let op = Op::AddPhoto { photo: Box::new(photo(&mut c, &mut d)) };
            c.apply(op).unwrap();
        }
        let mut t = LibraryTally::of(&c);
        assert_same(&t, &c, 0);
        let mut followed = 0;
        for step in 1..=400 {
            let op = edit(&mut c, &mut d);
            // (an edit the catalog refuses changes nothing: it is followed all the same)
            let _ = c.apply(op);
            if t.follow(&c).is_some() && t.exact() {
                followed += 1;
            } else {
                t = LibraryTally::of(&c);
            }
            assert_same(&t, &c, step);
        }
        assert!(followed > 300, "seed {seed}: almost every edit is followed ({followed} of 400)");
    }
}

#[test]
fn edits_that_change_no_photo_are_followed_without_counting() {
    let mut d = Dice(3);
    let mut c = Catalog::new();
    for _ in 0..10 {
        let op = Op::AddPhoto { photo: Box::new(photo(&mut c, &mut d)) };
        c.apply(op).unwrap();
    }
    let mut t = LibraryTally::of(&c);
    let aid = c.alloc_album_id();
    c.apply(Op::AddAlbum { album: Album::new(aid, "Trip") }).unwrap();
    c.apply(Op::SetKeyword { path: "Listed|Only".into(), info: Some(KeywordInfo::default()) }).unwrap();
    c.apply(c.folder_label_op("/Users/me/Pictures", Some(ColorLabel::Green))).unwrap();
    assert_eq!(t.follow(&c), Some(Vec::new()), "albums, the keyword list and folder labels touch no photo");
    assert_same(&t, &c, 1);
}

#[test]
fn photos_added_in_a_batch_are_followed_photo_by_photo() {
    let mut d = Dice(11);
    let mut c = Catalog::new();
    for _ in 0..50 {
        let op = Op::AddPhoto { photo: Box::new(photo(&mut c, &mut d)) };
        c.apply(op).unwrap();
    }
    let mut t = LibraryTally::of(&c);
    let batch: Vec<Op> = (0..16).map(|_| Op::AddPhoto { photo: Box::new(photo(&mut c, &mut d)) }).collect();
    let added: Vec<PhotoId> = batch.iter().filter_map(|op| if let Op::AddPhoto { photo } = op { Some(photo.id) } else { None }).collect();
    c.apply(Op::Batch { ops: batch }).unwrap();
    assert_eq!(t.follow(&c), Some(added), "the batch's photos, and only those");
    assert_same(&t, &c, 1);
}

#[test]
fn too_many_photos_changed_or_another_past_are_counted_again() {
    let mut d = Dice(5);
    let mut c = Catalog::new();
    let ops: Vec<Op> = (0..MAX_FOLLOWED + 1).map(|_| Op::AddPhoto { photo: Box::new(photo(&mut c, &mut d)) }).collect();
    let mut t = LibraryTally::of(&c);
    c.apply(Op::Batch { ops }).unwrap();
    assert_eq!(t.follow(&c), None, "more photos than are followed one by one");
    assert_eq!(t.revision(), 0, "a tally that can't follow is left as it was");
    let mut t = LibraryTally::of(&c);
    // a library just loaded takes revisions of its own: nothing is known of how it got there
    c.revision += 1 << 32;
    assert_eq!(t.follow(&c), None);
}

#[test]
fn a_keyword_whose_first_spelling_went_asks_to_be_counted_again() {
    let mut c = Catalog::new();
    let add = |c: &mut Catalog, kw: &str| {
        let id = c.alloc_photo_id();
        let mut p = Photo::new(id, Source::File { path: format!("/p/{}.jpg", id.0) }, "a.jpg", "JPEG", 6, 4, "2026-10-16");
        p.meta.keywords = vec![kw.to_string()];
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        id
    };
    let first = add(&mut c, "body");
    add(&mut c, "Body");
    add(&mut c, "body");
    let mut t = LibraryTally::of(&c);
    assert_eq!(t.keyword_tree(&c)[0].name, "body", "as first written");
    c.apply(Op::SetDeleted { id: first, deleted: true }).unwrap();
    assert!(t.follow(&c).is_some());
    // the photo that wrote it first is gone: whether the other spelling now comes first is not
    // known photo by photo
    assert!(!t.exact(), "the tally says it has to be counted again");
    let t = LibraryTally::of(&c);
    assert!(t.exact());
    assert_eq!(t.keyword_tree(&c)[0].name, "Body");
    assert_same(&t, &c, 1);
}

#[test]
fn a_bulk_edit_of_what_no_count_reads_is_followed_however_many_photos_it_changes() {
    let mut d = Dice(13);
    let mut c = Catalog::new();
    let ops: Vec<Op> = (0..MAX_FOLLOWED + 10).map(|_| Op::AddPhoto { photo: Box::new(photo(&mut c, &mut d)) }).collect();
    c.apply(Op::Batch { ops }).unwrap();
    let mut t = LibraryTally::of(&c);
    let ids: Vec<PhotoId> = c.photos().map(|p| p.id).collect();
    c.apply(Op::Batch { ops: ids.iter().map(|id| Op::SetRating { id: *id, rating: 4 }).collect() }).unwrap();
    assert_eq!(t.follow(&c), Some(Vec::new()), "a rating changes no folder, day, keyword or count");
    assert_same(&t, &c, 1);
    c.apply(Op::Batch { ops: ids.iter().map(|id| Op::SetFlag { id: *id, flag: Flag::Pick }).collect() }).unwrap();
    assert_eq!(t.follow(&c), None, "a flag on every photo changes the picks: counted whole");
}
