//! Two devices: the second graphics card's renders match the editing card's, batch work goes there
//! while it is ready, and its loss (or turning it off) leaves the editing card alone. On a machine
//! with one card the second device is made on that same card (`same`: the two-device logic, not
//! two cards' speed). Own process: the device and the second card are once per process.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lcc_develop::DevelopSettings;
use lcc_pipeline::{RenderRequest, SourceInfo};

fn wait_for(what: &str, ok: impl Fn() -> bool) {
    let t = Instant::now();
    while !ok() {
        assert!(t.elapsed() < Duration::from_secs(60), "{what}: still {:?}", lcc_gpu::helper_status());
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_second_device_renders_alike_and_its_loss_leaves_the_editing_card() {
    if !lcc_gpu::available() {
        lcc_testkit::skip!(Gpu, "no GPU adapter");
        return;
    }
    let editing = lcc_gpu::device_generation();
    // a second device on the same card (this machine may have one card)
    assert!(lcc_gpu::set_helper("same"));
    wait_for("the second device starts", || lcc_gpu::helper_status().0 != "starting" && lcc_gpu::helper_status().0 != "off");
    assert_eq!(lcc_gpu::helper_status().0, "ready", "{:?}", lcc_gpu::helper_status());

    let raw = SourceInfo { raw: true, ..Default::default() };
    let mut s = DevelopSettings::default();
    s.light.exposure = 0.4;
    s.effects.clarity = 15.0;
    s.detail.nr_luminance = 15.0;
    let src = Arc::new(lcc_scenes::demo_library()[1].render(900, 600));
    let req = RenderRequest::fit(900, 600);
    let on_editing = lcc_gpu::try_render(&src, &raw, &s, &req, None).expect("editing card");
    let before = lcc_gpu::helper_usage();
    let on_second = lcc_gpu::as_batch(|| lcc_gpu::try_render(&src, &raw, &s, &req, None)).expect("second device");
    assert_eq!(before.0, 0);
    assert!(on_second.image.data == on_editing.image.data, "both devices render the same pixels");
    assert_eq!(on_second.histogram, on_editing.histogram);
    // AI models go to the second card (here the same one)
    // (the editing card's name: with the adapter variable naming another card than the first
    // listed, the first listed is not the one in use)
    assert_eq!(lcc_gpu::ai_card(), lcc_gpu::adapter_name().map(|n| n.rsplit_once(" (").map_or(n.clone(), |(name, _)| name.to_string())));
    // a photo's turn on the second device: its renders run there
    let turn = lcc_gpu::as_batch(|| lcc_gpu::try_batch_turn(900 * 600, 900 * 600)).expect("a free turn");
    assert_eq!(lcc_gpu::helper_usage().0, 1, "the turn is the second device's");
    drop(turn);
    assert_eq!(lcc_gpu::helper_usage().0, 0);
    // a turn asked for on one card (an export set to one card) is that card's, and its renders run there
    assert!(lcc_gpu::helper_ready());
    for helper in [false, true] {
        lcc_gpu::as_batch(|| {
            let turn = lcc_gpu::try_batch_turn_on(helper, 900 * 600, 900 * 600).expect("a free turn on that card");
            assert_eq!(turn.on_helper(), helper);
            let before = lcc_gpu::thread_render_counts();
            lcc_gpu::try_render(&src, &raw, &s, &req, None).expect("renders on its card");
            let after = lcc_gpu::thread_render_counts();
            let (b, a) = if helper { (before.helper_batch, after.helper_batch) } else { (before.editing_batch, after.editing_batch) };
            assert_eq!(a, b + 1, "the photo rendered on the card it asked for (second card: {helper})");
        });
    }

    // both devices take batch photos: two cards take work, a photo at a time goes to each, and
    // each photo's renders run on its turn's card
    assert_eq!(lcc_gpu::batch_cards(), 2);
    assert_eq!(lcc_gpu::batch_lanes_hint(2), 4, "two photos per card");
    let (taken_tx, taken_rx) = std::sync::mpsc::channel::<bool>();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let go_rx = Arc::new(std::sync::Mutex::new(go_rx));
    let photo = |taken: std::sync::mpsc::Sender<bool>, go: Arc<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>| {
        let (src, raw, s, req) = (src.clone(), raw.clone(), s.clone(), req);
        std::thread::spawn(move || {
            lcc_gpu::as_batch(|| {
                let turn = lcc_gpu::wait_batch_turn_any(900 * 600, 900 * 600, &|| false).expect("a card has room");
                let on_helper = turn.on_helper();
                assert!(turn.card_name().is_some());
                let before = lcc_gpu::thread_render_counts();
                let r = lcc_gpu::try_render(&src, &raw, &s, &req, None).expect("renders on its card");
                let after = lcc_gpu::thread_render_counts();
                if on_helper {
                    assert_eq!(after.helper_batch, before.helper_batch + 1, "the photo rendered on the second device");
                } else {
                    assert_eq!(after.editing_batch, before.editing_batch + 1, "the photo rendered on the editing card");
                }
                taken.send(on_helper).unwrap();
                // (held until both photos have their turns)
                go.lock().unwrap().recv().unwrap();
                drop(turn);
                r
            })
        })
    };
    let a = photo(taken_tx.clone(), go_rx.clone());
    let first = taken_rx.recv().unwrap();
    // The second photo goes to the card that would finish it soonest at each card's measured batch
    // speed (`gate::order`): the other card, unless the first photo's card is at least twice as fast
    // (two photos there still finish before one on the other). On one card's two devices the speeds
    // are near alike, but they are measured from a render or two each, so a real card's (RADV)
    // first-render costs can make one device look twice as fast: the expectation follows the speeds.
    // The busy card's speed only falls while its turn is held (its busy time grows), so the ratio
    // read before the second photo asks bounds the one it was placed by from above: under 2, the
    // second photo must go to the other card; at 2 or more, either card can be right.
    let ratio = || match (lcc_gpu::batch_rate_of(first), lcc_gpu::batch_rate_of(!first)) {
        (Some(f), Some(o)) => f / o,
        _ => 1.0,
    };
    let at_most = ratio();
    let b = photo(taken_tx, go_rx);
    let second = taken_rx.recv().unwrap();
    assert!(at_most >= 2.0 || first != second, "one photo on each card: the first's card is only {at_most:.2}x as fast as the other");
    go_tx.send(()).unwrap();
    go_tx.send(()).unwrap();
    for h in [a, b] {
        let r = h.join().unwrap();
        assert!(r.image.data == on_editing.image.data, "both cards render the same pixels");
    }
    assert_eq!((lcc_gpu::helper_usage().0, lcc_gpu::batch_usage().jobs), (0, 0), "the turns were given back");
    assert!(lcc_gpu::batch_rate_of(true).is_some() && lcc_gpu::batch_rate_of(false).is_some(), "each card's speed is measured");

    // the second device fails: batch work goes back to the editing card, which is untouched
    assert!(lcc_gpu::simulate_helper_loss("test: the second card stopped"));
    assert_eq!(lcc_gpu::helper_status().0, "lost");
    assert_eq!(lcc_gpu::device_generation(), editing, "the editing card's device stays");
    assert!(lcc_gpu::lost().is_none(), "the editing card is not lost");
    let after = lcc_gpu::as_batch(|| lcc_gpu::try_render(&src, &raw, &s, &req, None)).expect("editing card again");
    assert!(after.image.data == on_editing.image.data);

    // chosen again, then turned off
    assert!(lcc_gpu::set_helper("auto"));
    assert!(lcc_gpu::set_helper("same"));
    wait_for("the second device starts again", || lcc_gpu::helper_status().0 == "ready");
    assert!(lcc_gpu::set_helper("off"));
    wait_for("the second device stops", || lcc_gpu::helper_status().0 == "off");
    assert_eq!(lcc_gpu::device_generation(), editing);
}

/// Hostile settings never panic: text that is no card is Automatic, and a card that isn't there
/// leaves the device in use.
#[test]
fn unknown_card_settings_are_harmless() {
    assert_eq!(lcc_gpu::cards::HelperChoice::parse_setting("dx12:zz:zz:#0"), lcc_gpu::cards::HelperChoice::Auto);
    assert!(lcc_gpu::cards::AdapterKey::parse("vulkan:1002:1586:nothing#99999999999").is_none());
    assert!(lcc_gpu::cards::AdapterKey::parse(":::").is_none());
}
