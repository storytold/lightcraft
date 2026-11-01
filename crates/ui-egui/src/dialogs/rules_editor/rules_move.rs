//! The move engine behind reordering rules and groups in the smart-album editor
//! ([`super::rules_editor`]): pure functions on a [`RuleSet`], and nothing else. A row is a path of
//! positions from the top list (`[1, 0]`: the first rule of the second rule's group); a place to
//! drop is a path too: the group's path and the position among its rules, counted before the row
//! leaves its old place.
//!
//! [`judge`], [`move_node`] and [`key_dest`] never trust a path: one that leads nowhere changes
//! nothing. A group left empty by a move goes away, and groups nest at most [`MAX_GROUP_DEPTH`] deep.

use lightcraft_catalog::{Rule, RuleSet};

/// Groups nest at most this deep: the editor offers a group only in a list whose depth is below it
/// (the top list is depth 0).
pub const MAX_GROUP_DEPTH: usize = 3;

/// Where the arrow keys send a row (↑ ↓ ← →).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    /// ← : out of its group.
    Out,
    /// → : into the group next to it.
    In,
}

/// Why a move is refused (shown, not just ignored).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A group dropped inside itself or something in it.
    IntoItself,
    /// A group dropped where its rules would nest deeper than [`MAX_GROUP_DEPTH`].
    TooDeep,
}

/// What a move of one row to a place would do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Moves,
    /// Where it already is, or a place or row that isn't there.
    Nothing,
    Refused(Refusal),
}

/// The group (or the top list) at `path`.
fn list_at<'a>(rs: &'a RuleSet, path: &[usize]) -> Option<&'a RuleSet> {
    let mut list = rs;
    for &i in path {
        match list.rules.get(i)? {
            Rule::Group { group } => list = group,
            Rule::Field { .. } => return None,
        }
    }
    Some(list)
}

fn list_at_mut<'a>(rs: &'a mut RuleSet, path: &[usize]) -> Option<&'a mut RuleSet> {
    let mut list = rs;
    for &i in path {
        match list.rules.get_mut(i)? {
            Rule::Group { group } => list = group,
            Rule::Field { .. } => return None,
        }
    }
    Some(list)
}

/// The rule or group at `path`.
fn node_at<'a>(rs: &'a RuleSet, path: &[usize]) -> Option<&'a Rule> {
    let (last, parent) = path.split_last()?;
    list_at(rs, parent)?.rules.get(*last)
}

/// How many lists a rule brings: none for a field rule, one more than its deepest group for a group.
fn height(rule: &Rule) -> usize {
    match rule {
        Rule::Field { .. } => 0,
        Rule::Group { group } => 1 + group.rules.iter().map(height).max().unwrap_or(0),
    }
}

fn remove_at(rs: &mut RuleSet, path: &[usize]) -> Option<Rule> {
    let (last, parent) = path.split_last()?;
    let list = list_at_mut(rs, parent)?;
    (*last < list.rules.len()).then(|| list.rules.remove(*last))
}

fn insert_at(rs: &mut RuleSet, path: &[usize], rule: Rule) -> Option<()> {
    let (last, parent) = path.split_last()?;
    let list = list_at_mut(rs, parent)?;
    if *last > list.rules.len() {
        return None;
    }
    list.rules.insert(*last, rule);
    Some(())
}

/// `path` as it reads once the row at `removed` is gone: a later sibling's position drops by one.
/// `false` when `path` runs through the removed row itself.
fn shift_after_removal(path: &mut [usize], removed: &[usize]) -> bool {
    let Some((last, parent)) = removed.split_last() else { return true };
    if path.len() <= parent.len() || !path.starts_with(parent) {
        return true;
    }
    let through = path.len() > removed.len();
    let Some(at) = path.get_mut(parent.len()) else { return true };
    if *at > *last {
        *at -= 1;
        true
    } else {
        *at != *last || !through
    }
}

