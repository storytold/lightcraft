//! Declare a window's HDR brightness range to the compositor (Vulkan `VK_EXT_hdr_metadata`).
//!
//! On Wayland, Mesa tags an extended-linear (scRGB) Vulkan swapchain with the colour-management
//! protocol but, without HDR metadata, gives it the protocol's default luminances (maximum =
//! reference white = 80 cd/m²): the content asks for no headroom, so KWin's EDR neither raises
//! the backlight nor keeps values above 1.0. `vkSetHdrMetadataEXT` makes Mesa add the mastering
//! luminance and max-CLL to the surface's image description, which KWin honours (verified on
//! KWin 6.7.5 / Mesa 26.2.4 RADV).
//!
//! wgpu 30 can neither enable the extension nor set the metadata, so this crate goes through
//! wgpu-hal: [`open_device`] creates the device with the extension, [`set_metadata`] sets the
//! metadata on a configured surface's swapchain. Together with `crates/sysmem` it is the
//! workspace's only code allowed `unsafe` (the `vulkan` module). The API is safe, and on other
//! backends and platforms, without the extension or without a swapchain, it does nothing.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

/// scRGB / extended-linear 1.0, in cd/m²: the reference white of an extended-linear Wayland
/// surface without luminances (wp_color_management_v1 defaults) and of Windows scRGB.
pub const LINEAR_UNIT_NITS: f32 = 80.0;

/// HDR metadata for an sRGB-primaries extended-linear surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HdrMetadata {
    /// Mastering display peak, cd/m².
    pub max_nits: f32,
    /// Mastering display black, cd/m².
    pub min_nits: f32,
    /// Maximum content light level, cd/m².
    pub max_cll: f32,
    /// Maximum frame-average light level, cd/m².
    pub max_fall: f32,
}

/// The metadata for content whose SDR white is `paper_white` surface units (1.0 = 80 cd/m²) and
/// whose highlights reach `headroom` × SDR white: peak = `headroom × paper_white × 80` cd/m².
/// `None` for no headroom (`≤ 1`) or nonsense values.
pub fn metadata_for(headroom: f32, paper_white: f32) -> Option<HdrMetadata> {
    if !(headroom.is_finite() && paper_white.is_finite() && headroom > 1.0 && paper_white > 0.0) {
        return None;
    }
    let peak = (headroom * paper_white * LINEAR_UNIT_NITS).min(10_000.0);
    Some(HdrMetadata { max_nits: peak, min_nits: 0.01, max_cll: peak, max_fall: peak })
}

/// Create the device for `adapter` as `wgpu::Adapter::request_device` would, with
/// `VK_EXT_hdr_metadata` enabled. `None` when that doesn't apply (not Vulkan, the extension
/// isn't there, another platform): the caller then creates the device itself.
pub fn open_device(adapter: &wgpu::Adapter, desc: &wgpu::DeviceDescriptor<'_>) -> Option<Result<(wgpu::Device, wgpu::Queue), String>> {
    #[cfg(all(unix, not(target_vendor = "apple"), not(target_arch = "wasm32")))]
    {
        vulkan::open_device(adapter, desc)
    }
    #[cfg(not(all(unix, not(target_vendor = "apple"), not(target_arch = "wasm32"))))]
    {
        let _ = (adapter, desc);
        None
    }
}

/// Set `meta` on `surface`'s current swapchain (call after every `configure`: a new swapchain
/// starts without it). `Ok(false)`: nothing to do here (not Vulkan, the device lacks the
/// extension, no swapchain, another platform).
pub fn set_metadata(device: &wgpu::Device, surface: &wgpu::Surface<'_>, meta: &HdrMetadata) -> Result<bool, String> {
    if !meta.max_nits.is_finite() || meta.max_nits <= 0.0 {
        return Err(format!("HDR metadata peak {} is not a luminance", meta.max_nits));
    }
    #[cfg(all(unix, not(target_vendor = "apple"), not(target_arch = "wasm32")))]
    {
        Ok(vulkan::set_metadata(device, surface, meta))
    }
    #[cfg(not(all(unix, not(target_vendor = "apple"), not(target_arch = "wasm32"))))]
    {
        let _ = (device, surface);
        Ok(false)
    }
}

