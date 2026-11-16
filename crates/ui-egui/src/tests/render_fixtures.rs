//! Test-only, real, file-backed photo fixtures for the render tests.
//!
//! The input/modal tests use a tiny `SyntheticLibrary` (64×48) that cannot exercise native-size
//! decoding, a 1300×700 region window, a >16 MiB decoded original or Compare's two originals.
//! These fixtures write real landscape 3:2 PNGs at the demo library's 24 MP size and register
//! them as ordinary `Source::File` photos, so preview, Before/After and slideshow tests decode
//! and render through the same path as a user's files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use lightcraft_catalog::{Op, Photo, Source};
use lightcraft_codecs::{EncodeImage, EncodeMeta, Samples, encode_png};
use lightcraft_engine::{Selection, Session};

/// The demo library's photo size: landscape 3:2, 24 MP.
pub(crate) const WIDTH: u32 = 6000;
pub(crate) const HEIGHT: u32 = 4000;
/// The long edge the native-resolution tests look for.
pub(crate) const LONG_EDGE: usize = WIDTH as usize;
/// The pixel count of every fixture photo.
pub(crate) const MEGAPIXELS: u64 = WIDTH as u64 * HEIGHT as u64;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
type FixtureCache = Mutex<HashMap<(String, usize), Weak<FixtureFiles>>>;
/// One generated set per (tag, count) in this test process: parallel tests share the same
/// read-only PNGs instead of each encoding 2–3 24 MP images (and spiking memory).
static FIXTURES: OnceLock<FixtureCache> = OnceLock::new();

struct FixtureFiles {
    dir: PathBuf,
    paths: Vec<String>,
    sizes: Vec<u64>,
}

impl Drop for FixtureFiles {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Owns (with other users) a temporary directory of real PNGs. Keep it alive until after the
/// [`crate::headless::Headless`] built from [`RenderFixture::session`] has dropped: the renderer
/// reads these paths lazily.
pub(crate) struct RenderFixture {
    files: Arc<FixtureFiles>,
}

impl RenderFixture {
    /// Write (or reuse) `count` deterministic, smooth, non-neutral landscape gradient PNGs.
    pub(crate) fn new(tag: &str, count: usize) -> Self {
        assert!(count > 0, "a render fixture needs at least one photo");
        let key = (tag.to_string(), count);
        let cache = FIXTURES.get_or_init(|| Mutex::new(HashMap::new()));
        let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(files) = cache.get(&key).and_then(Weak::upgrade) {
            return RenderFixture { files };
        }
        let files = Arc::new(build_files(tag, count));
        cache.insert(key, Arc::downgrade(&files));
        RenderFixture { files }
    }

    /// A session with `with_fs` loaders, one default-edited `Source::File` photo per fixture,
    /// distinct ids and captured times in a stable order, with the first visible photo selected.
    pub(crate) fn session(&self) -> Session {
        let mut session = Session::new().with_fs();
        for (i, path) in self.files.paths.iter().enumerate() {
            let id = session.catalog.alloc_photo_id();
            let name = format!("RENDER_{i:04}.PNG");
            let mut photo = Photo::new(id, Source::File { path: path.clone() }, &name, "PNG", WIDTH, HEIGHT, "2026-01-01T10:00:00");
            // Distinct captured times keep `visible_cloned()` deterministic (the default sort is
            // capture date, newest first) and make every PhotoId its own file.
            photo.captured = Some(format!("2026-01-01T10:{:02}:{:02}", i / 60, i % 60));
            photo.file_size = self.files.sizes.get(i).copied().unwrap_or(0);
            session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).expect("add a render fixture photo");
        }
        session.keep_active_in_view();
        let first = session.visible_cloned().first().copied().expect("the fixture has a visible photo");
        session.selection = Selection::single(first);
        session
    }

    pub(crate) fn paths(&self) -> &[String] {
        &self.files.paths
    }
}

