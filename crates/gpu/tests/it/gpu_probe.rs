//! What GPU this machine offers the develop pipeline, as one JSON line: the capability probe of
//! `cargo xtask ci`, which is std + serde_json and has no wgpu of its own. Ignored in normal runs;
//! xtask runs it after the workspace tests, so it costs no extra build:
//! `cargo test --workspace --test it -- --ignored --exact gpu_probe::gpu_probe --nocapture`.
//!
//! - `LIGHTCRAFT_GPU_PROBE` unset: the compute device the pipeline picks (`available()`,
//!   `adapter_name()`, `unavailable_reason()`), the backends it may use and the backends this build
//!   contains.
//! - `LIGHTCRAFT_GPU_PROBE=<backend>` (`vulkan`, `metal`, `dx12`, `gl`): every adapter that
//!   backend lists, software ones (lavapipe, llvmpipe, WARP) included. One backend per process, so
//!   a driver that crashes while it is enumerated (issue #136) costs only its own line.
//!
//! Only reports: which adapters the pipeline accepts is `ctx.rs`'s policy.

use lightcraft_gpu::backend::{self, Backends};

const NAMES: [(&str, Backends); 4] = [("vulkan", Backends::VULKAN), ("metal", Backends::METAL), ("dx12", Backends::DX12), ("gl", Backends::GL)];

#[test]
#[ignore = "the capability probe of `cargo xtask ci`"]
fn gpu_probe() {
    let line = match std::env::var("LIGHTCRAFT_GPU_PROBE").ok().filter(|b| !b.is_empty()) {
        None => pipeline(),
        Some(name) => adapters(&name),
    };
    // its own line, prefixed: libtest may have printed `test gpu_probe ... ` on this one
    println!("\ngpu_probe: {line}");
}

fn names(set: Backends) -> String {
    let list: Vec<String> = NAMES.iter().filter(|(_, b)| set.contains(*b)).map(|(n, _)| json_str(n)).collect();
    format!("[{}]", list.join(","))
}

fn pipeline() -> String {
    let available = lightcraft_gpu::available();
    let opt = |s: Option<String>| s.map_or_else(|| "null".to_owned(), |s| json_str(&s));
    let compiled = wgpu::Instance::enabled_backend_features();
    format!(
        "{{\"pipeline\":{{\"available\":{available},\"adapter\":{},\"reason\":{},\"backends\":{},\"compiled\":{}}}}}",
        opt(lightcraft_gpu::adapter_name()),
        opt(lightcraft_gpu::unavailable_reason()),
        names(backend::compute_backends().unwrap_or(Backends::empty()) & compiled),
        names(compiled),
    )
}

fn adapters(name: &str) -> String {
    let (_, set) = NAMES.iter().find(|(n, _)| *n == name).unwrap_or_else(|| panic!("unknown backend `{name}` (vulkan, metal, dx12, gl)"));
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = *set;
    desc.backend_options = backend::backend_options();
    desc.flags = backend::instance_flags_from_env(desc.flags);
    let instance = wgpu::Instance::new(desc);
    let adapters: Vec<String> = pollster::block_on(instance.enumerate_adapters(*set))
        .iter()
        .map(|a| {
            let i = a.get_info();
            format!(
                "{{\"name\":{},\"type\":{},\"vendor\":\"0x{:04x}\",\"device\":\"0x{:04x}\",\"driver\":{},\"driver_info\":{}}}",
                json_str(&i.name),
                json_str(&format!("{:?}", i.device_type)),
                i.vendor,
                i.device,
                json_str(&i.driver),
                json_str(&i.driver_info),
            )
        })
        .collect();
    format!("{{\"backend\":{},\"adapters\":[{}]}}", json_str(name), adapters.join(","))
}

/// A JSON string literal.
fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_without_a_marker_starts_with_glow() {
        let all = Backends::DX12 | Backends::VULKAN | Backends::GL | Backends::METAL;
        let gl = backend::WindowRenderer::Wgpu(Backends::GL);
        let vk = backend::WindowRenderer::Wgpu(Backends::VULKAN);

        // Test glow_without_marker returns true for Linux without marker
        assert!(backend::glow_without_marker(None, false, "linux", true));
        assert!(!backend::glow_without_marker(None, true, "linux", true));
        assert!(!backend::glow_without_marker(Some(vk), false, "linux", true));
        assert!(!backend::glow_without_marker(None, false, "linux", false));
        assert!(!backend::glow_without_marker(None, false, "windows", true));

        // Test initial_window_renderer returns Glow for Linux without marker
        assert_eq!(backend::initial_window_renderer(None, None, None, false, "linux", all, true), Some(backend::WindowRenderer::Glow));

        // A glow the last start died in is not retried first without a marker
        assert_eq!(backend::initial_window_renderer(None, None, Some(backend::WindowRenderer::Glow), false, "linux", all, true), Some(gl));
        assert_eq!(backend::initial_window_renderer(None, Some(vk), Some(backend::WindowRenderer::Glow), false, "linux", all, true), Some(gl));

        // A remembered wgpu renderer that worked is kept even without a marker
        assert_eq!(backend::initial_window_renderer(None, Some(gl), None, false, "linux", all, true), Some(gl));

        // A remembered renderer that is also the failed one is not retried first without a marker
        assert_eq!(backend::initial_window_renderer(None, Some(gl), Some(gl), false, "linux", all, true), Some(vk));
        assert_eq!(
            backend::initial_window_renderer(
                None,
                Some(backend::WindowRenderer::Glow),
                Some(backend::WindowRenderer::Glow),
                false,
                "linux",
                all,
                true
            ),
            Some(gl)
        );

        // A remembered Vulkan renderer is not kept without a marker
        assert_eq!(backend::initial_window_renderer(None, Some(vk), None, false, "linux", all, true), Some(backend::WindowRenderer::Glow));

        // With a marker available the existing order is kept
        assert_eq!(backend::initial_window_renderer(None, None, None, true, "linux", all, true), Some(gl));
        assert_eq!(backend::initial_window_renderer(Some(vk), None, None, false, "linux", all, true), Some(vk));
        assert_eq!(backend::initial_window_renderer(None, None, None, false, "linux", all, false), Some(gl));

        // The marker-less order should start with glow when no marker is available (regression test for #3885)
        let order = backend::window_renderer_order(None, None, None, "linux", all, true);
        assert_eq!(order.first(), Some(&backend::WindowRenderer::Glow));

        // Non-Windows selector coverage for Linux with/without marker, explicit backend lists, and non-Linux control
        assert_eq!(backend::choose_non_windows_window_renderer(None, None, None, false, "linux", all, true), backend::WindowRenderer::Glow);
        assert_eq!(backend::choose_non_windows_window_renderer(None, None, None, true, "linux", all, true), gl);
        assert_eq!(
            backend::choose_non_windows_window_renderer(Some("vulkan,gl"), None, None, false, "linux", all, true),
            backend::WindowRenderer::Wgpu(Backends::VULKAN | Backends::GL)
        );
        assert_eq!(
            backend::choose_non_windows_window_renderer(None, None, None, false, "freebsd", all, true),
            backend::WindowRenderer::Wgpu(backend::default_window_backends("freebsd") & all)
        );
    }
}
