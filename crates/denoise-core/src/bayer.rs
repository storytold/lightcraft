//! Bayer geometry: which way a mosaic is laid out, and how a patch of it is packed into the four planes
//! (red, green on the red row, green on the blue row, blue) a denoise model takes.
//!
//! Every Bayer layout is the same 2 × 2 cell shifted by one pixel at most, so a layout is just where its
//! first red sample is. The cell grid starts one pixel before the picture when that red sample is in
//! column or row 1, and the picture is read with its edge mirrored (without repeating the edge sample, so
//! the colour of a mirrored sample is the colour that belongs at that place).

/// Where the first red sample of a Bayer mosaic sits, `(dx, dy)` in `{0, 1}²`: RGGB is `(0, 0)`, GRBG `(1, 0)`,
/// GBRG `(0, 1)`, BGGR `(1, 1)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub dx: usize,
    pub dy: usize,
}

/// The mosaic dimensions and cell origin for one square denoiser input tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileRegion {
    pub mosaic_size: (usize, usize),
    pub cell_origin: (isize, isize),
    pub tile_size: usize,
}

impl Layout {
    /// The layout of a 2 × 2 colour pattern, row-major, colours 0 = red, 1 = green, 2 = blue; `None` when
    /// it is not a Bayer cell (one red, one blue, the greens on the other diagonal).
    pub fn from_cell(cell: [u8; 4]) -> Option<Layout> {
        let at = |c: u8| cell.iter().position(|&v| v == c);
        let red = at(0)?;
        let blue = at(2)?;
        let greens = cell.iter().filter(|&&v| v == 1).count();
        // red and blue sit on one diagonal: indices 0+3 or 1+2
        if greens != 2 || red + blue != 3 {
            return None;
        }
        Some(Layout { dx: red % 2, dy: red / 2 })
    }

    /// The cell grid's origin: `-1` along an axis whose first red sample is at 1, so that cell 0 starts on a red column or row.
    fn origin(self) -> (isize, isize) {
        (isize::try_from(self.dx).unwrap_or(isize::MAX).saturating_neg(), isize::try_from(self.dy).unwrap_or(isize::MAX).saturating_neg())
    }

    /// Cells across and down covering a `width × height` mosaic.
    pub fn cells(self, width: usize, height: usize) -> (usize, usize) {
        let cells = |n: usize, phase: usize| n / 2 + (n % 2 + phase.min(1)).div_ceil(2);
        (cells(width, self.dx), cells(height, self.dy))
    }

    /// The mosaic position of cell `(cx, cy)`'s top-left sample (may lie outside the picture).
    pub fn position(self, cx: isize, cy: isize) -> (isize, isize) {
        let (ox, oy) = self.origin();
        (ox.saturating_add(cx.saturating_mul(2)), oy.saturating_add(cy.saturating_mul(2)))
    }
}

/// `i` mirrored into `0..n` (ping-pong, edge not repeated), so `-1` is `1` and `n` is `n - 2`. `n == 0` gives 0.
pub fn reflect(i: isize, n: usize) -> usize {
    if n <= 1 {
        return 0;
    }
    if let Some(period) = isize::try_from(n).ok().and_then(|n| (n - 1).checked_mul(2)) {
        let m = i.rem_euclid(period);
        return (if m < n as isize { m } else { period - m }) as usize;
    }
    let period = 2 * (n as i128 - 1);
    let m = (i as i128).rem_euclid(period);
    // The reflected value is in 0..n, so this conversion always fits usize.
    (if m < n as i128 { m } else { period - m }) as usize
}

