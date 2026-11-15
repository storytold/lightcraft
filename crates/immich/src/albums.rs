//! IMM-SYNC, collections ↔ albums: the pure plan (no I/O) of one album sync run. The engine
//! gathers both sides, calls [`plan`], applies the Immich half on a worker and the catalog half on
//! the session (`immich.syncAlbums`).
//!
//! Sources: own design (the set three-way merge of the metadata sync, applied to album membership
//! and to album names; deletions follow the conflict policy).
//!
//! What it covers, gated by the account's album rule ([`SyncConfig::albums`](crate::sync::SyncConfig)):
//! - direction `off`: only albums linked by hand (`immich.linkAlbum`) sync, membership both ways;
//!   nothing is created, renamed or deleted.
//! - `toImmich` / `fromImmich` / `twoWay`: every plain catalog album maps to the Immich album of
//!   the same name; the side(s) the direction allows get creates, renames, membership changes and
//!   album deletes. A deleted album never deletes photos: only the album on the other side, and
//!   only as the policy says (`ask`, or a side that also changed it since the last sync, leaves it
//!   for the user).
//!
//! Two rules keep a failed or partial run from costing photos or collections:
//! - a link's `base` only ever holds assets known to be in the Immich album *and* in the catalog
//!   album. [`pair`] writes the base it expects after its ops; the engine then takes out what an
//!   op did not achieve ([`AlbumLink::not_added`], [`AlbumLink::not_removed`]). An asset wrongly
//!   left in the base would read as "was in both, now missing on one side" on the next run and be
//!   removed from the other.
//! - a catalog album that holds photos Immich never had (unlinked ones) is never deleted because
//!   its Immich album was: it is kept and taken out of the sync ([`AlbumLink::local_only`],
//!   [`Plan::kept`]).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::sync::{Direction, FieldRule, Policy};

/// A catalog album linked to an Immich album.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AlbumLink {
    pub immich: String,
    /// Asset ids both sides had after the last sync.
    pub base: Option<BTreeSet<String>>,
    /// The album name both sides had after the last sync.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl AlbumLink {
    /// The marker for a catalog album that is kept out of the album sync: its Immich album was
    /// deleted while it held photos that exist only in the catalog. No Immich album is created or
    /// matched for it again until it is linked by hand (`immich.linkAlbum`).
    pub fn local_only(name: &str) -> AlbumLink {
        AlbumLink { immich: String::new(), base: None, name: Some(name.to_string()) }
    }

    pub fn is_local_only(&self) -> bool {
        self.immich.is_empty()
    }

    /// `assets` did not reach the other side's album (an add that failed or was refused): they are
    /// not common to both sides, so the next run adds them again.
    pub fn not_added<'a>(&mut self, assets: impl IntoIterator<Item = &'a String>) {
        if let Some(b) = self.base.as_mut() {
            for a in assets {
                b.remove(a);
            }
        }
    }

    /// `assets` are still in the other side's album (a remove that failed): they stay in the base,
    /// so the next run removes them again instead of reading them as newly added there.
    pub fn not_removed<'a>(&mut self, assets: impl IntoIterator<Item = &'a String>) {
        if let Some(b) = self.base.as_mut() {
            b.extend(assets.into_iter().cloned());
        }
    }
}

/// Catalog album id → its link.
pub type AlbumLinks = BTreeMap<u64, AlbumLink>;

/// A plain catalog album: its linked photos as Immich asset ids.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocalAlbum {
    pub id: u64,
    pub name: String,
    pub assets: BTreeSet<String>,
    /// How many of the album's photos have no (confirmed) link to an Immich asset: photos only
    /// the catalog has.
    pub unlinked: usize,
}

/// An Immich album: its assets that are linked to catalog photos.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RemoteAlbum {
    pub id: String,
    pub name: String,
    pub assets: BTreeSet<String>,
}

/// Where an album was deleted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeletedOn {
    Catalog,
    Immich,
}

