//! Histograms of display-encoded images (what the Histogram panel shows).
//!
//! Every histogram carries five 256-bin channels of the same pixels: red, green, blue, luminance
//! (`0.2126 R + 0.7152 G + 0.0722 B` of the display-encoded values) and chroma (HSV saturation,
//! `(max − min) / max` of the display-encoded values). Which of them the panel draws, and how, is
//! [`HistogramMode`]; how tall the bars are is [`HistogramScale`].
//!
//! An HDR histogram ([`Histogram::of_hdr`]) keeps the same 256 bins: the first
//! [`Histogram::HDR_FROM`] cover SDR black to SDR white (sRGB-encoded, as an SDR histogram), the
//! rest cover the HDR range above SDR white in equal stops.

use serde::Serialize;

use crate::Rgba8;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Histogram {
    pub r: Vec<u32>,
    pub g: Vec<u32>,
    pub b: Vec<u32>,
    pub luma: Vec<u32>,
    /// HSV saturation of each pixel, `(max − min) / max` of its display-encoded channels, in 256
    /// bins (a neutral pixel is bin 0).
    pub chroma: Vec<u32>,
    /// 2D chrominance grid (`SCOPE_BINS × SCOPE_BINS`, row-major top-to-bottom `+Cr..-Cr` and
    /// left-to-right `-Cb..+Cb`) for the Vectorscope view ([`HistogramMode::Vectorscope`]). A
    /// neutral pixel (`R == G == B`) lands in the middle cell `(SCOPE_MID, SCOPE_MID)`.
    #[serde(skip)]
    pub scope: Vec<u32>,
    pub total: u32,
    /// HDR histograms: the first bin above SDR white, and the stops above SDR white the last bin
    /// reaches. `None` for SDR histograms (every bin is SDR).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hdr: Option<HdrBins>,
}

/// The HDR part of a [`Histogram`]: bins `from..256` cover `0..stops` stops above SDR white.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HdrBins {
    pub from: usize,
    pub stops: f32,
}

impl HdrBins {
    /// The bin of a linear value (1 = SDR white; `NaN` and negatives count as black).
    pub fn bin(self, v: f32) -> usize {
        let last = Histogram::BINS - 1;
        // a split outside 2..=last would leave one side without bins (or underflow)
        let from = self.from.clamp(2, last);
        if v.is_nan() || v <= 0.0 {
            return 0;
        }
        if v <= 1.0 {
            let e = lcc_color::transfer::linear_to_srgb(v);
            return ((e * (from - 1) as f32 + 0.5) as usize).min(from - 1);
        }
        let stops = if self.stops.is_finite() { self.stops.max(1e-3) } else { 1e-3 };
        let t = (v.log2() / stops).clamp(0.0, 1.0);
        (from + (t * (last - from) as f32 + 0.5) as usize).min(last)
    }
}

/// Which histogram channel a mode draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Red,
    Green,
    Blue,
    Luminance,
    Chroma,
}

/// How the Histogram panel draws the photo's distribution (`view.histogram.mode`).
///
/// The variants are the panel's own order: the cycle command steps through them in this order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistogramMode {
    /// Red, green and blue overlaid with additive blending: their pairwise overlaps read yellow,
    /// cyan and magenta and the three together read white, with the luminance as a hairline over
    /// them (the default, as in the target app).
    #[default]
    RgbOverlap,
    /// One monochrome curve of the perceptual luminance.
    Luminance,
    /// The plot split into three stacked tracks, one per channel.
    ChannelsSeparated,
    /// One monochrome curve of the perceptual chroma (saturation).
    Chroma,
    /// Polar chrominance scope (`Cb` horizontal, `Cr` vertical) with primary/secondary targets and
    /// the skin-tone line.
    Vectorscope,
}

impl HistogramMode {
    pub const ALL: [HistogramMode; 5] = [Self::RgbOverlap, Self::Luminance, Self::ChannelsSeparated, Self::Chroma, Self::Vectorscope];

    /// The id the commands, the control channel and the settings file use.
    pub fn name(self) -> &'static str {
        match self {
            Self::RgbOverlap => "rgb",
            Self::Luminance => "luminance",
            Self::ChannelsSeparated => "channels",
            Self::Chroma => "chroma",
            Self::Vectorscope => "vectorscope",
        }
    }

    /// The label shown in the UI (an i18n catalog key).
    pub fn label(self) -> &'static str {
        match self {
            Self::RgbOverlap => "RGB Overlay",
            Self::Luminance => "Luminance",
            Self::ChannelsSeparated => "Separated Channels",
            Self::Chroma => "Chroma",
            Self::Vectorscope => "Vectorscope",
        }
    }

    /// A short label for the panel's own selector, where there is no room for the full one.
    pub fn short(self) -> &'static str {
        match self {
            Self::RgbOverlap => "RGB",
            Self::Luminance => "Luma",
            Self::ChannelsSeparated => "R/G/B",
            Self::Chroma => "Chroma",
            Self::Vectorscope => "Vector",
        }
    }

    /// Whether this mode is a 2D scope rather than a left-to-right tonal curve.
    pub fn is_scope(self) -> bool {
        matches!(self, Self::Vectorscope)
    }

    /// The mode a settings file or command parameter names; `None` for anything else (callers fall
    /// back to the default rather than fail).
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == s)
    }

    /// The next mode, as `view.histogram.mode` steps with no parameter.
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Self::ALL.get((i + 1) % Self::ALL.len()).copied().unwrap_or_default()
    }
}

