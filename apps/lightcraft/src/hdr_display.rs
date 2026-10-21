//! HDR display output (LR-VIEW-HDR-DISPLAY): whether the window can show the range above SDR
//! white, and with which surface format. Pure decisions, unit-tested here; the window always
//! falls back to SDR when any condition is missing, so an HDR edit is never shown wrong.
//!
//! HDR needs three things: a platform path (macOS EDR via an extended-range float Metal layer,
//! Windows HDR via scRGB float or HDR10 swap chains, Wayland via the colour-management protocol),
//! a surface format that carries values above 1.0 (`Rgba16Float`) or a 10-bit PQ signal
//! (`Rgb10a2Unorm`), and a renderer that actually configured that format. eframe 0.36 picks the
//! surface format itself (8-bit only, `egui_wgpu::preferred_framebuffer_format`), so today the
//! choice reports SDR with the reason; the moment the renderer's target is an HDR format the same
//! decision turns HDR on.

use eframe::wgpu::TextureFormat;

/// Windowing platform, as far as HDR output is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Windows,
    Wayland,
    X11,
    Web,
    Other,
}

impl Platform {
    /// The platform this build runs on (`wayland` from the session environment on Linux / BSD).
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_arch = "wasm32") {
            Self::Web
        } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            Self::Wayland
        } else if std::env::var_os("DISPLAY").is_some() {
            Self::X11
        } else {
            Self::Other
        }
    }

    /// Whether the platform has an HDR compositing path wgpu can reach at all.
    fn has_hdr_path(self) -> bool {
        matches!(self, Self::MacOs | Self::Windows | Self::Wayland)
    }
}

/// How HDR values reach the display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HdrEncoding {
    /// Linear float, SDR white at 1.0 and the headroom above it (macOS EDR, Windows scRGB,
    /// Wayland extended linear).
    ExtendedLinear,
    /// 10-bit BT.2100 PQ (Windows HDR10, Wayland PQ).
    Pq10,
}

/// The display output decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayOutput {
    Sdr { reason: &'static str },
    Hdr { format: TextureFormat, encoding: HdrEncoding },
}

impl DisplayOutput {
    #[cfg_attr(not(test), allow(dead_code))] // read by the HDR view once the surface can be HDR
    pub fn is_hdr(self) -> bool {
        matches!(self, Self::Hdr { .. })
    }
}

/// The encoding a surface format carries on `platform`, if it is an HDR one.
fn encoding_of(format: TextureFormat, platform: Platform) -> Option<HdrEncoding> {
    match format {
        TextureFormat::Rgba16Float => Some(HdrEncoding::ExtendedLinear),
        // macOS takes HDR through EDR floats only
        TextureFormat::Rgb10a2Unorm if platform != Platform::MacOs => Some(HdrEncoding::Pq10),
        _ => None,
    }
}

/// The best HDR surface format among those a surface offers (`available`), for `platform`:
/// float first (keeps SDR white at 1.0 and needs no PQ encode), then 10-bit PQ. `None` = SDR only.
/// (For configuring the surface once eframe lets the app pick its format.)
#[cfg_attr(not(test), allow(dead_code))]
pub fn preferred_hdr_format(available: &[TextureFormat], platform: Platform) -> Option<TextureFormat> {
    if !platform.has_hdr_path() {
        return None;
    }
    [TextureFormat::Rgba16Float, TextureFormat::Rgb10a2Unorm].into_iter().find(|f| available.contains(f) && encoding_of(*f, platform).is_some())
}

