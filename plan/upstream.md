# Upstream-first policy

*Decided 2026-10-10.*

The app is a fork of LightCraft (see `NOTICE`). Upstream moves fast: 381 commits landed in the first day after the
fork. We want to **pull everything of value from upstream indefinitely**, and to **send our own improvements back
upstream quickly**. Both only work if the code we share stays close to upstream's.

## The rule

1. **Shared crates are frozen until the public release.**
   - "Shared" means every crate inherited from upstream that we don't own (list below).
   - Phase work does not change them, even when a phase task asks for it. Those tasks wait for the post-release
     upstream track (below), or are rebuilt as a new crate or a hook in a crate we own.
2. **Exceptions are upstream-first and timely.**
   - A crash, a data-loss bug, or a small hook we can't work around may touch a shared crate.
   - Make the change on its own branch off `upstream/main`, as a minimal patch in upstream's style, and open a PR
     there within days.
   - The same commit goes into our tree, tagged `UPSTREAM-PR: <link>` in the commit message, and drops out at the
     next merge once upstream has taken it.
3. **New work goes in crates we own.**
   - Either new crates (`net`, `credentials`, `hash`, `immich`, later `layout`, `pdf`, `text`, `tether`, …) or
     the owned paths below.
   - A new crate that is useful in general (such as `pdf`) is written so it can be offered upstream as is: no brand
     names, no dependency on our owned crates.
4. **Merge upstream weekly** with the routine in `docs/upstream-merge.md`, which keeps our version of owned paths and
   reports what upstream changed in them, for a quick review of anything worth porting by hand.
5. **After the public release**, work on shared crates resumes, **upstream-first**: each change is a PR to upstream,
   developed on a branch off `upstream/main` and merged back to us through the normal upstream merge. Phase 2
   (camera data, raw decoders, lens corrections, detail, Remove) and the HDR, AI and video parts of Phase 5 are
   done this way.

## What we own (we diverge here on purpose)

| Path | Why it's ours |
|---|---|
| `brand.toml`, `crates/brand`, `brand/` | the fork's identity |
| `crates/catalog` | catalog v4 (redb store, multiple catalogs, transfer); upstream fixes are reviewed and ported by hand |
| `crates/net`, `crates/credentials`, `crates/hash`, `crates/immich` | new crates (Immich, network, key storage) |
| `crates/ui-egui`: `module.rs`, `panels/classic.rs`, `navigator.rs`, `metadata.rs`, `cells.rs`, `libtools.rs`, `connections.rs`, `second.rs`, `plate.rs`, `catalog_ui.rs` | the Classic shell: modules, panels, keymap, identity plate, secondary window |
| `docs/catalog.md`, `docs/immich.md`, `plan/`, `PLAN*.md` | our docs and plans |
| `xtask/` brand, immich and bench-catalog commands | our tooling |

Everything else is **shared**: `geom`, `color`, `raster`, `tiff`, `raw`, `codecs`, `meta`, `develop`, `pipeline`,
`gpu`, `preview`, `denoise*`, `segment`, `faces`, `merge`, `fetch`, `engine` (and the `engine-*` crates as upstream
has them), `mcp`, the rest of `ui-egui`, and the apps.

Upstream changes to owned paths are not merged. They show up in the review report, and anything valuable (usually a
bug fix) is ported by hand.

Files both sides edit that can't be owned (`docs/parity.md`, locale catalogs, `Cargo.lock`) are regenerated or merged
row by row; see `docs/upstream-merge.md`.

## Phase 1 changes to shared crates (to settle before the first merge)

Phase 1 changed shared crates before this policy existed. Before the first upstream merge, sort each change into one
of three groups:
- **send upstream as a PR:** crash fixes, the export size check, true 1:1 region rendering, view paging;
- **move into an owned crate or hook:** catalog and Immich commands;
- **revert:** anything that only exists for the Classic shell and can live in `ui-egui` owned files.

The **engine split (task 1.1)** is the largest of these changes. It is finished on branch
`worktree-agent-abf3c789693fc3a16` (8 commits, CI green) and is **not merged**, because it moves about 30k lines out
of upstream's `engine` crate. It is offered upstream as a proposal instead. If upstream takes it, we get it through
the merge; if not, the branch stays parked.

## Tooling (task U.1, before the first merge)

- `cargo xtask upstream-merge`: fetch, merge on `merge/upstream-YYYYMMDD`, restore owned paths, run
  `rename-crates --upstream` and the brand fixes, write the review report (upstream commits touching owned paths,
  with subjects and diff sizes), run `ci`.
- `git config rerere.enabled true` so repeated conflict resolutions replay.
- `cargo xtask upstream-pr <commits>`: replay commits onto a branch off `upstream/main`, map our crate and brand
  names back to upstream's, and push it ready for a PR.
- A check in `ci` that lists changes to shared paths since the last upstream merge that have no `UPSTREAM-PR:` tag.
