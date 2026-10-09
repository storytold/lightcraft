//! The demo library: procedurally generated photos with realistic metadata, albums and a few edits.

use std::sync::Arc;

use dac_catalog::{Album, Flag, Meta, Op, Photo, Source};
use dac_develop::DevelopSettings;

use crate::Session;

pub fn load(s: &mut Session) {
    let scenes = dac_scenes::demo_library();
    let mut ids = Vec::new();
    let mut ops = Vec::new();
    for sc in &scenes {
        let id = s.catalog.alloc_photo_id();
        let file = format!("LC{:05}.JPG", 1200 + sc.id * 7);
        let mut p = Photo::new(id, Source::Demo { scene: sc.id }, &file, "JPEG", sc.width, sc.height, "2026-09-28T09:30:00");
        p.captured = Some(sc.meta.captured.clone());
        p.file_size = (sc.width as u64 * sc.height as u64) / 3;
        p.meta = Meta {
            camera: sc.meta.camera.into(),
            lens: sc.meta.lens.into(),
            focal_mm: Some(sc.meta.focal_mm),
            aperture: Some(sc.meta.aperture),
            shutter: sc.meta.shutter.into(),
            iso: Some(sc.meta.iso),
            location: sc.meta.location.into(),
            title: sc.name.clone(),
            keywords: sc.meta.keywords.iter().map(|k| k.to_string()).collect(),
            creator: format!("{} Demo", dac_brand::DISPLAY_NAME),
            ..Default::default()
        };
        p.rating = [0, 3, 4, 5, 2, 0, 4, 3][sc.id as usize % 8];
        p.flag = match sc.id % 6 {
            1 => Flag::Pick,
            4 => Flag::Reject,
            _ => Flag::None,
        };
        // A few photos come pre-edited so the grid shows the "edited" badge and variety.
        let mut d = DevelopSettings::default();
        match sc.id {
            3 => {
                d.light.highlights = -60.0;
                d.light.shadows = 35.0;
                d.color.vibrance = 25.0;
                d.vignette.amount = -20.0;
            }
            4 => {
                d.light.exposure = 0.35;
                d.effects.clarity = 20.0;
                d.effects.dehaze = 15.0;
            }
            9 => {
                d.treatment = dac_develop::Treatment::Bw;
                d.light.contrast = 35.0;
            }
            _ => {}
        }
        p.develop = Arc::new(d);
        ids.push(id);
        ops.push(Op::AddPhoto { photo: Box::new(p) });
    }
    let folder = s.catalog.alloc_album_id();
    ops.push(Op::AddAlbum {
        album: Album {
            id: folder,
            name: "Travel 2026".into(),
            parent: None,
            folder: true,
            photos: vec![],
            cover: None,
            smart: None,
            quick: false,
            order: None,
        },
    });
    let by_kw = |kw: &str| -> Vec<_> { scenes.iter().zip(&ids).filter(|(sc, _)| sc.meta.keywords.contains(&kw)).map(|(_, id)| *id).collect() };
    for (name, parent, photos) in [
        ("Mountains", Some(folder), by_kw("mountains")),
        ("Coastlines", Some(folder), [by_kw("ocean"), by_kw("beach")].concat()),
        ("Deserts", Some(folder), by_kw("desert")),
        ("Night Sky", None, by_kw("night")),
        ("Garden", None, by_kw("flower")),
        ("Portfolio", None, ids.iter().copied().step_by(3).collect()),
    ] {
        let id = s.catalog.alloc_album_id();
        let cover = photos.first().copied();
        ops.push(Op::AddAlbum {
            album: Album { id, name: name.into(), parent, folder: false, photos, cover, smart: None, quick: false, order: None },
        });
    }
    for op in ops {
        let _ = s.catalog.apply(op);
    }
    if let Some(first) = s.visible_cloned().first() {
        s.selection = crate::Selection::single(*first);
    }
}

/// Procedural scenes are deterministic but costly to evaluate: every pixel runs the scene's noise
/// functions, so a 24 MP demo original takes minutes of CPU time on a small machine (a 4-core CI
/// VM). Each scene at each size is therefore generated once per process and shared:
///
/// - concurrent requests wait for one generation (Before and After at 1:1 both load the
///   original; sessions side by side, e.g. parallel tests, open the same photo);
/// - the most recently used results are kept, up to [`scene_cache_limit`] bytes. They are the same
///   allocations the sessions' source caches hold, so they cost extra memory only once no session
///   holds them any more.
///
/// Only demo photos use this; real files are decoded by their loader as before.
pub(crate) fn scene_pixels(scene: &dac_scenes::Scene, max_edge: usize) -> Arc<dac_raster::Rgb32f> {
    let key = SceneKey { kind: scene.kind, seed: scene.seed, width: scene.width, height: scene.height, max_edge };
    let slot = {
        let mut c = scene_cache();
        c.tick += 1;
        let tick = c.tick;
        match c.entries.iter_mut().find(|e| e.key == key) {
            Some(e) => {
                e.used = tick;
                e.slot.clone()
            }
            None => {
                let slot = Arc::new(std::sync::OnceLock::new());
                c.entries.push(SceneEntry { key, slot: slot.clone(), used: tick });
                slot
            }
        }
    };
    // (no lock held while generating: other scenes and sizes are served meanwhile)
    let pixels = slot.get_or_init(|| Arc::new(scene.render_fit(max_edge))).clone();
    trim_scene_cache(&mut scene_cache(), scene_cache_limit());
    pixels
}

