# Architecture

> **Last reviewed:** 2026-10-10 · **Last updated:** 2026-10-26 · **Change:** minor (`ci` and `xtask test` bound each test binary's run and the wait for the CI slot) · **Target:** Adobe Lightroom Classic 15.5 / Lightroom 9.6

How LightCraft is built today. The longer design document (`plan/architecture.md`) is local-only
(`plan/` is gitignored); this page is the committed, current summary. Sizes are Rust lines counted
on 2026-10-10 (≈ 202,000 tracked lines in 505 files: 24 library crates, 3 apps and `xtask`).

## Layers

`cargo xtask layers` (part of `cargo xtask ci`, `xtask/src/layers.rs`) enforces that nothing below
L5 depends on egui, eframe, winit or rfd, and that each crate only uses lower layers.

| Layer | Crates (lines) | Role |
|---|---|---|
| L0 | `geom` (1.4k), `color` (0.9k), `raster` (0.8k), `tiff` (2.0k), `fetch` (1.4k), `sysmem` (0.1k) | Matrices and warps, colour spaces and transfer functions, float image buffers, TIFF/IFD read and write, pinned HTTPS model downloads, the one `unsafe` FFI call (`malloc_zone_pressure_relief`) |
| L1 | `raw` (22.3k), `codecs` (7.3k), `meta` (4.3k), `develop` (2.5k), `denoise-core` (2.2k), `denoise` (2.7k), `faces` (5.3k), `scenes` (1.0k), `heif` (2.1k, optional) | Raw decoders and demosaic, DNG colour model; standard codecs and encoders; EXIF/XMP/IPTC; the non-destructive edit model and control specs; AI denoise; face detection and recognition; procedural demo photos; HEIC |
| L2 | `pipeline` (9.8k) | The develop pipeline: scene-referred float, resolution independent; the CPU reference (oracle) for everything else |
| L3 | `catalog` (11.0k), `gpu` (5.1k), `preview` (1.3k), `merge` (3.8k), `segment` (2.3k) | Library model and persistence; wgpu compute twin of the pipeline; thumbnail/preview cache and job pool; HDR and panorama merge; SAM 3 segmentation |
| L4 | `engine` (53.2k) | Session, command registry (everything is a command), history, render and export jobs, import, Lightroom catalog and preset import, faces index, view-models |
| L5 | `ui-egui` (46.9k), `mcp` (2.3k) | The Lightroom-style egui interface (swappable); the MCP server |
| L6 | `apps/lightcraft` (2.8k), `apps/lightcraft-cli` (1.5k), `apps/lightcraft-web` (3.4k), `xtask` (3.4k), `testkit` (0.2k, dev-dependency only) | Desktop app (eframe + wgpu, native menus, control server), CLI (render, commands, calibrate, snapshot, MCP), WASM build, tooling; test helpers (skips the CI report lists) |

## Data model

- **Photo** records in `catalog` hold the original's path, content hash, metadata, ratings, flags,
  labels, keywords, faces and the current `develop::Settings`. Albums, smart albums (rule trees),
  stacks, virtual copies, versions and folder records live alongside.
- **Persistence** (`crates/catalog/src/journal.rs`): an append-only journal of human-readable
  operations plus periodic snapshots, compacted in the background; torn appends and crashes are
  tested (`tests_torn_append.rs`, `tests_journal.rs`). A lock file guards against two writers.
- **Edits** are a `develop::Settings` value (every slider is a control spec with id, range and
  default), normalized to image coordinates so previews and exports match. Process versions
  (`docs/process-versions.md`) keep old edits rendering the same after a retune.
- **Sidecars**: XMP read and write with Adobe `crs:` interop (`docs/xmp-interop.md`); Lightroom
  Classic `.lrcat` import through a pure-Rust SQLite reader (`docs/lightroom-catalog-import.md`).

## Rendering

1. **Decode** (`raw`, `codecs`): raw files to linear camera RGB (demosaic, black/white levels,
   opcodes, highlight handling) and a camera-to-XYZ colour model; standard images to linear light
   through their ICC profile.
2. **Develop** (`pipeline`): white balance, profile, exposure and tone (local Laplacian
   highlights/shadows), curves, HSL / colour mixer, point colour, colour grading, texture, clarity,
   dehaze, detail (sharpening, NR, AI denoise result), optics (lens profile, CA, defringe,
   vignette), geometry (crop, Upright, transforms), masks with local adjustments, heal/clone, grain,
   effects. Stage caching and a source pyramid make slider drags cheap.
3. **GPU** (`gpu`): the same stages in WGSL on Metal / DX12 / Vulkan, checked against the CPU path
   (within 1/255); GPU errors fall back to the CPU (`docs/gpu-pipeline.md`).
4. **Display and export**: display-profile conversion (`docs/display-profiles.md`); export through
   one encoder for app, CLI, MCP and web (`lightcraft_engine::export`).

## Agent control

Every action is an engine command (id, label, menu path, shortcut, params, enabled, run). The UI,
the CLI, the JSON-lines control channel (`docs/control-protocol.md`) and MCP (`docs/mcp.md`) all
dispatch by id, and headless snapshots render the UI without a window.

## Quality gates

`cargo xtask ci`: fmt, clippy `-D warnings` with the never-crash lints, the HEIF build, tests,
`parity` (every id and path cited in [parity-checklist.md](parity-checklist.md) exists), `layers`,
`assets` (every asset attributed, no Adobe formats) and the WASM build. Other workflows build and
test FreeBSD and Windows ARM64, lint packaging, and build releases.

The tests run under `cargo test --workspace --lib --bins --tests`, or `cargo nextest run` with the
same targets when `LIGHTCRAFT_NEXTEST=1` is set and cargo-nextest is installed; doctests run after
them with `cargo test --workspace --doc`. `cargo test` is the default because the demo library's
generated scenes are cached per process, and nextest's process per test regenerates them.
Examples are left out of the test build (clippy `--all-targets` still
checks them). Each crate links its integration tests into one binary, `tests/it/main.rs` with one
module per file (`cargo test -p <crate> --test it <module>::`); a test that changes state for its
whole process (a global allocator, closing the GPU) keeps a binary of its own in `tests/`. The WASM
build is one `cargo check --target wasm32-unknown-unknown --keep-going` over the wasm-safe crates.

`ci`'s resource caps (build jobs by available RAM, at most 4 test threads, line-table debuginfo) and
its one-run-per-machine slot (`xtask/src/ci_lock.rs`) also cover `cargo xtask test [-p PKG]…
[--keep-going] [--doc] [-- ARGS]` (`xtask/src/test.rs`), the way to run the tests alone: subcommands
opt into the caps with `use_ci_caps`. It builds every workspace test target once, as `ci` does, and
runs each test binary, or the `-p` packages' binaries, itself (`test::run_binary`, which `ci --quick`
uses too); `--keep-going` runs every binary and names each that failed instead of stopping at the
first.

