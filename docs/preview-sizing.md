# Preview size

**Settings → Performance → Preview size** limits whole-frame previews to a percentage
of each photo's long edge after crop and rotation. The slider accepts 0–100% in steps
of 1%. **Pixel limit** accepts a positive integer, or `-1` for no user pixel limit.
Press Return or leave the field to apply it; Escape cancels the edit. Invalid text
leaves the previous limit in effect and reports how to correct it.

The smaller limit wins. For a 6000 px photo, 25% means at most 1500 px; adding a
1000 px pixel limit reduces that to 1000 px. A smaller on-screen view is not enlarged
to reach the limit. The app rounds requested sizes in 64 px steps but never rounds
past a cap. At 0%, or a pixel limit below 8, it keeps an 8 px minimum long edge.

## What changes

- Whole-frame previews in Detail, Before/After, Compare, Reference and slideshow
  previews use the limits. Smaller previews reduce the rendering work and look softer.
- Zoomed detail retains the separate sharp window of the original, as before: these
  limits do not reduce the resolution of the visible region at 100% zoom and above.
  Explicit percentage zoom also stays sharp when the photo fits inside its pane.
  Whole-pixel rounding in tiny previews preserves the photo's geometry and sharp zoom,
  including the painted image in Before/After, wipe and hovered-preview views.
  At Fit, an intentional cap is respected without a native-detail window bypassing it.
- **Build Standard-Sized Previews** uses the percentage per photo and the pixel cap
  (2560 px when no user pixel cap is set). Grid thumbnails and **Build 1:1 Previews**
  retain their independent sizes. Rebuilding does not discard other cached sizes.
- Exports, originals, edit settings and saved smart previews are unchanged.

Removing the user pixel cap does not remove image bounds, GPU texture limits or
allocation safeguards. A whole-frame preview is still limited to 8192 px and the
display backend's texture size. Neighbour and hover previews retain their source-level
limits. A zoom window renders only the visible region of its larger frame.

## Saved preferences and automation

The default is 100% with no user pixel cap, preserving Automatic's current behavior.
Existing `previewLimit` values (0 for Automatic, 1600, 2560, 3840, 5120) are preserved;
older files without `previewPercent` load at 100%. The saved zero sentinel maps to
`-1` in the interface and command. Custom caps also persist across restarts.

Through the app/control channel or connected MCP:

```json
{"method":"engine.execute","params":{"command":"app.previewSize","params":{"percent":25,"pixelLimit":1000}}}
```

Omit either parameter to keep it, or omit both to query the current values. Percent
must be an integer from 0 to 100; the pixel limit must be -1 or an integer from 1 to
4294967295 (safety ceilings still apply). Invalid parameters cause an error before any
preference changes. The app saves both with its UI preferences, not the photo library.

The engine's `library.buildPreviews` command also accepts `percent` (default 100) for
standard builds, alongside `edge` (8–8192 px). It ignores the percentage for full 1:1
builds; the app's standard-preview menu supplies both saved settings.
