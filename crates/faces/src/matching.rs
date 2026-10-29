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
/// profile) less likely to be suggested as them, at the cost of fewer suggestions for people with few photos.
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
    let d: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    d.is_finite().then_some(d)
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