/// An album deleted on one side, waiting for the user (policy `ask`, or changed on the other side
/// since the last sync).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingDelete {
    pub album: u64,
    pub immich: String,
    pub name: String,
    pub deleted_on: DeletedOn,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum RemoteOp {
    /// Create an Immich album for catalog album `local`, holding `assets`.
    Create {
        local: u64,
        name: String,
        assets: BTreeSet<String>,
    },
    Rename {
        id: String,
        name: String,
    },
    Add {
        id: String,
        assets: Vec<String>,
    },
    Remove {
        id: String,
        assets: Vec<String>,
    },
    /// Delete the album (never its assets).
    Delete {
        id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum LocalOp {
    /// Create a catalog album for Immich album `immich`, holding the photos of `assets`.
    Create {
        immich: String,
        name: String,
        assets: BTreeSet<String>,
    },
    Rename {
        id: u64,
        name: String,
    },
    Add {
        id: u64,
        assets: Vec<String>,
    },
    Remove {
        id: u64,
        assets: Vec<String>,
    },
    /// Delete the album (never its photos).
    Delete {
        id: u64,
    },
}

/// One run's plan.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub remote: Vec<RemoteOp>,
    pub local: Vec<LocalOp>,
    /// The links after the run (albums created by the run are linked when created).
    pub links: AlbumLinks,
    /// Deletions waiting for the user, old and new.
    pub pending: Vec<PendingDelete>,
    /// Albums renamed differently on both sides (left alone; the policy said neither wins).
    pub conflicts: Vec<String>,
    /// Catalog albums whose Immich album was deleted and that the policy would have deleted too,
    /// but which hold photos only the catalog has: kept, and no longer synced.
    pub kept: Vec<KeptAlbum>,
}

/// A catalog album kept although its Immich album was deleted ([`Plan::kept`]).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeptAlbum {
    pub album: u64,
    pub name: String,
    /// The photos in it that Immich never had.
    pub unlinked: usize,
}

fn push_allowed(d: Direction) -> bool {
    matches!(d, Direction::TwoWay | Direction::ToImmich | Direction::Off)
}

fn pull_allowed(d: Direction) -> bool {
    matches!(d, Direction::TwoWay | Direction::FromImmich | Direction::Off)
}

/// Plan a run. `pending`: the deletions still waiting (their links are left alone).
pub fn plan(rule: FieldRule, local: &[LocalAlbum], remote: &[RemoteAlbum], links: &AlbumLinks, pending: &[PendingDelete]) -> Plan {
    let auto = rule.direction != Direction::Off;
    let (push, pull) = (push_allowed(rule.direction), pull_allowed(rule.direction));
    let by_local: BTreeMap<u64, &LocalAlbum> = local.iter().map(|a| (a.id, a)).collect();
    let by_remote: BTreeMap<&str, &RemoteAlbum> = remote.iter().map(|a| (a.id.as_str(), a)).collect();
    let waiting: BTreeSet<u64> = pending.iter().map(|p| p.album).collect();
    let mut out = Plan { pending: pending.to_vec(), ..Default::default() };
    let mut used_remote: BTreeSet<String> = links.values().map(|l| l.immich.clone()).collect();
    let mut used_local: BTreeSet<u64> = links.keys().copied().collect();

    for (id, link) in links {
        if waiting.contains(id) || link.is_local_only() {
            // (a local-only album that is gone from the catalog needs no marker any more)
            if waiting.contains(id) || by_local.contains_key(id) {
                out.links.insert(*id, link.clone());
            }
            continue;
        }
        let (l, r) = (by_local.get(id), by_remote.get(link.immich.as_str()));
        match (l, r) {
            (Some(l), Some(r)) => {
                let new = pair(rule, l, r, link, &mut out);
                out.links.insert(*id, new);
            }
            (None, None) => {}
            _ if !auto => {
                // hand-made links: a missing side just ends the link
            }
            (None, Some(r)) => {
                if deleted(rule, *id, link, r.name.clone(), changed_remote(link, r), DeletedOn::Catalog, push, &mut out) {
                    used_remote.remove(&r.id);
                }
            }
            (Some(l), None) => {
                if pull && l.unlinked > 0 && deleter_wins(rule.policy, DeletedOn::Immich, changed_local(link, l)) == Some(true) {
                    // it holds photos Immich never had: deleting it would lose the only place
                    // they are collected. Kept, out of the sync, and said so
                    out.links.insert(*id, AlbumLink::local_only(&l.name));
                    out.kept.push(KeptAlbum { album: *id, name: l.name.clone(), unlinked: l.unlinked });
                } else if deleted(rule, *id, link, l.name.clone(), changed_local(link, l), DeletedOn::Immich, pull, &mut out) {
                    used_local.remove(&l.id);
                }
            }
        }
    }
    if !auto {
        return out;
    }
    // unlinked albums: same name → linked; otherwise created on the other side
    let mut free_remote: Vec<&RemoteAlbum> = remote.iter().filter(|r| !used_remote.contains(&r.id)).collect();
    for l in local.iter().filter(|l| !used_local.contains(&l.id)) {
        if let Some(pos) = free_remote.iter().position(|r| r.name == l.name) {
            let r = free_remote.remove(pos);
            let link = AlbumLink { immich: r.id.clone(), base: None, name: Some(l.name.clone()) };
            let new = pair(rule, l, r, &link, &mut out);
            out.links.insert(l.id, new);
        } else if push {
            out.remote.push(RemoteOp::Create { local: l.id, name: l.name.clone(), assets: l.assets.clone() });
        }
    }
    if pull {
        for r in free_remote {
            out.local.push(LocalOp::Create { immich: r.id.clone(), name: r.name.clone(), assets: r.assets.clone() });
        }
    }
    out
}

