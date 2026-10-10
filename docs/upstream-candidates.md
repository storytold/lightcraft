# Upstream candidates and our diff against upstream (U.5)

Goal: keep the fork's diff in files shared with upstream (everything not `[owned]` in `upstream-owned.txt`) small, so
the weekly upstream merge stays cheap. Fork-only logic lives in owned files; shared files carry one-line call-outs.
Generic fixes and features go upstream as PRs (this list) and disappear from our diff once merged.

## Measurement

Lines of diff (`+` and `-`) between our tree and `upstream/main` (already merged into ours, so this is purely our
divergence), shared paths only:

| | before U.5 | after U.5 |
|---|---|---|
| `git diff --numstat upstream/main HEAD` (raw, renames detected) | 28 259 (code 22 516, locales 4 411, md 1 332) | 18 351 (code 16 697, locales 322, md 1 332), before merging integ/batch4 |
| same, upstream normalised for the crate rename (`lightcraft_*`→`dac_*`, `LightcraftApp`→`DacApp`, app dirs) and re-`rustfmt`ed | **21 310** (code 15 426, locales 4 411, md 1 473) | **11 127** (code 9 332, locales 322, md 1 473) |

The normalised number is the one that predicts merge work: rename-only lines are rewritten by
`cargo xtask rename-crates --upstream`. What was done:

- **Fork-only files in shared crates declared owned** (no upstream counterpart, so they never conflict): Immich,
  credentials and catalog commands, `remote.rs`, fork test files, `legacy.rs` files, brand/packaging tooling.
- **Classic shell out of shared ui-egui files**, each into an owned child module (it sees the parent's private
  helpers, so the parent's signatures stay upstream's):
  - `panels/left.rs` → `panels/classic_left.rs` (Library's column, Catalog rows, disk folders): 416 → 49 lines.
  - `shortcuts.rs` → `keymap_classic.rs` (keymap sets, Classic table, keymap files, module keys, deferred Tab): 251 → 39.
  - `menus.rs` → `menus_fork.rs` (one call-out each in `ui_commands`, `run_ui_command`, `ui_enabled`; the fork's
    command arms): 259 → 124. Table rows stay in place so menu order is unchanged.
  - `lib.rs` → `shell.rs` (the module shell's edges, Tab deferral): 153 → 116.
- **Catalog steps of the apps**: `apps/app/src/catalog_hooks.rs`, `apps/cli/src/catalog_migrate.rs`.
- **Locale catalogs split**: shared `locales/<code>.json` / `<code>-formats.json` now hold upstream's rows only;
  the fork's added and changed messages are in owned `locales/fork/` and are merged over them at load
  (`i18n_fork.rs` for messages, `build.rs` for formats). Upstream rows that still name the legacy product are
  replaced in place (brand check). Remaining locale diff: rows upstream has that we dropped.

## Still divergent, by decision

- **Brand** (product name, env prefix, binary names, icon paths) across apps and docs: required, rewritten per merge.
- **`panels/right.rs`**: upstream's Info panel removed (replaced by owned `panels/metadata.rs`); kept as a deletion.
- **`UiState` / `DacApp` fields of the shell, Immich, Print, Map** (`state.rs`, `lib.rs`): next step would be one
  `#[serde(flatten)]` owned sub-struct, but that renames every access in owned UI files (deferred while P6.3 edits them).
- **`with_default_connections()`** at ~10 session builders in apps/MCP: one call each; could fold into a single
  app-level init hook in `dac-engine` (needs an engine change; small).
- **Engine split** (`engine-export`, `engine-library`, `engine-develop`, `engine-core`): see `plan/upstream.md`.

## UPSTREAM-PR candidates

Generic, no dependency on owned crates; each removes its lines from our diff once upstream takes it.

| # | What | Where | Why upstream wants it |
|---|---|---|---|
| 1 | Export size check / overflow guard (`MAX_OUTPUT_SIDE/PIXELS`, `check_output_size`, `checked_mul`) | `pipeline/src/lib.rs`, `gpu/src/render.rs`, `engine/media.rs` | crash fix (never-crash) |
| 2 | True 1:1 tiled region renderer, zoom ladder 1:4…11:1, `view.zoomLevel` | `ui-egui/region.rs`, `render.rs`, `panels/detail.rs`, `compare.rs`, `state.rs`, `tests_preview_limit.rs`, `preview` | Lightroom parity; high conflict risk, send early |
| 3 | 1:1 preview store, discard, previews at import | `engine/cmd/previews.rs`, `tests_previews.rs`, `preview/disk.rs` | generic (strip the CatalogSettings part) |
| 4 | View paging (`visible_page` / `visible_position`), `as usize` casts | `engine/lib.rs`, `cmd/query.rs` | generic + cast fix |
| 5 | Edit Capture Time from file mtime | `cmd/manage.rs`, `panels/dialogs.rs` | generic feature |
| 6 | Folders panel ops, collection definition export/import, Quick Develop, Keyword List / keyword attributes, Quick Collection / Previous Import | `cmd/folders.rs`, `cmd/collections.rs`, `cmd/quick.rs`, `cmd/keywords.rs`, `panels/{folders,collections,quick_develop}.rs` | check upstream's own keyword panels first (add/add) |
| 7 | Perf U1: keep the developed full-res source across 1:1 region jobs | `engine/media.rs`, `pipeline` | `docs/perf.md` U1 |
| 8 | Perf U2: `library.buildPreviews` in parallel | `cmd/previews.rs` | `docs/perf.md` U2 |
| 9 | Perf U4: export source develop reuse | `engine` export path | `docs/perf.md` U4 |
| 10 | Locale overlay loader (`locales/fork/`-style overlay) | `i18n.rs`, `build.rs` | lets any downstream add strings without touching catalogs |
| 11 | Child-module hooks for shell extension (`left::classic`, `shortcuts::classic`, `menus::fork`) as generic extension points | `left.rs`, `shortcuts.rs`, `menus.rs`, `lib.rs` | would leave zero fork lines in those files |
