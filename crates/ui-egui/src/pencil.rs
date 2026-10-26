//! Apple Pencil gestures: draw 1–5 anywhere on the photo and the selected photos get that star
//! rating; a tick picks them and a cross (one X-shaped stroke, or two crossing diagonals) rejects them. A stroke is a list of points; [`recognize_digit`] matches it against a few
//! templates per digit ($1-style: resample, centre, scale to a unit box, mean point distance).
//! Nothing here touches the screen or the session, so it is tested on synthetic strokes.

const N: usize = 48;
/// Mean cloud distance (unit box) a lifted digit may be from its template.
const ACCEPT_CLOUD: f32 = 0.16;
const TICK: u8 = 10;
const CROSS: u8 = 11;
/// Mean distance (in a unit box) a stroke may be from its template and still count.
const ACCEPT: f32 = 0.2;
/// The runner-up must be this much worse, or the stroke is ambiguous and ignored.
const MARGIN: f32 = 1.12;

type Pt = (f32, f32);

/// Templates in a unit box, y down, in the order people usually draw them.
fn templates() -> Vec<(u8, Vec<Pt>)> {
    vec![
        (2, vec![(0.0, 0.2), (0.2, 0.02), (0.6, 0.0), (0.9, 0.2), (0.8, 0.45), (0.5, 0.7), (0.0, 1.0), (1.0, 1.0)]),
        (2, vec![(0.0, 0.25), (0.3, 0.0), (0.75, 0.05), (0.95, 0.3), (0.6, 0.6), (0.05, 1.0), (0.5, 0.97), (1.0, 1.0)]),
        (3, vec![(0.0, 0.1), (0.5, 0.0), (0.9, 0.2), (0.5, 0.5), (0.9, 0.75), (0.5, 1.0), (0.0, 0.9)]),
        (3, vec![(0.05, 0.0), (0.85, 0.0), (0.45, 0.45), (0.9, 0.65), (0.8, 0.95), (0.4, 1.0), (0.0, 0.85)]),
        (4, vec![(0.8, 1.0), (0.8, 0.0), (0.0, 0.65), (1.0, 0.65)]),
        (4, vec![(0.65, 0.0), (0.0, 0.65), (1.0, 0.65)]),
        (5, vec![(0.9, 0.0), (0.1, 0.0), (0.05, 0.45), (0.5, 0.4), (0.9, 0.6), (0.8, 0.9), (0.4, 1.0), (0.0, 0.9)]),
        (5, vec![(0.85, 0.0), (0.15, 0.02), (0.1, 0.4), (0.6, 0.35), (0.95, 0.65), (0.7, 0.95), (0.3, 1.0), (0.0, 0.85)]),
        // a tick: a short stroke down to the right, then a long one up to the right
        (TICK, vec![(0.0, 0.55), (0.28, 0.85), (1.0, 0.0)]),
        (TICK, vec![(0.0, 0.5), (0.3, 0.9), (0.55, 0.45), (1.0, 0.0)]),
        // a cross drawn without lifting: down-right, back up, then down-left
        (CROSS, vec![(0.0, 0.0), (1.0, 1.0), (1.0, 0.0), (0.0, 1.0)]),
        (CROSS, vec![(0.0, 0.0), (1.0, 1.0), (0.5, 0.5), (1.0, 0.0), (0.0, 1.0)]),
    ]
}

fn path_len(p: &[Pt]) -> f32 {
    p.windows(2).map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1)).sum()
}

/// `n` points evenly spaced along the path.
fn resample(p: &[Pt], n: usize) -> Vec<Pt> {
    let total = path_len(p);
    if n < 2 || p.len() < 2 || total <= 0.0 || !total.is_finite() {
        return Vec::new();
    }
    let step = total / (n - 1) as f32;
    let mut out = vec![p[0]];
    let mut acc = 0.0;
    let mut prev = p[0];
    let mut i = 1;
    while i < p.len() && out.len() < n {
        let seg = (p[i].0 - prev.0).hypot(p[i].1 - prev.1);
        if seg > 0.0 && acc + seg >= step {
            let t = (step - acc) / seg;
            let q = (prev.0 + t * (p[i].0 - prev.0), prev.1 + t * (p[i].1 - prev.1));
            out.push(q);
            prev = q;
            acc = 0.0;
        } else {
            acc += seg;
            prev = p[i];
            i += 1;
        }
    }
    while out.len() < n {
        out.push(p[p.len() - 1]);
    }
    out
}

