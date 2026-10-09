# Phase 4: Workflow power features and full Immich integration

Part of [PLAN.md](PLAN.md). **Estimate:** 60–105 agent-hours (including about 20–35 h of Immich work).
**Depends on:** Phase 1 (catalog v4, `net`, `credentials`, the `immich` crate and links).
**Runs in parallel with:** Phases 2 and 3.

**Goal:**
- the workflow features power users rely on: tethering, publish services, plugins, round trips, actions, people and
  catalog migration;
- **two-way integration with Immich**, so the desktop catalog and a self-hosted Immich server stay one library.

---

## 4.1 Tethered capture (≈ 10–16 h)

1. **Studio capture first** (the design described in Ansel's prose note `doc/studio-capture.md`): watch a folder written by any vendor tool or the camera's own app;
   auto-import with:
   - a develop preset;
   - a metadata preset and keywords;
   - a session name and naming template;
   - a target collection.
   The newest shot shows in the loupe automatically.
2. **Native tethering**, new crate `tether`:
   - PTP over USB with `nusb` (pure Rust), plus PTP/IP over Wi-Fi;
   - vendor extensions for Canon, Nikon, Sony and Fujifilm, in that order;
   - capture, download-on-capture, delete-from-card option.
3. **Tether bar** (as in Classic): camera name, shutter, aperture, ISO and WB readouts and settings; capture button;
   develop settings "same as previous"; **live view** where the camera supports it.
4. Platform notes:
   - Linux needs a udev rule (shipped in packages).
   - macOS: release the camera from `ptpcamerad`.
   - Windows: WinUSB driver notes.
   - Unsupported cameras fall back to studio capture with a clear message.

**Done when:** studio capture works with any camera; native capture is verified on at least one Canon, Nikon and Sony
body; CI uses a simulated PTP device.

---

## 4.2 Publish services framework (≈ 8–12 h)

- **Model:**
  - a *publish service* (type plus account plus export settings) contains **published collections** and sets;
  - each photo has a state: *new to publish*, *modified to re-publish*, *published*, *deleted to remove*;
  - remote ids are stored in `remote_identity` (Phase 1.7);
  - the modified state comes from the develop/metadata change feed (the catalog op log).
- **UI:**
  - Publish Services panel in Library;
  - the Publish button;
  - the grid split into New / Modified / Published sections;
  - "Mark as Up-to-date";
  - remote comments and likes where the service supports them.
  - This closes LRC-LIB-COMMENTS for services that have them.
- **Built-in services:**
  - **Hard Drive** (export to a folder kept in sync);
  - **Immich** (4.6);
  - **SFTP** (reuses Phase 3.6).
- Other services (Flickr, SmugMug, Piwigo, Nextcloud, PhotoPrism, WebDAV) come as **plugins** (4.3).

---

## 4.3 Plugin SDK (≈ 10–16 h)

- **Sandbox:** port PhotoCraft's `plugins` crate (`wasmi`, no host imports by default, fuel and memory limits).
- **Host API, versioned and capability-gated:**
  - catalog query (read);
  - metadata read/write in a plugin-owned namespace, plus standard fields with permission;
  - **export post-process hooks**;
  - **publish service provider interface** (the same trait the built-in services implement);
  - metadata/keyword providers;
  - menu commands that register as engine commands;
  - simple dialogs from a declarative UI description.
- **Permissions:**
  - network per host;
  - filesystem roots;
  - granted on install and revocable in the Plugin Manager.
- **Plugin Manager:** install, enable, disable, update, view logs.
- **SDK:** Rust crate plus template, docs and two example plugins (a WebDAV publisher and a metadata reverse-lookup).
- Lightroom's Lua plugins are not supported, and no promise is made to support them.

---

## 4.4 Edit In and the PhotoCraft round trip (≈ 5–8 h)

- External editor presets (app, file format, colour space, bit depth, resolution, compression, stack with original,
  naming).
- **PhotoCraft integration:**
  - "Edit in PhotoCraft" sends TIFF or **layered PSD** using PhotoCraft's `psd` writer (ported). "Open as Layers"
    builds a layered PSD from the selection.
  - When the file is saved, it is re-imported and stacked with the original; the edit-in round trip keeps metadata.
  - If PhotoCraft runs with `--control`, the two apps can talk directly over the control channel: open, and notify on
    save.
- PSD/PSB **import** uses the PhotoCraft `psd` reader (flattened composite plus layer metadata).

---

## 4.5 Actions, people, migration (≈ 10–18 h)

- **Actions:**
  - record a sequence of engine commands (built on PhotoCraft's `actions_cmds` design and our journal);
  - parameterise it, save it, assign a shortcut, run it on a selection;
  - also exposed through MCP and the CLI (`app-cli run-action`).
- **People:**
  - face clusters with separators and confidence;
  - merge and split;
  - small-face detection on large images (tiled detection beyond 640 px);
  - **write face regions to XMP** (MWG Regions) and read them back;
  - person keywords.
- **`.lrcat` migration v2:**
  - replay develop history and snapshots;
  - a profile mapping table (Adobe profile names → our camera-matching/creative profiles from Phase 2.3);
  - masks (brush, linear, radial, range) and spot/heal;
  - a per-photo **fidelity report** listing what could not be mapped;
  - publish-service collections imported as plain collections.
  - Mapping is derived from the published XMP `crs:` field descriptions and from our own measurements: apply known
    values in Lightroom, export, compare (the local fidelity references of Phase 2.8). darktable's importer is
    **not** read (MIT clean-room).

---

## 4.6 Immich: full two-way integration (≈ 20–35 h)

Builds on Phase 1.8: connection, link by checksum, import, shared originals. Every Immich action is a command
(`immich.*`), so it works from the UI, CLI, control channel and MCP (**IMM-MCP**).

### 4.6.1 IMM-PUBLISH: Immich as a publish service

- An **"Immich" publish service** per connected account. Published collections map 1:1 to **Immich albums** (created
  if missing, renamed if the collection is renamed). Collection sets map to album name prefixes.
- **What is sent**, per service:
  1. **Rendered** JPEG/AVIF/JXL using the export settings, so Immich shows the edited look; or
  2. **Original** (plus XMP sidecar); or
  3. **Both, stacked in Immich**, with the render on top and the original underneath, using Immich's stack feature.
- **Upload:**
  - streaming multipart upload with progress and resume-on-failure (via `net`);
  - a stable device asset id (catalog photo id plus virtual-copy id);
  - **checksum pre-check before upload**, so Immich never stores a duplicate; already-present assets are just linked
    and added to the album.
- **Re-publish:** edits mark photos *modified*; re-publishing replaces the rendered asset in place (keeping its id,
  albums and favourites where the API allows), otherwise uploads a new asset, moves album membership and trashes the
  old one.
- **Removal:** removing from a published collection removes the photo from the album. Deleting the Immich asset is a
  separate, explicit option (default **off**); assets go to the Immich trash, never a hard delete.
- **Shared-originals mode** (IMM-EXTLIB): for originals already in an Immich external library, publish only changes
  album membership and metadata; nothing is uploaded except renders, if chosen.

### 4.6.2 IMM-SYNC: two-way metadata sync

- **Per-field opt-in mapping**, with defaults:

| Catalog | Immich | Default |
|---|---|---|
| Star rating 0–5 | asset rating | two-way |
| Pick flag | favourite | two-way (configurable: favourite ↔ pick, or favourite ↔ 5★, or off) |
| Reject flag | archived | off (configurable) |
| Title / Caption | description | two-way (caption ↔ description; title optional) |
| Keywords (hierarchical) | tags (hierarchical, `A/B/C`) | two-way; export-excluded keywords never sent |
| Collections | albums | two-way for linked collections only |
| GPS, capture date/time | asset location and date | two-way, with a conflict prompt |
| People names / face regions | people / faces | see IMM-PEOPLE |
| Deleted photo | trashed asset | **never automatic**; a queue for the user to confirm |

- **Change detection:**
  - local changes come from the catalog op log since `last_synced_at`;
  - remote changes come from incremental queries (`updatedAfter` or the server's sync stream, depending on the pinned
    Immich API version in `plan/immich.md`).
- **Conflicts** (both sides changed since the last sync):
  - a per-field policy: catalog wins, Immich wins, or newest wins (the default);
  - a Sync Conflicts view that shows both values for anything unresolved.
- **Scheduling:** manual, on publish, or a background interval. It runs offline-safe: queued and retried with
  backoff, and it never blocks the UI.
- Activity log entries for every sync run; a dry-run mode shows what would change.

### 4.6.3 IMM-PEOPLE

- Import Immich **people** (names, birthdates, hidden flag) and **face regions** (bounding boxes) for linked assets
  into the People view. They are marked "from Immich" and can be confirmed or corrected.
- Push names assigned in the app back to Immich people (opt-in); merges map to Immich person merges.
- This gives good face recognition **before Phase 5**, by reusing Immich's own machine learning.

### 4.6.4 IMM-SEARCH

- The Library filter bar and the Text filter get an **"Immich smart search"** mode: a natural-language query (Immich's
  CLIP search) returns asset ids, which are mapped to linked catalog photos. The results show as a temporary
  collection that can be saved as a collection.
- Also: metadata search (OCR text, objects, places) when the server supports it.
- This stands in for local AI search (FILT-SEARCH-AI) until Phase 5, and can coexist with it later.

### 4.6.5 Hardening and tests

- **Version policy:** support the pinned minimum Immich release and the latest release. A nightly CI job runs
  integration tests against both through `cargo xtask immich up --version …`.
- **Scale:** sync of 100k assets completes, resumes after interruption and stays under 5% CPU when idle.
- **Failure:**
  - server down, key revoked, permission missing, TLS change: each gives a clear error, nothing is lost, and work is
    queued;
  - never crash, as the never-crash standard requires; fuzz the JSON decoding of server responses.
- **Privacy:** a "private" saved location (Phase 3.3) strips GPS from renders sent to Immich too. Uploads respect the
  export metadata policy.

---

## Exit gate

- [ ] Studio capture and native tethering (Canon/Nikon/Sony verified) ✅.
- [ ] Publish framework with Hard Drive, SFTP and Immich services; plugin SDK with two example plugins.
- [ ] Edit-in round trip with PhotoCraft (layered PSD) ✅; actions record and replay ✅.
- [ ] People clusters and XMP face regions ✅; `.lrcat` v2 migration with fidelity report.
- [ ] IMM-PUBLISH, IMM-SYNC, IMM-PEOPLE, IMM-SEARCH and IMM-MCP ✅ against the minimum and latest Immich releases.
- [ ] Scripted end-to-end session through MCP, headless in CI:
  1. simulated tethered shoot;
  2. import preset;
  3. cull;
  4. develop;
  5. publish to Hard Drive, SFTP and Immich (album plus stacked original);
  6. edit the rating in Immich and see it sync back;
  7. print a contact sheet to PDF.
