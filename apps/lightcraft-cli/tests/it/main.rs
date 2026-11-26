//! The integration tests, one module per file, linked into one test binary.

mod cli;
mod mcp_shutdown;

/// Nextest archives relocate the executable; plain Cargo uses its compile-time path.
fn binary() -> std::ffi::OsString {
    std::env::var_os("NEXTEST_BIN_EXE_lightcraft_cli").unwrap_or_else(|| env!("CARGO_BIN_EXE_lightcraft-cli").into())
}
