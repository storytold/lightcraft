//! A panorama or an HDR merge of raw files keeps their as-shot white balance (issue #1144).
//!
//! CR2, NEF and the like carry their white balance as vendor multipliers, not as DNG tags. The merged DNG has
//! to say it as `AsShotNeutral`, or a viewer has no white to start from and shows a colour cast.

use lightcraft_color::{D50, Xy};
use lightcraft_geom::Orientation;
use lightcraft_merge::frame::{Frame, FrameColor, consensus_multipliers, detect_clip};
use lightcraft_merge::hdr::{HdrOptions, HdrResult, merge_hdr};
use lightcraft_merge::output::color_data;
use lightcraft_merge::pano::{PanoOptions, PanoResult, Projection, stitch};
use lightcraft_merge::{DngSamples, no_progress, write_linear_dng};
use lightcraft_meta::Metadata;
use lightcraft_raster::Rgb32f;
use lightcraft_raw::ColorData;

type Multipliers = Option<[f32; 3]>;

/// A camera that sees linear sRGB, with the colour matrices a DNG gives but no white balance of its own, as a raw
/// file whose white balance is only in the vendor multipliers would be after the app has worked its colour out.
fn camera_without_white_balance() -> ColorData {
    let srgb = FrameColor::Linear {
        to_xyz_d50: lightcraft_color::bradford(lightcraft_color::D65, lightcraft_color::D50).mul(&lightcraft_color::SRGB.to_xyz()),
    };
    ColorData { as_shot_neutral: None, ..color_data(&srgb) }
}

fn raw_frame(image: Rgb32f, color: &ColorData, wb_multipliers: Multipliers) -> Frame {
    let clip = detect_clip(&image);
    Frame {
        image,
        clip,
        exposure: None,
        raw: true,
        color: FrameColor::Camera(Box::new(color.clone())),
        orientation: Orientation::Normal,
        metadata: Metadata::default(),
        baseline_exposure: 0.0,
        lens: Vec::new(),
        wb_multipliers,
    }
}

/// Overlapping views of one scene, one frame per entry of `wb`, in order.
fn shoot(color: &ColorData, wb: &[Multipliers]) -> Vec<Frame> {
    let n = wb.len();
    let yaws: Vec<f64> = (0..n).map(|i| (i as f64 - (n as f64 - 1.0) / 2.0) * 25.0).collect();
    let views = lightcraft_merge::synth::pano_views(400, 300, 350.0, &yaws).unwrap();
    views.into_iter().zip(wb).map(|(image, m)| raw_frame(image, color, *m)).collect()
}

fn pano(frames: Vec<Frame>) -> PanoResult {
    stitch(frames, &PanoOptions { projection: Projection::Cylindrical, ..Default::default() }, &no_progress).unwrap()
}

fn neutral_of(res: &PanoResult) -> Option<[f64; 3]> {
    match &res.color {
        FrameColor::Camera(c) => c.as_shot_neutral,
        FrameColor::Linear { .. } => None,
    }
}

fn reciprocal(m: [f64; 3]) -> [f64; 3] {
    m.map(|v| 1.0 / v)
}

fn assert_close(got: [f64; 3], want: [f64; 3], tolerance: f64) {
    assert!(got.iter().zip(want).all(|(a, b)| (a - b).abs() <= tolerance), "{got:?} vs {want:?}");
}

/// The panorama as a DNG and read back with the repo's own raw reader.
fn dng_of(res: &PanoResult) -> lightcraft_raw::RawImage {
    let bytes = write_linear_dng(&res.image, &res.color, Orientation::Normal, &res.metadata, res.baseline_exposure, DngSamples::U16, &[]).unwrap();
    lightcraft_raw::decode(&bytes).unwrap()
}

const DAYLIGHT: [f32; 3] = [2.0, 1.0, 1.6];

