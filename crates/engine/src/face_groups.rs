//! Incremental library-wide grouping, using the same best-two-example score as People → More.
//! Confirmed people seed galleries; unnamed groups learn from their members as they grow.
use std::sync::Arc;
use std::time::{Duration, Instant};

use lightcraft_catalog::PhotoId;
use lightcraft_faces::matching::cosine;
use lightcraft_geom::Rect;
use lightcraft_meta::RegionKind;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Result, Session};

struct Face {
    photo: u64,
    index: usize,
    rect: Rect,
    embedding: Arc<[f32]>,
}

pub(crate) struct Grouping {
    tag: String,
    strict: bool,
    faces: Vec<Face>,
    groups: Vec<Group>,
    next: usize,
    candidate: usize,
    example: usize,
    score: Score,
    best: Option<(usize, f32)>,
    runner_up: f32,
    unindexed: usize,
    saved: bool,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    version: u32,
    tag: String,
    strict: bool,
    unindexed: usize,
    groups: Vec<SavedGroup>,
}

#[derive(Serialize, Deserialize)]
struct SavedGroup {
    name: Option<String>,
    #[serde(default)]
    anchor: Option<SavedFace>,
    faces: Vec<SavedFace>,
}

#[derive(Clone, Serialize, Deserialize)]
struct SavedFace {
    photo: u64,
    index: usize,
    rect: Rect,
    content_hash: Option<String>,
}

/// Return the per-library cache path; in-memory libraries never persist suggestions.
fn saved_path(s: &Session) -> Option<std::path::PathBuf> {
    s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.join("unnamed-face-groups.json"))
}

/// Read a bounded saved partition, rejecting stale model tags and filtering replaced photos.
fn load(s: &Session) -> Result<Option<Grouping>> {
    use std::io::Read;
    let Some(path) = saved_path(s) else { return Ok(None) };
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(crate::cmd::bad("faces.groupUnnamed", format!("Cannot read saved groups: {e}. Use Refresh groups to rebuild."))),
    };
    let mut bytes = Vec::new();
    file.take(32 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|e| crate::cmd::bad("faces.groupUnnamed", e.to_string()))?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err(crate::cmd::bad("faces.groupUnnamed", "Saved groups exceed 32 MiB. Use Refresh groups to rebuild."));
    }
    let saved: Saved = serde_json::from_slice(&bytes)
        .map_err(|e| crate::cmd::bad("faces.groupUnnamed", format!("Cannot read saved groups: {e}. Use Refresh groups to rebuild.")))?;
    if saved.version != 1 || saved.tag != s.faces.index.tag {
        return Ok(None);
    }
    let count = saved.groups.iter().try_fold(0usize, |n, g| n.checked_add(g.faces.len()));
    if count.is_none_or(|n| n > 100_000) || saved.groups.len() > 100_000 {
        return Err(crate::cmd::bad("faces.groupUnnamed", "Invalid saved group count. Use Refresh groups to rebuild."));
    }
    let mut g = Grouping {
        tag: saved.tag,
        strict: saved.strict,
        faces: Vec::new(),
        groups: Vec::new(),
        next: 0,
        candidate: 0,
        example: 0,
        score: Score::default(),
        best: None,
        runner_up: -1.0,
        unindexed: saved.unindexed,
        saved: true,
    };
    let mut seen = std::collections::HashSet::new();
    for group in saved.groups {
        let mut members = Vec::new();
        for f in group.faces {
            if !seen.insert((f.photo, f.index)) {
                continue;
            }
            let Some(photo) = s.catalog.photo(PhotoId(f.photo)).filter(|p| p.in_library()) else { continue };
            if photo.content_hash != f.content_hash {
                continue;
            }
            members.push(g.faces.len());
            g.faces.push(Face { photo: f.photo, index: f.index, rect: f.rect, embedding: Arc::from([]) });
        }
        g.groups.push(Group { members, gallery: Vec::new(), name: group.name, anchor: group.anchor });
    }
    g.next = g.faces.len();
    Ok(Some(g))
}

struct Group {
    members: Vec<usize>,
    gallery: Vec<Arc<[f32]>>,
    name: Option<String>,
    anchor: Option<SavedFace>,
}

