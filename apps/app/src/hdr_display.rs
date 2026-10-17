//! HDR display output (LR-VIEW-HDR-DISPLAY): whether the window shows the range above SDR white,
//! and how. The decisions are pure and unit-tested here; the window falls back to SDR whenever a
//! condition is missing, so an HDR edit is never shown wrong.
//!
//! HDR needs a platform path (macOS EDR, Windows HDR / scRGB, Wayland colour management), a
//! window surface in a format and colour space that carry values above 1.0 (`Rgba16Float` in
//! extended linear sRGB: scRGB on Windows, EDR on macOS) and a display with headroom above SDR
//! white right now. The window's renderer is the vendored egui-wgpu (`vendor/egui-wgpu`), which
//! configures that surface when `WgpuConfiguration::hdr_output` asks and the surface offers it,
//! re-queries the display's HDR facts (`wgpu::DisplayHdrInfo`) on every surface configure, and
//! scales egui's drawing to SDR white ([`Levels::scale`]): the UI chrome stays at SDR white, only
//! the loupe's HDR texture ([`WindowHdr::upload`]) goes above it, up to the display's headroom.

use eframe::egui_wgpu::RenderState;
use eframe::wgpu;
use eframe::wgpu::{DisplayHdrInfo, SurfaceColorSpace as ColorSpace, TextureFormat};

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
/// `disabled` the user's opt-out (`<PREFIX>_HDR_DISPLAY=0`).
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

/// The user's opt-out: `<PREFIX>_HDR_DISPLAY=0` (or `off`, `false`, `no`).
pub fn disabled() -> bool {
    lcc_brand::env("HDR_DISPLAY").is_some_and(|v| matches!(v.trim(), "0" | "off" | "false" | "no"))
}

/// Whether to ask the window renderer for an extended-range surface at all.
pub fn wanted() -> bool {
    !disabled() && Platform::current().has_hdr_path()
}

/// The headroom the Wayland compositor reports for its best output (`wayland_hdr`), read once
/// at start-up ([`read_compositor_headroom`]). `None`: not Wayland, no colour management, or
/// every output is SDR right now.
static COMPOSITOR_HEADROOM: std::sync::OnceLock<Option<f32>> = std::sync::OnceLock::new();

/// HDR metadata for the window's swapchain (`lcc_hdrmeta`), kept in step with the levels.
static METADATA: std::sync::Mutex<Option<lcc_hdrmeta::HdrMetadata>> = std::sync::Mutex::new(None);

/// Ask the Wayland compositor once for its outputs' headroom (before the window opens; also
/// decides native Wayland over XWayland). Elsewhere, or with HDR switched off: `None`.
pub fn read_compositor_headroom() -> Option<f32> {
    *COMPOSITOR_HEADROOM.get_or_init(|| {
        if disabled() || std::env::var_os("WAYLAND_DISPLAY").is_none() {
            return None;
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            match crate::wayland_hdr::read() {
                Ok(outputs) => {
                    log::info!("display: Wayland outputs {outputs:?}");
                    crate::wayland_hdr::best_headroom(&outputs)
                }
                Err(e) => {
                    log::info!("display: no Wayland output luminances ({e})");
                    None
                }
            }
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        {
            None
        }
    })
}

/// The window renderer's HDR settings: an extended-range surface where offered and, on
/// Linux / BSD, a Vulkan device that can carry HDR metadata, set on every new swapchain.
pub fn configure(options: &mut eframe::egui_wgpu::WgpuConfiguration) {
    options.hdr_output = wanted();
    if !options.hdr_output || !cfg!(all(unix, not(target_os = "macos"))) {
        return;
    }
    options.open_device = Some(std::sync::Arc::new(lcc_hdrmeta::open_device));
    options.on_surface_configured = Some(std::sync::Arc::new(|device, surface| {
        let meta = *METADATA.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(m) = meta {
            match lcc_hdrmeta::set_metadata(device, surface, &m) {
                Ok(true) => log::debug!("display: HDR metadata {m:?}"),
                Ok(false) => {}
                Err(e) => log::warn!("display: HDR metadata: {e}"),
            }
        }
    }));
    set_metadata_for(levels(Platform::current(), None, env_f32("HDR_HEADROOM").or(read_compositor_headroom()), env_f32("HDR_PAPER_WHITE")));
}

/// Keep the swapchain metadata in step with `l` (taken at the next surface configure).
fn set_metadata_for(l: Levels) {
    let m = l.headroom.and_then(|h| lcc_hdrmeta::metadata_for(h, l.scale));
    *METADATA.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = m;
}

/// A positive finite number from `<PREFIX>_<name>`.
fn env_f32(name: &str) -> Option<f32> {
    lcc_brand::env(name).and_then(|v| v.trim().parse::<f32>().ok()).filter(|v| v.is_finite() && *v > 0.0)
}

/// scRGB's 1.0, in cd/m² (IEC 61966-2-2): what Windows (and Wayland's scRGB) shows for 1.0.
pub const SCRGB_UNIT_NITS: f32 = 80.0;

/// What the window draws with, from the display's HDR facts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Levels {
    /// The display's peak over SDR white right now (`None`: unknown).
    pub headroom: Option<f32>,
    /// SDR white in the surface's units (the "paper white" egui's drawing is scaled to): 1.0 on
    /// macOS EDR and whenever the display is SDR, `sdr_white_nits / 80` on Windows scRGB.
    pub scale: f32,
}