fn build_files(tag: &str, count: usize) -> FixtureFiles {
    let n = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("lightcraft-render-fixtures-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the render fixture directory");

    let mut paths = Vec::with_capacity(count);
    let mut sizes = Vec::with_capacity(count);
    for i in 0..count {
        let path = dir.join(format!("render-{i:02}.png"));
        let size = write_gradient(&path, i);
        paths.push(path.to_string_lossy().into_owned());
        sizes.push(size);
    }
    FixtureFiles { dir, paths, sizes }
}

/// A smooth, non-wrapping, non-neutral gradient: R rises with x, G with y, B falls with x. The
/// superimposed spatial waves make shifted pixels distinguishable in alignment regressions. The
/// centre stays non-neutral, so the white-balance picker sees a real colour and an
/// exposure lift of 1.5 EV is unmistakably brighter. `variant` shifts it slightly so no two
/// fixture files have the same bytes.
fn write_gradient(path: &Path, variant: usize) -> u64 {
    let w = WIDTH as usize;
    let h = HEIGHT as usize;
    let shift_r = (variant % 7) as f32 * 4.0;
    let shift_b = (variant % 5) as f32 * 3.0;
    let r_base: Vec<u8> = (0..w)
        .map(|x| {
            let u = x as f32 / (w - 1) as f32;
            clamp8(80.0 + 120.0 * u + shift_r + 44.0 * (std::f32::consts::TAU * x as f32 / 160.0).sin())
        })
        .collect();
    let b_base: Vec<u8> = (0..w)
        .map(|x| {
            let u = x as f32 / (w - 1) as f32;
            clamp8(40.0 + 100.0 * (1.0 - u) + shift_b + 22.0 * (std::f32::consts::TAU * x as f32 / 160.0).sin())
        })
        .collect();

    let mut bytes = vec![0u8; w * h * 4];
    for (y, row) in bytes.chunks_exact_mut(w * 4).enumerate() {
        let v = y as f32 / (h - 1) as f32;
        let wave = (std::f32::consts::TAU * y as f32 / 192.0).sin();
        let g = clamp8(60.0 + 100.0 * v + 44.0 * wave);
        for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            pixel[0] = r_base[x];
            pixel[1] = g;
            pixel[2] = clamp8(f32::from(b_base[x]) + 22.0 * wave);
            pixel[3] = 255;
        }
    }

    let image = EncodeImage::new(WIDTH, HEIGHT, 4, Samples::U8(&bytes));
    let encoded = encode_png(&image, &EncodeMeta::default()).expect("encode a render fixture PNG");
    std::fs::write(path, &encoded).expect("write a render fixture PNG");
    encoded.len() as u64
}

fn clamp8(v: f32) -> u8 {
    v.clamp(0.0, 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture must stay a real 24 MP landscape 3:2 PNG: a fast little image would silently
    /// weaken every render assertion that depends on native pixels and a >16 MiB decoded source.
    #[test]
    fn render_fixtures_are_real_24mp_landscape_pngs() {
        assert_eq!((WIDTH, HEIGHT), (6000, 4000));
        assert_eq!(MEGAPIXELS, 24_000_000, "the demo photos these replace are 24 MP");
        let fixture = RenderFixture::new("preview", 2);
        assert_eq!(fixture.paths().len(), 2);
        for path in fixture.paths() {
            let bytes = std::fs::read(path).expect("read the fixture PNG");
            let header = lightcraft_codecs::read_header(&bytes).expect("read the fixture PNG header");
            assert_eq!((header.width, header.height), (WIDTH, HEIGHT), "{path}");
            assert_eq!(header.format, lightcraft_codecs::Format::Png, "{path}");
            // A full-resolution decode is width × height linear RGB f32 samples, well over the
            // 16 MiB budget `a_zoomed_original_over_the_budget...` checks.
            let decoded_bytes = u64::from(header.width) * u64::from(header.height) * 3 * 4;
            assert!(decoded_bytes > (16 << 20), "{path}: a decoded source of {decoded_bytes} bytes");
        }
        // The files are distinct so each PhotoId is its own original.
        assert_ne!(fixture.paths()[0], fixture.paths()[1]);
    }
}
