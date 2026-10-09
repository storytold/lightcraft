# Phase 0: Fork, brand configuration and foundations

Part of [PLAN.md](PLAN.md). **Estimate:** 12–22 agent-hours. **Licence:** MIT (decided 2026-10-10, PLAN.md §4).
**Unblocks:** every later phase.

**Goal:** the LightCraft code base, running under a product name that lives in **one config file** and can be changed
at any time. It also gets a parity tracker that covers Lightroom Classic and Immich, and an upstream-merge routine.

---

## 0.1 Fork

1. Clone `../lightcraft` into this folder with full history:
   - `git clone ../lightcraft .`
   - rename `origin` to `upstream`
   - add the new origin.
2. Tag the fork point `fork-base` so `git log fork-base..upstream/main` always shows what upstream did since.
3. Write `docs/upstream-merge.md`. The routine:
   1. `git fetch upstream`
   2. merge into a `merge/upstream-YYYYMMDD` branch
   3. run `cargo xtask brand check` and `cargo xtask ci`
   4. fix, commit, merge.
   - Weekly at first.
   - Upstream renames are resolved by the brand tooling in 0.2, never by hand-editing names.

**Done when:** `cargo xtask ci` is green on the untouched fork.

---

## 0.2 The name lives in one config file (`brand.toml`)

**Requirement:** the product name (and everything derived from it) is defined **once** in `brand.toml` at the
workspace root. Changing that file and rebuilding renames the product everywhere:

- window title, menus and About;
- CLI help;
- logs and settings folders;
- installers;
- MCP server name;
- docs that are generated.

**No source file may contain the product name.**

### 0.2.1 The config file

`brand.toml` (workspace root, the only place a name is written):

```toml
# Changing anything in [product] is safe at any time: rebuild and repackage.
[product]
display_name   = "NoNameYet"            # window title, menus, About, dialogs, docs
short_name     = "NoNameYet"            # macOS menu bar, taskbar, notifications
tagline        = "Photo library and raw development"
vendor         = "NoNameYet contributors"
homepage       = "https://example.invalid"
repository     = "https://example.invalid/repo"
binary         = "nonameyet"            # desktop executable name in packages
cli_binary     = "nonameyet-cli"
env_prefix     = "NONAMEYET"            # NONAMEYET_LOG, NONAMEYET_GPU, …
mcp_server     = "nonameyet"            # MCP server id shown to agents
icon_svg       = "brand/icon.svg"       # source for every generated icon
logo_svg       = "brand/logo.svg"       # identity plate default, About box

# Identifiers stored in users' files and OS registrations. Changing these is allowed, but the old
# values MUST be appended to [legacy] in the same commit, so existing installs keep working.
[identity]
app_id          = "org.example.NoNameYet"   # reverse-DNS: bundle id, Flatpak id, MIME/desktop ids
settings_dir    = "NoNameYet"               # ~/.config/<x>, ~/Library/Application Support/<x>, %APPDATA%\<x>
library_default = "NoNameYet Library"       # default catalog folder name under Pictures
catalog_ext     = "nnycat"
preset_ext      = "nnypreset"
url_scheme      = "nonameyet"               # nonameyet:// deep links, OAuth redirects

# Never derived from the product name: these stay valid across renames forever.
[stable]
xmp_namespace_uri    = "https://ns.example.invalid/develop/1.0/"
xmp_namespace_prefix = "nnyd"
catalog_magic        = "PHOTO-CATALOG"      # file-format magic; neutral on purpose

# Previous identities, read on startup for migration (settings, catalogs, presets, env vars).
[legacy]
settings_dirs = ["LightCraft"]
env_prefixes  = ["LIGHTCRAFT"]
preset_exts   = ["lcpreset"]
catalog_exts  = []
app_ids       = []
```

Three groups, because names fall into three kinds:

- **[product]** is the user-visible brand, freely changeable.
- **[identity]** is OS and file identity. It is changeable, but it needs migration, which the app does automatically
  from **[legacy]**.
- **[stable]** holds format identifiers that must never follow the name. A rename must never orphan users' XMP sidecars.

### 0.2.2 How code gets the name

- **New crate `crates/brand` (L0, no dependencies).**
  - Its `build.rs` reads `../../brand.toml` (or `$BRAND_FILE`, for test builds) and generates `pub const`s:
    `DISPLAY_NAME`, `BINARY`, `APP_ID`, `ENV_PREFIX` and the rest.
  - It also provides helpers:
    - `brand::env("LOG")` reads `NONAMEYET_LOG`, falling back to every legacy prefix;
    - `brand::settings_dir()`;
    - `brand::legacy_settings_dirs()`.
  - `cargo:rerun-if-changed=brand.toml` means editing the file triggers a rebuild.
- **Code changes:**
  - Replace every hard-coded name in `crates/` and `apps/` (292 × "LightCraft" in 115 files, 25 `LIGHTCRAFT_*`
    environment variables, preset and library names) with `brand::` constants.
  - Replace user-visible format strings like `"{} Library"` with `brand::LIBRARY_DEFAULT`.