/// Pack the `tile × tile` cells whose top-left cell is `(cx0, cy0)` into `out` as four planes `[R, G1, G2, B]`
/// (`4 · tile²` values, NCHW). G1 is the green beside the red sample, G2 the green under it. Samples outside
/// the picture are mirrored. Returns `false` (and writes nothing) when the sizes do not fit.
pub fn pack_tile(mosaic: &[f32], layout: Layout, region: TileRegion, out: &mut [f32]) -> bool {
    let (width, height) = region.mosaic_size;
    let (cx0, cy0) = region.cell_origin;
    let tile = region.tile_size;
    let Some(plane) = tile.checked_mul(tile) else { return false };
    let Some(values) = plane.checked_mul(4) else { return false };
    if width == 0
        || height == 0
        || tile == 0
        || layout.dx > 1
        || layout.dy > 1
        || width.checked_mul(height) != Some(mosaic.len())
        || out.len() != values
    {
        return false;
    }
    let Ok(last) = isize::try_from(tile - 1) else { return false };
    let (ox, oy) = layout.origin();
    for (start, origin) in [(cx0, ox), (cy0, oy)] {
        if start.checked_mul(2).and_then(|v| v.checked_add(origin)).is_none()
            || start.checked_add(last).and_then(|v| v.checked_mul(2)).and_then(|v| v.checked_add(origin)).and_then(|v| v.checked_add(1)).is_none()
        {
            return false;
        }
    }
    // the four samples of a cell, as (dx, dy) from its top-left sample
    const SUB: [(isize, isize); 4] = [(0, 0), (1, 0), (0, 1), (1, 1)];
    for (p, &(sx, sy)) in SUB.iter().enumerate() {
        let Some(dst) = out.get_mut(p * plane..(p + 1) * plane) else { return false };
        for (j, row) in dst.chunks_exact_mut(tile).enumerate() {
            let (_, y) = layout.position(cx0, cy0 + j as isize);
            let ry = reflect(y + sy, height);
            let Some(src) = mosaic.get(ry * width..(ry + 1) * width) else { return false };
            for (i, v) in row.iter_mut().enumerate() {
                let (x, _) = layout.position(cx0 + i as isize, cy0);
                *v = src.get(reflect(x + sx, width)).copied().unwrap_or(0.0);
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_sizes_and_positions_never_panic_or_write_output() {
        let layout = Layout::from_cell([0, 1, 1, 2]).unwrap();
        let mut out = [7.0; 4];
        for (w, h, tile, x) in [(usize::MAX, 2, 1, 0), (1, 1, usize::MAX, 0), (1, 1, 0, 0), (1, 1, 1, isize::MAX), (1, 1, 1, isize::MIN)] {
            assert!(!pack_tile(&[0.5], layout, TileRegion { mosaic_size: (w, h), cell_origin: (x, 0), tile_size: tile }, &mut out));
            assert_eq!(out, [7.0; 4]);
        }
        for n in [1, 2, isize::MAX as usize, usize::MAX] {
            for i in [isize::MIN, -1, 0, isize::MAX] {
                assert!(reflect(i, n) < n);
            }
        }
        assert_eq!(layout.cells(usize::MAX, usize::MAX), (usize::MAX / 2 + 1, usize::MAX / 2 + 1));
    }

    #[test]
    fn layouts_from_cells() {
        assert_eq!(Layout::from_cell([0, 1, 1, 2]), Some(Layout { dx: 0, dy: 0 }));
        assert_eq!(Layout::from_cell([1, 0, 2, 1]), Some(Layout { dx: 1, dy: 0 }));
        assert_eq!(Layout::from_cell([1, 2, 0, 1]), Some(Layout { dx: 0, dy: 1 }));
        assert_eq!(Layout::from_cell([2, 1, 1, 0]), Some(Layout { dx: 1, dy: 1 }));
        // not Bayer: two reds, a missing blue, greens on the wrong diagonal
        assert_eq!(Layout::from_cell([0, 0, 1, 2]), None);
        assert_eq!(Layout::from_cell([0, 1, 2, 1]), None);
        assert_eq!(Layout::from_cell([1, 1, 1, 1]), None);
    }

    #[test]
    fn reflection_keeps_the_colour_of_a_site() {
        assert_eq!(reflect(-1, 10), 1);
        assert_eq!(reflect(-2, 10), 2);
        assert_eq!(reflect(10, 10), 8);
        assert_eq!(reflect(11, 10), 7);
        assert_eq!(reflect(25, 10), 7);
        assert_eq!(reflect(5, 0), 0);
        assert_eq!(reflect(5, 1), 0);
        for i in -40..60 {
            assert_eq!(reflect(i, 11) % 2, (i.rem_euclid(2)) as usize, "parity at {i}");
        }
    }

    #[test]
    fn packing_puts_each_colour_in_its_plane_for_every_layout() {
        // a mosaic whose sample value says which colour it is: R = 10, G = 20, B = 30
        for cell in [[0u8, 1, 1, 2], [1, 0, 2, 1], [1, 2, 0, 1], [2, 1, 1, 0]] {
            let layout = Layout::from_cell(cell).unwrap();
            let (w, h) = (10usize, 8usize);
            let mosaic: Vec<f32> = (0..w * h).map(|i| [10.0, 20.0, 30.0][cell[(i / w % 2) * 2 + i % w % 2] as usize]).collect();
            let (nx, ny) = layout.cells(w, h);
            let tile = nx.max(ny);
            let mut out = vec![0.0; 4 * tile * tile];
            assert!(pack_tile(&mosaic, layout, TileRegion { mosaic_size: (w, h), cell_origin: (0, 0), tile_size: tile }, &mut out));
            let plane = tile * tile;
            for (p, want) in [10.0, 20.0, 20.0, 30.0].into_iter().enumerate() {
                assert!(out[p * plane..(p + 1) * plane].iter().all(|&v| v == want), "layout {cell:?} plane {p}");
            }
        }
    }

    #[test]
    fn g1_is_beside_the_red_sample_and_g2_under_it() {
        // RGGB with a distinct value at every site
        let (w, h) = (4, 4);
        let mosaic: Vec<f32> = (0..16).map(|i| i as f32).collect();
        let mut out = vec![0.0; 4 * 4];
        assert!(pack_tile(&mosaic, Layout { dx: 0, dy: 0 }, TileRegion { mosaic_size: (w, h), cell_origin: (0, 0), tile_size: 2 }, &mut out));
        // cell (0,0): R = site (0,0)=0, G1 = site (1,0)=1, G2 = site (0,1)=4, B = site (1,1)=5
        assert_eq!([out[0], out[4], out[8], out[12]], [0.0, 1.0, 4.0, 5.0]);
        // cell (1,0): sites (2,0)=2, (3,0)=3, (2,1)=6, (3,1)=7
        assert_eq!([out[1], out[5], out[9], out[13]], [2.0, 3.0, 6.0, 7.0]);
    }

    #[test]
    fn packing_rejects_wrong_sizes() {
        let mut out = vec![0.0; 16];
        assert!(!pack_tile(&[0.0; 15], Layout { dx: 0, dy: 0 }, TileRegion { mosaic_size: (4, 4), cell_origin: (0, 0), tile_size: 2 }, &mut out));
        assert!(!pack_tile(&[0.0; 16], Layout { dx: 0, dy: 0 }, TileRegion { mosaic_size: (4, 4), cell_origin: (0, 0), tile_size: 3 }, &mut out));
        assert!(!pack_tile(&[], Layout { dx: 0, dy: 0 }, TileRegion { mosaic_size: (0, 0), cell_origin: (0, 0), tile_size: 2 }, &mut out));
    }
}
