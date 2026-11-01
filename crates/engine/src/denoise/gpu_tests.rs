use super::*;
use lightcraft_denoise::manifest::{Domain, Gain};

/// A folder of the test's own: the card's set-up leaves its marker beside the model.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-engine-gpu-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
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
        tile: tile as u32,
        overlap: 16,
        gain: Gain::MatchMean { nominal: 1.0, max_deviation: 0.05 },
    };
    let loaded = default_loader()(&path, &manifest).unwrap();
    let cpu = lightcraft_denoise::runtime::CpuRunner::load(&path, &manifest).unwrap();
    (loaded, cpu, path)
}
/// The two cards' sides: the editing card `editing`, no second card.
fn one_card(editing: Result<GpuSide, String>) -> [CardSide; 2] {
    two_cards(Err(lightcraft_gpu::ai::NO_HELPER.into()), editing)
}

/// The two cards' sides, set up as given.
fn two_cards(helper: Result<GpuSide, String>, editing: Result<GpuSide, String>) -> [CardSide; 2] {
    use lightcraft_gpu::Role;
    let side = |card, g| {
        let s = CardSide::new(card);
        let _ = s.gpu.set(g);
        s
    };
    [side(Role::Helper, helper), side(Role::Primary, editing)]
}

/// A network on `cpu` with these cards.
fn with_cards(cpu: &lightcraft_denoise::runtime::CpuRunner, path: &Path, cards: [CardSide; 2]) -> Network {
    Network { cpu: cpu.clone(), path: path.to_path_buf(), tile: 32, cards, picking: Mutex::new(()), last_threads: AtomicUsize::new(0), remote: None }
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
/// Each graphics card is set up on its own and passes the check tile against the CPU ([`GPU_AGREE`]); the two cards'
/// answers on a test tile agree as closely. A card that isn't here (no second card: `LIGHTCRAFT_GPU_HELPER=same`
/// makes one on the same card) is skipped.
#[test]
fn each_card_passes_the_check_tile() {
    let (_, cpu, path) = model("eachcard", 64);
    let (input, _) = lightcraft_denoise::runtime::test_tile(64);
    let want = cpu.run(&input).unwrap();
    let mut answers = Vec::new();
    for card in lightcraft_gpu::ai::BACKGROUND_ORDER {
        match make_gpu(&cpu, &path, 64, card) {
            Ok(g) => {
                let got = g.runner.run(&input).unwrap();
                let off = disagreement(&want, &got);
                eprintln!("{} card: {} — check tile {:.1} ms (CPU {:.0} ms), off by {off:.1e}", card.id(), g.adapter, g.card_ms, g.cpu_ms);
                assert!(off <= GPU_AGREE, "{}: {off:e}", g.adapter);
                answers.push(got);
            }
            Err(why) => lightcraft_testkit::skip!(Gpu, "AI Denoise on the {} card: {why}", card.id()),
        }
        assert!(!path.with_file_name(setup_marker(card)).exists(), "a set-up that ended leaves no marker");
    }
    if let [a, b] = answers.as_slice() {
        let off = disagreement(a, b);
        eprintln!("the two cards differ by {off:.1e}");
        assert!(off <= GPU_AGREE);
    }
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
    net.cards = one_card(Ok(GpuSide { runner: Box::new(cpu.clone()), adapter: "Fast card".into(), card_ms: 1e-6, cpu_ms: 1000.0 }));
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
    let model = with_cards(&cpu, &path, one_card(Ok(card)));
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
/// A card that runs the CPU's model and counts its tiles.
struct Counting(lightcraft_denoise::runtime::CpuRunner, Arc<AtomicUsize>);

impl TileRunner for Counting {
    fn run(&self, input: &[f32]) -> Result<Vec<f32>, RunError> {
        self.1.fetch_add(1, Ordering::Relaxed);
        self.0.run(input)
    }
}

fn counting(cpu: &lightcraft_denoise::runtime::CpuRunner, name: &str) -> (GpuSide, Arc<AtomicUsize>) {
    let n = Arc::new(AtomicUsize::new(0));
    (GpuSide { runner: Box::new(Counting(cpu.clone(), n.clone())), adapter: name.into(), card_ms: 1.0, cpu_ms: 1000.0 }, n)
}

/// Decision 7 (31 Oct 2026): photos of a batch run on both cards at once, the second graphics card first, each
/// photo's tiles on its own card, with the CPU's answers.
#[test]
fn two_photos_run_on_two_cards_the_helper_first() {
    let (_, cpu, path) = model("twocards", 64);
    let ((helper, on_helper), (editing, on_editing)) = (counting(&cpu, "Helper card"), counting(&cpu, "Editing card"));
    let net = with_cards(&cpu, &path, two_cards(Ok(helper), Ok(editing)));
    assert_eq!(net.lanes(RunOn::Gpu), 2, "a photo on each card");
    assert_eq!(net.lanes(RunOn::Cpu), 1, "the processor chosen: one photo at a time");
    let (input, _) = lightcraft_denoise::runtime::test_tile(64);
    let want = cpu.run(&input).unwrap();
    let (first, at_once) = net.runner(RunOn::Gpu, 16);
    assert_eq!(at_once, GPU_PARALLEL);
    assert_eq!(first.run(&input).unwrap(), want);
    assert_eq!((on_helper.load(Ordering::Relaxed), on_editing.load(Ordering::Relaxed)), (1, 0), "the first photo takes the helper");
    let (second, _) = net.runner(RunOn::Gpu, 16);
    assert_eq!(second.run(&input).unwrap(), want);
    assert_eq!(on_editing.load(Ordering::Relaxed), 1, "the second photo takes the free card");
    let d = net.device(RunOn::Gpu);
    assert_eq!((d["kind"].as_str(), d["adapter"].as_str()), (Some("gpu"), Some("Helper card")), "{d}");
    let cards = d["cards"].as_array().unwrap();
    assert_eq!(
        cards.iter().map(|c| (c["card"].as_str().unwrap(), c["photos"].as_u64().unwrap())).collect::<Vec<_>>(),
        [("helper", 1), ("editing", 1)]
    );
    drop(first);
    let (third, _) = net.runner(RunOn::Gpu, 16);
    third.run(&input).unwrap();
    assert_eq!(on_helper.load(Ordering::Relaxed), 2, "a card given back takes the next photo");
    drop((second, third));
    assert!(net.cards.iter().all(|c| c.photos.load(Ordering::Relaxed) == 0), "every card was given back");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// A second graphics card that gets tiles wrong: its tiles are the CPU's, and after three it gets no more photos;
/// the card for editing takes them, and the batch never loses a tile.
#[test]
fn a_broken_helper_leaves_the_work_to_the_editing_card_and_the_cpu() {
    let (_, cpu, path) = model("brokenhelper", 64);
    let garbage = Arc::new(AtomicUsize::new(0));
    struct Shared(Arc<AtomicUsize>);
    impl TileRunner for Shared {
        fn run(&self, input: &[f32]) -> Result<Vec<f32>, RunError> {
            Garbage(AtomicUsize::new(0)).run(input).inspect(|_| {
                self.0.fetch_add(1, Ordering::Relaxed);
            })
        }
    }
    let helper = GpuSide { runner: Box::new(Shared(garbage.clone())), adapter: "Broken card".into(), card_ms: 1.0, cpu_ms: 1000.0 };
    let (editing, on_editing) = counting(&cpu, "Editing card");
    let net = with_cards(&cpu, &path, two_cards(Ok(helper), Ok(editing)));
    let (input, _) = lightcraft_denoise::runtime::test_tile(64);
    let want = cpu.run(&input).unwrap();
    let (photo, _) = net.runner(RunOn::Gpu, 8);
    for _ in 0..GPU_MAX_FALLBACKS + 2 {
        assert_eq!(photo.run(&input).unwrap(), want, "a wrong tile is the CPU's tile");
    }
    assert_eq!(garbage.load(Ordering::Relaxed), GPU_MAX_FALLBACKS, "after three wrong tiles the card is not asked again");
    drop(photo);
    assert_eq!(net.lanes(RunOn::Gpu), 1, "one card left");
    let (next, at_once) = net.runner(RunOn::Gpu, 8);
    assert_eq!(at_once, GPU_PARALLEL, "the next photo still runs on a card");
    assert_eq!(next.run(&input).unwrap(), want);
    assert_eq!((garbage.load(Ordering::Relaxed), on_editing.load(Ordering::Relaxed)), (GPU_MAX_FALLBACKS, 1), "on the editing card");
    let d = net.device(RunOn::Gpu);
    assert_eq!(d["adapter"], "Editing card", "{d}");
    let helper = d["cards"].as_array().unwrap().iter().find(|c| c["card"] == "helper").cloned().unwrap();
    assert!(helper["reason"].as_str().is_some_and(|r| r.contains("tiles wrong")), "{d}");
    // both broken: the CPU does it all
    net.cards[1].fallbacks.store(GPU_MAX_FALLBACKS, Ordering::Relaxed);
    drop(next);
    let (cpu_only, at_once) = net.runner(RunOn::Gpu, 8);
    assert_eq!((at_once, cpu_only.run(&input).unwrap()), (8, want));
    assert_eq!(net.device(RunOn::Gpu)["kind"], "cpu");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_slow_card_is_left_out_unless_it_is_chosen() {
    let (_, cpu, path) = model("slow", 64);
    let slow = GpuSide { runner: Box::new(cpu.clone()), adapter: "Slow card".into(), card_ms: 490.0, cpu_ms: 1400.0 };
    let model = with_cards(&cpu, &path, one_card(Ok(slow)));
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
    let all = model.device(RunOn::Gpu);
    // a second graphics card (when this machine has one) is not the card that crashed: it may do the work
    let device = all["cards"].as_array().and_then(|c| c.iter().find(|c| c["card"] == "editing")).cloned().unwrap_or(all.clone());
    if all["cards"].is_null() {
        assert_eq!(at_once, 4, "the CPU does the work");
    }
    assert_eq!((device["kind"].as_str(), device["reason"].as_str()), (Some("cpu"), Some(GPU_CRASHED)), "{all}");
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
