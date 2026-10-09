# Execution plan

Summary of [PLAN.md §6](../PLAN.md#6-plan-to-completion). The detailed phase plans are copied here
([phase-0.md](phase-0.md) … [phase-4.md](phase-4.md)); the root `PLAN_phase_*.md` files are the working copies.
Estimates are agent-hours and show relative size; every phase ends with an exit gate checked by tests or
measurement.

| Phase | Scope | Estimate | Depends on | Detail |
|---|---|---|---|---|
| 0 | Fork, brand config (`brand.toml`), MIT licence, `plan/`, tracker with Classic and Immich rows, Immich test server | 12–22 h | — | [phase-0.md](phase-0.md) |
| 1 | Classic shell (modules, panels, keymap), catalog v4 (500k+ photos), engine split, full-resolution zoom, `net` / `credentials`, Immich connect / link / import / external library | 50–85 h | 0 | [phase-1.md](phase-1.md) |
| 2 | Image quality: `camdb`, lens profiles, CR3 and other raws, profiled noise reduction, `cms` | 110–180 h | 1 | [phase-2.md](phase-2.md) |
| 3 | Output modules: shared text / PDF / layout, Print, Map, Slideshow, Book, Web; Immich shared links | 72–124 h | 1 | [phase-3.md](phase-3.md) |
| 4 | Workflow: tethering, publish services, plugins, round trips, actions, people, catalog migration; Immich publish, two-way sync, people, search, MCP | 60–105 h | 1 | [phase-4.md](phase-4.md) |
| 5 | AI masks, HDR editing and output, video | 100–200 h | 1–2 | PLAN.md §6 |
| 6 | 1.0 hardening: performance, fuzzing, packaging, docs | 30–50 h | all | PLAN.md §6 |

Phases 2, 3 and 4 run in parallel after Phase 1.

## Tracker mapping

| Phase | Tracker rows it closes (docs/parity.md) |
|---|---|
| 0 | none directly; adds the rows (KEYC-MODULES and LRC-WEB-* / LRC-MAP-REVGEO reopened, LRC-SHELL-* / LRC-CAT-* and others added, IMM-*) |
| 1 | KEYC-MODULES, KEYC-PANELS, KEYC-VIEWS, LRC-SHELL-*, LRC-CAT-*, LR-VIEW-ZOOM, IMM-CONNECT, IMM-LINK, IMM-IMPORT, IMM-EXTLIB |
| 2 | LR-PROF-CAMERACOLOR, LR-IMP-FORMATS, LR-IMP-CAMERA-COVERAGE, LR-EDIT-OPTICS-PROFILE, LRC-DEV-SOFTPROOF |
| 3 | LRC-MAP-*, LRC-BOOK-*, LRC-SS-*, LRC-PRINT-*, LRC-WEB-*, KEYC-MODULE-OUTPUT, IMM-SHARELINK |
| 4 | LRC-LIB-TETHER, LRC-LIB-PUBLISH, LRC-SHELL-PLUGINS, LRC-AUTO-ACTIONS, LRC-LIB-PEOPLE, IMM-PUBLISH, IMM-SYNC, IMM-PEOPLE, IMM-SEARCH, IMM-MCP |
| 5 | Q. HDR, R. Video, P. Enhance, AI rows in K. Masking |

## Working rules

- Pick work from the tracker's "Top gaps"; update the row in the same commit that lands the feature.
- `cargo xtask ci` must be green before merging; `cargo xtask parity --write` refreshes the summary.
- Product name only via `brand::`; prose says "the app".
- Clean-room rules (PLAN.md §4.2) apply to every phase.
