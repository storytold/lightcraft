//! Survey layout: the cells the photos are tiled in, fitted to their shapes, and the glide that
//! moves the tiles left to their new cells when a photo leaves or joins the survey, so it is
//! plain which photo went where.

use std::collections::HashMap;

use egui::{Rect, pos2, vec2};
use lightcraft_catalog::PhotoId;

/// Space a tile keeps below its photo for the caption (as `photo_tile` draws it).
pub const CAPTION_H: f32 = 26.0;
/// Margin around each tile inside its cell.
pub const GAP: f32 = 6.0;
/// Seconds a tile takes to glide to its new cell.
pub const GLIDE_SECS: f64 = 0.2;

/// A width / height ratio that layout arithmetic can use: the photo's own, as it is drawn (a
/// 100:1 panorama counts as one), or a square for a size that is none.
fn usable_aspect(a: f32) -> f32 {
    if a.is_finite() && a > 0.0 { a } else { 1.0 }
}

/// Image area photos with `aspects` show in a grid of `cols` equal cells in `area`.
fn shown_area(aspects: &[f32], cols: usize, area: Rect) -> f32 {
    let rows = aspects.len().div_ceil(cols.max(1));
    let iw = area.width() / cols.max(1) as f32 - 2.0 * GAP;
    let ih = area.height() / rows.max(1) as f32 - 2.0 * GAP - CAPTION_H;
    if iw <= 0.0 || ih <= 0.0 {
        return 0.0;
    }
    aspects
        .iter()
        .map(|&a| {
            let a = usable_aspect(a);
            let w = iw.min(ih * a);
            w * w / a
        })
        .sum()
}

/// The cell of each photo (width / height `aspects`, in survey order) in `area`: equal cells in
/// rows, with the column count that shows the most of the photos, so the survey fits their shapes
/// to the screen as Lightroom Classic's does (four portrait photos side by side on a wide screen,
/// not two by two). An incomplete last row is centred.
pub fn cells(aspects: &[f32], area: Rect) -> Vec<Rect> {
    let n = aspects.len();
    if n == 0 {
        return Vec::new();
    }
    let mut best = (1, f32::NEG_INFINITY);
    for cols in 1..=n {
        let shown = shown_area(aspects, cols, area);
        if shown > best.1 {
            best = (cols, shown);
        }
    }
    let cols = best.0;
    let rows = n.div_ceil(cols);
    let (cw, ch) = (area.width() / cols as f32, area.height() / rows as f32);
    let last_row = n - (rows - 1) * cols;
    (0..n)
        .map(|i| {
            let (c, r) = (i % cols, i / cols);
            let indent = if r == rows - 1 { (cols - last_row) as f32 * cw / 2.0 } else { 0.0 };
            Rect::from_min_size(pos2(area.left() + indent + c as f32 * cw, area.top() + r as f32 * ch), vec2(cw, ch))
        })
        .collect()
}

/// One tile's way from where it was drawn to its cell.
#[derive(Clone, Copy, Debug)]
struct Glide {
    from: Rect,
    to: Rect,
    start: f64,
}

impl Glide {
    fn at(&self, now: f64) -> Rect {
        let t = ((now - self.start) / GLIDE_SECS).clamp(0.0, 1.0) as f32;
        // ease out: quick to leave, gentle to land
        let e = 1.0 - (1.0 - t).powi(3);
        self.from.lerp_towards(&self.to, e)
    }

    fn on_its_way(&self, now: f64) -> bool {
        !self.done(now) && self.from != self.to
    }

    fn done(&self, now: f64) -> bool {
        // (a hair early: the clock's rounding must not keep a landed tile moving)
        now - self.start >= GLIDE_SECS - 1e-9
    }
}

/// Where the survey's tiles are drawn while they glide, and which render slot each photo keeps
/// (a photo keeps its slot while it is shown, so the tiles that move keep their renders).
#[derive(Default)]
pub struct SurveyMotion {
    /// The survey area of the last frame drawn, and its frame number.
    last: Option<(Rect, u64)>,
    tiles: HashMap<PhotoId, Glide>,
    slots: HashMap<PhotoId, u8>,
}