/// [`Levels`] for `platform` from the display's `info` (`None`: not queried), with the user's
/// overrides `<PREFIX>_HDR_HEADROOM` (× SDR white) and `<PREFIX>_HDR_PAPER_WHITE` (cd/m²) for
/// platforms that don't report them (Wayland).
pub fn levels(platform: Platform, info: Option<&DisplayHdrInfo>, headroom_override: Option<f32>, paper_white_override: Option<f32>) -> Levels {
    let positive = |v: &f32| v.is_finite() && *v > 0.0;
    let headroom = headroom_override.or_else(|| info.and_then(DisplayHdrInfo::tone_map_headroom)).filter(positive);
    let sdr = headroom.is_none_or(|h| h <= 1.0);
    // macOS EDR: 1.0 is SDR white by definition; an SDR display shows 1.0 as its white
    let scale = if platform == Platform::MacOs || sdr {
        1.0
    } else {
        let windows_white = (platform == Platform::Windows).then(|| info.and_then(|i| i.luminance).and_then(|l| l.sdr_white_nits)).flatten();
        paper_white_override.or(windows_white).filter(positive).map_or(1.0, |nits| (nits / SCRGB_UNIT_NITS).clamp(0.25, 16.0))
    };
    Levels { headroom, scale }
}

/// sRGB's encoding curve, continued above 1 (egui's shader decodes it the same way).
fn srgb_encode(v: f32) -> f32 {
    if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

/// One linear-light pixel (SDR white = 1) for the loupe's HDR texture: negatives and non-numbers
/// to 0, brighter than `headroom` scaled down to it keeping its hue, then sRGB-encoded (egui's
/// shader decodes it and scales by [`Levels::scale`]).
pub fn encode_pixel(rgb: [f32; 3], headroom: f32) -> [f32; 4] {
    let h = if headroom.is_finite() { headroom.max(1.0) } else { 1.0 };
    let c = rgb.map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 });
    let m = c[0].max(c[1]).max(c[2]);
    let k = if m > h { h / m } else { 1.0 };
    let [r, g, b] = c.map(|v| srgb_encode(v * k));
    [r, g, b, 1.0]
}

/// The largest HDR texture side accepted (the loupe's whole-frame render is far smaller).
const MAX_SIDE: usize = 16_384;

/// `rgb` (3 linear floats per pixel, `width × height`) as `Rgba16Float` texels (the bits of each half float).
pub fn encode_rgba16f(rgb: &[f32], width: usize, height: usize, headroom: f32) -> Result<Vec<u16>, String> {
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(format!("HDR texture size {width}×{height} is out of range"));
    }
    let n = width.checked_mul(height).ok_or("HDR texture too large")?;
    let px = rgb.as_chunks::<3>().0;
    let px = px.get(..n).ok_or("HDR render has fewer pixels than its size")?;
    Ok(px.iter().flat_map(|p| encode_pixel(*p, headroom).map(|v| half::f16::from_f32(v).to_bits())).collect())
}

