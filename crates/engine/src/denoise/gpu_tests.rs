use super::*;
use lightcraft_denoise::manifest::{Domain, Gain, LinearSpace};

/// A folder of the test's own: the card's set-up leaves its marker beside the model.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-engine-gpu-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // exclusive: a directory planted at this predictable name fails the test instead of receiving its files
    std::fs::create_dir(&dir).unwrap();
    dir
}

/// The model file is read again when the card is set up (for the first photo), so it stays until the test ends.
fn model(name: &str, tile: u64) -> (Arc<dyn Model>, lightcraft_denoise::runtime::CpuRunner, PathBuf) {
    let path = folder(name).join("model.onnx");
    std::fs::write(&path, lightcraft_denoise::synthetic::unet_onnx(tile, 8, 2, 7)).unwrap();
    let manifest = DenoiserManifest {
        id: "synthetic".into(),
        name: "Synthetic".into(),
        version: "1".into(),
        licence: Default::default(),
        source: None,
        sha256: None,
        size_bytes: None,
        provenance: String::new(),
        domain: Domain::BayerToRgb,
        space: LinearSpace::CameraRgb,
        tile: tile as u32,
        overlap: 16,
        gain: Gain::MatchMean { nominal: 1.0, max_deviation: 0.05 },
    };
    let loaded = default_loader()(&path, &manifest).unwrap();
    let cpu = lightcraft_denoise::runtime::CpuRunner::load(&path, &manifest).unwrap();
    (loaded, cpu, path)
}

fn worst(want: &[f32], got: &[f32]) -> f32 {
    assert_eq!(want.len(), got.len());
    let scale = want.iter().fold(1e-9f32, |m, v| m.max(v.abs()));
    want.iter().zip(got).fold(0f32, |m, (a, b)| m.max((a - b).abs())) / scale
}