impl SurveyMotion {
    /// Where to draw `photos` (in survey order) at `now`, given their `cells` in `area` this frame
    /// (`frame`, egui's cumulative frame number). A tile whose cell changed since the last frame glides
    /// there from where it was drawn; a photo that joined appears in its cell. A survey just opened
    /// or a canvas that changed size is laid out at once.
    pub fn place(&mut self, photos: &[PhotoId], cells: &[Rect], area: Rect, frame: u64, now: f64) -> Vec<Rect> {
        let continued = self.last.is_some_and(|(a, p)| a == area && frame <= p.saturating_add(1));
        self.last = Some((area, frame));
        let mut next = HashMap::with_capacity(photos.len());
        let placed = photos
            .iter()
            .zip(cells)
            .map(|(id, &to)| {
                let glide = match self.tiles.get(id) {
                    Some(g) if continued && g.to == to => *g,
                    Some(g) if continued => Glide { from: g.at(now), to, start: now },
                    _ => Glide { from: to, to, start: now },
                };
                next.insert(*id, glide);
                glide.at(now)
            })
            .collect();
        self.tiles = next;
        placed
    }

    /// Whether a tile is still on its way.
    pub fn moving(&self, now: f64) -> bool {
        self.tiles.values().any(|g| g.on_its_way(now))
    }

    /// Whether the tile of `id` is still on its way (a tile that stays put, or a photo that just
    /// joined, is not, while others glide).
    pub fn gliding(&self, id: PhotoId, now: f64) -> bool {
        self.tiles.get(&id).is_some_and(|g| g.on_its_way(now))
    }

    /// The render slot of each of `photos`: the one it had, else the lowest free one. Returns the
    /// slots that photos no longer shown gave up, whose renders can be freed.
    pub fn assign_slots(&mut self, photos: &[PhotoId]) -> (Vec<u8>, Vec<u8>) {
        let freed: Vec<u8> = self.slots.iter().filter(|(id, _)| !photos.contains(id)).map(|(_, s)| *s).collect();
        self.slots.retain(|id, _| photos.contains(id));
        let used: Vec<u8> = self.slots.values().copied().collect();
        let mut free = (0..=u8::MAX).filter(move |s| !used.contains(s));
        let slots = photos.iter().map(|id| *self.slots.entry(*id).or_insert_with(|| free.next().unwrap_or(u8::MAX))).collect();
        (slots, freed)
    }

    /// The render slot `id` keeps, if it has one.
    #[cfg(test)]
    pub fn slot(&self, id: PhotoId) -> Option<u8> {
        self.slots.get(&id).copied()
    }

    /// Whether `id` has a tile here (its place or its render slot).
    #[cfg(test)]
    pub fn tracks(&self, id: PhotoId) -> bool {
        self.tiles.contains_key(&id) || self.slots.contains_key(&id)
    }
}

#[cfg(test)]
mod tests {
    use super::{CAPTION_H, GAP, GLIDE_SECS, SurveyMotion, cells};
    use egui::{Rect, pos2, vec2};
    use lightcraft_catalog::PhotoId;

    const WIDE: Rect = Rect { min: pos2(0.0, 0.0), max: pos2(1600.0, 800.0) };

    fn cols(cells: &[Rect]) -> usize {
        cells.iter().filter(|c| c.top() == cells[0].top()).count()
    }

    #[test]
    fn four_portraits_stand_side_by_side_on_a_wide_screen() {
        let c = cells(&[2.0 / 3.0; 4], WIDE);
        assert_eq!(cols(&c), 4, "{c:?}");
    }

    #[test]
    fn four_landscapes_on_a_wide_screen_take_two_rows() {
        let c = cells(&[1.5; 4], WIDE);
        assert_eq!(cols(&c), 2, "{c:?}");
    }