/// How tall the Histogram panel draws a bin (`view.histogram.scale`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistogramScale {
    /// The bin count against the plot's peak, so a tall spike flattens the rest.
    #[default]
    Linear,
    /// `log10(1 + count)` against the same peak: the quiet parts of the distribution stay visible.
    Logarithmic,
}

impl HistogramScale {
    pub const ALL: [HistogramScale; 2] = [Self::Linear, Self::Logarithmic];

    pub fn name(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Logarithmic => "logarithmic",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Linear => "Linear",
            Self::Logarithmic => "Logarithmic",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == s)
    }

    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Self::ALL.get((i + 1) % Self::ALL.len()).copied().unwrap_or_default()
    }

    /// `count` as a height in 0..1 against `peak` (a robust peak, see [`robust_peak`]).
    ///
    /// A peak that isn't a usable number, or is below one pixel, is one: a bin never draws taller
    /// than the plot, and an empty histogram draws flat rather than dividing by zero.
    pub fn height(self, count: f32, peak: f32) -> f32 {
        let peak = if peak.is_finite() && peak >= 1.0 { peak } else { 1.0 };
        let h = match self {
            Self::Linear => count / peak,
            Self::Logarithmic => {
                let top = (1.0 + peak).log10();
                if top <= 0.0 { 0.0 } else { (1.0 + count).log10() / top }
            }
        };
        if h.is_nan() { 0.0 } else { h.clamp(0.0, 1.0) }
    }
}

/// A robust peak to draw against: the `q` quantile of the bins' heights, so one spike (a large
/// flat area, a channel pinned at an end) does not flatten the rest of the curve. `q` is clamped
/// to 0..1 and an empty or all-`NaN` set gives 1.
pub fn robust_peak(bins: &[f32], q: f32) -> f32 {
    let mut v: Vec<f32> = bins.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return 1.0;
    }
    v.sort_by(f32::total_cmp);
    let q = if q.is_finite() { q.clamp(0.0, 1.0) } else { 1.0 };
    let last = v.len() - 1;
    let i = ((q * last as f32).round() as usize).min(last);
    v.get(i).copied().unwrap_or(1.0).max(1.0)
}

/// Which channels reach one end of the histogram, as a set (bit 0 red, 1 green, 2 blue).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clipped(u8);

impl Clipped {
    /// No channel clips.
    pub const NONE: Clipped = Clipped(0);

    /// From the three channels' `bool`s, in red, green, blue order.
    pub fn of(parts: [bool; 3]) -> Clipped {
        let mut c = Clipped::NONE;
        for (i, p) in parts.into_iter().enumerate() {
            if p {
                c = c.with(i);
            }
        }
        c
    }

    /// This set plus channel `i` (0 red, 1 green, 2 blue); an index outside 0..3 adds nothing.
    pub fn with(self, i: usize) -> Clipped {
        match i {
            0..=2 => Clipped(self.0 | (1 << i)),
            _ => self,
        }
    }

    pub fn any(self) -> bool {
        self.0 != 0
    }

    /// The channels that clip, in red, green, blue order.
    pub fn parts(self) -> [bool; 3] {
        [self.0 & 1 != 0, self.0 & 2 != 0, self.0 & 4 != 0]
    }
}

/// A histogram's clipping at its two ends: the fraction of pixels in the first / last bin of each
/// channel, in red, green, blue order.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Clipping {
    pub shadow: [f32; 3],
    pub highlight: [f32; 3],
}

impl Clipping {
    /// The channels at black, above [`Histogram::CLIP`].
    pub fn shadow_channels(&self) -> Clipped {
        self.shadow.map(|f| f > Histogram::CLIP).into()
    }

    /// The channels at white, above [`Histogram::CLIP`].
    pub fn highlight_channels(&self) -> Clipped {
        self.highlight.map(|f| f > Histogram::CLIP).into()
    }
}

impl From<[bool; 3]> for Clipped {
    fn from(parts: [bool; 3]) -> Clipped {
        Clipped::of(parts)
    }
}

/// Add one to bin `i` of `ch` (its low bits: an index outside the bins lands in one of them rather
/// than panicking).
#[inline]
fn bump(ch: &mut [u32], i: usize) {
    if let Some(c) = ch.get_mut(i & (Histogram::BINS - 1)) {
        *c = c.saturating_add(1);
    }
}

/// Bin `i` of `ch`, or 0 outside the bins.
fn at(ch: &[u32], i: usize) -> u32 {
    ch.get(i).copied().unwrap_or(0)
}

/// `255 / max` in 16.16 fixed point, so a pixel's saturation is one multiply and one shift instead
/// of a division per pixel. `max == 0` (black) has no saturation: scale 0.
fn saturation_scale(max: u8) -> u32 {
    (255u32 << 16).checked_div(max as u32).unwrap_or(0)
}

