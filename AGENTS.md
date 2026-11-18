# Instructions for agents

Chromaforge: clean-room, pure-Rust photo library + non-destructive raw developer, targeting Lightroom parity.
Native macOS/Windows/Linux, web via WASM. Fork of an upstream project (`NOTICE`); "Lightroom" parity target only.
Upstream is **not merged**: each upstream
PR/commit is reviewed on its own and ported by hand; one intake round = one fork PR
([`docs/upstream-pr-intake.md`](docs/upstream-pr-intake.md), mechanics: [`docs/upstream-merge.md`](docs/upstream-merge.md)).

This is the one rulebook for agents and humans; `CLAUDE.md` only imports it. Claiming issues / opening PRs:
[`CONTRIBUTING.md`](CONTRIBUTING.md).

## Brand: `brand.toml` only
- Name, binary names, app id, settings dir, env prefix, MCP id, URL scheme, extensions live in [`brand.toml`](brand.toml)
  → `lcc-brand`. Never write the product or legacy name in code/UI/tests: `{app}`/`{cli}`/`{env}` in i18n catalogs,
  `lcc_brand::…` in code. Docs say "Chromaforge" (display name only, never "this fork").
- `cargo xtask brand check` (in `ci`) enforces; exceptions: `brand.toml`, `[legacy]` handling, `NOTICE`, `LICENSE`,
  `assets/ATTRIBUTION.md`, `docs/upstream-merge.md`, Markdown, `.md.in`.
- Docs needing real names are `X.md.in` templates (`{{app}}`, `{{binary}}`, `{{cli_binary}}`, `{{env_prefix}}`,
  `{{app_id}}`, `{{settings_dir}}`, rendered by `cargo xtask docs`); otherwise write `<app>` `<binary>` `<cli>`
  `<PREFIX>` `<settings_dir>`.
- Crates `lcc-*`; binaries `lcc-app`/`lcc-cli`/`lcc-web`; in `target/` always `app`/`app-cli` (brand names via
  `cargo xtask run|package|install`). Env vars `<PREFIX>_X` (e.g. `<PREFIX>_PROFILE`); legacy prefix still read.

