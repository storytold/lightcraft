//! Canon's autofocus points, from the maker note's AFInfo2 (`0x0026`) or AFInfo3 (`0x003c`)
//! record: CR3 (the `CMT3` maker-note block), CR2 and Canon JPEGs (a header-less maker-note IFD).
//!
//! Tag identities, the record's layout and its value names come from the public ExifTool
//! tag-name documentation (Canon AFInfo2 table); no third-party code was read. Confirmed on real
//! EOS R5 C CR3s (8192 × 5464): the point drawn lands on the face the camera tracked (Face +
//! Tracking) or on the zone it used, at the documented signs.
//!
//! The record is int16 values:
//!
//! | Index | Value |
//! |---|---|
//! | 0 | the record's size in bytes |
//! | 1 | AFAreaMode (see [`area_name`]) |
//! | 2 | NumAFPoints `n` |
//! | 3 | ValidAFPoints: how many of the points carry a frame |
//! | 4, 5 | CanonImageWidth, CanonImageHeight |
//! | 6, 7 | AFImageWidth, AFImageHeight: the frame the positions are measured in |
//! | 8 … | `n` AFAreaWidths, `n` AFAreaHeights, `n` AFAreaXPositions, `n` AFAreaYPositions (signed, from the frame's centre, x to the right, y **up**) |
//! | then | AFPointsInFocus: `⌈n / 16⌉` words, one bit per point; then AFPointsSelected, the same |
//!
//! The box drawn covers the points in focus, else the points selected (a DSLR whose single point
//! wasn't confirmed in focus), else the only valid point; never a union of a whole AF grid. Manual
//! focus (AFAreaMode 0) and a record without a usable frame give no point. Checked on a 6D's
//! 11-point CR2s (centre point selected, and in focus or not) as well as the R5 C.

use crate::focus::FocusPoint;
use crate::{Orientation, Rect};
use lightcraft_tiff::Ifd;

/// AFInfo2.
const AF_INFO2: u16 = 0x0026;
/// AFInfo3 (newer bodies; the same layout).
const AF_INFO3: u16 = 0x003c;
/// The most AF points a record may list (the R5 C lists 1053).
const MAX_POINTS: usize = 4096;

/// The record's values as signed 16-bit numbers (they are stored as `int16u` but the positions are
/// signed).
fn values(ifd: &Ifd, tag: u16) -> Option<Vec<i16>> {
    let v = ifd.value(tag)?;
    let n = v.count();
    if !(8..=8 + 6 * MAX_POINTS).contains(&n) {
        return None;
    }
    (0..n).map(|i| v.get_i64(i).map(|x| x as u16 as i16)).collect()
}

/// AFAreaMode's name, for the modes the documentation names plainly; others are shown by number.
pub fn area_name(v: i16) -> String {
    match v {
        1 => "AF Point Expansion".into(),
        2 => "Single-point AF".into(),
        4 => "Multi-point AF".into(),
        5 => "Face Detect AF".into(),
        6 => "Face + Tracking".into(),
        7 => "Zone AF".into(),
        8 | 10 => "AF Point Expansion".into(),
        9 => "Spot AF".into(),
        13 => "Flexizone Single".into(),
        14 | 16 | 17 => "Large Zone AF".into(),
        other => format!("AF area {other}"),
    }
}

