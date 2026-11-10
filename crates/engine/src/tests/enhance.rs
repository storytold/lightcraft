//! Enhance ▸ Super Resolution and AI Sharpen with a stand-in upscaler.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lightcraft_denoise::manifest::{DenoiserManifest, Domain};
use lightcraft_denoise::run::TileRunner;
use serde_json::{Value, json};

use super::denoise::manifest;
use crate::Session;
use crate::denoise::{Model, RunOn};
use crate::export::ExportOptions;
use crate::tests_xmp::temp_dir;

/// A stand-in upscaler: every pixel of its tile becomes a 4 × 4 block.
struct Repeat4;
impl TileRunner for Repeat4 {
    fn run(&self, input: &[f32]) -> Result<Vec<f32>, lightcraft_denoise::Error> {
        let t = ((input.len() / 3) as f64).sqrt() as usize;
        let big = 4 * t;
        Ok((0..3 * big * big).map(|k| input[(k / (big * big)) * t * t + (k % (big * big)) / big / 4 * t + k % big / 4]).collect())
    }
}
impl Model for Repeat4 {
    fn runner(&self, _: RunOn, threads: usize) -> (&dyn TileRunner, usize) {
        (self, threads)
    }

    fn self_test(&self, _: RunOn) -> Result<Value, String> {
        Ok(json!({"ok": true}))
    }
}