fn changed_remote(link: &AlbumLink, r: &RemoteAlbum) -> bool {
    link.name.as_ref().is_some_and(|n| *n != r.name) || link.base.as_ref().is_some_and(|b| *b != r.assets)
}

fn changed_local(link: &AlbumLink, l: &LocalAlbum) -> bool {
    link.name.as_ref().is_some_and(|n| *n != l.name) || link.base.as_ref().is_some_and(|b| *b != l.assets)
}

/// An album deleted on `on`, still present on the other side. `allowed`: the deletion may travel
/// to the other side. `other_changed`: the surviving album changed since the last sync.
/// → true when the link just ends: the survivor is then an unlinked album of this run (created
/// again on the deleting side when the direction allows).
fn deleted(rule: FieldRule, id: u64, link: &AlbumLink, name: String, other_changed: bool, on: DeletedOn, allowed: bool, out: &mut Plan) -> bool {
    if !allowed {
        return true;
    }
    // the survivor's side wins: it is created again on the deleting side
    let Some(deleter_wins) = deleter_wins(rule.policy, on, other_changed) else { return true };
    if deleter_wins {
        match on {
            DeletedOn::Catalog => out.remote.push(RemoteOp::Delete { id: link.immich.clone() }),
            DeletedOn::Immich => out.local.push(LocalOp::Delete { id }),
        }
    } else {
        out.links.insert(id, link.clone());
        out.pending.push(PendingDelete { album: id, immich: link.immich.clone(), name, deleted_on: on });
    }
    false
}

/// Does an album delete on `on` travel to the other side? `Some(true)`: yes; `Some(false)`: the
/// user decides; `None`: no, the surviving side wins.
fn deleter_wins(policy: Policy, on: DeletedOn, other_changed: bool) -> Option<bool> {
    match (policy, on) {
        (Policy::Ask, _) => Some(false),
        (Policy::Newest, _) => Some(!other_changed),
        (Policy::Catalog, DeletedOn::Catalog) | (Policy::Immich, DeletedOn::Immich) => Some(true),
        (Policy::Catalog, DeletedOn::Immich) | (Policy::Immich, DeletedOn::Catalog) => None,
    }
}

