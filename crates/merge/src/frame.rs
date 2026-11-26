//! Merge inputs: scene-linear frames decoded from raw files (camera RGB, before white balance) or
//! standard images (linear light in the file's primaries), with their exposure from EXIF.

use lightcraft_color::Mat3;
use lightcraft_geom::Orientation;
use lightcraft_meta::Metadata;
use lightcraft_raster::Rgb32f;
use lightcraft_raster::resample::{Filter, fit};
use lightcraft_raw::{ColorData, Opcode, RawImage};

use crate::{MergeError, Result};

/// What the RGB values of a frame mean.
#[derive(Clone, Debug, PartialEq)]
pub enum FrameColor {
    /// Camera RGB of a raw file (not white balanced): the DNG colour tags of the source.
    Camera(Box<ColorData>),
    /// Linear RGB with these primaries: linear RGB → XYZ (D50-adapted, ICC PCS).
    Linear { to_xyz_d50: Mat3 },
}

impl FrameColor {
    /// Make `multipliers` (green = 1) the as-shot white balance of camera colour. A DNG says it as `AsShotNeutral`, the
    /// reciprocal of the multipliers. The source's `AsShotWhiteXY` goes: a reader takes it before the neutral and the
    /// DNG spec has the two exclude each other, so a stale one would hide the white balance set here. Linear colour has
    /// no white balance to set, and multipliers that aren't finite and above zero change nothing.
    pub fn set_as_shot_multipliers(&mut self, multipliers: [f32; 3]) {
        if let FrameColor::Camera(c) = self
            && multipliers.iter().all(|v| v.is_finite() && *v > 0.0)
        {
            c.as_shot_neutral = Some(multipliers.map(|v| 1.0 / f64::from(v)));
            c.as_shot_white_xy = None;
        }
    }
}

/// One input photo.
#[derive(Clone, Debug)]
pub struct Frame {
    /// Scene-linear pixels; 1.0 = the sensor/file white level.
    pub image: Rgb32f,
    /// Values at or above this (any channel) are saturated.
    pub clip: f32,
    /// Capture brightness from EXIF, in EV: log2(t · ISO / N²) (higher = brighter frame); `None`
    /// when the file doesn't say.
    pub exposure: Option<f64>,
    pub raw: bool,
    pub color: FrameColor,
    /// How `image` must be rotated/flipped for display (`Normal` once applied).
    pub orientation: Orientation,
    pub metadata: Metadata,
    /// Raw files: the source's `BaselineExposure` (EV), carried into the output.
    pub baseline_exposure: f64,
    /// Raw files: the lens corrections the source applies (`WarpRectilinear`, `WarpRectilinear2`, `FixVignetteRadial`,
    /// in order), re-expressed for `image` (the default crop, not yet turned), for the merged DNG to carry. Empty for
    /// other sources and once [`Frame::orient`] has turned the pixels.
    pub lens: Vec<Opcode>,
    /// Raw files: the as-shot white-balance multipliers from the vendor's maker notes (green = 1), for formats such
    /// as CR2 and NEF that have no `AsShotNeutral` of their own. `None` when the file doesn't say.
    pub wb_multipliers: Option<[f32; 3]>,
}

impl Frame {
    /// A frame from linear pixels in the develop working space (linear Rec.2020), e.g. a rendered
    /// procedural scene. `exposure` as in [`Frame::exposure`].
    pub fn from_linear_rec2020(image: Rgb32f, exposure: Option<f64>) -> Frame {
        let to_xyz_d50 = lightcraft_color::bradford(lightcraft_color::D65, lightcraft_color::D50).mul(&lightcraft_color::REC2020.to_xyz());
        let clip = detect_clip(&image);
        Frame {
            image,
            clip,
            exposure,
            raw: false,
            color: FrameColor::Linear { to_xyz_d50 },
            orientation: Orientation::Normal,
            metadata: Metadata::default(),
            baseline_exposure: 0.0,
            lens: Vec::new(),
            wb_multipliers: None,
        }
    }

    pub fn width(&self) -> usize {
        self.image.width
    }
    pub fn height(&self) -> usize {
        self.image.height
    }

