//! Smart albums that would include themselves: through the albums they test, the smart albums
//! those test, and the folders any of them are in.
//!
//! The rule is the catalog's: a new edit that would put a smart album on such a loop is refused
//! (`Catalog::apply_new`), whoever makes it. Libraries saved before the rule, or damaged, may
//! still hold loops and must open (`Catalog::apply`, `replay` and snapshots take them); there a
//! loop has one meaning: every smart album on it holds nothing.
//!
//! Scenarios:
//! - Given smart albums on a loop, then each holds nothing, whichever album is asked first, one
//!   photo at a time or in a pass; albums that only test them see them as empty.
//! - Given any library, loops or not, then what an album holds doesn't depend on the order the
//!   albums are asked in.
//! - Given many smart albums each limited to the folder they are in, then showing the folder
//!   costs in proportion to their number.
//! - When a new edit (adding a smart album, moving an album or folder, changing rules, alone or
//!   in a batch) would put a smart album on a loop, then it is refused, names the album, and
//!   nothing changed.
//! - Given a long chain of folders whose smart albums are each limited to the next folder (no
//!   loop), then counting every album, as the sidebar does after each edit, takes what it took
//!   before the rule: which albums are on a loop is worked out once per change to the albums.
//! - Given a batch of edits that fails part-way in a library whose saved rules the catalog
//!   would no longer take, then the rules are as they were: none half-changed, no loop left.
//! - Given a chain of folders four times as long, then asking which albums are on a loop takes
//!   about four times as long, not sixteen.
//! - Given a library that already holds a loop, then it opens, the albums on the loop can still
//!   be edited and the loop undone, and edits elsewhere go through.

use crate::*;

