//! The perceptual hash (pHash) the duplicate finder compares copies with ([`crate::duplicates`]):
//! a 32×32 luminance grid of the picture, an orthonormal DCT-II of it, and the 8×8 low-frequency
//! block thresholded at its median — the classic 64-bit pHash. Copies that differ by resizing,
//! re-compression or a gamma shift keep most low-frequency structure, so their hashes stay within
//! [`MATCH_DISTANCE`] bits, while different pictures fall far outside it.
//!
//! Two front ends cover the finder's sources: sRGB 8-bit pictures (a raw's camera JPEG, a decoded
//! image file) and linear Rec.709 (a clip's poster frame, [`crate::media::video_poster`]). Each
//! compares like with like — videos with videos, photos with photos — so the gamma difference
//! between the front ends never lands inside a comparison.

use lcc_raster::{Image, Rgb32f, Rgba8};

/// Bump when the hash changes what it computes (the cache key holds it).
pub const PHASH_VERSION: u64 = 1;

/// The luminance grid's side.
const N: usize = 32;
/// The low-frequency block's side whose bits the hash keeps.
const LOW: usize = 8;
/// Two pictures whose hashes differ in at most this many bits count as copies of the same
/// picture. The usual 64-bit pHash cut-off; [`crate::duplicates`] pairs groups on it.
pub const MATCH_DISTANCE: u32 = 10;

/// Rec.709 luminance of an sRGB pixel, 0–1 (the eye finder's weights).
fn luma8(p: [u8; 4]) -> f32 {
    (0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2])) / 255.0
}

/// Rec.709 luminance of a linear pixel.
fn luma32(p: [f32; 3]) -> f32 {
    0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
}

/// The picture's luminance, box-averaged onto the N×N grid: each cell the mean of the pixels
/// inside it (the averaging is the low-pass that makes the hash survive a smaller or larger copy).
fn luma_grid<T: Copy>(img: &Image<T>, w: usize, h: usize, luma: impl Fn(T) -> f32) -> Box<[f32; N * N]> {
    let mut grid = Box::new([0.0f32; N * N]);
    for gy in 0..N {
        let y0 = (gy * h) / N;
        let y1 = (((gy + 1) * h) / N).max(y0 + 1).min(h);
        for gx in 0..N {
            let x0 = (gx * w) / N;
            let x1 = (((gx + 1) * w) / N).max(x0 + 1).min(w);
            let (mut sum, mut n) = (0.0f32, 0.0f32);
            for y in y0..y1 {
                let row = y * w;
                for x in x0..x1 {
                    if let Some(p) = img.data.get(row + x) {
                        sum += luma(*p);
                        n += 1.0;
                    }
                }
            }
            grid[gy * N + gx] = if n > 0.0 { sum / n } else { 0.0 };
        }
    }
    grid
}

/// The DCT-II basis: `t[k][x] = c(k) · cos(π(2x+1)k / 2N)` with the orthonormal scale factors.
fn table() -> &'static [[f32; N]; N] {
    static TABLE: std::sync::OnceLock<Box<[[f32; N]; N]>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = Box::new([[0.0f32; N]; N]);
        for (k, row) in t.iter_mut().enumerate() {
            let c = (if k == 0 { 1.0 } else { 2.0 } / N as f32).sqrt();
            for (x, v) in row.iter_mut().enumerate() {
                *v = c * (std::f32::consts::PI * (2 * x + 1) as f32 * k as f32 / (2 * N) as f32).cos();
            }
        }
        t
    })
}

