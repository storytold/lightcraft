//! Budgeted More matching. Confirmations extend the gallery without repeating old comparisons.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lightcraft_faces::matching::cosine;
use lightcraft_geom::Rect;
use rayon::prelude::*;

pub(crate) type Key = (u64, usize);

pub(crate) struct Face {
    pub key: Key,
    pub rect: Rect,
    pub view: Rect,
    pub embedding: Arc<[f32]>,
}

/// Keep spare matches so removing even a best example can promote the next one immediately.
/// `omitted_max` bounds discarded scores; it prevents stale low spares from being trusted
/// after several removals and additions have exhausted the original shortlist.
#[derive(Default)]
struct BestTwo {
    matches: Vec<(Key, f32)>,
    omitted_max: Option<f32>,
}

impl BestTwo {
    fn add(&mut self, key: Key, score: f32) {
        const KEEP: usize = 8;
        let index = self.matches.partition_point(|(_, other)| *other >= score);
        if index < KEEP {
            if self.matches.len() == KEEP
                && let Some((_, discarded)) = self.matches.pop()
            {
                self.omitted_max = Some(self.omitted_max.map_or(discarded, |old| old.max(discarded)));
            }
            self.matches.insert(index, (key, score));
        } else {
            self.omitted_max = Some(self.omitted_max.map_or(score, |old| old.max(score)));
        }
    }

    fn mean(&self) -> Option<f32> {
        let first = self.matches.first()?.1;
        Some(self.matches.get(1).map_or(first, |(_, second)| (first + second) / 2.0))
    }

    fn remove(&mut self, removed: &HashSet<Key>) -> bool {
        self.matches.retain(|(key, _)| !removed.contains(key));
        self.omitted_max.is_none_or(|bound| self.matches.get(1).is_some_and(|(_, second)| *second >= bound))
    }
}

struct Candidate {
    face: Face,
    best: BestTwo,
    compared: usize,
}

impl Candidate {
    fn compare(&mut self, example: &Face) {
        if let Some(score) = cosine(&self.face.embedding, &example.embedding) {
            self.best.add(example.key, score);
        }
        self.compared += 1;
    }
}

pub(crate) struct Suggestions {
    pub name: String,
    pub tag: String,
    pub stamp: Option<(u64, u64)>,
    gallery: Vec<Face>,
    candidates: Vec<Candidate>,
    next: usize,
    // Best fully scored candidates so far, published between work slices.
    published: Vec<(Key, f32)>,
    published_next: usize,
    deferred: bool,
}

impl Suggestions {
    pub fn new(name: String, tag: String) -> Self {
        Self {
            name,
            tag,
            stamp: None,
            gallery: Vec::new(),
            candidates: Vec::new(),
            next: 0,
            published: Vec::new(),
            published_next: 0,
            deferred: false,
        }
    }

    /// Removing an example only invalidates candidates whose shortlist no longer proves their best two.
    /// Keep the order of surviving examples so in-progress candidates can resume their cursor.
    pub fn refresh(&mut self, gallery: Vec<Face>, candidates: Vec<Face>, stamp: (u64, u64)) {
        self.refresh_with(gallery, candidates, stamp, false);
    }

    /// New scan results wait for the current cohort to finish. Removals and replacements
    /// still invalidate current faces immediately; incoming work cannot starve publication.
    pub fn refresh_incremental(&mut self, gallery: Vec<Face>, candidates: Vec<Face>, stamp: (u64, u64)) {
        self.refresh_with(gallery, candidates, stamp, true);
    }

    pub fn refresh_needed(&self, stamp: (u64, u64), synchronous: bool) -> bool {
        self.stamp != Some(stamp) || (self.deferred && (self.complete() || synchronous))
    }