    /// Apply the display orientation to the pixels. The lens corrections, written for the unturned pixels, are dropped.
    pub fn orient(&mut self) {
        if self.orientation != Orientation::Normal {
            self.image = self.image.oriented(self.orientation);
            self.orientation = Orientation::Normal;
            self.lens.clear();
        }
    }
}

/// EV of a capture from its EXIF: log2(t · ISO / N²). ISO defaults to 100 and N to 1 when absent
/// (only ratios between frames matter); `None` without an exposure time.
pub fn exif_exposure(m: &Metadata) -> Option<f64> {
    let t = m.exposure_time.filter(|t| *t > 0.0 && t.is_finite())?;
    let iso = m.iso.filter(|i| *i > 0).unwrap_or(100) as f64;
    let n = m.f_number.filter(|n| *n > 0.0 && n.is_finite()).unwrap_or(1.0);
    Some((t * iso / (n * n)).log2())
}

/// The as-shot white balance `frames` agree on, as multipliers with green = 1: the median R/G and the median B/G of
/// the frames that have usable multipliers (all three finite and above zero), so one frame the camera balanced
/// differently doesn't move it. With an even number of voters the median is the mean of the middle two. Frames without
/// usable multipliers don't vote; `None` when none does.
pub fn consensus_multipliers<'a>(frames: impl IntoIterator<Item = &'a Frame>) -> Option<[f32; 3]> {
    let (mut r_over_g, mut b_over_g) = (Vec::new(), Vec::new());
    for [r, g, b] in frames.into_iter().filter_map(|f| f.wb_multipliers) {
        if [r, g, b].iter().all(|v| v.is_finite() && *v > 0.0) {
            r_over_g.push(f64::from(r) / f64::from(g));
            b_over_g.push(f64::from(b) / f64::from(g));
        }
    }
    Some([median(r_over_g)? as f32, 1.0, median(b_over_g)? as f32])
}

/// The median of `v` (the mean of the middle two when the count is even); `None` when empty.
fn median(mut v: Vec<f64>) -> Option<f64> {
    v.sort_by(f64::total_cmp);
    let upper = *v.get(v.len() / 2)?;
    let lower = *v.get(v.len().checked_sub(1)? / 2)?;
    Some((lower + upper) / 2.0)
}

/// The saturation level of a frame: 1.0, or the (lower) level at which values pile up when the
/// file's white level is optimistic. Returned slightly below the level.
pub fn detect_clip(img: &Rgb32f) -> f32 {
    let maxv = img.data.iter().map(|p| p[0].max(p[1]).max(p[2])).fold(0.0f32, f32::max);
    if maxv.is_nan() || maxv <= 0.0 {
        return 1.0;
    }
    let level = maxv.min(1.0);
    // a pile-up: the top 0.5 % of the range is ≥ 4× denser than the 2 % just below it
    let (mut near, mut below) = (0usize, 0usize);
    for p in &img.data {
        let m = p[0].max(p[1]).max(p[2]);
        if m >= level * 0.995 {
            near += 1;
        } else if m >= level * 0.97 && m < level * 0.99 {
            below += 1;
        }
    }
    let piled = near as f64 > img.data.len() as f64 * 1e-4 && near >= 4 && near as f64 / 0.005 > 4.0 * below as f64 / 0.02;
    if maxv >= 1.0 || piled { level * 0.995 } else { 1.0 }
}

/// The colour tags a raw source starts from when the caller knows better than the file (`None`: the
/// file's own tags). Called with the decoded raw, its samples still there, and the file's bytes.
pub type RawColour<'a> = dyn Fn(&mut lightcraft_raw::RawImage, &[u8]) -> Option<ColorData> + Sync + 'a;

/// Decode a photo (raw or standard format) into a frame no larger than `max_edge` (when given).
/// `orient` applies the EXIF orientation to the pixels.
pub fn load_frame(bytes: &[u8], max_edge: Option<usize>, orient: bool) -> Result<Frame> {
    load_frame_with(bytes, max_edge, orient, &|_, _| None)
}

