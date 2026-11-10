//! A text prompt seen before reuses its features, with the same detector output as the first time.
//!
//! Needs the model: `$LIGHTCRAFT_SAM3_DIR`, else the desktop app's folder (`~/.config/lightcraft/models/sam3` on
//! Linux). Skipped when it is missing. The backbone output is made up (no ViT run); the necks, the text encoder
//! and the detector are candle's on the CPU.

use std::path::PathBuf;

use candle_core::Tensor;
use lightcraft_segment::{BackboneAccel, DetectorOutput, Device, Sam3};

fn model_dir() -> Option<PathBuf> {
    // only `$LIGHTCRAFT_SAM3_DIR`, as the GPU tests: never whatever is in the user's model folder
    let dir = std::env::var_os("LIGHTCRAFT_SAM3_DIR").filter(|v| !v.is_empty()).map(PathBuf::from)?;
    lightcraft_segment::is_model_dir(&dir).then_some(dir)
}

/// A backbone output `[1024, 72, 72]` that varies over the channels and the grid.
struct Waves;

impl BackboneAccel for Waves {
    fn forward(&self, _: &[f32]) -> Result<Vec<f32>, String> {
        Ok((0..1024 * 72 * 72).map(|i| (i as f32 * 0.37).sin() * 0.5).collect())
    }
    fn device(&self) -> String {
        "Waves (test)".into()
    }
}

fn values(t: &Tensor) -> Vec<f32> {
    t.flatten_all().unwrap().to_vec1().unwrap()
}

fn assert_same(a: &DetectorOutput, b: &DetectorOutput) {
    assert_eq!(a.logits, b.logits);
    assert_eq!(a.presence, b.presence);
    assert_eq!(a.boxes, b.boxes);
    assert_eq!(values(&a.text), values(&b.text));
    assert_eq!(values(&a.memory), values(&b.memory));
    assert_eq!(values(&a.masks), values(&b.masks));
}

#[test]
fn a_repeated_prompt_gives_the_same_output() {
    let Some(dir) = model_dir() else {
        lightcraft_testkit::skip!(Model, "SAM 3 not installed (set LIGHTCRAFT_SAM3_DIR)");
        return;
    };
    let mut m = Sam3::load_accelerated(&dir, Device::Cpu, |_| Ok(Box::new(Waves) as Box<dyn BackboneAccel>)).unwrap();
    let (w, h) = (64, 48);
    let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i % 251) as u8).collect();
    let mut enc = m.encode(&rgb, w, h).unwrap();
    let fresh = m.detect(&mut enc, "person").unwrap();
    m.detect(&mut enc, "sky").unwrap();
    let cached = m.detect(&mut enc, "person").unwrap();
    assert_eq!(cached.text.id(), fresh.text.id(), "the prompt's features were reused");
    assert_same(&cached, &fresh);
    // and on the next photo
    let mut next = m.encode(&rgb, w, h).unwrap();
    let again = m.detect(&mut next, "person").unwrap();
    assert_eq!(again.text.id(), fresh.text.id());
    assert_same(&again, &fresh);
}
