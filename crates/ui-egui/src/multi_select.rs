//! Choosing several rows of a list at once: Cmd/Ctrl+click adds a row or takes it away,
//! Shift+click takes every row from the row last clicked without Shift (else the row chosen last)
//! to this one. One rule for the sidebars' lists (albums, folders, keywords, dates, the Keyword
//! List), so they all answer the same way.
//!
//! The rows chosen are kept in the order chosen; the last of them is the one an action for a
//! single row takes (Rename…, Edit…).
//!
//! Nothing calls this yet. It is the model for LR-BEHAV-MULTISELECT's sidebar multi-select (the
//! albums, folders, keywords and dates lists and the Keyword List), whose menus then act on every
//! row chosen through the batch commands (`album.delete`, `keyword.delete`, … taking several
//! ids). It keeps no egui state (only `egui::Modifiers` says which keys were held) and calls no
//! engine code, so those parts can be built on it.

/// What a click asks for, by the keys held with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    /// Choose this row alone.
    Plain,
    /// Cmd/Ctrl: add this row to those chosen, or take it away.
    Toggle,
    /// Shift: the rows from the anchor to this one, instead of what was chosen.
    Range,
    /// Cmd/Ctrl+Shift: the rows from the anchor to this one, added to what was chosen.
    AddRange,
}

impl Click {
    pub fn of(m: egui::Modifiers) -> Click {
        match (m.shift, m.command) {
            (true, true) => Click::AddRange,
            (true, false) => Click::Range,
            (false, true) => Click::Toggle,
            (false, false) => Click::Plain,
        }
    }
}

/// The rows chosen after a click, and where the next Shift+click starts from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chosen<K> {
    /// In the order chosen; the last is the one a single-row action takes.
    pub keys: Vec<K>,
    pub anchor: Option<K>,
}

/// What a click on the row `at` makes of the rows `chosen`. `rows` are the list's rows as drawn,
/// top to bottom: every row of an open level (also one a frame skipped drawing because it was
/// scrolled out of view), and none inside a closed level, so a range never takes rows that are
/// folded away; `anchor` is the row last clicked without Shift; `same` tells two rows' keys alike.
///
/// A range needs its anchor among `rows`: without one (nothing clicked yet, or it was folded
/// away) Shift+click chooses the row alone, like a plain click.
pub fn click<K: Clone>(rows: &[K], chosen: &[K], anchor: Option<&K>, at: &K, how: Click, same: &dyn Fn(&K, &K) -> bool) -> Chosen<K> {
    let alone = || Chosen { keys: vec![at.clone()], anchor: Some(at.clone()) };
    match how {
        Click::Plain => alone(),
        Click::Toggle => {
            let mut keys: Vec<K> = chosen.to_vec();
            let before = keys.len();
            keys.retain(|k| !same(k, at));
            if keys.len() == before {
                keys.push(at.clone());
            }
            Chosen { keys, anchor: Some(at.clone()) }
        }
        Click::Range | Click::AddRange => {
            let index = |k: &K| rows.iter().position(|r| same(r, k));
            let (Some(from), Some(to)) = (anchor.and_then(index), index(at)) else { return alone() };
            let mut keys: Vec<K> = if how == Click::AddRange { chosen.to_vec() } else { Vec::new() };
            // from the anchor towards the row clicked, so that one is the last chosen
            let range: Vec<&K> = if from <= to {
                rows.get(from..=to).unwrap_or_default().iter().collect()
            } else {
                rows.get(to..=from).unwrap_or_default().iter().rev().collect()
            };
            for k in range {
                keys.retain(|x| !same(x, k));
                keys.push(k.clone());
            }
            Chosen { keys, anchor: anchor.cloned() }
        }
    }
}

