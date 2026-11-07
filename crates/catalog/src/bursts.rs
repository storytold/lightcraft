//! Bursts: the look-alike shots Assisted Culling groups ([`crate::Analysis::group`]), in the
//! order they are shown and arranged — the pick first.
//!
//! A burst culling made is ordered by what culling found: its best photo, then the sharper
//! shots, then capture time. Once the person arranges a burst by hand every member carries its
//! place ([`crate::Analysis::rank`]), and that order is kept.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use crate::{Catalog, Photo, PhotoId};

/// A photo's place in its burst: hand-placed ones by their place, then the best, the sharper,
/// the earlier (a time that doesn't read last), the lower id.
fn order_key(p: &Photo) -> (u32, bool, Reverse<i64>, bool, &str, PhotoId) {
    let a = p.analysis.unwrap_or_default();
    let when = p.captured.as_deref().and_then(wall_clock);
    (a.rank.unwrap_or(u32::MAX), !a.best, Reverse(sharpness_key(a.sharpness)), when.is_none(), when.unwrap_or(""), p.id)
}

/// Sharpness (0..100) in thousandths: a fixed-point key keeps the sort total. `as` saturates, so
/// an infinity is the sharpest or softest there is, and a NaN counts as 0.
fn sharpness_key(sharpness: f32) -> i64 {
    (sharpness * 1000.0) as i64
}

/// The local wall-clock part of an ISO 8601 capture time (`2026-05-01T10:00:00`, with any
/// fraction of a second), or `None` when it isn't one. Its digits stand in fixed places, so as
/// text it sorts in time order. A zone suffix is left out, as culling's burst gap does: a burst's
/// shots are seconds apart from one camera and share their zone.
fn wall_clock(iso: &str) -> Option<&str> {
    let b = iso.as_bytes();
    let digits = |from: usize, to: usize| b.get(from..to).is_some_and(|d| d.iter().all(u8::is_ascii_digit));
    let at = |i: usize, c: u8| b.get(i) == Some(&c);
    let shape = digits(0, 4) && at(4, b'-') && digits(5, 7) && at(7, b'-') && digits(8, 10) && at(10, b'T');
    if !(shape && digits(11, 13) && at(13, b':') && digits(14, 16) && at(16, b':') && digits(17, 19)) {
        return None;
    }
    let end = iso[19..].find(|c: char| c != '.' && !c.is_ascii_digit()).map_or(iso.len(), |i| 19 + i);
    Some(&iso[..end])
}

impl Catalog {
    /// Every burst: its number → its photos (deleted ones left out), pick first.
    pub fn burst_index(&self) -> HashMap<u32, Vec<PhotoId>> {
        let mut by: HashMap<u32, Vec<&Photo>> = HashMap::new();
        for p in self.photos.values().filter(|p| !p.deleted) {
            if let Some(g) = p.analysis.and_then(|a| a.group) {
                by.entry(g).or_default().push(&**p);
            }
        }
        by.into_iter()
            .map(|(g, mut photos)| {
                photos.sort_by(|a, b| order_key(a).cmp(&order_key(b)));
                (g, photos.into_iter().map(|p| p.id).collect())
            })
            .collect()
    }

    /// The photos of burst `group`, pick first (empty when there is no such burst).
    pub fn burst(&self, group: u32) -> Vec<PhotoId> {
        self.burst_index().remove(&group).unwrap_or_default()
    }

    /// The bursts `ids` meet, in the order their first photo appears in `ids`, each pick first.
    /// Only photos in `scope` count as members (the photos of a view and the hidden members of
    /// its collapsed stacks, say); `None` takes every photo of the burst.
    pub fn bursts_among(&self, ids: &[PhotoId], scope: Option<&HashSet<PhotoId>>) -> Vec<(u32, Vec<PhotoId>)> {
        let mut index = self.burst_index();
        let mut out = Vec::new();
        for id in ids {
            let Some(g) = self.photo(*id).and_then(|p| p.analysis).and_then(|a| a.group) else { continue };
            if let Some(mut photos) = index.remove(&g) {
                if let Some(scope) = scope {
                    photos.retain(|p| scope.contains(p));
                }
                if !photos.is_empty() {
                    out.push((g, photos));
                }
            }
        }
        out
    }

    /// A burst number no photo uses (deleted ones included, so Undo never brings back a clash).
    pub fn next_burst_id(&self) -> u32 {
        self.photos.values().filter_map(|p| p.analysis.and_then(|a| a.group)).max().unwrap_or(0) + 1
    }
}

