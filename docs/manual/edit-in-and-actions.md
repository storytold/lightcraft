# Edit In and Actions

> Names in angle brackets are the values set in [`brand.toml`](../../brand.toml); see the [manual index](README.md).

## Edit In

Photo → Edit In (`photo.editIn`) and Photo → Edit in External Editor (`Cmd+Shift+E`) hand a photo to another
editor, with Lightroom's three choices:

| Mode | What the editor gets |
|---|---|
| Edit a copy with adjustments (`copyWithAdjustments`) | a TIFF (or layered PSD) rendered with your edits |
| Edit a copy (`copy`) | a copy of the original file |
| Edit original (`original`) | the original itself (JPEG, TIFF, PNG, PSD, WebP only) |

A new file (default naming `{name}-Edit`) is added to the library, stacked on the original and selected; the desktop app
opens it in the preset's editor and reloads the photo when it regains focus. Each **Edit In preset** names an application and its
arguments (`{file}` marks where the path goes), the format, colour space (sRGB, Adobe RGB, Display P3, ProPhoto) and
bit depth. Built-in presets: *External Editor* (the editor set in Settings → General, or the system default) and
*PhotoCraft*. Add your own with `editIn.savePreset`; `editIn.presets` lists them.

## Actions

An action is a named, recorded sequence of commands, replayed on the selection, like Photoshop actions or Lightroom
plug-in batch tools.

- **Record:** `actions.record name=…`, then do the work in the app; `actions.stop` saves it. Selection, view and
  filter commands are left out, and the photos a command named are dropped, so the action applies to whatever is
  selected when you play it (`keepTargets=true` keeps them).
- **Play:** `actions.play name=…`. A per-photo action (the default) runs once for each selected photo; playing stops
  at the first failing step and can be undone.
- **Parameters:** a step can refer to `{{name}}`; `actions.save` declares parameters with defaults, a description
  and an optional keyboard shortcut; `actions.play args={…}` fills them.
- From the command line: `<cli> run-action NAME [param=value…] [ids=[…]]`.

Actions are stored in `actions.json` in the library.

```sh
<cli> run-action "Web Prep" --library DIR long=1600 ids='[12,13]'
```
