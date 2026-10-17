//! Stack mirroring (RT.C3): catalog stacks ⇄ Immich stacks for linked assets — the pure part.
//!
//! Immich stacks are flat sets with one primary asset; catalog stacks are ordered lists whose
//! first photo is the top. Mirrored: which linked assets are stacked together, and which one is on
//! top (= primary). The order below the top stays local. Unlinked catalog photos are invisible
//! here (they keep their place in the catalog stack).
//!
//! The merge is three-way against the groups both sides had at the last sync ([`reconcile`]), as
//! for the other synced fields:
//! - a group on one side only: added there when the base lacks it, removed there when the base
//!   had it (the other side removed or changed it);
//! - both sides changed overlapping groups differently: the catalog wins (the catalog is where
//!   stacks are edited; Immich's stack UI is coarser), and the conflict is reported.
//!
//! Sources: own design; Immich's stack model from its public API specification (v3.3.1 OpenAPI:
//! `POST /stacks` creates with the first asset as primary and merges stacks whose primary is
//! listed, `DELETE /stacks`, `PUT /stacks/{id}` sets the primary).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// One stack as far as mirroring sees it: the primary (catalog top) and every member (primary
/// included), as Immich asset ids.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Group {
    pub primary: String,
    pub members: BTreeSet<String>,
}

impl Group {
    pub fn new(primary: &str, members: impl IntoIterator<Item = String>) -> Group {
        let mut m: BTreeSet<String> = members.into_iter().collect();
        m.insert(primary.to_string());
        Group { primary: primary.to_string(), members: m }
    }
    /// A group mirrors only with two or more members.
    pub fn is_stack(&self) -> bool {
        self.members.len() >= 2
    }
}

/// What to change on each side.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Groups to create (or re-create with a new primary) in the catalog.
    pub to_local: Vec<Group>,
    /// Groups to dissolve in the catalog (their linked members leave their stack).
    pub remove_local: Vec<Group>,
    /// Groups to create on Immich (overlapping remote stacks are deleted first).
    pub to_remote: Vec<Group>,
    /// Groups to delete on Immich.
    pub remove_remote: Vec<Group>,
    /// Groups both sides changed differently (the catalog's version won).
    pub conflicts: Vec<(Group, Group)>,
}

impl Plan {
    pub fn is_noop(&self) -> bool {
        self.to_local.is_empty() && self.remove_local.is_empty() && self.to_remote.is_empty() && self.remove_remote.is_empty()
    }
}

fn overlaps(a: &Group, b: &Group) -> bool {
    a.members.intersection(&b.members).next().is_some()
}

/// Normalise a side: groups of two or more, each asset in one group only (the first wins).
fn clean(groups: &[Group]) -> BTreeSet<Group> {
    let mut seen = BTreeSet::new();
    let mut out = BTreeSet::new();
    for g in groups.iter().filter(|g| g.is_stack() && g.members.contains(&g.primary)) {
        if g.members.iter().any(|m| seen.contains(m)) {
            continue;
        }
        seen.extend(g.members.iter().cloned());
        out.insert(g.clone());
    }
    out
}

/// The three-way merge (see the module docs). `base` = the groups after the last sync.
pub fn reconcile(local: &[Group], remote: &[Group], base: &[Group]) -> Plan {
    let (l, r, b) = (clean(local), clean(remote), clean(base));
    let mut plan = Plan::default();
    let local_new: Vec<&Group> = l.iter().filter(|g| !r.contains(*g) && !b.contains(*g)).collect();
    let remote_new: Vec<&Group> = r.iter().filter(|g| !l.contains(*g) && !b.contains(*g)).collect();
    for g in &local_new {
        plan.to_remote.push((*g).clone());
    }
    for g in &remote_new {
        match local_new.iter().find(|x| overlaps(x, g)) {
            Some(x) => plan.conflicts.push(((*x).clone(), (*g).clone())),
            None => plan.to_local.push((*g).clone()),
        }
    }
    // in the base and on one side only: the other side removed (or changed) it
    for g in l.iter().filter(|g| b.contains(*g) && !r.contains(*g)) {
        // unless the remote change lost a conflict: then the catalog's group stays
        if !plan.conflicts.iter().any(|(_, rg)| overlaps(rg, g)) {
            plan.remove_local.push(g.clone());
        }
    }
    for g in r.iter().filter(|g| b.contains(*g) && !l.contains(*g)) {
        plan.remove_remote.push(g.clone());
    }
    // a remote group that lost a conflict goes too (the catalog's version replaces it)
    for (_, rg) in &plan.conflicts {
        if !plan.remove_remote.contains(rg) {
            plan.remove_remote.push(rg.clone());
        }
    }
    plan
}

/// The groups both sides hold after `plan` is applied (the next base).
pub fn next_base(local: &[Group], remote: &[Group], plan: &Plan) -> Vec<Group> {
    let mut r: BTreeSet<Group> = clean(remote);
    for g in &plan.remove_remote {
        r.remove(g);
    }
    for g in &plan.to_remote {
        r.retain(|x| !overlaps(x, g));
        r.insert(g.clone());
    }
    let mut l: BTreeSet<Group> = clean(local);
    for g in &plan.remove_local {
        l.remove(g);
    }
    for g in &plan.to_local {
        l.retain(|x| !overlaps(x, g));
        l.insert(g.clone());
    }
    l.intersection(&r).cloned().collect()
}

