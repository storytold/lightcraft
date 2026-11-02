use super::prefs::config_dir;
use super::window_gpu;

/// The window's renderer (egui-wgpu): eframe's defaults, with LightCraft's backend choice — DX12
/// alone on Windows unless `LIGHTCRAFT_GPU_BACKEND` / `WGPU_BACKEND` say otherwise (issue #136:
/// with Vulkan in the set, wgpu loads the Vulkan driver even when it then picks DX12) — and DX12
/// shaders compiled with FXC, never a stray `dxcompiler.dll` (issue #471).
/// The fallback chain on top of it (DX12 → Vulkan → GL → glow) is in `window_gpu`.
#[cfg(test)]
pub(crate) fn window_wgpu_options() -> eframe::egui_wgpu::WgpuConfiguration {
    window_gpu::base_options()
}

/// Where the window's renderer files live (`window_gpu::Files`): nothing for `--memory` sessions
/// and `LIGHTCRAFT_NO_PREFS`.
pub(crate) fn window_files(in_memory: bool) -> window_gpu::Files {
    window_gpu::Files::new(config_dir().filter(|_| !in_memory && std::env::var_os("LIGHTCRAFT_NO_PREFS").is_none()))
}

/// Written while the GPU compute device is created, removed once that returned
/// (`lightcraft_gpu::backend::set_init_marker`). Not with `LIGHTCRAFT_NO_PREFS` (tests, scripts).
fn gpu_marker_path() -> Option<std::path::PathBuf> {
    if std::env::var_os("LIGHTCRAFT_NO_PREFS").is_some() {
        return None;
    }
    config_dir().map(|d| d.join("gpu-init.marker"))
}

/// Arm the GPU crash sentinel; if the previous launch left it behind (it died inside the GPU
/// driver while creating the device), the notice to show — GPU rendering then starts off.
///
/// A `--memory` session writes nothing (issue #169): it neither arms the sentinel — which would
/// create the settings folder — nor removes a marker it finds. It still honours one, so the GPU
/// starts off there too, and the next ordinary launch reports and clears it.
pub(crate) fn gpu_crash_check(in_memory: bool, saved_card: &str) -> Option<GpuCrash> {
    gpu_crash_check_at(gpu_marker_path(), in_memory, saved_card)
}

pub(crate) fn gpu_crash_check_at(marker: Option<std::path::PathBuf>, in_memory: bool, saved_card: &str) -> Option<GpuCrash> {
    use lightcraft_engine::gpu::backend;
    let left = if in_memory {
        marker.as_deref().and_then(backend::read_init_marker)
    } else {
        let left = marker.as_deref().and_then(backend::take_init_marker);
        backend::set_init_marker(marker);
        left
    };
    left.map(|what| gpu_crash_from(&what, saved_card))
}

/// What a crash of the last launch inside the graphics driver costs this one, and the notice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GpuCrash {
    pub(crate) notice: String,
    /// GPU rendering starts off (the crash was in driver work that names no chosen card).
    pub(crate) gpu_off: bool,
    /// The graphics card chosen for editing goes back to Automatic (it crashed starting).
    pub(crate) reset_card: bool,
    /// The second graphics card is no longer used (it crashed starting).
    pub(crate) helper_off: bool,
}

impl GpuCrash {
    /// A crash (or a lost device) that turns GPU rendering off.
    pub(crate) fn gpu_off(notice: String) -> GpuCrash {
        GpuCrash { notice, gpu_off: true, reset_card: false, helper_off: false }
    }

    /// The start-up crash check's result and the lost-device check's together: both are read on
    /// every launch, a lost device turns GPU rendering off, and both notices are shown (review of
    /// PR #3506).
    pub(crate) fn with_lost(crash: Option<GpuCrash>, lost: Option<String>) -> Option<GpuCrash> {
        match (crash, lost) {
            (Some(mut c), Some(l)) => {
                c.gpu_off = true;
                c.notice = format!("{}\n\n{l}", c.notice);
                Some(c)
            }
            (c, None) => c,
            (None, Some(l)) => Some(GpuCrash::gpu_off(l)),
        }
    }
}