#[test]
fn a_panorama_of_an_odd_number_of_raw_frames_has_their_median_white_balance() {
    // three frames the camera balanced slightly differently: the middle R/G and the middle B/G, from different frames
    let wb = [Some([2.1, 1.0, 1.55]), Some([1.9, 1.0, 1.7]), Some(DAYLIGHT)];
    let res = pano(shoot(&camera_without_white_balance(), &wb));
    assert_eq!(res.used, [0, 1, 2]);
    assert_close(neutral_of(&res).expect("an as-shot neutral"), reciprocal([2.0, 1.0, 1.6f32.into()]), 1e-6);
    let FrameColor::Camera(c) = &res.color else { panic!("camera colour") };
    assert_eq!(c.as_shot_white_xy, None);
}

#[test]
fn a_panorama_of_an_even_number_of_frames_takes_the_mean_of_the_middle_two() {
    let wb = [Some([1.8, 1.0, 1.9]), Some([2.0, 1.0, 1.6]), Some([2.2, 1.0, 1.5]), Some([2.4, 1.0, 1.4])];
    let res = pano(shoot(&camera_without_white_balance(), &wb));
    assert_eq!(res.used.len(), 4);
    let (r, b) = ((f64::from(2.0f32) + f64::from(2.2f32)) / 2.0, (f64::from(1.6f32) + f64::from(1.5f32)) / 2.0);
    assert_close(neutral_of(&res).expect("an as-shot neutral"), reciprocal([r, 1.0, b]), 1e-6);
    // two frames: the mean of both
    let res = pano(shoot(&camera_without_white_balance(), &[Some([2.0, 1.0, 1.0]), Some([3.0, 1.0, 2.0])]));
    assert_close(neutral_of(&res).expect("an as-shot neutral"), reciprocal([2.5, 1.0, 1.5]), 1e-6);
}

#[test]
fn frames_without_multipliers_do_not_vote() {
    // one frame of three has none (a DNG, say): the other two decide
    let wb = [Some([2.0, 1.0, 1.6]), None, Some([2.2, 1.0, 1.4])];
    let res = pano(shoot(&camera_without_white_balance(), &wb));
    assert_eq!(res.used.len(), 3);
    assert_close(neutral_of(&res).expect("an as-shot neutral"), reciprocal([2.1, 1.0, 1.5]), 1e-6);
    // a first frame without them doesn't hide the others'
    let wb = [None, Some(DAYLIGHT), Some(DAYLIGHT)];
    let res = pano(shoot(&camera_without_white_balance(), &wb));
    assert_close(neutral_of(&res).expect("an as-shot neutral"), reciprocal([2.0, 1.0, 1.6f32.into()]), 1e-6);
    // unusable values don't count either
    let wb = [Some([f32::NAN, 1.0, 1.0]), Some(DAYLIGHT), Some([0.0, 1.0, 1.4]), Some([2.0, f32::INFINITY, 1.6]), Some(DAYLIGHT)];
    let res = pano(shoot(&camera_without_white_balance(), &wb));
    assert_close(neutral_of(&res).expect("an as-shot neutral"), reciprocal([2.0, 1.0, 1.6f32.into()]), 1e-6);
}

#[test]
fn a_frame_outside_the_panorama_does_not_vote() {
    let color = camera_without_white_balance();
    let mut frames = shoot(&color, &[Some(DAYLIGHT), Some(DAYLIGHT), Some(DAYLIGHT)]);
    // four photos of other scenes, balanced very differently: counted, they would be the majority
    let others = lightcraft_scenes::demo_library().into_iter().filter(|s| s.kind != lightcraft_scenes::Kind::Canyon).take(4);
    frames.extend(others.map(|scene| raw_frame(scene.render(400, 300), &color, Some([4.0, 1.0, 0.8]))));
    assert_eq!(frames.len(), 7);
    let res = pano(frames);
    assert_eq!(res.used, [0, 1, 2], "the others don't connect");
    assert_close(neutral_of(&res).expect("an as-shot neutral"), reciprocal([2.0, 1.0, 1.6f32.into()]), 1e-6);
}

