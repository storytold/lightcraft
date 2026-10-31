//! Two devices at once (design: dual GPU, PR 2): the second card's device renders the same picture
//! as the editing card's, batch turns go to it first, the editing card yields to the interface, a
//! loss of the second card leaves the editing card rendering, and turning it off releases its
//! memory. On a one-card machine both devices are on the same card (`set_helper("same")`); with a
//! second card the last part renders on it too. Its own process: it changes the devices the others
//! render on.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lightcraft_develop::DevelopSettings;
use lightcraft_gpu::Role;
use lightcraft_pipeline::{RenderRequest, SourceInfo, render};
use lightcraft_raster::Rgba8;

/// (mean, max) difference of the colour channels.
fn diff(a: &Rgba8, b: &Rgba8) -> (f64, u8) {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let mut sum = 0u64;
    let mut max = 0u8;
    for (p, q) in a.data.iter().zip(&b.data) {
        for c in 0..3 {
            let d = p[c].abs_diff(q[c]);
            sum += d as u64;
            max = max.max(d);
        }
    }
    (sum as f64 / (a.data.len() * 3) as f64, max)
}

fn wait_helper_ready() {
    assert!(lightcraft_gpu::wait_helper(Duration::from_secs(240)), "the second card's set-up didn't finish");
}

