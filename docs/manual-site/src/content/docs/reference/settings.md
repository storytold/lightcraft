---
title: Settings
description: Every page of the Settings window, what its groups do, and where each is explained in full.
sidebar:
  order: 0
---

- **Edit ▸ Settings…** (`Cmd/Ctrl+,`). Changes apply at once; there is no OK or Cancel.
- The **?** beside a group or a setting opens this manual at the part about it.
- **App settings** (layout, theme, GPU, shortcuts…) are this computer's. **Catalog settings** (import defaults, XMP
  sidecars, thumbnail cache, slideshow playback) travel with the catalog.
- Where the settings are kept, and what goes online: [Settings and privacy](../../platforms/settings-and-privacy/).

## General

### Layout and settings categories
- **Layout**: **Classic** (the Lightroom Classic layout), **Compact** (the same, drawn denser) or **Modern** (Library
  and Develop on screen, the rest in the ☰ menu). See [Layouts](../../start/layouts/).
- **Settings Categories** (Classic and Compact): the Settings window lists its pages down its **Left** side (Classic's
  default) or in a row along the **Top** (Compact's default). Each layout keeps its own choice; Modern always lists them
  on the left.
- **Grading Mode**: Witchcraft or Eboda; see [Two grading modes](../../develop/colour/#two-grading-modes).
- **Language**: the app's language; English first, then by each language's own name.

### Catalog
- **Location** of the open catalog, **Open Catalog…** and **New Catalog…**.
- **At launch**: open the last catalog, or **Ask me** (lists recent catalogs; holding Alt/Option as the window appears
  asks once).
- **When importing a Lightroom or Capture One catalog**: ask where it goes, or always merge into the open catalog.

### Startup and updates
- **Open in**: the last view, Photo Grid, Square Grid or Detail View.
- **Check for updates when the app starts** (desktop only; off by default).

### Culling and dialogs
- **Auto Advance**: move to the next photo after rating or flagging.
- Confirm before moving photos to **Recently Deleted**, and before moving them to another folder on disk.
- **Close a dialog by clicking outside it**.

### External editing
- **Edit In**: ask which file to edit, or always edit a copy with adjustments, a copy, or the original.
- **PhotoCraft** and **Other editor**: where each is, and the copy's file type (PSD or TIFF), bit depth and colour space.
  See [Edit In and actions](../../automate/actions/).

## Import

### Raw defaults
- What new raw photos start from: the app default, a preset, or **per camera** (each camera seen in the catalog gets
  its own choice). Photos already in the catalog are not touched.
- **AI Denoise from** an ISO: new raws at or above it start with AI Denoise on (needs a denoise model).
- **Remove dust on import** (runs in the background, with the camera's dust map) and **Constrain crop by default**.

### Camera profile
- The camera profile new photos start from, for all cameras or per camera: **Built-in colour**, a camera's imported
  profile, or another installed profile. The Profiles page sets one per camera body.

### Metadata
- Metadata added to imported photos that don't already have it, and a **metadata preset** applied on import.

### XMP sidecars
- **Automatically write changes into XMP sidecars**, and **read sidecars other apps changed** (for photos unchanged
  here).
- **Sidecar names**: how sidecars are named beside the raw.

### Folder polling and drag and drop
- **Check folders for new photos** every so often; new photos are offered ("N new") or added at once.
- **Import dropped photos without review**: off, a drop opens the import review first.
- **Show the import review when a camera or card is connected**.

### Auto Import
- A **watched folder** (tethering, a scanner, a sync app): photos are added as soon as they are complete, copied into
  the catalog or used where they are, with a collection, file naming, develop preset, metadata preset and keywords.
  See [Tethering](../../library/tethering/).

### Lens profiles
- The lens database used for raws whose files carry no correction of their own, and where it is kept. See
  [Lens corrections](../../develop/lens/#where-profiles-come-from).

## Folders
- Every folder the app uses (the catalog, exports, models, caches…) on one page, with where each is now. Changes apply
  at once; each folder is also on the page its feature lives on.

## Performance

### Rendering
- **Use the GPU for rendering**, the GPU backend and card, the device for AI models, **HDR display**, the HDR zoom cache,
  **Preview size** (Automatic renders at the size shown), **High quality adjustment previews** and **Memory for
  caches**. Numbers and budgets: [Performance](../../platforms/performance/#rendering).

### Exports
- **Photos at once** (Automatic follows a test of this computer), **Use the GPU for exports**, which cards exports
  use, and **Test my machine**.

### Thumbnail cache and local folders
- **Size limit** of the thumbnail cache (Automatic, a size, or Custom in GB), what is **in use**, and **Clear Cache**.
- **Forget unchanged photos**: photos seen in Local but never added or changed leave the catalog after a while.
  Files stay on disk.

### Background preparation
- While you step through photos, the next ones are made ready, and when the computer is idle the previews of every
  photo in view are built. Separate settings for **Culling** (a folder before import) and **Library**: previews,
  **Photos ahead**, **Eye focus ahead** and **Which photos**. **Editing** sets how many photos ahead Develop prepares.
- **Use different settings for this catalog**: a timelapse catalog can prepare nothing while a wedding catalog prepares
  everything.

### Smart previews
- The folder smart previews are kept in (the catalog's folder by default; about 1 MB a photo), and moving existing
  previews there.

## Display
- **Monitor profile**: the screen's own profile, an ICC file you choose, or sRGB. Photos are shown through it, so
  colours are right on wide-gamut and calibrated displays; histogram, caches and exports are not affected.
  HDR screens: [Display and HDR](../../platforms/desktop/#display-and-hdr).

## Interface

### Appearance
- **Appearance Mode** (Sync with System, Dark, Light), a theme for each, and how sliders are drawn (Classic only; Compact
  always draws one row, Modern always pills). Shift while dragging gives fine control.
- **Expanded sliders** (off by default) let sliders that support it go past Lightroom's range; not XMP compliant.

### Filmstrip, grid, keywords and presets
- Filmstrip file names and badges; when the grid shows ratings and flags; **Double-click opens the photo in Develop**;
  Square Grid file names; **Wheel scroll speed**; **Reduce motion**; **Wheel resizes brush instead of zooming**; **Invert brush resize direction**.
- **Keywords of the selection**: whole names, or grouped by parent.
- **List User Presets first**, and whether this catalog keeps its own presets, profiles and habits.

### Detail view and eye focus strip
- **Show the Navigator while zoomed in**, **Hide filter text in Develop mode** and the **Info overlay** (Off, Info 1,
  Info 2; View ▸ View Options chooses the lines).
- **Eye focus strip**: each person's sharper eye at 100 % across the top of the loupe, judged sharp, soft or blurry.
  Shown in the Library, and separately **while browsing a folder** before import. Pointing at the strip shows an
  **eye** (zooms the loupe to 100 % on that eye) and a **✕** (hides the strip; its switch in the
  [quick settings bar](#quick-settings-bar) blinks twice so you know where to turn it back on).

### Quick settings bar
- Point at the empty stretch of the module bar between the identity plate and the module names: a row of switches
  fades in, and out again when the pointer leaves. It holds the **info overlay** (Off, Info 1, Info 2), the
  [on-screen overlay](#on-screen-overlay), the **eye focus strip**, **clipping**, the **focus point**, **face boxes**
  and the **filmstrip**. A message in the status line there goes away as soon as the pointer arrives.
- Each switch runs the same command as its menu item. Plug-ins can add their own buttons
  ([Plug-ins](../../automate/plugins/)).

### On-screen overlay
- **Show changes**: Off, Control surface only, or Always. A short note in the window for every change: a slider's value,
  a rating, a profile applied, and how many photos it changed. **Size**, **Opacity**, **Shown for**, **Position** and
  **Background**. Also on the Controls page.

## Slideshow
- Templates, **Playback** (slide and fade durations, the fade, random order, repeat, preparing slides in advance;
  saved with the catalog), the slide layout, overlays, title screens
  and **Export** (PDF, JPEG sequence or video), with a live preview of the slide beside them. See
  [Slideshow](../../output/slideshow/).

## Faces
- **Recognise faces (experimental)**, off until you switch it on; the face detector download; recognition models;
  **Matching** and **Processor**. See [Faces and people](../../ai/faces/).

### Face scan
- Where the scan is (not started, indexing, paused, complete, photos done) and how many faces it has learned.
- **Pause** / **Resume** (a pause is kept across restarts), **Clear…** (deletes what the scan learned and leaves it
  paused; named faces stay) and **Restart…** (deletes it and scans every photo again). Both ask first.
- While it runs, its row in the activity stack has a pause button and a gear that opens this page.

## Profiles
- **Profile Manager…**: every look, LUT profile and camera profile; hide, rename, group, import and delete your own.
- **Camera bodies**: each body's own camera profile, used when its raws are imported; **Apply to Photos** gives it to
  the body's photos already in the catalog.

## Develop

### Tool profiles
- A **tool profile** picks which groups and sliders the Develop panel shows. The low row at the top of the Edit panel
  names the profile in use: its menu switches profile, and the gear beside it opens this page.
- **Default** (every group and slider) and **Witchcraft** (Primaries and HSL Curves in either grading mode, Eboda's
  Color Grading hidden) are built in and can't be changed. **New** makes a profile showing everything;
  **Duplicate** copies the one in use. Your own can be renamed and deleted.
- **Custom**: one group of the sliders you pick, in the order you set, shown first in the panel; a slider in it is
  not drawn again in its own group.
- **Groups**: switch any group off, or open it to hide single sliders and options (the Auto · B&W | HDR row, the
  white balance selector, Contrast Mode, AI Denoise, Remove Hot Pixels, the vignette style…).
- Profiles are saved with the app's settings. Scripts and agents: `view.toolProfile {id}` picks one (without an id it
  lists them); `view.toolProfileEdit {op, …}` changes your own. The Modern layout's cards show their sections whole.

## AI Denoise
- The denoiser in use (the built-in one needs nothing installed), where it runs (**Automatic**, graphics card or
  processor), **Make the denoised picture of the photos I am looking at**, **Denoise all photos with an Amount**, and
  the cache limit. See [AI Denoise](../../ai/denoise/).

## AI Models
- Download, test, choose and remove the AI models (masks, faces, denoise, super resolution, search), each with its
  licence. See [AI models](../../ai/models/); the large SAM 3 model can be kept in a folder of your own
  ([Installing SAM 3](../../ai/models/#installing-sam-3)).

### Semantic search
- **Semantic search: search photos by description** (on by default; off by default on Android). Off: nothing is indexed and the AI search button leaves the
  search box.
- Indexing status with **Pause** / **Resume**, **Clear…** (deletes the index and leaves indexing paused) and
  **Restart…** (deletes it and indexes every photo again). Both ask first. The activity stack's "Indexing photos" row
  has the same pause button and a gear that opens this page.
- **AI embedding device**: Automatic, CPU or a GPU. See [Search by description](../../ai/search/).

## Generative Fill
- One card per image service: your own **API key** (kept on this computer, never in the catalog, sidecars, exports or
  logs), **Get an API key**, the service's **Data policy**, and **Withdraw Consent**. See
  [Generative Remove](../../ai/generative-remove/).

## Shortcuts
- **Keymap**: Classic or Alternative; **Import…** and **Export…** a keymap file; the shortcut editor lists every
  command, with conflicts shown. See [Keyboard shortcuts](../../automate/shortcuts/).

## Controls
- **Use control surfaces**, the profile, and per device: **Keys and Dials**, **Pages and Tools**, **Dial Feel**,
  **Readout and Look**, **Profiles and Setup**. Touching a control on the device selects it here. See
  [Control surfaces and MIDI](../../automate/control-surfaces/).

## Connections
- Immich servers (add with the server URL and an API key, **Test**, **Connect**) and where keys are kept (**System
  Keychain** or **Encrypted File**). See [Immich](../../sync/immich/#connect).

## Sync
- This device's name, finding and pairing devices on your network, syncing the open catalog (the whole catalog or
  collections only), invitations, and what each collection allows or takes. See [Device sync](../../sync/devices/#setup).
