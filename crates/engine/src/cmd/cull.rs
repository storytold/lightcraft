//! Assisted culling: score photos for focus and clipping, group similar shots taken close
//! together (bursts) and mark the sharpest of each group; optionally reject blurry ones and pick
//! each group's best, and stack each burst with its best photo on top. Scores are kept on the
//! photos (rules / smart albums: `sharpness`, `bestOfGroup`).

use lightcraft_catalog::{Analysis, Flag, Op, PhotoId};
use serde_json::{Value, json};

use super::{CommandSpec, always, cmd};
use crate::{Result, Session};

/// Seconds between shots of the same burst at most (default of `burstGap`).
const BURST_GAP: f64 = 10.0;
/// Signature similarity for "the same scene" (default of `similarity`).
const SAME: f64 = 0.93;
/// The longest `burstGap` accepted: a day.
const MAX_GAP: f64 = 86_400.0;

fn secs(iso: &str) -> Option<i64> {
    let d = lightcraft_meta::DateTime::parse_iso(iso)?;
    Some(d.unix_seconds())
}

fn analyze(s: &mut Session, p: &Value) -> Result<Value> {
    let ids: Vec<PhotoId> = if p.get("ids").is_some() || s.selection.ids.len() > 1 { s.targets(p)? } else { s.visible_cloned() };
    // a photo listed twice is one photo, not a burst of two look-alikes (first occurrence kept)
    let mut seen = std::collections::HashSet::new();
    let ids: Vec<PhotoId> = ids.into_iter().filter(|id| seen.insert(*id)).collect();
    let reject_below = p.get("rejectBelow").and_then(Value::as_f64).map(|v| v as f32);
    let pick_best = p.get("pickBest").and_then(Value::as_bool).unwrap_or(false);
    let (gap, same) = burst_params(p)?;
    let flag_param = |key: &str| -> Result<bool> {
        match p.get(key) {
            None | Some(Value::Null) => Ok(false),
            Some(v) => v.as_bool().ok_or_else(|| super::bad("photo.analyze", format!("`{key}` must be true or false"))),
        }
    };
    let stack_groups = flag_param("stackGroups")?;
    let reset = flag_param("resetBursts")?;
    // measure (thumbnail-level sources: fast, and enough for focus at 512 px)
    let mut rows: Vec<(PhotoId, Option<i64>, f32, f32, [f32; 64])> = Vec::new();
    let mut failed = Vec::new();
    for id in ids {
        match s.source_now(id, crate::media::SourceLevel::Thumb) {
            Ok(src) => {
                let t = s.catalog.photo(id).and_then(|p| p.captured.as_deref().and_then(secs));
                use lightcraft_pipeline::cull;
                rows.push((id, t, cull::sharpness(&src), cull::clipped(&src), cull::signature(&src)));
            }
            Err(e) => failed.push(json!([id.0, e])),
        }
    }
    // bursts: consecutive (by capture time) look-alike shots
    rows.sort_by_key(|r| (r.1.unwrap_or(i64::MAX), r.0));
    // A photo whose burst was arranged by hand keeps it (burst, place, pick): only its scores
    // are measured again. The others are grouped among themselves, in bursts numbered after
    // every burst that stays (earlier runs on other photos, the ones arranged by hand).
    let by_hand = |id: PhotoId| -> Option<Analysis> { s.catalog.photo(id).and_then(|p| p.analysis).filter(|a| !reset && a.by_hand()) };
    let kept: Vec<Option<Analysis>> = rows.iter().map(|r| by_hand(r.0)).collect();
    let auto: Vec<usize> = (0..rows.len()).filter(|i| kept[*i].is_none()).collect();
    let regrouped: std::collections::HashSet<PhotoId> = auto.iter().map(|i| rows[*i].0).collect();
    let mut next = s.catalog.photos().filter(|p| !regrouped.contains(&p.id)).filter_map(|p| p.analysis.and_then(|a| a.group)).max().unwrap_or(0) + 1;
    let mut group_of: Vec<Option<u32>> = vec![None; rows.len()];
    for w in auto.windows(2) {
        let (i, j) = (w[0], w[1]);
        let (a, b) = (&rows[i], &rows[j]);
        let close = matches!((a.1, b.1), (Some(x), Some(y)) if (y.saturating_sub(x)).unsigned_abs() as f64 <= gap);
        if close && lightcraft_pipeline::cull::similarity(&a.4, &b.4) >= same {
            let g = *group_of[i].get_or_insert_with(|| {
                next += 1;
                next - 1
            });
            group_of[j] = Some(g);
        }
    }
    let mut best: std::collections::HashMap<u32, (usize, f32)> = Default::default();
    for (i, g) in group_of.iter().enumerate() {
        if let Some(g) = g {
            let e = best.entry(*g).or_insert((i, -1.0));
            if rows[i].2 > e.1 {
                *e = (i, rows[i].2);
            }
        }
    }
    let mut ops = Vec::new();
    let (mut rejected, mut picked) = (0, 0);
    let mut out = Vec::new();
    for (i, (id, _, sharp, clip, _)) in rows.iter().enumerate() {
        let analysis = match kept[i] {
            Some(k) => Analysis { sharpness: *sharp, clipped: *clip, ..k },
            None => {
                let g = group_of[i];
                let is_best = g.is_some_and(|g| best.get(&g).is_some_and(|b| b.0 == i));
                Analysis { sharpness: *sharp, clipped: *clip, group: g, best: is_best, rank: None }
            }
        };
        let (g, is_best) = (analysis.group, analysis.best);
        ops.push(Op::SetAnalysis { id: *id, analysis: Some(analysis) });
        let flag = s.catalog.photo(*id).map(|p| p.flag).unwrap_or_default();
        if reject_below.is_some_and(|t| *sharp < t) && flag != Flag::Reject {
            ops.push(Op::SetFlag { id: *id, flag: Flag::Reject });
            rejected += 1;
        } else if pick_best && is_best && flag == Flag::None {
            ops.push(Op::SetFlag { id: *id, flag: Flag::Pick });
            picked += 1;
        }
        out.push(json!({"id": id.0, "sharpness": (sharp * 10.0).round() / 10.0, "clipped": clip, "group": g, "best": is_best, "byHand": kept[i].is_some()}));
    }
    let mut stacks = 0;
    if stack_groups {
        // Photos already in a stack stay where they are (like `stack.auto`): a burst is stacked
        // from its other photos when two or more remain. A burst culling grouped has the sharpest
        // of those on top; a burst arranged by hand keeps its order, its pick (place 0) on top.
        let index = s.catalog.stack_index();
        let mut members: std::collections::BTreeMap<u32, Vec<(PhotoId, f32)>> = Default::default();
        let mut arranged: std::collections::BTreeMap<u32, Vec<(u32, PhotoId)>> = Default::default();
        for (i, r) in rows.iter().enumerate() {
            if index.contains_key(&r.0) || !s.catalog.photo(r.0).is_some_and(|ph| !ph.deleted) {
                continue;
            }
            match kept[i] {
                Some(k) => {
                    if let Some(g) = k.group {
                        arranged.entry(g).or_default().push((k.rank.unwrap_or(u32::MAX), r.0));
                    }
                }
                None => {
                    if let Some(g) = group_of[i] {
                        members.entry(g).or_default().push((r.0, r.2));
                    }
                }
            }
        }
        let mut orders: Vec<Vec<PhotoId>> = Vec::new();
        for group in members.values() {
            let Some(top) = group.iter().fold(None::<(PhotoId, f32)>, |b, m| if b.is_none_or(|b| m.1 > b.1) { Some(*m) } else { b }) else {
                continue;
            };
            let mut photos = vec![top.0];
            photos.extend(group.iter().map(|m| m.0).filter(|p| *p != top.0));
            orders.push(photos);
        }
        for group in arranged.values_mut() {
            group.sort();
            orders.push(group.iter().map(|m| m.1).collect());
        }
        for photos in orders {
            if photos.len() >= 2
                && let Some(op) = s.catalog.group_ops(photos[0], &photos, true)
            {
                ops.push(op);
                stacks += 1;
            }
        }
    }
    if !ops.is_empty() {
        s.commit("Assisted Culling", Op::Batch { ops })?;
    }
    if stacks > 0 {
        keep_active_shown(s);
    }
    let kept = kept.iter().filter(|k| k.is_some()).count();
    Ok(json!({"photos": out, "groups": best.len(), "kept": kept, "stacks": stacks, "rejected": rejected, "picked": picked, "failed": failed}))
}

