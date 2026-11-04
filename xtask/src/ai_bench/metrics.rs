//! WIDER FACE average precision, implemented from the published description of the evaluation (Yang et al., "WIDER
//! FACE", CVPR 2016, and the evaluation toolkit's documented procedure; no evaluation code was copied): detection
//! scores are normalised over the whole set, a detection hits a face when their intersection over union is at least 0.5
//! (with the toolkit's inclusive-pixel `+1` convention), faces outside the setting's keep list are neither counted nor
//! punished, precision and recall are taken at 1,000 score thresholds and the area under the curve is the VOC
//! all-point average precision.

/// How many score thresholds the precision/recall curve has.
const THRESHOLDS: usize = 1000;
/// Overlap at which a detection hits a face.
const MIN_IOU: f64 = 0.5;

/// One image: ground-truth boxes `[x, y, w, h]`, the 1-based indices of the faces this setting counts, and detections
/// `[x, y, w, h, score]`, all in pixels.
pub struct Image<'a> {
    pub gt: &'a [[f64; 4]],
    pub keep: &'a [usize],
    pub preds: &'a [[f64; 5]],
}

/// IoU of two `[x1, y1, x2, y2]` boxes, counting pixels inclusively (a box from 0 to 9 is ten pixels wide).
fn iou(a: &[f64; 4], b: &[f64; 4]) -> f64 {
    let iw = a[2].min(b[2]) - a[0].max(b[0]) + 1.0;
    let ih = a[3].min(b[3]) - a[1].max(b[1]) + 1.0;
    if iw <= 0.0 || ih <= 0.0 {
        return 0.0;
    }
    let area = |r: &[f64; 4]| (r[2] - r[0] + 1.0) * (r[3] - r[1] + 1.0);
    let inter = iw * ih;
    let union = area(a) + area(b) - inter;
    if union > 0.0 { inter / union } else { 0.0 }
}

/// VOC all-point average precision of a precision/recall curve (recall ascending).
fn voc_ap(recall: &[f64], precision: &[f64]) -> f64 {
    let mut mrec = vec![0.0];
    mrec.extend_from_slice(recall);
    mrec.push(1.0);
    let mut mpre = vec![0.0];
    mpre.extend_from_slice(precision);
    mpre.push(0.0);
    for i in (1..mpre.len()).rev() {
        mpre[i - 1] = mpre[i - 1].max(mpre[i]);
    }
    mrec.windows(2).zip(mpre.iter().skip(1)).filter(|(r, _)| r[1] != r[0]).map(|(r, p)| (r[1] - r[0]) * p).sum()
}