/// Centred and scaled so the longer side of the bounding box is 1 (the aspect is kept).
fn normalize(p: &[Pt]) -> Vec<Pt> {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for q in p {
        x0 = x0.min(q.0);
        y0 = y0.min(q.1);
        x1 = x1.max(q.0);
        y1 = y1.max(q.1);
    }
    let side = (x1 - x0).max(y1 - y0).max(1e-6);
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    p.iter().map(|q| ((q.0 - cx) / side, (q.1 - cy) / side)).collect()
}

fn distance(a: &[Pt], b: &[Pt]) -> f32 {
    a.iter().zip(b).map(|(p, q)| (p.0 - q.0).hypot(p.1 - q.1)).sum::<f32>() / a.len().max(1) as f32
}

/// What a pencil mark means for the selected photos.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// A digit 1–5: that star rating.
    Rate(u8),
    /// A tick: flag as picked.
    Pick,
    /// A cross: flag as rejected.
    Reject,
}

/// The mark one pencil stroke draws, if it clearly draws one.
pub fn recognize_mark(stroke: &[Pt]) -> Option<Mark> {
    if is_circle(stroke) {
        return Some(Mark::Rate(0));
    }
    match recognize_code(stroke)? {
        TICK => Some(Mark::Pick),
        CROSS => Some(Mark::Reject),
        d => Some(Mark::Rate(d)),
    }
}

/// A closed, roundish loop: a "0", which clears the rating.
fn is_circle(stroke: &[Pt]) -> bool {
    let (Some(&a), Some(&b)) = (stroke.first(), stroke.last()) else { return false };
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for q in stroke {
        x0 = x0.min(q.0);
        y0 = y0.min(q.1);
        x1 = x1.max(q.0);
        y1 = y1.max(q.1);
    }
    let (w, h) = (x1 - x0, y1 - y0);
    if !(w.is_finite() && h.is_finite()) || w.min(h) < 30.0 {
        return false;
    }
    let len = path_len(stroke);
    // the ends meet, it is about as wide as tall, and the path wraps the box about once (a circle's length is ~ pi * d)
    let closed = (b.0 - a.0).hypot(b.1 - a.1) < 0.3 * w.max(h);
    let round = (w / h).clamp(0.0, 10.0) > 0.6 && (h / w).clamp(0.0, 10.0) > 0.6;
    let wraps = len > 2.4 * w.max(h) && len < 4.2 * w.max(h);
    closed && round && wraps
}

/// `(angle in degrees 0..180, length, centre)` of a nearly straight stroke.
fn straight_line(stroke: &[Pt]) -> Option<(f32, f32, Pt)> {
    let (a, b) = (*stroke.first()?, *stroke.last()?);
    let chord = (b.0 - a.0).hypot(b.1 - a.1);
    if chord < 40.0 || path_len(stroke) > chord * 1.3 {
        return None;
    }
    let angle = (b.1 - a.1).atan2(b.0 - a.0).to_degrees().rem_euclid(180.0);
    Some((angle, chord, ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)))
}

/// Two separate strokes that cross as an X: one "\" and one "/", similar in length, centred together.
pub fn is_cross(first: &[Pt], second: &[Pt]) -> bool {
    let (Some((a1, l1, c1)), Some((a2, l2, c2))) = (straight_line(first), straight_line(second)) else { return false };
    let diag = |a: f32| (25.0..=65.0).contains(&a) || (115.0..=155.0).contains(&a);
    let opposite = (a1 - a2).abs() > 50.0;
    let near = (c1.0 - c2.0).hypot(c1.1 - c2.1) < 0.5 * l1.max(l2);
    let alike = l1.max(l2) < 2.5 * l1.min(l2);
    diag(a1) && diag(a2) && opposite && near && alike
}

/// The digit 1–5 a pencil stroke draws, if it clearly draws one.
pub fn recognize_digit(stroke: &[Pt]) -> Option<u8> {
    recognize_code(stroke).filter(|c| (1..=5).contains(c))
}