/// A photo now hidden inside a collapsed stack must not stay the active one: its stack's top
/// takes over, and the rest of the selection stays selected.
fn keep_active_shown(s: &mut Session) {
    let vis = s.visible_cloned();
    if let Some(a) = s.selection.active
        && !vis.contains(&a)
        && let Some(st) = s.catalog.stack_of(a)
    {
        let top = vis.iter().copied().find(|p| st.photos.contains(p)).unwrap_or(st.top());
        s.selection.ids.retain(|id| vis.contains(id));
        if !s.selection.ids.contains(&top) {
            s.selection.ids.push(top);
        }
        s.selection.active = Some(top);
    }
}

/// Where `photo.moveToBurst` puts the photos.
#[derive(Clone, Copy, PartialEq)]
enum Dest {
    Burst(u32),
    New,
    Nowhere,
}

/// Arrange bursts by hand: `ids` go into a burst at `index` (0 = the pick), into a new burst, or
/// out of every burst. Every burst touched is then arranged by hand ([`Analysis::rank`]), so
/// culling run again keeps it. What marks a burst's pick follows its first photo: `best` (the
/// Best of Similar Shots rule), the Pick flag when culling's pick had it, and the top of the
/// burst's stack when the burst is stacked. One undo step.
fn move_to_burst(s: &mut Session, p: &Value) -> Result<Value> {
    use std::collections::{BTreeMap, HashMap};
    const C: &str = "photo.moveToBurst";
    let mut ids = s.targets(p)?;
    let mut seen = std::collections::HashSet::new();
    ids.retain(|id| seen.insert(*id));
    if ids.is_empty() {
        return Err(super::bad(C, "no photos"));
    }
    for id in &ids {
        match s.catalog.photo(*id) {
            Some(ph) if ph.deleted => return Err(super::bad(C, format!("“{}” is deleted", ph.file_name))),
            Some(ph) if ph.analysis.is_none() => {
                return Err(super::bad(C, format!("“{}” has no culling scores yet: run Assisted Culling on it first", ph.file_name)));
            }
            Some(_) => {}
            None => return Err(super::bad(C, format!("no photo {}", id.0))),
        }
    }
    let group = |s: &Session, id: PhotoId| s.catalog.photo(id).and_then(|p| p.analysis).and_then(|a| a.group);
    let index = s.catalog.burst_index();
    let into = match p.get("burst") {
        None => Dest::Burst(group(s, ids[0]).ok_or_else(|| super::bad(C, "the photo isn't in a burst: say which `burst` it goes to"))?),
        Some(Value::Null) => Dest::Nowhere,
        Some(Value::String(n)) if n == "new" => Dest::New,
        Some(v) => match v.as_u64().and_then(|n| u32::try_from(n).ok()) {
            Some(n) if index.contains_key(&n) => Dest::Burst(n),
            Some(n) => return Err(super::bad(C, format!("there is no burst {n}"))),
            None => return Err(super::bad(C, "`burst` must be a burst number, \"new\" or null")),
        },
    };
    let at = match p.get("index") {
        None | Some(Value::Null) => usize::MAX,
        Some(v) => v.as_u64().map(|n| n as usize).ok_or_else(|| super::bad(C, "`index` must be a place in the burst: 0 (the pick), 1, …"))?,
    };
    // the bursts touched, before and after
    let target = match into {
        Dest::Burst(g) => Some(g),
        Dest::New => Some(s.catalog.next_burst_id()),
        Dest::Nowhere => None,
    };
    let mut old: BTreeMap<u32, Vec<PhotoId>> = BTreeMap::new();
    for g in ids.iter().filter_map(|id| group(s, *id)).chain(target) {
        old.entry(g).or_insert_with(|| index.get(&g).cloned().unwrap_or_default());
    }
    let mut new = old.clone();
    for members in new.values_mut() {
        members.retain(|m| !ids.contains(m));
    }
    if let Some(g) = target {
        let members = new.entry(g).or_default();
        let at = at.min(members.len());
        members.splice(at..at, ids.iter().copied());
    }
    // each photo's burst, place and pick: a burst a move leaves with one photo is no burst (the
    // burst moved into stays one, so a new burst may start with one photo for more to join)
    let remains = |g: u32, members: &Vec<PhotoId>| members.len() >= 2 || (target == Some(g) && !members.is_empty());
    let mut placed: HashMap<PhotoId, (Option<u32>, u32, bool)> = ids.iter().map(|id| (*id, (None, 0, false))).collect();
    for (g, members) in &new {
        let stays = remains(*g, members);
        for (i, m) in members.iter().enumerate() {
            placed.insert(*m, if stays { (Some(*g), i as u32, i == 0) } else { (None, 0, false) });
        }
    }
    let mut ops = Vec::new();
    let mut by_id: Vec<_> = placed.iter().collect();
    by_id.sort_by_key(|(id, _)| **id);
    for (id, (g, rank, best)) in by_id {
        let Some(was) = s.catalog.photo(*id).and_then(|p| p.analysis) else { continue };
        let now = Analysis { group: *g, rank: Some(*rank), best: *best, ..was };
        if now != was {
            ops.push(Op::SetAnalysis { id: *id, analysis: Some(now) });
        }
    }
    // what the target burst holds now
    let members: Option<Vec<u64>> = target.and_then(|g| new.get(&g).filter(|m| remains(g, m))).map(|m| m.iter().map(|p| p.0).collect());
    let report = |changed: bool| json!({"burst": members.as_ref().and(target), "pick": members.as_ref().and_then(|m| m.first()), "photos": members, "changed": changed});
    if ops.is_empty() {
        return Ok(report(false));
    }
    // the Pick culling gave a burst's pick goes to its new pick
    let is_pick = |id: PhotoId| placed.get(&id).is_some_and(|p| p.2);
    let flag = |s: &Session, id: PhotoId| s.catalog.photo(id).map(|p| p.flag).unwrap_or_default();
    let mut flags: BTreeMap<PhotoId, Flag> = BTreeMap::new();
    for (g, before) in &old {
        let Some(old_pick) = before.iter().copied().find(|m| s.catalog.photo(*m).and_then(|p| p.analysis).is_some_and(|a| a.best)) else { continue };
        let new_pick = new.get(g).filter(|m| remains(*g, m)).and_then(|m| m.first()).copied();
        if flag(s, old_pick) != Flag::Pick || new_pick == Some(old_pick) {
            continue;
        }
        if !is_pick(old_pick) {
            flags.insert(old_pick, Flag::None);
        }
        // the new pick was chosen by hand: it takes the Pick even over a Reject
        if let Some(n) = new_pick {
            flags.insert(n, Flag::Pick);
        }
    }
    ops.extend(flags.into_iter().map(|(id, flag)| Op::SetFlag { id, flag }));
    // a stacked burst (a stack of exactly its photos, as culling stacks them) follows it: its stack
    // shrinks, grows or goes, in the burst's order. Shrinking first: a photo is in one stack at a time.
    let (mut shrink, mut grow) = (Vec::new(), Vec::new());
    let mut grows_target = false;
    let mut followed = std::collections::HashSet::new();
    for (g, before) in old.iter().filter(|(_, m)| m.len() >= 2) {
        let Some(st) = s.catalog.stack_of(before[0]).filter(|st| st.photos.len() == before.len() && before.iter().all(|m| st.photos.contains(m)))
        else {
            continue;
        };
        followed.insert(st.id);
        let after = new.get(g).filter(|m| m.len() >= 2);
        match after {
            None => shrink.push(Op::RemoveStack { id: st.id }),
            Some(after) if after == &st.photos => {}
            Some(after) => {
                let op = Op::SetStack { id: st.id, photos: after.clone(), collapsed: st.collapsed };
                if after.iter().all(|m| st.photos.contains(m)) {
                    shrink.push(op)
                } else {
                    grows_target = true;
                    grow.push(op)
                }
            }
        }
    }
    if grows_target {
        // photos joining a stacked burst leave any other stack they are in
        let strays: Vec<PhotoId> = ids.iter().copied().filter(|id| s.catalog.stack_of(*id).is_some_and(|st| !followed.contains(&st.id))).collect();
        shrink.extend(s.catalog.remove_from_stacks_ops(&strays));
    }
    let restacked = !shrink.is_empty() || !grow.is_empty();
    ops.extend(shrink);
    ops.extend(grow);
    let same = matches!(into, Dest::Burst(g) if ids.iter().all(|id| group(s, *id) == Some(g)));
    let label = match into {
        Dest::Burst(_) if same && at == 0 => "Choose Burst Pick",
        Dest::Burst(_) if same => "Rearrange Burst",
        Dest::Burst(_) => "Move to Burst",
        Dest::New => "New Burst",
        Dest::Nowhere => "Remove from Burst",
    };
    s.commit(label, Op::Batch { ops })?;
    if restacked {
        keep_active_shown(s);
    }
    Ok(report(true))
}

