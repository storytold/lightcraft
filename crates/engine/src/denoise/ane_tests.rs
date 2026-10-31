use super::ane::{ANE_CRASHED, ANE_PARALLEL, ANE_SLOW, ANE_STUCK, possible, worth_retrying as ane_retry};
use super::gpu::{ANE_SETUP_MARKER, GPU_CRASHED, GPU_SLOW, GPU_STUCK};
use super::*;
use lightcraft_denoise::manifest::{Domain, Gain};

/// A folder of the test's own: the Neural Engine's set-up leaves its marker and the kept Core ML model beside the model.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-engine-ane-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The manifest of the small model the tests use.
fn manifest(tile: u64) -> DenoiserManifest {
    DenoiserManifest {
        id: "synthetic".into(),
        name: "Synthetic".into(),
        version: "1".into(),
        licence: Default::default(),
        source: None,
        sha256: None,
        size_bytes: None,
        provenance: String::new(),
        domain: Domain::BayerToRgb,
        tile: tile as u32,
        overlap: 16,
        gain: Gain::MatchMean { nominal: 1.0, max_deviation: 0.05 },
    }
}

/// A synthetic model in a folder of its own, loaded as the app loads one.
fn model(name: &str, tile: u64) -> (Arc<dyn Model>, lightcraft_denoise::runtime::CpuRunner, PathBuf) {
    let path = folder(name).join("model.onnx");
    std::fs::write(&path, lightcraft_denoise::synthetic::unet_onnx(tile, 8, 2, 7)).unwrap();
    let loaded = default_loader()(&path, &manifest(tile)).unwrap();
    let cpu = lightcraft_denoise::runtime::CpuRunner::load(&path, &manifest(tile)).unwrap();
    (loaded, cpu, path)
}

/// The largest difference between two answers, as a share of the largest value of the first.
fn worst(want: &[f32], got: &[f32]) -> f32 {
    assert_eq!(want.len(), got.len());
    let scale = want.iter().fold(1e-9f32, |m, v| m.max(v.abs()));
    want.iter().zip(got).fold(0f32, |m, (a, b)| m.max((a - b).abs())) / scale
}

/// The folders of the Core ML models kept for a model file.
fn kept(model: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(model.with_file_name("coreml")).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default()
}

/// Test: the choice is named and parsed.
#[test]
fn the_choice_is_named_and_parsed() {
    assert_eq!(RunOn::parse("ane"), Some(RunOn::Ane));
    for r in [RunOn::Auto, RunOn::Gpu, RunOn::Ane, RunOn::Cpu] {
        assert_eq!(RunOn::parse(r.name()), Some(r));
    }
    assert_eq!(RunOn::parse("neural"), None);
    let st = Settings { run_on: Some("ane".into()), ..Default::default() };
    assert_eq!(st.run_on(), RunOn::Ane);
    // an unknown word is Automatic, as before
    assert_eq!(Settings { run_on: Some("tpu".into()), ..Default::default() }.run_on(), RunOn::Auto);
}

/// Test: the neural engine says why in the graphics cards words.
#[test]
fn the_neural_engine_says_why_in_the_graphics_cards_words() {
    // `set_up_card` words its reasons for a graphics card; the Neural Engine's are those, reworded
    assert_eq!(ANE_CRASHED, GPU_CRASHED.replace("the graphics card", "the Neural Engine"));
    assert_eq!(ANE_STUCK, GPU_STUCK.replace("the graphics card", "the Neural Engine"));
    assert_eq!(ANE_SLOW, GPU_SLOW.replace("the graphics card", "the Neural Engine"));
    for why in [ANE_CRASHED, ANE_STUCK] {
        assert!(ane_retry(why) && worth_retrying(why), "{why}");
    }
    assert!(worth_retrying(&format!("{ANE_SLOW} 60 s, so it is not used")));
    assert!(!ane_retry("its answer on the check tile is not the CPU's"));
}