/// [`load_frame`] with a raw's colour tags from `colour`: a raw without colour matrices of its own
/// (CR2, NEF, ARW…) starts from a colour the app works out (issue #965).
pub fn load_frame_with(bytes: &[u8], max_edge: Option<usize>, orient: bool, colour: &RawColour) -> Result<Frame> {
    let metadata = lightcraft_meta::extract(bytes);
    let exposure = exif_exposure(&metadata);
    if lightcraft_raw::probe(bytes).is_some() {
        let raw = lightcraft_raw::decode(bytes).map_err(|e| MergeError::Decode(e.to_string()))?;
        return raw_frame(raw, bytes, metadata, exposure, max_edge, orient, colour);
    }
    let opts = match max_edge {
        Some(m) => lightcraft_codecs::DecodeOptions::fit(m as u32, m as u32),
        None => lightcraft_codecs::DecodeOptions::default(),
    };
    let d = lightcraft_codecs::decode(bytes, opts).map_err(|e| MergeError::Decode(e.to_string()))?;
    let mut image = d.image;
    if let Some(m) = max_edge
        && image.width.max(image.height) > m
    {
        image = fit(&image, m, m, Filter::Box);
    }
    let clip = detect_clip(&image);
    let mut f = Frame {
        image,
        clip,
        exposure,
        raw: false,
        color: FrameColor::Linear { to_xyz_d50: d.space.to_xyz_d50 },
        orientation: Orientation::from_exif(d.orientation),
        metadata,
        baseline_exposure: 0.0,
        lens: Vec::new(),
        wb_multipliers: None,
    };
    if orient {
        f.orient();
    }
    Ok(f)
}