/// `burstGap` (seconds, 0..=a day) and `similarity` (0..=1) of `photo.analyze`, refused before
/// anything is measured when they are not numbers in range.
fn burst_params(p: &Value) -> Result<(f64, f32)> {
    const C: &str = "photo.analyze";
    let num = |key: &str, d: f64, max: f64| -> Result<f64> {
        match p.get(key) {
            None | Some(Value::Null) => Ok(d),
            Some(v) => match v.as_f64() {
                Some(x) if x.is_finite() && (0.0..=max).contains(&x) => Ok(x),
                _ => Err(super::bad(C, format!("`{key}` must be a number from 0 to {max}"))),
            },
        }
    };
    Ok((num("burstGap", BURST_GAP, MAX_GAP)?, num("similarity", SAME, 1.0)? as f32))
}

/// Find Similar: photos that look like `id` (signatures from thumbnail-level sources, cached by
/// content), most similar first; the view is filtered to them.
fn find_similar(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.findSimilar";
    let id = super::photo_id(p, C)?.or(s.active()).ok_or_else(|| super::bad(C, "no photo"))?;
    let min = p.get("similarity").and_then(Value::as_f64).unwrap_or(0.8) as f32;
    let all: Vec<PhotoId> = s.catalog.photos().filter(|q| q.in_library()).map(|q| q.id).collect();
    let sig = |s: &mut Session, id: PhotoId| -> Option<[f32; 64]> {
        let key = s.catalog.photo(id).map(|p| crate::media::content_key(p))?;
        if let Some(v) = s.signatures.get(&key) {
            return Some(*v);
        }
        let src = s.source_now(id, crate::media::SourceLevel::Thumb).ok()?;
        let v = lightcraft_pipeline::cull::signature(&src);
        s.signatures.insert(key, v);
        Some(v)
    };
    let me = sig(s, id).ok_or_else(|| super::bad(C, "can't read the photo"))?;
    let mut hits: Vec<(PhotoId, f32)> = Vec::new();
    for q in all {
        if let Some(v) = sig(s, q) {
            let sim = lightcraft_pipeline::cull::similarity(&me, &v);
            if sim >= min {
                hits.push((q, sim.clamp(0.0, 1.0)));
            }
        }
    }
    hits.sort_by(|a, b| b.1.total_cmp(&a.1));
    if p.get("filter").and_then(Value::as_bool).unwrap_or(true) {
        s.shelved_filter = None;
        s.filter.only = hits.iter().map(|h| h.0).collect();
        // an empty `only` means "no constraint": no hits is its own state that shows no photos
        s.filter.only_none = s.filter.only.is_empty();
    }
    Ok(json!({"photos": hits.iter().map(|(i, v)| json!({"id": i.0, "similarity": (v * 1000.0).round() / 1000.0})).collect::<Vec<_>>()}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "library.findSimilar",
            "Find Similar Photos",
            ["Photo"],
            None,
            "{id?, similarity?: 0..1 (0.8), filter?: bool (true)} — photos that look like the active one (composition and tones), most similar first; filters the view to them (Clear Filters to go back) → {photos: [{id, similarity}]}",
            always,
            find_similar
        ),
        cmd!(
            "photo.analyze",
            "Assisted Culling",
            [],
            None,
            "{ids?, rejectBelow?: sharpness 0..100, pickBest?: bool, stackGroups?: bool (false), burstGap?: seconds 0..86400 (10), similarity?: 0..1 (0.93), resetBursts?: bool (false)} — score the selected photos (else all in view) for focus (0..100) and clipping, group look-alike shots (signature similarity at least `similarity`) taken at most `burstGap` seconds apart and mark the sharpest of each; optionally reject blurry photos, pick each group's best and, with `stackGroups`, stack each burst collapsed with its sharpest photo on top (photos already in a stack stay put: the rest of a burst is stacked when two or more remain). Photos whose burst was arranged by hand (`photo.moveToBurst`) keep their burst, place and pick and are only scored again, unless `resetBursts`; one undo step → {photos: [{id, sharpness, clipped, group, best, byHand}], groups, kept, stacks, rejected, picked, failed}",
            always,
            analyze
        ),
        cmd!(
            "photo.moveToBurst",
            "Move to Burst",
            [],
            None,
            "{ids?, burst?: number | \"new\" | null (default: the first photo's burst), index?: place (0 = the pick; default last)} — arrange Assisted Culling's bursts by hand (Window ▸ Bursts): the photos go into that burst at `index`, into a new burst, or (null) out of every burst; reordering within a burst is the same call. A burst left with one photo is no burst. The pick follows the first place: Best of Similar Shots, the Pick flag culling gave the old pick, and the top of the burst's stack. Every burst touched is kept as arranged when culling runs again (`photo.analyze` `resetBursts` forgets it). Photos must have been culled; one undo step → {burst, photos, pick, changed}",
            always,
            move_to_burst
        ),
    ]
}
