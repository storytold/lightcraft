//! Who does this face look like? Rank the people in a gallery of named faces by how close their faces are,
//! and suggest one only when the match is strong *and* clearly ahead of the runner-up.
//!
//! Embeddings are unit vectors, so closeness is the dot product (cosine similarity). A person's score is the
//! mean of their best few faces (one stray, wrongly named face cannot make a stranger look like them). The
//! result is only ever a suggestion for the user to confirm: it is better to leave a face unnamed than to
//! name it wrongly.

use std::cmp::Ordering;

/// How many of a person's best matching faces are averaged into their score, unless the user chose otherwise
/// (Settings ▸ Faces). More faces make a stranger who happens to match two odd photos of someone (blurred, in
/// profile) less likely to be suggested as them. A person with fewer faces than the count is scored on the ones they
/// have, so their score does not change.
pub const TOP_FACES: usize = 2;
/// The most faces a score may be based on.
pub const MAX_TOP_FACES: usize = 8;

#[derive(Clone, Debug, PartialEq)]
pub struct Ranked {
    pub name: String,
    pub score: f32,
    /// How many of their faces the score is based on.
    pub faces: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub name: String,
    pub score: f32,
    /// The next best person, and how far behind.
    pub runner_up: Option<(String, f32)>,
}

/// Cosine similarity of two unit vectors. `None` when their lengths differ or either has a non-finite number.
pub fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let d = dot(a, b);
    d.is_finite().then_some(d)
}

/// The dot product of two vectors of the same length, summed in eight lanes so the compiler can use the processor's
/// vector instructions (one running sum can't be: each addition waits for the one before). Some four times faster on
/// 512-long embeddings; the result differs from a plain running sum only in the last bits.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut lanes = [0f32; 8];
    let ((ca, ra), (cb, rb)) = (a.as_chunks::<8>(), b.as_chunks::<8>());
    for (x, y) in ca.iter().zip(cb) {
        for ((l, x), y) in lanes.iter_mut().zip(x).zip(y) {
            *l += x * y;
        }
    }
    lanes.iter().sum::<f32>() + ra.iter().zip(rb).map(|(x, y)| x * y).sum::<f32>()
}

/// Rank the people in `gallery` (a name and one face's embedding per entry; a person appears once per face),
/// best first. Names are compared case-insensitively and shown as first seen.
pub fn rank(query: &[f32], gallery: &[(&str, &[f32])]) -> Vec<Ranked> {
    rank_top(query, gallery, TOP_FACES)
}

/// [`rank`], with each person's score the mean of their `top` best matching faces (1 to [`MAX_TOP_FACES`]; a person
/// with fewer faces is scored on those they have).
pub fn rank_top(query: &[f32], gallery: &[(&str, &[f32])], top: usize) -> Vec<Ranked> {
    let top = top.clamp(1, MAX_TOP_FACES);
    let mut people: Vec<(String, String, Vec<f32>)> = Vec::new(); // (key, shown name, similarities)
    for (name, emb) in gallery {
        let Some(c) = cosine(query, emb) else { continue };
        let key = name.to_lowercase();
        match people.iter_mut().find(|p| p.0 == key) {
            Some(p) => p.2.push(c),
            None => people.push((key, (*name).to_string(), vec![c])),
        }
    }
    let mut out: Vec<Ranked> = people
        .into_iter()
        .map(|(_, name, mut sims)| {
            sims.sort_by(|a, b| b.partial_cmp(a).unwrap_or(Ordering::Equal));
            let top = sims.get(..sims.len().min(top)).unwrap_or(&[]);
            Ranked { name, score: top.iter().sum::<f32>() / top.len().max(1) as f32, faces: top.len() }
        })
        .collect();
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal).then_with(|| a.name.cmp(&b.name)));
    out
}

/// One person's score for `query`, as [`rank`] gives it, when all of `gallery` is that person: the mean of the `top`
/// (1 to 8) faces most like it, or of all of them when they have fewer. `None` when no face compares. Made for scoring
/// many faces against one person: no names to group by and nothing allocated, so it can run on every core.
pub fn person_score(query: &[f32], gallery: &[&[f32]], top: usize) -> Option<f32> {
    const MOST: usize = 8;
    let top = top.clamp(1, MOST);
    // the best similarities so far, highest first
    let mut best = [f32::NEG_INFINITY; MOST];
    let mut seen = 0usize;
    for face in gallery {
        let Some(c) = cosine(query, face) else { continue };
        seen += 1;
        if c > best[top - 1] {
            let mut i = top - 1;
            while i > 0 && best[i - 1] < c {
                best[i] = best[i - 1];
                i -= 1;
            }
            best[i] = c;
        }
    }
    let n = seen.min(top);
    (n > 0).then(|| best[..n].iter().sum::<f32>() / n as f32)
}

/// How close to a person's average face one of theirs must be to stand for them when looking for look-alikes.
pub const TYPICAL: f32 = 0.3;

