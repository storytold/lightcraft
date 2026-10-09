# Classic keyboard map

Grouped list of the Lightroom Classic keys we track (`KEYC-*` rows; the desktop `KEY-*` rows cover the cloud-style
app). ⌘ = Ctrl on Windows and Linux, ⌥ = Alt. Our current bindings and deliberate differences are in
`docs/parity.md` ("Shortcuts: conflicts and missing bindings"). The plan is a switchable **Classic keymap layer**
(`LRC-SHELL-KEYMAP`, Phase 1) so these work exactly, with today's layout kept as an option.

## Desktop part (shared with the cloud-style app)

The `KEY-*` rows in `docs/parity.md` list the desktop keys one per row (C crop, D detail, E edit, G grid, I info,
K keywords, L / R linear and radial gradients, M masking, W white-balance picker, `\` show original, Y before/after,
Space zoom, 0–5 rating, 6–9 labels, Z / X / U flags, ⇧ variants advance, ⌘C / ⌘V copy and paste settings, …).

## Classic part

| Group (row) | Keys | Behaviour |
|---|---|---|
| Modules (KEYC-MODULES) | ⌘⌥1 … ⌘⌥7 | Library, Develop, Map, Book, Slideshow, Print, Web; ⌘⌥↑ returns to the previous module; G / E / D / C / N also jump to Library views or Develop |
| Panels (KEYC-PANELS) | Tab, ⇧Tab, F5, F6, F7, F8, T, ⌘⇧F (hide all) | hide side panels / all panels; top bar, filmstrip, left, right; toolbar; ⌥-click a panel header for solo mode |
| Views (KEYC-VIEWS) | G grid, E loupe, C compare, N survey, D develop, L lights-out cycle, F screen-mode cycle, ⇧F, I info overlay cycle, J grid cell style cycle, ⇧R reference view | the Library views and screen modes |
| Secondary window (KEYC-SECONDWINDOW) | F11 open/close, ⇧ + G / E / C / N on the second window, ⇧⌘↩ full screen | live / locked loupe on the second display |
| Photo and catalog (KEYC-CATALOG) | ⇧⌘I import, ⇧⌘E export, ⌘' virtual copy, ⌘R show in folder, F2 rename, ⌫ remove, ⌘E external editor, ⌘G / ⇧⌘G stack | photo-level commands |
| Grid / compare / navigation (KEYC-COMPARE) | Z zoom toggle, Home / End, = / − thumbnail size, ⌘⇧D deselect, S stack expand, ↑ ↓ ← → in compare | navigate and compare |
| Rating and flags (KEYC-RATING) | 0–5, ⇧0–5 (advance), 6–9 labels, P pick, X reject, U unflag, ⇧P / ⇧X / ⇧U (advance), `[` / `]` step rating, ` toggle flag, ⌘↑ / ⌘↓ flag up / down | culling |
| Collections (KEYC-COLLECTIONS) | ⌘N new collection, B add to target collection, ⌘B show quick collection, ⇧⌘B clear it | |
| Keywords and metadata (KEYC-METADATA) | ⌘K keyword entry, ⇧K add keyword shortcut, ⌥0–9 / ⌥⌘0–9 keyword set entries, ⌘S save metadata to file, ⌥⇧⌘C / ⌥⇧⌘V copy / paste metadata | |
| Develop (KEYC-DEVELOP) | V B&W, ⌘U auto tone, ⇧⌘U auto white balance, R crop, Q remove, ⇧T red eye, M / K / ⇧M masking tools, W white-balance picker, ⇧W, ⇧J clipping, ⇧Q, ⌘⇧C / ⌘⇧V copy / paste, ⌘⇧S sync, ⌥⌘V paste from previous, ⌘N new snapshot, ⇧⌘N new preset, ⌘⇧R reset | |
| Output modules (KEYC-MODULE-OUTPUT) | module-specific: page navigation in Book, play / pause in Slideshow, ⌘P / ⌥⌘P print / print one, ⌘J / web export | |
| Help (KEYC-HELP) | ⌘/ shortcuts overlay for the current module, F1 help | |
