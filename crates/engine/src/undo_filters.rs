//! What a step did to the person's filter, for undo and redo to carry back and forth.
//!
//! Deleting a keyword, renaming it or a person, removing a library folder: each changes the filter that names it, in use
//! or put aside by Enable Filters. The filter is session state, not catalog state, so the step's inverse op cannot
//! restore it. `Session::run_command` therefore compares the person's filter before and after every
//! command that committed a step, and the step's undo entry keeps the difference as a [`FilterChange`]. No command
//! records anything itself, so a command that changes the filter next needs nothing here.
//!
//! Undo moves the filter from what the step left to what it found, and the entry it hands to redo does the reverse.
//! Only what the step changed is carried, group by group (the fields one filter chip clears together, see
//! [`filter_chips`]), and a group only moves while it still holds what the step left: a keyword or folder chosen
//! since is not replaced, and the rest of the filter is never touched. What the source shown now owns is left out
//! ([`crate::LibrarySource::reconcile`]), as for every other way into the filter. The entry undo hands to redo (and
//! redo's to undo) leaves alone a group that was left alone because the person had chosen a value of their own since,
//! for as long as the group still holds that value: redo does not overwrite it either, though it may now hold what the
//! step found. It is the person's filter, whatever it matches. Once they change it, the group moves whenever the filter
//! holds what the step left.
//!
//! A filter the person built after the step is left alone, and the one the step put aside is not brought back beside
//! it (setting a filter while filters are off forgets the one put aside, `library.toggleFilters`): nothing is carried
//! once the filter was thrown away since (replaced, emptied, or the one put aside forgotten by setting another: the
//! filter's `replaced` count), nor into a filter the person has again after the step dropped the put-aside one.
//!
//! A filter the step itself emptied is not one the person threw away: what they add to it afterwards is an edit of
//! it, so undo brings the keyword back beside it. One the person emptied (by hand, with Clear Filters, or by showing
//! a source that owns its last criterion) ended there (it is counted), and the filter after it is a new one. The same
//! holds for the one Enable Filters brings back when the source shown owns its last criterion: it is counted once the
//! person sets another filter (until then they have none, so undo can still bring a folder or keyword back into it,
//! and what they set after that is an edit of it, or put the filter a step dropped aside for them).

use lightcraft_catalog::{Catalog, Filter};
use serde_json::{Map, Value};

use crate::filter_state::{Carried, Snapshot};
use crate::{Session, filter_chips};

/// Moving the person's filter from the state a step left it in (`from`) to the one it found (`to`).
#[derive(Clone, Debug)]
pub(crate) struct FilterChange {
    from: Snapshot,
    to: Snapshot,
    /// [`crate::filter_state::FilterState`]'s `replaced` count after the step.
    replaced: u64,
    /// The fields of groups the carry that made this change left alone, because the person had chosen a value of their own
    /// (see [`carry`]), with that value: this change leaves such a group alone once too, while it still holds it, though
    /// it may hold what this change moves from.
    skip: Map<String, Value>,
}

