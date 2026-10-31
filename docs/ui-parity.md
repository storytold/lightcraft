# UI parity

> **Last reviewed:** 2026-10-11 · **Last updated:** 2026-10-25 · **Change:** minor (KEYC-METADATA: ⌘⌥⇧K Set Keyword Shortcut and ⇧K; KEYC-METADATA: ⌘⌥⇧C / ⌘⌥⇧V Copy / Paste Metadata; KEYC-RATING: ⌘L Enable Filters; context menus: a right-click elsewhere on the same widget moves its open menu instead of flashing it, issue #1877; Reference Escape, catalog-local pins and deleted-photo handling verified on Windows; KEYC-RATING: `\` shows / hides the Filter Bar in the grids; earlier: zoom row soft proofing sharp at 1:1, canvas tools by keyboard, focus-order test, ~50 unnamed hand-drawn widgets named; KEYC-METADATA: ⌥0 / ⌥⇧0 next / previous keyword set; KEYC-RATING: ⌘L Enable Filters; context menus: a right-click elsewhere on the same widget moves its open menu instead of flashing it, issue #1877; Reference Escape, catalog-local pins and deleted-photo handling verified on Windows; KEYC-RATING: `\` shows / hides the Filter Bar in the grids; earlier: zoom row soft proofing sharp at 1:1, canvas tools by keyboard, focus-order test, ~50 unnamed hand-drawn widgets named; KEYC-METADATA: ⌘K Add Keywords; KEYC-RATING: ⌘L Enable Filters; context menus: a right-click elsewhere on the same widget moves its open menu instead of flashing it, issue #1877; Reference Escape, catalog-local pins and deleted-photo handling verified on Windows; KEYC-RATING: `\` shows / hides the Filter Bar in the grids; earlier: zoom row soft proofing sharp at 1:1, canvas tools by keyboard, focus-order test, ~50 unnamed hand-drawn widgets named) · **Target:** Adobe Lightroom Classic 15.5 / Lightroom 9.6

How LightCraft's interaction compares with Lightroom's: views, panels, tools and handles, modifier
keys, shortcuts, zoom, context menus and feel. Rows for individual menu items and shortcuts live in
[parity-checklist.md](parity-checklist.md) (sections Y and Z, Classic keys); this page is the
assessment and the notes that don't fit a row. Visual reference for agents: `plan/lightroom/`
(local, gitignored; `10-observed-ui.md` and screenshots).

## Measured counts (2026-10-10)

- 226 engine commands (`crates/engine/src/cmd/`, 30 modules) and 143 UI commands (`UI_COMMANDS` in
  `crates/ui-egui/src/menus.rs`, 73 with a shortcut) plus 10 language commands; 23 secondary
  shortcut bindings (`ALIASES`, `crates/ui-egui/src/shortcuts.rs`); a keymap editor.
- 122 develop controls plus 10 indexed templates (`crates/develop/src/controls.rs`).
- Checklist: menus 95.3% (86 in-scope rows), desktop shortcuts 94.4% (81 rows), Classic extras
  48.2% (82 rows), measured by recounting [parity-checklist.md](parity-checklist.md).
- 31 panel modules (`crates/ui-egui/src/panels/`), 26 context-menu sites.

## Assessment by area

| Area | Target | LightCraft | Ready | Main gaps |
|---|---|---|---:|---|
| Layout and views | Lightroom: grid / detail / edit; Classic: Library, Develop, Map, Book, Slideshow, Print, Web modules, filmstrip, panels | Lightroom-style single window: Photo Grid, Square Grid, Detail, Compare, Survey (≤ 48), Before / After, People, quick slideshow, second window, filmstrip | 75% | no Classic module switcher (by design) or output modules; dark theme only |
| Develop panels and sliders | Light, Color, Effects, Detail, Optics, Geometry, Lens Blur, Calibration | every global slider; scrub, double-click reset, Alt-drag previews where implemented | 80% | Lens Blur, Calibration panel depth; slider feel (acceleration, fine Shift-drag) unmeasured |
| Crop, Upright, transform handles | handles, aspect lock, angle, overlays (O / ⇧O), guided Upright | ✅ all of them; overlay cycle on ⇧O | 85% | crop opacity of cut-out area (15.5) |
| Masking tools | brush (A / B, flow, density, auto mask, pressure), linear / radial, range masks, AI masks, pins | brush, linear, radial, colour / luminance range, add / subtract / intersect / invert, masks panel, pins | 60% | brush live preview (#517 / #518), no pen pressure, no depth range, AI masks, Feather / Edge controls (15.5) |
| Remove / heal | content-aware remove, heal, clone, visualize spots | heal, clone, auto source, visualize spots, red / pet eye | 65% | content-aware (patch synthesis) remove, generative remove, detect distractions |
| Zoom and navigation | Fit / Fill / 1:1 / 2:1 … 11:1, Navigator, spacebar pan | Fit to 800%, pinch zoom, pan, Navigator-like; every view, mask and diagnostic overlays (Visualize Spots, Point Color range, HDR range) and soft proofing included, is sharp at 1:1–8:1 | 60% | zoom stops at 800 % (Adobe's reference goes to 11:1) |
| Library culling and metadata | ratings, flags, labels with keys, painter, keywording, metadata panel, filter bar | ratings / flags / labels and advance, keyword list and sets, keywording box, metadata fields, smart albums, filter bar | 85% | painter tool, quick collection, some Classic keys |
| Shortcuts | Lightroom desktop and Classic keymaps | Lightroom-desktop keymap with Classic additions, editable | 85% | deliberate differences below; no Classic keymap layer |
| Context menus | everywhere | 31 sites (grid, filmstrip, albums, folders, keywords, masks, presets, text fields…), all through `widgets::context_menu` (`crates/ui-egui/src/widgets.rs`): a right-click elsewhere while a menu is open closes it and opens the menu there, also on the same widget (issue #1877: that one flashed at the new spot and closed, as egui counts the opening click as a click while open) | 70% | unaudited against Lightroom's menus item by item |
| Text fields | native editing behaviour | custom text field (LR-BEHAV-TEXTFIELD) | 70% | IME and selection edge cases |
| Accessibility | VoiceOver / Narrator, keyboard navigation | AccessKit tree audited by a test in every view, Develop tool and the main dialogs: every control named, sliders with value, range and Increment / Decrement, dialogs titled, tool sliders reachable with Tab; crop handles, spots, gradient handles and mask pins by keyboard | 75% | brush strokes and red-eye ellipses need the pointer; not tried with a real screen reader ([audit](#accessibility-audit)) |
| Feel and responsiveness | GPU-interactive sliders (15.3) | ≤ 16 ms slider budget on GPU; background renders | 75% | colour NR at half resolution, 100k-photo grid test |
| **UI/UX overall** | | | **~70%** | estimated, weighted by use (develop and library 60%, tools 25%, rest 15%) |

Remaining: **32–54 h** (zoom detail 8–15 h, brush and masking feel 8–12 h, accessibility 2–4
h, Classic keymap layer 4–8 h, context-menu audit 4–8 h, text-field and IME polish 4–8 h). Output
modules and AI tools are counted with their features in [target-app-parity.md](target-app-parity.md).

## Shortcuts

Compared `plan/lightroom/06-shortcuts.md` (desktop part) with our bindings: command specs in `crates/engine/src/cmd/`,
`UI_COMMANDS` in `crates/ui-egui/src/menus.rs` and secondary bindings (`ALIASES`) in `crates/ui-egui/src/shortcuts.rs`.
`no_conflicting_bindings` (same file) fails when one key fires two actions.

**Fixed (M16.1):** added Lightroom-desktop keys as secondary bindings for existing commands — ⌘D Select None, ⇧E
Export dialog, Space Toggle zoom, ⇧M Create Version, ⇧X Reject + advance, ⇧U Unflag + advance. `⇧Y` fired both
Before/After Split and the History panel; History lost the binding. `W` and `⇧⌘I` also fired the engine command
under the UI command that wraps it (a no-op error); the UI command now wins.

**Deliberate differences (our key → Lightroom desktop key)** — each is a conflict with another binding we have:

| Action | Ours | Lightroom desktop | Why |
|---|---|---|---|
| Pick flag | P | Z | Z = toggle zoom (Classic convention); P is the Classic pick key |
| Photos panel (left) | ⌘⇧L | P | P = pick |
| Expand/collapse edit sections | ⌘⌥1–5 | ⌘1–6 | ⌘0 = Zoom to Fit, ⌘1 = Zoom 100 %; Geometry lives in the Crop panel |
| Histogram | ⌘⇧H | ⌘0 | ⌘0 = Zoom to Fit |
| Square Grid | ⇧G | (none; ⇧G = Guided Upright) | Guided Upright is a button in the Crop panel |
| Hide / show pins (Tool Overlay) | ⇧H (cycles Auto, Always, Selected, Never) | H (Classic) | H opens Remove; View ▸ Tool Overlay and the Remove / Masking panels pick a mode |
| Export dialog | ⌘⇧E (+ ⇧E) | ⇧E | ⌘⇧E is "Edit in Photoshop" there; no external-editor command yet |
| Crop overlay cycle | ⇧O | O | O = mask overlay; ⇧O (mask colour / overlay orientation) unused otherwise |
| Create Version | ⌘⇧S (+ ⇧M) | ⇧M (Windows: Ctrl+⇧S) | — |
| Select None | ⌘⇧A (+ ⌘D) | ⌘D | — |

**Library culling (KEYC-RATING):** `0–5`, `6–9` and `P/X/U` reach egui on macOS even when displayed in the native menu (adapted from PR #261; issue #283). `Shift+0–9` applies and advances once; `Shift+P` picks and advances in Photo Grid / Square Grid and retains Presets elsewhere. See [library shortcuts](library-shortcuts.md) for the verified Classic mapping and regression coverage. F6, F7 and F8 show / hide the filmstrip, the left panel and the right panel (F8 brings back the panel it hid; `view.rightPanel`). `` ` `` cycles the flag (unflagged → pick → reject → unflagged) and ⌘↑ / ⌘↓ raise / lower it; the native menu leaves ⌘↑ / ⌘↓ to egui so Compare/Survey act on the active photo. `\` shows / hides the Filter Bar in Photo Grid / Square Grid and keeps Show Original in Detail. ⌘L / Ctrl+L (Enable Filters) puts the filter aside and shows every photo, and the next press brings back the same filter, stars, flag and text together; setting or clearing a filter while it is off drops the one put aside (not documented by Adobe, our choice). ⌘⌥⇧C / ⌘⌥⇧V (Ctrl+Alt+Shift+C / V on Windows and Linux, KEYC-METADATA) copy the active photo's metadata and paste it on the selected photos.

**Grid thumbnail size (KEYC-COMPARE):** Classic's `=` and `-` increase and decrease the Grid thumbnail size (same keys on macOS and Windows; `+` also works for the larger size here, as Shift+`=` or the keypad). They step the bottom-bar slider by 30 pt inside its 90–480 pt range, in Photo Grid, Square Grid and People only; Detail, Compare, Survey and Reference ignore them, and ⌘= / ⌘− stay Zoom In / Out. See [library shortcuts](library-shortcuts.md).

**Add Keywords key (KEYC-METADATA):** `⌘K` (`Ctrl+K` on Windows and Linux) is Add Keywords (`panel.addKeywords`): it shows the Keywords panel, opens its Keywording section when that is folded shut, and puts the cursor in the Add keyword box; pressed again it leaves the panel open. With no photo selected the panel says "Select photos to give them keywords." and the key does not move the cursor. The shortcut list names the key only; what the key opens is LightCraft's choice. Covered by `crates/ui-egui/src/tests/keywording.rs`.

**Library navigation keys (KEYC-COMPARE):** in Photo Grid and Square Grid, `Home` / `End` make the first / last photo of the view the active, selected one, and `Shift+Left` / `Shift+Right` add the previous / next photo of the view to the selection (the added photo becomes active; the keys never remove a photo from the selection, and do nothing at either end of the view). They wait in the other views, in the full-screen preview, while a text field or a dialog has the keyboard, and when you gave the key to a command; they are listed under Fixed keys in Help → Keyboard Shortcuts. `Cmd+O` (`Ctrl+O` on Windows and Linux) runs File → Open Library… ; View → Visualize HDR has no default key (`Shift+H` cycles the Tool Overlay, issue #224; `Cmd+Shift+H` stays the Histogram). Covered by `crates/ui-egui/src/tests/keymap.rs`; the macOS native menu path for `Cmd+O` is not exercised by the headless tests.

**Keyword sets (KEYC-METADATA):** `⌥0` / `⌥⇧0` (`Alt+0` / `Alt+Shift+0` on Windows and Linux) make the next / previous keyword set current (`keyword.nextSet`, `keyword.previousSet`), in the Keyword Set menu's order: Recent Keywords, then the saved sets. After the last set comes Recent Keywords, and before Recent Keywords comes the last set (not documented by Adobe, our choice). `⌥1`–`⌥9` then apply the new set's keywords. Both keys can be reassigned in Help → Keyboard Shortcuts.

**Keyword shortcut keys (KEYC-METADATA):** `⌘⌥⇧K` (`Ctrl+Alt+Shift+K` on Windows and Linux) is Set Keyword Shortcut… (`dialog.keywordShortcut`): it asks for one or more keywords with commas between them and shows the ones set before; a new name goes inside the default parent, as in the Keywording box. A keyword's Use This as Keyword Shortcut in the Keyword List sets that keyword alone, and the list shows a + after each keyword of the keyword shortcut. `Shift+K` (`keyword.toggleShortcut`) adds the keyword shortcut's keywords to the selected photos, or removes them when every selected photo has them all (the help pages do not say how a mixed selection behaves; this rule is our choice); with no keyword shortcut it does nothing. A photo's context menu offers Add Keyword with the keyword shortcut's keywords. The keyword shortcut is a mark on its keywords in the library's keyword list (`keywordShortcut`, like Put New Keywords Inside This Keyword): a new name is listed, the mark moves with a renamed, moved or merged keyword and goes with a deleted one, and setting it is one undo step. The tests are in `crates/ui-egui/src/tests/keyword_list.rs`, `crates/engine/src/tests/keyword_list.rs` and `crates/catalog/src/keywords/tests.rs`.

**Still missing / broken:**
- Tab / ⇧Tab (hide the side panels / all panels): Tab moves the keyboard focus here (accessibility), so it is
  not bound; F5 (hide the top panel) is not bound either.
- ⌘M / ⌘H / ⌘Q / ⌘W rely on the platform window defaults (unverified).


## Accessibility audit

`crates/ui-egui/tests/it/accessibility.rs` walks the AccessKit tree egui hands to VoiceOver, Narrator and
AT-SPI, headless, in Photo Grid, Square Grid, Detail, Compare, Survey and Reference, in each right
panel (Edit, Profiles, Crop, Remove, Masking, Red Eye, Versions, Activity, Keywords, Info) and in 13
dialogs (Export, Copy / Paste Settings, Create Preset, Rename, New Album, New Smart Album, Capture
Time, Label Names, All Metadata, Save Metadata Preset, Auto Stack, Contact Sheet). It fails on a
button, check box, slider, text field, dropdown, tab, link or menu item without a name, on a slider
without its value and range, and on a dialog window without a title.

Found and fixed (2026-10-12): 47 problems in the views and tools, 11 more in the dialogs.
- Every text field announced only its contents: the top search box, the Info panel's metadata fields
  and GPS, the keyword fields, Crop's custom aspect, and plain egui fields in the dialogs (export file
  name, folder, subfolder, watermark, preset names, label names, rename template, metadata filter).
  `TextField` now names itself by its caption (`.label`) or hint; plain egui fields are named with
  `widgets::named`.
- Develop sliders announced a value but no range: they now carry min, max, step, the value as shown
  ("+0.50"), and answer a screen reader's Increment / Decrement (VoiceOver Ctrl-Option-Up / Down) by
  one step, as the arrow keys do. A slider reached with Tab takes the arrow keys as a clicked one does.
- Auto Stack's time slider had no name.

Keyboard reach: Crop (angle, offsets, scale, aspect) and Remove (size, feather, opacity) settings
are reached with Tab and adjusted with the arrow keys (tested).

Canvas tools by keyboard (`crates/ui-egui/src/canvas_keys.rs`, tests in `tests_canvas_keys.rs`):
every handle drawn on the photo has an invisible stand-in that takes the focus. Tab reaches it, a
screen reader reads its name and position ("Crop bottom-right corner", "left 0%, top 0%, right
100%, bottom 100%"; "Spot 1", "x 30%, y 60%, size 5%"), and the arrow keys move it by 0.5% of the
photo (Shift: 5%), one undo step per press. While it has the focus it keeps the arrows, Return
and Space, and the app's other shortcuts still work.
- Crop: four corners, four edges and the whole box (`crop.drag`, so the aspect lock and image
  bounds apply as they do for a drag); Return applies the crop.
- Remove: each spot's target and source; a "New spot" point that starts at the centre, moves
  with the arrows, and turns into a spot of the brush's size when you press Return.
- Masking: the four edges and the rotation handle of a radial gradient, the start and end of a
  linear one, and each mask pin (moving one selects its mask).

Focus order: the audit now also counts any widget that takes the focus or a click, whatever its
role. That found about 50 hand-drawn icons, tiles and rows with no name: top-bar icons, flags and
stars, filmstrip and survey tiles, preset and profile rows, curve channels, colour wheels, mask
tiles and rows, slider labels and readouts. They are now announced as buttons. A test walks Tab
through the Edit, Crop, Remove and Masking panels and checks that it goes top to bottom (the tool
strip beside them is its own column, walked first). No reordering was needed once nothing
unnamed was in the way.

Not done: brush strokes and red-eye ellipses still need a pointer. Nobody has tried the app with
a real screen reader on any platform yet.

## Reference view lifecycle

Escape leaves Reference for Detail, preserving the active photo and its edits; a second Escape
returns to the grid. Existing dialogs, tools and Lights Out retain their higher-priority Escape
handling. The reference stays pinned when leaving the view, but a successful Open/New Library
clears it: numeric photo IDs are local to each catalog. A refused switch keeps the current pin,
including Open/New refused while an import or its folder scan is running.

`photo.setReference` refuses missing or Recently Deleted photos without replacing the current
pin. Deleting a pinned reference stops drawing it; re-entering Reference falls back to a live
active photo. Restoring the photo permits pinning it again. An empty catalog cannot supply one.

Verification: four new behavioral regressions failed on upstream `76b5377` before the fix.
The patched Windows x64 app on RTX 5080 / DX12 passed Escape, edit preservation, delete/restore,
and New/Open Library checks using isolated catalogs. Saved exposure survived reopening. Native
1400 × 900 captures did not fall back to headless rendering. Control-channel input does not
establish OS keyboard delivery or cross-platform acceptance.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-25 | minor | KEYC-METADATA: ⌘⌥⇧K / Ctrl+Alt+Shift+K (Set Keyword Shortcut) and ⇧K add and remove the keyword shortcut's keywords; the Keyword List marks them with a + |
| 2026-10-24 | patch | KEYC-METADATA: ⌘⌥⇧C / ⌘⌥⇧V (Ctrl+Alt+Shift+C / V on Windows and Linux) copy and paste metadata |
| 2026-10-24 | minor | KEYC-METADATA: ⌥0 / ⌥⇧0 (Alt+0 / Alt+Shift+0) go to the next / previous keyword set, Recent Keywords first, wrapping around at either end |
| 2026-10-24 | minor | KEYC-METADATA: ⌘K / Ctrl+K (Add Keywords) shows the Keywords panel with the cursor in its Add keyword box |
| 2026-10-15 | patch | Context menus: a right-click elsewhere on the same widget while its menu is open moves the menu to the pointer; before it flashed at the new spot and closed (issue #1877); every site uses `widgets::context_menu`; 70% unchanged |
| 2026-10-23 | patch | KEYC-RATING: ⌘L / Ctrl+L (Library ▸ Enable Filters) switches the filter off and on, restoring it as it was |
| 2026-10-15 | patch | KEYC-RATING: `\` shows / hides the Filter Bar in Photo Grid and Square Grid (Show Original in Detail); ⌘L is the filter-bar key left |
| 2026-10-14 | minor | Zoom row: diagnostic overlays render zoom windows too |
| 2026-10-14 | patch | Shortcuts: Visualize HDR loses its default ⇧H, which cycles the Tool Overlay (issue #224); both PRs had claimed it |
| 2026-10-12 | minor | Accessibility: crop handles, spots, gradient handles and mask pins operable by keyboard with AccessKit names and values; Tab order test for the right panels; hand-drawn icons and rows named; 55% → 75%; UI remaining 34–58 h → 32–54 h |
| 2026-10-13 | minor | Shortcuts: Tool Overlay modes for spot and mask pins on ⇧H (H stays Remove; issue #224), listed with the deliberate differences; dropped from the missing list |
| 2026-10-13 | minor | Library culling: backtick cycles the flag; ⌘↑ / ⌘↓ raise / lower flag status |
| 2026-10-13 | minor | KEYC-COMPARE: `=` / `-` grid thumbnail size keys noted in the shortcuts section |
| 2026-10-13 | minor | Shortcuts: Home / End and Shift+Left / Right in the two library grids, Cmd+O (Open Library…) and Shift+H (Visualize HDR) get bindings and keyboard tests; percentages and the dated counts above are unchanged |
| 2026-10-14 | minor | Zoom row: soft proofing gets zoom windows; LR-VIEW-ZOOM gap closed |
| 2026-10-12 | minor | Accessibility: an AccessKit tree audit test over every view, Develop tool and 13 dialogs; unnamed fields, slider ranges and the Auto Stack slider fixed; 30% → 55%; UI remaining 40–70 h → 34–58 h |
| 2026-10-21 | minor | Reference Escape exits to Detail; catalog-local pins and missing/deleted reference handling verified with regressions and native Windows smoke |
| 2026-10-11 | minor | Zoom row: Compare and Reference get zoom windows; what remains of LR-VIEW-ZOOM is overlays and soft proofing |
| 2026-10-10 | major | Created: measured command and shortcut counts, assessment by area, estimates; moved the shortcut conflicts section here from the checklist |