/// Test: a tile on the neural engine is the cpus tile and the model is kept.
#[test]
fn a_tile_on_the_neural_engine_is_the_cpus_tile_and_the_model_is_kept() {
    if !possible() {
        lightcraft_testkit::skip!(System, "the Neural Engine is for Apple silicon");
        return;
    }
    let (model, cpu, path) = model("same", 64);
    let (input, _) = lightcraft_denoise::runtime::test_tile(64);
    let want = cpu.run(&input).unwrap();
    let (runner, at_once) = model.runner(RunOn::Ane, 16);
    let got = runner.run(&input).unwrap();
    let device = model.device(RunOn::Ane);
    eprintln!("device: {device}");
    assert_eq!(device["kind"], "ane", "{device}");
    assert!(at_once <= ANE_PARALLEL, "the Neural Engine is fed by a couple of tiles at once");
    assert!(device["aneMs"].as_f64().is_some_and(|ms| ms > 0.0) && device["cpuMs"].as_f64().is_some_and(|ms| ms > 0.0), "{device}");
    assert!(worst(&want, &got) < 2e-2, "the Neural Engine's tile is off by {}", worst(&want, &got));
    assert!(!path.with_file_name(ANE_SETUP_MARKER).exists(), "a set-up that ended leaves no marker");
    // the Core ML model is kept beside the model file, with the scale it was written with
    let folders = kept(&path);
    assert_eq!(folders.len(), 1, "{folders:?}");
    assert!(folders[0].join("net.mlmodelc").is_dir() && folders[0].join("scale").is_file());
    assert!(!folders[0].join("net.mlmodel").exists(), "the file Core ML compiled from is not kept");
    // another load of the same model file finds it: no second folder, and the same answer
    let again = default_loader()(&path, &manifest(64)).unwrap();
    let (runner2, _) = again.runner(RunOn::Ane, 4);
    assert_eq!(runner2.run(&input).unwrap(), got);
    assert_eq!(kept(&path).len(), 1);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// Test: a damaged kept model is made again.
#[test]
fn a_damaged_kept_model_is_made_again() {
    if !possible() {
        lightcraft_testkit::skip!(System, "the Neural Engine is for Apple silicon");
        return;
    }
    let (model, cpu, path) = model("damaged", 64);
    let (input, _) = lightcraft_denoise::runtime::test_tile(64);
    let want = cpu.run(&input).unwrap();
    let _ = model.runner(RunOn::Ane, 2);
    let folders = kept(&path);
    assert_eq!(folders.len(), 1);
    // a kept model that is not one, and a scale that is not a power of two
    std::fs::remove_dir_all(folders[0].join("net.mlmodelc")).unwrap();
    std::fs::create_dir(folders[0].join("net.mlmodelc")).unwrap();
    std::fs::write(folders[0].join("scale"), "0.3\n").unwrap();
    let again = default_loader()(&path, &manifest(64)).unwrap();
    let (runner, _) = again.runner(RunOn::Ane, 2);
    assert_eq!(again.device(RunOn::Ane)["kind"], "ane");
    assert!(worst(&want, &runner.run(&input).unwrap()) < 2e-2);
    let scale: f32 = std::fs::read_to_string(folders[0].join("scale")).unwrap().trim().parse().unwrap();
    assert!(scale > 0.0 && scale <= 1.0 && scale.log2().fract() == 0.0, "the scale is a power of two again: {scale}");
    assert!(std::fs::read_dir(folders[0].join("net.mlmodelc")).unwrap().count() > 0, "a model is there again");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// Test: a marker from a crash keeps the neural engine off until the user chooses.
#[test]
fn a_marker_from_a_crash_keeps_the_neural_engine_off_until_the_user_chooses() {
    if !possible() {
        lightcraft_testkit::skip!(System, "the Neural Engine is for Apple silicon");
        return;
    }
    let (model, _cpu, path) = model("crashed", 64);
    std::fs::write(path.with_file_name(ANE_SETUP_MARKER), "a set-up that never ended\n").unwrap();
    let (_runner, _) = model.runner(RunOn::Ane, 2);
    let device = model.device(RunOn::Ane);
    assert_ne!(device["kind"], "ane", "{device}");
    // the set-up has finished (crashed): its reason is in the status, and a retry is offered where the processor runs
    assert_eq!(device["neuralEngineReason"], ANE_CRASHED, "{device}");
    if device["kind"] == "cpu" {
        assert_eq!(device["retry"], true, "{device}");
    }
    // Automatic says it too: its first choice failed, and a retry is offered where the processor runs the photo
    let auto = model.device(RunOn::Auto);
    assert_eq!(auto["neuralEngineReason"], ANE_CRASHED, "{auto}");
    assert!(path.with_file_name(ANE_SETUP_MARKER).exists(), "the marker stays until the user chooses again");
    assert!(kept(&path).is_empty(), "nothing is made while the marker is there");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// Test: automatic uses the neural engine only where it measures faster than the processor.
#[test]
fn automatic_uses_the_neural_engine_only_where_it_measures_faster_than_the_processor() {
    if !possible() {
        lightcraft_testkit::skip!(System, "the Neural Engine is for Apple silicon");
        return;
    }
    let (model, _cpu, path) = model("auto", 64);
    let (_runner, at_once) = model.runner(RunOn::Auto, 8);
    let device = model.device(RunOn::Auto);
    eprintln!("device: {device}");
    match device["kind"].as_str() {
        Some("ane") => {
            assert!(at_once <= ANE_PARALLEL);
            let (ane, cpu) = (device["aneMs"].as_f64().unwrap(), device["cpuMs"].as_f64().unwrap());
            assert!(ane <= cpu, "Automatic only picks the Neural Engine when it measured faster: {device}");
        }
        Some("gpu" | "cpu") => {}
        other => panic!("the first photo decides where the work runs, not {other:?}"),
    }
    // the processor is never overridden
    assert_eq!(model.device(RunOn::Cpu)["kind"], "cpu");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// Test: two set ups of the same model at once do not share a half made folder.
#[test]
fn two_set_ups_of_the_same_model_at_once_do_not_share_a_half_made_folder() {
    if !possible() {
        lightcraft_testkit::skip!(System, "the Neural Engine is for Apple silicon");
        return;
    }
    let (_first, cpu, path) = model("twice", 64);
    let (input, _) = lightcraft_denoise::runtime::test_tile(64);
    let want = cpu.run(&input).unwrap();
    // two sessions of the app (or a retry beside a set-up that outlived its limit) make the Core ML model together
    let results: Vec<_> = std::thread::scope(|s| {
        let spawn = || {
            s.spawn(|| {
                let m = default_loader()(&path, &manifest(64)).unwrap();
                let (runner, _) = m.runner(RunOn::Ane, 2);
                (m.device(RunOn::Ane)["kind"].as_str().map(str::to_string), runner.run(&input).map(|out| worst(&want, &out)))
            })
        };
        let (a, b) = (spawn(), spawn());
        vec![a.join().unwrap(), b.join().unwrap()]
    });
    for (kind, run) in &results {
        let run = run.as_ref().unwrap();
        assert!(*run < 2e-2, "{run}");
        // either the Neural Engine, or (when the other set-up replaced the folder under this one) the processor
        assert!(matches!(kind.as_deref(), Some("ane" | "gpu" | "cpu")), "{kind:?}");
    }
    let leftovers: Vec<_> = kept(&path).into_iter().filter(|p| p.to_string_lossy().contains(".part-")).collect();
    assert!(leftovers.is_empty(), "no half-made folder is left: {leftovers:?}");
    assert!(!kept(&path).is_empty());
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
