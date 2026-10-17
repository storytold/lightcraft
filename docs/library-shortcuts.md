# Library ratings, labels and flags

Select photos in Photo Grid or Square Grid, then use:

| Key | Action |
|---|---|
| `Enter` | Open the active photo in Detail from Photo Grid or Square Grid |
| `Cmd+Shift+D` (macOS) / `Ctrl+Shift+D` (Windows/Linux) | Select only the active photo |
| `0` | Clear the star rating |
| `1`–`5` | Set the star rating |
| `[` / `]` | Decrease / increase each selected photo's rating by one, within 0–5 |
| `6`, `7`, `8`, `9` | Set a red, yellow, green or blue label |
| `P` | Flag as a pick |
| `X` | Flag as rejected |
| `U` | Clear the flag |
| `` ` `` (backtick) | Cycle the flag: unflagged → pick → reject → unflagged |
| `Cmd`/`Ctrl` + `↑` / `↓` | Raise / lower flag status (reject < unflagged < pick; stops at either end) |
| `Shift` + `0`–`9`, `P`, `X` or `U` | Apply the action and select the next photo |
| `\` (backslash) | Show / hide the Filter Bar |

Enter opens the active photo without changing the selection. Text fields, dialogs and open popups keep the key; a custom Enter binding takes precedence. Enter has no additional action in Detail.

Select Only Active Photo (Edit menu) narrows the selection to the active photo. It does not change
the photo, its edits or the last edit Undo step, and it works in any view that has an active photo.
It can be reassigned or disabled in Help → Keyboard Shortcuts; text fields and dialogs keep the key.

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

`\` shows and hides the Filter Bar in Photo Grid and Square Grid, as in Lightroom Classic's Library; in
Detail it keeps showing the original (Develop's Before / After key). View → Filter Bar (`Shift+F`) works
in every view. A Show Original moved to another key keeps its meaning in the grids, and `\` given to
another command runs that command.

Flag cycling and raise/lower use each photo's own flag, so a mixed selection steps together without being made uniform. Adobe's shortcut list names the backtick "Cycle flag settings" but does not state the order; unflagged → pick → reject is LightCraft's choice. The key is the logical grave key, so it follows the keyboard layout (on layouts where it is a dead accent key, assign another key in Help → Keyboard Shortcuts to `photo.cycleFlag`).

`Home` and `End` make the first or last photo of the view the active, selected one. `Shift` + `←` /
`→` add the previous or next photo of the view to the selection and make the added photo active; the
key never removes a photo from the selection, and at either end of the view it does nothing. These
four keys work in Photo Grid and Square Grid only. They wait in Detail, Compare and Survey, in the
full-screen preview (which covers the grid), while a text field or a dialog has the keyboard, and
when you gave the same key to a command in Help → Keyboard Shortcuts, where they are listed under
Fixed keys.

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

## Grid thumbnail size

`=` makes the grid thumbnails larger and `-` smaller (`+` also enlarges), as in Lightroom Classic. Each
press moves the bottom-bar slider by 30 pt within its 90–480 pt range and stops at either end. The keys
work in Photo Grid, Square Grid and People (the views with the slider), do nothing in Detail, Compare,
Survey and Reference, and leave text fields alone, so the search box still types `-` and `=`. `⌘=` /
`⌘−` remain Zoom In / Out. The same steps are the View ▸ Increase / Decrease Grid Thumbnail Size
commands (`view.thumbSizeUp`, `view.thumbSizeDown`, reachable from the control channel and MCP, which
return `{"size"}`); the size is saved with the other interface settings.

Public behaviour references, checked 2026-10-08 (thumbnail size: 2026-10-13):

- [Lightroom Classic keyboard shortcuts](https://helpx.adobe.com/lightroom-classic/help/keyboard-shortcuts.html)
- [Lightroom Classic flagging, labelling and rating](https://helpx.adobe.com/lightroom-classic/help/flag-label-rate-photos.html)

Only public descriptions were consulted. No Adobe assets or application data are included.
