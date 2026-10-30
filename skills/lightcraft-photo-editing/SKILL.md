---
name: lightcraft-photo-editing
description: Develop and correct existing photos in LightCraft through MCP, including exposure, white balance, color, cropping, consistent batch edits, and export. Use for agent-assisted photo finishing, not image generation or LightCraft source-code changes.
---

# LightCraft photo editing

Use LightCraft's non-destructive development tools to finish the user's photos.
Keep the requested mood and existing edits; automatic correction is a starting
point to inspect, not proof that an image is better.

## Establish the target

- Call `doc_inspect` to identify the library, current selection, and active photo.
  For "these photos", capture the selected IDs before any tool changes selection.
  Use `query_photos` to find other requested photos; paginate rather than treating
  its default 200 results as the entire library. Do not expand an unspecified
  selection to the whole catalog.
- Read `get_develop {id}` and `render_preview {id, max_side: 1600}` before editing.
  These reads do not change selection. Use explicit IDs from the library, not
  filenames or guessed numbers, for all targeted helpers.
- For a fresh import, `import {paths, mode: "add"}` references files in place;
  copy/move is a separate file-management choice. Check the returned IDs.
- If no MCP tools are available, report the missing connection. The server is
  `lightcraft-cli mcp --library /absolute/library` for a persistent headless
  library, or `lightcraft-cli mcp --connect 127.0.0.1:7980` for an app started
  with `lightcraft --control 7980`. Use matching CLI/app releases. A library may
  have only one writer: connect to the app if it already has that library open.
  A bare headless session without `--library` does not persist catalog edits.

## Correct and inspect

1. Note the visible issue and choose adjustments that serve the request: exposure
   and highlight/shadow balance, white balance and color, then geometry and detail.
   Keep intentional sunset warmth, low-key lighting, silhouettes, and other scene
   choices unless the user asks to change them.
2. Select only the target with `select_photos {ids: [id], active: id}` before
   active-photo commands. This also prevents Auto Sync from modifying other
   selected photos. For a multi-step pass, create a named starting version with
   `command_run {id: "version.create", params: {name: "Before agent correction"}}`.
   Give each pass a distinct version name and inspect its index with
   `command_run {id: "photo.inspect", params: {id}}` for later restoration.
3. Use `auto_develop {id, adjustment: "tone"}` when automatic tone correction
   fits the request. It replaces exposure, contrast, highlights, shadows, whites,
   blacks, vibrance, and saturation; it is not an additive exposure tweak.
   White balance is separate: `auto_develop {id, adjustment: "white_balance"}`.
   If both are needed, apply white balance first, then tone. Each call edits one
   photo, creates one undo step, and returns its before/after settings.
4. Call `list_controls` for actual control IDs, ranges, and values, then use
   `set_develop {id, values: {"light.exposure": 0.3}}` for focused refinement.
   Values are absolute, not deltas. Read the current value before calculating a
   relative change. Use `settings` only for structures observed in `get_develop`;
   partial objects merge, but arrays replace their existing contents. A call
   containing both `values` and `settings` has separate undo steps and can partly
   succeed. Prefer one form per call. White-balance temperature uses Kelvin on
   calibrated RAW files and a relative scale (6500 = as shot) elsewhere; do not
   assume the same temperature number means the same look on every input. Prefer
   the WB controls or automatic WB over constructing a `settings.wb` object.
5. Render a new preview at the same size and compare it with the starting image.
   Check subject brightness, highlight detail, shadow noise, natural skin/color,
   and crop boundaries. Stop when the requested correction is achieved; avoid
   repeatedly invoking Auto or increasing sliders without a visible reason.
   A small whole-image preview cannot establish pixel-level sharpness or mask
   quality. State that limitation if no suitable detail view is available.

Use `command_list {filter}` to discover available masking, lens, denoise, version,
and other commands and their parameter descriptions. Do not infer support from
a button name or invent parameters. Crop coordinates are normalized 0..1 in the
straightened frame. Inspect local corrections and cropping visually.

If an edit fails, read back the photo's settings before retrying: a timeout or a
failed multi-step call does not prove nothing changed. `command_batch` stops on
error by default but does not roll back completed steps. Session Undo can include
the user's edits in a connected app; use the target's named starting version for
restoration when intervening work makes Undo ambiguous. Re-read `photo.inspect`
to find that version's current index, select the target alone, then use
`version.restore {index}`. Coordinate edits with the user in a shared live session.

## A consistent series

Group photos by scene/lighting and inspect representative images before choosing
a shared look. Correct exposure and white balance per photo; copying one image's
absolute values to a mixed-light series is not equivalent to matching its look.
Process the captured target IDs, inspect results and outliers, and report which
photos could not be completed. Do not copy crops, masks, or healing spots across
different compositions without a specific reason. Restore the original selection
when useful and the user has not changed it during the work.

## Finish

Keep edits in the library unless export was requested. For export, pass the
captured IDs and requested destination, dimensions, and format explicitly.
`longEdge: 0` means native cropped resolution; the default is 3000 pixels.
Use `conflict: "unique"` unless the user requested replacement. Check export
results and report actual output paths and failures. Summarize the corrections
and any unchecked details; do not claim a visual check unless you saw the preview.
