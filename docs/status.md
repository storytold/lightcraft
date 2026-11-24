# Status

> **Last updated:** 2026-11-25 · **Change:** patch (Metrics: Export screen preview render); minor (HDR export: gain map AVIF and JPEG XL, `hdrMode: gainMap`; reader side shows the SDR base); patch (Metrics: 24 MP HDR export encodes); patch (Metrics: first `cargo xtask bench` budget run, 4.4); patch (Metrics: 100k-grid wheel-scroll frame, plan 4.8); major (created: tracked replacement for the gitignored `plan/STATUS.md`); minor (Metrics: draft NR drag, task 4.5); minor (Metrics: CPU-only stages, plan 4.6)

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

Checklist breadth: **90.3%** of 630 in-scope rows (P0 98.0%, P1 94.8%, P2 80.3%), from `cargo xtask parity`.
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
| 2026-11-25 | Export screen preview (release CLI `snapshot --demo`, headless CPU render, demo photo 24 MP, `<PREFIX>_PROFILE=1`, single runs, machine shared with builds) | CachyOS Linux x64 desktop | no preview | screen opens without it: `dialog.export` 25 ms; the preview settles 325 ms after opening (150 ms debounce + render + fit); `export render` on thread `lc-export-preview` 46 ms at 2048 px long edge (decode 0.0 ms, source cached); a second run of the same settings 209 ms (noisy); HDR mode on an SDR display re-renders nothing (settle 44 ms) |
| 2026-11-24 | 4.1 start-up, 100 000-photo synthetic library (`startup_bench`, release, 3 interleaved runs each) | Windows 11 dev box, shared with other builds (wall times ±100 %) | background-preparation tick (slowest of first 30) 12.1 / 17.6 / 18.6 ms; library ready 0.99–3.9 s; slowest frame 0.52–3.3 s | tick 22.9\* / 4.0 / 4.2 ms; library ready 1.0–8.2 s; slowest frame 0.59–4.7 s (\*first pair under the heaviest load). Tick bounded at 16 384 photos looked at; the multi-second frames are the session open, not preparation |
| 2026-11-25 | 4.1 session open off the interface thread (`startup_bench`, 100 000 photos, release, 3 interleaved before/after runs, `<PREFIX>_PROFILE=1`) | Windows 11 dev box, heavily loaded by ~10 parallel builds (wall times ±100 %) | slowest frame 4.30 / 3.92 / 3.31 s (open on the interface thread: catalog read 2.9–3.7 s + first view 0.2–3.5 s) | slowest frame 0.65 / 0.36 / 0.89 s; the open's own frame 0.34 s (catalog 0 ms: read and first view sorted on the worker). Library ready 5.9–11.6 s both ways (noise); the first frame's seconds are not the open |
| 2026-11-24 | 4.4: first `cargo xtask bench --save-baseline` (render a7m3 24 MP, grid 100k, TIFF ZIP, keyword tree; release) — baseline, no before | Windows 11, heavily loaded by concurrent builds | — | GPU draft drag 3.9–4.1 ms, GPU loupe exposure 13 ms, CPU draft NR 519 ms (load), grid frame 13.2 ms, TIFF ZIP 960 ms, kept keyword tree 0.6 ms; table in perf.md |
| 2026-11-24 | Grid wheel-scroll frame, 100k photos, 1600×1000 (`wheel_scroll_100k_frame`, release, median / p95 of 120 frames, 3 runs) | Windows 11 x64 desktop, shared with other builds | 3.3–5.3 / 5.1–7.5 ms (grid panel 0.8–1.4 ms) | unchanged: a prefetch-row shortcut measured 3.7–5.3 / 6.4–21 ms and was dropped |
| 2026-11-24 | 4.5: draft Luminance NR drag, CPU (`ONLY=drag N=9 render_bench`, release, `<PREFIX>_GPU=0`, a7m3 24 MP, draft 1152×768); paired runs of both binaries alternated, median of 4 | Windows 11, shared with other build agents (loaded; no process CPU time on Windows) | 63.8 ms wall (60.5–66.4); single thread 280 ms (268–506) | **33.4 ms** (31.9–34.6); single thread 200 ms (168–259). Still over the 16 ms budget on this loaded machine |
| 2026-11-24 | CPU-only stages at 1920×1280 (4.6, `cpu_only_stage_cost`, release): device proof / Even Out | Windows, 16 cores, heavily loaded | plain CPU render 74 ms | device proof +6–12 ms; Even Out spot 34 ms (size 0.05), 223 ms (size 0.20); see perf.md |

| 2026-11-24 | HDR export of a 6000×4000 synthetic image, quality 90 (gain-map paths: `encode_gain_map_file`; single-layer: `encode_deep`; release, 2 rounds) | Linux x64, AMD RYZEN AI MAX+ PRO 395 w/ Radeon 8060S, 32 threads | — (the single-layer PQ paths' code is unchanged by the gain map export; measured here, not against a main build) | AVIF PQ 27.5 / 27.1 s, 6.82 MB; JPEG XL PQ 1.65 / 1.77 s, 6.49 MB; gain map JPEG 125 / 149 ms, 25.3 MB (noise-like image); **new:** AVIF gain map 10.1 / 10.9 s, 9.82 MB; JPEG XL gain map 0.62 / 0.74 s, 5.59 MB |

Earlier numbers lived in the untracked `plan/STATUS.md` (scale figures are in [gaps.md](gaps.md), gap 26). Add a row per measurement; keep the method (command, build profile) in the commit message.
