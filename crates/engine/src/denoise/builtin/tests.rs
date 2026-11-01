use super::*;
use crate::denoise::forget_gpu_set_ups;

/// A folder of the test's own: the card's set-up leaves its marker in it.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-engine-builtin-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The built-in denoiser for one photo of `noise` `(a, b)`, its card state kept by a marker in a folder of its own.
fn builtin(name: &str) -> (BuiltinModel, PathBuf) {
    let dir = folder(name);
    let m = BuiltinModel::new(&dir.join(GPU_SETUP_MARKER));
    let _ = m.runner.set(lightcraft_denoise::builtin::Builtin::with_noise(CHECK_NOISE));
    (m, dir)
}

/// Compare a tile from the card with the CPU's picture: equal on average, and with few values far off. (The demosaic
/// chooses a direction at each pixel, so a tiny difference of the filter can change a pixel by a good deal.)
fn agree(want: &[f32], got: &[f32]) {
    assert_eq!(want.len(), got.len());
    let scale = want.iter().fold(1e-9f32, |m, v| m.max(v.abs()));
    let mean = want.iter().zip(got).map(|(a, b)| (a - b).abs()).sum::<f32>() / want.len() as f32 / scale;
    let far = want.iter().zip(got).filter(|(a, b)| (*a - *b).abs() > 0.05 * scale).count() as f32 / want.len() as f32;
    assert!(mean < 1e-3 && far < 5e-3, "off by {mean:e} on average of the largest value, {far:e} of the values far off");
}

/// With the card chosen, the built-in denoiser's tile agrees with the processor's, a few tiles feed the card at once,
/// and a set-up that ended leaves no marker; where there is no card it stays on the processor.
#[test]
fn the_built_in_denoiser_on_the_card_gives_the_cpus_tile() {
    use lightcraft_denoise::builtin::{Builtin, TILE};
    let (m, dir) = builtin("builtin-card");
    let tile = builtin_check_tile(TILE as usize);
    let want = Builtin::with_noise(CHECK_NOISE).run(&tile).unwrap();
    let (runner, at_once) = m.runner(RunOn::Gpu, 16);
    let device = m.device(RunOn::Gpu);
    eprintln!("device: {device}");
    match device["kind"].as_str() {
        Some("gpu") => {
            assert!(at_once <= GPU_PARALLEL, "a card is fed by a few tiles at once");
            agree(&want, &runner.run(&tile).unwrap());
            assert!(device["cardMs"].as_f64().is_some_and(|ms| ms > 0.0) && device["cpuMs"].as_f64().is_some_and(|ms| ms > 0.0), "{device}");
        }
        Some("cpu") => {
            lightcraft_testkit::skip!(Gpu, "the built-in denoiser stays on the CPU here: {}", device["reason"]);
            assert_eq!(at_once, 16);
        }
        other => panic!("the first photo decides where the work runs, not {other:?}"),
    }
    assert!(!dir.join(GPU_SETUP_MARKER).exists(), "a set-up that ended leaves no marker");
    forget_builtin_card(&dir.join(GPU_SETUP_MARKER));
    let _ = std::fs::remove_dir_all(&dir);
}