#[test]
fn without_any_multipliers_the_colour_is_the_first_frames_as_before() {
    for source in [
        camera_without_white_balance(),
        ColorData::default(),
        ColorData { as_shot_neutral: Some([0.5, 1.0, 0.62]), ..camera_without_white_balance() },
    ] {
        let frames = shoot(&source, &[None, None, None]);
        let first = frames[0].color.clone();
        let res = pano(frames);
        assert_eq!(res.color, first);
        assert_eq!(neutral_of(&res), source.as_shot_neutral);
    }
    // a DNG's own AsShotWhiteXY survives when no frame brings multipliers
    let xy = ColorData { as_shot_white_xy: Some(D50), ..camera_without_white_balance() };
    assert_eq!(pano(shoot(&xy, &[None, None, None])).color, FrameColor::Camera(Box::new(xy)));
}

#[test]
fn the_consensus_replaces_a_neutral_or_white_point_the_first_frame_had() {
    // a neutral the app worked out for the first frame, and a white point of the source: both give way
    let had = ColorData { as_shot_neutral: Some([0.9, 1.0, 0.8]), as_shot_white_xy: Some(Xy::new(0.31, 0.33)), ..camera_without_white_balance() };
    let res = pano(shoot(&had, &[Some(DAYLIGHT), Some(DAYLIGHT), Some(DAYLIGHT)]));
    assert_close(neutral_of(&res).expect("an as-shot neutral"), reciprocal([2.0, 1.0, 1.6f32.into()]), 1e-6);
    let FrameColor::Camera(c) = &res.color else { panic!("camera colour") };
    assert_eq!(c.as_shot_white_xy, None);
    // everything else of the first frame's colour is as it was
    assert_eq!(**c, ColorData { as_shot_neutral: c.as_shot_neutral, as_shot_white_xy: None, ..had });
}

#[test]
fn the_dng_says_the_median_white_balance_as_as_shot_neutral() {
    let source = camera_without_white_balance();
    let wb = [Some([2.1, 1.0, 1.55]), Some([1.9, 1.0, 1.7]), Some(DAYLIGHT)];
    let frames = shoot(&source, &wb);
    let median = consensus_multipliers(&frames).unwrap();
    let res = pano(frames);
    let raw = dng_of(&res);
    // AsShotNeutral is the reciprocal of the median multipliers, to the precision of a TIFF rational
    let got = raw.color.as_shot_neutral.expect("AsShotNeutral in the DNG");
    assert_close(got, reciprocal(median.map(f64::from)), 2e-6);
    assert_close(got, [0.5, 1.0, 0.625], 2e-6);
    assert_eq!(raw.color.as_shot_white_xy, None, "the DNG spec has the two tags exclude each other");
    // what a viewer starts from: the white and the camera transform the source's multipliers give
    let want = lightcraft_raw::color::as_shot_white_of(&source, Some(median));
    let xy = lightcraft_raw::color::as_shot_white_xy(&raw);
    assert!((xy.x - want.x).abs() < 1e-4 && (xy.y - want.y).abs() < 1e-4, "{xy:?} vs {want:?}");
    let (got, want) = (lightcraft_raw::color::camera_transform(&raw, xy), lightcraft_raw::color::camera_transform_of(&source, want));
    assert!(got.wb.iter().zip(want.wb).all(|(a, b)| (a - b).abs() < 1e-3), "{:?} vs {:?}", got.wb, want.wb);
    // the colour model and the picture are the source's, and the same without the colour matrices
    assert_eq!(raw.color.forward_matrix[0].is_some(), source.forward_matrix[0].is_some());
    assert_eq!((raw.width, raw.height, raw.cpp), (res.image.width, res.image.height, 3));
    let bare = dng_of(&pano(shoot(&ColorData::default(), &wb)));
    assert_close(bare.color.as_shot_neutral.expect("AsShotNeutral without colour matrices"), [0.5, 1.0, 0.625], 2e-6);
}

