//! `lightcraft-cli`: process wiring only. Everything the CLI does is in the library's `run`.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::process::ExitCode;

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn main() -> ExitCode {
    // `--features dhat-heap`: count allocations; the profile is written when `_heap` drops
    // (LIGHTCRAFT_DHAT_FILE, default dhat-heap.json).
    #[cfg(feature = "dhat-heap")]
    let _heap = {
        let file = std::env::var("LIGHTCRAFT_DHAT_FILE").unwrap_or_else(|_| "dhat-heap.json".into());
        lightcraft_engine::memory::set_heap_stats(|| {
            let s = dhat::HeapStats::get();
            lightcraft_engine::memory::HeapUsage { current: s.curr_bytes as u64, peak: s.max_bytes as u64 }
        });
        dhat::Profiler::builder().file_name(file).build()
    };
    lightcraft_cli::alloc_release::install();
    // Warnings (a GPU render redone on the CPU, an unknown backend name) on stderr; LIGHTCRAFT_LOG
    // or RUST_LOG picks another level (#168).
    lightcraft_engine::logging::install("lightcraft-cli");
    // `std::io::stdout()`, not its lock: a writer thread (MCP) writes to the same stream.
    let code = lightcraft_cli::run(std::env::args_os(), &mut std::io::stdout(), &mut std::io::stderr());
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
