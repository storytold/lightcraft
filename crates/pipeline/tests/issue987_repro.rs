// Regression for issue #987: a curve must not move its own input backdrop.
use lightcraft_develop::DevelopSettings;
use lightcraft_geom::Point;
use lightcraft_pipeline::{
    RenderRequest, SourceInfo,
    curve_input::{CurveInput, render},
};
use lightcraft_raster::Rgb32f;
use std::sync::Arc;

#[test]
fn the_curve_backdrop_must_not_follow_its_own_curve() {
    let src = Arc::new(Rgb32f::from_fn(128, 8, |x, _| [0.02 + x as f32 / 512.0; 3]));
    let req = RenderRequest::fit(128, 8);
    let mut s = DevelopSettings::default();
    // Previously the backdrop used the rendered output, which changes here.
    let before = render(&src, &SourceInfo::default(), &s, &req, None, CurveInput::Master).histogram;
    s.curve.master = vec![Point::new(0.0, 0.0), Point::new(0.25, 0.7), Point::new(1.0, 1.0)];
    let after = render(&src, &SourceInfo::default(), &s, &req, None, CurveInput::Master).histogram;
    let changed = before.luma.iter().zip(&after.luma).filter(|(a, b)| a != b).count();
    assert_eq!(changed, 0, "the alleged input backdrop changed in {changed} luminance bins when only its own master curve moved");
}
