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
