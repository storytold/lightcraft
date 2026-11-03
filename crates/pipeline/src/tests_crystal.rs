use super::*;

/// The field rendered at `long` px per long edge on an `n × n` patch starting at pixel `origin`.
fn patch(tile: &CrystalTile, long: f64, n: usize, origin: usize) -> Vec<f32> {
    let w = (TILE_PER_LONG / long) as f32;
    (0..n * n).map(|k| tile.field(((origin + k % n) as f32 + 0.5) * w, ((origin + k / n) as f32 + 0.5) * w, w)).collect()
}

fn std(v: &[f32]) -> f64 {
    let m = v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64;
    (v.iter().map(|&x| (x as f64 - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt()
}

/// Lag (px) at which the autocorrelation (mean of horizontal and vertical) falls to ½.
fn half_width(v: &[f32], n: usize) -> f64 {
    let m = v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64;
    let d: Vec<f64> = v.iter().map(|&x| x as f64 - m).collect();
    let var = d.iter().map(|x| x * x).sum::<f64>() / d.len() as f64;
    let corr = |k: usize, horizontal: bool| {
        let mut s = 0.0;
        let mut c = 0usize;
        for y in 0..n - if horizontal { 0 } else { k } {
            for x in 0..n - if horizontal { k } else { 0 } {
                let b = if horizontal { y * n + x + k } else { (y + k) * n + x };
                s += d[y * n + x] * d[b];
                c += 1;
            }
        }
        s / c as f64 / var
    };
    let mut out = 0.0;
    for horizontal in [true, false] {
        let mut prev = 1.0;
        let mut w = 16.0;
        for k in 1..16 {
            let c = corr(k, horizontal);
            if c <= 0.5 {
                w = k as f64 - 1.0 + (prev - 0.5) / (prev - c).max(1e-9);
                break;
            }
            prev = c;
        }
        out += w / 2.0;
    }
    out
}

fn tile_of(size: f64, roughness: f64) -> Arc<CrystalTile> {
    tile(CrystalLook::new(size, roughness), 7, true).expect("a tile")
}

#[test]
fn reference_render_has_unit_variance_and_no_mean() {
    let t = tile_of(25.0, 50.0);
    let v = patch(&t, REF_LONG, 600, 37);
    let m = v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64;
    assert!((std(&v) - 1.0).abs() < 0.05, "std {}", std(&v));
    assert!(m.abs() < 0.03, "mean {m}");
}

#[test]
fn small_render_is_the_downscaled_reference() {
    for (size, rough) in [(0.0, 50.0), (25.0, 50.0), (100.0, 50.0), (25.0, 0.0), (25.0, 100.0)] {
        let t = tile_of(size, rough);
        for k in [2usize, 4, 8] {
            let full = patch(&t, REF_LONG, 120 * k, 0);
            let n = 120;
            let down: Vec<f32> = (0..n * n)
                .map(|q| {
                    let (x, y) = (q % n, q / n);
                    let mut s = 0.0;
                    for dy in 0..k {
                        for dx in 0..k {
                            s += full[(y * k + dy) * n * k + x * k + dx];
                        }
                    }
                    s / (k * k) as f32
                })
                .collect();
            let small = patch(&t, REF_LONG / k as f64, n, 0);
            let worst = down.iter().zip(&small).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
            assert!(worst < 2e-3, "size {size} roughness {rough} 1/{k}: differs by {worst}");
        }
    }
}

#[test]
fn strength_matches_the_target_at_mid_grey() {
    for (a, s, r) in [(50.0, 25.0, 50.0), (25.0, 25.0, 50.0), (50.0, 0.0, 50.0), (50.0, 100.0, 50.0), (50.0, 25.0, 0.0)] {
        let g = CrystalGrain::new(&Grain { amount: a, size: s, roughness: r, seed: 3 }, REF_LONG).expect("grain");
        let n = 500;
        let lum: Vec<f32> = (0..n * n)
            .map(|k| {
                let (x, y) = ((k % n) as f32 + 0.5, (k / n) as f32 + 0.5);
                let e = g.apply([MID_GREY; 3], x / REF_LONG as f32, y / REF_LONG as f32);
                (0.2126 * e[0] + 0.7152 * e[1] + 0.0722 * e[2]) * 255.0
            })
            .collect();
        let want = strength_levels(a, s, r);
        assert!((std(&lum) / want - 1.0).abs() < 0.06, "{a}/{s}/{r}: {} levels, want {want}", std(&lum));
    }
}

#[test]
fn tones_keep_their_mean() {
    // the strongest settings in the shadows; near white, strong grain clips at 255 like any grain
    let cases = [
        (100.0, 0.0, &[5.0f32, 20.0, 40.0, 60.0, 128.0, 255.0][..]),
        (50.0, 0.0, &[200.0, 245.0][..]),
        (50.0, 25.0, &[20.0, 128.0, 245.0, 255.0][..]),
    ];
    for (amount, size, levels) in cases {
        let g = CrystalGrain::new(&Grain { amount, size, roughness: 50.0, seed: 3 }, REF_LONG).expect("grain");
        for &level in levels {
            let n = 400;
            let mut sum = 0.0f64;
            for k in 0..n * n {
                let (x, y) = ((k % n) as f32 + 0.5, (k / n) as f32 + 0.5);
                let e = g.apply([level / 255.0; 3], x / REF_LONG as f32, y / REF_LONG as f32);
                sum += (e[1].clamp(0.0, 1.0) * 255.0) as f64;
            }
            let shift = sum / (n * n) as f64 - level as f64;
            assert!(shift.abs() < 0.5, "{amount}/{size}: level {level} moved {shift:+.2}");
        }
    }
}

#[test]
fn hostile_settings_render_finite_or_no_grain() {
    let bad = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1e9, 1e9, 0.0];
    for &a in &bad {
        for &s in &bad {
            for &ppl in &[f64::NAN, 0.0, -5.0, 1.0, 24.0, 1e12, 6000.0] {
                let g = Grain { amount: if a == 0.0 { 50.0 } else { a }, size: s, roughness: s, seed: u32::MAX };
                if let Some(c) = CrystalGrain::new(&g, ppl) {
                    for (x, y) in [(0.0, 0.0), (-3.5, 1e7), (f32::NAN, 0.2), (0.5, f32::INFINITY), (1.0, 0.6667)] {
                        let e = c.apply([0.5, 0.2, 0.9], x, y);
                        assert!(e.iter().all(|v| v.is_finite()), "{g:?} at {ppl}: {e:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn seed_picks_the_pattern() {
    let look = CrystalLook::new(25.0, 50.0);
    let (a, b, c) = (tile(look, 1, true), tile(look, 1, true), tile(look, 2, true));
    let (Some(a), Some(b), Some(c)) = (a, b, c) else { panic!("tiles") };
    let (pa, pb, pc) = (patch(&a, REF_LONG, 64, 0), patch(&b, REF_LONG, 64, 0), patch(&c, REF_LONG, 64, 0));
    assert_eq!(pa, pb);
    assert_ne!(pa, pc);
}

/// A draft (a Size or Roughness drag) doesn't wait for a new tile: the last tile of the seed stands
/// in while the new one is drawn in the background, and then the drag gets it. A render that
/// waits always gets the exact tile, as does a draft for a seed with no tile yet.
#[test]
fn a_drag_takes_the_last_tile_while_the_next_is_drawn() {
    // a cache of its own: other tests' tiles (through any V3 render) would evict from the shared one
    static OWN: Cache = Mutex::new(Vec::new());
    let tile = |look, seed, wait| tile_in(&OWN, look, seed, wait);
    let (seed, fresh) = (0x0d4a_0001, 0x0d4a_0002);
    let (a, b) = (CrystalLook::new(30.0, 50.0), CrystalLook::new(31.0, 50.0));
    let exact = |look: CrystalLook, seed: u32| patch(&CrystalTile::new(look, seed).expect("tile"), REF_LONG, 32, 0);
    let first = tile(a, seed, true).expect("tile");
    assert_eq!(first.look, a);
    let stand_in = tile(b, seed, false).expect("tile");
    assert_eq!(stand_in.look, a, "the draft waited for its tile");
    // the drag's next renders get the new tile once it is drawn
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let drawn = loop {
        let t = tile(b, seed, false).expect("tile");
        if t.look == b {
            break t;
        }
        assert!(std::time::Instant::now() < deadline, "the background tile never arrived");
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert_eq!(patch(&drawn, REF_LONG, 32, 0), exact(b, seed));
    assert_eq!(patch(&tile(b, seed, true).expect("tile"), REF_LONG, 32, 0), exact(b, seed));
    // nothing drawn for this seed: nothing to stand in, the draft draws it
    assert_eq!(patch(&tile(a, fresh, false).expect("tile"), REF_LONG, 32, 0), exact(a, fresh));
}

/// Grain width (autocorrelation half-width at mid grey, 6000 px) at Lightroom Classic's
/// measured settings (open PR #1060's table); Size 0 is as fine as uncorrelated pixels get (0.5).
#[test]
fn width_matches_lightroom_classic() {
    let lrc = [(0.0, 50.0, 0.5), (25.0, 50.0, 1.13), (100.0, 50.0, 3.38), (25.0, 0.0, 0.86), (25.0, 100.0, 1.54)];
    for (s, r, want) in lrc {
        let got = half_width(&patch(&tile_of(s, r), REF_LONG, 360, 0), 360);
        assert!((got / want - 1.0).abs() < 0.1, "size {s} roughness {r}: width {got:.3}, want {want}");
    }
}

/// Prints how long drawing a tile takes per setting (`--ignored --nocapture`, alone).
#[test]
#[ignore]
fn tile_timing_report() {
    for (s, r) in [(0.0, 50.0), (10.0, 50.0), (25.0, 50.0), (50.0, 50.0), (100.0, 50.0), (25.0, 0.0), (25.0, 100.0), (100.0, 100.0)] {
        let look = CrystalLook::new(s, r);
        let mut best = f64::INFINITY;
        for seed in 0..5 {
            let start = std::time::Instant::now();
            let t = CrystalTile::new(look, 100 + seed);
            best = best.min(start.elapsed().as_secs_f64() * 1e3);
            assert!(t.is_some());
        }
        eprintln!("size {s:>5} roughness {r:>5}: {best:.1} ms ({:?})", look);
    }
}

/// Fits the median crystal width per Size (Roughness 50) and the Roughness factors at Size 25 to
/// Lightroom Classic's grain widths (`--ignored --nocapture`; paste into the look tables).
#[test]
#[ignore]
fn fit_widths() {
    let width_of = |look: CrystalLook| {
        let mut w = 0.0;
        for seed in [11, 12] {
            let t = CrystalTile::new(look, seed).expect("tile");
            w += half_width(&patch(&t, REF_LONG, 360, 0), 360) / 2.0;
        }
        w
    };
    let fit = |s: f64, r: f64, want: f64| {
        let base = CrystalLook::new(s, r);
        let (mut lo, mut hi) = (0.05f64, 40.0f64);
        for _ in 0..18 {
            let mid = (lo * hi).sqrt();
            if width_of(CrystalLook { width: mid, ..base }) < want { lo = mid } else { hi = mid }
        }
        let d = (lo * hi).sqrt();
        (d, width_of(CrystalLook { width: d, ..base }))
    };
    for (s, want) in [(0.0, 0.43), (10.0, 0.71), (25.0, 1.13), (50.0, 1.70), (75.0, 2.54), (100.0, 3.38)] {
        let (d, got) = fit(s, 50.0, want);
        eprintln!("size {s}: width {d:.3} gives {got:.3} (want {want})");
    }
    let (d50, _) = fit(25.0, 50.0, 1.13);
    for (r, want) in [(0.0, 0.86), (100.0, 1.54)] {
        let (d, got) = fit(25.0, r, want);
        eprintln!("roughness {r}: factor {:.3} gives {got:.3} (want {want})", d / d50);
    }
}

/// Autocorrelation at lags 1–4 and the strength kept by box downscales of 2–16 (relative), of
/// candidate looks (`--ignored --nocapture`): compares a look's texture with a reference's.
#[test]
#[ignore]
fn texture_report() {
    let profile = |look: CrystalLook| {
        let n = 512;
        let (mut acf, mut down) = ([0.0; 4], [0.0; 4]);
        for seed in [21, 22] {
            let t = CrystalTile::new(look, seed).expect("tile");
            let v = patch(&t, REF_LONG, n, 0);
            let s0 = std(&v);
            for (k, a) in acf.iter_mut().enumerate() {
                let lag = k + 1;
                let mut s = 0.0f64;
                for y in 0..n {
                    for x in 0..n - lag {
                        s += (v[y * n + x] * v[y * n + x + lag] + v[x * n + y] * v[(x + lag) * n + y]) as f64;
                    }
                }
                *a += s / (2 * n * (n - lag)) as f64 / (s0 * s0) / 2.0;
            }
            for (k, d) in down.iter_mut().enumerate() {
                let f = 2usize << k;
                let m = n / f;
                let b: Vec<f32> = (0..m * m)
                    .map(|q| {
                        let (x, y) = (q % m * f, q / m * f);
                        (0..f * f).map(|i| v[(y + i / f) * n + x + i % f]).sum::<f32>() / (f * f) as f32
                    })
                    .collect();
                *d += std(&b) / s0 / 2.0;
            }
        }
        (acf, down)
    };
    for (s, r) in [(25.0, 50.0), (50.0, 50.0), (100.0, 50.0), (25.0, 0.0), (25.0, 100.0)] {
        let (a, d) = profile(CrystalLook::new(s, r));
        eprintln!(
            "size {s} roughness {r}: acf {:.2} {:.2} {:.2} {:.2}  down {:.2} {:.2} {:.2} {:.3}",
            a[0], a[1], a[2], a[3], d[0], d[1], d[2], d[3]
        );
    }
}

/// A row computed with shared box edges equals the per-pixel boxes: any size, mirrored rows, rows
/// across the tori's seams and far from the origin.
#[test]
fn row_field_matches_per_pixel_field() {
    let t = tile_of(25.0, 50.0);
    for (w, sign, x, y) in [
        (2.0f32, 1.0f32, 3.3f32, 7.9f32),
        (2.0, -1.0, 9000.2, 1275.0),
        (8.0, 1.0, 11990.0, 2260.5),
        (0.7, 1.0, 1279.6, 0.1),
        (31.0, -1.0, 400.0, 1133.9),
    ] {
        for along_x in [true, false] {
            let mut row = vec![0.0f32; 700];
            t.row_field(x, y, w, sign * w, along_x, &mut row);
            for (k, &v) in row.iter().enumerate() {
                let d = sign * w * k as f32;
                let want = if along_x { t.field(x + d, y, w) } else { t.field(x, y + d, w) };
                assert!((v - want).abs() < 2e-3, "w {w} sign {sign} x {along_x} from ({x}, {y}), pixel {k}: {v} vs {want}");
            }
        }
    }
}

/// Per-pixel cost of the grain on one thread (`--ignored --nocapture`).
#[test]
#[ignore]
fn pixel_cost_report() {
    let g = CrystalGrain::new(&Grain { amount: 50.0, size: 25.0, roughness: 50.0, seed: 1 }, REF_LONG).expect("grain");
    let (w, h) = (4000usize, 600usize);
    let mut sink = 0.0f32;
    let t0 = std::time::Instant::now();
    for y in 0..h {
        for x in 0..w {
            sink += g.tile.field((x as f32 + 0.5) * 2.0, (y as f32 + 0.5) * 2.0, 2.0);
        }
    }
    let per_pixel = t0.elapsed().as_secs_f64() / (w * h) as f64 * 1e9;
    let mut row = vec![0.0; w];
    let t1 = std::time::Instant::now();
    for y in 0..h {
        g.tile.row_field(1.0, (y as f32 + 0.5) * 2.0, 2.0, 2.0, true, &mut row);
        sink += row[7];
    }
    let per_row = t1.elapsed().as_secs_f64() / (w * h) as f64 * 1e9;
    let t3 = std::time::Instant::now();
    for y in 0..h {
        g.tile.row_field((y as f32 + 0.5) * 2.0, 1.0, 2.0, 2.0, false, &mut row);
        sink += row[7];
    }
    eprintln!("column {:.1} ns/px", t3.elapsed().as_secs_f64() / (w * h) as f64 * 1e9);
    let t2 = std::time::Instant::now();
    for y in 0..h {
        for x in 0..w {
            sink += g.apply_field([0.4, 0.5, 0.6], (x + y) as f32 * 1e-4)[1];
        }
    }
    let tone = t2.elapsed().as_secs_f64() / (w * h) as f64 * 1e9;
    eprintln!("field {per_pixel:.1} ns/px, row {per_row:.1} ns/px, tone {tone:.1} ns/px ({sink})");
}
