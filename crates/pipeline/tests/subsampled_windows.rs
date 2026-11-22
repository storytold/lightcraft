#[path = "support/subsampled.rs"]
mod subsampled;

#[test]
fn subsampled_windows_keep_full_frame_grid() {
    subsampled::check_windows(|src, info, s, req| lightcraft_pipeline::render(src, info, s, req).unwrap().image, false);
}

#[test]
fn subsampled_repeating_detail_keeps_full_frame_grid() {
    subsampled::check_windows(|src, info, s, req| lightcraft_pipeline::render(src, info, s, req).unwrap().image, true);
}
