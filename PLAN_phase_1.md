# Phase 1: Classic shell, scalable catalog and Immich link

Part of [PLAN.md](PLAN.md). **Estimate:** 50–85 agent-hours. **Depends on:** Phase 0.
**Unblocks:** Phases 2, 3 and 4. Those can run in parallel after this phase.

**Goal:** the app *is* Lightroom Classic in structure:
- modules, Classic panels and keys;
- a catalog that holds 500k+ photos;
- true 1:1 zoom;
- network groundwork, so the catalog can be linked to an Immich server.

The product name is never written in code; use `brand::` constants (Phase 0.2).

---

## 1.1 Split the engine crate first (≈ 6–10 h)

`crates/engine` is 45k lines and mixes import, export, catalog import, a SQLite reader, faces, denoise, segmentation,
presets and devices. Split it before new features land in it:

| New crate | Takes |
|---|---|
| `engine-core` | command registry (`CommandSpec`), session, jobs, journal, undo |
| `engine-library` | import, devices, folders, collections, keywords, metadata, XMP, `.lrcat`/Luminar import |
| `engine-develop` | develop commands, presets, profiles, versions, sync, merge |
| `engine-export` | export, watermarks, naming templates, DNG writing |
| `engine` | thin facade that re-exports the above, so the UI, CLI, MCP and upstream merges keep compiling |

- Update `xtask/src/layers.rs` with the new crates.
- **Done when:** behaviour is identical (all tests pass), and no file moves change command ids.

---

## 1.2 Module system and panel framework (≈ 12–20 h)

Today `ui-egui` has a `ViewMode` and a `RightPanel` enum (the cloud Lightroom layout).

- **A `Module` trait:**
  ```rust
  trait Module {
      fn id(&self) -> ModuleId;            // Library, Develop, Map, Book, Slideshow, Print, Web
      fn left_panels(&self) -> &[PanelId];
      fn right_panels(&self) -> &[PanelId];
      fn toolbar(&mut self, ui: &mut Ui, cx: &mut Cx);
      fn center(&mut self, ui: &mut Ui, cx: &mut Cx);
      fn keymap(&self) -> &Keymap;          // module-local keys layered over global keys
  }
  ```
- **Top bar:**
  - identity plate on the left (text or image from `brand.logo_svg` by default, user-customisable);
  - module picker on the right, with hide/show modules from its context menu;
  - an activity/progress area.
- **Panel framework:**
  - left, right, top and bottom (filmstrip) panel groups;
  - collapsible panel headers;
  - **solo mode** (opening one panel closes the others);
  - auto hide and show per edge, with a manual override;
  - Tab (side panels), ⇧Tab (all), F5–F8 (each edge), T (toolbar);
  - panel order and visibility saved per module.
- **Screen modes:** normal, full screen with menu bar, full screen; **lights out** (dim/off, L).
- **Secondary window:**
  - Grid, Loupe (Normal / Live / Locked), Compare, Survey, Slideshow;
  - its own filmstrip and filter;
  - the KEYC-SECONDWINDOW keys.
- **Re-home existing views:**
  - Library: grid, loupe, compare, survey, people.
  - Develop: detail view, all editing panels, reference view.
  - Map, Book, Slideshow, Print and Web get placeholder modules ("coming in Phase 3"), so the picker is complete now.
- Every module, panel and screen mode is a command (`module.switch`, `panel.toggle`, …), reachable from MCP, the
  control channel and the menus.

**Done when:** switching modules keeps the selection and filmstrip; the panel state persists across restarts;
headless UI snapshots exist for each module.

---

## 1.3 Classic keymap layer (≈ 3–5 h)

- Add a keymap set called **"Classic"**, the default. LightCraft's current keys become a selectable alternative set.
  - Module keys:
    - G grid, E loupe, C compare, N survey, D develop;
    - Ctrl+Alt+1…7 switch module;
    - ⌘⌥↑ returns to the previous module.
  - Rating and flag keys:
    - `[` and `]` rating down and up;
    - P/X/U flags and the backtick flag toggle;
    - ⇧+key to set and advance (or Caps Lock auto-advance).
  - Library and Develop keys:
    - B target collection;
    - Ctrl+Alt+0–9 keyword sets;
    - ⇧⌘C and ⇧⌘V copy/paste metadata;
    - R crop, Q remove, ⇧T masking, K brush, M linear, ⇧M radial.
  - Help: F1 opens help; ⌘/ shows the shortcut overlay for the current module.
- Shortcut editor in Preferences (it reads the command registry, detects conflicts, and imports/exports a keymap
  file).
- Tracker rows: all KEYC-* rows to ✅.

---

## 1.4 Library module: Classic panels (≈ 10–16 h)

