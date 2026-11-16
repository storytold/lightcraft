//! Test-only fixtures: a small, real, file-backed photo library.
//!
//! The demo library generates 24 MP procedural scenes, which Nextest regenerates in every
//! process. The library/lifecycle tests only need an active detail photo whose file really
//! decodes, so this fixture writes 64×48 gradient PNGs and imports them as the only photos.

use std::path::PathBuf;

use lightcraft_catalog::{Op, Photo, Source};

use crate::{LightcraftApp, Services};

/// A temporary folder holding small PNGs; removes only its own folder when dropped.
///
/// Keep this guard alive until after the [`Headless`](crate::headless::Headless) built from
/// [`SyntheticLibrary::app`] is dropped: the renderer reads the photo file from disk.
pub(crate) struct SyntheticLibrary {
    dir: PathBuf,
    photos: Vec<PathBuf>,
}

impl SyntheticLibrary {
    /// A fresh folder for `tag`, with a deterministic 64×48 gradient `a.png`.
    pub(crate) fn new(tag: &str) -> SyntheticLibrary {
        SyntheticLibrary::with_photos(tag, 1)
    }

    /// A fresh folder for `tag`, with `n` distinct 64×48 gradient PNGs (`a.png`, `b.png`, …).
    pub(crate) fn with_photos(tag: &str, n: usize) -> SyntheticLibrary {
        assert!((1..=26).contains(&n), "fixture photo names use a through z");
        let dir = std::env::temp_dir().join(format!("lc-ui-fixture-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (w, h) = (64usize, 48usize);
        let mut photos = Vec::new();
        for i in 0..n {
            let name = format!("{}.png", char::from(b'a' + i as u8));
            let data: Vec<[u8; 4]> = (0..w * h)
                .map(|p| {
                    let (x, y) = (p % w, p / w);
                    [((x * 3 + i * 17) % 256) as u8, ((y * 4 + i * 9) % 256) as u8, ((x + y + i) % 256) as u8, 255]
                })
                .collect();
            let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
            let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
            let path = dir.join(name);
            std::fs::write(&path, png).unwrap();
            photos.push(path);
        }
        SyntheticLibrary { dir, photos }
    }

    /// A session whose library holds only the fixture photos, the first active and selected, with
    /// the file system loaders installed so Detail really decodes the PNGs.
    pub(crate) fn app(&self) -> LightcraftApp {
        let mut session = lightcraft_engine::Session::new().with_fs();
        let mut ids = Vec::new();
        for (i, path) in self.photos.iter().enumerate() {
            let id = session.catalog.alloc_photo_id();
            // distinct capture times keep the default (newest first) view order deterministic
            let captured = format!("2026-01-01T10:{:02}:00", i % 60);
            let mut photo = Photo::new(
                id,
                Source::File { path: path.to_string_lossy().into_owned() },
                path.file_name().map_or("photo.png", |n| n.to_str().unwrap_or("photo.png")),
                "PNG",
                64,
                48,
                &captured,
            );
            photo.captured = Some(captured);
            session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
            ids.push(id);
        }
        if let Some(first) = ids.first() {
            session.selection = lightcraft_engine::Selection::single(*first);
        }
        LightcraftApp::new(session, Services { png: None, ..Default::default() })
    }
}

impl Drop for SyntheticLibrary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
