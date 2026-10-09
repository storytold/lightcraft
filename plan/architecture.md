# Architecture

Target architecture of the fork, from [PLAN.md §5](../PLAN.md#5-target-architecture-deltas-from-lightcraft). Section
numbers here are cited by tooling: `cargo xtask layers` points to **§3** for the layering rules.

## 1. Principles

- **Pure Rust.** No C/C++ is linked and no foreign code is pasted in. Algorithms come from papers, prose specs or our
  own design (clean-room rules in `AGENTS.md`).
- **Every action is a command.** The engine's command registry is the single API used by the UI, the CLI, the JSON
  control channel and the MCP server. New features land as commands first.
- **Downstream of upstream.** The fork merges upstream regularly (`docs/upstream-merge.md`). Keep upstream crates as
  unchanged as possible and put Classic-only work in **new crates and new UI modules**, so merges stay cheap.
- **One name, one file.** The product name lives only in `brand.toml`; code reads it through the `brand` crate.
- **Never crash.** Malformed input and failures are errors, never panics (`AGENTS.md`, "Never crash").

## 2. Target layers

Packages are named `dac-<name>`; the prefix is an internal codename never shown to users. `NEW` marks crates the
phases add; `(donor)` marks crates ported from the donor project.

```
L0  brand geom color raster tiff sysmem fetch heif
L1  raw codecs meta develop denoise-core denoise faces scenes
    cms (donor)      camdb NEW (per-camera data)      lensdb NEW (lens profiles)
    text (donor)     pdf NEW (multi-page PDF, font embedding, ICC)
L2  pipeline (+ region / tiled full-resolution renders)
    inpaint (donor algorithms: PatchMatch, content-aware fill, GrabCut refinement)
L3  gpu catalog preview export merge segment          catalog → indexed on-disk store
    layout NEW (pages, cells, templates for Print / Book / Slideshow / Web)
    geo NEW (tiles, projections, clustering, GPX, saved locations, geocoding)
    tether NEW (PTP over USB / PTP-IP; folder-watch fallback)
    media NEW (audio for slideshows; video decode/encode)
L4  engine → engine-core, engine-library, engine-develop, engine-export, engine-output, engine-publish
             (engine stays as a thin facade)
    plugins (donor: WASM sandbox, publish/export SDK)
L5  ui-egui (Classic shell: Module trait, panel framework, Classic keymap layer)   mcp
    net NEW (HTTPS client: JSON, multipart, proxies, custom CAs)
    credentials NEW (OS keychain / encrypted file)      immich NEW (client, link, publish, sync)
Apps: `app`, `app-cli` (+ web build), renamed from brand.toml when packaged
```

## 3. Crate layering rules

Enforced by `cargo xtask layers` (`xtask/src/layers.rs`, part of `cargo xtask ci`).

1. A crate may depend only on crates in **lower** layers, or on crates in the **same** layer when an explicit
   intra-layer order allows it (below).
2. Every workspace crate must be registered in the table; an unregistered crate fails the check.
3. **Standalone** crates sit at L0 and may depend on no workspace crate.
4. **testkit** may depend on anything up to L5; other crates may use it only as a dev-dependency.
5. UI toolkits (`egui`, `eframe`, `winit`, `egui_kittest`, `rfd`, `bevy*`) may be used from **L5** up only.
6. Apps and build tooling (`app`, `cli`, `web`, `xtask`) are exempt.

Current table (keep in sync with `TABLE` in `xtask/src/layers.rs`):

| Layer | Crates |
|---|---|
| L0 | brand, geom, color, raster, tiff, sysmem, fetch, heif |
| L1 | denoise-core, denoise, raw, codecs, meta, develop, faces, scenes |
| L2 | pipeline |
| L3 | gpu, catalog, preview, export, merge, segment |
| L4 | engine |
| L5 | ui-egui, mcp |
| testkit | testkit |
| exempt | app, cli, web, xtask |

Allowed orders inside a layer (earlier may be used by later):

| Layer | Order |
|---|---|
| L0 | geom → color → raster; tiff → raster |
| L1 | denoise-core → denoise; meta → raw; meta → codecs; codecs → raw; meta → develop; develop → scenes |

Planned additions (register each in the table in the same change that adds the crate): `cms`, `camdb`, `lensdb`,
`text`, `pdf` at L1; `inpaint` at L2; `layout`, `geo`, `tether`, `media` at L3; the `engine-*` split and `plugins`
at L4; `net`, `credentials`, `immich` at L5 (or lower if they turn out to need no engine types; `net` is a candidate
for L0).

## 4. Data and formats

- **Catalog:** today an operation log plus snapshot held in memory (fine to ~85k photos). Phase 1 moves it to an
  on-disk indexed store behind the same `catalog` API, with lazy photo records, backup with integrity check, and a
  `remote_identity` table that maps photos to Immich assets.
- **Sidecars:** XMP with our own namespace from `brand.toml` `[stable]`, never derived from the product name.
- **Checksums:** SHA-1 of the original file is computed at import (Immich uses SHA-1 for duplicate detection; see
  `plan/immich.md`).

## 5. Upstream strategy

- Merge upstream on a `merge/upstream-YYYYMMDD` branch, run `cargo xtask ci`, fix, merge.
- Crate renames were done once, mechanically, right after the fork; incoming paths are rewritten by tooling.
- Avoid editing hot upstream files for Classic features: add a module or crate and hook it in at one point.