Nothing these runs wait on is unbounded (`xtask/src/deadline.rs`, #3060). Stable libtest has no
per-test timeout, so each test binary gets `LIGHTCRAFT_TEST_BINARY_TIMEOUT` minutes (default 30; `0`
for none): timed from its start when xtask runs it (`test::run_binary`), and from cargo's `Running` /
`Doc-tests` line for it, and again from its own `running N tests` line, when `cargo test` runs it (the
full `ci`'s test and HEIF steps; the build before isn't bounded; cargo runs with
`CARGO_TERM_QUIET=false` and without `-q` so it prints those lines). The output streams through and is
read for libtest's lines, keeping a bounded amount. On timeout the binary and every process it started
are killed, and no process xtask didn't start: the binary and its process tree (a process only once
its parent is frozen, so its pid can't be reused meanwhile), and on Linux every process whose
`/proc/<pid>/environ` holds the run's exact marker entry (`LIGHTCRAFT_XTASK_RUN_<pid>_<nonce>_<n>=1`,
inherited by all it starts; the nonce is random per xtask process), which finds those that outlived
their parent. They are frozen with SIGSTOP, then killed (`taskkill /T` on Windows; elsewhere the
report says reparented ones can't be found). The child stays in xtask's process group, so Ctrl-C
still reaches it. The run reports `timed out after N min: <binary>`, what
the kill found, the tests still running (libtest's "has been running for over 60 seconds" warnings,
or the test a single-threaded run started) and those that never reported a result (`--list`, itself
bounded in time and size). Under nextest, `.config/nextest.toml`'s `slow-timeout` kills a test
after 30 minutes instead. The wait for the CI slot gives up after `LIGHTCRAFT_CI_LOCK_WAIT` minutes
(default 120, several full runs; `0` for no limit) with an error naming the run that holds it.

Outside `ci`, `.cargo/config.toml`'s `[env]` limits plain `cargo test` to 4 tests at once per test
binary (`RUST_TEST_THREADS = { value = "4", force = false }`, so a value already in the environment
wins), and `.config/nextest.toml` sets `test-threads = 4` for `cargo nextest run`. `cargo xtask` runs
under that config too, so `[env]` also sets a marker, `LIGHTCRAFT_TEST_THREADS_DEFAULT`, to the same
value: a `RUST_TEST_THREADS` equal to it counts as unset (`caller_sets` in `xtask/src/main.rs`) and the
caps replace it with the RAM-aware value, while any other value is the caller's and wins as-is. An
explicit `RUST_TEST_THREADS=4` can't be told apart from the default, so `ci` and `xtask test` may
lower it to their RAM-aware value (never raise it).

`ci` also probes the machine: OS, CPUs, RAM, the GPU adapters of each backend (from the ignored
`gpu_probe` test of `lightcraft-gpu`, run after the workspace tests so it reuses their build, each
test binary it starts bounded by 5 minutes, and skipped once a test binary timed out; xtask
has no wgpu), the corpus and `CRAFT_FONTS_DIR`. Tests
that need what is missing call `lightcraft_testkit::skip!(need, reason)`, which appends a JSON line to
the file `LIGHTCRAFT_SKIP_LOG` names. The run ends with the machine and the skips by need and reason,
and writes `target/ci/report.json` (commit, dirty flag, machine, capabilities, each step's result and
time, skips) and `target/ci/summary.md` (Markdown for a PR; `--summary-md` prints it). The report holds
no host name, user name or absolute path, and nothing is uploaded. `--hardware` adds a step that runs
the `lightcraft-gpu` tests once per backend with a hardware adapter (`LIGHTCRAFT_GPU_BACKEND`).

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-26 | minor | Quality gates: each test binary's run (`LIGHTCRAFT_TEST_BINARY_TIMEOUT`) and the wait for the CI slot (`LIGHTCRAFT_CI_LOCK_WAIT`) have a time limit |
| 2026-10-26 | minor | Quality gates: plain `cargo test` (and `cargo nextest run`) runs at most 4 tests at once; xtask tells that default from a caller's `RUST_TEST_THREADS` |
| 2026-10-26 | minor | Quality gates: `cargo xtask test` runs the tests with `ci`'s caps and slot, and can keep going past a failing binary |
| 2026-10-23 | minor | Quality gates: one integration-test binary per crate, doctests apart, nextest on request, one WASM check |
| 2026-10-11 | minor | Quality gates: the CI machine probe, visible skips (`testkit`) and `target/ci/report.json` |
| 2026-10-10 | major | Created from the code (crate sizes, layers, data model, rendering, control, gates) |
