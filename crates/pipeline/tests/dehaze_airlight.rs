#[path = "support/dehaze_airlight.rs"]
mod dehaze_airlight;

#[test]
fn dehaze_whole_frame_uses_full_airlight() {
    dehaze_airlight::check(|src, info, s, req| lightcraft_pipeline::render(src, info, s, req).unwrap().image, true, 0);
}

#[test]
fn dehaze_partial_window_uses_full_airlight() {
    dehaze_airlight::check(|src, info, s, req| lightcraft_pipeline::render(src, info, s, req).unwrap().image, false, 1);
}

#[test]
fn dehaze_statistic_tracks_upstream_edits() {
    let cache = lightcraft_pipeline::StageCache::default();
    dehaze_airlight::check_edits(|src, info, s, req, cached| {
        if cached {
            lightcraft_pipeline::render_cached(src, info, s, req, &cache).unwrap().image
        } else {
            lightcraft_pipeline::render(src, info, s, req).unwrap().image
        }
    });
}

#[test]
fn enabling_dehaze_replaces_a_window_local_preparation() {
    let cache = lightcraft_pipeline::StageCache::default();
    dehaze_airlight::check_enabling(|src, info, s, req, cached| {
        if cached {
            lightcraft_pipeline::render_cached(src, info, s, req, &cache).unwrap().image
        } else {
            lightcraft_pipeline::render(src, info, s, req).unwrap().image
        }
    });
}
