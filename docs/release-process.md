# Release process

How a Chromaforge release goes from `main` to a published GitHub release, and the problems earlier releases hit.
Written after 26.46.2; keep it current. CI mechanics: [`ci.md`](ci.md). Changelog entries:
[`changelog/unreleased/README.md`](changelog/unreleased/README.md).

## Steps

1. **Prep branch** `prep/release-<ver>` from latest `main`: bump `version` in the root `Cargo.toml` (and `Cargo.lock`),
   run `cargo xtask notes --release <ver>` (gathers `docs/changelog/unreleased/*` into `docs/changelog/<ver>.md`),
   update the README via `cargo xtask docs` if it names the version. Commit `Release <ver>: changelog and version`.
2. **Local checks before the long run** (minutes, not hours): `cargo xtask ci --fast`, then
   `cargo test -p lcc-engine --lib` and
   `cargo test -p lcc-ui-egui --features lcc-engine/sam,lcc-engine/own-model,lcc-engine/rawnind-model`. The full
   suite is the only place stale tests of changes merged days earlier show up; most reproduce locally in seconds.
3. **Open the prep PR** to `main` and **dispatch the full CI** on the prep branch (`gh workflow run CI --ref
   prep/release-<ver>`, about 2.5 h). All three OSes must be green apart from the known Windows noise below.
4. **Fix failures on a `fix/release-<ver>-ci` branch** merged to `main` (re-merge `origin/main` first), never on `release`.
5. **main → release:** branch `prep/release-<ver>-to-release` from `origin/release`, merge `origin/main`, PR to
   `release`, merge with `gh pr merge --admin --merge` (the ruleset wants a non-author approval; admins bypass).
6. **Draft notes:** the workflow seeds the draft with generated notes. While the build runs, once the draft
   exists: `gh release edit v<ver> --notes-file docs/changelog/<ver>.md`. The final job reads the draft body for
   `latest.json` (the updater's notes), so do it before that job.
7. **Publish** when the Release workflow succeeded and the draft has all assets (19 in 26.46.2; compare with the
   previous release): `gh release edit v<ver> --draft=false --latest`.
8. **Clean up** local prep branches, finished agents' worktrees and scratch/target dirs.

## Known problems

- **Full CI only runs on dispatch**, so a behaviour change merged without updating the tests that call it stays
  hidden until release day. A change to a command's semantics (for instance `panel.presets` becoming a toggle) must
  run the tests that use that command in the same PR.
- **Windows CLI/MCP tests fail with Vulkan validation text on stdout** (`contact_sheet_exports_pdf…`,
  `mcp_over_stdio_…`, `run_subcommand_chains…`, `transparent_png_export…`, `tether_cull_develop_publish_sync_print`,
  plus an abort in `tests_i18n_sweep`). The validation layer rejects naga's strided arrays in Function/Workgroup
  storage (`hist.wgsl` `local_bins`, `blur.wgsl` median `v`, `finish.wgsl` `band_weights`). They were red for
  26.46.1 and 26.46.2 and shipped anyway; fix the shaders or quarantine the tests rather than re-diagnosing.
- **`exports_are_atomic_without_a_sync`** counts syncs on the exporting thread, so any other durable write on that
  thread (a cache, a baseline) fails it. Recreatable caches use `write_atomic_nosync`.
- **Tests that press a toggling command** (`panel.presets`) must first put the panel in the state they expect.
- **Never `pkill -f <run id>`** from the agent shell: it kills the shell itself. Stop background tasks properly.
- `gh pr merge --auto` on a PR into `release` waits for an approval; use `--admin`.
- `[skip ci]` PRs bundled into another PR have no changelog entries; cross-check the generated PR list.
