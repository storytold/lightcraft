//! Which rows of the sidebar's trees are open and which are folded, kept between sessions with
//! the settings (`ui.json`): Albums' folders, Local, By Date, Folders and Keywords open as they
//! were left.
//!
//! Each tree has a way its rows start (Albums' folders open, a disk in Folders open, the rest
//! folded). A row that was opened or folded by hand is kept as it was left, also when that is
//! how it starts: how a row starts can change (a folder of the library that gets a second folder
//! inside it), and what was said of it should not. Rows that open by themselves to show where
//! the grid is (the folders above the one that is shown) are open for the session, as they
//! always were, and are not kept; the one exception is such a row that was folded, by hand or with
//! the rows above it: the next time one of them is opened to show where the grid is, `show` opens
//! it for good and keeps it as open. The settings hold what somebody decided, not every folder that was ever browsed.
//! The library's thousands of other rows are not kept either. A click with
//! Option held says of a row and of all the rows inside it that they are open, or folded, and
//! is kept as two things said, however many rows there are: of the row, and of what is inside
//! it (`in:<row>`). A row is as the last thing said of it, or of a row above it, has it. A row that is
//! gone (an album deleted, a folder renamed) is never asked about again; so that such rows don't
//! pile up for ever, the rows changed longest ago go when there are more than [`MAX_ROWS`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// The most rows kept: far more than anyone folds by hand, few enough that the settings stay a
/// small file.
pub const MAX_ROWS: usize = 2000;

/// How a row's name in the settings begins: the tree it is a row of.
const TREES: [&str; 5] = ["album:", "date:", "folder:", "keyword:", "local:"];

/// Before a row's name: what is said of all the rows inside it (`in:album:12`).
const INSIDE: &str = "in:";

/// Something said of a row, or of all the rows inside one: open (or folded), and when (a
/// count, larger is later).
pub type Said = (bool, u64);

/// A row of a tree that can be opened: what it is called in the settings, and how it starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    /// Its name in the settings: the tree and what the row is of (`album:12`, `date:2026-05`).
    pub key: String,
    /// Whether it is open until somebody folds it (or folded until somebody opens it).
    pub starts_open: bool,
}

impl Branch {
    /// A folder of albums: open until it is folded.
    pub fn album(id: u64) -> Branch {
        Branch { key: format!("album:{id}"), starts_open: true }
    }

    /// A folder on disk, in the tree that hangs from the Local place `place` (a folder listed in
    /// two places has a row in each, and each opens by itself).
    pub fn local(place: &str, path: &str) -> Branch {
        // (the place's length tells where it ends, whatever characters the two paths hold)
        Branch { key: format!("local:{}:{place}{path}", place.len()), starts_open: false }
    }

    /// A year or month of By Date (`2026`, `2026-05`).
    pub fn date(key: &str) -> Branch {
        Branch { key: format!("date:{key}"), starts_open: false }
    }

    /// A folder of the library (`key`: [`lightcraft_catalog::query::folder_key`]), which starts
    /// as `starts_open` says (a disk, and a folder that only leads to one folder, start open).
    pub fn folder(key: &str, starts_open: bool) -> Branch {
        Branch { key: format!("folder:{key}"), starts_open }
    }

    /// A keyword with keywords inside it, by its path (any case).
    pub fn keyword(path: &str) -> Branch {
        Branch { key: format!("keyword:{}", path.to_lowercase()), starts_open: false }
    }

    /// The row's id for egui (its triangle is a widget of its own).
    pub fn id(&self) -> egui::Id {
        egui::Id::unique(("tree-row", &self.key))
    }
}

/// The rows that were opened or folded by hand (row → open), and those open for this session.
#[derive(Clone, Debug, Default)]
pub struct TreeState {
    /// What was said of a row, and of the rows inside one ([`INSIDE`]).
    rows: HashMap<String, Said>,
    changes: u64,
    /// How many of `rows` are things said of the rows inside a row ([`INSIDE`]): most often
    /// none, and then nothing is looked up for them.
    whole: usize,
    /// Rows opened to show where the grid is, for this session, and when (never one of `rows`).
    shown: HashMap<String, u64>,
}