- **Localisation:**
  - Catalogs use a `{app}` placeholder instead of the name;
  - the i18n loader substitutes `brand::DISPLAY_NAME` at load time.
  - Update every `docs/localization-*.md` table.
- **Native macOS menu bar:** take the application menu title from `brand::SHORT_NAME`.
- **Packaging becomes templates rendered by `cargo xtask package` from `brand.toml`:**
  - `packaging/linux/{app_id}.desktop.in`, `{app_id}.metainfo.xml.in`, `{app_id}.mime.xml.in`;
  - Flatpak manifest, `nfpm.yaml.in`;
  - `macos/Info.plist.in`;
  - `windows/app.wxs.in`.
  - The current files hard-code `ai.storyteller.lightcraft.*` and `lightcraft.wxs`.
- **Icons:** `packaging/icons.sh` renders every icon size from `brand.icon_svg`.
- **Binaries:**
  - Cargo `[[bin]] name`s cannot read a config file, so the binaries keep **neutral internal names** (`app`, `app-cli`).
  - `cargo xtask run`, `cargo xtask package` and `cargo xtask install` copy and rename them to `brand.binary` /
    `brand.cli_binary`.
  - Shell completions and man pages are generated with the branded name.
- **Crate names:**
  - Packages are named by function with a neutral internal prefix (`dac-raw`, `dac-catalog`, …; the prefix is an
    internal codename never shown to users and can be changed with `cargo xtask rename-crates <prefix>`).
  - Renaming the crates is a separate one-time mechanical commit, done right after the fork, so upstream merges map
    cleanly. See `docs/upstream-merge.md`: the xtask also rewrites `lightcraft_*`/`lightcraft-*` paths in incoming
    upstream changes.

### 0.2.3 Migration on rename

On first start under a new `[identity]`, the app checks every `[legacy]` entry in order:

- if a legacy settings folder exists and the new one does not, it **copies** the folder (never moves it) and writes a
  `migrated-from` marker;
- it opens the legacy default library if no new one exists;
- it reads legacy preset extensions;
- it honours legacy env var prefixes, with a one-time deprecation log line.

LightCraft itself is listed in `[legacy]`, so existing LightCraft users can switch with their settings and libraries.

### 0.2.4 Enforcement

- **`cargo xtask brand check`** (part of `cargo xtask ci`) fails if any of these appears outside `brand.toml`,
  `[legacy]` handling, `ATTRIBUTION.md`, `docs/upstream-merge.md` or `CHANGELOG` history:
  - the current `display_name`, `binary` or `env_prefix`;
  - any legacy name (`LightCraft`, `lightcraft`, `LIGHTCRAFT_`, `ArtCraft`, `storyteller`).
- **A CI job builds with a throw-away brand** (`BRAND_FILE=xtask/test-brand.toml`, display name "Zzyzx Test") and
  checks two things:
  - The headless UI snapshots (LightCraft already captures every widget through the control channel) contain
    "Zzyzx Test" and no other product name.
  - Settings, logs and the default library go to Zzyzx paths.
- **Docs:** README and docs are written with the name in a `{{app}}` template where it matters. `cargo xtask docs`
  renders them; prose elsewhere says "the app".

**Done when:**
- Editing `display_name` in `brand.toml` and running `cargo xtask run` shows the new name in the title, menus, About,
  logs path, CLI `--help`, MCP `serverInfo` and installer metadata, with **zero** source edits.
- The brand-check job is green.

---

## 0.3 Remove upstream branding

- Delete `docs/brand/` (ArtCraft trademarks, not open licensed), the README hero and badges, Discord links and the
  "Crafting Apps" sections.
- Remove the `contributors/` and `assets/ATTRIBUTION.md` entries that only concern ArtCraft branding.
- Add our own placeholder `brand/icon.svg` and `brand/logo.svg` (original work) with attribution entries.
- Keep the upstream copyright notices in `NOTICE`/`LICENSE-*`; MIT and Apache require it.

---

## 0.4 Apply the licence: MIT (PLAN.md §4)

1. **`LICENSE`:**
   - MIT text, with the copyright lines "the LightCraft contributors" and "the PhotoCraft contributors" for inherited
     code, and "the project contributors" for new code;
   - no product name.
2. **Remove `LICENSE-APACHE`** from the root and set `license = "MIT"` in `[workspace.package]`.
3. **Apache-2.0-only crates keep their licence:**
   - `crates/segment` (ported Hugging Face SAM 3 code) and `crates/fetch` keep `license = "Apache-2.0"`, a local
     copy of `LICENSE-APACHE` in each crate folder, and their `NOTICE` entries;
   - Phase 1.7 writes the new `net` crate fresh under MIT instead of extending `fetch`.