/// The WIDER FACE average precision (0 to 1) of one setting (easy, medium or hard: the `keep` lists) over `images`.
pub fn wider_ap(images: &[Image]) -> f64 {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for p in images.iter().flat_map(|i| i.preds).filter(|p| p[4].is_finite()) {
        lo = lo.min(p[4]);
        hi = hi.max(p[4]);
    }
    let span = if hi > lo { hi - lo } else { 1.0 };
    // per threshold: detections kept that are not on an uncounted face, and counted faces found
    let mut curve = vec![(0.0f64, 0.0f64); THRESHOLDS];
    let mut faces = 0usize;
    for img in images {
        faces += img.keep.len();
        if img.gt.is_empty() || img.preds.is_empty() {
            continue;
        }
        let gt: Vec<[f64; 4]> = img.gt.iter().map(|g| [g[0], g[1], g[0] + g[2], g[1] + g[3]]).collect();
        let mut preds: Vec<([f64; 4], f64)> = img
            .preds
            .iter()
            .filter(|p| p[4].is_finite())
            .map(|p| ([p[0], p[1], p[0] + p[2], p[1] + p[3]], ((p[4] - lo) / span).clamp(0.0, 1.0)))
            .collect();
        preds.sort_by(|a, b| b.1.total_cmp(&a.1));
        // per face: whether this setting counts it, and 0 unseen / 1 found / -1 matched by a detection that is then ignored
        let mut state: Vec<(bool, i8)> = vec![(false, 0); gt.len()];
        for k in img.keep {
            if let Some(s) = k.checked_sub(1).and_then(|i| state.get_mut(i)) {
                s.0 = true;
            }
        }
        // after each detection (best first): the proposals so far and the counted faces found so far
        let (mut proposals, mut recalled) = (0usize, 0usize);
        let mut after: Vec<(usize, usize)> = Vec::with_capacity(preds.len());
        for (b, _) in &preds {
            let mut best = (0.0f64, None);
            for (j, g) in gt.iter().enumerate() {
                let o = iou(b, g);
                if o > best.0 {
                    best = (o, Some(j));
                }
            }
            let mut ignored = false;
            if let (o, Some(j)) = best
                && o >= MIN_IOU
                && let Some((counted, found)) = state.get_mut(j)
            {
                if !*counted {
                    if *found == 1 {
                        recalled = recalled.saturating_sub(1);
                    }
                    *found = -1;
                    ignored = true;
                } else if *found == 0 {
                    *found = 1;
                    recalled += 1;
                }
            }
            if !ignored {
                proposals += 1;
            }
            after.push((proposals, recalled));
        }
        for (t, point) in curve.iter_mut().enumerate() {
            let threshold = 1.0 - (t + 1) as f64 / THRESHOLDS as f64;
            if let Some((p, r)) = preds.iter().rposition(|p| p.1 >= threshold).and_then(|last| after.get(last)) {
                point.0 += *p as f64;
                point.1 += *r as f64;
            }
        }
    }
    if faces == 0 {
        return 0.0;
    }
    let precision: Vec<f64> = curve.iter().map(|c| if c.0 > 0.0 { c.1 / c.0 } else { 0.0 }).collect();
    let recall: Vec<f64> = curve.iter().map(|c| c.1 / faces as f64).collect();
    voc_ap(&recall, &precision)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn iou_counts_pixels_inclusively() {
        let a = [0.0, 0.0, 9.0, 9.0]; // 10 x 10 pixels
        close(iou(&a, &a), 1.0);
        close(iou(&a, &[20.0, 20.0, 29.0, 29.0]), 0.0);
        // 5 x 10 shared of 10 x 10 and 10 x 10: 50 / 150
        close(iou(&a, &[5.0, 0.0, 14.0, 9.0]), 1.0 / 3.0);
        // touching at one pixel column still shares a pixel under the inclusive convention
        close(iou(&a, &[9.0, 0.0, 18.0, 9.0]), 10.0 / 190.0);
        // a degenerate pair has no overlap rather than a division by zero
        close(iou(&[5.0, 5.0, 4.0, 4.0], &[5.0, 5.0, 4.0, 4.0]), 0.0);
    }

    #[test]
    fn a_detection_hits_from_half_overlap() {
        let gt = [[0.0, 0.0, 10.0, 10.0]]; // pixels 0..=10 under the +1 convention: 11 x 11
        // 11 x 11 box against an 11 x 6 detection sharing its top rows: 66 / 121 > 0.5 hits
        let hit = [[0.0, 0.0, 10.0, 5.0, 1.0]];
        close(wider_ap(&[Image { gt: &gt, keep: &[1], preds: &hit }]), 1.0);
        // 11 x 5: 55 / 121 < 0.5 misses
        let miss = [[0.0, 0.0, 10.0, 4.0, 1.0]];
        close(wider_ap(&[Image { gt: &gt, keep: &[1], preds: &miss }]), 0.0);
    }

    /// Two faces, three detections: a hit (score .75), a false positive (.5) and a hit (.25). Scores normalise to 1, .5
    /// and 0. At thresholds .999 to .501 only the first detection counts (precision 1, recall 1/2, 499 times); at .500 to
    /// .001 the first two (precision 1/2, recall 1/2, 500 times); at 0 all three (precision 2/3, recall 1). The
    /// precision envelope is 1 up to recall 1/2 and 2/3 beyond it: AP = 1/2 * 1 + 1/2 * 2/3 = 5/6.
    #[test]
    fn average_precision_by_hand() {
        let gt = [[0.0, 0.0, 10.0, 10.0], [100.0, 100.0, 10.0, 10.0]];
        let preds = [[0.0, 0.0, 10.0, 10.0, 0.75], [300.0, 300.0, 10.0, 10.0, 0.5], [100.0, 100.0, 10.0, 10.0, 0.25]];
        close(wider_ap(&[Image { gt: &gt, keep: &[1, 2], preds: &preds }]), 5.0 / 6.0);
        // the same detections without the false positive: a perfect curve
        let clean = [preds[0], preds[2]];
        close(wider_ap(&[Image { gt: &gt, keep: &[1, 2], preds: &clean }]), 1.0);
    }

    #[test]
    fn faces_outside_the_keep_list_are_neither_missed_nor_punished() {
        let gt = [[0.0, 0.0, 10.0, 10.0], [100.0, 100.0, 10.0, 10.0]];
        // only the first face counts; a detection on the second must not count as a false positive
        let both = [[0.0, 0.0, 10.0, 10.0, 0.9], [100.0, 100.0, 10.0, 10.0, 0.8]];
        close(wider_ap(&[Image { gt: &gt, keep: &[1], preds: &both }]), 1.0);
        // and not finding it costs no recall
        close(wider_ap(&[Image { gt: &gt, keep: &[1], preds: &both[..1] }]), 1.0);
    }

    #[test]
    fn nothing_to_find_or_nothing_found_scores_zero() {
        let gt = [[0.0, 0.0, 10.0, 10.0]];
        close(wider_ap(&[]), 0.0);
        close(wider_ap(&[Image { gt: &gt, keep: &[1], preds: &[] }]), 0.0);
        close(wider_ap(&[Image { gt: &gt, keep: &[], preds: &[[0.0, 0.0, 10.0, 10.0, 0.9]] }]), 0.0);
        // a non-finite score is dropped, not propagated
        close(wider_ap(&[Image { gt: &gt, keep: &[1], preds: &[[0.0, 0.0, 10.0, 10.0, f64::NAN]] }]), 0.0);
    }
}