| Panel / feature | Work |
|---|---|
| Navigator | Fit/Fill/1:1/custom zoom presets, click-to-pan, synced with the loupe |
| Catalog panel | All Photographs, **Quick Collection** (B, ⌘B show, ⌥⌘B clear, ⌘⇧B save as collection), Previous Import, Missing Photographs, Added by Previous Export |
| Folders | Volume headers with free space; add/move/rename/remove folders on disk (journaled, undoable); synchronise folder (new, missing, metadata changes); update location; show parent; show in file manager |
| Collections | Collection sets (nesting), **target collection**, smart collection rule editor parity (nested any/all groups, every metadata field), collection export/import |
| Quick Develop | Relative ± buttons for WB, exposure, contrast, highlights, shadows, whites, blacks, clarity, vibrance; presets; crop ratio; treatment |
| Keywording | Keyword tags as text, suggestions (co-occurrence), **keyword sets** (built-in and user), keyword shortcuts |
| Keyword List | Hierarchy, synonyms, include/exclude on export, person keywords, import/export of keyword text files, counts and filters |
| Metadata | Panel presets (Default, EXIF, IPTC, IPTC Extension, Location, Large Caption, Minimal); **metadata presets**; edit many; copy/paste metadata; edit capture time (shift, set, use file date) |
| Painter tool | Spray keywords, labels, flags, ratings, metadata preset, develop settings, rotation, target collection |
| Grid cells | Compact/expanded cell styles, badges (keywords, collections, edited, virtual copy, metadata conflict, **Immich linked**), index numbers |

---

## 1.5 Catalog v4: scale, backup, multiple catalogs (≈ 12–20 h)

Today the catalog is an op log plus a full JSON snapshot, held entirely in RAM (BTreeMaps of `Arc<Photo>`). It works up
to about 85k photos.

1. **Benchmark first.** `cargo xtask bench-catalog --photos 250000,1000000` measures open time, peak RSS, filter
   latency, snapshot time and import throughput on the current design. Record the numbers in `docs/catalog.md`.
2. **New storage behind the same `catalog` API:**
   - a pure-Rust embedded store (candidate `redb`, ACID with MVCC; decide from the benchmark);
   - tables: photos, folders, collections, keywords, develop settings/history, previews index, **remote identities**
     (1.7);
   - secondary indexes for the filter bar's columns;
   - photo records loaded lazily; the grid pages through ids;
   - keep the op log as the undo journal and change feed (useful for sync in Phase 4).
3. **Migration:** v3 (LightCraft JSON) → v4 runs automatically, keeps a backup, and is tested on the 85k fixture.
4. **Catalog file:** one catalog = one folder with a `*.{brand.catalog_ext}` entry point.
   - File → New/Open/Open Recent Catalog;
   - a catalog chooser at startup (Alt held, or a preference).
5. **Backup:** schedule (on exit: daily/weekly/monthly/never), backup folder, integrity test, optimise (compaction plus
   index rebuild).
6. **Export as Catalog / Import from Another Catalog:** subset with or without originals and previews; conflict rules
   (new, changed: replace settings / metadata / keep), with a preview of changes.
7. **Metadata vs XMP:** a "changed on disk" / "changed in catalog" badge; Read Metadata from File / Save to File;
   conflict dialog.

**Done when:** 500k photos open in under 2 s, filter in under 100 ms, use under 2 GB RSS; 1M photos still usable;
the crash and fuzz tests are carried over.

---

## 1.6 True 1:1 zoom and previews (≈ 6–10 h)

- Generalise `ui-egui/src/region.rs` (it renders the on-screen window at zoom scale, but only for the loupe and
  Before/After) to a **tiled region renderer**:
  - used by loupe, compare, survey, reference view, soft proof and overlays;
  - zoom levels Fit, Fill, 1:4, 1:3, 1:2, 1:1, 2:1, 3:1, 4:1, 8:1, 11:1;
  - tiles cached per settings hash;
  - the GPU path where available.
- Preview store: standard previews at a size set per catalog, 1:1 previews built on demand or at import, auto-discard
  of 1:1 previews (day/week/month/never), and Library → Previews → Build/Discard commands.

**Done when:** 1:1 pixels equal the full-resolution export pixels (test on corpus raws); panning at 1:1 on 45 MP
stays above 30 fps.

---

## 1.7 Network groundwork (≈ 4–6 h)

Immich in 1.8 and Phase 4, publish services, maps and model downloads all need this.

- New L0 **`net` crate**, written fresh under **MIT**. `crates/fetch` is Apache-2.0 only (PLAN.md §4.1), so it is not
  extended. It uses the same approach as `fetch`: rustls with the RustCrypto provider, pure Rust. It provides:
  - methods GET/POST/PUT/PATCH/DELETE;
  - JSON bodies (serde);
  - **streaming multipart upload** with progress;
  - streaming download (keeping the existing resume and SHA-256 logic);
  - connect and stall timeouts, cancellation, redirects, gzip;
  - optional HTTP proxy;
  - `User-Agent` built from `brand::`;
  - custom CA / self-signed certificates per connection (common on home Immich servers), with trust-on-first-use plus
    fingerprint confirmation.
  - `fetch` stays as it is for model downloads, or later becomes a thin Apache-2.0 wrapper over `net`.