/// The chroma bin of a display-encoded pixel: HSV saturation `(max − min) / max`, 0..=255. A
/// neutral or black pixel is bin 0.
#[inline]
fn chroma_bin(scale: &[u32; 256], r: u8, g: u8, b: u8) -> usize {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let s = ((max - min) as u32 * scale.get(max as usize).copied().unwrap_or(0)) >> 16;
    (s.min(255)) as usize
}

/// The scale table [`chroma_bin`] indexes.
fn saturation_table() -> [u32; 256] {
    std::array::from_fn(|m| saturation_scale(m as u8))
}

/// The `(u, v)` chrominance coordinates of a display-encoded pixel in `[-1.0, 1.0]`, where
/// `(0, 0)` is neutral (`r == g == b`), `+u` is towards blue (`Cb`) and `+v` is towards red (`Cr`).
///
/// The integer weights sum to zero (`-169 - 331 + 500 == 0` and `500 - 419 - 81 == 0`), so any
/// neutral pixel lands at `(0.0, 0.0)` and every 8-bit colour stays inside the unit disc
/// (`u² + v² < 1`).
pub fn scope_uv(r: u8, g: u8, b: u8) -> (f32, f32) {
    let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
    let cb = -169 * r - 331 * g + 500 * b;
    let cr = 500 * r - 419 * g - 81 * b;
    ((cb as f32 / 140_000.0).clamp(-1.0, 1.0), (cr as f32 / 140_000.0).clamp(-1.0, 1.0))
}

/// The `(bx, by)` cell in `0..Histogram::SCOPE_BINS` a display-encoded pixel lands in: `(32, 32)`
/// is the neutral centre, `bx` increases with `+Cb` (right) and `by` increases with `-Cr` (down,
/// screen order). Computed in integer arithmetic (`32 × 4375 == 140_000`).
#[inline]
pub fn scope_cell(r: u8, g: u8, b: u8) -> (usize, usize) {
    let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
    let cb = -169 * r - 331 * g + 500 * b;
    let cr = 500 * r - 419 * g - 81 * b;
    let step = |v: i32| if v >= 0 { (v + 2187) / 4375 } else { (v - 2187) / 4375 };
    let mid = Histogram::SCOPE_MID as i32;
    let last = (Histogram::SCOPE_BINS - 1) as i32;
    let bx = (mid + step(cb)).clamp(0, last) as usize;
    let by = (mid - step(cr)).clamp(0, last) as usize;
    (bx, by)
}

/// Display-encoded `[r, g, b]` colour of a vectorscope coordinate `(u, v)` in `[-1.0, 1.0]` at a
/// readable mid-high luma: `(0, 0)` is neutral grey (`r == g == b`) and the six primaries /
/// secondaries read in their own hues around the rim. Non-finite coordinates fall back to neutral.
pub fn scope_rgb(u: f32, v: f32) -> [u8; 3] {
    if !u.is_finite() || !v.is_finite() {
        return [166, 166, 166];
    }
    let cb = u.clamp(-1.0, 1.0) * (140_000.0 / 255_000.0);
    let cr = v.clamp(-1.0, 1.0) * (140_000.0 / 255_000.0);
    let y = 0.65f32;
    let r = (y + 1.402 * cr).clamp(0.0, 1.0);
    let g = (y - 0.344_136 * cb - 0.714_136 * cr).clamp(0.0, 1.0);
    let b = (y + 1.772 * cb).clamp(0.0, 1.0);
    let byte = |x: f32| (x * 255.0 + 0.5) as u8;
    [byte(r), byte(g), byte(b)]
}

#[inline]
fn bump_scope(scope: &mut [u32], r: u8, g: u8, b: u8) {
    let (bx, by) = scope_cell(r, g, b);
    if let Some(c) = scope.get_mut(by * Histogram::SCOPE_BINS + bx) {
        *c = c.saturating_add(1);
    }
}

impl Histogram {
    pub const BINS: usize = 256;
    /// Side length of the square vectorscope grid (odd so [`Self::SCOPE_MID`] is the exact neutral
    /// origin `(0, 0)`).
    pub const SCOPE_BINS: usize = 65;
    /// Neutral centre index (`32`) along each axis of the vectorscope grid.
    pub const SCOPE_MID: usize = Self::SCOPE_BINS / 2;
    /// Total cells in [`Self::scope`] (`65 × 65 = 4,225`).
    pub const SCOPE_CELLS: usize = Self::SCOPE_BINS * Self::SCOPE_BINS;
    /// HDR histograms: the first bin above SDR white (SDR takes the left half).
    pub const HDR_FROM: usize = Self::BINS / 2;
    /// Fraction of a channel's pixels at one end from which the panel calls that channel clipped
    /// (about 1900 pixels of a 625k-sample histogram).
    pub const CLIP: f32 = 0.003;

    fn empty(hdr: Option<HdrBins>) -> Histogram {
        let bins = || vec![0; Self::BINS];
        Histogram { r: bins(), g: bins(), b: bins(), luma: bins(), chroma: bins(), scope: vec![0; Self::SCOPE_CELLS], total: 0, hdr }
    }