/// The decision for the window: `target` is the format the renderer configured (`None`: the glow
/// renderer or no wgpu state), `display_headroom` the display's current peak over SDR white as the
/// OS reports it (`None`: unknown, `Some(1.0)`: an SDR display or HDR switched off in the OS),
/// `disabled` the user's opt-out (`LIGHTCRAFT_HDR_DISPLAY=0`).
pub fn choose(target: Option<TextureFormat>, platform: Platform, display_headroom: Option<f32>, disabled: bool) -> DisplayOutput {
    if disabled {
        return DisplayOutput::Sdr { reason: "HDR display is switched off" };
    }
    if !platform.has_hdr_path() {
        return DisplayOutput::Sdr { reason: "this windowing system has no HDR output" };
    }
    let Some(format) = target else {
        return DisplayOutput::Sdr { reason: "the window renderer is not wgpu" };
    };
    let Some(encoding) = encoding_of(format, platform) else {
        return DisplayOutput::Sdr { reason: "the window surface is 8-bit" };
    };
    match display_headroom {
        Some(h) if h.is_finite() && h > 1.0 => DisplayOutput::Hdr { format, encoding },
        Some(_) => DisplayOutput::Sdr { reason: "the display is in SDR mode" },
        None => DisplayOutput::Sdr { reason: "the display's HDR headroom is unknown" },
    }
}

/// The decision for this start, logged once (the window renderer's `target` format).
pub fn report(target: Option<TextureFormat>) -> DisplayOutput {
    let disabled = std::env::var("LIGHTCRAFT_HDR_DISPLAY").ok().is_some_and(|v| matches!(v.trim(), "0" | "off" | "false" | "no"));
    // no OS headroom query is wired yet: unknown, so SDR unless everything else is HDR-ready
    let out = choose(target, Platform::current(), None, disabled);
    match out {
        DisplayOutput::Sdr { reason } => log::info!("display: SDR ({reason})"),
        DisplayOutput::Hdr { format, encoding } => log::info!("display: HDR {encoding:?} ({format:?})"),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use TextureFormat as F;

    #[test]
    fn eight_bit_surfaces_are_sdr() {
        for p in [Platform::MacOs, Platform::Windows, Platform::Wayland] {
            for f in [F::Bgra8Unorm, F::Bgra8UnormSrgb, F::Rgba8Unorm] {
                assert!(!choose(Some(f), p, Some(4.0), false).is_hdr(), "{p:?} {f:?}");
            }
        }
    }

    #[test]
    fn hdr_needs_platform_format_and_headroom() {
        let hdr = DisplayOutput::Hdr { format: F::Rgba16Float, encoding: HdrEncoding::ExtendedLinear };
        assert_eq!(choose(Some(F::Rgba16Float), Platform::MacOs, Some(2.0), false), hdr);
        assert_eq!(
            choose(Some(F::Rgb10a2Unorm), Platform::Windows, Some(4.0), false),
            DisplayOutput::Hdr { format: F::Rgb10a2Unorm, encoding: HdrEncoding::Pq10 }
        );
        // any missing condition falls back to SDR
        assert!(!choose(Some(F::Rgba16Float), Platform::X11, Some(4.0), false).is_hdr());
        assert!(!choose(Some(F::Rgba16Float), Platform::Web, Some(4.0), false).is_hdr());
        assert!(!choose(Some(F::Rgba16Float), Platform::MacOs, Some(1.0), false).is_hdr());
        assert!(!choose(Some(F::Rgba16Float), Platform::MacOs, None, false).is_hdr());
        assert!(!choose(Some(F::Rgba16Float), Platform::MacOs, Some(f32::NAN), false).is_hdr());
        assert!(!choose(Some(F::Rgba16Float), Platform::MacOs, Some(2.0), true).is_hdr());
        assert!(!choose(None, Platform::Windows, Some(4.0), false).is_hdr());
        // macOS has no PQ swap chain path
        assert!(!choose(Some(F::Rgb10a2Unorm), Platform::MacOs, Some(4.0), false).is_hdr());
    }

    #[test]
    fn preferred_format_picks_float_then_pq() {
        let all = [F::Bgra8Unorm, F::Rgb10a2Unorm, F::Rgba16Float];
        assert_eq!(preferred_hdr_format(&all, Platform::Windows), Some(F::Rgba16Float));
        assert_eq!(preferred_hdr_format(&[F::Bgra8Unorm, F::Rgb10a2Unorm], Platform::Wayland), Some(F::Rgb10a2Unorm));
        assert_eq!(preferred_hdr_format(&[F::Bgra8Unorm, F::Rgb10a2Unorm], Platform::MacOs), None);
        assert_eq!(preferred_hdr_format(&all, Platform::X11), None);
        assert_eq!(preferred_hdr_format(&[], Platform::MacOs), None);
    }
}
