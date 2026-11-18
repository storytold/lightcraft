//! Generated demo scenes kept on disk, for test runs.
//!
//! A demo scene is costly to generate ([`crate::demo::scene_pixels`]): a 24 MP original is about a
//! minute of CPU time, and the previews around it add to that. The in-memory cache shares each
//! one within a process, but a test runner that starts one process per test (cargo-nextest)
//! generates them again for every test: the UI suite spent over 90 % of its time there. With a
//! folder set, a scene is generated once and read back by every later process.
//!
//! Off unless asked for: `<PREFIX>_DEMO_SCENE_CACHE=<folder>`, or [`use_build_dir`] (unit tests
//! call it; the folder is `tmp/demo-scenes` in cargo's target directory). The app never turns it
//! on by itself.
//!
//! A file is named after the scene, the size asked for and a fingerprint of the scene's pixels at
//! a small size, so a change to how a scene is drawn makes new files instead of serving old
//! pixels. Anything wrong with a file (missing, short, another size, unreadable folder, full disk)
//! means "not cached": the scene is generated as before.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use lcc_raster::Rgb32f;

const MAGIC: &[u8; 8] = b"LCSCENE1";
const HEADER: usize = 24;
/// Nothing is added once the folder holds this much (an original is 288 MB).
const FOLDER_LIMIT: u64 = 16 << 30;
/// Longest edge of the render the fingerprint is taken from.
const FINGERPRINT_EDGE: usize = 48;

static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

fn from_env() -> Option<PathBuf> {
    lcc_brand::env_os("DEMO_SCENE_CACHE").filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// `tmp/demo-scenes` in the target directory this executable was built into (the nearest folder
/// above it that cargo marked with `CACHEDIR.TAG`).
fn build_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let target = exe.ancestors().skip(1).find(|dir| dir.join("CACHEDIR.TAG").is_file())?;
    Some(target.join("tmp").join("demo-scenes"))
}

/// Keep generated demo scenes in the build's target directory from now on (the environment
/// variable, when set, names the folder instead). For test binaries; does nothing when the
/// executable isn't inside a cargo target directory.
pub fn use_build_dir() {
    DIR.get_or_init(|| from_env().or_else(build_dir));
}

fn dir() -> Option<&'static Path> {
    DIR.get_or_init(|| from_env().or_else(|| if cfg!(test) { build_dir() } else { None })).as_deref()
}

/// Where one scene at one size is kept.
pub(crate) struct Entry {
    path: PathBuf,
    max_edge: usize,
}

impl Entry {
    /// `None` when no folder is set.
    pub(crate) fn of(scene: &lcc_scenes::Scene, max_edge: usize) -> Option<Entry> {
        let dir = dir()?;
        let small = scene.render_fit_serial(FINGERPRINT_EDGE);
        // FNV-1a over the small render's bits
        let mut fingerprint = 0xcbf2_9ce4_8422_2325_u64;
        for value in small.data.iter().flatten() {
            for byte in value.to_bits().to_le_bytes() {
                fingerprint = (fingerprint ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
            }
        }
        let name = format!("{:?}-{}-{}x{}-{}-{:016x}.rgbf", scene.kind, scene.seed, scene.width, scene.height, max_edge, fingerprint);
        Some(Entry { path: dir.join(name), max_edge })
    }

    pub(crate) fn load(&self) -> Option<Rgb32f> {
        decode(&std::fs::read(&self.path).ok()?, self.max_edge)
    }

    /// Best effort: written beside its final name and renamed, so a process reading meanwhile
    /// sees a whole file or none.
    pub(crate) fn store(&self, image: &Rgb32f) {
        let Some(dir) = self.path.parent() else { return };
        if std::fs::create_dir_all(dir).is_err() || folder_bytes(dir) >= FOLDER_LIMIT {
            return;
        }
        let mut partial = self.path.clone().into_os_string();
        partial.push(format!(".{}.partial", std::process::id()));
        let partial = PathBuf::from(partial);
        if std::fs::write(&partial, encode(image)).is_err() || std::fs::rename(&partial, &self.path).is_err() {
            let _ = std::fs::remove_file(&partial);
        }
    }
}

fn folder_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).fold(0, u64::saturating_add)
}

fn encode(image: &Rgb32f) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER.saturating_add(image.data.len().saturating_mul(12)));
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(image.width as u64).to_le_bytes());
    out.extend_from_slice(&(image.height as u64).to_le_bytes());
    for value in image.data.iter().flatten() {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// The image in `bytes`, when it is a whole one no larger than `max_edge` on its longest edge.
fn decode(bytes: &[u8], max_edge: usize) -> Option<Rgb32f> {
    let (header, body) = bytes.split_at_checked(HEADER)?;
    let (magic, dims) = header.split_at_checked(MAGIC.len())?;
    let (width, height) = dims.split_at_checked(8)?;
    let width = usize::try_from(u64::from_le_bytes(width.try_into().ok()?)).ok()?;
    let height = usize::try_from(u64::from_le_bytes(height.try_into().ok()?)).ok()?;
    let pixels = width.checked_mul(height)?;
    if magic != MAGIC || pixels == 0 || width.max(height) > max_edge.max(1) || pixels.checked_mul(12)? != body.len() {
        return None;
    }
    let mut data = Vec::with_capacity(pixels);
    for &[r0, r1, r2, r3, g0, g1, g2, g3, b0, b1, b2, b3] in body.as_chunks::<12>().0 {
        data.push([f32::from_le_bytes([r0, r1, r2, r3]), f32::from_le_bytes([g0, g1, g2, g3]), f32::from_le_bytes([b0, b1, b2, b3])]);
    }
    Some(Rgb32f { width, height, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_reads_back_bit_for_bit_and_damaged_files_are_not_images() {
        let image = Rgb32f::from_fn(5, 3, |x, y| [x as f32 * 0.1, -(y as f32), f32::MIN_POSITIVE]);
        let bytes = encode(&image);
        let back = decode(&bytes, 5).expect("a whole file");
        assert_eq!((back.width, back.height), (5, 3));
        assert!(back.data.iter().flatten().zip(image.data.iter().flatten()).all(|(a, b)| a.to_bits() == b.to_bits()));

        // larger than the size asked for
        assert!(decode(&bytes, 4).is_none());
        let mut huge = bytes.clone();
        huge[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
        let mut wrong_magic = bytes.clone();
        wrong_magic[0] ^= 1;
        let mut empty = bytes[..HEADER].to_vec();
        empty[8..24].fill(0);
        for (what, damaged) in [
            ("empty", &[][..]),
            ("header only", &bytes[..HEADER]),
            ("cut short", &bytes[..bytes.len() - 1]),
            ("too long", &[&bytes[..], &[0]].concat()),
            ("a size that overflows", &huge),
            ("another format", &wrong_magic),
            ("no pixels", &empty),
        ] {
            assert!(decode(damaged, 5).is_none(), "{what}");
        }
    }

    #[test]
    fn a_stored_scene_is_loaded_instead_of_generated() {
        let dir = lcc_testkit::TempDir::new("lcc-scene-disk");
        let entry = Entry { path: dir.path().join("deeper").join("scene.rgbf"), max_edge: 8 };
        assert!(entry.load().is_none(), "nothing stored yet");
        let image = Rgb32f::from_fn(8, 4, |x, y| [x as f32, y as f32, 0.5]);
        entry.store(&image);
        assert_eq!(entry.load().map(|i| i.data), Some(image.data));
        assert_eq!(std::fs::read_dir(dir.path().join("deeper")).unwrap().count(), 1, "no partial file is left behind");
    }
}