    /// Count in cell `(bx, by)` of the vectorscope grid, or 0 outside `0..SCOPE_BINS`.
    pub fn scope_at(&self, bx: usize, by: usize) -> u32 {
        if bx < Self::SCOPE_BINS && by < Self::SCOPE_BINS { self.scope.get(by * Self::SCOPE_BINS + bx).copied().unwrap_or(0) } else { 0 }
    }

    /// Histogram of linear RGB samples (1 = SDR white) reaching `stops` stops above SDR white.
    /// `samples` are visited at most ~1 MP of them (evenly strided).
    pub fn of_hdr(samples: &[[f32; 3]], stops: f32) -> Histogram {
        let bins = HdrBins { from: Self::HDR_FROM, stops };
        let mut h = Self::empty(Some(bins));
        let scale = saturation_table();
        let step = (samples.len() / 1_000_000).max(1);
        for c in samples.iter().step_by(step) {
            let (r, g, b) = (c[0], c[1], c[2]);
            // the same display encoding the SDR histogram bins by, so chroma and the vectorscope
            // mean one thing
            let e = lcc_color::transfer::encode_srgb8;
            let (er, eg, eb) = (e(r), e(g), e(b));
            bump(&mut h.r, bins.bin(r));
            bump(&mut h.g, bins.bin(g));
            bump(&mut h.b, bins.bin(b));
            bump(&mut h.luma, bins.bin(0.2126 * r + 0.7152 * g + 0.0722 * b));
            bump(&mut h.chroma, chroma_bin(&scale, er, eg, eb));
            bump_scope(&mut h.scope, er, eg, eb);
            h.total = h.total.saturating_add(1);
        }
        h
    }

    /// HDR histogram of per-pixel bins already computed with `bins` (`[r, g, b, luma]` each),
    /// visiting at most ~1 MP of them. Chroma and the vectorscope are binned from the channels' own
    /// bin indices, the only colour this caller still has.
    pub fn of_bins(px: impl ExactSizeIterator<Item = [u8; 4]>, bins: HdrBins) -> Histogram {
        let mut h = Self::empty(Some(bins));
        let scale = saturation_table();
        let step = (px.len() / 1_000_000).max(1);
        for [r, g, b, l] in px.step_by(step) {
            bump(&mut h.r, r as usize);
            bump(&mut h.g, g as usize);
            bump(&mut h.b, b as usize);
            bump(&mut h.luma, l as usize);
            bump(&mut h.chroma, chroma_bin(&scale, r, g, b));
            bump_scope(&mut h.scope, r, g, b);
            h.total = h.total.saturating_add(1);
        }
        h
    }

    /// The histogram as flat words (to cross a worker boundary): r, g, b, luma, chroma (256 each),
    /// the total, the first HDR bin (0: SDR) and the HDR stops as `f32` bits, then the vectorscope
    /// grid (`SCOPE_CELLS` words).
    pub fn to_words(&self) -> Vec<u32> {
        let mut w = Vec::with_capacity(5 * Self::BINS + 3 + Self::SCOPE_CELLS);
        for ch in [&self.r, &self.g, &self.b, &self.luma, &self.chroma] {
            w.extend(ch.iter().copied().chain(std::iter::repeat(0)).take(Self::BINS));
        }
        w.push(self.total);
        let (from, stops) = self.hdr.map_or((0, 0.0), |b| (b.from as u32, b.stops));
        w.extend([from, stops.to_bits()]);
        w.extend(self.scope.iter().copied().chain(std::iter::repeat(0)).take(Self::SCOPE_CELLS));
        w
    }

    /// [`Histogram::to_words`] back; also takes the older layouts without the vectorscope grid, the
    /// chroma channel or the HDR words. `None` for anything malformed.
    pub fn from_words(w: &[u32]) -> Option<Histogram> {
        let n = Self::BINS;
        let sc = Self::SCOPE_CELLS;
        // 4 channels (before chroma existed) or 5, with or without the two HDR words and the
        // vectorscope tail
        let (channels, has_scope) = match w.len() {
            l if l == 4 * n + 1 || l == 4 * n + 3 => (4, false),
            l if l == 5 * n + 1 || l == 5 * n + 3 => (5, false),
            l if l == 5 * n + 3 + sc => (5, true),
            _ => return None,
        };
        let ch = |k: usize| w.get(k * n..(k + 1) * n).map(<[u32]>::to_vec);
        let chroma = if channels == 5 { ch(4)? } else { vec![0; n] };
        let hdr = match (w.get(channels * n + 1), w.get(channels * n + 2)) {
            (Some(&from), Some(&bits)) if from > 1 && (from as usize) < n => {
                let stops = f32::from_bits(bits);
                Some(HdrBins { from: from as usize, stops: if stops.is_finite() && stops > 0.0 { stops } else { 1.0 } })
            }
            _ => None,
        };
        let scope = if has_scope { w.get(5 * n + 3..5 * n + 3 + sc)?.to_vec() } else { vec![0; sc] };
        Some(Histogram { r: ch(0)?, g: ch(1)?, b: ch(2)?, luma: ch(3)?, chroma, scope, total: *w.get(channels * n)?, hdr })
    }

