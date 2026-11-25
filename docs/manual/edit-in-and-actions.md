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
| Open source file (`sourceFile`) | the photo's own file, a raw included: nothing is made and the library is not touched |

A new file (default naming `{name}-Edit`) is added to the library, stacked on the original and selected; the desktop app
opens it in the preset's editor and reloads the photo when it regains focus. Each **Edit In preset** names an application and its
arguments (`{file}` marks where the path goes), the format, colour space (sRGB, Adobe RGB, Display P3, ProPhoto) and
bit depth. Built-in presets: *External Editor* (the editor set in Settings → General, or the system default) and
*PhotoCraft*. Add your own with `editIn.savePreset`; `editIn.presets` lists them.

### Presets in the menu

Saved presets are listed in Photo → Edit In (and on a photo's right-click menu), each with the items its **Edit In Menu**
setting picks (the `menu` field of `editIn.savePreset`):

| `menu` | Items |
|---|---|
| `choices` (default) | Edit a Copy with Adjustments, Edit a Copy, Edit Original; on a raw the third is Open Source File and Edit a Copy is not offered |
| `single` | Edit in `<name>`: one item, which hands over what the preset's `mode` says (set `mode` to `sourceFile` for a raw developer such as DxO PhotoLab) |
| `actions` | The preset's own items, up to 8 (`actions: [{label, args}]`), each with launch arguments: a denoiser's *Process with the Latest Settings*, *Process Immediately*, *Preview and Process* |

An item runs `photo.editInPreset {preset, mode?, action?}` on the selection: each photo's file is made first (a photo that
can't be done is named in the error and the rest go on), then one start of the editor takes every file. The arguments are the
preset's own, then the item's; `{file}` in an argument takes each file's path where it stands (then every file is a start of
its own). Over MCP and the control channel an item names the preset, never a program.

Only you choose which program Edit In starts. Over MCP, the control channel and in actions, `photo.editIn` and
`photo.openAsLayers` take no `app`, `args` or `control`, `editIn.savePreset` can't set or change them, the Edit In
menu commands take no `app`, and `ui.set` can't change the editors in Settings → External Editing: name a saved preset
(`preset=…`) instead. On the command line (`<cli> run photo.editIn app=… launch=true wait=true`, without `--connect`,
which goes through the running app's control channel) and in the Edit In Presets dialog they work as before.

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