/// What moving the row at `from` to the place `dest` would do.
pub fn judge(rs: &RuleSet, from: &[usize], dest: &[usize]) -> Verdict {
    let (Some((from_last, from_parent)), Some((dest_last, dest_parent))) = (from.split_last(), dest.split_last()) else {
        return Verdict::Nothing;
    };
    let Some(node) = node_at(rs, from) else { return Verdict::Nothing };
    if matches!(node, Rule::Group { .. }) && dest_parent.starts_with(from) {
        return Verdict::Refused(Refusal::IntoItself);
    }
    let Some(list) = list_at(rs, dest_parent) else { return Verdict::Nothing };
    if *dest_last > list.rules.len() {
        return Verdict::Nothing;
    }
    // before itself, or after itself: the same place
    if dest_parent == from_parent && (dest_last == from_last || *dest_last == from_last.saturating_add(1)) {
        return Verdict::Nothing;
    }
    // a group may not end up deeper than the editor offers, but one that is already deeper (rules
    // written by an agent) can still be moved about at its own depth or higher
    let h = height(node);
    let (now, then) = (from_parent.len() + h, dest_parent.len() + h);
    if h > 0 && then > MAX_GROUP_DEPTH && then > now {
        return Verdict::Refused(Refusal::TooDeep);
    }
    Verdict::Moves
}

/// Moves the row at `from` to the place `dest` (see the module doc), if [`judge`] allows it, and
/// returns where the row is now. Groups it leaves empty go away. All or nothing: `rs` is only
/// replaced by a finished result.
pub fn move_node(rs: &mut RuleSet, from: &[usize], dest: &[usize]) -> Option<Vec<usize>> {
    if judge(rs, from, dest) != Verdict::Moves {
        return None;
    }
    let mut next = rs.clone();
    let node = remove_at(&mut next, from)?;
    let mut dest = dest.to_vec();
    if !shift_after_removal(&mut dest, from) {
        return None;
    }
    let mut chain = from.get(..from.len().checked_sub(1)?)?.to_vec();
    // (a group the row is going into stays, however empty it is for now)
    while !(chain.is_empty() || dest.len() > chain.len() && dest.starts_with(&chain)) && list_at(&next, &chain).is_some_and(|l| l.rules.is_empty()) {
        remove_at(&mut next, &chain)?;
        if !shift_after_removal(&mut dest, &chain) {
            return None;
        }
        chain.pop();
    }
    insert_at(&mut next, &dest, node)?;
    *rs = next;
    Some(dest)
}