/// The hash of a luminance grid: the orthonormal DCT-II's low-frequency `LOW × LOW` block,
/// each coefficient one bit set where it is above the block's median. Roughly half the bits are
/// set whatever the picture, so every picture's hash carries the same amount of distinction.
pub fn hash_grid(grid: &[f32; N * N]) -> u64 {
    let t = table();
    // rows: tmp[k][x] = Σy t[k][y]·grid[y][x]; cols: dct[k][l] = Σx t[l][x]·tmp[k][x]
    let mut tmp = [[0.0f32; N]; N];
    for (k, row) in tmp.iter_mut().enumerate() {
        for x in 0..N {
            let mut sum = 0.0;
            for (y, tk) in t[k].iter().enumerate() {
                sum += tk * grid[y * N + x];
            }
            row[x] = sum;
        }
    }
    let mut block = [0.0f32; LOW * LOW];
    for k in 0..LOW {
        for l in 0..LOW {
            let mut sum = 0.0;
            for (x, tl) in t[l].iter().enumerate() {
                sum += tl * tmp[k][x];
            }
            block[k * LOW + l] = sum;
        }
    }
    let mut sorted = block;
    sorted.sort_by(f32::total_cmp);
    let median = (sorted[LOW * LOW / 2 - 1] + sorted[LOW * LOW / 2]) / 2.0;
    let mut hash = 0u64;
    for (i, v) in block.iter().enumerate() {
        if *v > median {
            hash |= 1 << i;
        }
    }
    hash
}

/// The hash of an sRGB 8-bit picture (a raw's camera JPEG, a decoded image file).
pub fn hash_rgba8(img: &Rgba8) -> u64 {
    hash_grid(&luma_grid(img, img.width, img.height, luma8))
}

/// The hash of a linear Rec.709 picture (a clip's poster frame).
pub fn hash_rgb32f(img: &Rgb32f) -> u64 {
    hash_grid(&luma_grid(img, img.width, img.height, luma32))
}

/// How many of the 64 bits two hashes differ in.
pub fn distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resized_or_recompressed_copy_stays_within_the_match_distance() {
        // one gamma domain throughout (the production paths are too: stills hash sRGB pictures,
        // clips hash their linear posters, and the two never compare with each other)
        let to_srgb = |img: &lcc_raster::Rgb32f| -> Rgba8 {
            let mut out = Rgba8::new(img.width, img.height);
            for (o, p) in out.data.iter_mut().zip(img.data.iter()) {
                let f = |c: f32| (c.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0).round() as u8;
                *o = [f(p[0]), f(p[1]), f(p[2]), 255];
            }
            out
        };
        for scene in lcc_scenes::demo_library().iter().take(4) {
            let reference = hash_rgba8(&to_srgb(&scene.render(512, 384)));
            // a half-size render of the same scene is the resized copy
            let half = hash_rgba8(&to_srgb(&scene.render(256, 192)));
            assert!(distance(reference, half) <= MATCH_DISTANCE, "scene {}: resized copy {} bits away", scene.id, distance(reference, half));
            // a JPEG round trip of the sRGB pixels is the re-compressed copy
            let srgb = to_srgb(&scene.render(512, 384));
            let jpeg = lcc_preview::encode_jpeg(&srgb).unwrap();
            let back = lcc_preview::decode_jpeg(&jpeg).unwrap();
            assert!(
                distance(reference, hash_rgba8(&back)) <= MATCH_DISTANCE,
                "scene {}: recompressed copy {} bits away",
                scene.id,
                distance(reference, hash_rgba8(&back)),
            );
        }
    }

    #[test]
    fn different_pictures_stay_apart_and_spread_their_bits() {
        let scenes = lcc_scenes::demo_library();
        let hashes: Vec<u64> = scenes.iter().take(6).map(|s| hash_rgb32f(&s.render(256, 192))).collect();
        for (scene, other) in scenes.iter().zip(&hashes).skip(1).take(4) {
            assert!(distance(hashes[0], *other) > MATCH_DISTANCE, "scene {} vs scene 0: only {} bits apart", scene.id, distance(hashes[0], *other));
        }
        // every picture sets a middle share of its 64 bits, so hashes carry the distinction
        for (scene, hash) in scenes.iter().zip(&hashes) {
            let bits = hash.count_ones();
            assert!((16..48).contains(&bits), "scene {}: {bits} of 64 bits set", scene.id);
        }
    }
}