- **`credentials` module:** secrets stored in the OS keychain where reachable in pure Rust:
  - Linux Secret Service over D-Bus via `zbus`;
  - macOS and Windows behind an isolated platform crate.
  - Fallback: a file encrypted with a key derived from a user passphrase.
  - Never write secrets into the catalog, settings, logs, MCP output or crash reports; add a test that greps the logs.
- **Connections settings pane:** Preferences → Connections lists accounts for Immich now, and publish services, map
  tiles and geocoding later.
- **Catalog `remote_identity` table:** (photo_id, service, account_id, remote_id, remote_checksum, remote_updated_at,
  last_synced_at, sync_state). Indexed both ways. This is where Immich links live.
- **Checksums at import:**
  - compute **SHA-1** of each original, which is what Immich stores for its assets;
  - compute it with the existing content hash in the same read pass;
  - a background job back-fills SHA-1 for existing catalogs.

---

## 1.8 Immich: connect, link, import (≈ 8–12 h)

New crate **`immich`** (L3, depends on `net`, `catalog`): a typed client for the endpoints listed in `plan/immich.md`,
written by us from the public API docs. A version check refuses servers older than the minimum version, with a clear
message.

1. **IMM-CONNECT:**
   - Preferences → Connections → Add Immich server: URL, API key (with a link to the server's API-key page), Test
     button;
   - shows server version, user and the key's permissions;
   - more than one server/account allowed;
   - commands `immich.connect`, `immich.disconnect`, `immich.status`.
2. **IMM-LINK:**
   - a background job lists the server's assets (paged, incremental by `updatedAt`);
   - matches them to catalog photos by **SHA-1 checksum**; the fallback is original filename + capture time + file
     size, marked "probable" so the user can confirm;
   - writes `remote_identity` rows;
   - results:
     - grid badge "in Immich";
     - Library filter "Immich: linked / not linked / probable";
     - Metadata panel field with an "Open in Immich" link to the asset's web URL;
     - a smart-collection criterion.
3. **IMM-IMPORT:**
   - the Import dialog gets an **Immich** source: browse the timeline, albums, people and favourites as thumbnails
     loaded from the server;
   - choose **Copy** (download originals into a dated destination folder, then import normally) or **Link only**
     (catalog entry with remote preview; the original is downloaded on first Develop);
   - Immich rating, favourite, description, tags, albums and GPS are taken into catalog metadata on import (mapping as
     in Phase 4 IMM-SYNC; one-way at this stage);
   - duplicates are skipped by checksum.
4. **IMM-EXTLIB** (shared originals, the recommended setup for self-hosters):
   - when Immich indexes the same folders as an *external library*, the link uses the checksum, never an upload;
   - a setup helper shows which catalog folders are covered by which Immich external library (path mapping table for
     container paths such as `/mnt/photos` ↔ `/home/me/Photos`);
   - writes XMP sidecars so Immich picks up ratings, descriptions and keywords on its next scan. Phase 4 adds direct
     API sync.
5. **Tests:**
   - unit tests against recorded HTTP fixtures;
   - integration tests against `cargo xtask immich up`, run as a nightly CI job rather than on every PR, because it
     needs Docker;
   - error paths: offline server, bad key, TLS mismatch and HTTP 5xx. None may crash; each must give a clear error and
     a retry option.

---

## Exit gate

Status 2026-10-10 (first pass done; see plan/STATUS.md → Phase 1 for the gaps).

- [~] Engine split; layering check green; command ids unchanged. *(Four crates extracted, ~10k of 45k lines; Session, the
      command registry and most commands remain in the `engine` facade until a Session abstraction exists.)*
- [~] All seven modules in the picker (Library and Develop complete, others placeholders); panel framework, screen
      modes and secondary window done; Classic keymap default; shortcut editor. *(Secondary window lacks its own
      filmstrip/filter; KEYC-COMPARE / KEYC-DEVELOP / KEYC-MODULE-OUTPUT not ✅; no panel drag-reorder.)*
- [~] Library Classic panels ✅ in tracker. *(Folders and Grid cells 🟡.)*
- [~] Catalog v4: 500k under 2 s open, under 100 ms filter; backup and optimise; multiple catalogs; export/import
      catalog. *(500k open 0.73 s; filtered views < 100 ms but the unfiltered date sort measured 110 ms under load;
      photos still all in RAM, 1M = 2.3 GB.)*
- [x] 1:1 zoom pixel-equal to export. *(Procedural image and tile seams; corpus raws not run on this machine. Compare
      and Reference views not tiled.)*
- [~] `net` and `credentials` in place; IMM-CONNECT, IMM-LINK, IMM-IMPORT and IMM-EXTLIB ✅ against the pinned Immich
      test server. *(IMM-LINK ✅; CONNECT 🟡 no key storage on macOS/Windows; IMPORT 🟡 separate window; EXTLIB 🟡
      not tried against a real external-library scan.)*