fn recognize_code(stroke: &[Pt]) -> Option<u8> {
    let pts: Vec<Pt> = stroke.iter().copied().filter(|p| p.0.is_finite() && p.1.is_finite()).collect();
    if pts.len() < 3 {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for q in &pts {
        x0 = x0.min(q.0);
        y0 = y0.min(q.1);
        x1 = x1.max(q.0);
        y1 = y1.max(q.1);
    }
    let (w, h) = (x1 - x0, y1 - y0);
    // too small to be a deliberate digit (a tap or a wobble)
    if h.max(w) < 24.0 {
        return None;
    }
    // "1": a tall, narrow, nearly straight stroke (a small flag on top is allowed)
    let straight = path_len(&pts) / ((pts[0].0 - pts[pts.len() - 1].0).hypot(pts[0].1 - pts[pts.len() - 1].1)).max(1.0);
    if h > 3.0 * w && straight < 1.5 {
        return Some(1);
    }
    let me = normalize(&resample(&pts, N));
    if me.is_empty() {
        return None;
    }
    let mut scores: Vec<(f32, u8)> = templates().into_iter().map(|(d, t)| (distance(&me, &normalize(&resample(&t, N))), d)).collect();
    scores.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let (best, digit) = *scores.first()?;
    let other = scores.iter().find(|s| s.1 != digit).map_or(f32::MAX, |s| s.0);
    (best < ACCEPT && other > best * MARGIN).then_some(digit)
}

/// Digits people draw with a lift: a "4" (a corner and a stem), a "5" (a bar and a body), a "1" with a base.
/// Each is the strokes together; the matcher ignores stroke order and direction.
fn multi_templates() -> Vec<(u8, Vec<Vec<Pt>>)> {
    vec![
        (4, vec![vec![(0.7, 0.0), (0.0, 0.65), (1.0, 0.65)], vec![(0.7, 0.0), (0.7, 1.0)]]),
        (4, vec![vec![(0.6, 0.0), (0.0, 0.65), (1.0, 0.65)], vec![(0.75, 0.25), (0.75, 1.0)]]),
        (4, vec![vec![(0.0, 0.0), (0.0, 0.6), (1.0, 0.6)], vec![(0.8, 0.0), (0.8, 1.0)]]),
        (5, vec![vec![(0.95, 0.0), (0.15, 0.0)], vec![(0.15, 0.0), (0.1, 0.45), (0.6, 0.38), (0.95, 0.65), (0.7, 0.95), (0.3, 1.0), (0.0, 0.85)]]),
        (5, vec![vec![(0.9, 0.0), (0.2, 0.0), (0.1, 0.45)], vec![(0.1, 0.45), (0.6, 0.35), (0.95, 0.65), (0.7, 0.95), (0.3, 1.0), (0.0, 0.85)]]),
        (2, vec![vec![(0.0, 0.2), (0.3, 0.0), (0.8, 0.1), (0.7, 0.45), (0.0, 1.0)], vec![(0.0, 1.0), (1.0, 1.0)]]),
        (1, vec![vec![(0.5, 0.0), (0.5, 1.0)], vec![(0.2, 1.0), (0.8, 1.0)]]),
    ]
}

/// `n` points spread along all the strokes together (a lift is not joined by a line).
fn resample_strokes(strokes: &[Vec<Pt>], n: usize) -> Vec<Pt> {
    let total: f32 = strokes.iter().map(|s| path_len(s)).sum();
    if total <= 0.0 || !total.is_finite() || n < 2 {
        return Vec::new();
    }
    let step = total / (n - 1) as f32;
    let mut out: Vec<Pt> = Vec::with_capacity(n);
    let mut carry = 0.0;
    for stroke in strokes {
        let Some(&first) = stroke.first() else { continue };
        if out.is_empty() || carry >= step {
            out.push(first);
            carry = 0.0;
        }
        let mut prev = first;
        for &q in stroke.iter().skip(1) {
            let mut seg = (q.0 - prev.0).hypot(q.1 - prev.1);
            while seg > 0.0 && carry + seg >= step && out.len() < n {
                let t = (step - carry) / seg;
                let m = (prev.0 + t * (q.0 - prev.0), prev.1 + t * (q.1 - prev.1));
                out.push(m);
                prev = m;
                seg = (q.0 - prev.0).hypot(q.1 - prev.1);
                carry = 0.0;
            }
            carry += seg;
            prev = q;
        }
    }
    while out.len() < n {
        if let Some(&l) = out.last() {
            out.push(l);
        } else {
            break;
        }
    }
    out.truncate(n);
    out
}

/// Greedy point-cloud distance: each point of `a` is matched to the nearest unmatched point of `b`, the first
/// matches weighing most; tried from several start points and both ways, the smallest wins ($P recogniser).
fn cloud_distance(a: &[Pt], b: &[Pt]) -> f32 {
    let n = a.len().min(b.len());
    if n == 0 {
        return f32::MAX;
    }
    let step = ((n as f32).sqrt().floor() as usize).max(1);
    let one_way = |x: &[Pt], y: &[Pt]| -> f32 {
        let mut best = f32::MAX;
        for start in (0..n).step_by(step) {
            let mut taken = vec![false; n];
            let (mut sum, mut weights) = (0.0, 0.0);
            for k in 0..n {
                let i = (start + k) % n;
                let (mut dmin, mut at) = (f32::MAX, None);
                for (j, q) in y.iter().enumerate().take(n) {
                    if taken[j] {
                        continue;
                    }
                    let Some(p) = x.get(i) else { continue };
                    let d = (p.0 - q.0).hypot(p.1 - q.1);
                    if d < dmin {
                        dmin = d;
                        at = Some(j);
                    }
                }
                let Some(j) = at else { continue };
                if let Some(t) = taken.get_mut(j) {
                    *t = true;
                }
                let w = 1.0 - k as f32 / n as f32;
                sum += w * dmin;
                weights += w;
            }
            if weights > 0.0 {
                best = best.min(sum / weights);
            }
        }
        best
    };
    one_way(a, b).min(one_way(b, a))
}

/// The mark several strokes drawn together make (a lifted 4 or 5), if they clearly make one.
pub fn recognize_strokes(strokes: &[Vec<Pt>]) -> Option<Mark> {
    match strokes {
        [] => None,
        [one] => recognize_mark(one),
        many => {
            let (a, b) = (many.get(many.len().saturating_sub(2))?, many.last()?);
            if is_cross(a, b) {
                return Some(Mark::Reject);
            }
            let clean: Vec<Vec<Pt>> = many.iter().map(|s| s.iter().copied().filter(|p| p.0.is_finite() && p.1.is_finite()).collect()).collect();
            let me = normalize(&resample_strokes(&clean, N));
            if me.is_empty() {
                return recognize_mark(b);
            }
            let mut scores: Vec<(f32, u8)> =
                multi_templates().into_iter().map(|(d, t)| (cloud_distance(&me, &normalize(&resample_strokes(&t, N))), d)).collect();
            scores.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
            let &(best, digit) = scores.first()?;
            let other = scores.iter().find(|s| s.1 != digit).map_or(f32::MAX, |s| s.0);
            if best < ACCEPT_CLOUD && other > best * MARGIN {
                return Some(Mark::Rate(digit));
            }
            // not a lifted digit: the last stroke may be a mark of its own
            recognize_mark(b)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A template drawn at `scale` points with deterministic hand wobble.
    fn drawn(t: &[Pt], scale: f32, seed: u32) -> Vec<Pt> {
        let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(12345);
        let mut r = move || {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((s >> 16) as f32 / 65535.0 - 0.5) * 0.06
        };
        let dense = resample(t, 60);
        dense.iter().map(|p| (300.0 + (p.0 + r()) * scale, 200.0 + (p.1 + r()) * scale)).collect()
    }

    #[test]
    fn every_template_is_read_back_with_hand_wobble_at_any_size() {
        for (digit, t) in templates() {
            for (scale, seed) in [(60.0, 1), (140.0, 2), (260.0, 3), (90.0, 4)] {
                assert_eq!(recognize_code(&drawn(&t, scale, seed)), Some(digit), "template {digit} at {scale}");
            }
        }
    }

    #[test]
    fn a_vertical_stroke_is_one_even_with_a_tilt_or_a_flag() {
        let line: Vec<Pt> = (0..30).map(|i| (100.0 + i as f32 * 0.4, 50.0 + i as f32 * 6.0)).collect();
        assert_eq!(recognize_digit(&line), Some(1));
        let mut flag = vec![(80.0, 80.0), (100.0, 50.0)];
        flag.extend((0..30).map(|i| (100.0, 50.0 + i as f32 * 6.0)));
        assert_eq!(recognize_digit(&flag), Some(1));
    }

    #[test]
    fn a_tick_picks_and_a_cross_rejects() {
        for (code, mark) in [(TICK, Mark::Pick), (CROSS, Mark::Reject)] {
            for (digit, t) in templates() {
                if digit != code {
                    continue;
                }
                for (scale, seed) in [(70.0, 11), (150.0, 12), (240.0, 13)] {
                    assert_eq!(recognize_mark(&drawn(&t, scale, seed)), Some(mark), "{mark:?} at {scale}");
                }
            }
        }
        // a digit is still a rating, and not a pick or a reject
        assert_eq!(
            recognize_mark(&drawn(&templates().into_iter().find(|t| t.0 == 3).map(|t| t.1).unwrap_or_default(), 150.0, 5)),
            Some(Mark::Rate(3))
        );
    }

    #[test]
    fn two_crossing_diagonals_reject_but_two_parallel_or_far_strokes_do_not() {
        let line = |x0: f32, y0: f32, x1: f32, y1: f32| -> Vec<Pt> {
            (0..=20).map(|i| (x0 + (x1 - x0) * i as f32 / 20.0, y0 + (y1 - y0) * i as f32 / 20.0)).collect()
        };
        assert!(is_cross(&line(100.0, 100.0, 200.0, 200.0), &line(200.0, 105.0, 105.0, 195.0)));
        assert!(!is_cross(&line(100.0, 100.0, 200.0, 200.0), &line(300.0, 100.0, 400.0, 200.0)), "parallel");
        assert!(!is_cross(&line(100.0, 100.0, 200.0, 200.0), &line(600.0, 200.0, 500.0, 100.0)), "far apart");
        assert!(!is_cross(&line(100.0, 100.0, 100.0, 200.0), &line(50.0, 150.0, 150.0, 150.0)), "a plus is not a cross");
    }

    /// A multi-stroke template drawn at `scale` points: every stroke resampled, wobbled, and placed together.
    fn drawn_strokes(t: &[Vec<Pt>], scale: f32, seed: u32) -> Vec<Vec<Pt>> {
        t.iter().enumerate().map(|(i, stroke)| drawn(stroke, scale, seed.wrapping_add(i as u32 * 7))).collect()
    }

    #[test]
    fn a_four_or_five_drawn_with_a_lift_is_read_with_hand_wobble_at_any_size() {
        for (digit, t) in multi_templates() {
            if digit != 4 && digit != 5 {
                continue;
            }
            for (scale, seed) in [(70.0, 21), (150.0, 22), (260.0, 23)] {
                assert_eq!(recognize_strokes(&drawn_strokes(&t, scale, seed)), Some(Mark::Rate(digit)), "lifted {digit} at {scale}");
            }
        }
    }

    #[test]
    fn single_strokes_and_crosses_still_resolve_through_the_multi_stroke_path() {
        let three = templates().into_iter().find(|t| t.0 == 3).map(|t| t.1).unwrap_or_default();
        assert_eq!(recognize_strokes(&[drawn(&three, 150.0, 31)]), Some(Mark::Rate(3)));
        let line = |x0: f32, y0: f32, x1: f32, y1: f32| -> Vec<Pt> {
            (0..=20).map(|i| (x0 + (x1 - x0) * i as f32 / 20.0, y0 + (y1 - y0) * i as f32 / 20.0)).collect()
        };
        assert_eq!(recognize_strokes(&[line(100.0, 100.0, 200.0, 200.0), line(200.0, 105.0, 105.0, 195.0)]), Some(Mark::Reject));
        assert_eq!(recognize_strokes(&[]), None);
    }

    #[test]
    fn taps_wobbles_and_scribbles_rate_nothing() {
        assert_eq!(recognize_digit(&[(10.0, 10.0), (11.0, 10.0), (11.0, 11.0)]), None);
        assert_eq!(recognize_digit(&[]), None);
        assert_eq!(recognize_digit(&[(f32::NAN, 1.0), (2.0, 2.0), (3.0, 3.0)]), None);
        // a circle is a zero (clears the rating), not a digit; half a circle is nothing
        let circle: Vec<Pt> = (0..=40)
            .map(|i| (300.0 + 90.0 * (i as f32 * std::f32::consts::TAU / 40.0).cos(), 300.0 + 90.0 * (i as f32 * std::f32::consts::TAU / 40.0).sin()))
            .collect();
        assert_eq!(recognize_digit(&circle), None);
        assert_eq!(recognize_mark(&circle), Some(Mark::Rate(0)));
        let half: Vec<Pt> = (0..=20)
            .map(|i| (300.0 + 90.0 * (i as f32 * std::f32::consts::PI / 20.0).cos(), 300.0 + 90.0 * (i as f32 * std::f32::consts::PI / 20.0).sin()))
            .collect();
        assert_eq!(recognize_mark(&half), None);
        let zigzag: Vec<Pt> = (0..40).map(|i| (100.0 + (i % 2) as f32 * 200.0, 100.0 + i as f32 * 4.0)).collect();
        assert_eq!(recognize_digit(&zigzag), None);
    }
}