impl TreeState {
    /// Whether `row` is open, for a row with no row above it (or to ask what was said of the
    /// row alone): as it was left, else opened to show where the grid is, else as it starts.
    pub fn is_open(&self, row: &Branch) -> bool {
        self.is_open_under(row, None)
    }

    /// Whether `row` is open, given what was last said of all the rows inside the rows `above`
    /// it ([`TreeState::inside`]): as the last of those things has it (said of the row by hand,
    /// said of the rows it lies in, opened to show where the grid is), else as it starts.
    pub fn is_open_under(&self, row: &Branch, above: Option<Said>) -> bool {
        let shown = self.shown.get(&row.key).map(|when| (true, *when));
        let last = [self.rows.get(&row.key).copied(), shown, above].into_iter().flatten().max_by_key(|(_, when)| *when);
        last.map_or(row.starts_open, |(open, _)| open)
    }

    /// What holds for all the rows inside `row`, if anything was said of them: the later of what
    /// was said of the rows inside `row` itself and `above`, which holds for `row` from the rows
    /// above it.
    pub fn inside(&self, row: &Branch, above: Option<Said>) -> Option<Said> {
        if self.whole == 0 {
            return above;
        }
        let own = self.rows.get(&format!("{INSIDE}{}", row.key)).copied();
        [own, above].into_iter().flatten().max_by_key(|(_, when)| *when)
    }

    /// `row` is open (or folded) from now on: somebody opened or folded it.
    pub fn set(&mut self, row: &Branch, open: bool) {
        self.shown.remove(&row.key);
        self.keep(row.key.clone(), open);
    }

    /// `row` and all the rows inside it, however many and however deep, are open (or folded)
    /// from now on: somebody opened or folded the whole of it. Two things are kept, of the row
    /// and of what is inside it; what is said of a row inside it later holds for that row.
    pub fn set_with_inside(&mut self, row: &Branch, open: bool) {
        // (what is inside first: should the two ever make room one after the other, the row
        // stays as it was left the longer, and what is inside it goes back to how it starts)
        self.keep(format!("{INSIDE}{}", row.key), open);
        self.set(row, open);
    }

    /// Nothing is said of the rows inside `row` any more: they have no rows inside them now.
    pub fn forget_inside(&mut self, row: &Branch) {
        if self.whole > 0 && self.rows.remove(&format!("{INSIDE}{}", row.key)).is_some() {
            self.whole = self.whole.saturating_sub(1);
        }
    }

    /// Opens `row` to show where the grid is, or something just made. A row of which nothing
    /// was said is open for this session. One that was said to be folded, itself or with the
    /// rows `above` it ([`TreeState::inside`]), is opened for good: it is open now, can be
    /// folded again, and is open when the app starts again on what it shows. A row said to be
    /// open already is left as it is.
    pub fn show(&mut self, row: &Branch, above: Option<Said>) {
        let shown = self.shown.get(&row.key).map(|when| (true, *when));
        let last = [self.rows.get(&row.key).copied(), shown, above].into_iter().flatten().max_by_key(|(_, when)| *when);
        match last {
            Some((true, _)) => {}
            Some((false, _)) => {
                self.shown.remove(&row.key);
                self.keep(row.key.clone(), true);
            }
            // (though it may start open: how a row starts can change, and it was opened)
            None => {
                self.changes = self.changes.saturating_add(1);
                self.shown.insert(row.key.clone(), self.changes);
            }
        }
    }

