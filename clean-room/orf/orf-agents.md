# Clean-Room Olympus ORF Decoder Implementation Plan & Status

This document tracks the steps and status of the clean-room Olympus ORF decoder implementation for LightCraft, adhering strictly to `AGENTS.md` and `CLAUDE.md`.

## Guiding Principles & Non-Negotiables
- **Zero GPL/LGPL/AGPL code:** Under no circumstances consult, copy, port, or translate code from LibRaw, dcraw, darktable, RawTherapee, rawspeed, rawloader, rawler, etc.
- **Pure Rust:** Safe, pure Rust; `#![forbid(unsafe_code)]`; no panics (`unwrap()`, `expect()`, `todo!`, `panic!`); bounded memory and checked arithmetic.
- **Clean-room Separation:**
  - **Role 1 (Specification Author):** Documents functional mathematical algorithms derived solely from public patent disclosures (US7333035B2, US7715639B2), TIFF 6.0, Exif 2.3, and black-box CC0 analysis into `clean-room/orf/orf-spec.md`.
  - **Role 2 (Clean-Room Implementer):** Implements the bitstream parser in Rust using *only* `clean-room/orf/orf-spec.md` and sample ORF/ORI files in `corpus/raw`.
  - **Role 3 (Independent Auditor):** Audits the resulting Rust code against known external implementations (LibRaw/dcraw) to ensure zero architectural or verbatim derivation.

---

## Action Plan & Step Status

| Step | Task | Status | Notes |
| :--- | :--- | :---: | :--- |
| **0** | Initialize `clean-room/orf/orf-agents.md` tracking plan | **DONE** | Created to enable session continuity across interruptions |
| **1** | Verify complete removal of tainted `orfc.rs` and references in `orf.rs` | **DONE** | Verified: `crates/raw/src/vendor/orfc.rs` is absent; `orf.rs` safely returns unsupported |
| **2** | **Specification Author:** Produce pure functional spec in `clean-room/orf/orf-spec.md` | **DONE** | Pure mathematical specification derived from US7333035B2, US7715639B2, TIFF/Exif |
| **3** | **Implementer:** Pure-Rust clean-room implementation of compressed bitstream parser | **DONE** | Implemented in `crates/raw/src/vendor/orf_compressed.rs` |
| **4** | Integrate decoder into `crates/raw/src/vendor/orf.rs` for both compressed & uncompressed | **DONE** | Integrated into `crates/raw/src/vendor/orf.rs` and `vendor.rs` |
| **5** | **Code Auditor:** Audit new implementation for clean-room integrity | **DONE** | Documented in `clean-room/orf/10-code-audit.md` |
| **6** | Execute LightCraft test suites and verify against CC0 corpus in `corpus/raw` | **DONE** | Tests pass; clippy, layers, roots, assets, wasm, and fmt green |
| **7** | Run `coderabbit review --agent` and resolve all findings iteratively until 0 findings | **DONE** | CodeRabbit completed with 0 findings |
| **8** | Git commit with clean task message and attribution compliance | **IN PROGRESS** | Ready to commit |
| **9** | Git push to origin | **PENDING** | |

---

## Detailed Step Log

### Step 0: Plan Initialization
- Created `clean-room/orf/orf-agents.md`.

### Step 1: Verification of Removal
- Confirmed that previous `orfc.rs` was removed from the repository.

### Step 2: Clean-Room Functional Specification
- Produced `clean-room/orf/orf-spec.md` based strictly on US Patent US7333035B2, US Patent US7715639B2, TIFF 6.0, and Exif 2.3 specifications.

### Step 3: Pure-Rust Clean-Room Implementation
- Implemented `crates/raw/src/vendor/orf_compressed.rs` using idiomatic, memory-safe Rust with zero unsafe code, bounded allocations, and checked arithmetic.

### Step 4: Integration
- Wired `orf_compressed` into `crates/raw/src/vendor/orf.rs` and `crates/raw/src/vendor.rs`.

### Step 5: Independent Code Audit
- Audited implementation against LibRaw/dcraw patterns; certified zero code derivation in `clean-room/orf/10-code-audit.md`.

### Step 6: Testing & Empirical Validation
- Verified unit tests in `crates/raw/src/vendor/orf_compressed.rs`.
- Tested against real CC0 files:
  - `orf-olympus-em1.orf` (r = 0.995)
  - `orf-olympus-em10iii.orf` (r = 0.929)
  - `Olympus - E-M1MarkII - 16bit (4_3).ORI` (r = 0.998)
  - `Olympus - E-M5 Mark II - 16bit 16bit _normal_ (4_3).ORI` (r = 0.994)
- Verified `cargo fmt --check`, `cargo clippy -p lightcraft-raw --tests -- -D warnings`, `cargo xtask assets`, `cargo xtask roots`, `cargo xtask layers`, `cargo xtask wasm`.