/// The decision for this start, logged once with the window surface's format, colour space,
/// what the surface offered and the display's HDR facts.
pub fn report(state: Option<&RenderState>) -> DisplayOutput {
    let Some(state) = state else {
        let out = choose(None, Platform::current(), None, disabled());
        log::info!("display: {out:?}");
        return out;
    };
    let offered: Vec<String> = state.surface_formats.iter().map(|f| format!("{:?} {:?}", f.format, f.color_spaces)).collect();
    log::info!("display: surface offers [{}]", offered.join(", "));
    let info = state.display_hdr_info.read().clone();
    log::info!("display: HDR facts {info:?}");
    let l = levels(Platform::current(), info.as_ref(), env_f32("HDR_HEADROOM").or_else(read_compositor_headroom), env_f32("HDR_PAPER_WHITE"));
    let target = (state.target_color_space == ColorSpace::ExtendedSrgbLinear || state.target_format != TextureFormat::Rgba16Float)
        .then_some(state.target_format);
    let out = choose(target, Platform::current(), l.headroom, disabled());
    let sdr_at_start = Platform::current() == Platform::Windows && !disabled() && !eframe::egui_wgpu::hdr_surface_wanted(info.as_ref(), true);
    match out {
        DisplayOutput::Sdr { .. } if sdr_at_start => log::info!(
            "display: SDR (the display was not in HDR mode at start-up, headroom {:?}; turning Windows HDR on takes effect at the next start; surface {:?} {:?})",
            info.as_ref().and_then(DisplayHdrInfo::tone_map_headroom),
            state.target_format,
            state.target_color_space
        ),
        DisplayOutput::Sdr { reason } => log::info!("display: SDR ({reason}; surface {:?} {:?})", state.target_format, state.target_color_space),
        DisplayOutput::Hdr { format, encoding } => {
            log::info!("display: HDR {encoding:?} ({format:?}), headroom {:?}, SDR white ×{}", l.headroom, l.scale)
        }
    }
    out
}

/// The loupe's HDR output in the desktop window ([`lcc_ui_egui::HdrOutput`]): the display's
/// headroom (re-read every frame, as the window moves or the OS switches HDR / brightness),
/// egui's paper-white scale, and the loupe's float texture.
pub struct WindowHdr {
    state: RenderState,
    extended: bool,
    headroom_override: Option<f32>,
    paper_white_override: Option<f32>,
    levels: Option<Levels>,
    upload_headroom: Option<f32>,
    texture: Option<(wgpu::Texture, egui::TextureId, [usize; 2])>,
}

impl WindowHdr {
    /// `None` unless the window surface is extended-range float (otherwise the loupe stays SDR).
    pub fn new(state: &RenderState) -> Option<WindowHdr> {
        let w = Self::any(state);
        w.extended.then_some(w)
    }

    /// Also for an 8-bit surface (the probe shows what an SDR window makes of the ramp).
    pub fn any(state: &RenderState) -> WindowHdr {
        WindowHdr {
            state: state.clone(),
            extended: state.target_format == TextureFormat::Rgba16Float && state.target_color_space == ColorSpace::ExtendedSrgbLinear,
            headroom_override: env_f32("HDR_HEADROOM").or_else(read_compositor_headroom),
            paper_white_override: env_f32("HDR_PAPER_WHITE"),
            levels: None,
            upload_headroom: None,
            texture: None,
        }
    }

    /// The levels last applied ([`lcc_ui_egui::HdrOutput::headroom`]).
    pub fn levels(&self) -> Option<Levels> {
        self.levels
    }

    /// Upload with this headroom instead of the display's (the probe's ramp: unclipped).
    pub fn set_upload_headroom(&mut self, headroom: f32) {
        self.upload_headroom = Some(headroom);
    }
}

impl lcc_ui_egui::HdrOutput for WindowHdr {
    fn headroom(&mut self) -> Option<f32> {
        let info = self.state.display_hdr_info.read().clone();
        let l = levels(Platform::current(), info.as_ref(), self.headroom_override, self.paper_white_override);
        if self.levels != Some(l) {
            log::info!("display: headroom {:?}, SDR white ×{}", l.headroom, l.scale);
            self.state.renderer.write().set_output_scale(l.scale);
            set_metadata_for(l);
            self.levels = Some(l);
        }
        let out = choose(self.extended.then_some(self.state.target_format), Platform::current(), l.headroom, disabled());
        if out.is_hdr() { l.headroom } else { None }
    }