    /// Opens `row` for this session unless it was left some way already, by what was said of it
    /// or of the rows `above` it. For showing where the grid is when that is no choice of now
    /// but what the library opened with (as the app starts, or by Open Library): the trees are
    /// then as they were left, and a row that was folded stays folded.
    pub fn show_unless_left(&mut self, row: &Branch, above: Option<Said>) {
        if above.is_none() && !self.rows.contains_key(&row.key) && !self.shown.contains_key(&row.key) {
            self.changes = self.changes.saturating_add(1);
            self.shown.insert(row.key.clone(), self.changes);
        }
    }

    /// Keeps what `before` held of this session, for settings that were written and read
    /// again while the app runs with the same rows (`ui.set` of something else): the rows it had
    /// open for the session, and when each thing was said, which what is written does not hold.
    pub fn carry_session(&mut self, before: &TreeState) {
        // (a row said as before keeps its time; the others are said now, in their order)
        let mut order: Vec<(u64, String)> = self.rows.iter().map(|(key, (_, when))| (*when, key.clone())).collect();
        order.sort_unstable();
        let mut now = before.changes;
        for (_, key) in order {
            let Some(row) = self.rows.get_mut(&key) else { continue };
            row.1 = match before.rows.get(&key) {
                Some((open, when)) if *open == row.0 => *when,
                _ => {
                    now = now.saturating_add(1);
                    now
                }
            };
        }
        self.changes = now;
        self.shown = before.shown.iter().filter(|(key, _)| !self.rows.contains_key(*key)).map(|(key, when)| (key.clone(), *when)).collect();
    }

    /// Keeps the rows `before` had open for the session under rows that a script has just set
    /// (`ui.set` with `sidebarTrees`): the list holds as it was given, in its order, and what
    /// was open for the session comes before all of it.
    pub fn carry_session_under(&mut self, before: &TreeState) {
        self.shown = before.shown.keys().filter(|key| !self.rows.contains_key(*key)).map(|key| (key.clone(), 0)).collect();
    }