    /// The five bin sets, by [`Channel`].
    pub fn channel(&self, c: Channel) -> &[u32] {
        match c {
            Channel::Red => &self.r,
            Channel::Green => &self.g,
            Channel::Blue => &self.b,
            Channel::Luminance => &self.luma,
            Channel::Chroma => &self.chroma,
        }
    }

    /// Fraction of pixels above SDR white in any channel (0 for SDR histograms).
    pub fn above_sdr(&self) -> f32 {
        let Some(b) = self.hdr else { return 0.0 };
        if self.total == 0 {
            return 0.0;
        }
        let over = |v: &[u32]| v.iter().skip(b.from).sum::<u32>();
        over(&self.r).max(over(&self.g)).max(over(&self.b)) as f32 / self.total as f32
    }

    pub fn of_srgb8(img: &Rgba8) -> Histogram {
        let mut h = Self::empty(None);
        let scale = saturation_table();
        // Sample at most ~1 MP for speed.
        let step = ((img.len() as f64 / 1_000_000.0).sqrt().ceil() as usize).max(1);
        for y in (0..img.height).step_by(step) {
            for x in (0..img.width).step_by(step) {
                let p = img.get(x, y);
                bump(&mut h.r, p[0] as usize);
                bump(&mut h.g, p[1] as usize);
                bump(&mut h.b, p[2] as usize);
                let l = (p[0] as u32 * 54 + p[1] as u32 * 183 + p[2] as u32 * 19) >> 8;
                bump(&mut h.luma, (l.min(255)) as usize);
                bump(&mut h.chroma, chroma_bin(&scale, p[0], p[1], p[2]));
                bump_scope(&mut h.scope, p[0], p[1], p[2]);
                h.total = h.total.saturating_add(1);
            }
        }
        h
    }

    /// Fraction of pixels clipped at black / white in any channel (approximate: per channel max).
    pub fn clipping(&self) -> (f32, f32) {
        let c = self.clipping_channels();
        (c.shadow.iter().copied().fold(0.0, f32::max), c.highlight.iter().copied().fold(0.0, f32::max))
    }

    /// Clipping per channel: which of red, green and blue reach the first / last bin, and how much
    /// of the image does. The panel colours its clipping triangles from this.
    pub fn clipping_channels(&self) -> Clipping {
        if self.total == 0 {
            return Clipping::default();
        }
        let t = self.total as f32;
        let end = |at_bin: usize| [&self.r, &self.g, &self.b].map(|ch| at(ch, at_bin) as f32 / t);
        Clipping { shadow: end(0), highlight: end(Self::BINS - 1) }
    }

    /// Mean luma in 0..1.
    pub fn mean_luma(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        self.luma.iter().enumerate().map(|(i, &c)| i as f32 * c as f32).sum::<f32>() / (255.0 * self.total as f32)
    }

