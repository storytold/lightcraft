//! The public GPU render seam must decline the CPU-only grain process, never draw legacy grain.
use lightcraft_develop::DevelopSettings;
use lightcraft_gpu::{Fallback, try_render};
use lightcraft_pipeline::{RenderRequest, SourceInfo};
use lightcraft_raster::Rgb32f;
use std::sync::Arc;

#[test]
fn medium_particles_explicitly_declines_gpu_rendering() {
    let src = Arc::new(Rgb32f::from_fn(32, 24, |_, _| [0.18; 3]));
    for process in [None, Some("legacy"), Some("mediumParticlesV1")] {
        let mut input = serde_json::json!({"grain":{"amount":40}});
        if let Some(p) = process {
            input["grain"]["process"] = serde_json::json!(p);
        }
        let s = DevelopSettings::from_json(&input).unwrap();
        let result = try_render(&src, &SourceInfo::default(), &s, &RenderRequest::fit(32, 24), None);
        assert!(matches!(result, Err(Fallback::NotTried)), "process={process:?}");
    }
}

#[test]
fn disabled_effects_and_zero_amount_remain_gpu_eligible() {
    let src = Arc::new(Rgb32f::from_fn(32, 24, |_, _| [0.18; 3]));
    let req = RenderRequest::fit(32, 24);
    let info = SourceInfo::default();
    let base = DevelopSettings::default();
    if try_render(&src, &info, &base, &req, None).is_err() {
        lightcraft_testkit::skip!(Gpu, "GPU unavailable: eligibility render comparison skipped");
    }
    for disabled in [false, true] {
        let mut s = base.clone();

        s.grain.amount = if disabled { 40.0 } else { 0.0 };
        if disabled {
            s.disabled_sections.push("effects".into());
        }
        assert!(try_render(&src, &info, &s, &req, None).is_ok(), "disabled={disabled}");
    }
    println!("GPU available: disabled Effects and zero amount both rendered on GPU");
}