/// With the processor chosen, the built-in denoiser gives the processor's tile bit for bit, uses all the threads
/// asked for and never sets the card up.
#[test]
fn with_the_processor_chosen_the_built_in_denoiser_runs_on_the_cpu_and_never_starts_the_card() {
    use lightcraft_denoise::builtin::{Builtin, TILE};
    let (m, dir) = builtin("builtin-cpu");
    let tile = builtin_check_tile(TILE as usize);
    let (runner, at_once) = m.runner(RunOn::Cpu, 16);
    assert_eq!(runner.run(&tile).unwrap(), Builtin::with_noise(CHECK_NOISE).run(&tile).unwrap(), "the CPU path is the CPU runner, bit for bit");
    assert_eq!(at_once, 16, "the threads asked for are the threads used");
    assert_eq!(m.device(RunOn::Cpu)["kind"], "cpu");
    assert_eq!(m.device(RunOn::Auto)["kind"], "pending", "the card was not even set up");
    forget_builtin_card(&dir.join(GPU_SETUP_MARKER));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A set-up marker left behind (the app closed while it ran) keeps the card off the built-in denoiser, with a reason
/// and a retry, until choosing where denoise runs forgets it.
#[test]
fn a_set_up_marker_left_behind_keeps_the_card_off_the_built_in_denoiser_until_a_choice_is_made() {
    let (m, dir) = builtin("builtin-marker");
    let marker = dir.join(GPU_SETUP_MARKER);
    std::fs::write(&marker, "setting up the graphics card for AI Denoise (unix time 1)\n").unwrap();
    let (_, at_once) = m.runner(RunOn::Gpu, 6);
    assert_eq!(at_once, 6, "no card: the threads asked for");
    let device = m.device(RunOn::Gpu);
    assert_eq!(device["kind"], "cpu", "{device}");
    assert_eq!(device["retry"], true, "{device}");
    assert!(device["reason"].as_str().unwrap().contains("closed"), "{device}");
    // choosing where denoise runs forgets it (and the marker): the card is set up afresh for the next photo
    forget_gpu_set_ups(&dir);
    assert!(!marker.exists());
    let (again, _) = builtin("builtin-marker-again");
    assert_eq!(again.device(RunOn::Auto)["kind"], "pending");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The card's filter stays within the agreement limits of the processor's on the check tile, well inside them, and
/// gives the same answer every time.
#[test]
fn the_cards_filter_agrees_with_the_cpus_on_the_check_tile_and_gives_the_same_answer_every_time() {
    use lightcraft_denoise::builtin::{Builtin, TILE};
    let tile = builtin_check_tile(TILE as usize);
    let b = Builtin::with_noise(CHECK_NOISE);
    let params = b.bm3d_params().unwrap();
    let gpu = match lightcraft_gpu::bm3d::Bm3dGpu::new(&params, TILE as usize) {
        Ok(g) => Arc::new(g),
        Err(why) => return lightcraft_testkit::skip!(Gpu, "no card for the built-in denoiser here: {why}"),
    };
    let runner = lightcraft_gpu::bm3d::BuiltinRunner::new(gpu, b).unwrap();
    let a = runner.agreement(&tile).unwrap();
    eprintln!("the card's filter is off by {:.3} at most and {:.1e} on average (noise deviations)", a.worst, a.mean);
    assert!(a.worst <= BUILTIN_AGREE_WORST && a.mean <= BUILTIN_AGREE_MEAN, "{a:?}");
    assert!(a.worst < 0.15 && a.mean < 1e-4, "a card within the limits but far from what it manages: {a:?}");
    let (first, again) = (runner.run(&tile).unwrap(), runner.run(&tile).unwrap());
    assert_eq!(first, again, "the card gives the same answer every time");
    agree(&Builtin::with_noise(CHECK_NOISE).run(&tile).unwrap(), &first);
}

/// A tile the card cannot do (another size than it was made for) is run on the processor, and after three of them
/// the card is stopped for the built-in denoiser.
#[test]
fn a_tile_the_card_gets_wrong_is_run_on_the_processor_and_three_stop_the_card_for_the_built_in_denoiser() {
    use lightcraft_denoise::builtin::Builtin;
    let (m, dir) = builtin("builtin-wrong");
    let (_, _) = m.runner(RunOn::Gpu, 8);
    if m.device(RunOn::Gpu)["kind"] != "gpu" {
        lightcraft_testkit::skip!(Gpu, "no card for the built-in denoiser here: {}", m.device(RunOn::Gpu)["reason"]);
        forget_builtin_card(&dir.join(GPU_SETUP_MARKER));
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    // a tile of another size than the card was made for is one it cannot do: the CPU runs it, and three stop the card
    let small = 64;
    let input = builtin_check_tile(small);
    let want = Builtin::with_noise(CHECK_NOISE).run(&input).unwrap();
    let (runner, _) = m.runner(RunOn::Gpu, 8);
    for _ in 0..GPU_MAX_FALLBACKS {
        assert_eq!(runner.run(&input).unwrap(), want, "a tile the card cannot do is the CPU's tile");
    }
    let device = m.device(RunOn::Gpu);
    assert_eq!(device["kind"], "cpu", "{device}");
    assert!(device["reason"].as_str().unwrap().contains("tiles wrong"), "{device}");
    assert_eq!(device["fellBack"], GPU_MAX_FALLBACKS);
    let (_, at_once) = m.runner(RunOn::Gpu, 8);
    assert_eq!(at_once, 8, "the next photo runs on the CPU, even with the card chosen");
    forget_builtin_card(&dir.join(GPU_SETUP_MARKER));
    let _ = std::fs::remove_dir_all(&dir);
}
