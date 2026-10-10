# Library ratings, labels and flags

Select photos in Photo Grid or Square Grid, then use:

| Key | Action |
|---|---|
| `0` | Clear the star rating |
| `1`–`5` | Set the star rating |
| `[` / `]` | Decrease / increase each selected photo's rating by one, within 0–5 |
| `6`, `7`, `8`, `9` | Set a red, yellow, green or blue label |
| `P` | Flag as a pick |
| `X` | Flag as rejected |
| `U` | Clear the flag |
| `Shift` + `0`–`9`, `P`, `X` or `U` | Apply the action and select the next photo |

Purple and clearing a colour label are available in Photo → Set Color Label and the photo context
menu. Purple has no default number key, matching Lightroom Classic. Rejection marks a photo for
culling; it does not delete the original or remove it from the library.
Setting a colour label shows the same brief bottom toast as a rating, using the colour or your
custom label name. Clearing it shows a confirmation too, whether applied by a key, menu or swatch.
Labelled thumbnails have a translucent matching colour on the square-grid surround, photo-grid
footer (translucent, over the bottom of the photo) and Detail filmstrip surround. The white active-photo outline remains clear.
The label confirmation uses a pale matching background with dark text; ratings and errors retain
their neutral HUD styling. Custom names use the underlying label's colour.

Bracket ratings use each photo's own starting rating, and the changed photos form one Undo / Redo
step. AutoWrite uses the normal rating metadata path. In Detail, including Masking and Remove,
`[` / `]` keep resizing the brush and shifted brackets keep adjusting feather. Saved overrides,
remapping and disabling the default brush bindings take precedence over their grid bracket actions.
The separate Decrease Rating / Increase Rating commands can also be assigned in Help → Keyboard Shortcuts.

Grid actions apply to all selected photos and support Undo / Redo. In Compare and Survey, culling
actions apply to the active photo. Auto Advance moves forward after an action; holding Shift with
Auto Advance on still advances only once. The last photo stays selected. Text fields keep their
keyboard input, and `X` in the Crop panel swaps the crop aspect.
Shifted number keys also use the physical number key when the keyboard reports punctuation
(for example, `Shift+1` as `!`). Letter shortcuts continue to follow the keyboard layout.

`Shift+P` picks and advances in the two library grids. In other views it retains LightCraft's
Presets-panel binding. `Shift+Z` remains an alternative pick-and-advance key in all views.

The macOS native menu displays plain-key bindings but leaves execution to egui; only shortcuts
AppKit delivers to the menu (Command, Control or function keys) are skipped by egui. The native
menu correction is adapted from [PR #261](https://github.com/storytold/lightcraft/pull/261),
commit `4e012a59a6d2442ba749f109ea27aadb5c0b11a5` by Gardy, and addresses the cause reported in
[issue #283](https://github.com/storytold/lightcraft/issues/283). That PR was open when checked
on 2026-10-08; this work extends its fix with library culling bindings and tests.

Public behaviour references, checked 2026-10-08:

- [Lightroom Classic keyboard shortcuts](https://helpx.adobe.com/lightroom-classic/help/keyboard-shortcuts.html)
- [Lightroom Classic flagging, labelling and rating](https://helpx.adobe.com/lightroom-classic/help/flag-label-rate-photos.html)

Only public descriptions were consulted. No Adobe assets or application data are included.
