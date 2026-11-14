# Clean-Room Olympus ORF Decoder Implementation Plan & Status

This document tracks the steps and status of the clean-room Olympus ORF decoder implementation for LightCraft, adhering strictly to `AGENTS.md` and `CLAUDE.md`.

## Guiding Principles & Non-Negotiables
- **Zero GPL/LGPL/AGPL code:** Under no circumstances consult, copy, port, or translate code from LibRaw, dcraw, darktable, RawTherapee, rawspeed, rawloader, rawler, etc.
- **Pure Rust:** Safe, pure Rust; `#![forbid(unsafe_code)]`; no panics (`unwrap()`, `expect()`, `todo!`, `panic!`); bounded memory and checked arithmetic.
- **Clean-room Separation:**
  - **Role 1 (Specification Author):** Documents functional mathematical algorithms derived solely from public patent disclosures (US7333035B2, US7715639B2), TIFF 6.0, Exif 2.3, and black-box CC0 analysis into `clean-room/orf/orf-spec.md`.
  - **Role 2 (Clean-Room Implementer):** Implements the bitstream parser in Rust using *only* `clean-room/orf/orf-spec.md` and sample ORF/ORI files in `corpus/raw`.
  - **Role 3 (Independent Auditor):** Audits the resulting Rust code against the clean-room specification, patent disclosures, and repository house rules to verify clean-room integrity, architectural novelty, and that no external source code was consulted or copied.

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
| **8** | Git commit with clean task message and attribution compliance | **DONE** | Committed cleanly as `036f84d9` |
| **9** | Git push to origin | **DONE** | Pushed branch `orf-clean2` to `origin` |

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
- Audited implementation against clean-room specifications and provenance requirements; certified zero external code derivation in `clean-room/orf/10-code-audit.md`.

### Step 6: Testing & Empirical Validation
- Verified unit tests in `crates/raw/src/vendor/orf_compressed.rs`.
- Tested against real CC0 files:
  - `orf-olympus-em1.orf` (r = 0.995)
  - `orf-olympus-em10iii.orf` (r = 0.929)
  - `Olympus - E-M1MarkII - 16bit (4_3).ORI` (r = 0.998)
  - `Olympus - E-M5 Mark II - 16bit 16bit _normal_ (4_3).ORI` (r = 0.994)
- Verified `cargo fmt --check`, `cargo clippy -p lightcraft-raw --tests -- -D warnings`, `cargo xtask assets`, `cargo xtask roots`, `cargo xtask layers`, `cargo xtask wasm`.

### Step 7: Code Review & Quality Gates
- Ran `coderabbit review --agent`.
- Resolved findings concerning strict parameter validation and audit phrasing accuracy.
- Achieved clean CodeRabbit review with 0 findings.

### Step 8 & 9: Commit & Remote Sync
- Committed changes as `036f84d9` and synced `orf-clean2` to `origin`.

---

## Reference Guide: Documentation Files to Modify for Camera Parity & Coverage

Per `AGENTS.md` (section *Start every session here*, rule 2) and the raw-parity governance guidelines, landing a camera format feature requires updating four tiers of documentation:

### 1. Camera Coverage & Verification Database (The Source of Truth)
- `docs/raw/evidence.tsv`: Add verified records for newly checked camera models (e.g. `Olympus OM-D E-M1`, `Olympus OM-D E-M10 Mark III`, `Olympus OM-D E-M1 Mark II (ORI)`, `Olympus OM-D E-M5 Mark II (ORI)`), recording test references and camera-JPEG correlation.
- `docs/raw/coverage.py`: Update the format classification rules (e.g. adjust `ORF_DECODED` pattern or format handlers) so that compressed ORF bodies are recognized as supported rather than preview-only.
- `docs/raw/coverage.tsv`: Generated file (do not edit directly). Regenerate it by running `python docs/raw/coverage.py`, which re-evaluates all camera models against `docs/raw/evidence.tsv` and the updated format rules in `docs/raw/coverage.py`, outputting the recalculated coverage tables:
  - OM / Olympus fully working: **6% → 81%**
  - Overall use-weighted camera coverage: **94% → 97%**
  - Preview-only models: **187 → 129**

### 2. RAW Format Specification & Clean-Room Records
- `docs/raw-parity.md`:
  - Updates the *By-use estimated* table (OM/Olympus to 81%, weighted camera total from 94% to 97%).
  - Updates the per-manufacturer breakdown table for Olympus and OM Digital Solutions.
  - Adds notes citing the clean-room implementation in `crates/raw/src/vendor/orf_compressed.rs`.
- `docs/orf-compression.md`: Documents clean-room status, patent citations (US7333035B2, US7715639B2), CC0 verification results, and links to `clean-room/orf/`.