    /// Column choice follows the ratio drawn, also past 20:1: four 100:1 panoramas and four
    /// portraits show most side by side in four columns, not one tall column.
    #[test]
    fn panoramas_count_at_their_own_ratio() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(2560.0, 1440.0));
        let aspects = [100.0, 100.0, 100.0, 100.0, 2.0 / 3.0, 2.0 / 3.0, 2.0 / 3.0, 2.0 / 3.0];
        assert_eq!(cols(&cells(&aspects, area)), 4);
    }

    #[test]
    fn the_last_row_is_centred() {
        let c = cells(&[1.5; 5], WIDE);
        assert_eq!(cols(&c), 3, "{c:?}");
        let (row, last) = (&c[..3], &c[3..]);
        assert_eq!(last.len(), 2);
        let mid = |r: &[Rect]| (r[0].left() + r[r.len() - 1].right()) / 2.0;
        assert!((mid(row) - mid(last)).abs() < 0.01, "{c:?}");
    }

    #[test]
    fn cells_stay_inside_the_area_and_survive_hostile_sizes() {
        for aspects in [vec![f32::NAN, 0.0, -1.0, f32::INFINITY, 1e9, f32::MAX, f32::MIN_POSITIVE], vec![1.5; 48]] {
            let c = cells(&aspects, WIDE);
            assert_eq!(c.len(), aspects.len());
            assert!(c.iter().all(|r| WIDE.expand(0.01).contains_rect(*r)), "{c:?}");
        }
        assert!(cells(&[], WIDE).is_empty());
        // a canvas too small for any tile still lays them out, one column
        let tiny = Rect::from_min_size(pos2(0.0, 0.0), vec2(2.0 * GAP, CAPTION_H));
        assert_eq!(cells(&[1.0; 3], tiny).len(), 3);
    }

    #[test]
    fn a_tile_glides_to_its_new_cell_and_lands_there() {
        let (a, b) = (PhotoId(1), PhotoId(2));
        let mut m = SurveyMotion::default();
        let first = cells(&[1.5; 2], WIDE);
        assert_eq!(m.place(&[a, b], &first, WIDE, 1, 0.0), first, "a new survey is laid out at once");
        assert!(!m.moving(0.0));
        // a leaves: b moves to the single cell
        let one = cells(&[1.5], WIDE);
        let mid = m.place(&[b], &one, WIDE, 2, 1.0);
        assert_eq!(mid[0], first[1], "it starts where it was drawn");
        assert!(m.moving(1.0));
        let half = m.place(&[b], &one, WIDE, 3, 1.0 + GLIDE_SECS / 2.0)[0];
        assert!(half != first[1] && half != one[0], "on its way: {half:?}");
        assert_eq!(m.place(&[b], &one, WIDE, 4, 1.0 + GLIDE_SECS)[0], one[0], "it lands in its cell");
        assert!(!m.moving(1.0 + GLIDE_SECS));
    }

    #[test]
    fn a_reopened_survey_or_a_resized_canvas_does_not_glide() {
        let (a, b) = (PhotoId(1), PhotoId(2));
        let mut m = SurveyMotion::default();
        m.place(&[a, b], &cells(&[1.5; 2], WIDE), WIDE, 1, 0.0);
        let one = cells(&[1.5], WIDE);
        assert_eq!(m.place(&[b], &one, WIDE, 9, 1.0), one, "frames skipped: the survey was not shown");
        let narrow = Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 800.0));
        let c = cells(&[1.5], narrow);
        assert_eq!(m.place(&[b], &c, narrow, 10, 2.0), c, "a new canvas size");
    }

    #[test]
    fn photos_keep_their_slots_and_new_ones_take_freed_slots() {
        let (a, b, c, d) = (PhotoId(1), PhotoId(2), PhotoId(3), PhotoId(4));
        let mut m = SurveyMotion::default();
        assert_eq!(m.assign_slots(&[a, b, c]), (vec![0, 1, 2], vec![]));
        assert_eq!(m.assign_slots(&[a, c]), (vec![0, 2], vec![1]), "c keeps its render");
        assert_eq!(m.assign_slots(&[a, c, d]), (vec![0, 2, 1], vec![]), "d takes the freed slot");
    }
}