#[test]
fn without_multipliers_the_dng_has_no_as_shot_neutral_as_before() {
    let res = pano(shoot(&ColorData::default(), &[None, None, None]));
    let raw = dng_of(&res);
    assert_eq!((raw.color.as_shot_neutral, raw.color.as_shot_white_xy), (None, None));
}

/// Exposures `evs` (EV, relative to the view's) of one view, one frame each, with `wb` as their multipliers.
fn bracket(color: &ColorData, evs: &[f64], wb: &[Multipliers]) -> Vec<Frame> {
    let view = lightcraft_merge::synth::pano_views(320, 240, 280.0, &[0.0]).unwrap().remove(0);
    evs.iter()
        .zip(wb)
        .map(|(ev, m)| {
            let k = 2f32.powf(*ev as f32);
            let mut frame = raw_frame(view.map(|p| p.map(|v| (v * k).min(1.0))), color, *m);
            frame.exposure = Some(*ev);
            frame
        })
        .collect()
}

fn hdr(frames: Vec<Frame>) -> HdrResult {
    merge_hdr(frames, &HdrOptions::default(), &no_progress).unwrap()
}

fn hdr_neutral(res: &HdrResult) -> Option<[f64; 3]> {
    match &res.color {
        FrameColor::Camera(c) => c.as_shot_neutral,
        FrameColor::Linear { .. } => None,
    }
}

#[test]
fn an_hdr_merge_of_raw_frames_has_their_median_white_balance() {
    let source = camera_without_white_balance();
    let wb = [Some([2.1, 1.0, 1.55]), Some(DAYLIGHT), Some([1.9, 1.0, 1.7])];
    let res = hdr(bracket(&source, &[-2.0, 0.0, 2.0], &wb));
    assert_eq!(res.wb_multipliers, Some(DAYLIGHT));
    assert_close(hdr_neutral(&res).expect("an as-shot neutral"), reciprocal([2.0, 1.0, 1.6f32.into()]), 1e-6);
    // the DNG says it, to the precision of a TIFF rational
    let bytes =
        write_linear_dng(&res.radiance, &res.color, res.orientation, &res.metadata, res.baseline_exposure, DngSamples::Half, &res.lens).unwrap();
    let raw = lightcraft_raw::decode(&bytes).unwrap();
    assert_close(raw.color.as_shot_neutral.expect("AsShotNeutral in the DNG"), [0.5, 1.0, 0.625], 2e-6);
    // an even count takes the mean of the middle two; frames without multipliers do not vote
    let res = hdr(bracket(&source, &[-1.0, 1.0], &[Some([2.0, 1.0, 1.0]), Some([3.0, 1.0, 2.0])]));
    assert_close(hdr_neutral(&res).expect("an as-shot neutral"), reciprocal([2.5, 1.0, 1.5]), 1e-6);
    let res = hdr(bracket(&source, &[-2.0, 0.0, 2.0], &[None, Some([2.0, 1.0, 1.6]), Some([f32::NAN, 1.0, 1.0])]));
    assert_eq!(res.wb_multipliers, Some([2.0, 1.0, 1.6]));
    assert_close(hdr_neutral(&res).expect("an as-shot neutral"), reciprocal([2.0, 1.0, 1.6f32.into()]), 1e-6);
}

#[test]
fn an_hdr_merge_without_multipliers_keeps_the_reference_colour_as_before() {
    let source = ColorData { as_shot_neutral: Some([0.5, 1.0, 0.62]), ..camera_without_white_balance() };
    let frames = bracket(&source, &[-2.0, 0.0, 2.0], &[None, None, None]);
    let reference = frames[1].color.clone();
    let res = hdr(frames);
    assert_eq!((res.reference, res.wb_multipliers), (1, None));
    assert_eq!(res.color, reference);
}