    fn refresh_with(&mut self, gallery: Vec<Face>, candidates: Vec<Face>, stamp: (u64, u64), incremental: bool) {
        let freeze_additions = incremental && self.stamp.is_some() && !self.complete();
        self.deferred = false;
        let mut examples: HashMap<_, _> = gallery.into_iter().map(|face| (face.key, face)).collect();
        let mut removed = HashSet::new();
        let mut retained_prefix = Vec::with_capacity(self.gallery.len().saturating_add(1));
        retained_prefix.push(0usize);
        let mut retained = Vec::with_capacity(self.gallery.len());
        for old in std::mem::take(&mut self.gallery) {
            if examples.get(&old.key).is_some_and(|new| same_face(&old, new)) {
                if let Some(new) = examples.remove(&old.key) {
                    retained.push(new);
                }
            } else {
                removed.insert(old.key);
            }
            retained_prefix.push(retained.len());
        }
        self.gallery = retained;
        let added = !examples.is_empty() && !freeze_additions;
        if !removed.is_empty() || added {
            // Published scores may predate a partially processed gallery extension.
            self.published.clear();
        }
        let mut added: Vec<_> = examples.into_values().collect();
        if freeze_additions && !added.is_empty() {
            self.deferred = true;
            added.clear();
        }
        added.sort_by_key(|face| face.key);
        self.gallery.extend(added);
        let mut previous: HashMap<_, _> = std::mem::take(&mut self.candidates).into_iter().map(|c| (c.face.key, c)).collect();
        self.candidates = candidates
            .into_iter()
            .filter_map(|face| {
                if freeze_additions && !previous.contains_key(&face.key) {
                    self.deferred = true;
                    return None;
                }
                let old =
                    previous.remove(&face.key).filter(|old| same_face(&old.face, &face)).and_then(|mut old| old.best.remove(&removed).then_some(old));
                Some(match old {
                    Some(old) => Candidate { face, compared: retained_prefix.get(old.compared).copied().unwrap_or(0), ..old },
                    None => Candidate { face, best: BestTwo::default(), compared: 0 },
                })
            })
            .collect();
        self.candidates.sort_by_key(|candidate| candidate.face.key);
        // A published tile must still be unnamed and represent the same face content.
        self.published.retain(|(key, _)| {
            self.candidates
                .binary_search_by_key(key, |candidate| candidate.face.key)
                .ok()
                .and_then(|i| self.candidates.get(i))
                .is_some_and(|c| c.compared > 0)
        });
        self.next = self.candidates.iter().position(|candidate| candidate.compared < self.gallery.len()).unwrap_or(self.candidates.len());
        self.stamp = Some(stamp);
        self.published_next = 0;
    }

    /// A cursor can yield between individual examples, even when a person has thousands of faces.
    pub fn step(&mut self, budget: Option<Duration>) {
        // Synchronous command clients can use every core to finish the remaining comparisons.
        let Some(budget) = budget else {
            self.candidates.par_iter_mut().for_each(|candidate| {
                for example in self.gallery.iter().skip(candidate.compared) {
                    candidate.compare(example);
                }
            });
            self.next = self.candidates.len();
            self.publish_progress();
            return;
        };
        let start = Instant::now();
        while !self.complete() {
            if start.elapsed() >= budget {
                break;
            }
            // Checking the clock for every dot product costs more than the best-two bookkeeping.
            // A small batch still yields inside a large gallery without paying that overhead per pair.
            for _ in 0..32 {
                if self.complete() {
                    break;
                }
                self.compare_next();
            }
        }
        self.publish_progress();
    }

    fn compare_next(&mut self) {
        let Some(candidate) = self.candidates.get_mut(self.next) else { return };
        if let Some(example) = self.gallery.get(candidate.compared) {
            candidate.compare(example);
        } else {
            self.next += 1;
        }
    }

