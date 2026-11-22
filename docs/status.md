# Status

> **Last updated:** 2026-11-23 · **Change:** major (created: tracked replacement for the gitignored `plan/STATUS.md`)

The one tracked place for where Chromaforge stands right now. Agents read it first (see `AGENTS.md`, *Session start*)
and update it in the commit that changes what it says. The detailed phased plan lives in the gitignored
`plan/completion-plan-2026-11-21.md`; this file carries what every contributor needs without it.

## Milestone

**Alpha → beta**, working through the completion plan of 2026-11-21:

| Phase | Goal | State (2026-11-23) |
|---|---|---|
| 0 | Triage and process drift (issues, PR #1292, gap rows, this file) | landing (PR `p0/triage-docs`) |
| 1 | Lightroom migration without silent data loss, Auto Import fixes | landed (#1320, #1321, #1325); leftovers in gap 41 |
| 2 | Never-crash hardening: fuzz targets, checked indexing, allocation caps, SIL round trips | landed (#1328, #1331, #1334, #1337, #1339, #1340); model-reference job in progress |
| 3 | Camera colour, CR3 variants, lens data, per-model verification | next |

Checklist breadth: **90.3%** of 629 in-scope rows (P0 98.0%, P1 94.8%, P2 80.2%), from `cargo xtask parity`.
"Ready for real work" stays an estimate of about 60% (see [target-app-parity.md](target-app-parity.md)).

## Next task

Phase 3: camera colour and raw coverage, starting from the top of [gaps.md](gaps.md) (gap 1 B, camera colour
estimated, not measured; gap 2, CR3 variants; gap 10 B, per-model verification).

## Blockers

- **Hardware:** tethering (gap 17) needs real Canon / Nikon / Sony bodies; Android and peer sync (gap 31) need real
  devices; HDR / VRR on physical monitors (gap 27).
- **Data / licences:** shippable AI weights (gaps 5, 11, 39); SAM 2.1 not yet verified on its real checkpoint
  (LR-MASK-OBJECTS stays 🟡); measured camera profiles need chart shots (gap 1).
- **Dependencies:** `rustls-webpki` 0.102.8 (via `rustls-rustcrypto` in `lcc-fetch`) has no stable 0.103 path; the
  `windows` crate is held at 0.62.2 by hand ([ci.md](ci.md#dependency-updates)).
- **Local test run:** the full `lcc-ui-egui --lib` run crashes locally on Windows; run it filtered with
  `--test-threads=1` and `WGPU_BACKEND=dx12`.

## Upstream intake

- `upstream-intake-2026-11-17` (local branch, no PR): checked 2026-11-23. Its tip `d5ae7373d` is an ancestor of
  `origin/main` and `git log origin/main..upstream-intake-2026-11-17` is empty: every commit already landed. Nothing to
  port; the branch can be deleted locally. The next round starts from latest `main`
  ([upstream-pr-intake.md](upstream-pr-intake.md)).

## Metrics

Same-machine before/after pairs only (`AGENTS.md`, *Testing & performance*). Budgets: slider ≤ 16 ms (draft), loupe
≤ 60 ms at about 2.5 MP, export ≤ 1 s per 24 MP JPEG.

| Date | What | Machine | Before | After |
|---|---|---|---|---|

No rows yet: earlier numbers lived in the untracked `plan/STATUS.md` (scale figures are in [gaps.md](gaps.md), gap 26). Add a row per measurement; keep the method (command, build profile) in the commit message.