/// The stand-in upscaler's `.onnx` (never read: the loader is the stand-in) with its manifest beside it.
fn upscaler_files(dir: &Path) -> PathBuf {
    let src = dir.join("src-upscaler");
    std::fs::create_dir_all(&src).unwrap();
    let m = DenoiserManifest { domain: Domain::RgbToRgb4x, ..manifest("stand-in-upscaler") };
    std::fs::write(src.join("denoise-model.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    std::fs::write(src.join("model.onnx"), "not an onnx file, only a stand-in").unwrap();
    src.join("model.onnx")
}

/// A session with a library holding one PNG of 150 × 100 (a ramp), selected, and the stand-in loader.
fn setup(tag: &str) -> (PathBuf, Session, lightcraft_catalog::PhotoId) {
    let dir = temp_dir(tag);
    let originals = dir.join("originals");
    std::fs::create_dir_all(&originals).unwrap();
    let img = lightcraft_raster::Rgba8::from_fn(150, 100, |x, y| [(40 + x) as u8, (60 + y) as u8, 120, 255]);
    let o = ExportOptions { format: crate::export::ExportFormat::Png, ..Default::default() };
    std::fs::write(originals.join("ramp.png"), crate::export::encode_image(&img, &o).unwrap()).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(dir.join("library"), false).unwrap();
    s.execute("library.import", &json!({"paths": [originals.join("ramp.png").to_string_lossy()]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.set_denoise_models_dir(Some(dir.join("denoise-models")));
    s.denoise.loader = Arc::new(|_, _| Ok(Arc::new(Repeat4) as Arc<dyn Model>));
    (dir, s, id)
}

fn decoded(path: &str) -> lightcraft_codecs::Decoded {
    lightcraft_codecs::decode(&std::fs::read(path).unwrap(), lightcraft_codecs::DecodeOptions::default()).unwrap()
}

#[test]
fn super_resolution_writes_a_larger_tiff_stacked_on_top_of_its_photo() {
    let (dir, mut s, original) = setup("enhance-sr");
    // without an upscaler it says where to get one
    let e = s.execute("enhance.superResolution", &json!({})).unwrap_err().to_string();
    assert!(e.contains("no upscaler is installed"), "{e}");

    // an upscaler fills its own slot: the denoise models of both kinds of photo stay as they were
    let r = s.execute("denoise.models.install", &json!({"path": upscaler_files(&dir).to_string_lossy(), "acknowledged": true})).unwrap();
    assert_eq!((r["upscaleModel"].clone(), r["model"].clone(), r["rgbModel"].clone()), (json!("stand-in-upscaler"), Value::Null, Value::Null), "{r}");

    let r = s.execute("enhance.superResolution", &json!({})).unwrap();
    assert_eq!((r["width"].clone(), r["height"].clone(), r["factor"].clone()), (json!(300), json!(200), json!(2)), "{r}");
    let path = r["path"].as_str().unwrap().to_string();
    assert!(path.ends_with("ramp-Enhanced-SR.tif"), "{path}");
    let id = lightcraft_catalog::PhotoId(r["id"].as_u64().unwrap());
    assert_eq!(s.catalog.stack_of(original).unwrap().photos, vec![id, original], "stacked with its photo, on top");
    assert_eq!(s.selected(), vec![id]);

    // a 16-bit TIFF of the picture, every pixel twice as large (the stand-in repeats, averaged down by 2)
    let d = decoded(&path);
    assert_eq!((d.image.width, d.image.height, d.bit_depth), (300, 200, 16));
    let srgb = crate::denoise::srgb_encoded(&d);
    for (x, y) in [(0, 0), (149, 99), (75, 50), (10, 90)] {
        let want = [(40 + x) as f32, (60 + y) as f32, 120.0].map(|v| v / 255.0);
        for (dx, dy) in [(0, 0), (1, 1)] {
            let got = srgb.data[(2 * y + dy) * 300 + 2 * x + dx];
            for c in 0..3 {
                assert!((got[c] - want[c]).abs() < 1.5 / 255.0, "({x}, {y}): {got:?} vs {want:?}");
            }
        }
    }

    // again: a new file beside it, never the first one replaced; 4× on request
    let r = s.execute("enhance.superResolution", &json!({"id": original.0, "scale": 4})).unwrap();
    assert!(r["path"].as_str().unwrap().ends_with("ramp-Enhanced-SR-2.tif"), "{r}");
    let id4 = r["id"].as_u64().unwrap();
    assert_eq!((r["width"].clone(), r["height"].clone()), (json!(600), json!(400)));

    // the same request gives the same bytes: the photo that has them is the result, and no copy is left behind
    let photos = s.catalog.photos().count();
    let again = s.execute("enhance.superResolution", &json!({"id": original.0})).unwrap();
    assert_eq!((again["id"].clone(), again["path"].as_str()), (json!(id.0), Some(path.as_str())), "{again}");
    assert_eq!(s.catalog.photos().count(), photos);
    assert!(!Path::new(&path.replace("ramp-Enhanced-SR.tif", "ramp-Enhanced-SR-3.tif")).exists());
    assert_eq!(s.selected(), vec![id]);

    // a reused result that went into a stack of its own stays there: that stack's photos don't join the original's
    s.execute("stack.ungroup", &json!({"ids": [original.0]})).unwrap();
    s.execute("stack.group", &json!({"ids": [id4, id.0], "top": id4})).unwrap();
    let again = s.execute("enhance.superResolution", &json!({"id": original.0})).unwrap();
    assert_eq!(again["id"], json!(id.0));
    assert!(s.catalog.stack_of(original).is_none());
    assert_eq!(s.catalog.stack_of(id).unwrap().photos.len(), 2);
    drop(s);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn ai_sharpen_keeps_the_photos_size_and_bad_requests_are_errors() {
    let (dir, mut s, original) = setup("enhance-sharpen");
    s.execute("denoise.models.install", &json!({"path": upscaler_files(&dir).to_string_lossy(), "acknowledged": true})).unwrap();
    let r = s.execute("enhance.aiSharpen", &json!({})).unwrap();
    assert_eq!((r["width"].clone(), r["height"].clone(), r["factor"].clone()), (json!(150), json!(100), json!(1)), "{r}");
    assert_eq!((decoded(r["path"].as_str().unwrap()).image.width, s.catalog.photos().count()), (150, 2));

    // a result in Recently Deleted comes back as the result of the same request, and no copy is left beside it
    let (result, path) = (r["id"].as_u64().unwrap(), r["path"].as_str().unwrap().to_string());
    s.execute("photo.delete", &json!({"ids": [result]})).unwrap();
    let again = s.execute("enhance.aiSharpen", &json!({"id": original.0})).unwrap();
    assert_eq!((again["id"].clone(), again["path"].as_str()), (json!(result), Some(path.as_str())), "{again}");
    assert!(!s.catalog.photo(lightcraft_catalog::PhotoId(result)).unwrap().deleted);
    assert!(!Path::new(&path.replace("ramp-Enhanced-SR.tif", "ramp-Enhanced-SR-2.tif")).exists());

    // a photo named by `id` needs no selection; one in Recently Deleted is not enhanced
    s.execute("library.select", &json!({"ids": []})).unwrap();
    assert_eq!(s.execute("enhance.aiSharpen", &json!({"id": original.0})).unwrap()["id"], json!(result));
    s.execute("photo.delete", &json!({"ids": [original.0]})).unwrap();
    let e = s.execute("enhance.aiSharpen", &json!({"id": original.0})).unwrap_err().to_string();
    assert!(e.contains("Recently Deleted"), "{e}");
    s.execute("photo.restore", &json!({"ids": [original.0]})).unwrap();
    s.execute("library.select", &json!({"ids": [original.0]})).unwrap();
    let e = s.execute("enhance.aiSharpen", &json!({"scale": 2})).unwrap_err().to_string();
    assert!(e.contains("`scale`"), "{e}");

    for scale in [json!(3), json!(1), json!("2")] {
        let e = s.execute("enhance.superResolution", &json!({"id": original.0, "scale": scale})).unwrap_err().to_string();
        assert!(e.contains("`scale` must be 2 or 4"), "{e}");
    }
    let both: Vec<u64> = s.catalog.photos().map(|p| p.id.0).collect();
    let e = s.execute("enhance.aiSharpen", &json!({"ids": both})).unwrap_err().to_string();
    assert!(e.contains("one photo at a time"), "{e}");
    // switched off, Super Resolution asks for an upscaler again
    s.execute("denoise.models.select", &json!({"id": null, "kind": "upscale"})).unwrap();
    assert!(s.execute("enhance.aiSharpen", &json!({"id": original.0})).unwrap_err().to_string().contains("no upscaler"));
    drop(s);
    let _ = std::fs::remove_dir_all(dir);
}