#[cfg(all(unix, not(target_vendor = "apple"), not(target_arch = "wasm32")))]
#[allow(unsafe_code)]
mod vulkan {
    use super::HdrMetadata;
    use ash::vk;

    pub fn open_device(adapter: &wgpu::Adapter, desc: &wgpu::DeviceDescriptor<'_>) -> Option<Result<(wgpu::Device, wgpu::Queue), String>> {
        // SAFETY: `as_hal` borrows the adapter's Vulkan object (or returns None for another
        // backend); we only read its capabilities and open a device from it, while `adapter`
        // stays alive for the whole block, as the wgpu docs require.
        let open = unsafe {
            let hal = adapter.as_hal::<wgpu_hal::vulkan::Api>()?;
            if !hal.physical_device_capabilities().supports_extension(ash::ext::hdr_metadata::NAME) {
                return None;
            }
            hal.open_with_callback(
                desc.required_features,
                &desc.required_limits,
                &desc.memory_hints,
                Some(Box::new(|args: wgpu_hal::vulkan::CreateDeviceCallbackArgs| {
                    // only adds an extension the adapter reports, as the callback contract allows
                    args.extensions.push(ash::ext::hdr_metadata::NAME);
                })),
            )
        };
        let open = match open {
            Ok(o) => o,
            Err(e) => return Some(Err(format!("opening the Vulkan device with HDR metadata: {e}"))),
        };
        // SAFETY: `open` was just created from this same adapter's hal object with the features
        // and limits of `desc`, which is what `create_device_from_hal` requires.
        let made = unsafe { adapter.create_device_from_hal(open, desc) };
        Some(made.map_err(|e| e.to_string()))
    }

    pub fn set_metadata(device: &wgpu::Device, surface: &wgpu::Surface<'_>, meta: &HdrMetadata) -> bool {
        // SAFETY: `as_hal` borrows the device's and surface's Vulkan objects (None for another
        // backend) for the duration of the block. The extension's function is called only when
        // the device was created with it, with the surface's live swapchain (owned by that same
        // device, as wgpu configures surfaces on it), and with a fully initialised metadata
        // struct; the call has no failure mode.
        unsafe {
            let Some(dev) = device.as_hal::<wgpu_hal::vulkan::Api>() else { return false };
            if !dev.enabled_device_extensions().contains(&ash::ext::hdr_metadata::NAME) {
                return false;
            }
            let Some(surf) = surface.as_hal::<wgpu_hal::vulkan::Api>() else { return false };
            let Some(swapchain) = surf.raw_native_swapchain() else { return false };
            let ext = ash::ext::hdr_metadata::Device::new(dev.shared_instance().raw_instance(), dev.raw_device());
            let xy = |x, y| vk::XYColorEXT { x, y };
            let md = vk::HdrMetadataEXT::default()
                .display_primary_red(xy(0.64, 0.33))
                .display_primary_green(xy(0.30, 0.60))
                .display_primary_blue(xy(0.15, 0.06))
                .white_point(xy(0.3127, 0.3290))
                .max_luminance(meta.max_nits)
                .min_luminance(meta.min_nits)
                .max_content_light_level(meta.max_cll)
                .max_frame_average_light_level(meta.max_fall);
            ext.set_hdr_metadata(&[swapchain], &[md]);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_peak_is_the_headroom_over_sdr_white() {
        // KWin EDR, eDP-1 at 70 %: 1.4× over an 80 cd/m² linear unit
        let m = metadata_for(1.4, 1.0).unwrap();
        assert!((m.max_nits - 112.0).abs() < 1e-3 && m.max_cll == m.max_nits && m.min_nits < 1.0);
        // Windows scRGB with SDR white at 240 cd/m² (scale 3) and 4× headroom: 960 cd/m²
        assert!((metadata_for(4.0, 3.0).unwrap().max_nits - 960.0).abs() < 1e-3);
        // capped at PQ's 10 000 cd/m²
        assert_eq!(metadata_for(1e6, 1.0).unwrap().max_nits, 10_000.0);
    }

    #[test]
    fn no_headroom_or_nonsense_gives_no_metadata() {
        for (h, s) in [(1.0, 1.0), (0.5, 1.0), (f32::NAN, 1.0), (2.0, 0.0), (2.0, f32::INFINITY), (f32::INFINITY, 1.0)] {
            assert_eq!(metadata_for(h, s), None, "{h} {s}");
        }
    }
}