4. **`NOTICE`:** keep the upstream attributions, the Hugging Face notice and third-party data attributions.
5. **Clean-room rules:** keep LightCraft's `AGENTS.md`/`CLAUDE.md` non-negotiables word for word and add:
   - darktable and Ansel may be used **as running applications, manuals and prose design notes only**;
   - their source trees in `../darktable` and `../ansel` are off-limits to agents, as are GPL data tables
     (`noiseprofiles.json`, `wb_presets.json`, `colormatrices.c`, basecurves);
   - camera matrices are never harvested from Adobe-converted DNGs (check the `Software` tag); camera-native DNGs are
     fine;
   - a "Sources" line in every PR that adds an algorithm (paper, prose spec, or "own design").
6. **`cargo deny`** in CI, so a GPL dependency can't arrive silently:
   - licences allowed: MIT, MIT-0, Apache-2.0 (incl. LLVM exception), BSD-2/3-Clause, ISC, Zlib, Unicode-3.0, CC0-1.0;
   - everything else, including GPL, LGPL, AGPL and MPL, is denied.
7. **`assets/ATTRIBUTION.md` policy (unchanged):** CC-BY and CC-BY-SA are allowed only as separate data or asset
   files, with attribution (for example the lensfun DB and GeoNames).

---

## 0.5 Rebuild `plan/` and extend the tracker

LightCraft's tracker ids point to a local `plan/` folder that wasn't published.

1. Write `plan/` from scratch, **in our own words**:
   - `lightroom-classic/` (feature catalog, menus, Classic shortcuts, per-module behaviour notes);
   - `architecture.md` (PLAN.md §5);
   - `STATUS.md`;
   - `execution-plan.md` (these phase files).
2. Decide whether to keep `plan/` gitignored. Recommendation: **commit it**. The ignore rule only existed to keep
   Adobe-observation notes local, so keep that one subfolder (`plan/observations/`) ignored.
3. Extend `docs/parity.md`:
   - reopen the 🚫 Classic rows (KEYC-MODULES, the seven LRC-WEB rows, LRC-MAP-REVGEO);
   - add rows for every PLAN.md §2 feature that has no row yet;
   - fix the duplicated "Top gaps" list.
4. Add a new tracker section **"IMM. Immich integration"** with the rows below. They are worked through in Phases 1,
   3 and 4. `cargo xtask parity` counts them like any other row.

| Id | Feature | Tier | Phase |
|---|---|---|---|
| IMM-CONNECT | Connect to one or more Immich servers (URL + API key), version check, secure key storage | P1 | 1 |
| IMM-LINK | Link catalog photos to Immich assets by checksum; "in Immich" badge and filter | P1 | 1 |
| IMM-IMPORT | Immich as an import source (browse albums/timeline, download originals or add as linked) | P1 | 1 |
| IMM-EXTLIB | Shared-originals mode: Immich external library and catalog over the same folders, no duplicate uploads | P1 | 1 |
| IMM-SHARELINK | Create Immich shared links for published albums (from Web/Slideshow/Publish) | P2 | 3 |
| IMM-PUBLISH | Immich publish service: collections → albums, renders and/or originals, re-publish, stacks | P1 | 4 |
| IMM-SYNC | Two-way metadata sync (rating, favourite, title/description, tags ↔ keywords, albums ↔ collections, GPS, time, archive) | P1 | 4 |
| IMM-PEOPLE | Import Immich people and face regions into People; push names back | P2 | 4 |
| IMM-SEARCH | Immich smart (CLIP) and metadata search from the Library filter bar | P2 | 4 |
| IMM-MCP | Immich commands available via CLI, control channel and MCP | P2 | 4 |

5. Write `plan/immich.md`, the research note:
   - Immich concepts (assets, checksums, albums, tags, people, stacks, external libraries, shared links, API keys
     and permissions);
   - the API endpoints we need, with the Immich release they were checked against;
   - the minimum supported Immich version;
   - how Immich's ratings and favourites behave.
   - **Licence note:** the Immich server is AGPL-3.0. We only talk to it over HTTP and write our own client from its
     public API documentation. We don't vendor its code or its generated SDK.
6. Add `xtask/immich/compose.yml`, a pinned Immich release for local and CI integration tests, plus
   `cargo xtask immich up|down|seed`. The seed step loads a CC0 fixture set and creates an API key.

---

## 0.6 Shared infrastructure

- `craft-fonts` as the `CRAFT_FONTS_DIR` build input (CJK glyphs), and the raw corpus via `cargo xtask corpus`.
- Read and keep the never-crash standard (LightCraft's `AGENTS.md` already includes its substance; `craftrules` is
  not needed at build time).

---

## Exit gate

- [ ] `cargo xtask ci` green under the configurable brand; brand-check job green with the throw-away brand.
- [ ] Renaming the product = editing `brand.toml` only (demonstrated in a PR that renames twice and back).
- [ ] Legacy LightCraft settings and library are picked up on first start.
- [ ] `plan/` exists; `docs/parity.md` contains Classic rows and IMM rows; `cargo xtask parity` prints the new totals.
- [ ] `cargo xtask immich up` starts a test server; licence policy enforced by `cargo deny`.