#[test]
fn two_devices_render_alike_and_each_card_goes_its_own_way() {
    if !lightcraft_gpu::available() {
        lightcraft_testkit::skip!(Gpu, "no GPU adapter ({})", lightcraft_gpu::unavailable_reason().unwrap_or_default());
        return;
    }
    let editing = lightcraft_gpu::card_in_use().unwrap_or_default();
    eprintln!("editing card: {editing}");
    let (w, h) = (640, 427);
    let src = Arc::new(lightcraft_scenes::demo_library()[2].render(w, h));
    let info = SourceInfo { raw: true, ..Default::default() };
    let mut s = DevelopSettings::default();
    s.light.exposure = 0.4;
    s.effects.clarity = 25.0;
    s.detail.nr_luminance = 20.0;
    let req = RenderRequest::fit(w, h);
    let cpu = render(&src, &info, &s, &req).image;
    let primary = lightcraft_gpu::try_render(&src, &info, &s, &req, None).expect("renders on the editing card").image;
    let (mean, max) = diff(&primary, &cpu);
    eprintln!("editing card vs CPU: mean {mean:.3}, max {max}");
    assert!(mean < 0.5 && max <= 3, "editing card vs CPU: mean {mean}, max {max}");

    // ---- a second device on the same card
    assert!(!lightcraft_gpu::helper_ready(), "no second card before it is set up");
    lightcraft_gpu::set_helper("same").expect("same");
    let t = Instant::now();
    wait_helper_ready();
    let st = lightcraft_gpu::helper_status();
    eprintln!("second device: {st:?} after {:?}", t.elapsed());
    assert_eq!(st.state, "ready", "{st:?}");
    assert!(lightcraft_gpu::helper_ready());
    let on_helper = lightcraft_gpu::with_role(Role::Helper, || {
        assert_eq!(lightcraft_gpu::current_role(), Role::Helper);
        lightcraft_gpu::try_render(&src, &info, &s, &req, None)
    })
    .expect("renders on the second device")
    .image;
    assert_eq!(lightcraft_gpu::current_role(), Role::Primary, "the role is the thread's only inside with_role");
    let (mean, max) = diff(&on_helper, &cpu);
    eprintln!("second device vs CPU: mean {mean:.3}, max {max}");
    assert!(mean < 0.5 && max <= 3, "second device vs CPU: mean {mean}, max {max}");
    let (mean, max) = diff(&on_helper, &primary);
    assert!(max <= 1, "two devices on one card: mean {mean}, max {max}");
    assert!(lightcraft_gpu::memory_of(Role::Helper).allocated > 0, "the second device's buffers are counted as its own");

    // ---- batch turns: the second card first, then the editing card
    lightcraft_gpu::as_batch(|| {
        let a = lightcraft_gpu::batch_turn_any(w * h, w * h).expect("a turn");
        assert_eq!(a.role(), Role::Helper);
        assert_eq!(lightcraft_gpu::current_role(), Role::Helper, "the turn routes the thread's renders");
        assert!(a.card_name().is_some());
        let r = lightcraft_gpu::try_render(&src, &info, &s, &req, None).expect("renders in the turn").image;
        assert!(diff(&r, &on_helper).1 <= 1);
        drop(a);
        assert_eq!(lightcraft_gpu::current_role(), Role::Primary);
        let b = lightcraft_gpu::try_batch_turn_on(Role::Primary, w * h, w * h).expect("the editing card has room");
        assert_eq!(b.role(), Role::Primary);
    });
    // two photos at once, the interface idle: one on each card (a 2-lane build uses both)
    assert_eq!(lightcraft_gpu::batch_cards(), 2);
    assert_eq!(lightcraft_gpu::batch_lanes_hint(2), 4);
    lightcraft_gpu::as_batch(|| {
        let a = lightcraft_gpu::batch_turn_any(w * h, w * h).expect("a turn");
        let b = lightcraft_gpu::batch_turn_any(w * h, w * h).expect("a second turn");
        assert_ne!(a.role(), b.role(), "both cards get a photo");
    });

    // ---- §8: batch work leaves the editing card to the interface
    lightcraft_gpu::track_interaction(true);
    lightcraft_gpu::set_interaction_idle(Duration::from_millis(300));
    lightcraft_gpu::note_interaction();
    assert!(lightcraft_gpu::interface_busy());
    lightcraft_gpu::as_batch(|| {
        assert!(
            lightcraft_gpu::try_batch_turn_on(Role::Primary, w * h, w * h).is_none(),
            "no batch turn on the editing card while the interface is in use"
        );
        let h1 = lightcraft_gpu::batch_turn_any(w * h, w * h).expect("the second card doesn't wait");
        assert_eq!(h1.role(), Role::Helper);
    });
    std::thread::sleep(Duration::from_millis(450));
    assert!(!lightcraft_gpu::interface_busy());
    lightcraft_gpu::as_batch(|| {
        assert!(lightcraft_gpu::try_batch_turn_on(Role::Primary, w * h, w * h).is_some(), "a turn once the interface is idle")
    });
    lightcraft_gpu::track_interaction(false);

    // ---- the second card stops responding: the editing card goes on
    let seen = lightcraft_gpu::events_after(0).last().map_or(0, |e| e.seq);
    lightcraft_gpu::with_role(Role::Helper, || lightcraft_gpu::simulate_device_loss("device lost (Unknown): simulated driver reset"));
    assert!(!lightcraft_gpu::helper_ready());
    assert_eq!(lightcraft_gpu::helper_status().state, "lost");
    assert!(lightcraft_gpu::lost().is_none(), "the editing card isn't lost");
    let still = lightcraft_gpu::try_render(&src, &info, &s, &req, None).expect("the editing card still renders").image;
    assert!(diff(&still, &primary).1 <= 1);
    let events = lightcraft_gpu::events_after(seen);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].role, Role::Helper);
    assert!(events[0].text.contains("other graphics card"), "{}", events[0].text);
    lightcraft_gpu::as_batch(|| {
        let t = lightcraft_gpu::batch_turn_any(w * h, w * h).expect("a turn");
        assert_eq!(t.role(), Role::Primary, "batch work goes to the editing card meanwhile");
    });

    // ---- turned off: its device and memory go
    lightcraft_gpu::set_helper("off").expect("off");
    wait_helper_ready();
    let st = lightcraft_gpu::helper_status();
    assert_eq!(st.state, "off", "{st:?}");
    assert!(!lightcraft_gpu::helper_ready());
    assert_eq!(lightcraft_gpu::memory_of(Role::Helper).allocated, 0, "the second device's buffers are released");
    assert!(lightcraft_gpu::memory_of(Role::Primary).allocated > 0);

    // ---- a real second card, when there is one
    let other = lightcraft_gpu::list_adapters().into_iter().find(|c| c.usable && c.kind != "software" && c.name != editing);
    let Some(other) = other else {
        lightcraft_testkit::skip!(Gpu, "one graphics card: the second-card part ran on two devices of {editing}");
        return;
    };
    lightcraft_gpu::set_helper(&other.key).expect("the other card");
    let t = Instant::now();
    wait_helper_ready();
    let st = lightcraft_gpu::helper_status();
    eprintln!("second card: {st:?} after {:?}", t.elapsed());
    assert_eq!(st.state, "ready", "{st:?}");
    assert_eq!(st.name.as_deref(), Some(other.name.as_str()));
    let on_other = lightcraft_gpu::with_role(Role::Helper, || lightcraft_gpu::try_render(&src, &info, &s, &req, None))
        .expect("renders on the second card")
        .image;
    let (mean, max) = diff(&on_other, &cpu);
    eprintln!("{} vs CPU: mean {mean:.3}, max {max}", other.name);
    assert!(mean < 0.5 && max <= 3, "second card vs CPU: mean {mean}, max {max}");
    let (mean, max) = diff(&on_other, &primary);
    eprintln!("{} vs {editing}: mean {mean:.4}, max {max}", other.name);
    assert!(max <= 1, "two cards (decision 6: at most 1 LSB): mean {mean}, max {max}");
    lightcraft_gpu::set_helper("off").expect("off");
    wait_helper_ready();
}