/// The place an arrow key sends the row at `from` to, if there is one (the move may still be
/// refused: [`judge`]).
pub fn key_dest(rs: &RuleSet, from: &[usize], dir: Dir) -> Option<Vec<usize>> {
    let (&i, parent) = from.split_last()?;
    let list = list_at(rs, parent)?;
    // a row that isn't there has no place to go (and `i` is then below `len`, so `i + 1` can't overflow)
    list.rules.get(i)?;
    let at = |n: usize| parent.iter().copied().chain([n]).collect::<Vec<usize>>();
    match dir {
        Dir::Up if i > 0 => Some(at(i - 1)),
        // the first of a group: out, to just before the group
        Dir::Up => (!parent.is_empty()).then(|| parent.to_vec()),
        Dir::Down if i + 1 < list.rules.len() => Some(at(i + 2)),
        // the last of a group, and ←: out, to just after the group
        Dir::Down | Dir::Out => {
            let (&g, outer) = parent.split_last()?;
            Some(outer.iter().copied().chain([g + 1]).collect())
        }
        Dir::In => {
            let above = i.checked_sub(1).and_then(|a| match list.rules.get(a)? {
                Rule::Group { group } => Some(at(a).into_iter().chain([group.rules.len()]).collect::<Vec<usize>>()),
                Rule::Field { .. } => None,
            });
            above.or_else(|| match list.rules.get(i + 1)? {
                Rule::Group { .. } => Some(at(i + 1).into_iter().chain([0]).collect()),
                Rule::Field { .. } => None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn rs(v: Value) -> RuleSet {
        serde_json::from_value(v).unwrap()
    }

    /// Each rule is named by its rating, each group by `g` and what is in it: `1 2 (3 4) 5`.
    fn shape(rs: &RuleSet) -> String {
        let parts: Vec<String> = rs
            .rules
            .iter()
            .map(|r| match r {
                Rule::Field { value, .. } => value.to_string(),
                Rule::Group { group } => format!("({})", shape(group)),
            })
            .collect();
        parts.join(" ")
    }

    fn rule(n: u32) -> Value {
        json!({"field": "rating", "op": "is", "value": n})
    }

    fn group(rules: Vec<Value>) -> Value {
        json!({"group": {"match": "any", "rules": rules}})
    }

    fn set(rules: Vec<Value>) -> RuleSet {
        rs(json!({"rules": rules}))
    }

    fn moved(rules: Vec<Value>, from: &[usize], dest: &[usize]) -> (String, Option<Vec<usize>>) {
        let mut r = set(rules);
        let at = move_node(&mut r, from, dest);
        (shape(&r), at)
    }

    #[test]
    fn a_rule_moves_down_and_up_in_its_list() {
        let list = || vec![rule(1), rule(2), rule(3), rule(4)];
        assert_eq!(moved(list(), &[0], &[3]), ("2 3 1 4".into(), Some(vec![2])), "before the fourth");
        assert_eq!(moved(list(), &[0], &[4]), ("2 3 4 1".into(), Some(vec![3])), "to the end");
        assert_eq!(moved(list(), &[3], &[0]), ("4 1 2 3".into(), Some(vec![0])), "to the top");
        assert_eq!(moved(list(), &[2], &[1]), ("1 3 2 4".into(), Some(vec![1])));
    }

    #[test]
    fn a_move_to_where_the_row_already_is_changes_nothing() {
        let list = || vec![rule(1), rule(2), rule(3)];
        for dest in [[1], [2]] {
            assert_eq!(judge(&set(list()), &[1], &dest), Verdict::Nothing);
            assert_eq!(moved(list(), &[1], &dest), ("1 2 3".into(), None));
        }
    }

    #[test]
    fn a_rule_moves_into_and_out_of_a_group() {
        let list = || vec![rule(1), group(vec![rule(2), rule(3)]), rule(4)];
        assert_eq!(moved(list(), &[0], &[1, 1]), ("(2 1 3) 4".into(), Some(vec![0, 1])), "into the group, between its rules");
        assert_eq!(moved(list(), &[2], &[1, 2]), ("1 (2 3 4)".into(), Some(vec![1, 2])), "into its end");
        assert_eq!(moved(list(), &[1, 0], &[0]), ("2 1 (3) 4".into(), Some(vec![0])), "out, to the top");
        assert_eq!(moved(list(), &[1, 1], &[3]), ("1 (2) 4 3".into(), Some(vec![3])), "out, to the end");
    }

    #[test]
    fn a_group_left_empty_goes_away() {
        let list = || vec![rule(1), group(vec![rule(2)]), rule(3)];
        assert_eq!(moved(list(), &[1, 0], &[0]), ("2 1 3".into(), Some(vec![0])), "the group's only rule goes before it");
        assert_eq!(moved(list(), &[1, 0], &[3]), ("1 3 2".into(), Some(vec![2])), "after everything");
        assert_eq!(moved(list(), &[1, 0], &[1]), ("1 2 3".into(), Some(vec![1])), "to the group's own place");
        // the emptied group is inside another: both go
        let deep = vec![rule(1), group(vec![group(vec![rule(2)])]), rule(3)];
        assert_eq!(moved(deep, &[1, 0, 0], &[0]), ("2 1 3".into(), Some(vec![0])));
        // into a group, and the old group is gone: the places after it shift
        let two = vec![group(vec![rule(1)]), group(vec![rule(2)])];
        assert_eq!(moved(two, &[0, 0], &[1, 1]), ("(2 1)".into(), Some(vec![0, 1])));
    }

    #[test]
    fn a_group_moves_whole_and_never_into_itself() {
        let list = || vec![group(vec![rule(1), rule(2)]), rule(3), group(vec![rule(4)])];
        assert_eq!(moved(list(), &[0], &[2]), ("3 (1 2) (4)".into(), Some(vec![1])));
        assert_eq!(moved(list(), &[0], &[3]), ("3 (4) (1 2)".into(), Some(vec![2])), "to the end");
        assert_eq!(moved(list(), &[0], &[2, 1]), ("3 (4 (1 2))".into(), Some(vec![1, 1])), "into the other group");
        for dest in [vec![0, 0], vec![0, 1], vec![0, 2]] {
            assert_eq!(judge(&set(list()), &[0], &dest), Verdict::Refused(Refusal::IntoItself), "{dest:?}");
            assert_eq!(moved(list(), &[0], &dest), ("(1 2) 3 (4)".into(), None));
        }
        let nested = || vec![group(vec![rule(1), group(vec![rule(2)])])];
        assert_eq!(judge(&set(nested()), &[0], &[0, 1, 0]), Verdict::Refused(Refusal::IntoItself), "inside what is inside it");
    }

    #[test]
    fn groups_stay_within_the_depth_the_editor_offers() {
        // a group holding a group holding a group: three lists below the top one
        let tall = group(vec![group(vec![group(vec![rule(1)])])]);
        let list = || vec![tall.clone(), group(vec![rule(2)])];
        // into the second group it would be four deep
        assert_eq!(judge(&set(list()), &[0], &[1, 0]), Verdict::Refused(Refusal::TooDeep));
        assert_eq!(moved(list(), &[0], &[1, 0]).1, None);
        // but beside it is fine, and so is a rule or a small group at any depth
        assert_eq!(judge(&set(list()), &[0], &[2]), Verdict::Moves);
        let flat = vec![rule(1), group(vec![group(vec![group(vec![rule(2)])])])];
        assert_eq!(judge(&set(flat.clone()), &[0], &[1, 0, 0, 0]), Verdict::Moves, "a rule can go into the deepest list");
        let small = vec![group(vec![rule(1)]), group(vec![group(vec![group(vec![rule(2)])])])];
        assert_eq!(judge(&set(small.clone()), &[0], &[1, 0, 0, 0]), Verdict::Refused(Refusal::TooDeep), "a group can't");
        assert_eq!(judge(&set(small), &[0], &[1, 0, 0]), Verdict::Moves, "one level up it can");
    }

    #[test]
    fn a_group_that_is_already_too_deep_can_still_be_reordered() {
        let too_deep = group(vec![group(vec![group(vec![group(vec![rule(1)]), group(vec![rule(2)])])])]);
        let r = set(vec![too_deep]);
        // the two innermost groups swap places: same depth, allowed
        assert_eq!(judge(&r, &[0, 0, 0, 0], &[0, 0, 0, 2]), Verdict::Moves);
        // moving one of them deeper is not
        assert_eq!(judge(&r, &[0, 0, 0, 0], &[0, 0, 0, 1, 0]), Verdict::Refused(Refusal::TooDeep));
    }

    #[test]
    fn paths_that_lead_nowhere_change_nothing() {
        let r = || set(vec![rule(1), group(vec![rule(2)])]);
        for (from, dest) in [
            (vec![], vec![0]),
            (vec![0], vec![]),
            (vec![5], vec![0]),
            (vec![0], vec![9]),
            (vec![0], vec![0, 0]),
            (vec![0], vec![5, 0]),
            (vec![1, 4], vec![0]),
            (vec![usize::MAX], vec![0]),
            (vec![0], vec![usize::MAX]),
            (vec![1], vec![1, usize::MAX]),
        ] {
            let mut x = r();
            assert_eq!(move_node(&mut x, &from, &dest), None, "{from:?} -> {dest:?}");
            assert_eq!(x, r());
        }
    }

    /// Given a path whose last step is past the end of its list (`usize::MAX` too), when an arrow
    /// key asks where the row goes, then there is no place, and no index arithmetic overflows.
    #[test]
    fn arrow_keys_on_a_path_that_leads_nowhere_find_no_place() {
        let r = set(vec![rule(1), group(vec![rule(2)])]);
        for from in [vec![2], vec![5], vec![usize::MAX], vec![1, 1], vec![1, usize::MAX]] {
            for dir in [Dir::Up, Dir::Down, Dir::Out, Dir::In] {
                assert_eq!(key_dest(&r, &from, dir), None, "{from:?} {dir:?}");
            }
        }
    }

    #[test]
    fn arrow_keys_move_a_row_one_place() {
        let list = || vec![rule(1), group(vec![rule(2), rule(3)]), rule(4)];
        let key = |from: &[usize], dir| {
            let mut r = set(list());
            let at = key_dest(&r, from, dir).and_then(|d| move_node(&mut r, from, &d));
            (shape(&r), at)
        };
        assert_eq!(key(&[0], Dir::Down), ("(2 3) 1 4".into(), Some(vec![1])), "a group is passed, not entered");
        assert_eq!(key(&[2], Dir::Up), ("1 4 (2 3)".into(), Some(vec![1])));
        assert_eq!(key(&[0], Dir::Up), ("1 (2 3) 4".into(), None), "the top stays");
        assert_eq!(key(&[2], Dir::Down), ("1 (2 3) 4".into(), None), "the end stays");
        // at a group's ends, ↑ ↓ carry the row out of it
        assert_eq!(key(&[1, 0], Dir::Up), ("1 2 (3) 4".into(), Some(vec![1])));
        assert_eq!(key(&[1, 1], Dir::Down), ("1 (2) 3 4".into(), Some(vec![2])));
        assert_eq!(key(&[1, 0], Dir::Down), ("1 (3 2) 4".into(), Some(vec![1, 1])), "inside, one place down");
        // ← : out, just after the group; at the top level nothing
        assert_eq!(key(&[1, 0], Dir::Out), ("1 (3) 2 4".into(), Some(vec![2])));
        assert_eq!(key(&[0], Dir::Out), ("1 (2 3) 4".into(), None));
        // → : into the group above (its end) or below (its start)
        assert_eq!(key(&[2], Dir::In), ("1 (2 3 4)".into(), Some(vec![1, 2])));
        assert_eq!(key(&[0], Dir::In), ("(1 2 3) 4".into(), Some(vec![0, 0])));
        let lone = || vec![rule(1), rule(2)];
        let mut r = set(lone());
        assert_eq!(key_dest(&r, &[0], Dir::In), None, "no group beside it");
        assert_eq!(move_node(&mut r, &[0], &[0]), None);
    }

    #[test]
    fn a_rows_only_rule_going_out_takes_its_group_with_it() {
        let list = || vec![group(vec![rule(1)]), rule(2)];
        let mut r = set(list());
        let at = key_dest(&r, &[0, 0], Dir::Down).and_then(|d| move_node(&mut r, &[0, 0], &d));
        assert_eq!((shape(&r), at), ("1 2".into(), Some(vec![0])), "the group would be empty");
        let mut r = set(list());
        let at = key_dest(&r, &[0, 0], Dir::Out).and_then(|d| move_node(&mut r, &[0, 0], &d));
        assert_eq!((shape(&r), at), ("1 2".into(), Some(vec![0])), "← too: just after a group that is gone");
    }

    #[test]
    fn a_row_leaving_nested_groups_may_leave_the_group_it_is_going_to_empty() {
        let mut r = set(vec![group(vec![group(vec![rule(1)])])]);
        assert_eq!(move_node(&mut r, &[0, 0, 0], &[0, 0]), Some(vec![0, 0]));
        assert_eq!(shape(&r), "(1)");
        let mut r = set(vec![group(vec![group(vec![rule(1)])])]);
        let dest = key_dest(&r, &[0, 0, 0], Dir::Out).unwrap();
        assert!(move_node(&mut r, &[0, 0, 0], &dest).is_some());
        assert_eq!(shape(&r), "(1)");
    }

    #[test]
    fn arrow_keys_respect_the_depth_limit() {
        let tall = group(vec![group(vec![group(vec![rule(1)])])]);
        let r = set(vec![tall.clone(), group(vec![rule(2)])]);
        // → would put it in the group below: four deep
        let dest = key_dest(&r, &[0], Dir::In).unwrap();
        assert_eq!(judge(&r, &[0], &dest), Verdict::Refused(Refusal::TooDeep));
    }
}