/// The focus point a Canon maker-note IFD records (see the module notes), on the upright photo.
pub fn canon(ifd: &Ifd, orientation: Orientation) -> Option<FocusPoint> {
    let v = values(ifd, AF_INFO2).or_else(|| values(ifd, AF_INFO3))?;
    let at = |i: usize| v.get(i).copied();
    let mode = at(1)?;
    if mode == 0 {
        return None;
    }
    let n = usize::try_from(at(2)?).ok().filter(|n| (1..=MAX_POINTS).contains(n))?;
    // a record that names no valid point has no point to draw
    let valid = usize::try_from(at(3)?).ok().filter(|&v| v > 0)?.min(n);
    let (fw, fh) = (f64::from(at(6)?), f64::from(at(7)?));
    if !(fw >= 16.0 && fh >= 16.0) {
        return None;
    }
    // the bit masks after the positions: the points in focus, then (on most bodies) the points
    // selected; the box covers the first of them that names any point
    let words = n.div_ceil(16);
    let bit = |base: usize, k: usize| at(base + k / 16).is_some_and(|w| (w as u16) & (1 << (k % 16)) != 0);
    let (focus_at, selected_at) = (8 + 4 * n, 8 + 4 * n + words);
    let chosen: Vec<usize> = [focus_at, selected_at]
        .iter()
        .map(|&base| (0..valid).filter(|&k| bit(base, k)).collect::<Vec<_>>())
        .find(|v| !v.is_empty())
        // one valid point and no mask: that point (no union of a DSLR's whole AF grid)
        .unwrap_or_else(|| if valid == 1 { vec![0] } else { Vec::new() });
    let mut bounds: Option<(f64, f64, f64, f64)> = None;
    for k in chosen {
        let (w, h, x, y) = (at(8 + k)?, at(8 + n + k)?, at(8 + 2 * n + k)?, at(8 + 3 * n + k)?);
        if w <= 0 || h <= 0 {
            continue;
        }
        // from the frame's centre, y up → from its top left, y down
        let (cx, cy) = (fw / 2.0 + f64::from(x), fh / 2.0 - f64::from(y));
        let (hw, hh) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
        let b = (cx - hw, cy - hh, cx + hw, cy + hh);
        bounds = Some(match bounds {
            Some(a) => (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)),
            None => b,
        });
    }
    let (x0, y0, x1, y1) = bounds?;
    // a frame entirely off the AF image is not a point
    if x1 <= 0.0 || y1 <= 0.0 || x0 >= fw || y0 >= fh {
        return None;
    }
    let sensor = Rect { x0: x0 / fw, y0: y0 / fh, x1: x1 / fw, y1: y1 / fh };
    FocusPoint { rect: orientation.map_norm_rect(sensor), mode: None, area: Some(area_name(mode)), tracking: None }.sanitized()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_tiff::{Entry, Value};

    /// An AFInfo2 record as the R5 C writes it: 1053 points, the first valid, 8192 × 5464.
    fn record(mode: u16, w: i16, h: i16, x: i16, y: i16, in_focus: bool) -> Vec<u16> {
        let n = 1053usize;
        let mut v = vec![0u16; 8 + 4 * n + n.div_ceil(16) + 1];
        v[..8].copy_from_slice(&[0, mode, n as u16, 1, 8192, 5464, 8192, 5464]);
        v[8] = w as u16;
        v[8 + n] = h as u16;
        v[8 + 2 * n] = x as u16;
        v[8 + 3 * n] = y as u16;
        v[8 + 4 * n] = u16::from(in_focus);
        v[0] = (v.len() * 2) as u16;
        v
    }

    fn ifd(tag: u16, v: Vec<u16>) -> Ifd {
        Ifd { entries: vec![Entry { tag, value: Value::Short(v), offset: 0 }], ..Default::default() }
    }

    /// The R5 C sample: Face + Tracking, a 115-pixel frame 1087 right of and 911 above the centre.
    #[test]
    fn reads_the_r5c_face_tracking_point() {
        let p = canon(&ifd(AF_INFO2, record(6, 115, 115, 1087, 911, true)), Orientation::Normal).unwrap();
        let (cx, cy) = p.center();
        assert!((cx - (4096.0 + 1087.0) / 8192.0).abs() < 1e-9, "{cx}");
        assert!((cy - (2732.0 - 911.0) / 5464.0).abs() < 1e-9, "y is up: {cy}");
        assert!((p.rect.x1 - p.rect.x0 - 115.0 / 8192.0).abs() < 1e-9);
        assert_eq!(p.area.as_deref(), Some("Face + Tracking"));
        // AFInfo3 reads the same
        assert_eq!(canon(&ifd(AF_INFO3, record(6, 115, 115, 1087, 911, true)), Orientation::Normal), Some(p.clone()));
        // a portrait frame (camera turned right): the point turns with the photo
        let r = canon(&ifd(AF_INFO2, record(6, 115, 115, 1087, 911, true)), Orientation::Rotate90).unwrap();
        assert!((r.center().0 - p.center().0).abs() > 0.05 || (r.center().1 - p.center().1).abs() > 0.05);
    }

    /// Manual focus, no frame, junk and short records give no point and never panic.
    #[test]
    fn no_point_and_junk() {
        assert!(canon(&ifd(AF_INFO2, record(0, 115, 115, 0, 0, true)), Orientation::Normal).is_none(), "manual focus");
        assert!(canon(&ifd(AF_INFO2, record(6, 0, 0, 0, 0, true)), Orientation::Normal).is_none(), "no frame");
        assert!(canon(&ifd(AF_INFO2, record(6, 100, 100, 30000, 30000, true)), Orientation::Normal).is_none(), "off the frame");
        assert!(canon(&ifd(AF_INFO2, vec![1, 2, 3]), Orientation::Normal).is_none());
        let mut huge = record(6, 100, 100, 0, 0, true);
        huge[2] = u16::MAX;
        assert!(canon(&ifd(AF_INFO2, huge), Orientation::Normal).is_none());
        let mut zero = record(6, 100, 100, 0, 0, true);
        zero[6] = 0;
        assert!(canon(&ifd(AF_INFO2, zero), Orientation::Normal).is_none());
        assert!(canon(&Ifd::default(), Orientation::Normal).is_none());
        // a record that names no valid point: no box (review of #3566), whatever its masks say
        let mut none_valid = record(6, 100, 100, 0, 0, true);
        none_valid[3] = 0;
        assert!(canon(&ifd(AF_INFO2, none_valid), Orientation::Normal).is_none(), "no valid point");
        // the mask says nothing in focus: the only valid point still makes the box
        assert!(canon(&ifd(AF_INFO2, record(7, 576, 480, -348, 270, false)), Orientation::Normal).is_some());
    }

    /// A 6D's 11 AF points, all valid: the box is the point in focus, else the one selected,
    /// never the whole grid.
    #[test]
    fn dslr_grid_uses_the_focused_or_selected_point() {
        let xs: [i16; 11] = [-1143, -547, -547, -547, 0, 0, 0, 547, 547, 547, 1143];
        let ys: [i16; 11] = [0, 350, 0, -350, 488, 0, -488, 350, 0, -350, 0];
        let grid = |in_focus: u16, selected: u16| {
            let mut v = vec![112u16, 2, 11, 11, 5472, 3648, 5472, 3648];
            v.extend([82u16; 11]);
            v.extend([82u16; 11]);
            v.extend(xs.map(|x| x as u16));
            v.extend(ys.map(|y| y as u16));
            v.extend([in_focus, selected, 0, 0xffff]);
            v
        };
        for (f, sel) in [(0x20u16, 0x20u16), (0, 0x20)] {
            let p = canon(&ifd(AF_INFO2, grid(f, sel)), Orientation::Normal).unwrap();
            let (cx, cy) = p.center();
            assert!((cx - 0.5).abs() < 1e-9 && (cy - 0.5).abs() < 1e-9, "{cx} {cy}");
            assert!(p.rect.x1 - p.rect.x0 < 0.03, "one point, not the grid");
            assert_eq!(p.area.as_deref(), Some("Single-point AF"));
        }
        assert!(canon(&ifd(AF_INFO2, grid(0, 0)), Orientation::Normal).is_none(), "no point named");
    }

    /// Opt-in: the points of real Canon files (`LIGHTCRAFT_CANON_SAMPLES`: paths separated by `;`),
    /// printed for checking against the photos; each must have one, read whole or by its header.
    #[test]
    fn real_canon_files() {
        let Ok(list) = std::env::var("LIGHTCRAFT_CANON_SAMPLES") else {
            lightcraft_testkit::skip!(Input, "LIGHTCRAFT_CANON_SAMPLES is not set");
            return;
        };
        for path in list.split(';').filter(|p| !p.trim().is_empty()) {
            let bytes = std::fs::read(path.trim()).unwrap();
            let p = crate::extract(&bytes).focus;
            let h = crate::focus::read_header(bytes.len() as u64, &mut |off, buf| {
                buf.copy_from_slice(&bytes[off as usize..off as usize + buf.len()]);
                Ok(())
            })
            .unwrap();
            println!("{path}: {p:?} (header: {} bytes)", h.bytes_read);
            assert!(p.is_some(), "{path}");
            assert_eq!(h.focus, p, "{path}");
        }
    }
}