    fn upload(&mut self, width: usize, height: usize, rgb: &[f32]) -> Result<egui::TextureId, String> {
        let headroom = self.upload_headroom.or_else(|| self.levels.and_then(|l| l.headroom)).unwrap_or(1.0);
        let texels = encode_rgba16f(rgb, width, height, headroom)?;
        let (device, queue) = (&self.state.device, &self.state.queue);
        let size = [width, height];
        let (w, h) = (u32::try_from(width).map_err(|e| e.to_string())?, u32::try_from(height).map_err(|e| e.to_string())?);
        if self.texture.as_ref().is_none_or(|(_, _, s)| *s != size) {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("loupe HDR"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            let mut r = self.state.renderer.write();
            let id = match self.texture.take() {
                Some((_, id, _)) => {
                    r.update_egui_texture_from_wgpu_texture(device, &view, wgpu::FilterMode::Linear, id);
                    id
                }
                None => r.register_native_texture(device, &view, wgpu::FilterMode::Linear),
            };
            self.texture = Some((tex, id, size));
        }
        let Some((tex, id, _)) = self.texture.as_ref() else { return Err("no HDR texture".into()) };
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            bytemuck::cast_slice(&texels),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w.saturating_mul(8)), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        Ok(*id)
    }
}

impl Drop for WindowHdr {
    fn drop(&mut self) {
        if let Some((_, id, _)) = self.texture.take() {
            self.state.renderer.write().free_texture(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TextureFormat as F;

    #[test]
    fn windows_uses_the_scrgb_surface_only_when_the_display_is_in_hdr_mode() {
        use eframe::egui_wgpu::hdr_surface_wanted as wanted;
        use eframe::wgpu::{DisplayCoarseRange, DisplayLuminance};
        let sdr = DisplayHdrInfo {
            coarse: Some(DisplayCoarseRange { high_dynamic_range: Some(false), ..Default::default() }),
            luminance: Some(DisplayLuminance { max_nits: Some(270.0), sdr_white_nits: Some(80.0), ..Default::default() }),
            ..Default::default()
        };
        let hdr = DisplayHdrInfo {
            coarse: Some(DisplayCoarseRange { high_dynamic_range: Some(true), ..Default::default() }),
            luminance: Some(DisplayLuminance { max_nits: Some(1000.0), sdr_white_nits: Some(200.0), ..Default::default() }),
            ..Default::default()
        };
        assert!(!wanted(Some(&sdr), true), "an SDR Windows display gets the SDR surface");
        assert!(wanted(Some(&hdr), true), "Windows HDR on gets scRGB");
        assert!(!wanted(Some(&DisplayHdrInfo::default()), true), "unknown facts: SDR on Windows");
        assert!(!wanted(None, true), "no facts: SDR on Windows");
        // macOS EDR / Wayland: unchanged, the surface's offer decides.
        assert!(wanted(None, false) && wanted(Some(&sdr), false));
    }

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

    use eframe::egui_wgpu::hdr_surface_format;
    use eframe::wgpu::{DisplayCoarseRange, DisplayHeadroom, DisplayLuminance, SurfaceColorSpaces as Cs, SurfaceFormatCapabilities};

    fn offer(format: F, color_spaces: Cs) -> SurfaceFormatCapabilities {
        SurfaceFormatCapabilities { format, color_spaces }
    }

    #[test]
    fn the_surface_is_extended_float_only_where_it_is_offered() {
        // a typical SDR surface: 8-bit sRGB, and float only in sRGB (no extended range)
        let sdr = [offer(F::Bgra8Unorm, Cs::SRGB), offer(F::Bgra8UnormSrgb, Cs::SRGB), offer(F::Rgba16Float, Cs::SRGB)];
        assert_eq!(hdr_surface_format(&sdr), None);
        assert_eq!(hdr_surface_format(&[]), None);
        // macOS EDR / Windows scRGB: Rgba16Float in extended linear sRGB
        let hdr = [offer(F::Bgra8Unorm, Cs::SRGB), offer(F::Rgba16Float, Cs::SRGB | Cs::EXTENDED_SRGB_LINEAR), offer(F::Rgb10a2Unorm, Cs::BT2100_PQ)];
        assert_eq!(hdr_surface_format(&hdr), Some((F::Rgba16Float, ColorSpace::ExtendedSrgbLinear)));
        // PQ alone is not taken (the UI would need a PQ encode)
        assert_eq!(hdr_surface_format(&[offer(F::Rgb10a2Unorm, Cs::BT2100_PQ)]), None);
    }

    fn windows(max: f32, white: f32, hdr: bool) -> DisplayHdrInfo {
        DisplayHdrInfo {
            luminance: Some(DisplayLuminance { max_nits: Some(max), sdr_white_nits: Some(white), ..Default::default() }),
            coarse: Some(DisplayCoarseRange { high_dynamic_range: Some(hdr), gamut: None }),
            ..Default::default()
        }
    }

    #[test]
    fn paper_white_keeps_the_ui_at_sdr_white() {
        // Windows HDR: scRGB 1.0 is 80 nits, the user's SDR white 240 → the UI is drawn at 3.0
        let l = levels(Platform::Windows, Some(&windows(1000.0, 240.0, true)), None, None);
        assert!((l.scale - 3.0).abs() < 1e-6);
        assert!((l.headroom.unwrap_or(0.0) - 1000.0 / 240.0).abs() < 1e-4);
        // macOS EDR: 1.0 is SDR white, whatever else is known
        let edr = DisplayHdrInfo { headroom: Some(DisplayHeadroom { current: Some(2.5), ..Default::default() }), ..Default::default() };
        assert_eq!(levels(Platform::MacOs, Some(&edr), None, Some(300.0)), Levels { headroom: Some(2.5), scale: 1.0 });
        // the user's overrides where nothing is reported (Wayland)
        assert_eq!(levels(Platform::Wayland, None, Some(4.0), Some(160.0)), Levels { headroom: Some(4.0), scale: 2.0 });
        assert_eq!(levels(Platform::Wayland, None, None, None), Levels { headroom: None, scale: 1.0 });
        // nonsense never reaches the shader
        let bad = levels(Platform::Windows, Some(&windows(f32::NAN, f32::INFINITY, true)), Some(-1.0), Some(f32::NAN));
        assert!(bad.scale.is_finite() && bad.scale > 0.0);
    }

    #[test]
    fn an_sdr_display_is_drawn_exactly_as_before() {
        // Windows with HDR off still reports nits: no headroom, no scaling, the loupe stays 8-bit
        let l = levels(Platform::Windows, Some(&windows(400.0, 200.0, false)), None, None);
        assert_eq!(l, Levels { headroom: Some(1.0), scale: 1.0 });
        assert!(!choose(Some(F::Rgba16Float), Platform::Windows, l.headroom, false).is_hdr());
        // and SDR values come back as egui's own sRGB encoding (within f16 precision)
        for v in [0.0f32, 0.002, 0.18, 0.5, 1.0] {
            let [r, g, b, a] = encode_pixel([v; 3], 1.0);
            let srgb = if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
            assert!((r - srgb).abs() < 1e-6 && r == g && g == b && a == 1.0, "{v}");
        }
    }

    #[test]
    fn highlights_go_above_sdr_white_up_to_the_headroom_keeping_their_hue() {
        // 2× SDR white with 4× headroom: encoded above 1, decodes back to 2
        let [r, ..] = encode_pixel([2.0; 3], 4.0);
        assert!(r > 1.0);
        let decoded = ((r + 0.055) / 1.055).powf(2.4);
        assert!((decoded - 2.0).abs() < 1e-4);
        // past the headroom: scaled down to it, channel ratios kept
        let [r, g, ..] = encode_pixel([8.0, 4.0, 0.0], 2.0);
        let dec = |v: f32| ((v + 0.055) / 1.055).powf(2.4);
        assert!((dec(r) - 2.0).abs() < 1e-4 && (dec(g) - 1.0).abs() < 1e-4);
        // hostile values: no NaN, no negatives
        let p = encode_pixel([f32::NAN, -3.0, f32::INFINITY], f32::NAN);
        assert!(p.iter().all(|v| v.is_finite() && *v >= 0.0));
    }

    #[test]
    fn texture_encoding_rejects_bad_sizes() {
        assert!(encode_rgba16f(&[0.5; 12], 2, 2, 4.0).is_ok_and(|t| t.len() == 16));
        assert!(encode_rgba16f(&[0.5; 9], 2, 2, 4.0).is_err(), "short buffer");
        assert!(encode_rgba16f(&[], 0, 0, 4.0).is_err());
        assert!(encode_rgba16f(&[], usize::MAX, 2, 4.0).is_err());
    }
}