    fn publish_progress(&mut self) {
        if self.next == self.published_next {
            return;
        }
        let mut scores: HashMap<_, _> = self.published.drain(..).collect();
        for candidate in self.candidates.get(self.published_next..self.next).unwrap_or_default() {
            if let Some(score) = candidate.best.mean() {
                scores.insert(candidate.face.key, score);
            }
        }
        self.published = scores.into_iter().collect();
        let order = |a: &(Key, f32), b: &(Key, f32)| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0));
        // faces.person accepts at most 1000 suggestions, including for synchronous callers.
        if self.published.len() > 1000 {
            self.published.select_nth_unstable_by(1000, order);
            self.published.truncate(1000);
        }
        self.published.sort_by(order);
        self.published_next = self.next;
    }

    pub fn complete(&self) -> bool {
        self.next == self.candidates.len()
    }

    pub fn current_complete(&self) -> bool {
        self.complete() && !self.deferred
    }

    pub fn progress(&self) -> (usize, usize) {
        (self.next, self.candidates.len())
    }

    pub fn results(&self, threshold: f32, limit: usize) -> Vec<(&Face, f32)> {
        self.published
            .iter()
            .filter(|(_, score)| *score >= threshold)
            .take(limit)
            .filter_map(|(key, score)| {
                let index = self.candidates.binary_search_by_key(key, |candidate| candidate.face.key).ok()?;
                Some((&self.candidates.get(index)?.face, *score))
            })
            .collect()
    }
}

