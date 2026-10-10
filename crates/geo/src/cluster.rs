//! Grid clustering of photo pins: at a zoom, the world is cut into square cells of `cell_px`
//! screen pixels and each non-empty cell becomes one pin (one photo) or one cluster (a count),
//! placed at its photos' mean position. Zooming in splits clusters; it is deterministic, so pins
//! don't jump between frames.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::mercator::{LatLon, project, unproject, world_px};

/// One pin on the map.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Cluster<T> {
    pub centre: LatLon,
    /// The photos in it (one for a single pin), in input order.
    pub items: Vec<T>,
}

/// Cluster `points` for display at `zoom` with cells of `cell_px` screen pixels.
pub fn cluster<T: Clone>(points: &[(T, LatLon)], zoom: f64, cell_px: f64) -> Vec<Cluster<T>> {
    let cell = cell_px.max(1.0) / world_px(zoom);
    if !cell.is_finite() || cell <= 0.0 {
        return Vec::new();
    }
    let mut cells: BTreeMap<(i64, i64), (f64, f64, Vec<T>)> = BTreeMap::new();
    for (item, p) in points.iter().filter(|(_, p)| p.is_valid()) {
        let (x, y) = project(*p);
        let key = ((x / cell).floor() as i64, (y / cell).floor() as i64);
        let e = cells.entry(key).or_insert((0.0, 0.0, Vec::new()));
        e.0 += x;
        e.1 += y;
        e.2.push(item.clone());
    }
    cells
        .into_values()
        .map(|(sx, sy, items)| {
            let n = items.len().max(1) as f64;
            Cluster { centre: unproject(sx / n, sy / n), items }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clusters_split_when_zooming_in() {
        let pts = vec![
            (1, LatLon::new(48.8566, 2.3522)),
            (2, LatLon::new(48.8606, 2.3376)),
            (3, LatLon::new(51.5, -0.12)),
            (4, LatLon::new(f64::NAN, 0.0)),
        ];
        let far = cluster(&pts, 6.0, 60.0);
        assert_eq!(far.len(), 2, "{far:?}");
        assert_eq!(far.iter().map(|c| c.items.len()).sum::<usize>(), 3);
        let near = cluster(&pts, 16.0, 60.0);
        assert_eq!(near.len(), 3);
        assert!(cluster(&pts, f64::NAN, 60.0).len() <= 3);
        assert!(cluster::<i32>(&[], 5.0, 60.0).is_empty());
    }
}