    fn keep(&mut self, key: String, open: bool) {
        self.changes = self.changes.saturating_add(1);
        if !self.rows.contains_key(&key) && self.rows.len() >= MAX_ROWS {
            // the row changed longest ago makes room
            if let Some(oldest) = self.rows.iter().min_by_key(|(key, (_, when))| (*when, key.as_str())).map(|(key, _)| key.clone()) {
                self.rows.remove(&oldest);
                self.whole = self.whole.saturating_sub(usize::from(oldest.starts_with(INSIDE)));
            }
        }
        let whole = key.starts_with(INSIDE);
        if self.rows.insert(key, (open, self.changes)).is_none() && whole {
            self.whole = self.whole.saturating_add(1);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// The rows, the one changed longest ago first.
    fn listed(&self) -> Vec<(&str, bool)> {
        let mut rows: Vec<(&str, bool, u64)> = self.rows.iter().map(|(key, (open, when))| (key.as_str(), *open, *when)).collect();
        rows.sort_unstable_by_key(|(key, _, when)| (*when, *key));
        rows.into_iter().map(|(key, open, _)| (key, open)).collect()
    }

    /// One entry of the settings' list as a row: `["album:12", false]`.
    fn row_of(entry: &serde_json::Value) -> Option<(&str, bool)> {
        match entry.as_array().map(Vec::as_slice) {
            Some([key, open]) => key.as_str().filter(|key| !key.is_empty()).zip(open.as_bool()),
            _ => None,
        }
    }

    /// The rows of `value` (the settings' list). `strict`: anything that is not a list of no
    /// more rows than are kept is an error that says so; else what is no row is passed over, and
    /// of more rows than are kept the last (those changed latest) are taken. A row of a tree this
    /// version does not have is kept in a settings file (a newer version wrote it, and is not made
    /// to lose it); a script's is refused.
    fn read(value: &serde_json::Value, strict: bool) -> Result<TreeState, String> {
        let what = "`sidebarTrees` is a list of rows that were opened or folded, the one changed longest ago first: [[\"album:12\", false], [\"date:2026\", true]]";
        let entries = match value {
            serde_json::Value::Array(entries) => entries.as_slice(),
            _ if strict => return Err(what.to_string()),
            _ => &[],
        };
        if strict && entries.len() > MAX_ROWS {
            return Err(format!("{what} (no more than {MAX_ROWS} rows, not {})", entries.len()));
        }
        let mut rows = Vec::new();
        for (i, entry) in entries.iter().enumerate() {
            match TreeState::row_of(entry) {
                // (a settings file may hold the rows of a tree this version does not have)
                Some((key, _)) if strict && !TREES.iter().any(|tree| key.strip_prefix(INSIDE).unwrap_or(key).starts_with(tree)) => {
                    return Err(format!("{what} (entry {i} is a row of no tree: {})", TREES.join(" ")));
                }
                Some(row) => rows.push(row),
                None if strict => return Err(format!("{what} (entry {i} is not a row)")),
                None => {}
            }
        }
        let mut state = TreeState::default();
        for (key, open) in rows.into_iter().rev().take(MAX_ROWS).rev() {
            state.keep(key.to_string(), open);
        }
        Ok(state)
    }

    /// What a script sets (`ui.set`): checked, and refused with the reason when it is not a list
    /// of rows, where a settings file's is read as far as it goes.
    pub fn parse(value: &serde_json::Value) -> Result<TreeState, String> {
        TreeState::read(value, true)
    }
}

impl PartialEq for TreeState {
    /// The same rows, each open or folded alike (when they were changed is not what they are,
    /// and what is open for the session is not kept).
    fn eq(&self, other: &Self) -> bool {
        self.rows.len() == other.rows.len() && self.rows.iter().all(|(key, (open, _))| other.rows.get(key).is_some_and(|(o, _)| o == open))
    }
}

impl Serialize for TreeState {
    /// In the settings: `[["album:12", false], ["date:2026", true]]`, the row changed longest ago
    /// first, so that the order they make room in is kept too, and the file only changes when a
    /// row does.
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.listed().serialize(s)
    }
}

impl<'de> Deserialize<'de> for TreeState {
    /// Read leniently: the settings are a file a person or another version may have written.
    /// Anything that is not a row that is open or folded is passed over, no more than
    /// [`MAX_ROWS`] are taken, and nothing here ever costs the other settings.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        Ok(TreeState::read(&value, false).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Given trees whose rows start open (Albums' folders) or folded (the others), when a row is
    /// folded or opened, then it is as it was left, and the other rows are as they start. Put
    /// back as it starts, it is kept as that: one entry for a row, however often it is changed.
    #[test]
    fn a_row_is_as_it_was_left() {
        let mut trees = TreeState::default();
        let (album, date) = (Branch::album(12), Branch::date("2026-05"));
        assert!(trees.is_open(&album) && !trees.is_open(&date), "as they start");
        assert!(trees.is_empty());
        trees.set(&album, false);
        trees.set(&date, true);
        assert!(!trees.is_open(&album) && trees.is_open(&date));
        assert_eq!(trees.len(), 2);
        // other rows of the same trees are as they start
        assert!(trees.is_open(&Branch::album(13)) && !trees.is_open(&Branch::date("2026-06")));
        // said again, and put back as they start: the same two rows
        trees.set(&date, true);
        trees.set(&album, true);
        trees.set(&date, false);
        assert!(trees.is_open(&album) && !trees.is_open(&date));
        assert_eq!(trees.len(), 2);
    }

