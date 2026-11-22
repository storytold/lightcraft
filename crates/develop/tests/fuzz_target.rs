//! Fuzz target `develop` (`cargo xtask fuzz develop`): develop settings from outside the app (sidecar
//! `lc:settings`, presets, catalog rows) as JSON, structurally and byte-mutated, through
//! `from_untrusted_json`, `merged` and the reads the renderer makes. A short run in `cargo test`;
//! time-boxed long runs under `LCC_FUZZ_SECS` (see `lcc-fuzzkit`).

use lcc_develop::DevelopSettings;

#[test]
fn fuzz_target() {
    let base = DevelopSettings::default();
    let raw = DevelopSettings::for_raw(5200.0, 8.0);
    let seeds = [base.to_json().to_string(), raw.to_json().to_string(), r#"{"light":{"exposure":1.5},"detail":{"sharpen_amount":40}}"#.to_string()];
    let refs: Vec<&str> = seeds.iter().map(String::as_str).collect();
    lcc_fuzzkit::run_json("develop", &refs, 400, |text| {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return };
        if let Ok(s) = DevelopSettings::from_untrusted_json(&v) {
            let _ = s.is_unedited();
            let _ = s.effective();
            let _ = s.denoise_amount();
            let _ = s.next_mask_id();
            let _ = DevelopSettings::from_untrusted_json(&s.to_json());
        }
        let _ = base.merged(&v);
    });
}