/// What a context menu opened on the row `at` acts on: every row chosen when `at` is one of
/// several chosen, else that row alone (the rows chosen stay as they are either way).
pub fn targets<K: Clone>(chosen: &[K], at: &K, same: &dyn Fn(&K, &K) -> bool) -> Vec<K> {
    if chosen.len() > 1 && chosen.iter().any(|k| same(k, at)) { chosen.to_vec() } else { vec![at.clone()] }
}

/// One list's rows as drawn, for Shift+click: `seen` while a frame draws them, `rows` of the
/// frame before (what the person clicked on), and the anchor.
#[derive(Clone, Debug)]
pub struct ListRows<K> {
    seen: Vec<K>,
    rows: Vec<K>,
    pub anchor: Option<K>,
}

impl<K> Default for ListRows<K> {
    fn default() -> Self {
        ListRows { seen: Vec::new(), rows: Vec::new(), anchor: None }
    }
}

impl<K: Clone> ListRows<K> {
    /// A new frame: the rows of the last one are what a click now is on.
    pub fn begin_frame(&mut self) {
        std::mem::swap(&mut self.rows, &mut self.seen);
        self.seen.clear();
    }
    /// A row of this frame, in drawing order.
    pub fn saw(&mut self, key: K) {
        self.seen.push(key);
    }
    /// The rows a click is on: the last frame's.
    pub fn rows(&self) -> &[K] {
        &self.rows
    }
    /// A click on `at` with `how`, among the rows drawn; remembers the anchor.
    pub fn click(&mut self, chosen: &[K], at: &K, how: Click, same: &dyn Fn(&K, &K) -> bool) -> Vec<K> {
        // a range starts from a row that is chosen: the one last clicked while it still is, else
        // the one chosen last (chosen by a menu, an agent or the last session, or after what was
        // clicked stopped being shown)
        let anchor = self.anchor.as_ref().filter(|a| chosen.iter().any(|c| same(c, a))).or(chosen.last());
        let out = click(&self.rows, chosen, anchor, at, how, same);
        self.anchor = out.anchor;
        out.keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROWS: [&str; 6] = ["a", "b", "c", "d", "e", "f"];
    fn eq(a: &&str, b: &&str) -> bool {
        a == b
    }
    fn go(chosen: &[&'static str], anchor: Option<&'static str>, at: &'static str, how: Click) -> (Vec<&'static str>, Option<&'static str>) {
        let out = click(&ROWS, chosen, anchor.as_ref(), &at, how, &eq);
        (out.keys, out.anchor)
    }

    #[test]
    fn the_keys_held_say_what_a_click_asks_for() {
        use egui::Modifiers as M;
        assert_eq!(Click::of(M::NONE), Click::Plain);
        assert_eq!(Click::of(M::COMMAND), Click::Toggle);
        assert_eq!(Click::of(M::SHIFT), Click::Range);
        assert_eq!(Click::of(M::SHIFT | M::COMMAND), Click::AddRange);
        // Alt alone asks for nothing special
        assert_eq!(Click::of(M::ALT), Click::Plain);
    }

    #[test]
    fn a_plain_click_chooses_the_row_alone() {
        assert_eq!(go(&["a", "c"], Some("a"), "e", Click::Plain), (vec!["e"], Some("e")));
        assert_eq!(go(&["a", "c"], Some("a"), "c", Click::Plain), (vec!["c"], Some("c")), "also one of several chosen");
    }

    #[test]
    fn cmd_click_adds_a_row_or_takes_it_away() {
        assert_eq!(go(&["a"], Some("a"), "c", Click::Toggle), (vec!["a", "c"], Some("c")));
        assert_eq!(go(&["a", "c"], Some("c"), "a", Click::Toggle), (vec!["c"], Some("a")));
        assert_eq!(go(&["a"], Some("a"), "a", Click::Toggle), (vec![], Some("a")), "the last one can be taken away too");
    }

    #[test]
    fn shift_click_takes_the_rows_from_the_anchor() {
        // downwards and upwards; the row clicked is the last chosen, and the anchor stays
        assert_eq!(go(&["b"], Some("b"), "e", Click::Range), (vec!["b", "c", "d", "e"], Some("b")));
        assert_eq!(go(&["e"], Some("e"), "c", Click::Range), (vec!["e", "d", "c"], Some("e")));
        // a second Shift+click from the same anchor replaces the first range
        assert_eq!(go(&["b", "c", "d", "e"], Some("b"), "c", Click::Range), (vec!["b", "c"], Some("b")));
        // …and what was chosen elsewhere goes
        assert_eq!(go(&["f", "b"], Some("b"), "c", Click::Range), (vec!["b", "c"], Some("b")));
    }

    #[test]
    fn cmd_shift_click_adds_the_range_to_what_was_chosen() {
        assert_eq!(go(&["a", "d"], Some("d"), "f", Click::AddRange), (vec!["a", "d", "e", "f"], Some("d")));
        // a row already chosen is not chosen twice, and the row clicked is still the last
        assert_eq!(go(&["e", "c"], Some("c"), "e", Click::AddRange), (vec!["c", "d", "e"], Some("c")));
    }

    #[test]
    fn a_range_without_an_anchor_in_sight_chooses_the_row_alone() {
        assert_eq!(go(&[], None, "c", Click::Range), (vec!["c"], Some("c")));
        // the anchor was folded away since (not among the rows drawn)
        assert_eq!(go(&["x"], Some("x"), "c", Click::Range), (vec!["c"], Some("c")));
        // a row that is not drawn (a stale click) chooses itself and nothing else
        assert_eq!(go(&["a"], Some("a"), "zz", Click::AddRange), (vec!["zz"], Some("zz")));
        // the anchor is the row clicked: that row alone, anchor kept
        assert_eq!(go(&["b", "c"], Some("b"), "b", Click::Range), (vec!["b"], Some("b")));
        // no rows drawn at all
        let none: [&str; 0] = [];
        assert_eq!(click(&none, &["a"], Some(&"a"), &"c", Click::Range, &eq).keys, vec!["c"]);
    }

    #[test]
    fn a_menu_acts_on_the_rows_chosen_only_when_opened_on_one_of_them() {
        assert_eq!(targets(&["a", "c"], &"c", &eq), vec!["a", "c"]);
        assert_eq!(targets(&["a", "c"], &"e", &eq), vec!["e"], "another row: that row alone");
        assert_eq!(targets(&["a"], &"a", &eq), vec!["a"]);
        assert_eq!(targets(&[], &"a", &eq), vec!["a"]);
    }

    #[test]
    fn a_list_s_clicks_are_on_the_rows_of_the_frame_before() {
        let mut l: ListRows<&str> = ListRows::default();
        for r in ROWS {
            l.saw(r);
        }
        // nothing drawn before this frame: no range yet
        assert_eq!(l.click(&[], &"b", Click::Plain, &eq), vec!["b"]);
        l.begin_frame();
        for r in ["a", "b", "e", "f"] {
            l.saw(r); // c and d folded away, but this frame is not the one clicked on
        }
        assert_eq!(l.click(&["b"], &"d", Click::Range, &eq), vec!["b", "c", "d"]);
        // the row clicked is not chosen any more (All Photos was shown since): a range does not
        // start from it; it starts from what is chosen now, or the row is chosen alone
        assert_eq!(l.click(&["f"], &"d", Click::Range, &eq), vec!["f", "e", "d"]);
        assert_eq!(l.click(&[], &"c", Click::Range, &eq), vec!["c"]);
        assert_eq!(l.click(&["c"], &"b", Click::Plain, &eq), vec!["b"]);
        l.begin_frame();
        assert_eq!(l.rows(), ["a", "b", "e", "f"]);
        assert_eq!(l.click(&["b"], &"e", Click::Range, &eq), vec!["b", "e"], "rows folded away are not taken");
    }
}