    /// Given a folder of the library that starts open because it only leads to one folder, when
    /// it is opened (to show a folder inside it, which changes nothing to see) and a second
    /// folder then appears inside it, so that it would start folded, then it is still open: what
    /// was said of a row holds when the way it starts changes. A row nobody touched follows the
    /// way it starts.
    #[test]
    fn a_row_stays_as_it_was_left_when_the_way_it_starts_changes() {
        let mut trees = TreeState::default();
        trees.set(&Branch::folder("/photos/2026", true), true);
        assert!(trees.is_open(&Branch::folder("/photos/2026", false)), "opened, and it stays open");
        trees.set(&Branch::folder("/photos/2025", false), false);
        assert!(!trees.is_open(&Branch::folder("/photos/2025", true)), "folded, and it stays folded");
        assert!(trees.is_open(&Branch::folder("/photos/2024", true)) && !trees.is_open(&Branch::folder("/photos/2024", false)));
    }

    /// Given the trees as they were left, when the library opens and the app shows where the
    /// grid is, then a row on the way that nobody left any way is opened for the session, one
    /// left open stays open, and one that was folded stays folded. Nothing more is kept for it.
    #[test]
    fn showing_where_the_grid_is_as_the_library_opens_leaves_a_folded_row_folded() {
        let mut trees = TreeState::default();
        let (folded, open, untouched) =
            (Branch::local("/home", "/home/a"), Branch::local("/home", "/home/a/b"), Branch::local("/home", "/home/a/b/c"));
        trees.set(&folded, false);
        trees.set(&open, true);
        let kept = serde_json::to_string(&trees).unwrap();
        for row in [&folded, &open, &untouched] {
            trees.show_unless_left(row, None);
        }
        assert!(!trees.is_open(&folded) && trees.is_open(&open) && trees.is_open(&untouched));
        assert_eq!(serde_json::to_string(&trees).unwrap(), kept);
        // (a folder of albums, which starts open, folded by hand: the same)
        trees.set(&Branch::album(3), false);
        trees.show_unless_left(&Branch::album(3), None);
        assert!(!trees.is_open(&Branch::album(3)));
    }

