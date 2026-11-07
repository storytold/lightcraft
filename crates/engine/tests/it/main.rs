//! The integration tests, one module per file, linked into one test binary.

#[cfg(feature = "denoise")]
mod denoise_cpu;
#[cfg(feature = "denoise")]
mod denoise_real;
mod negative_scans;
mod preview_read;
