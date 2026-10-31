//! Previews built on the second graphics card (design: dual GPU of 2026-10-31, PR 4): Build
//! Previews renders on the second card first, the loupe never does, and a built preview equals the
//! loupe's own render of the photo within 1 LSB (§7.6). On a one-card machine both devices are on
//! that card (`set_helper("same")`); `LIGHTCRAFT_GPU_HELPER` picks another (a real second card).
//! Its own process: it changes the devices every render goes to.

use serde_json::json;

#[test]
fn previews_build_on_the_second_card_and_match_the_loupe() {
    if !lightcraft_engine::gpu::available() {
        lightcraft_testkit::skip!(Gpu, "no GPU adapter ({})", lightcraft_engine::gpu::unavailable_reason().unwrap_or_default());
        return;
    }
    // (with LIGHTCRAFT_GPU_HELPER set, that card: the variable overrides the setting)
    lightcraft_engine::gpu::set_helper("same").expect("a second device on the editing card");
    assert!(lightcraft_engine::gpu::wait_helper(std::time::Duration::from_secs(240)), "the second card's set-up didn't finish");
    if !lightcraft_engine::gpu::helper_ready() {
        lightcraft_testkit::skip!(Gpu, "no second device ({:?})", lightcraft_engine::gpu::helper_status());
        return;
    }
    eprintln!("editing card: {:?}, second card: {:?}", lightcraft_engine::gpu::card_in_use(), lightcraft_engine::gpu::helper_status());
    let mut s = lightcraft_engine::Session::with_demo();
    let ids: Vec<_> = s.catalog.photos().map(|p| p.id).collect();
    let before = lightcraft_engine::media::render_counts();
    let t = std::time::Instant::now();
    let r = s
        .execute("library.buildPreviews", &json!({"ids": ids.iter().map(|i| i.0).collect::<Vec<_>>(), "edge": 512, "wait": true, "force": true}))
        .expect("build");
    let built = lightcraft_engine::media::render_counts();
    eprintln!("built {} photos in {:?}: {built:?}", ids.len(), t.elapsed());
    assert_eq!(r["failed"], json!(0), "{r}");
    assert!(built.background_helper > before.background_helper, "the second card built previews: {before:?} → {built:?}");

    let mut worst = 0u8;
    for id in &ids {
        let mut job = s.loupe_job(*id, 512, 512, true).expect("loupe job");
        let (cache, key) = job.view_cache.clone().expect("the view keeps its render");
        let kept = cache.get(key).expect("the build kept the view preview");
        (job.view_cache, job.cache) = (None, None);
        let shown = job.run();
        assert!(shown.quick.is_none(), "rendered, not served from the built preview");
        let shown = shown.rendered.expect("loupe render").image;
        assert_eq!((kept.width, kept.height), (shown.width, shown.height));
        let d = kept.data.iter().zip(&shown.data).flat_map(|(a, b)| a.iter().zip(b).map(|(x, y)| x.abs_diff(*y))).max().unwrap_or(0);
        worst = worst.max(d);
    }
    let after = lightcraft_engine::media::render_counts();
    eprintln!("built previews vs the loupe's renders: max |Δ| = {worst} LSB; {after:?}");
    assert!(worst <= 1, "max |Δ| = {worst} LSB");
    assert_eq!(after.interactive_helper, 0, "the loupe never renders on the second card");
    assert!(after.interactive_editing > built.interactive_editing, "the loupe rendered on the editing card");

    // ---- second card off: everything on the editing card, as before
    lightcraft_engine::gpu::set_helper("off").expect("off");
    assert!(lightcraft_engine::gpu::wait_helper(std::time::Duration::from_secs(60)));
    let r = s.execute("library.buildPreviews", &json!({"ids": [ids[0].0], "edge": 512, "wait": true, "force": true})).expect("build");
    assert_eq!(r["failed"], json!(0), "{r}");
    let off = lightcraft_engine::media::render_counts();
    assert_eq!(off.background_helper, after.background_helper, "nothing on the second card once it is off");
    assert!(off.background_editing > after.background_editing, "the build renders on the editing card");
}