/// A decoded raw (from `bytes`, which `colour` may read) as a frame: developed to camera RGB, with its colour tags,
/// its as-shot white-balance multipliers and its orientation.
fn raw_frame(
    mut raw: RawImage,
    bytes: &[u8],
    metadata: Metadata,
    exposure: Option<f64>,
    max_edge: Option<usize>,
    orient: bool,
    colour: &RawColour,
) -> Result<Frame> {
    // Lens corrections stay with the develop settings ("profile corrections"): frames are merged uncorrected, and
    // the output DNG carries the reference frame's corrections, re-expressed for the default crop `develop` keeps.
    // The DNG 1.6 skip rule is applied here, so no `WarpRectilinear2` is left optional.
    let (aw, ah) = (raw.active_area.width, raw.active_area.height);
    let crop = raw.develop_crop(aw, ah);
    let lens = lightcraft_raw::opcodes::applied_ops(&raw.opcodes.list3)
        .filter(|op| op.is_lens_correction())
        .filter_map(|op| op.cropped(aw, ah, crop))
        .map(|op| match op {
            Opcode::WarpRectilinear2 { planes, center, reciprocal, .. } => Opcode::WarpRectilinear2 { planes, center, reciprocal, optional: false },
            op => op,
        })
        .collect();
    raw.opcodes.list3.retain(|op| !op.is_lens_correction());
    if let Some(color) = colour(&mut raw, bytes) {
        raw.color = color;
    }
    let small = max_edge.is_some_and(|m| m <= 1200);
    let method = if small { lightcraft_raw::Method::Bilinear } else { lightcraft_raw::Method::Ahd };
    let mut image = raw.develop(method).map_err(|e| MergeError::Decode(e.to_string()))?;
    for p in image.data.iter_mut() {
        *p = p.map(|v| v.max(0.0));
    }
    let clip = detect_clip(&image);
    if let Some(m) = max_edge
        && image.width.max(image.height) > m
    {
        image = fit(&image, m, m, Filter::Box);
    }
    let mut f = Frame {
        image,
        clip,
        exposure,
        raw: true,
        color: FrameColor::Camera(Box::new(raw.color.clone())),
        orientation: raw.orientation,
        metadata: if raw.metadata == Metadata::default() { metadata } else { raw.metadata.clone() },
        baseline_exposure: raw.color.baseline_exposure,
        lens,
        wb_multipliers: raw.wb_multipliers,
    };
    if f.exposure.is_none() {
        f.exposure = exif_exposure(&f.metadata);
    }
    if orient {
        f.orient();
    }
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposure_from_exif() {
        let m = Metadata { exposure_time: Some(1.0 / 125.0), f_number: Some(8.0), iso: Some(100), ..Default::default() };
        let e = exif_exposure(&m).unwrap();
        let m2 = Metadata { exposure_time: Some(4.0 / 125.0), ..m.clone() };
        assert!((exif_exposure(&m2).unwrap() - e - 2.0).abs() < 1e-9);
        assert!(exif_exposure(&Metadata::default()).is_none());
    }

    #[test]
    fn clip_detection() {
        let mut img = Rgb32f::from_fn(100, 100, |x, _| [x as f32 / 100.0; 3]);
        assert_eq!(detect_clip(&img), 1.0);
        for p in img.data.iter_mut().take(500) {
            *p = [0.9, 0.9, 0.9];
        }
        for p in img.data.iter_mut() {
            *p = p.map(|v| v.min(0.9));
        }
        assert!((detect_clip(&img) - 0.9 * 0.995).abs() < 1e-6);
    }

    /// A 16×12 CR2-style mosaic (no colour tags, as the vendor decoders leave them) with as-shot multipliers.
    fn mosaic(wb_multipliers: Option<[f32; 3]>) -> RawImage {
        use lightcraft_raw::{BlackLevel, Cfa, OpcodeLists, RawData, RawFormat, Rect};
        let (w, h) = (16usize, 12usize);
        RawImage {
            format: RawFormat::Cr2,
            width: w,
            height: h,
            cpp: 1,
            data: RawData::U16((0..w * h).map(|i| (100 + (i * 37) % 900) as u16).collect()),
            cfa: Cfa::bayer("RGGB"),
            bits: 12,
            black: BlackLevel::uniform(64.0),
            white: vec![1023.0],
            active_area: Rect::new(0, 0, w, h),
            crop: Rect::new(0, 0, w, h),
            orientation: Orientation::Normal,
            color: ColorData::default(),
            wb_multipliers,
            linearized: false,
            opcodes: OpcodeLists::default(),
            lens_metadata: Default::default(),
            pixel_aspect: 1.0,
            metadata: Metadata::default(),
        }
    }

    fn frame_of(raw: RawImage, colour: &RawColour) -> Frame {
        raw_frame(raw, &[], Metadata::default(), None, None, true, colour).unwrap()
    }

    /// Issue #1144: the as-shot multipliers of a CR2, NEF… (its only white balance) were dropped on loading, so
    /// nothing downstream could tell a merged DNG what the camera had balanced to.
    #[test]
    fn a_raw_frame_carries_the_as_shot_multipliers() {
        let wb = [2.05, 1.0, 1.62];
        let f = frame_of(mosaic(Some(wb)), &|_, _| None);
        assert!(f.raw);
        assert_eq!(f.wb_multipliers, Some(wb));
        // colour tags the caller works out leave them alone
        let own = ColorData { as_shot_neutral: Some([0.5, 1.0, 0.6]), ..Default::default() };
        let f = frame_of(mosaic(Some(wb)), &|_, _| Some(own.clone()));
        assert_eq!(f.wb_multipliers, Some(wb));
        assert_eq!(f.color, FrameColor::Camera(Box::new(own)));
        // a file that gives none gives none
        assert_eq!(frame_of(mosaic(None), &|_, _| None).wb_multipliers, None);
        assert_eq!(Frame::from_linear_rec2020(Rgb32f::new(2, 2), None).wb_multipliers, None);
    }

    fn voter(wb_multipliers: Option<[f32; 3]>) -> Frame {
        Frame { wb_multipliers, ..Frame::from_linear_rec2020(Rgb32f::new(2, 2), None) }
    }

    fn consensus(wb: &[Option<[f32; 3]>]) -> Option<[f32; 3]> {
        let frames: Vec<Frame> = wb.iter().map(|m| voter(*m)).collect();
        consensus_multipliers(&frames)
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6)
    }

    #[test]
    fn the_consensus_is_the_median_of_the_voters() {
        // one voter is its own median
        assert_eq!(consensus(&[Some([2.0, 1.0, 1.6])]), Some([2.0, 1.0, 1.6]));
        // an odd count takes the middle one of each ratio, whichever frame it came from, whatever the order
        let odd = [Some([2.4, 1.0, 1.4]), Some([1.8, 1.0, 1.9]), Some([2.0, 1.0, 1.6])];
        assert_eq!(consensus(&odd), Some([2.0, 1.0, 1.6]));
        // an outlier doesn't pull it
        let outlier = [Some([2.0, 1.0, 1.6]), Some([2.1, 1.0, 1.5]), Some([1.9, 1.0, 1.7]), Some([5.0, 1.0, 0.7]), Some([2.0, 1.0, 1.6])];
        assert_eq!(consensus(&outlier), Some([2.0, 1.0, 1.6]));
        // an even count takes the mean of the middle two
        let even = [Some([1.8, 1.0, 1.9]), Some([2.0, 1.0, 1.6]), Some([2.2, 1.0, 1.5]), Some([2.4, 1.0, 1.4])];
        assert!(close(consensus(&even).unwrap(), [2.1, 1.0, 1.55]), "{:?}", consensus(&even));
        let two = [Some([2.0, 1.0, 1.0]), Some([3.0, 1.0, 2.0])];
        assert!(close(consensus(&two).unwrap(), [2.5, 1.0, 1.5]), "{:?}", consensus(&two));
        // green is the unit even when a file's multipliers aren't normalised to it
        assert!(close(consensus(&[Some([4.0, 2.0, 3.0])]).unwrap(), [2.0, 1.0, 1.5]));
    }

    #[test]
    fn frames_without_usable_multipliers_do_not_vote() {
        assert_eq!(consensus(&[]), None);
        assert_eq!(consensus(&[None, None]), None);
        let bad = |m: [f32; 3]| consensus(&[Some(m)]);
        assert_eq!(bad([0.0, 1.0, 1.5]), None);
        assert_eq!(bad([2.0, 0.0, 1.5]), None);
        assert_eq!(bad([2.0, 1.0, -1.5]), None);
        assert_eq!(bad([f32::NAN, 1.0, 1.5]), None);
        assert_eq!(bad([2.0, f32::INFINITY, 1.5]), None);
        assert_eq!(bad([2.0, 1.0, f32::NEG_INFINITY]), None);
        // mixed: the good ones decide, the missing and the unusable ones are not counted as anything
        let mixed = [None, Some([2.0, 1.0, 1.6]), Some([f32::NAN, 1.0, 1.0]), Some([2.2, 1.0, 1.4]), Some([0.0, 0.0, 0.0])];
        assert!(close(consensus(&mixed).unwrap(), [2.1, 1.0, 1.5]), "{:?}", consensus(&mixed));
        assert_eq!(consensus(&[None, Some([2.0, 1.0, 1.6]), None]), Some([2.0, 1.0, 1.6]));
    }

    #[test]
    fn multipliers_become_the_reciprocal_as_shot_neutral() {
        use lightcraft_color::{D50, Xy};
        let tagged = ColorData { as_shot_neutral: Some([0.9, 1.0, 0.9]), as_shot_white_xy: Some(Xy::new(0.34, 0.35)), ..Default::default() };
        let mut color = FrameColor::Camera(Box::new(tagged.clone()));
        color.set_as_shot_multipliers([2.0, 1.0, 1.6]);
        let FrameColor::Camera(c) = &color else { panic!("still camera colour") };
        assert_eq!(c.as_shot_neutral, Some([0.5, 1.0, 1.0 / f64::from(1.6f32)]));
        assert_eq!(c.as_shot_white_xy, None, "a stale AsShotWhiteXY would be read before the neutral");
        assert_eq!(**c, ColorData { as_shot_neutral: c.as_shot_neutral, as_shot_white_xy: None, ..tagged.clone() }, "nothing else changes");
        // multipliers that aren't usable change nothing
        for bad in [[0.0, 1.0, 1.0], [2.0, f32::NAN, 1.6], [2.0, 1.0, f32::INFINITY], [-2.0, 1.0, 1.6]] {
            let mut color = FrameColor::Camera(Box::new(ColorData { as_shot_white_xy: Some(D50), ..Default::default() }));
            let before = color.clone();
            color.set_as_shot_multipliers(bad);
            assert_eq!(color, before, "{bad:?}");
        }
        // linear colour has no white balance to set
        let mut linear = Frame::from_linear_rec2020(Rgb32f::new(2, 2), None).color;
        let before = linear.clone();
        linear.set_as_shot_multipliers([2.0, 1.0, 1.6]);
        assert_eq!(linear, before);
    }
}
