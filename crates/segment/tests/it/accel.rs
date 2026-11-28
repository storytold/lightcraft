//! A backbone accelerator that fails leaves the work to candle, with the real checkpoint.
//!
//! Needs the model: `$LIGHTCRAFT_SAM3_DIR`, else the desktop app's folder (`~/.config/lightcraft/models/sam3` on
//! Linux). Skipped when it is missing. Runs the candle backbone once on the CPU (about half a minute in release).

use std::path::PathBuf;

use lightcraft_segment::{BackboneAccel, Device, Sam3, WeightReader};

fn model_dir() -> Option<PathBuf> {
    // only `$LIGHTCRAFT_SAM3_DIR`, as the GPU tests: never whatever is in the user's model folder
    let dir = std::env::var_os("LIGHTCRAFT_SAM3_DIR").filter(|v| !v.is_empty()).map(PathBuf::from)?;
    lightcraft_segment::is_model_dir(&dir).then_some(dir)
}

/// Fails on every photo, as a GPU that lost its device would.
struct Lost;

impl BackboneAccel for Lost {
    fn forward(&self, _: &[f32]) -> Result<Vec<f32>, String> {
        Err("device lost".into())
    }
    fn device(&self) -> String {
        "Lost GPU (Vulkan)".into()
    }
}

#[test]
fn an_accelerator_failing_on_a_photo_falls_back_to_candle() {
    let Some(dir) = model_dir() else {
        lightcraft_testkit::skip!(Model, "SAM 3 not installed (set LIGHTCRAFT_SAM3_DIR)");
        return;
    };
    let m = Sam3::load_accelerated(&dir, Device::Cpu, |_: &WeightReader| Ok(Box::new(Lost) as Box<dyn BackboneAccel>)).unwrap();
    assert!(m.accelerated());
    assert_eq!(m.backbone_device(), "Lost GPU (Vulkan)");
    let (w, h) = (64, 48);
    let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i % 251) as u8).collect();
    let enc = m.encode(&rgb, w, h).unwrap();
    assert_eq!(enc.backbone().dims(), [1, 1024, 72, 72]);
    let v = enc.backbone().flatten_all().unwrap().to_vec1::<f32>().unwrap();
    assert!(v.iter().all(|x| x.is_finite()) && v.iter().any(|x| *x != 0.0), "candle computed the features");
    assert!(!m.accelerated());
    assert_eq!(m.backbone_device(), "CPU");
    assert_eq!(m.backbone_fallback().as_deref(), Some("Lost GPU (Vulkan) failed: device lost"));
}

#[test]
fn an_accelerator_that_cannot_be_built_loads_candles_backbone() {
    let Some(dir) = model_dir() else {
        lightcraft_testkit::skip!(Model, "SAM 3 not installed (set LIGHTCRAFT_SAM3_DIR)");
        return;
    };
    let m = Sam3::load_accelerated(&dir, Device::Cpu, |_| Err("no GPU adapter".into())).unwrap();
    assert!(!m.accelerated());
    assert_eq!(m.backbone_device(), "CPU");
    assert_eq!(m.backbone_fallback().as_deref(), Some("no GPU adapter"));
}