/// The fields of a filter by their names in commands (a few at their default are left out).
fn fields(f: &Filter) -> Map<String, Value> {
    match serde_json::to_value(f) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

/// Fields that change together: those one chip of either filter clears (`label` and `labels`, `only` and `onlyNone`…),
/// and every other field of either on its own.
fn groups(from: &Map<String, Value>, to: &Map<String, Value>, chips: Vec<Vec<String>>) -> Vec<Vec<String>> {
    let mut groups = chips;
    for key in from.keys().chain(to.keys()) {
        if !groups.iter().flatten().any(|k| k == key) {
            groups.push(vec![key.clone()]);
        }
    }
    groups
}

/// Move the groups of `filter` that the step changed (`from` → `to`) and that still hold what the step left, except the
/// ones `skip` names while they still hold the value it gives. Returns the fields of the groups left alone because the
/// person had set them to a value of their own since (not empty, and not what the step left), with that value: the step's
/// reverse leaves them alone once too.
pub(crate) fn carry(filter: &mut Filter, from: &Filter, to: &Filter, cat: &Catalog, skip: &Map<String, Value>) -> Map<String, Value> {
    let (mut now, left, found) = (fields(filter), fields(from), fields(to));
    // (the groups are judged against the filter as it was: a chip of both filters comes up twice)
    let held = now.clone();
    let value = |m: &Map<String, Value>, k: &String| m.get(k).cloned().unwrap_or(Value::Null);
    let mut chosen = Map::new();
    let chips = filter_chips(from, cat).into_iter().chain(filter_chips(to, cat));
    let chips = chips.filter_map(|c| c.clear.as_object().map(|o| o.keys().cloned().collect())).collect();
    for group in groups(&left, &found, chips) {
        if group.iter().all(|k| left.get(k) == found.get(k)) || group.iter().all(|k| skip.get(k) == Some(&value(&held, k))) {
            continue;
        }
        if group.iter().all(|k| held.get(k) == left.get(k)) {
            for k in &group {
                match found.get(k) {
                    Some(v) => now.insert(k.clone(), v.clone()),
                    None => now.remove(k),
                };
            }
        } else if group.iter().any(|k| held.get(k) != left.get(k) && !matches!(held.get(k), None | Some(Value::Null))) {
            chosen.extend(group.iter().map(|k| (k.clone(), value(&held, k))));
        }
    }
    if let Ok(moved) = serde_json::from_value(Value::Object(now)) {
        *filter = moved;
    }
    chosen
}

impl Session {
    /// A command that changed the person's filter ran: if it also committed one step, keep what it did to the filter
    /// with that step, so undo can bring it back. `before` is the filter before its first change, `commits` is
    /// [`Session::commits`] before it ran: what shows how many steps it committed. A command that committed several
    /// has no single step the change belongs to (none does today), and one that failed after committing one still
    /// left that step to undo.
    pub(crate) fn note_filter_change(&mut self, before: Snapshot, commits: u64) {
        if self.commits.wrapping_sub(commits) != 1 {
            return;
        }
        let after = self.filters.snapshot();
        if before == after {
            return;
        }
        let replaced = self.filters.replaced();
        if let Some(step) = self.undo.last_mut() {
            step.filters = Some(FilterChange { from: after, to: before, replaced, skip: Map::new() });
        }
    }

    /// Apply a step's filter change (undo or redo); returns the change that takes it back, for the other history stack.
    /// That change leaves alone once what this one left alone because the person had chosen it since (see [`carry`]).
    pub(crate) fn carry_filters(&mut self, change: Option<&FilterChange>) -> Option<FilterChange> {
        let change = change?;
        let mut skip = Map::new();
        if self.filters.replaced() == change.replaced {
            let (from, to) = (&change.from, &change.to);
            let cat = &self.catalog;
            self.filters.carry(self.source, |filter, aside| {
                // the step dropped the put-aside filter it emptied, and the person has a filter again: built since,
                // so left alone (the put-aside one is not brought back beside it)
                if !from.aside && to.aside && *filter != Filter::default() {
                    return Carried::InPlace;
                }
                skip = carry(filter, &from.filter, &to.filter, cat, &change.skip);
                // the step had dropped the put-aside filter after emptying it: drop it again once nothing is left in it
                if aside && from.aside && !to.aside && *filter == to.filter {
                    Carried::Drop
                // ... or it had dropped it and the person has no filter now: put it aside again
                } else if !aside && !from.aside && to.aside {
                    Carried::PutAside { even_if_empty: to.filter == Filter::default() }
                } else {
                    Carried::InPlace
                }
            });
        }
        Some(FilterChange { from: change.to.clone(), to: change.from.clone(), replaced: change.replaced, skip })
    }
}