#[cfg(test)]
mod tests {
    use crate::{Analysis, Catalog, Op, Photo, PhotoId, Source};

    fn add(c: &mut Catalog, when: &str, a: Option<Analysis>) -> PhotoId {
        let id = c.alloc_photo_id();
        let mut p = Photo::new(id, Source::File { path: format!("/p/{}.jpg", id.0) }, "p.jpg", "JPEG", 4, 3, "2026-01-01");
        p.captured = Some(when.into());
        p.analysis = a;
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        id
    }

    fn shot(group: u32, sharpness: f32, best: bool, rank: Option<u32>) -> Option<Analysis> {
        Some(Analysis { sharpness, group: Some(group), best, rank, ..Default::default() })
    }

    #[test]
    fn a_culled_burst_shows_its_best_then_the_sharper_shots() {
        let mut c = Catalog::new();
        let a = add(&mut c, "2026-05-01T10:00:00", shot(1, 20.0, false, None));
        let b = add(&mut c, "2026-05-01T10:00:01", shot(1, 80.0, true, None));
        let d = add(&mut c, "2026-05-01T10:00:02", shot(1, 50.0, false, None));
        let loose = add(&mut c, "2026-05-01T10:00:03", Some(Analysis::default()));
        assert_eq!(c.burst(1), vec![b, d, a]);
        assert!(c.burst(2).is_empty());
        assert_eq!(c.bursts_among(&[a, loose, d], None), vec![(1, vec![b, d, a])]);
        assert_eq!(c.next_burst_id(), 2);
    }

    #[test]
    fn a_burst_arranged_by_hand_keeps_its_order() {
        let mut c = Catalog::new();
        // the pick by hand is the softest shot
        let a = add(&mut c, "2026-05-01T10:00:00", shot(3, 20.0, true, Some(0)));
        let b = add(&mut c, "2026-05-01T10:00:01", shot(3, 80.0, false, Some(2)));
        let d = add(&mut c, "2026-05-01T10:00:02", shot(3, 50.0, false, Some(1)));
        let other = add(&mut c, "2026-05-01T11:00:00", shot(7, 50.0, true, None));
        let other2 = add(&mut c, "2026-05-01T11:00:01", shot(7, 40.0, false, None));
        assert_eq!(c.burst(3), vec![a, d, b]);
        // in the order the view meets them; a scope keeps a burst's members within it
        assert_eq!(c.bursts_among(&[other, b], None), vec![(7, vec![other, other2]), (3, vec![a, d, b])]);
        let scope = [b, d, other].into_iter().collect();
        assert_eq!(c.bursts_among(&[b, other], Some(&scope)), vec![(3, vec![d, b]), (7, vec![other])]);
        assert_eq!(c.next_burst_id(), 8);
        // deleted photos are no member
        c.apply(Op::SetDeleted { id: d, deleted: true }).unwrap();
        assert_eq!(c.burst(3), vec![a, b]);
        assert_eq!(c.next_burst_id(), 8);
    }

    #[test]
    fn a_sharpness_that_does_not_measure_sorts_without_a_panic() {
        let mut c = Catalog::new();
        let nan = add(&mut c, "2026-05-01T10:00:00", shot(4, f32::NAN, false, None));
        let soft = add(&mut c, "2026-05-01T10:00:01", shot(4, 10.0, false, None));
        let low = add(&mut c, "2026-05-01T10:00:02", shot(4, f32::NEG_INFINITY, false, None));
        let high = add(&mut c, "2026-05-01T10:00:03", shot(4, f32::INFINITY, false, None));
        // a NaN counts as 0; the infinities saturate at either end
        assert_eq!(c.burst(4), vec![high, soft, nan, low]);
    }

    #[test]
    fn capture_times_sort_by_their_wall_clock() {
        let mut c = Catalog::new();
        let s = |c: &mut Catalog, when: &str| add(c, when, shot(5, 50.0, false, None));
        let unread = s(&mut c, "1. Mai 2026");
        let second = s(&mut c, "2026-05-01T10:00:01+02:00");
        let half = s(&mut c, "2026-05-01T10:00:00.5");
        let missing = s(&mut c, "");
        let tenth = s(&mut c, "2026-05-01T10:00:00.12Z");
        // fractions of a second in order, the zone left out, a time that doesn't read last by id
        assert_eq!(c.burst(5), vec![tenth, half, second, unread, missing]);
        assert_eq!(super::wall_clock("2026-05-01T10:00:00.12-07:00"), Some("2026-05-01T10:00:00.12"));
        assert_eq!(super::wall_clock("2026-05-01 10:00:00"), None);
    }
}