/// [`GpuCrash`] from what the crash marker recorded: when every scope it names was a device of a
/// card chosen in the settings (`saved_card`) or of the second card, only those choices are
/// dropped and the GPU stays on; anything else (Automatic's card, listing the cards, AI kernels)
/// turns GPU rendering off, as before.
pub(crate) fn gpu_crash_from(what: &str, saved_card: &str) -> GpuCrash {
    use lightcraft_engine::gpu::{backend, cards::AdapterKey};
    let saved = AdapterKey::parse(saved_card);
    let (mut card, mut helper) = (None, None);
    let scopes = backend::marker_scopes(what);
    let only_choices = !scopes.is_empty()
        && scopes.iter().all(|s| match &s.card {
            Some((k, role)) if role == "helper" => {
                helper = Some(k.name.clone());
                true
            }
            Some((k, role)) if role == "editing" && saved.as_ref().is_some_and(|c| c.same_model(k)) => {
                card = Some(k.name.clone());
                true
            }
            _ => false,
        });
    if !only_choices {
        return GpuCrash::gpu_off(gpu_crash_notice(what));
    }
    let mut notice = Vec::new();
    if let Some(name) = &card {
        notice.push(format!(
            "LightCraft closed unexpectedly while starting the graphics card chosen for editing ({name}) last time, so it now uses Automatic. \
             To try that card again, choose it in Settings ▸ Performance ▸ Graphics card for editing."
        ));
    }
    if let Some(name) = &helper {
        notice.push(format!(
            "LightCraft closed unexpectedly while starting the second graphics card ({name}) last time, so it isn't used any more. \
             To try it again, choose it in Settings ▸ Performance ▸ Second graphics card."
        ));
    }
    GpuCrash { notice: notice.join(" "), gpu_off: false, reset_card: card.is_some(), helper_off: helper.is_some() }
}

pub(crate) fn gpu_crash_notice(what: &str) -> String {
    format!(
        "LightCraft closed unexpectedly while starting the GPU last time ({what}), so GPU rendering is now off and photos render on the CPU. \
         To try the GPU again, turn on Settings ▸ Performance ▸ Use the GPU for rendering; to try another graphics backend, start LightCraft with \
         LIGHTCRAFT_GPU_BACKEND=dx12, vulkan or off (see docs/gpu-pipeline.md → Troubleshooting)."
    )
}

/// Written when the window stopped drawing after the GPU device was lost (issue #1337): the next
/// launch renders on the CPU and says why.
pub(crate) fn gpu_lost_marker_path() -> Option<std::path::PathBuf> {
    gpu_marker_path().map(|p| p.with_file_name("gpu-lost.marker"))
}

/// Why the GPU was lost, when the device failed in this process (a driver reset, a hang, the
/// Windows watchdog), even if a new device replaced it since: `None` when it never failed.
pub(crate) fn gpu_lost_reason() -> Option<String> {
    lightcraft_engine::gpu::last_loss()
}

/// Record that LightCraft stopped after the GPU was lost, for the next launch.
pub(crate) fn record_gpu_lost(marker: Option<&std::path::Path>, reason: &str) {
    let Some(m) = marker else { return };
    if let Some(d) = m.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Err(e) = std::fs::write(m, format!("{reason}\n")) {
        log::warn!("could not write {}: {e}", m.display());
    }
}

/// A GPU loss the last launch stopped on: its notice, once (the marker is removed, except by a
/// `--memory` session, which leaves the disk as it found it).
pub(crate) fn gpu_lost_check_at(marker: Option<&std::path::Path>, in_memory: bool) -> Option<String> {
    let m = marker?;
    let reason = std::fs::read_to_string(m).ok()?;
    if !in_memory {
        let _ = std::fs::remove_file(m);
    }
    Some(format!(
        "LightCraft stopped last time after the graphics driver reset the GPU ({}) while photos rendered on it, so GPU rendering \
         is now off and photos render on the CPU. To try the GPU again, turn on Settings ▸ Performance ▸ Use the GPU for rendering; \
         an up-to-date graphics driver may help.",
        reason.trim()
    ))
}
