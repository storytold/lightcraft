//! RC-B188-01: bypassing global Effects must preserve whole-frame airlight for active mask Dehaze.

use std::sync::Arc;

use lightcraft_develop::{DevelopSettings, LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape};
use lightcraft_geom::Point;
use lightcraft_pipeline::{PixelWindow, Quality, RenderRequest, SourceInfo, StageCache, plan, render, render_cached};
use lightcraft_raster::{Rgb32f, Rgba8};

const WINDOW: PixelWindow = PixelWindow { x: 24, y: 32, w: 48, h: 40 };

fn source() -> Arc<Rgb32f> {
    // Bright pixels outside the window determine the frame's airlight. The kept interior is
    // constant and eight pixels from the boundary, isolating airlight from filter-edge effects.
    Arc::new(Rgb32f::from_fn(128, 96, |x, y| if x > 94 || y < 12 { [0.8; 3] } else { [0.12; 3] }))
}

fn settings(dehaze: f64) -> DevelopSettings {
    DevelopSettings {
        masks: vec![Mask {
            id: 1,
            components: vec![MaskComponent {
                name: None,
                op: MaskOp::Add,
                invert: false,
                shape: MaskShape::Radial { center: Point::new(0.5, 0.5), rx: 1.0, ry: 1.0, angle: 0.0, feather: 0.0, invert: false },
            }],
            adjust: LocalAdjustments { dehaze, ..Default::default() },
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn assert_interior_matches(full: &Rgba8, window: &Rgba8, label: &str) {
    assert_eq!((window.width, window.height), (WINDOW.w, WINDOW.h));
    let (mut max, mut changed) = (0u8, 0usize);
    for y in 8..WINDOW.h - 8 {
        for x in 8..WINDOW.w - 8 {
            let expected = full.get(WINDOW.x + x, WINDOW.y + y);
            let actual = window.get(x, y);
            changed += usize::from(actual != expected);
            for channel in 0..3 {
                max = max.max(actual[channel].abs_diff(expected[channel]));
            }
        }
    }
    assert_eq!((max, changed), (0, 0), "{label}: window/full interior max channel error and changed pixels");
}

#[test]
fn mask_dehaze_windows_match_full_frame_when_global_effects_are_bypassed() {
    let src = source();
    let info = SourceInfo::default();
    for amount in [60.0, -60.0] {
        for quality in [Quality::Draft, Quality::Full] {
            let on = settings(amount);
            let mut off = on.clone();
            off.set_section_enabled("effects", false);
            let saved = off.clone();
            let req = RenderRequest { quality, ..RenderRequest::fit(128, 96) };
            let wr = RenderRequest { window: Some(WINDOW), ..req };
            let full = render(&src, &info, &on, &req).image;
            let neutral = render(&src, &info, &settings(0.0), &req).image;
            assert_ne!(full.get(48, 52), neutral.get(48, 52), "the fixture must visibly exercise local Dehaze");
            assert_eq!(render(&src, &info, &off, &req).image, full, "global Effects bypass preserves mask-only full rendering");

            // Healthy direct/cached controls, then reuse the same cache through bypass/re-enable.
            let cache = StageCache::default();
            let healthy = render(&src, &info, &on, &wr).image;
            assert_interior_matches(&full, &healthy, "healthy direct");
            assert_eq!(render_cached(&src, &info, &on, &wr, &cache).image, healthy);
            let direct = render(&src, &info, &off, &wr).image;
            assert_interior_matches(&full, &direct, &format!("bypassed direct {amount} {quality:?}"));
            let cold = StageCache::default();
            assert_eq!(render_cached(&src, &info, &off, &wr, &cold).image, direct, "cold cache");
            assert_eq!(render_cached(&src, &info, &off, &wr, &cold).image, direct, "warm cache");
            assert_eq!(render_cached(&src, &info, &off, &wr, &cache).image, direct, "cache reused after bypass");
            assert_eq!(plan(&src, &info, &off, &wr).settings.masks, on.masks);
            assert_eq!(off, saved, "planning and rendering must leave stored settings intact");
            off.set_section_enabled("effects", true);
            assert_eq!(off, on, "re-enable restores the stored settings");
            assert_eq!(render_cached(&src, &info, &off, &wr, &cache).image, healthy, "cache reused after re-enable");
        }
    }
}

#[test]
fn bypassed_global_dehaze_without_active_masks_skips_frame_airlight() {
    let src = source();
    let info = SourceInfo::default();
    let req = RenderRequest { window: Some(WINDOW), ..RenderRequest::fit(128, 96) };
    let neutral = render(&src, &info, &DevelopSettings::default(), &req).image;
    for inactive in ["absent", "hidden", "empty", "zero dehaze"] {
        let mut s = settings(60.0);
        match inactive {
            "absent" => s.masks.clear(),
            "hidden" => s.masks[0].visible = false,
            "empty" => s.masks[0].components.clear(),
            "zero dehaze" => s.masks[0].adjust.dehaze = 0.0,
            _ => unreachable!(),
        }
        s.effects.dehaze = -60.0;
        s.set_section_enabled("effects", false);
        let saved = s.clone();
        let p = plan(&src, &info, &s, &req);
        assert_eq!(p.settings.effects.dehaze, 0.0);
        assert_eq!(p.fixed_air, None, "{inactive}: no rendered Dehaze needs whole-frame airlight");
        let direct = render(&src, &info, &s, &req).image;
        assert_eq!(direct, neutral, "{inactive}: bypassed global and inactive local Dehaze are neutral");
        assert_eq!(render_cached(&src, &info, &s, &req, &StageCache::default()).image, direct);
        assert_eq!(s, saved);
        s.set_section_enabled("effects", true);
        assert!(plan(&src, &info, &s, &req).fixed_air.is_some(), "global Dehaze re-enabled");
    }
}

#[test]
fn window_airlight_tracks_visible_masks_independently_of_global_effects() {
    let src = source();
    let info = SourceInfo::default();
    let full = RenderRequest::fit(128, 96);
    let window = RenderRequest { window: Some(WINDOW), ..full };
    let mut s = settings(60.0);
    assert_eq!(plan(&src, &info, &s, &full).fixed_air, None, "full renders estimate their own airlight");
    assert!(plan(&src, &info, &s, &window).fixed_air.is_some(), "healthy local Dehaze");
    s.masks[0].visible = false;
    assert_eq!(plan(&src, &info, &s, &window).fixed_air, None, "hidden mask");
    s.set_section_enabled("effects", false);
    s.masks[0].visible = true;
    assert!(plan(&src, &info, &s, &window).fixed_air.is_some(), "active local Dehaze with global Effects bypassed");
    s.masks[0].components.clear();
    assert_eq!(plan(&src, &info, &s, &window).fixed_air, None, "empty mask");
}