## Session start
1. `plan/STATUS.md` (milestone, next task, blockers) → `plan/execution-plan.md` §3 (task) + §7 (autonomy protocol:
   don't ask unless §7 says it's the user's call) → relevant `plan/architecture.md` section + crate docs. Visual
   reference: `plan/lightroom/` (gitignored; `10-observed-ui.md` + screenshots).
2. `ROADMAP.md` + [`docs/target-app-parity.md`](docs/target-app-parity.md). The checklist counts existence; real gaps
   are quality/coverage (camera colour, CR3/compressed raws, per-model verification, render fidelity, AI, HDR/video/
   Classic; raw detail: [`docs/raw-parity.md`](docs/raw-parity.md)). A wrong ✅ row gets downgraded to 🟡.
3. Pick work from [`docs/gaps.md`](docs/gaps.md) (impact-ranked, `B` = beta blocker); row tracker:
   [`docs/parity-checklist.md`](docs/parity-checklist.md). Landing a feature updates its rows + gap entry + ROADMAP/
   target-app-parity numbers + *Last updated* lines in the same commit — except parity-checklist.md, which never
   carries a *Last updated*/*Change* line. `cargo xtask parity` (in `ci`) verifies cited ids/paths and gap uniqueness;
   `--write` refreshes the summary.
4. `plan/observations/` (Adobe notes) is gitignored, local-only.

**Branches:** PRs start from latest `main`; re-merge `origin/main` before pushing and whenever `main` moves
(auto-merge doesn't update the branch; `gh pr update-branch N` does). Merge agent branches one at a time with
`cargo xtask ci --fast` + targeted tests between (two green branches can still conflict). Resuming after a crash:
check `git status` on main, `git worktree list`, `main..HEAD` per worktree; commit small and often.

## Never crash (outranks features)
Malformed raw/JPEG/XMP, bad command/control/MCP argument, corrupt catalog/settings, full disk → actionable error,
never a panic. Never ship a panic path; fix crashes before building on them. This section is the full standard.
- Non-test code: no `unwrap()`/`expect()`/`panic!`/`unreachable!`/`todo!`/`unimplemented!`, no `unsafe`. `Result`+`?`,
  `ok_or(..)?`, `let…else`, `if let`; `unwrap_or…` only where it can't silently corrupt. Unfinished feature →
  "unsupported" error or disabled. Sole exception: provably infallible literal with
  `#[allow(clippy::expect_used)]` + `.expect("why")`.
- `unsafe` lives only in: `crates/sysmem` (Win: `GetCurrentThread`+`SetThreadPriority` for render workers,
  `GetDiskFreeSpaceExW` for cache sizing; macOS: `malloc_zone_pressure_relief` after raw decodes, `fsync` for SMB; glibc Linux: `malloc_trim` after raw
  decodes and preview builds; Android: bionic `mallopt(M_PURGE_ALL)` on entering Develop;
  free space elsewhere = POSIX `df`), `crates/hdrmeta` (wgpu-hal: `VK_EXT_hdr_metadata` + `vkSetHdrMetadataEXT` on
  Linux/BSD Wayland), `crates/gpumem` (wgpu-hal GPU memory budget for the HDR zoom cache), `crates/android-host`
  (JNI VM/activity adoption), `crates/winspool` (Win32 print spooler), `crates/winmsi` (`MsiEnumClientsW` +
  `MsiGetComponentPathW` for the updater), and `apps/android`'s `#[unsafe(no_mangle)] android_main`. Every other
  production crate root has `#![forbid(unsafe_code)]`. New unsafe need → isolated, well-tested helper crate:
  `// SAFETY:` everywhere, safe API, safe fallback, and a line here naming it.
- Input-derived numbers are hostile: `get()` not `[i]`/`[a..b]`; checked/saturating math; no division by zero,
  NaN/inf, negative→`usize`; cap input-sized allocations; slice strings at char boundaries.
- Bound recursion (depth limits/seen-sets: IFD chains, nested metadata). Poisoned locks:
  `lock().unwrap_or_else(PoisonError::into_inner)` or error; thread joins are `Result`s.
- Last-resort guard: panic hook + `catch_unwind` around command dispatch and import/export → error dialog, document
  kept. Safety net, not a licence; keep `panic = "unwind"` on native.
- Every crash fix lands with a hostile-input no-panic check, folded into the format's table-driven hostile-input test
  where one exists.
- Enforced by clippy: `clippy.toml` allows unwrap/expect/panic/indexing in tests only; every production crate root
  (`lib.rs`, each `main.rs`) has `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic,
  clippy::unimplemented, clippy::todo, clippy::unreachable)]` — deliberately not `[workspace.lints]` (that would hit
  tests/examples/benches). New crates start with it.

## Non-negotiables
- **Third-party material** — see the licence/derivation policy below.
- **Pure Rust** in the product, no C/C++ dependencies; covers our crates and anything we bundle. **Plug-ins are
  exempt:** third-party WASM modules (ABI v1, `wasmi`-sandboxed, [`docs/plugins.md`](docs/plugins.md)), any language
  that compiles to WASM, own licence, untrusted input to the host.
- **Layering** (`plan/architecture.md` §3, `cargo xtask layers`): nothing below L5 depends on egui/eframe/winit/rfd.
- **Everything is a command** (`crates/engine`): id, label, menu path, shortcut, params, `enabled()`, `run()`. UI,
  CLI, control channel, MCP all dispatch by id. Every slider is a `develop` control spec.
- **Activity stack** ([`docs/background-tasks.md`](docs/background-tasks.md)): off-UI-thread work longer than a moment
  holds a `TaskGuard` from `session.activity.start(kind, label, cancel)`; adopts the job's `Cancel::Flag` or
  `Cancel::No`. No own progress UI; tests assert the row.
- **Resolution independence:** normalized image coordinates, relative radii; previews == exports.
- **Time via `chrono`.** Never hand-roll calendar math. One exception: past chrono's range, shift by whole 400-year
  Gregorian cycles (146,097 days), let chrono compute, add cycles back.
- **Localized UI text:** `tr`/`tr_format!`; fork messages in `crates/ui-egui/locales/fork/`, complete in every
  language (tests fail on gaps). [`docs/localization.md`](docs/localization.md).
- **Release notes:** one file per change: `docs/whats-new/<YYYY-MM-DD>-<slug>.md` (one `### Title` + bullets) and,
  if the changelog should mention it, `docs/changelog/unreleased/<YYYY-MM-DD>-<slug>.md` (one `## Section` + bullets);
  READMEs there say how. Never touch `docs/whats-new.md` (frozen) or released `docs/changelog/<version>.md`;
  `cargo xtask notes --check` (in `ci`) fails if `docs/whats-new.md` changes outside an upstream merge.
- **The manual moves with the product:** any change a user can notice (new/changed/removed feature, command, menu item,
  panel, slider, setting, shortcut, default, file format, platform or install step, or a screenshot that no longer
  matches) updates the manual **in the same commit**. The manual has two parts: the page-per-feature site in
  `docs/manual-site/src/content/docs/` (picture-first; rules and screenshot runner in
  [`docs/manual-site/AUTHORING.md`](docs/manual-site/AUTHORING.md)) and the markdown guide in `docs/manual/`. Edit the
  page that names the feature, or add one; fix every sentence the change makes false; retake affected shots
  (`scripts/shots.d/<area>.json`) or mark them `<NeedsImage>`; add a row to `reference/` when it adds a menu item or
  shortcut. Removing a feature removes its manual text. Mention "Manual: <pages>" (or "Manual: none, no user-visible
  change") in the commit/PR. A release is not cut with a changelog entry that has no manual page behind it.
- **Releases:** follow [`docs/release-process.md`](docs/release-process.md) (steps and known problems).
- **Quality gates (2026-11-10):** *before pushing*, every commit passes `cargo xtask ci --fast` (fmt, clippy
  -D warnings, build incl. wasm check, brand, layers, assets, parity, deny), targeted `cargo test -p <crate>`, and
  `cargo xtask sil`. The full suite runs only in cloud CI on the PR; releases get a manual full-CI dispatch.
  [`docs/ci.md`](docs/ci.md).
- **Commits:** one task id per commit (`M2.3: local Laplacian highlights/shadows`); green states only; end messages
  with the environment's attribution line.

## Third-party sources: licence and derivation policy
One rule (2026-11-10): **use only material you may legally derive from.**
- **In:** licences allowing derivatives + sharing + commercial use — permissive (MIT, BSD, Apache-2.0, ISC, Zlib, …)
  and share-alike (GPL, LGPL, AGPL, CC BY, CC BY-SA, CC0/public domain), open source or not. Read, port, bundle, ship.
- **Out:** CC BY-NC, CC BY-ND, "research only"/"non-commercial", no licence, Adobe's own files (DCP/LCP, presets,
  assets — proprietary).
- Share-alike code in a distributed binary puts the binary under those terms; our dual licence already offers
  GPL-3.0-or-later; `NOTICE` says what shipped.
- **Provenance:** every port names its source — commit **Sources:** line, module docs saying "translated from"/"read
  as a reference" (never "no code copied" when structure follows a source), `NOTICE` credit + licence text next to the
  material.
- **Adobe boundaries (absolute):** never open/disassemble Adobe app bundles (file/symbol names and listings are the
  limit); observing installed Lightroom is read-only (it syncs the user's library — never write); imitating layout and
  behaviour is allowed, Adobe assets never ship; no camera matrices from Adobe-converted DNGs (check `Software` tag;
  camera-native DNGs fine); Adobe-rendered pixels never ship; black-box fits may ship only as aggregate calibration
  numbers (`<cli> calibrate-lightroom add`, allow-listed fields only); users' own `.dcp`/XMP import at run time
  (`camera.importDcp`, `profile.import`) but are never committed, bundled or used as fixtures (tests build bytes).
- **Other apps' black-box measurements (2026-11-17):** response curves measured from a non-Adobe app's output on our
  own synthetic charts (e.g. Capture One midtone tables in `crates/develop/src/levels_table.rs`) may ship as data,
  with the source app and method named in module docs and `NOTICE`. No copied files, assets or rendered photos.
- Now usable (with provenance): darktable/Ansel sources, all of dcraw incl. the GPL RESTRICTED (Foveon) block, LibRaw
  and other dcraw descendants, rawspeed/rawloader/rawler/dnglab/RawTherapee/ART, GPL data tables (noiseprofiles,
  wb_presets, colormatrices, basecurves). Quality habits stay: camera data ranks measured > spectral > dcraw tables
  (`lcc_camdb::Source::Dcraw` via `tools/dcraw_adobe_coeff.py`); decoder layouts verified on CC0 samples; dcraw 1.478
  (2018) predates CR3/Nikon HE/ARQ — no readable reference for those.
- Ports built on share-alike sources are Chromaforge-only, never offered upstream (upstream doesn't share this
  policy).
- Dependencies follow the same rule; `cargo deny` enforces the `deny.toml` allow-list (extend it as licences open up)
  and the `[sources] allow-git` list.
- **Licence (2026-11-10):** workspace default `MIT OR GPL-3.0-or-later` (root `Cargo.toml`; text:
  `LICENSE-GPL-3.0.txt`); share-alike contributions welcome. `crates/segment` + `crates/fetch`:
  `Apache-2.0 OR GPL-3.0-or-later` (`LICENSE-APACHE` per crate). Keep upstream notices in `LICENSE`/`NOTICE`.

## Assets (absolute)
- **No Adobe assets** — icons, screenshots, presets, profiles, LUTs, fonts, even open-licensed Adobe ones (Source
  Sans/Serif/Code, Source Han, Adobe Fonts). Observing Adobe UI to imitate layout/behaviour is fine; copying/tracing
  its assets is not. Sole exception: **Noto Sans/Serif CJK** (Google OFL) via craft-fonts, unmodified.
- Allowed: own original work, CC0/public domain, CC BY/CC BY-SA (attribution honoured), OFL, or any licence permitting
  derivatives + sharing + commercial use (MIT/Apache/BSD/ISC/share-alike).
- **Every asset gets an `assets/ATTRIBUTION.md` entry** (path, title, author, source or "original work", licence,
  date, modifications) in the same commit; `cargo xtask assets` (in `ci`) fails otherwise — never weaken the check.
  Brand art: `brand/icon.svg`/`brand/logo.svg` (original, MIT); code-drawn icons (e.g. `crates/ui-egui/src/icons.rs`)
  are original, recorded there too.
- **Fonts live in [craft-fonts](https://github.com/storytold/craft-fonts), not here** (Inter in `assets/fonts/` is the
  one exception). Build input: `git clone https://github.com/storytold/craft-fonts ../craft-fonts` +
  `CRAFT_FONTS_DIR=../craft-fonts` on any cargo/xtask command; `crates/engine/build.rs` embeds them, read through
  `lcc_engine::craft_fonts()` (wasm32: BIZ UDPGothic only). Without `CRAFT_FONTS_DIR` everything builds/runs but CJK
  glyphs are missing; releases always build with it (`CRAFT_FONTS_REQUIRED=1`) and ship OFL licences; tests needing
  the fonts skip without them. With `CRAFT_FONTS_EMBED=0` the package carries them as files instead of embedding
  them (macOS: `Contents/Resources/fonts/`, one copy for the app and the CLI). Ship fonts unmodified with their
  licence files, credited in `assets/ATTRIBUTION.md`.
- Demo/test images: procedural (`lcc-scenes`) or CC0 in the gitignored `corpus/`. Adobe screenshots live only in
  gitignored `plan/`. When in doubt about a licence: don't use it.

## Running the app
- `cargo xtask run -- --control 7980` = desktop app + JSON-lines control server (`docs/control-protocol.md`).
  Drive it only via the control channel (`ui.key`, `ui.pointer`, `ui.clickWidget`, `ui.menu.invoke`) or headless
  snapshots — **never OS-level synthetic input** (osascript/cliclick/accessibility): it hits whatever window is
  frontmost.
- UI work: look at the result — control-channel script + `ui.screenshot`, compare with `plan/lightroom/screenshots/`.
  Unattended/headless: `cargo run --release -p lcc-cli -- snapshot --demo --script tour.jsonl -o out.png --size
  1600x1000`; `ui.screenshot {"path":…, "headless": true}`.
- MCP: `cargo run -p lcc-cli -- mcp` (`--demo` for procedural library, `--connect` for the running app;
  `docs/mcp.md`). Quick non-UI check: `cargo run -p lcc-cli -- render in.jpg -o out.jpg --set light.exposure=1`.
- Export always goes through `lcc_engine::export`; UI-only commands live in `crates/ui-egui/src/menus.rs`.
- Text boxes: `crates/ui-egui/src/text_field.rs` (`TextField`), not `egui::TextEdit` (context menu, Esc restore,
  edit-end reporting); widget ids unique on screen.
- Shell: `mv`/`cp` are interactive aliases here — use `/bin/mv -f`/`/bin/cp -f`.
- Parallel agents: own git worktree + `CARGO_TARGET_DIR=target/agent-<name>`; own control port (18000–19999, never
  7980); own scratch subfolder; never `rm -rf` shared paths; delete your target dir after; keep every `Cargo.toml`
  valid (`crates/*` glob).
- Corpora: `cargo xtask corpus --download` → gitignored `corpus/` (CC0 only). Shared real-file corpora:
  [photocraft-corpus](https://github.com/storytold/photocraft-corpus), fetched pinned + sha256-verified. Never commit
  media or large fixtures.

## Testing & performance
- Tests check behaviour and invariants, not implementation: no change-detector tests (exact sizes/positions/counts/
  labels), no *exact*-pixel goldens. Reference comparison with numeric tolerance (SSIM/mean ΔE) is allowed and
  encouraged where stored references exist — not yet required anywhere (no reference images yet).
- Pyramid: unit/property tests next to the code (every hostile-input path) → SIL harness (`cargo xtask sil`,
  `docs/testing-sil.md`; pipeline/develop without a window) → e2e driving real binaries (`crates/mcp/tests/it/e2e.rs`,
  `apps/cli/tests/`) → app smoke (control script + `ui.screenshot`). Land at the lowest level that catches the
  regression, plus the level above when behaviour spans layers. Crash fixes always get the hostile-input check.
- After every user-visible change: run the app, screenshot, look; `ui.inspect` → `perf`.
- Measure, don't guess: `<PREFIX>_PROFILE=1` prints per-stage timings to stderr; `/usr/bin/time` for CLI renders;
  record numbers in `plan/STATUS.md` → Metrics. Budgets: slider ≤ 16 ms (draft), loupe ≤ 60 ms @ ~2.5 MP, export
  ≤ 1 s per 24 MP JPEG.
- Perf workflow: measure before (`cargo xtask bench` or `<PREFIX>_PROFILE=1`), change one thing, measure again,
  record both. Bench numbers compare only as same-machine, controlled-environment before/after pairs — never across
  machines or unrelated runs; `cargo xtask bench` (24 MP raw, CPU+GPU columns) flags > 20 % CPU regressions
  (`--strict` fails). No speed-up claims from single noisy runs; complexity must earn its cost.
- Unrunnable tests (no GPU/corpus/fonts/model) call `lcc_testkit::skip!(Gpu, "why")`, not `eprintln!`; `ci` reports
  skips in `target/ci/report.json` + `summary.md`.

## Map of the code
`geom`, `color`, `raster`, `tiff`, `sync` (device-sync model) (L0) → `raw`, `codecs`, `meta`, `develop` (L1) → `pipeline` → `catalog` → `engine`
→ `ui-egui`, `mcp` (L5) → apps `lcc-app` (desktop), `lcc-cli` (render/commands/MCP), `lcc-web`; `brand` carries
`brand.toml`; `scenes` = demo photos; `xtask` = tooling (`ci`, `layers`, `assets`, `parity`, `wasm`, `corpus`,
`stats`, `brand check`, `docs`, `run`, `package`, `install`); `testkit` = test helpers (dev-dep only).
Nix: `nix build` (binaries + fonts + desktop metadata; community-maintained, CI only evaluates it),
`nix develop` = dev shell.

## Contributor credits
- Credits live in `contributors/contributors.json` (generated, never hand-edit) and `contributors/people.toml`
  (the app no longer shows them). See `docs/contributors.md`.
- Agents: if your human's username lacks a `[people.<username>]` entry, ask them once whether they want a real/
  display/synced-GitHub name; if yes, add **only their own** entry (template at the file top) in their PR, committed
  as them. Never add or edit anyone else's entry; never touch `contributors.json`.
- Maintainers refresh stats with `python3 contributors/fork-contributors.py` (`gh` signed in, `upstream` fetched).
