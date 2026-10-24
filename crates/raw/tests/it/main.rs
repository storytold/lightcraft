//! The integration tests, one module per file, linked into one test binary.
//! `tests/sr2_heap.rs` stays a binary of its own: it installs dhat as the global allocator.

mod body_corpus;
mod canon_pentax_corpus;
mod corpus;
mod coverage_sweep_corpus;
mod cr3_corpus;
mod cr3_variants;
mod crx_properties;
mod dng;
mod gpr_corpus;
mod lossy_dng;
mod nrw_be16;
mod olympus_samsung_corpus;
mod rendered_dng;
mod robust;
mod sony_leica_corpus;
mod sony_lens_inventory;
mod sparse_corpus;
mod vendor_robust;