/// A person's faces without the odd ones: those far from their average face ([`TYPICAL`]), mostly a face from behind
/// or in the dark named after whoever was in the photo, or someone else named by mistake. Such a face looks like every
/// other face of its kind in the library (backs of heads all look alike), so it would bring them all along as
/// look-alikes. All of them are kept when fewer than three would be left.
pub fn typical<'a>(faces: &[&'a [f32]]) -> Vec<&'a [f32]> {
    let Some(dim) = faces.first().map(|f| f.len()) else { return Vec::new() };
    let mut mean = vec![0f32; dim];
    for f in faces.iter().filter(|f| f.len() == dim) {
        for (m, v) in mean.iter_mut().zip(f.iter()) {
            *m += v;
        }
    }
    let norm = dot(&mean, &mean).sqrt();
    if !norm.is_finite() || norm == 0.0 {
        return faces.to_vec();
    }
    mean.iter_mut().for_each(|m| *m /= norm);
    let kept: Vec<&[f32]> = faces.iter().copied().filter(|f| cosine(f, &mean).is_some_and(|c| c >= TYPICAL)).collect();
    if kept.len() < 3 { faces.to_vec() } else { kept }
}

/// The best person if they pass both bars: a score of at least `threshold`, and at least `margin` ahead of the
/// next person (when there is one).
pub fn suggest(ranked: &[Ranked], threshold: f32, margin: f32) -> Option<Suggestion> {
    let best = ranked.first()?;
    let next = ranked.get(1);
    if best.score.is_nan() || best.score < threshold || next.is_some_and(|n| best.score - n.score < margin) {
        return None;
    }
    Some(Suggestion { name: best.name.clone(), score: best.score, runner_up: next.map(|n| (n.name.clone(), n.score)) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: &[f32]) -> Vec<f32> {
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / n).collect()
    }

    #[test]
    fn people_are_ranked_by_their_closest_faces() {
        let (a1, a2, b1) = (unit(&[1.0, 0.1, 0.0]), unit(&[0.9, 0.2, 0.0]), unit(&[0.0, 1.0, 0.2]));
        let gallery: Vec<(&str, &[f32])> = vec![("Ann", &a1), ("ann", &a2), ("Bob", &b1)];
        let ranked = rank(&unit(&[1.0, 0.0, 0.0]), &gallery);
        assert_eq!(ranked.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["Ann", "Bob"], "case-insensitive, shown as first seen");
        assert_eq!(ranked[0].faces, 2);
        assert!(ranked[0].score > 0.95 && ranked[1].score < 0.2);
        // a stray, wrongly named face cannot carry a person: Cy has one far face and one good one
        let (c_good, c_bad) = (unit(&[1.0, 0.0, 0.0]), unit(&[0.0, 0.0, 1.0]));
        let g2: Vec<(&str, &[f32])> = vec![("Cy", &c_good), ("Cy", &c_bad), ("Di", &a1), ("Di", &a2)];
        let r2 = rank(&unit(&[1.0, 0.0, 0.0]), &g2);
        assert_eq!(r2[0].name, "Di", "{r2:?}");
    }

    #[test]
    fn more_faces_in_a_score_keep_two_odd_photos_from_carrying_a_stranger() {
        // Eve matches two odd photos of Ann very well, but none of her usual ones; Bob looks like Eve on all of his
        let (odd1, odd2, usual) = (unit(&[1.0, 0.05, 0.0]), unit(&[1.0, 0.0, 0.05]), unit(&[0.0, 0.0, 1.0]));
        let (b1, b2, b3) = (unit(&[0.8, 0.6, 0.0]), unit(&[0.8, 0.0, 0.6]), unit(&[0.8, 0.3, 0.3]));
        let gallery: Vec<(&str, &[f32])> =
            vec![("Ann", &odd1), ("Ann", &odd2), ("Ann", &usual), ("Ann", &usual), ("Bob", &b1), ("Bob", &b2), ("Bob", &b3)];
        let eve = unit(&[1.0, 0.0, 0.0]);
        assert_eq!(rank(&eve, &gallery)[0].name, "Ann", "two faces: the odd photos carry Ann");
        let four = rank_top(&eve, &gallery, 4);
        assert_eq!(four[0].name, "Bob", "{four:?}");
        assert!(four.iter().find(|r| r.name == "Ann").is_some_and(|r| r.score < 0.6), "{four:?}");
        // the More faces path scores with person_score: the same mean, and a count above two counts too
        let ann: Vec<&[f32]> = vec![&odd1, &odd2, &usual, &usual];
        let ann4 = person_score(&eve, &ann, 4).unwrap();
        assert!((ann4 - four.iter().find(|r| r.name == "Ann").unwrap().score).abs() < 1e-6, "{ann4} against {four:?}");
        assert!(ann4 < person_score(&eve, &ann, 2).unwrap() - 0.4, "{ann4}");
        // out-of-range counts are clamped, never a panic or an empty score
        assert_eq!(rank_top(&eve, &gallery, 0), rank_top(&eve, &gallery, 1));
        assert_eq!(rank_top(&eve, &gallery, 1000), rank_top(&eve, &gallery, MAX_TOP_FACES));
        // a person with fewer faces than asked is scored on the ones they have
        let one: Vec<(&str, &[f32])> = vec![("Cy", &odd1)];
        assert!(rank_top(&eve, &one, 4)[0].score > 0.99);
        // and the count says how many faces the score is based on
        assert_eq!(rank_top(&eve, &gallery, 1).iter().find(|r| r.name == "Ann").map(|r| r.faces), Some(1));
        assert_eq!(rank_top(&eve, &one, 4)[0].faces, 1);
    }

    #[test]
    fn a_persons_score_is_the_one_ranking_gives_them() {
        // pseudo-random unit vectors, the same on every run
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 40) as f32 / (1u64 << 24) as f32 - 0.5
        };
        let mut face = || unit(&(0..16).map(|_| next()).collect::<Vec<_>>());
        let gallery: Vec<Vec<f32>> = (0..40).map(|_| face()).collect();
        let refs: Vec<&[f32]> = gallery.iter().map(Vec::as_slice).collect();
        let named: Vec<(&str, &[f32])> = refs.iter().map(|e| ("Ann", *e)).collect();
        for _ in 0..50 {
            let q = face();
            let ranked = rank(&q, &named);
            let score = person_score(&q, &refs, TOP_FACES).unwrap();
            assert!((score - ranked[0].score).abs() < 1e-6, "{score} against {}", ranked[0].score);
        }
        // fewer faces than asked, bad embeddings skipped, nothing to compare, and any count
        let q = face();
        assert!((person_score(&q, &refs[..1], 4).unwrap() - cosine(&q, refs[0]).unwrap()).abs() < 1e-6);
        let nan = [f32::NAN; 16];
        assert_eq!(person_score(&q, &[&nan[..], refs[0]], 2), person_score(&q, &refs[..1], 2));
        assert_eq!(person_score(&q, &[], 2), None);
        assert_eq!(person_score(&q, &[&nan[..]], 2), None);
        assert_eq!(person_score(&q, &refs, 0), person_score(&q, &refs, 1));
        assert_eq!(person_score(&q, &refs, 1000), person_score(&q, &refs, 8));
    }

    #[test]
    fn a_persons_odd_faces_are_left_out_unless_too_few_would_be_left() {
        let (a, b, c) = (unit(&[1.0, 0.1, 0.0]), unit(&[0.9, 0.2, 0.0]), unit(&[1.0, 0.0, 0.1]));
        let odd = unit(&[-0.2, 0.0, 1.0]);
        let faces: Vec<&[f32]> = vec![&a, &b, &odd, &c];
        assert_eq!(typical(&faces), vec![&a[..], &b[..], &c[..]], "the face from behind is left out");
        let few: Vec<&[f32]> = vec![&a, &odd];
        assert_eq!(typical(&few).len(), 2, "kept when fewer than three would be left");
        assert!(typical(&[]).is_empty());
        let zero = [0.0f32; 3];
        assert_eq!(typical(&[&zero[..], &zero[..], &zero[..]]).len(), 3, "nothing to average: all kept");
    }

    #[test]
    fn a_suggestion_needs_a_strong_match_clearly_ahead() {
        let r = |name: &str, score: f32| Ranked { name: name.into(), score, faces: 1 };
        // strong and clear
        let s = suggest(&[r("Ann", 0.62), r("Bob", 0.20)], 0.45, 0.08).unwrap();
        assert_eq!((s.name.as_str(), s.runner_up.as_ref().map(|x| x.0.as_str())), ("Ann", Some("Bob")));
        // strong but too close to the runner-up: nobody
        assert!(suggest(&[r("Ann", 0.62), r("Bob", 0.58)], 0.45, 0.08).is_none());
        // clear but not strong enough
        assert!(suggest(&[r("Ann", 0.40), r("Bob", 0.10)], 0.45, 0.08).is_none());
        // a single person has no runner-up to be ahead of
        assert!(suggest(&[r("Ann", 0.5)], 0.45, 0.08).is_some());
        assert!(suggest(&[], 0.45, 0.08).is_none());
        assert!(suggest(&[r("Ann", f32::NAN)], 0.45, 0.08).is_none());
    }

    #[test]
    fn mismatched_or_non_finite_embeddings_are_skipped_not_panics() {
        assert!(cosine(&[1.0], &[1.0, 0.0]).is_none());
        assert!(cosine(&[], &[]).is_none());
        assert!(cosine(&[f32::NAN], &[1.0]).is_none());
        let good = unit(&[1.0, 0.0]);
        let bad = [f32::NAN, 0.0];
        let short = [1.0f32];
        let gallery: Vec<(&str, &[f32])> = vec![("Ann", &good), ("Nan", &bad), ("Short", &short)];
        let ranked = rank(&good, &gallery);
        assert_eq!(ranked.len(), 1);
        assert!(rank(&[], &gallery).is_empty());
        assert!(rank(&good, &[]).is_empty());
    }
}
