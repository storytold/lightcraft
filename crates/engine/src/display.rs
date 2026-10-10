//! The monitor profile (`app.displayProfile`): previews shown on a wide-gamut or calibrated
//! display through its ICC profile.
//!
//! The frontend holds the profile in use (Settings ▸ Display, the UI command `app.displayProfile`).
//! With a profile, the loupe renders into the display's own primaries
//! ([`RenderJob::with_display`](crate::media::RenderJob::with_display)) and everything else made
//! for sRGB (thumbnails, cached previews, embedded camera JPEGs) is converted to it
//! ([`present`]). Histograms and the preview caches stay sRGB, and exports never use it.

pub use dac_codecs::display::DisplayKind;
use dac_codecs::display::{DisplayProfile, MAX_PROFILE_BYTES};
use dac_pipeline::DisplaySpace;
use dac_raster::{Histogram, Rgba8};
use serde_json::{Value, json};
use std::sync::Arc;

use crate::media::RenderResult;

/// A loaded display profile: its transforms and the pipeline target.
#[derive(Debug)]
pub struct Display {
    /// The file it came from.
    pub path: String,
    pub profile: DisplayProfile,
    pub space: DisplaySpace,
}

impl Display {
    /// Load the ICC profile at `path`.
    pub fn load(path: &str) -> Result<Display, String> {
        let path = path.trim();
        let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
        if meta.len() > MAX_PROFILE_BYTES as u64 {
            return Err(format!("{path}: too large for an ICC profile"));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        Display::from_bytes(path, &bytes)
    }

    pub fn from_bytes(path: &str, bytes: &[u8]) -> Result<Display, String> {
        let profile = DisplayProfile::from_icc(bytes).map_err(|e| format!("{path}: {e}"))?;
        let space = DisplaySpace::new(profile.to_rec2020, profile.id).ok_or_else(|| format!("{path}: the profile's primaries are degenerate"))?;
        Ok(Display { path: path.to_string(), profile, space })
    }

    pub fn id(&self) -> u64 {
        self.profile.id
    }

    /// Load the profile at `path` (`""`: none, the display is treated as sRGB).
    pub fn load_opt(path: &str) -> Result<Option<Arc<Display>>, String> {
        if path.trim().is_empty() { Ok(None) } else { Display::load(path).map(|d| Some(Arc::new(d))) }
    }

    /// For `app.displayProfile` and Settings: path, description, kind, primaries (xy).
    pub fn describe(&self) -> Value {
        let to_xyz = dac_color::REC2020.to_xyz().mul(&self.profile.to_rec2020).0;
        let xy = |c: usize| {
            let (x, y, z) = (to_xyz[0][c], to_xyz[1][c], to_xyz[2][c]);
            let s = x + y + z;
            if s.abs() < 1e-12 { json!([0.0, 0.0]) } else { json!([round4(x / s), round4(y / s)]) }
        };
        json!({
            "path": self.path,
            "description": self.profile.description,
            "kind": self.profile.kind.label(),
            "primaries": {"red": xy(0), "green": xy(1), "blue": xy(2)},
        })
    }
}

fn round4(v: f64) -> f64 {
    (v * 1e4).round() / 1e4
}

/// Make `r` ready for `display`: an sRGB image (thumbnail, cached preview, camera JPEG) is
/// converted to the display's values. A render made for the display (see
/// [`RenderJob::with_display`](crate::media::RenderJob::with_display)) is already.
pub fn present(r: &mut RenderResult, display: &Display) {
    if r.display.is_some() {
        return;
    }
    if let Ok(rendered) = &mut r.rendered {
        match display.profile.from_srgb(&mut rendered.image) {
            Ok(()) => r.display = Some(display.id()),
            Err(e) => log::warn!("display profile: {e}"),
        }
    }
}

/// A render into `display`'s primaries (sRGB curve) → in place, the display's device values; and
/// its sRGB histogram, plus (`full_srgb`: for the cached view preview) its whole sRGB version.
/// The histogram samples the same pixels [`Histogram::of_srgb8`] would, so only those are
/// converted when the sRGB image itself isn't needed (slider drags).
pub(crate) fn finish_render(image: &mut Rgba8, display: &Display, full_srgb: bool) -> (Option<Rgba8>, Option<Histogram>) {
    let warn = |e: String| log::warn!("display profile: {e}");
    let (srgb, histogram) = if full_srgb {
        let srgb = display.profile.to_srgb(image).map_err(warn).ok();
        let h = srgb.as_ref().map(Histogram::of_srgb8);
        (srgb, h)
    } else {
        let sample = histogram_sample(image);
        (None, display.profile.to_srgb(&sample).map_err(warn).ok().map(|s| Histogram::of_srgb8(&s)))
    };
    if let Err(e) = display.profile.correct(image) {
        warn(e);
    }
    (srgb, histogram)
}

/// The pixels [`Histogram::of_srgb8`] reads (at most ~1 MP, a regular grid), as an image of
/// their own (which it then reads whole).
fn histogram_sample(img: &Rgba8) -> Rgba8 {
    let step = ((img.len() as f64 / 1_000_000.0).sqrt().ceil() as usize).max(1);
    if step == 1 {
        return img.clone();
    }
    let (w, h) = (img.width.div_ceil(step), img.height.div_ceil(step));
    let mut data = Vec::with_capacity(w * h);
    for y in (0..img.height).step_by(step) {
        for x in (0..img.width).step_by(step) {
            data.push(img.get(x, y));
        }
    }
    Rgba8 { width: w, height: h, data }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p3() -> Display {
        Display::from_bytes("p3.icc", &dac_codecs::icc::write_named(dac_codecs::NamedSpace::DisplayP3)).unwrap()
    }

    fn picture(w: usize, h: usize) -> Rgba8 {
        Rgba8 { width: w, height: h, data: (0..w * h).map(|i| [(i * 7 % 256) as u8, (i * 13 % 256) as u8, (i * 29 % 256) as u8, 255]).collect() }
    }

    /// The sampled histogram of a drag render is exactly the histogram of the whole sRGB image.
    #[test]
    fn sampled_histogram_matches_the_full_one() {
        let d = p3();
        for (w, h) in [(40, 30), (1500, 1100), (2561, 1709)] {
            let img = picture(w, h);
            let (mut a, mut b) = (img.clone(), img.clone());
            let (srgb, full) = finish_render(&mut a, &d, true);
            let (none, sampled) = finish_render(&mut b, &d, false);
            assert!(srgb.is_some() && none.is_none());
            assert_eq!(full.unwrap(), sampled.unwrap(), "{w}x{h}");
            assert_eq!(a, b);
        }
    }

    #[test]
    fn load_reports_and_rejects() {
        let dir = std::env::temp_dir().join(format!("lightcraft-display-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("p3.icc");
        std::fs::write(&good, dac_codecs::icc::write_named(dac_codecs::NamedSpace::DisplayP3)).unwrap();
        let bad = dir.join("bad.icc");
        std::fs::write(&bad, b"not a profile").unwrap();

        let d = Display::load_opt(&good.to_string_lossy()).unwrap().unwrap();
        let r = d.describe();
        assert_eq!(r["kind"], "matrix");
        assert_eq!(r["description"], "Display P3");
        let red = r["primaries"]["red"].as_array().unwrap();
        assert!((red[0].as_f64().unwrap() - 0.68).abs() < 2e-3 && (red[1].as_f64().unwrap() - 0.32).abs() < 2e-3, "{r}");
        assert!(Display::load_opt("  ").unwrap().is_none());
        let e = Display::load(&bad.to_string_lossy()).unwrap_err();
        assert!(e.contains("not a valid ICC profile"), "{e}");
        assert!(Display::load(&dir.join("missing.icc").to_string_lossy()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
