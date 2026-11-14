# Metadata, Keys and combined album views

## Read-only Metadata panel

Window → Metadata (`panel.metadata`) and the Metadata button in the right tool strip open a
compact two-column view of the active photo's catalog metadata. Selecting another photo updates
it immediately, in grids and Detail. The dock tab reads Metadata. Values can be selected and
copied; there are no metadata editing controls. Empty optional fields are omitted. The original
Info panel remains available for editing.

The panel shows file details, capture date, camera and exposure settings, existing descriptive
metadata, keywords and location. File paths wrap in full and can be selected, copied with a
button or revealed in the platform file manager. Labels and values align at the left edge.
Dates use the system locale with two-digit days and months (for example, 01.01.2014 in Germany).
All populated catalog fields follow the prominent fields, then all EXIF and XMP tags supported
by LightCraft's metadata reader, including sidecar metadata. Original files are read on a worker
so offline originals do not block the UI; read errors are shown explicitly. Stale results from
a previous photo or library are discarded. Photo → All Metadata remains available separately.

## Settings → Keys

Settings → Keys embeds the same editor as Help → Keyboard Shortcuts. Search for a command,
click its shortcut and press the replacement. Escape cancels recording. Remove, restore one and
Reset All work as before, and assignments persist in `ui.json`. Commands without a declared
shortcut can also be shown and assigned.

For example, search for Grid (`view.gridToggle`) and record Space. Space now switches to the
grid or cycles the grids instead of invoking its default zoom/hand action. G no longer invokes
Grid. The default bindings do not change until the user edits them. Reserved application keys
and the existing list of fixed contextual keys retain their current semantics.

## Combine album views

Select the first album, then Cmd-click (macOS) or Ctrl-click another album. Both sidebar rows
are selected and operation controls appear above the album tree:

- OR: photos in any selected album, deduplicated.
- AND: photos in every selected album.
- First minus others: photos in the first album that are absent from all the others.
- Only one album: photos in exactly one selected album (XOR for two albums).

The order of album names below the operation controls identifies the first album. A plain
click leaves the combination and shows one album normally. Modifier-click adds or removes an
album. Two to eight ordinary or smart albums can be combined. The command is
`view.combineAlbums {ids: [1, 2], operation: union|intersection|difference|exclusive}`.

The view uses live catalog rules rather than copying or moving photos: changing album
membership immediately updates the result. Ordinary rating, keyword and other filters can
narrow it further. A new combination starts with a fresh filter. For example, All minus
Children hides photographs already assigned to Children, including offline references.

Regression coverage: `crates/ui-egui/src/tests/custom_workflow.rs` checks selection-following
read-only metadata, recording Space from Settings and dispatching it, set-operation truth
tables, live membership and returning to a normal album source.