### 3. Feature & Parity Trackers
- `docs/gaps.md`: Gap #6 (Compressed ORF) — status update from withdrawn/pending to closed (`0 h` remaining), citing the clean-room implementation and the 58 newly unlocked camera bodies.
- `docs/parity-checklist.md`: Updates row `LR-IMP-FORMATS` with a note citing `crates/raw/src/vendor/orf_compressed.rs` and the clean-room verification.
  - **Rule from `AGENTS.md`:** Run `cargo xtask parity --write` to automatically synchronize and refresh the summary table and counts at the top of the file.

### 4. Headline Numbers & Project Milestones
- `ROADMAP.md`:
  - Updates the top-level readiness summary: weighted camera coverage goes from **94% → 97%**.
  - Updates the Top Beta Needs list (marks Compressed ORF as completed).
  - Adds an entry to the **Progress log**.
  - **Rule from `AGENTS.md`:** Adds a row to the **Revision history** table at the bottom (never add a `Last updated` date to the header, which causes merge conflicts across branches).
- `docs/target-app-parity.md`:
  - Updates the *Core user profiles* tables:
    - *Working pro user:* Raw decoding goes from **94% → 97%**; weighted depth **74.5% → 75.3%**.
    - *Essentials user:* Raw decoding goes from **70.5% → 73%**; weighted depth **78.1% → 78.4%**.
  - Updates the Alpha gate summary notes.
  - Adds a row to the **Revision history** table.
- `docs/roadmap.md`: Synchronizes the progress log entry with `ROADMAP.md`.
- `README.md`: Updates the headline camera support bullet (e.g. *"97% of raw users' cameras supported"*).

---

## Reference Guide: Updating Test Corpora from raw.pixls.us

The standard command documented across the repository (e.g. in `AGENTS.md` L179, `crates/raw/tests/it/corpus.rs`, and `xtask/src/corpus.rs`) is:

```powershell
cargo xtask corpus --download
```

### What It Does
1. **Fetches from `raw.pixls.us` into `corpus/raw/`:**
   - Reads the manifest of CC0 raw sample URLs defined in `xtask/src/raw_samples.rs` (covering hundreds of camera bodies across Canon, Nikon, Sony, Olympus, Fujifilm, Panasonic, Pentax, Hasselblad, etc.).
   - Downloads each file directly into `corpus/raw/`.
2. **SHA-256 Verification:**
   - Checks every downloaded file against its pinned SHA-256 hash. If any file fails verification, it reports the mismatch and aborts.
3. **Downloads Auxiliary Test Corpora:**
   - Downloads the Nikon D750 CC0 ColorChecker test suite into `corpus/d750-colorchecker/`.

### Useful Operational Notes & Obtaining ORI Test Files
- **Location:** The test media is stored in `corpus/` at the repository root, which is git-ignored and never committed.
- **Skip Behavior:** When files are absent, test suites use `lightcraft_testkit::skip!(Corpus, ...)` to skip gracefully instead of failing.
- **Standard Manifest Samples:** `cargo xtask corpus --download` fetches the entries registered in `xtask/src/raw_samples.rs` (such as `orf-olympus-em1.orf` and `orf-olympus-em10iii.orf`).
- **High-Resolution ORI Test Files:**
  The compressed-ORF tests in `crates/raw/tests/it/olympus_samsung_corpus.rs` check two composite high-res `.ORI` sample files:
  1. `ori-olympus-e-m1markii-16bit-4-3-2.ori` (`Olympus - E-M1MarkII - 16bit (4_3).ORI`, SHA-256: `89138ccc958509ca1beb7028452400ab7f42061c0fc56e35c8a7d7a4a808dd0a`)
  2. `ori-olympus-e-m5-mark-ii-16bit-16bit-normal-4-3.ori` (`Olympus - E-M5 Mark II - 16bit 16bit _normal_ (4_3).ORI`, SHA-256: `f2c7a1dfbfcc6093b28373a1616569b8bd9c2d91cb708480b8312d510c57fbd6`)

  Both are registered directly in `xtask/src/raw_samples.rs` and downloaded by `cargo xtask corpus --download`.
  
  These originate from the CC0 Olympus high-resolution multi-shot raw submissions on `raw.pixls.us`.
  To make them available locally for the test suite:
  - Copy them from local clean-room storage:
    ```powershell
    Copy-Item "clean-room/orf/corpus/*.ORI" -Destination "corpus/raw/"
    ```
  - Or download individually from `raw.pixls.us`:
    ```powershell
    curl -L -o "corpus/raw/<filename>" "https://raw.pixls.us/getfile.php/<id>/nice/<name>"
    ```
  If neither file is present in `corpus/raw/`, the test suite automatically calls `lightcraft_testkit::skip!(Corpus, ...)` so the overall test gate remains clean.
