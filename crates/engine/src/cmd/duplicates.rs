//! `library.findDuplicates`: the duplicate finder's scan ([`crate::duplicates`]). A query — the
//! groups it returns are facts, and what the caller does with them (remove, reject, stack) goes
//! through the existing commands.

use serde_json::Value;

use super::{CommandSpec, always, bool_or, cmd};
use crate::{Result, Session};

fn find_duplicates(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.findDuplicates";
    if p.get("similar").is_some_and(|v| v.as_bool().is_none()) {
        return Err(super::bad(C, "similar must be true or false"));
    }
    let similar = bool_or(p, "similar", false);
    if similar && cfg!(target_arch = "wasm32") {
        return Err(super::bad(C, "the similar-copies scan reads the photos' files: not available in the web build"));
    }
    let candidates = crate::duplicates::candidates(&s.catalog);
    let loader = s.media.preview_loader.clone();
    let cache = s.media.rendered.clone();
    let report = crate::duplicates::scan(&candidates, loader.as_ref(), Some(cache.as_ref()), similar, &Default::default(), &|_, _| {});
    Ok(crate::duplicates::report_to_json(&s.catalog, &report))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        query "library.findDuplicates",
        "Find Duplicates",
        [],
        None,
        "{similar?: false}: the photos another photo of the library has the same file bytes as, as groups with the best copy to keep first (most pixels, then the largest file); similar: true also reads every photo's picture (a raw by its camera JPEG, a clip by a poster frame) and adds groups of copies that differ (resized, re-compressed, renamed), plus raw + JPEG pairs of the same shot (kind: exact | similar | sameShot; a scan of a large library takes as long as reading it) → {groups: [{kind, photos: [{id, name, path, kind, format, width, height, fileSize, imported, captured, identical}]}], scanned, unhashed, failed, failures: [{id, why}], cancelled}",
        always,
        find_duplicates
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A scratch folder that goes away with the test, however it ends.
    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> Scratch {
        let d = std::env::temp_dir().join(format!("lc-duplicates-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Scratch(d)
    }

    /// A JPEG of demo-scene pixels, `w × h`, written at `path`.
    fn write_scene_jpeg(path: &std::path::Path, scene: &lcc_scenes::Scene, w: usize, h: usize) {
        let img = scene.render(w, h);
        let mut rgba = lcc_raster::Rgba8::new(w, h);
        for (o, p) in rgba.data.iter_mut().zip(img.data.iter()) {
            let f = |c: f32| (c.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0).round() as u8;
            *o = [f(p[0]), f(p[1]), f(p[2]), 255];
        }
        let bytes = lcc_codecs::encode_jpeg(&lcc_codecs::EncodeImage::rgba8(&rgba), 90, Default::default(), &Default::default()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// A session with its own library folder, which the caller keeps alive (the session writes
    /// its journal there).
    fn session_with_library(name: &str) -> (Session, Scratch) {
        let lib = scratch(name);
        let mut s = Session::new().with_fs();
        s.open_library(&lib.0, false).unwrap();
        (s, lib)
    }

    #[test]
    fn a_resized_copy_groups_only_with_the_similar_scan() {
        let src = scratch("similar");
        let scene = &lcc_scenes::demo_library()[1];
        write_scene_jpeg(&src.0.join("big.jpg"), scene, 640, 480);
        write_scene_jpeg(&src.0.join("small.jpg"), scene, 320, 240);
        let (mut s, _lib) = session_with_library("similar-lib");
        s.execute("library.import", &json!({"paths": [src.0.to_string_lossy()], "mode": "add"})).unwrap();

        // without the similar scan, different bytes never group
        let r = s.execute("library.findDuplicates", &json!({})).unwrap();
        assert_eq!(r["groups"].as_array().map(Vec::len), Some(0), "{r}");

        // with it, the resized copy joins the original, which is the better copy to keep
        let r = s.execute("library.findDuplicates", &json!({"similar": true})).unwrap();
        assert_eq!(r["scanned"], 2, "{r}");
        assert_eq!(r["failed"], 0, "{r}");
        assert_eq!(r["groups"].as_array().map(Vec::len), Some(1), "{r}");
        let g = &r["groups"][0];
        assert_eq!(g["kind"], "similar", "{g}");
        assert_eq!(g["photos"][0]["name"], "big.jpg", "{g}");
        assert_eq!(g["photos"][0]["identical"], true);
        assert_eq!(g["photos"][1]["name"], "small.jpg", "{g}");
        assert_eq!(g["photos"][1]["identical"], false);
    }

    #[test]
    fn identical_files_group_as_exact_duplicates_without_deleted_photos_and_virtual_copies() {
        let src = scratch("out-of-scope");
        let scene = &lcc_scenes::demo_library()[2];
        write_scene_jpeg(&src.0.join("one.jpg"), scene, 512, 384);
        std::fs::copy(src.0.join("one.jpg"), src.0.join("one-again.jpg")).unwrap();
        let (mut s, _lib) = session_with_library("scope-lib");
        // an empty library has no duplicates, and a wrong-typed parameter is an error, not a guess
        let r = s.execute("library.findDuplicates", &json!({})).unwrap();
        assert_eq!(r["groups"].as_array().map(Vec::len), Some(0), "empty library");
        assert_eq!(r["unhashed"], 0);
        assert!(s.execute("library.findDuplicates", &json!({"similar": "yes"})).is_err());
        let r = s.execute("library.import", &json!({"paths": [src.0.to_string_lossy()], "mode": "add", "importDuplicates": true})).unwrap();
        assert_eq!(r["imported"].as_array().map(Vec::len), Some(2), "{r}");
        let ids: Vec<u64> = s.catalog.photos().map(|p| p.id.0).collect();

        // identical files group as exact duplicates
        let r = s.execute("library.findDuplicates", &json!({})).unwrap();
        assert_eq!(r["groups"].as_array().map(Vec::len), Some(1), "{r}");
        let g = &r["groups"][0];
        assert_eq!(g["kind"], "exact");
        assert_eq!(g["photos"].as_array().map(Vec::len), Some(2));
        // both members are the same bytes; the keeper's facts come from the catalog
        assert_eq!(g["photos"][0]["identical"], true);
        assert_eq!(g["photos"][1]["identical"], true);
        assert!(g["photos"][0]["fileSize"].as_u64().is_some_and(|n| n > 0));
        assert!(g["photos"][0]["width"].as_u64().is_some_and(|n| n > 0));

        // a virtual copy shares its master's file but is the same photo, not another copy
        s.execute("library.select", &json!({"ids": [ids[0]]})).unwrap();
        s.execute("photo.virtualCopy", &json!({})).unwrap();
        let r = s.execute("library.findDuplicates", &json!({})).unwrap();
        assert_eq!(r["groups"].as_array().map(Vec::len), Some(1), "{r}");
        assert_eq!(r["groups"][0]["photos"].as_array().map(Vec::len), Some(2), "{r}");

        // a real duplicate (Photo ▸ Duplicate copies the file) is another copy of the same bytes
        s.execute("library.select", &json!({"ids": [ids[0]]})).unwrap();
        s.execute("photo.duplicate", &json!({})).unwrap();
        let r = s.execute("library.findDuplicates", &json!({})).unwrap();
        assert_eq!(r["groups"][0]["photos"].as_array().map(Vec::len), Some(3), "{r}");

        // a copy in Recently Deleted is out of scope until it comes back
        s.execute("photo.delete", &json!({"ids": [ids[1]]})).unwrap();
        let r = s.execute("library.findDuplicates", &json!({})).unwrap();
        assert_eq!(r["groups"][0]["photos"].as_array().map(Vec::len), Some(2), "{r}");
        s.execute("photo.restore", &json!({"ids": [ids[1]]})).unwrap();
        let r = s.execute("library.findDuplicates", &json!({})).unwrap();
        assert_eq!(r["groups"][0]["photos"].as_array().map(Vec::len), Some(3), "{r}");
    }
}