fn photo(c: &mut Catalog, name: &str) -> PhotoId {
    let id = c.alloc_photo_id();
    let p = Photo::new(id, Source::Demo { scene: 1 }, name, "JPEG", 6000, 4000, "2026-09-30T10:00:00");
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

fn folder(c: &mut Catalog, name: &str, parent: Option<AlbumId>) -> AlbumId {
    let id = c.alloc_album_id();
    c.apply(Op::AddAlbum { album: Album { parent, folder: true, ..Album::new(id, name) } }).unwrap();
    id
}

fn album(c: &mut Catalog, name: &str, parent: Option<AlbumId>, photos: &[PhotoId]) -> AlbumId {
    let id = c.alloc_album_id();
    c.apply(Op::AddAlbum { album: Album { parent, photos: photos.to_vec(), ..Album::new(id, name) } }).unwrap();
    id
}

/// A smart album as a library saved before the rule could hold it (no check).
fn smart(c: &mut Catalog, name: &str, parent: Option<AlbumId>, rules: Filter) -> AlbumId {
    let id = c.alloc_album_id();
    c.apply(Op::AddAlbum { album: Album { parent, smart: Some(Box::new(rules)), ..Album::new(id, name) } }).unwrap();
    id
}

fn limited_to(album: AlbumId) -> Filter {
    Filter { album: Some(album), ..Default::default() }
}

/// Rules of Album rules: `(op, album)` joined by `all`.
fn tests(rules: &[(&str, AlbumId)]) -> Filter {
    let rules: Vec<serde_json::Value> = rules.iter().map(|(op, a)| serde_json::json!({"field": "album", "op": op, "value": a.0})).collect();
    serde_json::from_value(serde_json::json!({"ruleSet": {"match": "all", "rules": rules}})).unwrap()
}

fn holds(c: &Catalog, a: AlbumId, p: PhotoId) -> bool {
    c.album_contains(a, c.photo(p).unwrap())
}

/// Two folders whose smart albums are limited to each other's folder, an album with a photo, and
/// albums outside that test them.
#[test]
fn albums_on_a_loop_hold_nothing_whoever_asks_first() {
    let mut c = Catalog::new();
    let p = photo(&mut c, "p.jpg");
    let (g, ff) = (folder(&mut c, "G", None), folder(&mut c, "FF", None));
    let s = smart(&mut c, "S", Some(ff), limited_to(g));
    let t = smart(&mut c, "T", Some(g), limited_to(ff));
    album(&mut c, "R", Some(g), &[p]);
    let both = smart(&mut c, "S and T", None, tests(&[("is", s), ("is", t)]));
    let not_s = smart(&mut c, "Not S", None, tests(&[("isNot", s)]));
    let of_g = smart(&mut c, "Of G", None, limited_to(g));
    assert_eq!(c.albums_on_a_loop(), [s, t].into_iter().collect());
    let ask = |order: &[AlbumId]| -> Vec<(AlbumId, bool)> {
        let mut answers: Vec<(AlbumId, bool)> = order.iter().map(|a| (*a, holds(&c, *a, p))).collect();
        answers.sort();
        answers
    };
    let all = [s, t, both, not_s, of_g, g, ff];
    let forwards = ask(&all);
    let mut reversed = all;
    reversed.reverse();
    assert_eq!(forwards, ask(&reversed));
    let answer = |a: AlbumId| forwards.iter().find(|(x, _)| *x == a).unwrap().1;
    assert!(!answer(s) && !answer(t), "on the loop: nothing");
    assert!(!answer(both), "tests two albums that hold nothing");
    assert!(answer(not_s), "S holds nothing, so p isn't in it");
    assert!(answer(of_g) && answer(g), "the folder still shows the album in it");
    assert!(!answer(ff), "only S is in FF");
    // the same in a pass over the library
    for a in all {
        assert_eq!(c.album_count(a), usize::from(answer(a)), "album {a:?}");
        assert_eq!(c.album_photos(a).len(), usize::from(answer(a)), "album {a:?}");
    }
    // and each is reported
    for a in [s, t] {
        assert!(c.smart_album_problems(a).iter().any(|p| p.issue == crate::rules::Issue::AlbumLoop));
    }
}

/// A small generator: the same sequence every run.
struct Rng(u64);
impl Rng {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

/// What a smart album of the random libraries asks: limited to an album (its `album` field), or
/// Album rules joined by all / any.
#[derive(Clone)]
enum Asks {
    Nothing,
    Limited(AlbumId),
    Rules { any: bool, rules: Vec<(bool, AlbumId)> },
}

impl Asks {
    fn filter(&self) -> Filter {
        match self {
            Asks::Nothing => Filter::default(),
            Asks::Limited(a) => limited_to(*a),
            Asks::Rules { any, rules } => {
                let rules: Vec<serde_json::Value> =
                    rules.iter().map(|(is, a)| serde_json::json!({"field": "album", "op": if *is { "is" } else { "isNot" }, "value": a.0})).collect();
                serde_json::from_value(serde_json::json!({"ruleSet": {"match": if *any { "any" } else { "all" }, "rules": rules}})).unwrap()
            }
        }
    }
    fn tested(&self) -> Vec<AlbumId> {
        match self {
            Asks::Nothing => Vec::new(),
            Asks::Limited(a) => vec![*a],
            Asks::Rules { rules, .. } => rules.iter().map(|(_, a)| *a).collect(),
        }
    }
}

/// The random library written down a second time, plainly, to answer by the definition: a
/// folder holds what the albums in it hold; an album holds its list; a smart album that leads
/// back to itself holds nothing, and any other holds what its rules say.
struct Plain {
    parent: std::collections::HashMap<AlbumId, Option<AlbumId>>,
    folders: std::collections::HashSet<AlbumId>,
    lists: std::collections::HashMap<AlbumId, Vec<PhotoId>>,
    asks: std::collections::HashMap<AlbumId, Asks>,
}

impl Plain {
    fn inside(&self, folder: AlbumId) -> Vec<AlbumId> {
        let mut out = Vec::new();
        for (a, parent) in &self.parent {
            if *parent == Some(folder) {
                if self.folders.contains(a) { out.extend(self.inside(*a)) } else { out.push(*a) }
            }
        }
        out
    }
    fn leads_to(&self, a: AlbumId) -> Vec<AlbumId> {
        if self.folders.contains(&a) { self.inside(a) } else { self.asks.get(&a).map(Asks::tested).unwrap_or_default() }
    }
    fn on_a_loop(&self, a: AlbumId) -> bool {
        let (mut seen, mut next) = (std::collections::HashSet::new(), self.leads_to(a));
        while let Some(x) = next.pop() {
            if x == a {
                return true;
            }
            if seen.insert(x) {
                next.extend(self.leads_to(x));
            }
        }
        false
    }
    fn holds(&self, a: AlbumId, p: PhotoId) -> bool {
        if self.folders.contains(&a) {
            return self.inside(a).into_iter().any(|m| self.holds(m, p));
        }
        if let Some(list) = self.lists.get(&a) {
            return list.contains(&p);
        }
        match self.asks.get(&a) {
            None => false,
            Some(_) if self.on_a_loop(a) => false,
            Some(Asks::Nothing) => true,
            Some(Asks::Limited(x)) => self.holds(*x, p),
            Some(Asks::Rules { any, rules }) => {
                let mut answers = rules.iter().map(|(is, x)| self.holds(*x, p) == *is);
                if *any { answers.any(|yes| yes) } else { answers.all(|yes| yes) }
            }
        }
    }
}

/// Random libraries of folders, albums and smart albums that test each other at random (most end
/// up with loops, many through folders): every way of asking what an album holds gives the answer
/// of the definition ([`Plain`]), whatever was asked before, one photo at a time or in a pass. Before, a
/// loop was cut wherever the question entered it, so an album seen from inside another's rules
/// could differ from the same album asked directly.
#[test]
fn what_an_album_holds_is_what_the_definition_says() {
    let mut rng = Rng(7);
    let (mut with_loops, mut through_folders) = (0, 0);
    for _ in 0..400 {
        let mut c = Catalog::new();
        let photos: Vec<PhotoId> = (0..4).map(|i| photo(&mut c, &format!("p{i}.jpg"))).collect();
        let mut plain = Plain { parent: Default::default(), folders: Default::default(), lists: Default::default(), asks: Default::default() };
        let n = 3 + rng.next(10);
        let mut ids: Vec<AlbumId> = Vec::new();
        let mut folders: Vec<AlbumId> = Vec::new();
        for i in 0..n {
            let parent = (!folders.is_empty() && rng.next(3) != 0).then(|| folders[rng.next(folders.len() as u64) as usize]);
            let id = match rng.next(4) {
                0 => {
                    let f = folder(&mut c, &format!("F{i}"), parent);
                    folders.push(f);
                    plain.folders.insert(f);
                    f
                }
                1 => {
                    let held: Vec<PhotoId> = photos.iter().copied().filter(|_| rng.next(2) == 0).collect();
                    let a = album(&mut c, &format!("A{i}"), parent, &held);
                    plain.lists.insert(a, held);
                    a
                }
                // rules are set once every album exists, so they can name later ones
                _ => {
                    let s = smart(&mut c, &format!("S{i}"), parent, Filter::default());
                    plain.asks.insert(s, Asks::Nothing);
                    s
                }
            };
            plain.parent.insert(id, parent);
            ids.push(id);
        }
        let smart_ids: Vec<AlbumId> = plain.asks.keys().copied().collect();
        for id in smart_ids {
            let pick = |rng: &mut Rng| ids[rng.next(ids.len() as u64) as usize];
            let asks = match rng.next(4) {
                0 => Asks::Limited(pick(&mut rng)),
                1 => Asks::Rules { any: false, rules: vec![(rng.next(2) == 0, pick(&mut rng))] },
                2 => Asks::Rules { any: false, rules: vec![(true, pick(&mut rng)), (false, pick(&mut rng))] },
                _ => Asks::Rules { any: true, rules: vec![(rng.next(2) == 0, pick(&mut rng)), (true, pick(&mut rng))] },
            };
            // unchecked, as an old library could hold them (a filter naming a smart album
            // directly is the one thing `apply` itself refuses: those keep asking nothing)
            if c.apply(Op::SetAlbumRules { id, rules: Box::new(asks.filter()) }).is_ok() {
                plain.asks.insert(id, asks);
            }
        }
        let looping: std::collections::BTreeSet<AlbumId> = plain.asks.keys().copied().filter(|a| plain.on_a_loop(*a)).collect();
        assert_eq!(c.albums_on_a_loop(), looping, "{}", c.to_snapshot());
        with_loops += usize::from(!looping.is_empty());
        through_folders += usize::from(looping.iter().any(|a| plain.asks[a].tested().iter().any(|t| plain.folders.contains(t))));
        let mut order = ids.clone();
        for round in 0..3 {
            // forwards, backwards, then in a pass
            if round == 1 {
                order.reverse();
            }
            let ask = || {
                for a in &order {
                    for p in &photos {
                        assert_eq!(holds(&c, *a, *p), plain.holds(*a, *p), "album {a:?}, photo {p:?}, round {round}: {}", c.to_snapshot());
                    }
                }
            };
            if round == 2 { crate::gathering(ask) } else { ask() }
        }
        for a in &ids {
            let want: Vec<PhotoId> = photos.iter().copied().filter(|p| plain.holds(*a, *p)).collect();
            assert_eq!(c.album_photos(*a), want, "{}", c.to_snapshot());
            assert_eq!(c.album_count(*a), want.len(), "{}", c.to_snapshot());
            // the ⚠ in the sidebar is on exactly the albums on a loop
            let marked = c.smart_album_problems(*a).iter().any(|p| p.issue == crate::rules::Issue::AlbumLoop);
            assert_eq!(marked, looping.contains(a), "album {a:?}: {}", c.to_snapshot());
        }
        for p in &photos {
            let want: Vec<AlbumId> = {
                let mut v: Vec<AlbumId> = ids.iter().copied().filter(|a| !plain.folders.contains(a) && plain.holds(*a, *p)).collect();
                v.sort();
                v
            };
            assert_eq!(c.albums_of(*p), want, "{}", c.to_snapshot());
        }
    }
    assert!(with_loops > 100 && through_folders > 30, "the generator makes loops: {with_loops} of 400, {through_folders} through folders");
}

/// `k` smart albums each limited to the folder they are all in, 200 photos: showing the folder
/// asked each of them about all the others (cubic in `k`). Four times the albums now costs a few
/// times more, not sixty.
#[test]
fn a_folder_of_looping_albums_costs_in_proportion() {
    let build = |k: usize| {
        let mut c = Catalog::new();
        let photos: Vec<PhotoId> = (0..200).map(|i| photo(&mut c, &format!("p{i}.jpg"))).collect();
        let f = folder(&mut c, "F", None);
        album(&mut c, "Plain", Some(f), &photos[..100]);
        for i in 0..k {
            smart(&mut c, &format!("S{i}"), Some(f), limited_to(f));
        }
        (c, f)
    };
    let timed = |c: &Catalog, f: AlbumId| {
        (0..5)
            .map(|_| {
                let start = std::time::Instant::now();
                assert_eq!(c.album_count(f), 100);
                start.elapsed()
            })
            .min()
            .unwrap()
    };
    let (small, f_small) = build(10);
    let (big, f_big) = build(40);
    let (t10, t40) = (timed(&small, f_small), timed(&big, f_big));
    assert!(t40 < t10 * 20 + std::time::Duration::from_millis(2), "10 albums: {t10:?}; 40 albums: {t40:?}");
}

/// The whole library as saved (ids still to hand out included).
fn album_names(c: &Catalog) -> String {
    c.to_snapshot()
}

#[test]
fn a_new_edit_that_would_make_a_loop_is_refused() {
    let mut c = Catalog::new();
    let trips = folder(&mut c, "Trips", None);
    let europe = folder(&mut c, "Europe", Some(trips));
    let view = smart(&mut c, "Trips view", None, limited_to(trips));
    let plain_rules = smart(&mut c, "Rated", Some(trips), Filter { rating: 3, ..Default::default() });
    let before = album_names(&c);
    let next = AlbumId(99);
    let new_smart = |parent, rules| Op::AddAlbum { album: Album { parent, smart: Some(Box::new(rules)), ..Album::new(next, "New one") } };
    let refused: Vec<(&str, Op, &str)> = vec![
        ("made in the folder it shows", new_smart(Some(trips), limited_to(trips)), "New one"),
        ("made in a folder inside it", new_smart(Some(europe), limited_to(trips)), "New one"),
        ("moved into it", Op::MoveAlbum { id: view, parent: Some(europe) }, "Trips view"),
        ("rules changed to show its own folder", Op::SetAlbumRules { id: plain_rules, rules: Box::new(limited_to(trips)) }, "Rated"),
        ("rules changed to test an album that tests it", Op::SetAlbumRules { id: view, rules: Box::new(tests(&[("is", view)])) }, "Trips view"),
        (
            "in a batch",
            Op::Batch { ops: vec![Op::RenameAlbum { id: view, name: "Renamed".into() }, Op::MoveAlbum { id: view, parent: Some(trips) }] },
            "Trips view",
        ),
    ];
    for (what, op, name) in refused {
        let e = c.apply_new(op).unwrap_err().to_string();
        assert!(e.contains(name) && e.contains("include itself"), "{what}: {e}");
        assert_eq!(album_names(&c), before, "{what}: nothing changed");
        assert_eq!(c.album(view).unwrap().name, "Trips view", "{what}");
        assert!(c.albums_on_a_loop().is_empty(), "{what}");
    }
    // what makes no loop goes through, and is undone by what it returns
    let allowed = vec![
        new_smart(None, limited_to(trips)),
        new_smart(Some(trips), limited_to(europe)),
        Op::MoveAlbum { id: plain_rules, parent: Some(europe) },
        Op::SetAlbumRules { id: view, rules: Box::new(tests(&[("isNot", plain_rules)])) },
    ];
    for op in allowed {
        let undo = c.apply_new(op.clone()).unwrap_or_else(|e| panic!("{op:?}: {e}"));
        assert!(c.albums_on_a_loop().is_empty());
        c.apply(undo).unwrap();
        // the albums are as they were (an id that was used is not handed out again)
        let albums = |snapshot: &str| serde_json::from_str::<serde_json::Value>(snapshot).unwrap()["albums"].clone();
        assert_eq!(albums(&album_names(&c)), albums(&before));
    }
}

/// A refused edit names the album the edit was about, when that one is among those it would put
/// on a loop (two albums testing each other both end up on it).
#[test]
fn a_refusal_names_the_album_that_was_edited() {
    let mut c = Catalog::new();
    let first = smart(&mut c, "First", None, Filter::default());
    let second = smart(&mut c, "Second", None, tests(&[("is", first)]));
    let e = c.apply_new(Op::SetAlbumRules { id: first, rules: Box::new(tests(&[("is", second)])) }).unwrap_err().to_string();
    assert!(e.contains("First"), "{e}");
    let third = smart(&mut c, "Third", None, Filter::default());
    let e = c.apply_new(Op::SetAlbumRules { id: third, rules: Box::new(tests(&[("is", third)])) }).unwrap_err().to_string();
    assert!(e.contains("Third"), "{e}");
}

/// The check looks at what the edit would do before doing it, so there is nothing to take back:
/// an album whose saved rules `apply` itself would no longer take (limited to what has since
/// become a smart album's id) can't be left half-edited on a loop.
#[test]
fn a_refused_edit_never_touched_the_library() {
    let mut c = Catalog::new();
    // T is limited to album 2 before there is one; then a smart album takes that id
    let t = smart(&mut c, "T", None, limited_to(AlbumId(2)));
    let u = smart(&mut c, "U", None, Filter { rating: 3, ..Default::default() });
    assert_eq!((t, u), (AlbumId(1), AlbumId(2)));
    assert!(c.apply(Op::SetAlbumRules { id: t, rules: Box::new(limited_to(u)) }).is_err(), "T's own rules can't be set again");
    let (before, revision) = (c.to_snapshot(), c.revision);
    let e = c.apply_new(Op::SetAlbumRules { id: t, rules: Box::new(tests(&[("is", t)])) }).unwrap_err().to_string();
    assert!(e.contains("include itself") && e.contains("“T”"), "{e}");
    assert_eq!(c.to_snapshot(), before);
    assert_eq!(c.revision, revision, "not even looked at as changed");
    assert!(c.albums_on_a_loop().is_empty());
    // an album refused with an id of the caller's choosing leaves the ids to hand out alone
    let chosen =
        Op::AddAlbum { album: Album { parent: None, smart: Some(Box::new(tests(&[("is", AlbumId(500))]))), ..Album::new(AlbumId(500), "Chosen") } };
    assert!(c.apply_new(chosen).is_err());
    assert_eq!(c.to_snapshot(), before);
    assert_eq!(c.alloc_album_id(), AlbumId(3));
}

/// A damaged library: a smart album whose folder is gone, limited to that folder's id. A folder
/// made with that id would take it in and close the loop.
#[test]
fn a_folder_that_would_adopt_a_looping_album_is_refused() {
    let mut c = Catalog::new();
    let s = smart(&mut c, "Orphan", None, limited_to(AlbumId(2)));
    c.damage(s, |a| a.parent = Some(AlbumId(2)));
    let e = c.apply_new(Op::AddAlbum { album: Album { folder: true, ..Album::new(AlbumId(2), "Back") } }).unwrap_err().to_string();
    assert!(e.contains("Orphan"), "{e}");
    assert!(c.album(AlbumId(2)).is_none());
}

/// A damaged library: a folder that also carries rules is a folder, so it is on no loop and
/// carries no ⚠ for one.
#[test]
fn a_folder_with_rules_is_marked_as_it_is_treated() {
    let mut c = Catalog::new();
    let f = folder(&mut c, "F", None);
    c.damage(f, |a| a.smart = Some(Box::new(limited_to(f))));
    assert!(c.albums_on_a_loop().is_empty());
    assert!(c.smart_album_problems(f).is_empty());
}

/// 300 folders, the smart album in each limited to the next folder (no loop): every edit of the
/// albums asks which of them are on a loop, and that looked through every album once per folder
/// per smart album (over a second). A drag has to stay a drag.
#[test]
fn asking_for_loops_in_a_long_chain_is_quick() {
    let mut c = Catalog::new();
    let folders: Vec<AlbumId> = (0..300).map(|i| folder(&mut c, &format!("F{i}"), None)).collect();
    for i in 0..1400 {
        album(&mut c, &format!("A{i}"), Some(folders[i % 300]), &[]);
    }
    let mut smarts = Vec::new();
    for i in 0..299 {
        smarts.push(smart(&mut c, &format!("S{i}"), Some(folders[i]), limited_to(folders[i + 1])));
    }
    let best = |f: &mut dyn FnMut()| {
        (0..3)
            .map(|_| {
                let start = std::time::Instant::now();
                f();
                start.elapsed()
            })
            .min()
            .unwrap()
    };
    let asked = best(&mut || assert!(c.albums_on_a_loop().is_empty()));
    assert!(asked < std::time::Duration::from_millis(250), "albums_on_a_loop took {asked:?}");
    let loose = album(&mut c, "Loose", None, &[]);
    let mut c2 = c.clone();
    let mut to = 0;
    let moved = best(&mut || {
        to += 1;
        c2.apply_new(Op::MoveAlbum { id: loose, parent: Some(folders[to]) }).unwrap();
    });
    assert!(moved < std::time::Duration::from_millis(250), "a move took {moved:?}");
    // closing the chain is still seen
    let e = c2.apply_new(Op::SetAlbumRules { id: smarts[0], rules: Box::new(limited_to(folders[0])) });
    assert!(e.is_err());
}

/// Every edit of the albums asks which are on a loop. One search per smart album made that grow
/// with the square of a chain's length (four times the chain: sixteen times the wait, a third of
/// a second per drag at 1,200 folders); one walk over the albums grows with the chain.
#[test]
fn asking_for_loops_grows_with_the_chain_not_its_square() {
    let chain = |n: usize| {
        let mut c = Catalog::new();
        let folders: Vec<AlbumId> = (0..n).map(|i| folder(&mut c, &format!("F{i}"), None)).collect();
        for i in 0..n - 1 {
            smart(&mut c, &format!("S{i}"), Some(folders[i]), limited_to(folders[i + 1]));
        }
        c
    };
    let asked = |c: &Catalog| {
        (0..5)
            .map(|_| {
                // a copy that has worked nothing out yet
                let fresh = Catalog::from_snapshot(&c.to_snapshot()).unwrap();
                let start = std::time::Instant::now();
                assert!(fresh.albums_on_a_loop().is_empty());
                start.elapsed()
            })
            .min()
            .unwrap()
    };
    let (short, long) = (asked(&chain(300)), asked(&chain(1200)));
    assert!(long < short * 8 + std::time::Duration::from_millis(2), "300 folders: {short:?}; 1,200 folders: {long:?}");
    // the chain closed is one loop of all of them
    let mut c = chain(300);
    let last = c.albums().filter(|a| a.folder).map(|a| a.id).max().unwrap();
    let first = c.albums().filter(|a| a.folder).map(|a| a.id).min().unwrap();
    smart(&mut c, "Closing", Some(last), limited_to(first));
    assert_eq!(c.albums_on_a_loop().len(), 300);
}

/// The sidebar counts every album after every edit. With 300 folders whose smart albums are
/// each limited to the next folder, asking each smart album along the way whether it is on a loop
/// made one recount take a quarter of a minute; it takes a fraction of a second.
#[test]
fn counting_every_album_of_a_long_chain_is_quick() {
    let mut c = Catalog::new();
    // few photos: counting them is not what is measured, and the clock gets a wide margin
    let photos: Vec<PhotoId> = (0..40).map(|i| photo(&mut c, &format!("p{i}.jpg"))).collect();
    let folders: Vec<AlbumId> = (0..300).map(|i| folder(&mut c, &format!("F{i}"), None)).collect();
    for i in 0..1400 {
        album(&mut c, &format!("A{i}"), Some(folders[i % 300]), &photos[i % 40..(i % 40 + 1)]);
    }
    let smarts: Vec<AlbumId> = (0..299).map(|i| smart(&mut c, &format!("S{i}"), Some(folders[i]), limited_to(folders[i + 1]))).collect();
    let start = std::time::Instant::now();
    let total: usize = smarts.iter().map(|s| c.album_count(*s)).sum();
    let counted = start.elapsed();
    assert!(total > 0);
    assert!(counted < std::time::Duration::from_secs(3), "one recount took {counted:?}");
    let start = std::time::Instant::now();
    for p in &photos[..5] {
        assert!(!c.albums_of(*p).is_empty());
    }
    let asked = start.elapsed();
    assert!(asked < std::time::Duration::from_secs(3), "the albums of five photos took {asked:?}");
    // an edit to the albums is seen at once: the chain closed (as an old library could hold it)
    assert!(c.albums_on_a_loop().is_empty());
    c.apply(Op::SetAlbumRules { id: smarts[298], rules: Box::new(limited_to(folders[0])) }).unwrap();
    assert_eq!(c.albums_on_a_loop().len(), 299);
    assert_eq!(c.album_count(smarts[0]), 0);
    // and so is one taken back
    c.apply(Op::SetAlbumRules { id: smarts[298], rules: Box::new(Filter::default()) }).unwrap();
    assert!(c.albums_on_a_loop().is_empty());
    assert!(c.album_count(smarts[0]) > 0);
}

/// A batch that fails part-way puts back the rules it changed, also where those are rules `apply`
/// would no longer take as a new edit (T limited to what has since become a smart album): putting
/// rules back is not checked like setting them. Before, T kept its new rules and a loop. (Other
/// things a batch may have to put back in a damaged library, a rating out of range or an album
/// whose folder is gone, are still checked, and still stay half applied: not this test's.)
#[test]
fn a_failed_batch_puts_back_rules_the_catalog_would_no_longer_take() {
    let mut c = Catalog::new();
    let t = smart(&mut c, "T", None, limited_to(AlbumId(2)));
    let u = smart(&mut c, "U", None, Filter { rating: 3, ..Default::default() });
    let v = smart(&mut c, "V", None, tests(&[("is", t)]));
    assert_eq!((t, u, v), (AlbumId(1), AlbumId(2), AlbumId(3)));
    let (before, revision) = (c.to_snapshot(), c.revision);
    let batch = || Op::Batch {
        ops: vec![Op::SetAlbumRules { id: t, rules: Box::new(tests(&[("is", v)])) }, Op::RemoveAlbum { id: AlbumId(999) }, Op::RemoveAlbum { id: v }],
    };
    for result in [c.apply_new(batch()), c.apply(batch())] {
        assert!(result.unwrap_err().to_string().contains("999"));
        assert_eq!(c.to_snapshot(), before);
        assert_eq!(c.revision, revision);
        assert!(c.albums_on_a_loop().is_empty());
    }
}

#[test]
fn a_library_that_already_holds_a_loop_opens_and_can_be_mended() {
    // as written by a version without the rule: the op log makes a loop
    let mut old = Catalog::new();
    let mut log = String::new();
    let mut record = |c: &mut Catalog, op: Op| {
        log.push_str(&Catalog::op_to_log_line(&op));
        c.apply(op).unwrap();
    };
    let trips = AlbumId(1);
    let (inside, other) = (AlbumId(2), AlbumId(3));
    record(&mut old, Op::AddAlbum { album: Album { folder: true, ..Album::new(trips, "Trips") } });
    record(&mut old, Op::AddAlbum { album: Album { parent: Some(trips), smart: Some(Box::new(limited_to(trips))), ..Album::new(inside, "Inside") } });
    record(&mut old, Op::AddAlbum { album: Album { smart: Some(Box::new(Filter::default())), ..Album::new(other, "Other") } });
    let mut c = Catalog::new();
    assert_eq!(c.replay(&log).unwrap(), 3, "the log replays");
    assert_eq!(c.albums_on_a_loop(), [inside].into_iter().collect());
    let from_snapshot = Catalog::from_snapshot(&c.to_snapshot()).unwrap();
    assert_eq!(from_snapshot.albums_on_a_loop(), [inside].into_iter().collect());
    // edits elsewhere, and edits to the album on the loop that leave it there, go through
    c.apply_new(Op::RenameAlbum { id: inside, name: "Still inside".into() }).unwrap();
    c.apply_new(Op::SetAlbumRules { id: other, rules: Box::new(Filter { rating: 2, ..Default::default() }) }).unwrap();
    c.apply_new(Op::SetAlbumRules { id: inside, rules: Box::new(Filter { rating: 4, album: Some(trips), ..Default::default() }) }).unwrap();
    // but it doesn't get to take another album onto a loop
    let e = c.apply_new(Op::MoveAlbum { id: other, parent: Some(trips) }).map(|_| ());
    assert!(e.is_ok(), "Other tests no album: moving it in makes no loop");
    let e = c.apply_new(Op::SetAlbumRules { id: other, rules: Box::new(limited_to(trips)) }).unwrap_err().to_string();
    assert!(e.contains("Other"), "{e}");
    // mended by moving it out
    c.apply_new(Op::MoveAlbum { id: inside, parent: None }).unwrap();
    assert!(c.albums_on_a_loop().is_empty());
}