/// Bytes of generated scenes kept for reuse: half the memory budget (768 MB by default; enough
/// for one 24 MP original and the previews around it).
fn scene_cache_limit() -> usize {
    crate::memory::budget() / 2
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct SceneKey {
    kind: dac_scenes::Kind,
    seed: u32,
    width: u32,
    height: u32,
    max_edge: usize,
}

type SceneSlot = Arc<std::sync::OnceLock<Arc<dac_raster::Rgb32f>>>;

struct SceneEntry {
    key: SceneKey,
    slot: SceneSlot,
    used: u64,
}

#[derive(Default)]
struct SceneCache {
    entries: Vec<SceneEntry>,
    tick: u64,
}

fn scene_cache() -> std::sync::MutexGuard<'static, SceneCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<SceneCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn scene_bytes(slot: &SceneSlot) -> usize {
    slot.get().map_or(0, |img| img.data.len().saturating_mul(std::mem::size_of::<[f32; 3]>()))
}

/// Drop the least recently used generated scenes until the rest fit in `limit` bytes. Scenes still
/// being generated are kept (their waiters hold the slot anyway).
fn trim_scene_cache(c: &mut SceneCache, limit: usize) {
    loop {
        let total: usize = c.entries.iter().map(|e| scene_bytes(&e.slot)).fold(0, usize::saturating_add);
        if total <= limit {
            return;
        }
        let Some(i) = c.entries.iter().enumerate().filter(|(_, e)| e.slot.get().is_some()).min_by_key(|(_, e)| e.used).map(|(i, _)| i) else {
            return;
        };
        c.entries.swap_remove(i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small_scene() -> dac_scenes::Scene {
        dac_scenes::demo_library().swap_remove(0)
    }

    #[test]
    fn a_scene_is_generated_once_per_size_and_shared() {
        let s = small_scene();
        let a = scene_pixels(&s, 48);
        let b = scene_pixels(&s, 48);
        assert!(Arc::ptr_eq(&a, &b), "the second request reuses the first generation");
        assert_eq!(a.width.max(a.height), 48);
        assert_eq!(a.data, s.render_fit(48).data, "the same pixels as generating it directly");
        let c = scene_pixels(&s, 40);
        assert_eq!(c.width.max(c.height), 40, "another size is its own generation");
    }

    #[test]
    fn concurrent_requests_wait_for_one_generation() {
        let s = small_scene();
        let edge = 37; // a size no other test asks for
        let got: Vec<_> = std::thread::scope(|sc| {
            let hs: Vec<_> = (0..4).map(|_| sc.spawn(|| scene_pixels(&s, edge))).collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(got.windows(2).all(|w| Arc::ptr_eq(&w[0], &w[1])), "every request got the same pixels");
    }

    #[test]
    fn the_cache_keeps_the_most_recent_scenes_within_its_limit() {
        let key = |max_edge| SceneKey { kind: dac_scenes::Kind::Dunes, seed: 1, width: 3, height: 2, max_edge };
        let entry = |max_edge, used, px: Option<usize>| {
            let slot: SceneSlot = Arc::new(std::sync::OnceLock::new());
            if let Some(n) = px {
                let _ = slot.set(Arc::new(dac_raster::Rgb32f::new(n, 1)));
            }
            SceneEntry { key: key(max_edge), slot, used }
        };
        let px = std::mem::size_of::<[f32; 3]>();
        let mut c = SceneCache { entries: vec![entry(1, 3, Some(10)), entry(2, 1, Some(10)), entry(3, 0, None), entry(4, 2, Some(10))], tick: 3 };
        trim_scene_cache(&mut c, 25 * px);
        let mut kept: Vec<usize> = c.entries.iter().map(|e| e.key.max_edge).collect();
        kept.sort_unstable();
        assert_eq!(kept, vec![1, 3, 4], "the least recently used finished scene went; the one in progress stays");
        trim_scene_cache(&mut c, 0);
        assert_eq!(c.entries.iter().map(|e| e.key.max_edge).collect::<Vec<_>>(), vec![3], "only the scene in progress is left");
    }
}