/// Build groups from catalog stacks: each stack's photos in order (top first), mapped to asset
/// ids by `asset_of` (unlinked photos are skipped; the top linked photo is the primary).
pub fn local_groups<P: Copy>(stacks: &[Vec<P>], asset_of: impl Fn(P) -> Option<String>) -> Vec<Group> {
    stacks
        .iter()
        .filter_map(|photos| {
            let ids: Vec<String> = photos.iter().filter_map(|p| asset_of(*p)).collect();
            let primary = ids.first()?.clone();
            Some(Group::new(&primary, ids)).filter(Group::is_stack)
        })
        .collect()
}

/// Immich's stacks (`GET /stacks`) as groups, keeping only assets in `linked`.
pub fn remote_groups(stacks: &[crate::types::StackInfo], linked: &BTreeSet<String>) -> Vec<Group> {
    stacks
        .iter()
        .filter_map(|s| {
            let members: BTreeSet<String> = s.assets.iter().map(|a| a.id.clone()).filter(|a| linked.contains(a)).collect();
            if !members.contains(&s.primary_asset_id) {
                // the primary isn't linked: the linked rest still counts as stacked, under its first
                let first = members.iter().next()?.clone();
                return Some(Group::new(&first, members)).filter(Group::is_stack);
            }
            Some(Group { primary: s.primary_asset_id.clone(), members }).filter(Group::is_stack)
        })
        .collect()
}

/// Asset id → the Immich stack holding it.
pub fn stack_index(stacks: &[crate::types::StackInfo]) -> BTreeMap<String, String> {
    stacks.iter().flat_map(|s| s.assets.iter().map(move |a| (a.id.clone(), s.id.clone()))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(primary: &str, rest: &[&str]) -> Group {
        Group::new(primary, rest.iter().map(|s| s.to_string()))
    }

    #[test]
    fn one_sided_changes_flow_to_the_other_side() {
        let base = vec![g("a", &["b"])];
        // nothing changed
        assert!(reconcile(&base, &base, &base).is_noop());
        // catalog added a stack
        let p = reconcile(&[g("a", &["b"]), g("x", &["y"])], &base, &base);
        assert_eq!(p.to_remote, vec![g("x", &["y"])]);
        assert!(p.to_local.is_empty() && p.remove_remote.is_empty() && p.remove_local.is_empty());
        // Immich changed the primary: the catalog follows (old group removed, new one created)
        let p = reconcile(&base, &[g("b", &["a"])], &base);
        assert_eq!((p.to_local.clone(), p.remove_local.clone()), (vec![g("b", &["a"])], vec![g("a", &["b"])]));
        assert!(p.to_remote.is_empty() && p.remove_remote.is_empty());
        // the catalog dissolved it: Immich follows
        let p = reconcile(&[], &base, &base);
        assert_eq!(p.remove_remote, vec![g("a", &["b"])]);
        // Immich dissolved it: the catalog follows
        let p = reconcile(&base, &[], &base);
        assert_eq!(p.remove_local, vec![g("a", &["b"])]);
        // first sync (no base): both sides' stacks are kept and mirrored
        let p = reconcile(&[g("a", &["b"])], &[g("x", &["y"])], &[]);
        assert_eq!((p.to_remote.clone(), p.to_local.clone()), (vec![g("a", &["b"])], vec![g("x", &["y"])]));
        assert_eq!(next_base(&[g("a", &["b"])], &[g("x", &["y"])], &p).len(), 2);
    }

    #[test]
    fn conflicting_changes_keep_the_catalog_version() {
        let base = vec![g("a", &["b"])];
        // catalog added c to the stack, Immich made b primary
        let p = reconcile(&[g("a", &["b", "c"])], &[g("b", &["a"])], &base);
        assert_eq!(p.to_remote, vec![g("a", &["b", "c"])]);
        assert!(p.to_local.is_empty());
        assert!(p.remove_local.is_empty(), "the catalog's group stays");
        assert_eq!(p.conflicts.len(), 1);
        assert_eq!(p.remove_remote, vec![g("b", &["a"])]);
        let nb = next_base(&[g("a", &["b", "c"])], &[g("b", &["a"])], &p);
        assert_eq!(nb, vec![g("a", &["b", "c"])]);
    }

    #[test]
    fn groups_from_both_sides() {
        let stacks = vec![vec![1u64, 2, 3], vec![4, 5], vec![6]];
        let linked = |p: u64| (p != 1 && p != 5).then(|| format!("as{p}"));
        // 1 unlinked: 2 is the primary; 4–5 has one linked member: no group
        assert_eq!(local_groups(&stacks, linked), vec![g("as2", &["as3"])]);
        let st = crate::types::StackInfo {
            id: "s".into(),
            primary_asset_id: "p".into(),
            assets: ["p", "q", "r"].iter().map(|i| crate::types::StackAsset { id: i.to_string() }).collect(),
        };
        let all: BTreeSet<String> = ["p", "q", "r"].iter().map(|s| s.to_string()).collect();
        assert_eq!(remote_groups(std::slice::from_ref(&st), &all), vec![g("p", &["q", "r"])]);
        let some: BTreeSet<String> = ["q", "r"].iter().map(|s| s.to_string()).collect();
        assert_eq!(remote_groups(std::slice::from_ref(&st), &some), vec![g("q", &["r"])]);
        assert_eq!(stack_index(&[st]).get("q").map(String::as_str), Some("s"));
    }

    #[test]
    fn hostile_groups_never_panic() {
        let weird = vec![g("a", &[]), Group { primary: "z".into(), members: ["a".into(), "b".into()].into() }, g("a", &["b"]), g("b", &["c"])];
        let p = reconcile(&weird, &weird, &[]);
        let _ = next_base(&weird, &weird, &p);
        assert!(p.to_remote.iter().all(Group::is_stack));
    }
}