/// The mean of the closest two valid examples, matching `matching::rank` without sorting.
#[derive(Default)]
struct Score {
    first: Option<f32>,
    second: Option<f32>,
}

impl Score {
    /// Retain the two strongest valid similarities without sorting a gallery.
    fn add(&mut self, value: f32) {
        if self.first.is_none_or(|best| value > best) {
            self.second = self.first;
            self.first = Some(value);
        } else if self.second.is_none_or(|best| value > best) {
            self.second = Some(value);
        }
    }

    /// Use one example when necessary, otherwise average the closest two examples.
    fn mean(&self) -> Option<f32> {
        self.first.map(|first| self.second.map_or(first, |second| (first + second) / 2.0))
    }
}

impl Grouping {
    /// Advance comparison cursors within the time budget, teaching only unnamed galleries.
    fn step(&mut self, threshold: f32, budget: Duration) {
        let start = Instant::now();
        while let Some(face) = self.faces.get(self.next) {
            if start.elapsed() >= budget {
                break;
            }
            if let Some(group) = self.groups.get(self.candidate) {
                let examples = if self.strict && group.name.is_none() { group.gallery.len().min(1) } else { group.gallery.len() };
                if self.example < examples {
                    if let Some(example) = group.gallery.get(self.example)
                        && let Some(score) = cosine(&face.embedding, example)
                    {
                        self.score.add(score);
                    }
                    self.example += 1;
                    continue;
                }
                if let Some(score) = self.score.mean() {
                    if self.best.is_none_or(|(_, best)| score > best) {
                        self.runner_up = self.best.map_or(-1.0, |(_, best)| best);
                        self.best = Some((self.candidate, score));
                    } else {
                        self.runner_up = self.runner_up.max(score);
                    }
                }
                self.candidate += 1;
                // Preserve the original More workflow: a known person's qualifying match
                // stays with a confirmed identity instead of being stolen by an unnamed cluster.
                if !self.strict
                    && self.groups.get(self.candidate).is_none_or(|g| g.name.is_none())
                    && self.best.is_some_and(|(i, score)| score >= threshold && self.groups.get(i).is_some_and(|g| g.name.is_some()))
                {
                    self.candidate = self.groups.len();
                }
                self.example = 0;
                self.score = Score::default();
                continue;
            }
            let margin = if self.strict { 0.06 } else { 0.0 };
            let matched = self.best.filter(|(_, score)| *score >= threshold && *score - self.runner_up >= margin);
            if let Some(group) = matched.and_then(|(i, _)| self.groups.get_mut(i)) {
                group.members.push(self.next);
                // Suggestions must not teach a confirmed identity before the user accepts them.
                if group.name.is_none() && !self.strict {
                    // Keep pose/age variety without comparing a large group against thousands
                    // of near-duplicate burst shots. Confirmed galleries above remain complete.
                    let score = self.best.map_or(-1.0, |(_, score)| score);
                    if group.gallery.len() < 2 || score < 0.9 {
                        if group.gallery.len() < 64 {
                            group.gallery.push(face.embedding.clone());
                        } else if let Some(example) = group.gallery.get_mut(group.members.len() % 64) {
                            *example = face.embedding.clone();
                        }
                    }
                }
            } else {
                self.groups.push(Group { members: vec![self.next], gallery: vec![face.embedding.clone()], name: None, anchor: None });
            }
            self.next += 1;
            self.candidate = 0;
            self.best = None;
            self.runner_up = -1.0;
        }
    }
}