#[test]
fn a_tile_on_the_card_is_the_cpus_tile() {
    let (model, cpu, path) = model("same", 64);
    let (input, _) = lightcraft_denoise::runtime::test_tile(64);
    let want = cpu.run(&input).unwrap();
    let (runner, at_once) = model.runner(RunOn::Gpu, 16);
    assert!(worst(&want, &runner.run(&input).unwrap()) < 1e-3);
    let device = model.device(RunOn::Gpu);
    eprintln!("device: {device}");
    match device["kind"].as_str() {
        Some("gpu") => {
            assert!(device["adapter"].as_str().is_some_and(|a| !a.is_empty()));
            assert!(at_once <= GPU_PARALLEL, "a card is fed by a few tiles at once");
            assert!(device["cardMs"].as_f64().is_some_and(|ms| ms > 0.0) && device["cpuMs"].as_f64().is_some_and(|ms| ms > 0.0), "{device}");
        }
        Some("cpu") => {
            assert!(device["reason"].as_str().is_some_and(|r| !r.is_empty()), "the CPU is only used for a reason: {device}");
            assert_eq!(at_once, 16);
        }
        other => panic!("the first photo decides where the work runs, not {other:?}"),
    }
    assert!(!path.with_file_name(GPU_SETUP_MARKER).exists(), "a set-up that ended leaves no marker");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// A server on this computer running `cpu` for the model with `sha256`.
fn server(cpu: lightcraft_denoise::runtime::CpuRunner, sha256: &str, tile: usize) -> String {
    use lightcraft_denoise::remote::{ModelId, ServerOptions, serve};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let id = ModelId { id: "synthetic".into(), version: "1".into(), sha256: sha256.into() };
    let run: lightcraft_denoise::remote::RunTile = Arc::new(move |input: &[f32]| cpu.run(input));
    let opts = ServerOptions { token: "remote-test-token-0123".into(), ..Default::default() };
    std::thread::spawn(move || serve(l, id, tile, "CPU (test server)".into(), run, opts, Arc::new(|_| {})));
    addr
}

fn with_remote(path: &Path, tile: u64, addr: &str, sha256: &str) -> Network {
    let manifest = DenoiserManifest {
        id: "synthetic".into(),
        name: "Synthetic".into(),
        version: "1".into(),
        licence: Default::default(),
        source: None,
        sha256: None,
        size_bytes: None,
        provenance: String::new(),
        domain: Domain::BayerToRgb,
        space: LinearSpace::CameraRgb,
        tile: tile as u32,
        overlap: 16,
        gain: Gain::MatchMean { nominal: 1.0, max_deviation: 0.05 },
    };
    let remote = RemoteSide::new(lightcraft_denoise::remote::Remote::new(addr, "remote-test-token-0123", sha256, tile as usize));
    network(path, &manifest, Some(remote)).unwrap()
}

#[test]
fn a_remote_server_shares_the_tiles_and_drops_out_when_it_cannot() {
    let (_, cpu, path) = model("remote", 64);
    let sha = lightcraft_denoise::hash::sha256_file(&path).unwrap();
    let (input, _) = lightcraft_denoise::runtime::test_tile(64);
    let want = cpu.run(&input).unwrap();
    // the same model on the server: its slots come on top of this computer's threads, answers within f16 rounding
    let addr = server(lightcraft_denoise::runtime::CpuRunner::load(&path, &cpu.manifest().clone()).unwrap(), &sha, 64);
    let net = with_remote(&path, 64, &addr, &sha);
    let (runner, at_once) = net.runner(RunOn::Cpu, 2);
    assert_eq!(at_once, 2 + 4, "four server slots on top of two local threads");
    assert!(worst(&want, &runner.run(&input).unwrap()) < 5e-3);
    let d = net.device(RunOn::Cpu);
    assert_eq!(d["remote"]["state"], "used", "{d}");
    assert_eq!(d["remote"]["device"], "CPU (test server)");
    // a local card faster than the server there and back (here a stand-in that takes no time): nothing is shared,
    // the photo never waits for the slower server, and the status says why
    let mut net = with_remote(&path, 64, &addr, &sha);
    net.gpu = OnceLock::from(Ok(GpuSide { runner: Box::new(cpu.clone()), adapter: "Fast card".into(), card_ms: 1e-6, cpu_ms: 1000.0 }));
    let (_, at_once) = net.runner(RunOn::Gpu, 8);
    assert_eq!(at_once, GPU_PARALLEL, "no server slots");
    let d = net.device(RunOn::Gpu);
    assert_eq!(d["remote"]["state"], "slower", "{d}");
    assert!(d["remote"]["remoteMs"].as_f64().is_some_and(|ms| ms > 0.0) && d["remote"]["localMs"].as_f64().is_some(), "{d}");
    // a server running another model is refused: everything runs here
    let other = server(lightcraft_denoise::runtime::CpuRunner::load(&path, &cpu.manifest().clone()).unwrap(), "0000", 64);
    let net = with_remote(&path, 64, &other, &sha);
    let (runner, at_once) = net.runner(RunOn::Cpu, 2);
    assert_eq!(at_once, 2);
    assert_eq!(runner.run(&input).unwrap(), want, "the CPU path, bit for bit");
    let d = net.device(RunOn::Cpu);
    assert!(d["remote"]["reason"].as_str().is_some_and(|r| r.contains("another model")), "{d}");
    // nobody listening: the same
    let net = with_remote(&path, 64, "127.0.0.1:1", &sha);
    assert_eq!(net.runner(RunOn::Cpu, 3).1, 3);
    assert_eq!(net.device(RunOn::Cpu)["remote"]["state"], "unavailable");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn with_the_processor_chosen_everything_runs_on_the_cpu() {
    let (model, cpu, path) = model("cpu", 64);
    let (input, _) = lightcraft_denoise::runtime::test_tile(64);
    let (runner, at_once) = model.runner(RunOn::Cpu, 16);
    assert_eq!(runner.run(&input).unwrap(), cpu.run(&input).unwrap(), "the CPU path is the CPU runner, bit for bit");
    assert_eq!(at_once, 16, "the threads asked for are the threads used");
    assert_eq!(model.device(RunOn::Cpu)["kind"], "cpu");
    assert_eq!(model.device(RunOn::Auto)["kind"], "pending", "the card was not even set up");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn the_card_is_used_where_it_is_faster_than_the_processor() {
    // measured: a tile alone on one CPU thread 1.4 s; an RTX 4090 Laptop 13 ms; a Radeon 610M 490 ms
    for threads in [1, 2, 8, 16, 64] {
        assert!(card_is_faster(13.0, 1400.0, threads), "a big card wins at {threads}");
    }
    assert!(card_is_faster(490.0, 1400.0, 2), "a small card beats the two threads background work gets");
    assert!(!card_is_faster(490.0, 1400.0, 16), "and loses to sixteen");
    assert!(!card_is_faster(f64::INFINITY, 1400.0, 1) && !card_is_faster(f64::NAN, 1400.0, 1), "a card that never ran is never faster");
    assert!(!card_is_faster(10.0, 0.0, 1));
}

/// A card that gives numbers that are not finite.
struct Garbage(AtomicUsize);

impl TileRunner for Garbage {
    fn run(&self, input: &[f32]) -> Result<Vec<f32>, RunError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        let n = input.len() / 4 * 12;
        Ok(vec![f32::NAN; n])
    }
}

#[test]
fn a_tile_the_card_gets_wrong_is_run_on_the_processor_and_three_stop_the_card() {
    let (_, cpu, path) = model("garbage", 64);
    let card = GpuSide { runner: Box::new(Garbage(AtomicUsize::new(0))), adapter: "Test card".into(), card_ms: 1.0, cpu_ms: 1000.0 };
    let model = Network {
        cpu: cpu.clone(),
        path: path.clone(),
        tile: 32,
        gpu: OnceLock::from(Ok(card)),
        fallbacks: AtomicUsize::new(0),
        last_threads: AtomicUsize::new(0),
        remote: None,
        local_card: AtomicBool::new(true),
    };
    let (input, _) = lightcraft_denoise::runtime::test_tile(64);
    let want = cpu.run(&input).unwrap();
    let (runner, at_once) = model.runner(RunOn::Auto, 8);
    assert_eq!(at_once, GPU_PARALLEL, "the (fast) card is chosen");
    for _ in 0..GPU_MAX_FALLBACKS {
        assert_eq!(runner.run(&input).unwrap(), want, "a wrong tile is the CPU's tile");
    }
    let device = model.device(RunOn::Auto);
    assert_eq!(device["kind"], "cpu", "{device}");
    assert!(device["reason"].as_str().unwrap().contains("tiles wrong"), "{device}");
    assert_eq!(device["fellBack"], GPU_MAX_FALLBACKS);
    let (_, at_once) = model.runner(RunOn::Gpu, 8);
    assert_eq!(at_once, 8, "the next photo runs on the CPU, even with the card chosen");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_slow_card_is_left_out_unless_it_is_chosen() {
    let (_, cpu, path) = model("slow", 64);
    let slow = GpuSide { runner: Box::new(cpu.clone()), adapter: "Slow card".into(), card_ms: 490.0, cpu_ms: 1400.0 };
    let model = Network {
        cpu,
        path: path.clone(),
        tile: 32,
        gpu: OnceLock::from(Ok(slow)),
        fallbacks: AtomicUsize::new(0),
        last_threads: AtomicUsize::new(0),
        remote: None,
        local_card: AtomicBool::new(true),
    };
    assert_eq!(model.runner(RunOn::Auto, 16).1, 16, "sixteen threads beat it");
    let device = model.device(RunOn::Auto);
    assert_eq!((device["kind"].as_str(), device["reason"].as_str()), (Some("cpu"), Some("it is faster than Slow card on this computer")));
    assert_eq!(device["cardMs"], 490.0, "the times measured are reported");
    assert_eq!(device["retry"], false, "trying again would not make it faster");
    assert_eq!(model.runner(RunOn::Auto, 2).1, GPU_PARALLEL.min(2), "two threads do not");
    assert_eq!(model.device(RunOn::Auto)["kind"], "gpu");
    assert_eq!(model.runner(RunOn::Gpu, 16).1, GPU_PARALLEL, "chosen, it is used anyway");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_set_up_marker_left_behind_keeps_the_card_off() {
    let (model, _, path) = model("crashed", 64);
    let marker = path.with_file_name(GPU_SETUP_MARKER);
    std::fs::write(&marker, "setting up the graphics card for AI Denoise (unix time 0)").unwrap();
    let (_, at_once) = model.runner(RunOn::Gpu, 4);
    assert_eq!(at_once, 4, "the CPU does the work");
    let device = model.device(RunOn::Gpu);
    assert_eq!((device["kind"].as_str(), device["reason"].as_str()), (Some("cpu"), Some(GPU_CRASHED)), "{device}");
    assert_eq!(device["retry"], true, "the interface offers to try again");
    assert!(marker.exists(), "it stays until the user chooses where denoise runs");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_set_up_runs_beside_the_cpu_and_removes_its_marker() {
    let dir = folder("setup");
    let marker = dir.join(GPU_SETUP_MARKER);
    let seen = Arc::new(AtomicBool::new(false));
    let (m, s) = (marker.clone(), seen.clone());
    let r = set_up_card(
        &marker,
        Duration::from_secs(10),
        move || {
            s.store(m.exists(), Ordering::Relaxed);
            Ok(7)
        },
        || "cpu",
    );
    assert_eq!(r, Ok((7, "cpu")));
    assert!(seen.load(Ordering::Relaxed), "the marker is there while the card is set up");
    assert!(!marker.exists(), "and gone after");
    // a set-up that fails or panics removes it too
    assert_eq!(set_up_card(&marker, Duration::from_secs(10), || Err::<(), _>("no card".into()), || ()), Err("no card".into()));
    assert!(!marker.exists());
    let r = set_up_card(&marker, Duration::from_secs(10), || -> Result<(), String> { panic!("driver") }, || ());
    assert!(r.is_err() && !marker.exists(), "{r:?}");
    // a marker found before the set-up starts means the last one took LightCraft down: the card is not touched
    std::fs::write(&marker, "left behind").unwrap();
    let touched = Arc::new(AtomicBool::new(false));
    let t = touched.clone();
    let r = set_up_card(
        &marker,
        Duration::from_secs(10),
        move || {
            t.store(true, Ordering::Relaxed);
            Ok(())
        },
        || (),
    );
    assert_eq!(r, Err(GPU_CRASHED.to_string()));
    assert!(!touched.load(Ordering::Relaxed) && marker.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_set_up_makes_the_folder_of_its_marker() {
    let dir = folder("setup-folder").join("not").join("yet");
    let marker = dir.join(GPU_SETUP_MARKER);
    let (m, seen) = (marker.clone(), Arc::new(AtomicBool::new(false)));
    let s = seen.clone();
    let r = set_up_card(
        &marker,
        Duration::from_secs(10),
        move || {
            s.store(m.exists(), Ordering::Relaxed);
            Ok(())
        },
        || (),
    );
    assert_eq!(r, Ok(((), ())));
    assert!(seen.load(Ordering::Relaxed), "the crash guard is written even when the models folder was missing");
    let _ = std::fs::remove_dir_all(dir.ancestors().nth(2).unwrap_or(&dir));
}

#[test]
fn a_set_up_that_takes_too_long_is_given_up_on() {
    let dir = folder("slowsetup");
    let marker = dir.join(GPU_SETUP_MARKER);
    let started = web_time::Instant::now();
    let slow = || -> Result<(), String> {
        std::thread::sleep(Duration::from_millis(600));
        Ok(())
    };
    let r = set_up_card(&marker, Duration::from_millis(50), slow, || ());
    assert!(r.as_ref().is_err_and(|e| e.contains("took longer") && worth_retrying(e)), "{r:?}");
    assert!(started.elapsed() < Duration::from_millis(500), "the caller does not wait for the card");
    // while the first is still stuck, another set-up of the same model does not start a second one
    let again = set_up_card(&marker, Duration::from_secs(5), || Ok(()), || ());
    assert!(again.is_err_and(|e| e.contains("still setting up") && worth_retrying(&e)));
    // once it ends its marker goes, and the card can be set up again
    while marker.exists() {
        assert!(started.elapsed() < Duration::from_secs(10), "the stuck set-up ended");
        std::thread::sleep(Duration::from_millis(20));
    }
    while SETTING_UP.lock().unwrap().iter().any(|m| m == &marker) {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(set_up_card(&marker, Duration::from_secs(5), || Ok(1), || 2), Ok((1, 2)));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The lock a set-up holds for its whole run is what says another process is setting the card up: a set-up that finds it
/// held waits for it (up to its deadline) instead of taking the marker for a crash's.
#[test]
fn a_set_up_that_another_process_is_running_is_waited_for_not_taken_for_a_crash() {
    let dir = folder("otherprocess");
    let marker = dir.join(GPU_SETUP_MARKER);
    // another LightCraft process: it took the lock and made its marker
    let other = std::fs::OpenOptions::new().read(true).write(true).truncate(false).create(true).open(dir.join(super::gpu::GPU_SETUP_LOCK)).unwrap();
    other.lock().unwrap();
    std::fs::write(&marker, "setting up the graphics card for AI Denoise (unix time 0)").unwrap();
    // it ends after a moment: the marker goes, then the lock
    let m = marker.clone();
    let ender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        let _ = std::fs::remove_file(&m);
        drop(other);
    });
    let started = web_time::Instant::now();
    let r = set_up_card(&marker, Duration::from_secs(10), || Ok(3), || 4);
    ender.join().unwrap();
    assert_eq!(r, Ok((3, 4)), "the card is set up once the other set-up has ended");
    assert!(started.elapsed() < Duration::from_secs(10), "the wait took less than its limit");
    // a lock that is not released in time: the card is not used, and the reason says why
    let other = std::fs::OpenOptions::new().read(true).write(true).truncate(false).create(true).open(dir.join(super::gpu::GPU_SETUP_LOCK)).unwrap();
    other.lock().unwrap();
    std::fs::write(&marker, "setting up the graphics card for AI Denoise (unix time 0)").unwrap();
    let r = set_up_card(&marker, Duration::from_millis(250), || Ok(()), || ());
    assert_eq!(r, Err(super::gpu::GPU_STUCK.to_string()));
    drop(other);
    let _ = std::fs::remove_file(&marker);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A caller that has spent its coordination deadline does not start a set-up after it, however long the other process's
/// set-up goes on: the deadline is one for the whole coordination.
#[test]
fn a_set_up_does_not_start_after_its_coordination_deadline() {
    let dir = folder("deadline");
    let marker = dir.join(GPU_SETUP_MARKER);
    let other = std::fs::OpenOptions::new().read(true).write(true).truncate(false).create(true).open(dir.join(super::gpu::GPU_SETUP_LOCK)).unwrap();
    other.lock().unwrap();
    std::fs::write(&marker, "setting up the graphics card for AI Denoise (unix time 0)").unwrap();
    let holder = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(600));
        drop(other);
    });
    let started = web_time::Instant::now();
    let r = set_up_card(&marker, Duration::from_millis(250), || Ok(1), || 2);
    let took = started.elapsed();
    holder.join().unwrap();
    assert_eq!(r, Err(super::gpu::GPU_STUCK.to_string()), "no new set-up after the deadline: {r:?}");
    assert!(took < Duration::from_millis(600), "it gives up at its deadline, not when the other set-up ends: {took:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Choosing where denoise runs clears a marker a crash left behind, but not one a set-up that is under way owns: that
/// set-up's own end removes its marker, and clearing it would let a second set-up start beside it.
#[test]
fn forgetting_set_ups_leaves_a_marker_another_process_holds() {
    let dir = folder("forget");
    let marker = dir.join(GPU_SETUP_MARKER);
    std::fs::write(&marker, "setting up the graphics card for AI Denoise (unix time 0)").unwrap();
    super::gpu::forget_marker(&marker).unwrap();
    assert!(!marker.exists(), "a marker nobody holds is one a crash left behind");
    // another process takes the lock and makes its own marker
    let other = std::fs::OpenOptions::new().read(true).write(true).truncate(false).create(true).open(dir.join(super::gpu::GPU_SETUP_LOCK)).unwrap();
    other.lock().unwrap();
    std::fs::write(&marker, "setting up the graphics card for AI Denoise (unix time 0)").unwrap();
    super::gpu::forget_marker(&marker).unwrap();
    assert!(marker.exists(), "a set-up that is under way keeps its marker");
    drop(other);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A marker that cannot be removed (a models folder that takes no change) is reported, so the caller does not say the
/// card may be tried again while its marker stays.
#[cfg(unix)]
#[test]
fn forgetting_a_marker_that_cannot_be_removed_says_so() {
    use std::os::unix::fs::PermissionsExt;
    let dir = folder("forget-unremovable");
    let marker = dir.join(GPU_SETUP_MARKER);
    std::fs::write(&marker, "setting up the graphics card for AI Denoise (unix time 0)").unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    // a process that writes anywhere (root) cannot make the failure this test is about
    if std::fs::write(dir.join("made-anyway"), b"").is_ok() {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        return lightcraft_testkit::skip!(System, "this process writes to a mode-500 folder, so an unremovable marker cannot be made");
    }
    let r = super::gpu::forget_marker(&marker);
    assert!(r.as_ref().is_err_and(|e| e.contains("cannot be removed")), "the caller is told: {r:?}");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(marker.exists(), "the marker is still there, as the error said");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A marker whose contents cannot be read is still just a marker: the set-up never reads it, and choosing where denoise
/// runs removes it (unlinking needs the folder's permission, not the file's). This used to leave the card off for good.
#[cfg(unix)]
#[test]
fn a_marker_that_cannot_be_read_still_keeps_the_card_off_and_can_be_cleared() {
    use std::os::unix::fs::PermissionsExt;
    let dir = folder("unreadable");
    let marker = dir.join(GPU_SETUP_MARKER);
    std::fs::write(&marker, "setting up the graphics card for AI Denoise (unix time 0)").unwrap();
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o000)).unwrap();
    let started = web_time::Instant::now();
    let r = set_up_card(&marker, Duration::from_secs(10), || Ok(1), || 2);
    assert_eq!(r, Err(GPU_CRASHED.to_string()), "the card is not used");
    assert!(started.elapsed() < Duration::from_secs(5), "and it does not try for ever");
    super::gpu::forget_marker(&marker).unwrap();
    assert!(!marker.exists(), "choosing where denoise runs clears it");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_set_up_that_could_not_make_its_marker_leaves_the_next_ones_alone() {
    use std::os::unix::fs::PermissionsExt;
    let dir = folder("unmade");
    let marker = dir.join(GPU_SETUP_MARKER);
    // the folder takes no new file, so this set-up runs without its crash guard
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    if std::fs::write(dir.join("made-anyway"), b"").is_ok() {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        return lightcraft_testkit::skip!(System, "this process writes to a mode-500 folder, so a marker that cannot be made cannot be made");
    }
    let (m, d) = (marker.clone(), dir.clone());
    let r = set_up_card(
        &marker,
        Duration::from_secs(10),
        move || {
            // meanwhile another process makes its own marker
            std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::write(&m, "setting up the graphics card for AI Denoise (unix time 1)").unwrap();
            Ok(())
        },
        || (),
    );
    assert_eq!(r, Ok(((), ())), "the set-up still runs, without its crash guard");
    assert!(marker.exists(), "the other process's marker is still there");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