    /// Luma value (0..1) below which `q` of the pixels fall.
    pub fn percentile(&self, q: f32) -> f32 {
        let target = (q.clamp(0.0, 1.0) * self.total as f32) as u32;
        let mut acc = 0u32;
        for (i, &c) in self.luma.iter().enumerate() {
            acc = acc.saturating_add(c);
            if acc >= target {
                return i as f32 / 255.0;
            }
        }
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_stats() {
        let img = Rgba8::from_fn(10, 10, |x, _| if x < 5 { [0, 0, 0, 255] } else { [255, 255, 255, 255] });
        let h = Histogram::of_srgb8(&img);
        assert_eq!(h.total, 100);
        assert_eq!(h.r[0], 50);
        assert_eq!(h.luma[255], 50);
        let (lo, hi) = h.clipping();
        assert!((lo - 0.5).abs() < 1e-6 && (hi - 0.5).abs() < 1e-6);
        assert!((h.mean_luma() - 0.5).abs() < 1e-3);
        assert_eq!(h.percentile(0.25), 0.0);
        assert_eq!(h.percentile(0.75), 1.0);
        assert_eq!(h.above_sdr(), 0.0);
    }

    /// The chroma channel bins HSV saturation of the display-encoded pixels: neutrals in bin 0,
    /// a saturated primary at the top, and a partly saturated colour in between.
    #[test]
    fn chroma_bins_the_saturation() {
        let px = [[10, 10, 10, 255], [255, 0, 0, 255], [255, 0, 255, 255], [128, 64, 64, 255]];
        let img = Rgba8::from_fn(4, 1, |x, _| px[x]);
        let h = Histogram::of_srgb8(&img);
        assert_eq!(h.chroma.len(), Histogram::BINS);
        assert_eq!(h.total, 4);
        assert_eq!(h.chroma[0], 1, "the neutral pixel");
        assert_eq!(h.chroma[255], 2, "both fully saturated pixels");
        assert!(h.chroma[127] + h.chroma[128] == 1, "128/255 of the way up: {:?}", h.chroma.iter().position(|&c| c > 0));
    }

    /// A black pixel has no saturation (no division by zero) and a neutral ramp stays in bin 0.
    #[test]
    fn a_neutral_or_black_pixel_is_chroma_bin_zero() {
        let img = Rgba8::from_fn(256, 1, |x, _| [x as u8, x as u8, x as u8, 255]);
        let h = Histogram::of_srgb8(&img);
        assert_eq!(h.chroma[0], 256, "{:?}", h.chroma.iter().position(|&c| c > 0));
        assert_eq!(Histogram::of_srgb8(&Rgba8::from_fn(4, 4, |_, _| [0, 0, 0, 255])).chroma[0], 16);
    }

    /// The vectorscope places every neutral pixel in the middle cell, places opposite primaries /
    /// secondaries symmetrically across the centre inside the unit disc, and colours each cell by
    /// its own chrominance.
    #[test]
    fn vectorscope_places_neutrals_at_the_centre_and_primaries_around_the_disc() {
        let mid = Histogram::SCOPE_MID;
        let ramp = Rgba8::from_fn(256, 1, |x, _| [x as u8, x as u8, x as u8, 255]);
        let h = Histogram::of_srgb8(&ramp);
        assert_eq!(h.scope_at(mid, mid), 256, "every neutral pixel lands in the exact centre");
        assert_eq!(h.scope.iter().copied().sum::<u32>(), 256);
        assert_eq!(h.scope_at(Histogram::SCOPE_BINS, 0), 0, "outside the grid is 0");
        assert_eq!(scope_uv(128, 128, 128), (0.0, 0.0));
        assert_eq!(scope_rgb(0.0, 0.0), [166, 166, 166], "the centre cell is neutral grey");
        assert_eq!(scope_rgb(f32::NAN, f32::INFINITY), [166, 166, 166]);

        // Opposite colour pairs sit on opposite sides of `(mid, mid)` and stay inside the circle.
        for ([r0, g0, b0], [r1, g1, b1]) in [([255, 0, 0], [0, 255, 255]), ([0, 255, 0], [255, 0, 255]), ([0, 0, 255], [255, 255, 0])] {
            let (x0, y0) = scope_cell(r0, g0, b0);
            let (x1, y1) = scope_cell(r1, g1, b1);
            assert_eq!((x0 + x1, y0 + y1), (2 * mid, 2 * mid), "antipodes across the centre");
            for (x, y) in [(x0, y0), (x1, y1)] {
                let (dx, dy) = (x as isize - mid as isize, y as isize - mid as isize);
                assert!(dx * dx + dy * dy > 16 * 16 && dx * dx + dy * dy <= (mid * mid) as isize, "({x}, {y})");
            }
        }
        // Red is upper-left (-Cb, +Cr -> bx < mid, by < mid) and Blue is to the right (+Cb -> bx > mid).
        let (rx, ry) = scope_cell(255, 0, 0);
        let (bx, _) = scope_cell(0, 0, 255);
        assert!(rx < mid && ry < mid && bx > mid, "red=({rx},{ry}), blue x={bx}");
        let red_rgb = scope_rgb(scope_uv(255, 0, 0).0, scope_uv(255, 0, 0).1);
        let blue_rgb = scope_rgb(scope_uv(0, 0, 255).0, scope_uv(0, 0, 255).1);
        assert!(red_rgb[0] > red_rgb[1] + 60 && red_rgb[0] > red_rgb[2] + 60, "{red_rgb:?}");
        assert!(blue_rgb[2] > blue_rgb[0] + 60 && blue_rgb[2] > blue_rgb[1] + 60, "{blue_rgb:?}");
    }

    /// `height` normalizes either way, never draws above the plot, and hostile input draws flat.
    #[test]
    fn the_scale_normalizes_and_stays_in_range() {
        let linear = HistogramScale::Linear;
        let log = HistogramScale::Logarithmic;
        assert!((linear.height(500.0, 1000.0) - 0.5).abs() < 1e-6);
        assert_eq!(linear.height(5000.0, 1000.0), 1.0, "a spike is clamped to the plot");
        assert_eq!(linear.height(0.0, 1000.0), 0.0);
        assert!((log.height(0.0, 1000.0)).abs() < 1e-6, "log of one is zero");
        assert_eq!(log.height(1000.0, 1000.0), 1.0);
        // the log scale lifts the quiet end of the distribution well above the linear one
        assert!(log.height(10.0, 1000.0) > linear.height(10.0, 1000.0) + 0.2);
        assert!(log.height(10.0, 1000.0) < log.height(100.0, 1000.0));
        for peak in [0.0, -5.0, f32::NAN, f32::INFINITY, 0.5] {
            for count in [0.0, 1.0, f32::NAN, f32::INFINITY, 1e30] {
                for s in HistogramScale::ALL {
                    let h = s.height(count, peak);
                    assert!((0.0..=1.0).contains(&h) && h.is_finite(), "{s:?} {count} {peak} → {h}");
                }
            }
        }
        assert_eq!(linear.height(1e30, f32::NAN), 1.0, "an unusable peak is one: everything is at the top");
        assert_eq!(log.height(0.0, 0.0), 0.0, "and an empty histogram is flat");
    }

    /// The peak is a quantile of the bins, so one spike does not flatten the rest.
    #[test]
    fn the_robust_peak_ignores_a_spike() {
        let mut bins = vec![10.0; 256];
        bins[3] = 100_000.0;
        assert_eq!(robust_peak(&bins, 0.99), 10.0, "one bin in 256 does not move the 99th percentile");
        assert_eq!(robust_peak(&bins, 1.0), 100_000.0, "the maximum is still available");
        // a stack of equal bins takes its own value, and unusable input falls back to 1
        assert_eq!(robust_peak(&[7.0; 8], 0.99), 7.0);
        assert_eq!(robust_peak(&[], 0.99), 1.0);
        assert_eq!(robust_peak(&[f32::NAN, f32::NAN], 0.99), 1.0);
        assert_eq!(robust_peak(&[0.0, 0.0], 0.99), 1.0, "a peak below one pixel is one");
        assert_eq!(robust_peak(&[1.0, 2.0, 3.0, 4.0], f32::NAN), 4.0, "an unusable quantile is the top");
        assert_eq!(robust_peak(&[1.0, 2.0, 3.0, 4.0], -1.0), 1.0);
    }

    /// The clipping channels are per channel, above the panel's threshold, and a set reads back as
    /// its own parts.
    #[test]
    fn clipping_names_the_channels_that_reach_the_ends() {
        // 1000 pixels: red and green at black, blue clipped at white
        let img = Rgba8::from_fn(100, 10, |x, _| if x < 5 { [0, 0, 128, 255] } else { [40, 40, 255, 255] });
        let h = Histogram::of_srgb8(&img);
        let c = h.clipping_channels();
        assert_eq!(c.shadow, [0.05, 0.05, 0.0]);
        assert_eq!(c.highlight, [0.0, 0.0, 0.95]);
        assert_eq!(c.shadow_channels(), Clipped::of([true, true, false]));
        assert_eq!(c.highlight_channels(), Clipped::of([false, false, true]));
        let (lo, hi) = h.clipping();
        assert!((lo - 0.05).abs() < 1e-6 && (hi - 0.95).abs() < 1e-6, "the per-channel maximum, as before");
        // a histogram with no pixels clips nowhere and never divides by zero
        let empty = Histogram::of_srgb8(&Rgba8::from_fn(1, 1, |_, _| [0, 0, 0, 255]));
        assert_eq!(empty.clipping_channels(), Clipping { shadow: [1.0; 3], highlight: [0.0; 3] });
        assert!(!Histogram::from_words(&[1, 2, 3]).is_some(), "malformed words stay rejected");
        let channels = Clipped::NONE.with(2).with(9);
        assert_eq!(channels.parts(), [false, false, true]);
        assert!(channels.any() && !Clipped::NONE.any());
    }

    /// The modes and scales round-trip through their names and cycle in their own order; anything
    /// else reads as the default.
    #[test]
    fn modes_and_scales_round_trip_through_their_names() {
        assert_eq!(HistogramMode::default(), HistogramMode::RgbOverlap);
        assert_eq!(HistogramScale::default(), HistogramScale::Linear);
        assert_eq!(HistogramMode::ALL.len(), 5);
        assert_eq!(HistogramScale::ALL.len(), 2);
        for m in HistogramMode::ALL {
            assert_eq!(HistogramMode::parse(m.name()), Some(m));
            assert!(!m.label().is_empty());
            assert_eq!(m.is_scope(), m == HistogramMode::Vectorscope);
        }
        for s in HistogramScale::ALL {
            assert_eq!(HistogramScale::parse(s.name()), Some(s));
            assert!(!s.label().is_empty());
        }
        assert_eq!(HistogramMode::RgbOverlap.next(), HistogramMode::Luminance);
        assert_eq!(HistogramMode::Chroma.next(), HistogramMode::Vectorscope);
        assert_eq!(HistogramMode::Vectorscope.next(), HistogramMode::RgbOverlap, "the cycle wraps");
        assert_eq!(HistogramScale::Logarithmic.next(), HistogramScale::Linear);
        assert_eq!(HistogramMode::parse("nope"), None);
        assert_eq!(HistogramScale::parse(""), None);
        assert_eq!(HistogramMode::parse("LUMINANCE"), None, "names are exact, not case-folded");
    }

    /// Every mode's channel is the bins the panel should draw.
    #[test]
    fn a_mode_names_its_own_bins() {
        let img = Rgba8::from_fn(8, 8, |x, _| [(x * 30) as u8, 9, 200, 255]);
        let h = Histogram::of_srgb8(&img);
        assert_eq!(h.channel(Channel::Red), h.r.as_slice());
        assert_eq!(h.channel(Channel::Green), h.g.as_slice());
        assert_eq!(h.channel(Channel::Blue), h.b.as_slice());
        assert_eq!(h.channel(Channel::Luminance), h.luma.as_slice());
        assert_eq!(h.channel(Channel::Chroma), h.chroma.as_slice());
        for c in [Channel::Red, Channel::Green, Channel::Blue, Channel::Luminance, Channel::Chroma] {
            assert_eq!(h.channel(c).len(), Histogram::BINS);
        }
    }

    #[test]
    fn hdr_bins_split_sdr_and_stops() {
        let px = [[0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [4.0, 4.0, 4.0], [16.0, 16.0, 16.0], [f32::NAN, -1.0, f32::INFINITY]];
        let h = Histogram::of_hdr(&px, 4.0);
        let b = h.hdr.unwrap();
        assert_eq!(h.total, 5);
        assert_eq!(b.bin(1.0), b.from - 1, "SDR white is the last SDR bin");
        assert_eq!(b.bin(16.0), 255);
        assert_eq!(b.bin(1e9), 255);
        let two = b.bin(4.0);
        assert!(two > b.from && two < 255, "{two}");
        assert_eq!(h.r[0], 2, "black and NaN");
        assert_eq!(h.b[255], 2, "16 and +inf");
        assert!((h.above_sdr() - 0.6).abs() < 1e-6, "{}", h.above_sdr());
        assert_eq!(Histogram::of_hdr(&[], 4.0).above_sdr(), 0.0);
        // the HDR chroma is binned from the same display encoding, so the neutral samples land in
        // bin 0 and the one with a colourless NaN/negative pair plus +inf lands at the top
        assert_eq!(h.chroma[0], 4, "the four neutral samples (black, 1, 4 and 16)");
        assert_eq!(h.chroma[255], 1, "+inf blue against a black red and green");
        assert_eq!(h.scope_at(Histogram::SCOPE_MID, Histogram::SCOPE_MID), 4, "the four neutral HDR samples sit at the vectorscope centre");
    }

    #[test]
    fn words_round_trip_and_reject_garbage() {
        let img = Rgba8::from_fn(8, 8, |x, y| [(x * 30) as u8, (y * 30) as u8, 9, 255]);
        let sdr = Histogram::of_srgb8(&img);
        assert_eq!(sdr.to_words().len(), 5 * Histogram::BINS + 3 + Histogram::SCOPE_CELLS);
        assert_eq!(Histogram::from_words(&sdr.to_words()), Some(sdr.clone()));
        let hdr = Histogram::of_hdr(&[[0.5, 2.0, 9.0]], 4.0);
        assert_eq!(Histogram::from_words(&hdr.to_words()), Some(hdr));
        // the older layouts (four or five channels without the vectorscope tail) still read
        let four = 4 * Histogram::BINS;
        let mut legacy: Vec<u32> = sdr.to_words()[..four].to_vec();
        legacy.push(sdr.total);
        assert_eq!(
            Histogram::from_words(&legacy),
            Some(Histogram { chroma: vec![0; Histogram::BINS], scope: vec![0; Histogram::SCOPE_CELLS], ..sdr.clone() }),
        );
        let five_no_scope = &sdr.to_words()[..5 * Histogram::BINS + 3];
        assert_eq!(Histogram::from_words(five_no_scope), Some(Histogram { scope: vec![0; Histogram::SCOPE_CELLS], ..sdr.clone() }),);
        assert_eq!(Histogram::from_words(&[1, 2, 3]), None);
        assert_eq!(Histogram::from_words(&legacy[..four]), None, "a truncated histogram is nothing");
        assert_eq!(Histogram::from_words(&sdr.to_words()[..four + 2]), None, "a half-written HDR tail is nothing");
        let mut bad = sdr.to_words();
        bad[5 * Histogram::BINS + 1] = 9999; // first HDR bin out of range: SDR
        bad[5 * Histogram::BINS + 2] = f32::NAN.to_bits();
        assert_eq!(Histogram::from_words(&bad).unwrap().hdr, None);
        bad[5 * Histogram::BINS + 1] = 100; // NaN stops fall back to something drawable
        assert_eq!(Histogram::from_words(&bad).unwrap().hdr.unwrap().stops, 1.0);
    }
    /// A hand-built split outside the bins (any `from`, any `stops`) never panics and stays in range.
    #[test]
    fn hdr_bins_with_any_split_stay_in_range() {
        for from in [0, 1, 2, 128, 255, 256, usize::MAX] {
            for stops in [0.0, 3.0, f32::NAN, f32::INFINITY] {
                let b = HdrBins { from, stops };
                for v in [f32::NAN, -1.0, 0.0, 0.5, 1.0, 2.0, 1e9] {
                    assert!(b.bin(v) < Histogram::BINS, "{from} {stops} {v}");
                }
            }
        }
    }

    /// Measuring a real preview, to keep the chroma and vectorscope channels honest: run
    /// `cargo test -p lcc-raster --release -- --ignored --nocapture histogram_cost`.
    /// A 1.7 MP image samples 0.43 MP (step 2): ~1.6 ms for the four older channels, ~2.7 ms with
    /// chroma and the vectorscope, so a 2.5 MP preview stays well inside the 16 ms slider-draft
    /// budget.
    #[test]
    #[ignore = "timing probe"]
    fn histogram_cost() {
        let img = Rgba8::from_fn(1600, 1067, |x, y| [((x * 7) % 256) as u8, ((y * 11) % 256) as u8, ((x + y) % 256) as u8, 255]);
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let n = 20;
            for _ in 0..n {
                std::hint::black_box(Histogram::of_srgb8(&img));
            }
            println!("Histogram::of_srgb8 1.7 MP: {:.2} ms", t.elapsed().as_secs_f64() * 1e3 / f64::from(n));
        }
    }
}