/// Load or advance suggestions, filter stale members, and atomically save completed refreshes.
pub(crate) fn query(s: &mut Session, p: &Value, model_threshold: f32) -> Result<Value> {
    if s.faces.groups.is_none() && p["restart"] != true {
        s.faces.groups = load(s)?;
    }
    let strict = p["strict"].as_bool().unwrap_or_else(|| s.faces.groups.as_ref().is_some_and(|g| g.strict));
    let threshold = if strict { model_threshold.max(0.55) } else { model_threshold * 0.8 };
    if p["restart"] == true || s.faces.groups.as_ref().is_some_and(|g| g.tag != s.faces.index.tag || g.strict != strict) {
        s.faces.groups = None;
    }
    if s.faces.groups.is_none() {
        let mut faces = Vec::new();
        let mut named: std::collections::BTreeMap<String, Group> = Default::default();
        let mut unindexed = 0usize;
        for photo in s.catalog.photos().filter(|p| p.in_library()) {
            for (index, region) in photo.meta.regions.iter().enumerate().filter(|(_, r)| r.kind == RegionKind::Face) {
                if let Some(name) = &region.name {
                    if let Some(e) = s.faces.index.get(photo.id.0, &region.rect) {
                        named
                            .entry(name.trim().to_lowercase())
                            .or_insert_with(|| Group {
                                members: Vec::new(),
                                gallery: Vec::new(),
                                name: Some(name.clone()),
                                anchor: Some(SavedFace { photo: photo.id.0, index, rect: region.rect, content_hash: photo.content_hash.clone() }),
                            })
                            .gallery
                            .push(e.clone());
                    }
                    continue;
                }
                if let Some(e) = s.faces.index.get(photo.id.0, &region.rect) {
                    if faces.len() >= 100_000 {
                        return Err(crate::cmd::bad("faces.groupUnnamed", "grouping supports up to 100000 indexed unnamed faces"));
                    }
                    faces.push(Face { photo: photo.id.0, index, rect: region.rect, embedding: e.clone() });
                } else {
                    unindexed += 1;
                }
            }
        }
        faces.sort_by_key(|f| (f.photo, f.index));
        s.faces.groups = Some(Grouping {
            strict,
            tag: s.faces.index.tag.clone(),
            faces,
            groups: named.into_values().collect(),
            next: 0,
            candidate: 0,
            example: 0,
            score: Score::default(),
            best: None,
            runner_up: -1.0,
            unindexed,
            saved: false,
        });
    }
    let Some(g) = &mut s.faces.groups else { return Ok(Value::Null) };
    g.step(threshold, Duration::from_millis(p["budgetMs"].as_u64().unwrap_or(8).clamp(1, 20)));
    let complete = g.next == g.faces.len();
    let mut groups = Vec::new();
    if complete {
        let mut current_names = std::collections::BTreeMap::new();
        if g.groups.iter().any(|group| group.name.is_some() && group.anchor.is_none()) {
            for photo in s.catalog.photos().filter(|p| p.in_library()) {
                for name in photo.meta.regions.iter().filter(|r| r.kind == RegionKind::Face).filter_map(|r| r.name.as_ref()) {
                    current_names.insert(name.trim().to_lowercase(), name);
                }
            }
        }
        for group in &g.groups {
            let faces: Vec<Value> = group
                .members
                .iter()
                .filter_map(|i| {
                    let f = g.faces.get(*i)?;
                    let photo = s.catalog.photo(PhotoId(f.photo)).filter(|p| p.in_library())?;
                    let region = photo.meta.regions.get(f.index)?;
                    if region.kind != RegionKind::Face || region.name.is_some() || region.rect != f.rect {
                        return None;
                    }
                    let view = s.faces.index.view(f.photo, &f.rect).unwrap_or(f.rect);
                    Some(json!({"photo": f.photo, "index": f.index, "rect": f.rect, "view": view, "contentHash": photo.content_hash}))
                })
                .collect();
            if !faces.is_empty() {
                // Follow the confirmed region through renames, merges and history, including
                // after reopening. Older saved sets have no anchor: only retain an existing name.
                let suggestion = if let Some(anchor) = &group.anchor {
                    s.catalog
                        .photo(PhotoId(anchor.photo))
                        .filter(|p| p.in_library() && p.content_hash == anchor.content_hash)
                        .and_then(|p| p.meta.regions.get(anchor.index))
                        .filter(|r| r.kind == RegionKind::Face && r.rect == anchor.rect)
                        .and_then(|r| r.name.as_ref())
                } else {
                    group.name.as_ref().and_then(|name| current_names.get(&name.trim().to_lowercase()).copied())
                };
                groups.push(json!({"faces": faces, "suggestion": suggestion}));
            }
        }
        groups.sort_by_key(|g| std::cmp::Reverse(g["faces"].as_array().map_or(0, Vec::len)));
    }
    let response =
        json!({"complete": complete, "strict": g.strict, "processed": g.next, "total": g.faces.len(), "unindexed": g.unindexed, "groups": groups});
    if complete && !g.saved {
        if let Some(path) = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.join("unnamed-face-groups.json")) {
            let groups = g
                .groups
                .iter()
                .map(|group| SavedGroup {
                    name: group.name.clone(),
                    anchor: group.anchor.clone(),
                    faces: group
                        .members
                        .iter()
                        .filter_map(|i| {
                            let f = g.faces.get(*i)?;
                            let photo = s.catalog.photo(PhotoId(f.photo))?;
                            Some(SavedFace { photo: f.photo, index: f.index, rect: f.rect, content_hash: photo.content_hash.clone() })
                        })
                        .collect(),
                })
                .collect();
            let saved = Saved { version: 1, tag: g.tag.clone(), strict: g.strict, unindexed: g.unindexed, groups };
            let bytes = serde_json::to_vec(&saved).map_err(|e| crate::cmd::bad("faces.groupUnnamed", e.to_string()))?;
            if bytes.len() > 32 * 1024 * 1024 {
                return Err(crate::cmd::bad("faces.groupUnnamed", "Saved groups exceed 32 MiB"));
            }
            lightcraft_catalog::safe_file::write_atomic(&path, &bytes)
                .map_err(|e| crate::cmd::bad("faces.groupUnnamed", format!("Cannot save unnamed groups: {e}")))?;
        }
        g.saved = true;
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strict grouping must resist similarity chains while retaining every input face.
    #[test]
    fn fixed_representative_prevents_similarity_chains_and_keeps_every_face() {
        let mut g = Grouping {
            tag: String::new(),
            strict: true,
            faces: Vec::new(),
            groups: Vec::new(),
            next: 0,
            candidate: 0,
            example: 0,
            score: Score::default(),
            best: None,
            runner_up: -1.0,
            unindexed: 0,
            saved: false,
        };
        // Adjacent directions match, but 0° and 60° must not end up together.
        for (i, angle) in [0.0_f32, 30.0, 60.0, 180.0].iter().enumerate() {
            let radians = angle.to_radians();
            g.faces.push(Face {
                photo: i as u64,
                index: 0,
                rect: Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 },
                embedding: Arc::from([radians.cos(), radians.sin()]),
            });
        }
        g.step(0.8, Duration::from_secs(1));
        assert_eq!(g.groups.iter().map(|g| g.members.clone()).collect::<Vec<_>>(), vec![vec![0, 1], vec![2], vec![3]]);
        assert_eq!(g.next, 4);
    }

    /// Grouping scores must match the existing named-person suggestion score.
    #[test]
    fn group_scores_match_the_original_person_suggestions() {
        let gallery = [vec![1.0, 0.0], vec![0.8, 0.6], vec![0.6, 0.8]];
        let query = [0.0, 1.0];
        let refs: Vec<_> = gallery.iter().map(|v| ("Person", v.as_slice())).collect();
        let mut score = Score::default();
        for example in &gallery {
            score.add(cosine(&query, example).unwrap());
        }
        assert_eq!(score.mean(), Some(lightcraft_faces::matching::rank(&query, &refs)[0].score));
        assert!(score.mean().unwrap() > cosine(&query, &gallery[0]).unwrap());
    }

    /// Broad galleries should retain pose variation beyond the first representative.
    #[test]
    fn broad_groups_use_their_members_instead_of_only_the_first_face() {
        let mut g = Grouping {
            tag: String::new(),
            strict: false,
            faces: Vec::new(),
            groups: Vec::new(),
            next: 0,
            candidate: 0,
            example: 0,
            score: Score::default(),
            best: None,
            runner_up: -1.0,
            unindexed: 0,
            saved: false,
        };
        for (i, angle) in [0.0_f32, 30.0, 60.0].iter().enumerate() {
            let radians = angle.to_radians();
            g.faces.push(Face {
                photo: i as u64,
                index: 0,
                rect: Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 },
                embedding: Arc::from([radians.cos(), radians.sin()]),
            });
        }
        g.step(0.65, Duration::from_secs(1));
        assert_eq!(g.groups.len(), 1, "the third face matches the two-example gallery but not its first face");
        assert_eq!(g.groups[0].members, vec![0, 1, 2]);
    }
}