    /// Given a folder chosen while the app runs, when the rows above it open to show it, then
    /// they are open for the session and not kept: after a restart they are as they start. A row
    /// on the way that was folded by hand is opened for good. A row opened to show the grid and
    /// then folded by hand is folded, and kept as that.
    #[test]
    fn rows_opened_to_show_where_the_grid_is_are_open_for_the_session() {
        let mut trees = TreeState::default();
        let (above, folded) = (Branch::folder("/pics", false), Branch::folder("/pics/trip", false));
        trees.set(&folded, false);
        trees.show(&above, None);
        trees.show(&folded, None);
        assert!(trees.is_open(&above) && trees.is_open(&folded));
        assert_eq!(serde_json::to_string(&trees).unwrap(), r#"[["folder:/pics/trip",true]]"#, "only the row that had been folded is kept");
        // shown again: nothing changes, nor does the file
        trees.show(&above, None);
        trees.show(&folded, None);
        assert_eq!(serde_json::to_string(&trees).unwrap(), r#"[["folder:/pics/trip",true]]"#);
        // the way a row starts changes (a second folder appears inside it): it is still open
        assert!(trees.is_open(&Branch::folder("/pics", true)) && trees.is_open(&Branch::folder("/pics", false)));
        let restarted: TreeState = serde_json::from_str(&serde_json::to_string(&trees).unwrap()).unwrap();
        assert!(!restarted.is_open(&above) && restarted.is_open(&folded));
        // folded by hand after it was opened to show the grid
        trees.set(&above, false);
        assert!(!trees.is_open(&above));
        trees.show_unless_left(&above, None);
        assert!(!trees.is_open(&above));
        assert_eq!(trees.len(), 2);
        // settings written and read again while the app runs keep what is open for the session
        let mut trees = TreeState::default();
        trees.show(&above, None);
        trees.set(&folded, false);
        let mut reread: TreeState = serde_json::from_str(&serde_json::to_string(&trees).unwrap()).unwrap();
        assert!(!reread.is_open(&above));
        reread.carry_session(&trees);
        assert!(reread.is_open(&above) && !reread.is_open(&folded) && reread == trees);
    }

    /// Each tree's rows have names of their own: an album and a date of the same number, a
    /// folder listed in two Local places, and two places whose paths run into each other are
    /// different rows; a keyword is the same row in any case.
    #[test]
    fn rows_are_told_apart() {
        let keys = [
            Branch::album(2026).key,
            Branch::date("2026").key,
            Branch::folder("2026", false).key,
            Branch::keyword("2026").key,
            Branch::local("/home", "/home/pictures").key,
            Branch::local("/home/pictures", "/home/pictures").key,
            Branch::local("/a", "b/c").key,
            Branch::local("/ab", "/c").key,
        ];
        let distinct: std::collections::HashSet<&String> = keys.iter().collect();
        assert_eq!(distinct.len(), keys.len(), "{keys:?}");
        assert_eq!(Branch::keyword("Travel|Italy"), Branch::keyword("travel|ITALY"));
        let ids: std::collections::HashSet<egui::Id> =
            [Branch::album(1), Branch::album(2), Branch::date("1"), Branch::keyword("1"), Branch::folder("1", true)].iter().map(Branch::id).collect();
        assert_eq!(ids.len(), 5, "and each has a triangle of its own");
    }

    /// Given rows that were changed, when the settings are written and read again, then the rows
    /// are as they were; the file lists them in the order they were changed, the same however
    /// often it is written, and holds an empty list when nothing was changed.
    #[test]
    fn the_settings_keep_the_rows() {
        let mut trees = TreeState::default();
        trees.set(&Branch::date("2026"), true);
        trees.set(&Branch::album(12), false);
        let written = serde_json::to_string(&trees).unwrap();
        assert_eq!(written, r#"[["date:2026",true],["album:12",false]]"#);
        let read: TreeState = serde_json::from_str(&written).unwrap();
        assert_eq!(read, trees);
        assert!(!read.is_open(&Branch::album(12)) && read.is_open(&Branch::date("2026")));
        assert_eq!(serde_json::to_string(&read).unwrap(), written, "read and written again: the same file");
        assert_eq!(serde_json::to_string(&TreeState::default()).unwrap(), "[]");
    }

    /// What the settings hold is not trusted as it is: anything that is no row is passed over
    /// without an error (which would have the whole settings file taken for damaged), and of
    /// more rows than are ever kept the ones changed latest are taken.
    #[test]
    fn what_is_no_row_is_passed_over() {
        for odd in ["null", "5", r#""open""#, "[]", "[true]", "{}", r#"{"album:1":false}"#, "[[]]", r#"[["album:1"]]"#] {
            assert!(serde_json::from_str::<TreeState>(odd).unwrap().is_empty(), "{odd}");
        }
        let mixed: TreeState = serde_json::from_str(
            r#"[["album:1",false],["album:2","folded"],["album:3",null],[4,true],"album:5",["date:2026",true],["album:6",false,1],["",true]]"#,
        )
        .unwrap();
        assert_eq!(mixed.len(), 2);
        assert!(!mixed.is_open(&Branch::album(1)) && mixed.is_open(&Branch::album(2)) && mixed.is_open(&Branch::date("2026")));
        // a row listed twice is as it is listed last
        let twice: TreeState = serde_json::from_str(r#"[["album:1",false],["album:1",true]]"#).unwrap();
        assert!(twice.len() == 1 && twice.is_open(&Branch::album(1)));
        let many = format!("[{}]", (0..MAX_ROWS + 500).map(|i| format!(r#"["album:{i}",false]"#)).collect::<Vec<_>>().join(","));
        let many: TreeState = serde_json::from_str(&many).unwrap();
        assert_eq!(many.len(), MAX_ROWS);
        assert!(many.is_open(&Branch::album(499)) && !many.is_open(&Branch::album(500)) && !many.is_open(&Branch::album(MAX_ROWS as u64 + 499)));
    }

    /// What a script sets is checked instead: a list of rows is taken, anything else is refused
    /// with what the list looks like, so a slip does not empty the trees without a word.
    #[test]
    fn what_a_script_sets_is_checked() {
        let set = |text: &str| TreeState::parse(&serde_json::from_str(text).unwrap());
        let rows = set(r#"[["album:1",false],["date:2026",true]]"#).unwrap();
        assert!(rows.len() == 2 && !rows.is_open(&Branch::album(1)));
        assert!(set("[]").unwrap().is_empty());
        for odd in ["null", "5", r#""open""#, r#"{"album:1":false}"#, "[true]", r#"[["album:1","folded"]]"#, r#"[["album:1",false],["album:2"]]"#] {
            let refused = set(odd).unwrap_err();
            assert!(refused.contains("`sidebarTrees` is a list of rows"), "{odd}: {refused}");
        }
        assert!(set(r#"[["album:1",false],["album:2"]]"#).unwrap_err().contains("entry 1"));
        let no_tree = set(r#"[["album:1",false],["albums:2",false]]"#).unwrap_err();
        assert!(no_tree.contains("entry 1 is a row of no tree: album: date: folder: keyword: local:"), "{no_tree}");
        // (every tree's rows are taken)
        for row in
            [Branch::album(1), Branch::date("2026"), Branch::folder("/pics", true), Branch::keyword("travel"), Branch::local("/home", "/home/a")]
        {
            assert!(TREES.iter().any(|tree| row.key.starts_with(tree)), "{}", row.key);
            assert_eq!(set(&format!(r#"[["{}",true]]"#, row.key)).map(|t| t.len()), Ok(1), "{}", row.key);
        }
        // more rows than are kept: refused, where a settings file's are cut to the last
        let many = format!("[{}]", (0..=MAX_ROWS).map(|i| format!(r#"["album:{i}",false]"#)).collect::<Vec<_>>().join(","));
        assert!(set(&many).unwrap_err().contains("no more than 2000 rows"));
        // what the app writes is what it takes
        let mut trees = TreeState::default();
        trees.set(&Branch::local("/home", &"deep/".repeat(4000)), true);
        assert_eq!(set(&serde_json::to_string(&trees).unwrap()).unwrap(), trees);
    }

    /// Given as many changed rows as are ever kept, when another is changed, then the row changed
    /// longest ago goes and the new one is kept: the settings never grow past that. A row that is
    /// changed again counts as changed then. Written to the settings and read again, the rows
    /// still make room in that order.
    #[test]
    fn the_rows_changed_longest_ago_make_room() {
        let mut trees = TreeState::default();
        for i in 0..MAX_ROWS as u64 {
            trees.set(&Branch::album(i), false);
        }
        assert_eq!(trees.len(), MAX_ROWS);
        // the first is folded again (it is kept already: that is a change now)
        trees.set(&Branch::album(0), false);
        trees.set(&Branch::date("2026"), true);
        assert_eq!(trees.len(), MAX_ROWS);
        assert!(trees.is_open(&Branch::date("2026")), "the new row is kept");
        assert!(!trees.is_open(&Branch::album(0)), "and the one changed again");
        assert!(trees.is_open(&Branch::album(1)), "the one changed longest ago is as it starts again");
        assert!(!trees.is_open(&Branch::album(2)));

        // after a restart: the next to go is the one changed longest ago, not the first by name
        let mut trees: TreeState = serde_json::from_str(&serde_json::to_string(&trees).unwrap()).unwrap();
        trees.set(&Branch::keyword("travel"), true);
        assert_eq!(trees.len(), MAX_ROWS);
        assert!(trees.is_open(&Branch::album(2)), "album 2 was changed longest ago");
        assert!(!trees.is_open(&Branch::album(0)) && !trees.is_open(&Branch::album(3)) && trees.is_open(&Branch::date("2026")));
        assert!(trees.is_open(&Branch::keyword("travel")));
    }
}