/// A linked pair present on both sides: renames and membership → the link after the run.
///
/// The link's base is what both sides hold once every op of this pair has worked; the caller
/// corrects it for ops that did not ([`AlbumLink::not_added`], [`AlbumLink::not_removed`]).
fn pair(rule: FieldRule, l: &LocalAlbum, r: &RemoteAlbum, link: &AlbumLink, out: &mut Plan) -> AlbumLink {
    let (push, pull) = (push_allowed(rule.direction), pull_allowed(rule.direction));
    let auto = rule.direction != Direction::Off;
    // names (hand-made links in `off` mode keep their own names)
    let mut name = link.name.clone();
    if auto {
        let base = link.name.clone().unwrap_or_else(|| l.name.clone());
        let (lc, rc) = (l.name != base, r.name != base);
        let to_remote = |out: &mut Plan| out.remote.push(RemoteOp::Rename { id: r.id.clone(), name: l.name.clone() });
        let to_local = |out: &mut Plan| out.local.push(LocalOp::Rename { id: l.id, name: r.name.clone() });
        match (lc, rc) {
            (true, false) if push => {
                to_remote(out);
                name = Some(l.name.clone());
            }
            (false, true) if pull => {
                to_local(out);
                name = Some(r.name.clone());
            }
            (true, true) if l.name == r.name => name = Some(l.name.clone()),
            (true, true) => match rule.policy {
                Policy::Catalog if push => {
                    to_remote(out);
                    name = Some(l.name.clone());
                }
                Policy::Immich if pull => {
                    to_local(out);
                    name = Some(r.name.clone());
                }
                _ => out.conflicts.push(format!("“{}” is “{}” in Immich", l.name, r.name)),
            },
            (false, false) => name = Some(base),
            _ => {}
        }
    }
    // membership
    let (lo, re) = (&l.assets, &r.assets);
    let (mut add_r, mut rm_r, mut add_l, mut rm_l): (Vec<String>, Vec<String>, Vec<String>, Vec<String>) = Default::default();
    match &link.base {
        None => {
            if push {
                add_r = lo.difference(re).cloned().collect();
            }
            if pull {
                add_l = re.difference(lo).cloned().collect();
            }
        }
        Some(b) => {
            if push {
                add_r = lo.difference(b).filter(|a| !re.contains(*a)).cloned().collect();
                rm_r = b.difference(lo).filter(|a| re.contains(*a)).cloned().collect();
            }
            if pull {
                add_l = re.difference(b).filter(|a| !lo.contains(*a)).cloned().collect();
                rm_l = b.difference(re).filter(|a| lo.contains(*a)).cloned().collect();
            }
        }
    }
    let new_remote: BTreeSet<String> = re.iter().filter(|a| !rm_r.contains(a)).cloned().chain(add_r.iter().cloned()).collect();
    let new_local: BTreeSet<String> = lo.iter().filter(|a| !rm_l.contains(a)).cloned().chain(add_l.iter().cloned()).collect();
    // one-way: the base is the side that was followed
    let base = match (push, pull) {
        (true, false) => new_local.clone(),
        (false, true) => new_remote.clone(),
        _ => new_remote.intersection(&new_local).cloned().collect(),
    };
    if !add_r.is_empty() {
        out.remote.push(RemoteOp::Add { id: r.id.clone(), assets: add_r });
    }
    if !rm_r.is_empty() {
        out.remote.push(RemoteOp::Remove { id: r.id.clone(), assets: rm_r });
    }
    if !add_l.is_empty() {
        out.local.push(LocalOp::Add { id: l.id, assets: add_l });
    }
    if !rm_l.is_empty() {
        out.local.push(LocalOp::Remove { id: l.id, assets: rm_l });
    }
    AlbumLink { immich: r.id.clone(), base: Some(base), name }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(x: &[&str]) -> BTreeSet<String> {
        x.iter().map(|s| s.to_string()).collect()
    }
    fn la(id: u64, name: &str, a: &[&str]) -> LocalAlbum {
        LocalAlbum { id, name: name.into(), assets: set(a), unlinked: 0 }
    }
    fn ra(id: &str, name: &str, a: &[&str]) -> RemoteAlbum {
        RemoteAlbum { id: id.into(), name: name.into(), assets: set(a) }
    }
    fn rule(direction: Direction, policy: Policy) -> FieldRule {
        FieldRule { direction, policy }
    }
    fn linked(base: &[&str], name: &str) -> AlbumLinks {
        BTreeMap::from([(1, AlbumLink { immich: "r1".into(), base: Some(set(base)), name: Some(name.into()) })])
    }

    #[test]
    fn off_syncs_only_hand_linked_albums() {
        let p = plan(rule(Direction::Off, Policy::Ask), &[la(1, "A", &["x"]), la(2, "B", &[])], &[ra("r9", "C", &["y"])], &AlbumLinks::new(), &[]);
        assert!(p.remote.is_empty() && p.local.is_empty() && p.links.is_empty());
    }

    #[test]
    fn two_way_links_by_name_and_creates_the_rest() {
        let p = plan(
            rule(Direction::TwoWay, Policy::Ask),
            &[la(1, "Trip", &["a"]), la(2, "Only here", &["b"])],
            &[ra("r1", "Trip", &["c"]), ra("r2", "Only there", &[])],
            &AlbumLinks::new(),
            &[],
        );
        assert_eq!(p.links[&1].immich, "r1");
        assert!(p.remote.contains(&RemoteOp::Add { id: "r1".into(), assets: vec!["a".into()] }));
        assert!(p.local.contains(&LocalOp::Add { id: 1, assets: vec!["c".into()] }));
        assert!(p.remote.contains(&RemoteOp::Create { local: 2, name: "Only here".into(), assets: set(&["b"]) }));
        assert!(p.local.contains(&LocalOp::Create { immich: "r2".into(), name: "Only there".into(), assets: set(&[]) }));
    }

    #[test]
    fn one_way_never_writes_the_source_side() {
        let p = plan(
            rule(Direction::ToImmich, Policy::Newest),
            &[la(1, "New", &["a"])],
            &[ra("r1", "Old", &["b"]), ra("r2", "X", &[])],
            &linked(&["b"], "Old"),
            &[],
        );
        assert!(p.local.is_empty(), "{:?}", p.local);
        assert!(p.remote.contains(&RemoteOp::Rename { id: "r1".into(), name: "New".into() }));
        assert!(p.remote.contains(&RemoteOp::Add { id: "r1".into(), assets: vec!["a".into()] }));
        assert!(p.remote.contains(&RemoteOp::Remove { id: "r1".into(), assets: vec!["b".into()] }));
        let p =
            plan(rule(Direction::FromImmich, Policy::Newest), &[la(1, "New", &["a"])], &[ra("r1", "Old", &["b", "c"])], &linked(&["b"], "Old"), &[]);
        assert!(p.remote.is_empty(), "{:?}", p.remote);
        assert_eq!(p.local, vec![LocalOp::Add { id: 1, assets: vec!["c".into()] }]);
    }

    #[test]
    fn renames_follow_the_policy() {
        let both = |pol| plan(rule(Direction::TwoWay, pol), &[la(1, "Mine", &[])], &[ra("r1", "Theirs", &[])], &linked(&[], "Old"), &[]);
        assert_eq!(both(Policy::Catalog).remote, vec![RemoteOp::Rename { id: "r1".into(), name: "Mine".into() }]);
        assert_eq!(both(Policy::Immich).local, vec![LocalOp::Rename { id: 1, name: "Theirs".into() }]);
        let p = both(Policy::Ask);
        assert!(p.remote.is_empty() && p.local.is_empty() && p.conflicts.len() == 1);
        assert_eq!(p.links[&1].name.as_deref(), Some("Old"));
    }

    #[test]
    fn deletes_follow_the_policy_and_never_touch_photos() {
        // deleted in the catalog
        let gone = |pol, remote: RemoteAlbum| plan(rule(Direction::TwoWay, pol), &[], &[remote], &linked(&["a"], "A"), &[]);
        let p = gone(Policy::Newest, ra("r1", "A", &["a"]));
        assert_eq!(p.remote, vec![RemoteOp::Delete { id: "r1".into() }]);
        assert!(p.links.is_empty());
        // …but Immich changed it since: the user decides
        let p = gone(Policy::Newest, ra("r1", "A", &["a", "b"]));
        assert!(p.remote.is_empty());
        assert_eq!(p.pending[0].deleted_on, DeletedOn::Catalog);
        assert!(p.links.contains_key(&1));
        let p = gone(Policy::Ask, ra("r1", "A", &["a"]));
        assert!(p.remote.is_empty() && p.pending.len() == 1);
        // Immich wins: unlinked, created again in the catalog
        let p = gone(Policy::Immich, ra("r1", "A", &["a"]));
        assert!(p.remote.is_empty() && p.links.is_empty() && p.pending.is_empty());
        assert_eq!(p.local, vec![LocalOp::Create { immich: "r1".into(), name: "A".into(), assets: set(&["a"]) }]);
        // deleted in Immich
        let p = plan(rule(Direction::TwoWay, Policy::Immich), &[la(1, "A", &["a"])], &[], &linked(&["a"], "A"), &[]);
        assert_eq!(p.local, vec![LocalOp::Delete { id: 1 }]);
        // a waiting deletion is left alone
        let w = PendingDelete { album: 1, immich: "r1".into(), name: "A".into(), deleted_on: DeletedOn::Immich };
        let p = plan(rule(Direction::TwoWay, Policy::Immich), &[la(1, "A", &["a"])], &[], &linked(&["a"], "A"), &[w]);
        assert!(p.local.is_empty() && p.links.contains_key(&1));
        // to Immich only: an Immich-side delete is not followed; the album goes back to Immich
        let p = plan(rule(Direction::ToImmich, Policy::Immich), &[la(1, "A", &["a"])], &[], &linked(&["a"], "A"), &[]);
        assert!(p.local.is_empty() && p.pending.is_empty());
        assert_eq!(p.remote, vec![RemoteOp::Create { local: 1, name: "A".into(), assets: set(&["a"]) }]);
    }

    /// An Immich album delete never takes a catalog album that holds photos Immich never had.
    #[test]
    fn an_immich_delete_keeps_an_album_with_local_only_photos() {
        for pol in [Policy::Immich, Policy::Newest] {
            let mut l = la(1, "A", &["a"]);
            l.unlinked = 2;
            let p = plan(rule(Direction::TwoWay, pol), &[l.clone()], &[], &linked(&["a"], "A"), &[]);
            assert!(p.local.is_empty(), "{pol:?}: the album is not deleted: {:?}", p.local);
            assert!(p.remote.is_empty(), "{pol:?}: and not created again in Immich: {:?}", p.remote);
            assert_eq!(p.kept, vec![KeptAlbum { album: 1, name: "A".into(), unlinked: 2 }]);
            assert!(p.links[&1].is_local_only());
            // later runs leave it alone: no album is made or matched for it, nothing is reported twice
            let again = plan(rule(Direction::TwoWay, pol), &[l], &[ra("r7", "A", &["z"])], &p.links, &[]);
            assert!(again.remote.is_empty() && again.kept.is_empty(), "{again:?}");
            assert!(!again.local.iter().any(|o| matches!(o, LocalOp::Add { .. } | LocalOp::Delete { .. } | LocalOp::Remove { .. })), "{again:?}");
            assert!(again.links[&1].is_local_only());
        }
        // only linked photos in it: the delete travels as before
        let p = plan(rule(Direction::TwoWay, Policy::Immich), &[la(1, "A", &["a"])], &[], &linked(&["a"], "A"), &[]);
        assert_eq!(p.local, vec![LocalOp::Delete { id: 1 }]);
        // ask still asks (the user may delete it knowingly)
        let mut l = la(1, "A", &["a"]);
        l.unlinked = 1;
        let p = plan(rule(Direction::TwoWay, Policy::Ask), &[l], &[], &linked(&["a"], "A"), &[]);
        assert!(p.local.is_empty() && p.kept.is_empty() && p.pending.len() == 1);
    }

    /// The base after a failed add must not hold the asset: else the next run reads "was in both,
    /// gone from Immich" and removes the photo from the catalog album.
    #[test]
    fn a_failed_add_is_added_again_and_never_removes_from_the_catalog() {
        let r = rule(Direction::TwoWay, Policy::Ask);
        // first matched by name, the catalog has `a` and `b`, Immich nothing
        let p = plan(r, &[la(1, "Trip", &["a", "b"])], &[ra("r1", "Trip", &[])], &AlbumLinks::new(), &[]);
        assert_eq!(p.remote, vec![RemoteOp::Add { id: "r1".into(), assets: vec!["a".into(), "b".into()] }]);
        // the add of `b` did not work
        let mut links = p.links.clone();
        links.get_mut(&1).unwrap().not_added([&"b".to_string()]);
        assert_eq!(links[&1].base, Some(set(&["a"])));
        let next = plan(r, &[la(1, "Trip", &["a", "b"])], &[ra("r1", "Trip", &["a"])], &links, &[]);
        assert!(next.local.is_empty(), "nothing leaves the catalog album: {:?}", next.local);
        assert_eq!(next.remote, vec![RemoteOp::Add { id: "r1".into(), assets: vec!["b".into()] }]);
        // …whereas the uncorrected base would have removed it (the bug this guards against)
        let wrong = plan(r, &[la(1, "Trip", &["a", "b"])], &[ra("r1", "Trip", &["a"])], &p.links, &[]);
        assert_eq!(wrong.local, vec![LocalOp::Remove { id: 1, assets: vec!["b".into()] }]);

        // a failed remove: the asset stays in the base and is removed again, not pulled back in
        let p = plan(r, &[la(1, "Trip", &["a"])], &[ra("r1", "Trip", &["a", "b"])], &linked(&["a", "b"], "Trip"), &[]);
        assert_eq!(p.remote, vec![RemoteOp::Remove { id: "r1".into(), assets: vec!["b".into()] }]);
        let mut links = p.links.clone();
        links.get_mut(&1).unwrap().not_removed([&"b".to_string()]);
        let next = plan(r, &[la(1, "Trip", &["a"])], &[ra("r1", "Trip", &["a", "b"])], &links, &[]);
        assert!(next.local.is_empty(), "{:?}", next.local);
        assert_eq!(next.remote, vec![RemoteOp::Remove { id: "r1".into(), assets: vec!["b".into()] }]);
    }
}