fn same_face(a: &Face, b: &Face) -> bool {
    a.rect == b.rect && Arc::ptr_eq(&a.embedding, &b.embedding)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(id: u64, embedding: Arc<[f32]>) -> Face {
        Face { key: (id, 0), rect: Rect { x0: 0.1, y0: 0.1, x1: 0.5, y1: 0.5 }, view: Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 }, embedding }
    }

    #[test]
    fn live_scan_cannot_starve_a_cohort_or_hide_completed_suggestions() {
        let embedding: Arc<[f32]> = Arc::from([1.0, 0.0]);
        let gallery = || (0..100).map(|i| face(i, embedding.clone())).collect();
        let candidates = |count| (1000..1000 + count).map(|i| face(i, embedding.clone())).collect();
        let mut job = Suggestions::new("person".into(), "model".into());
        job.refresh_incremental(gallery(), candidates(10), (1, 1));
        let mut first_published = None;
        let mut completed = None;
        let mut count = 10;
        for frame in 1..=64 {
            count = 10 + frame * 5;
            job.refresh_incremental(gallery(), candidates(count), (1, frame));
            for _ in 0..64 {
                job.compare_next();
            }
            // Yield without doing more comparisons, just as the real frame budget does.
            job.step(Some(Duration::ZERO));
            if !job.results(0.5, 1000).is_empty() && first_published.is_none() {
                first_published = Some(frame);
            }
            if job.complete() {
                completed = Some(frame);
                break;
            }
        }
        assert!(first_published.is_some_and(|frame| frame <= 2), "publish fully scored faces before the full ranking finishes");
        assert!(completed.is_some_and(|frame| frame <= 16), "new arrivals must not extend the active cohort indefinitely");
        assert!(!job.current_complete(), "newly indexed faces still need their next pass");
        let stamp = job.stamp.unwrap();
        assert!(job.refresh_needed(stamp, false), "deferred faces must be picked up even if scanning now stops");
        job.refresh_incremental(gallery(), candidates(count), stamp);
        job.step(None);
        assert!(job.current_complete());
        assert_eq!(job.results(0.5, 1000).len(), count as usize);
        assert!(!job.refresh_needed(stamp, false));
    }

    #[test]
    fn synchronous_query_can_include_deferred_faces_before_the_active_pass_finishes() {
        let embedding: Arc<[f32]> = Arc::from([1.0, 0.0]);
        let mut job = Suggestions::new("person".into(), "model".into());
        job.refresh_incremental(vec![face(1, embedding.clone())], vec![face(10, embedding.clone())], (1, 1));
        job.refresh_incremental(vec![face(1, embedding.clone())], vec![face(10, embedding.clone()), face(11, embedding.clone())], (1, 2));
        assert_eq!(job.progress().1, 1, "the active cohort stays fixed");
        assert!(!job.refresh_needed((1, 2), false));
        assert!(job.refresh_needed((1, 2), true));
        job.refresh(vec![face(1, embedding.clone())], vec![face(10, embedding.clone()), face(11, embedding)], (1, 2));
        job.step(None);
        assert!(job.current_complete());
        assert_eq!(job.results(0.5, 1000).len(), 2);
    }

    #[test]
    fn budgeted_scores_equal_original_matching_and_keep_tie_order() {
        let gallery: Vec<Arc<[f32]>> = vec![Arc::from([1.0, 0.0]), Arc::from([0.8, 0.6]), Arc::from([0.6, 0.8])];
        let refs: Vec<_> = gallery.iter().map(|e| ("person", e.as_ref())).collect();
        let mut job = Suggestions::new("person".into(), "model".into());
        job.refresh(
            gallery.iter().enumerate().map(|(i, e)| face(i as u64, e.clone())).collect(),
            vec![face(10, Arc::from([0.0, 1.0])), face(9, Arc::from([0.0, 1.0]))],
            (1, 1),
        );
        job.step(Some(Duration::ZERO));
        assert!(!job.complete(), "zero work budget must yield with work remaining");
        assert!(job.results(-1.0, 100).is_empty(), "never publish an unscored face");
        job.compare_next();
        assert_eq!(job.candidates[0].compared, 1, "yielding is possible inside one gallery");
        job.step(None);
        let result = job.results(-1.0, 100);
        assert_eq!(result.iter().map(|(f, _)| f.key.0).collect::<Vec<_>>(), vec![9, 10]);
        assert_eq!(result[0].1, lightcraft_faces::matching::rank(&[0.0, 1.0], &refs)[0].score);
        assert_eq!(job.results(0.99, 100).len(), 0);
        assert_eq!(job.results(-1.0, 1).len(), 1);
    }

    #[test]
    fn confirmation_reuses_old_comparisons_and_removes_named_tiles_immediately() {
        let a: Arc<[f32]> = Arc::from([1.0, 0.0]);
        let b: Arc<[f32]> = Arc::from([0.8, 0.6]);
        let c: Arc<[f32]> = Arc::from([0.6, 0.8]);
        let mut job = Suggestions::new("person".into(), "model".into());
        job.refresh(vec![face(1, a.clone())], vec![face(2, b.clone()), face(3, c.clone())], (1, 1));
        job.step(None);
        // Newly confirmed example sorts before the old one: the cached gallery order must survive.
        job.refresh(vec![face(0, b.clone()), face(1, a.clone())], vec![face(3, c.clone())], (2, 1));
        assert_eq!(job.candidates[0].compared, 1);
        assert!(job.results(-1.0, 100).is_empty(), "only fully rescored faces may enter the new gallery ranking");
        job.compare_next();
        assert_eq!(job.candidates[0].compared, 2, "only the new example needed comparison");
        job.step(None);
        let refs = [("person", a.as_ref()), ("person", b.as_ref())];
        assert_eq!(job.results(-1.0, 100)[0].1, lightcraft_faces::matching::rank(&c, &refs)[0].score);
        // Undo removes an example; its saved score is dropped without recomputing the survivor.
        job.refresh(vec![face(1, a.clone())], vec![face(2, b), face(3, c.clone())], (3, 1));
        assert!(job.results(-1.0, 100).is_empty());
        assert_eq!(job.candidates.iter().find(|candidate| candidate.face.key.0 == 3).unwrap().compared, 1);
        assert_eq!(
            job.candidates.iter().find(|candidate| candidate.face.key.0 == 2).unwrap().compared,
            0,
            "the newly unnamed face needs its first match"
        );
        job.step(None);
        // A replacement embedding at the same photo/index is a different face.
        job.refresh(vec![face(1, a)], vec![face(3, Arc::from([0.0, 1.0]))], (4, 2));
        assert!(job.results(-1.0, 100).is_empty());
        assert_eq!(job.candidates[0].compared, 0);
    }

    #[test]
    fn scanning_can_extend_an_in_progress_job_without_restarting_old_pairs() {
        let a: Arc<[f32]> = Arc::from([1.0, 0.0]);
        let b: Arc<[f32]> = Arc::from([0.8, 0.6]);
        let mut job = Suggestions::new("person".into(), "model".into());
        job.refresh(vec![face(1, a.clone()), face(2, b.clone())], vec![face(3, a.clone())], (1, 1));
        job.compare_next();
        job.refresh(vec![face(1, a.clone()), face(2, b.clone())], vec![face(3, a.clone()), face(4, b)], (1, 2));
        assert_eq!(job.candidates[0].compared, 1);
        assert_eq!(job.candidates[1].compared, 0);
        job.step(None);
        assert_eq!(job.results(-1.0, 100).len(), 2);
    }

    #[test]
    fn invalid_examples_match_reference_filtering_and_empty_galleries_finish() {
        let examples: Vec<Arc<[f32]>> = vec![Arc::from([f32::NAN, 0.0]), Arc::from([1.0]), Arc::from([0.8, 0.6])];
        let mut job = Suggestions::new("person".into(), "model".into());
        job.refresh(examples.iter().enumerate().map(|(i, e)| face(i as u64, e.clone())).collect(), vec![face(10, Arc::from([1.0, 0.0]))], (0, 0));
        job.step(None);
        let refs: Vec<_> = examples.iter().map(|e| ("person", e.as_ref())).collect();
        assert_eq!(job.results(-1.0, 100)[0].1, lightcraft_faces::matching::rank(&[1.0, 0.0], &refs)[0].score);
        job.refresh(Vec::new(), vec![face(10, Arc::from([1.0, 0.0]))], (1, 0));
        job.step(None);
        assert!(job.complete());
        assert!(job.results(-1.0, 100).is_empty());
    }

    #[test]
    fn retained_shortlists_match_reference_for_every_removal_subset_and_partial_cursor() {
        let examples: Vec<Arc<[f32]>> = (0..10)
            .map(|i| {
                let angle = i as f32 * 0.2;
                Arc::from([angle.cos(), angle.sin()])
            })
            .collect();
        let queries: Vec<Arc<[f32]>> = vec![Arc::from([1.0, 0.0]), Arc::from([0.0, 1.0]), Arc::from([0.8, 0.6])];
        for mask in 0..1024 {
            let mut job = Suggestions::new("person".into(), "model".into());
            let candidates = || queries.iter().enumerate().map(|(i, e)| face(100 + i as u64, e.clone())).collect();
            job.refresh(examples.iter().enumerate().map(|(i, e)| face(i as u64, e.clone())).collect(), candidates(), (1, 1));
            for _ in 0..mask % 40 {
                job.compare_next();
            }
            let keep: Vec<_> = examples.iter().enumerate().filter(|(i, _)| mask & (1 << i) == 0).collect();
            job.refresh(keep.iter().map(|(i, e)| face(*i as u64, (*e).clone())).collect(), candidates(), (2, 1));
            job.step(None);
            let refs: Vec<_> = keep.iter().map(|(_, e)| ("person", e.as_ref())).collect();
            let result = job.results(-2.0, 100);
            for (i, query) in queries.iter().enumerate() {
                let expected = lightcraft_faces::matching::rank(query, &refs).first().map(|r| r.score);
                let actual = result.iter().find(|(f, _)| f.key.0 == 100 + i as u64).map(|(_, score)| *score);
                assert_eq!(actual, expected, "removal mask {mask}, query {i}");
            }
        }
    }

    #[test]
    fn adding_low_spares_does_not_hide_better_discarded_examples_after_many_removals() {
        let mut best = BestTwo::default();
        for i in 0..10 {
            best.add((i, 0), 1.0 - i as f32 * 0.1);
        }
        assert!(best.remove(&(0..6).map(|i| (i, 0)).collect()));
        best.add((20, 0), -0.5);
        assert!(!best.remove(&HashSet::from([(6, 0)])), "discarded examples may outrank the new low spare; require a fresh score");
    }

    #[test]
    fn removal_reuses_scores_except_for_candidates_that_used_the_removed_face() {
        let a: Arc<[f32]> = Arc::from([1.0, 0.0]);
        let b: Arc<[f32]> = Arc::from([0.0, 1.0]);
        let c: Arc<[f32]> = Arc::from([0.8, 0.6]);
        let mut job = Suggestions::new("person".into(), "model".into());
        job.refresh(vec![face(1, a.clone()), face(2, b.clone()), face(3, c.clone())], vec![face(10, a.clone()), face(11, b.clone())], (1, 1));
        job.step(None);
        job.refresh(vec![face(1, a.clone()), face(3, c.clone())], vec![face(10, a.clone()), face(11, b.clone()), face(2, b.clone())], (2, 1));
        assert_eq!(
            job.candidates.iter().find(|c| c.face.key.0 == 10).unwrap().compared,
            2,
            "removing a non-contributing example costs no old comparisons"
        );
        assert_eq!(
            job.candidates.iter().find(|c| c.face.key.0 == 11).unwrap().compared,
            2,
            "a spare match replaces the removed best without rescoring"
        );
        job.step(None);
        let refs = [("person", a.as_ref()), ("person", c.as_ref())];
        for (f, score) in job.results(-1.0, 100) {
            assert_eq!(score, lightcraft_faces::matching::rank(&f.embedding, &refs)[0].score);
        }
        // Remove an unvisited example from an unfinished candidate; the prefix cursor follows
        // surviving examples, rather than skipping or comparing the same example twice.
        let mut job = Suggestions::new("person".into(), "model".into());
        job.refresh(vec![face(1, a.clone()), face(2, b.clone()), face(3, c.clone())], vec![face(10, a.clone())], (1, 1));
        job.compare_next();
        job.refresh(vec![face(1, a.clone()), face(3, c)], vec![face(10, a)], (2, 1));
        assert_eq!(job.candidates[0].compared, 1);
        job.step(None);
        assert_eq!(job.results(-1.0, 100)[0].1, 0.9);
    }

    #[test]
    fn removing_one_of_7000_faces_yields_and_rebuilds_scores_without_the_removed_example() {
        let ordinary: Arc<[f32]> = Arc::from([0.0, 1.0]);
        let removed: Arc<[f32]> = Arc::from([1.0, 0.0]);
        let gallery = || (0..6999).map(|id| face(id, ordinary.clone())).chain(std::iter::once(face(6999, removed.clone()))).collect();
        let mut job = Suggestions::new("person".into(), "model".into());
        job.refresh(gallery(), vec![face(10_000, removed.clone())], (1, 1));
        job.step(None);
        assert_eq!(job.results(-1.0, 100)[0].1, 0.5);
        // Removing an example invalidates the aggregate. No stale suggestion may survive.
        job.refresh((0..6999).map(|id| face(id, ordinary.clone())).collect(), vec![face(6999, removed.clone()), face(10_000, removed)], (2, 1));
        assert!(job.results(-1.0, 100).is_empty());
        job.step(Some(Duration::ZERO));
        assert!(!job.complete(), "a refresh must yield instead of scoring every candidate synchronously");
        job.compare_next();
        assert_eq!(job.candidates[0].compared, 1, "work can yield within a 7000-face gallery");
        job.step(None);
        let result = job.results(-1.0, 100);
        assert_eq!(result.len(), 2, "the removed face is unnamed again");
        assert!(result.iter().all(|(_, score)| *score == 0.0), "the removed example must not contribute to any score");
    }
}
