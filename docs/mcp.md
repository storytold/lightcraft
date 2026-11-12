# LightCraft MCP server

`lightcraft-cli mcp` exposes LightCraft to AI agents through the
[Model Context Protocol](https://modelcontextprotocol.io): newline-delimited JSON-RPC 2.0 over
stdio (protocol revision `2025-06-18`; `2025-03-26` and `2024-11-05` clients are accepted). The
server lives in `crates/mcp` (`lightcraft-mcp`, layer L5) and is hand-written: no async runtime,
no C dependencies.

It runs in one of two modes:

| Mode | Command | What it drives |
|---|---|---|
| **Headless** (default) | `lightcraft-cli mcp [--demo] [FILES/FOLDERS…]` | An in-process engine `Session`. Develop, render and export without a window. |
| **Connect** | `lightcraft-cli mcp --connect [127.0.0.1:7980]` | A running desktop app started with `lightcraft --control 7980`, through its loopback JSON-lines control channel ([control-protocol.md](control-protocol.md)). Adds the UI tools (screenshot, clicks, keys, pointer gestures). |

Options: `--library DIR` opens (or creates) a persistent LightCraft library — the same crash-safe
format the desktop app uses (`~/Pictures/LightCraft Library` by default there) — so ratings, edits and albums
survive between sessions (with `--demo`, a new library is seeded with the demo photos). A library
is open in one program at a time (`catalog.lock` in the library folder): while the desktop app has
it open, `--library` on the same folder fails with "This library is already open in LightCraft
(process N …)" — use connect mode to work with the running app instead;
`--demo` starts the headless session with the procedurally generated demo library;
`--compact` lists only the helper tools (see below). In connect mode the server starts even when
the app is not running yet and connects on the first call (and reconnects if the app restarts).
A request is sent again (once) only when it never reached the app: it could not be sent, or the app's
idle-close notice came back in its place (the app closed the idle connection without reading it; see
[control-protocol.md](control-protocol.md)). If the connection drops after a request
was sent but before its reply, the tool fails with "the outcome is unknown" and nothing is resent, so a
command such as undo can't run twice: check the app's state before retrying. Replies must be
newline-terminated, at most 64 MiB before the newline (including encoded previews/screenshots),
and carry the request's integer `id`, a boolean `ok`, and either a `result` or a string `error`.
A malformed, incomplete, oversized or mismatched reply also closes the connection and reports
an unknown outcome without resending. An idless `ok: false` with a string `error` is preserved
as a pre-dispatch refusal (for example, too many control connections); the next call reconnects.

Logs go to stderr; stdout carries only protocol messages.

## Automation roots

`lightcraft-cli mcp` can keep an agent's file access to the folders you give it, for a server that hosts agents or any
other unattended use:

- `--automation-read-root DIR`: reads stay inside `DIR` (or the write root).
- `--automation-write-root DIR`: writes stay inside `DIR`.

```sh
lightcraft-cli mcp --automation-read-root /srv/inbox --automation-write-root /srv/out --library /srv/library
```

Either flag also accepts `--flag=DIR`. A folder whose name starts with `-` has to use that form (`--automation-read-root=-folder`). The flags are rejected when:

- one of them is given twice (the space form and the `=` form count as the same flag)
- the folder operand is missing
- the folder text is empty (`--automation-read-root=`)
- the next argument starts with `-` and was not passed with `=`
- `--connect` is set as well (a running app is not confined by these flags)
- the folder does not exist, or exists and is not a directory (`cannot open the automation read root` / `cannot open the automation write root`)

The rules:

- **A root grants its own kind only.** Reads are allowed inside either root, writes only inside the write root. With only
  a read root nothing can be written; with neither option nothing is confined.
- **A file has one name.** An existing file with any other name is refused, even when the name that was
  checked is inside a root, because another name may lie outside. A name count that cannot be read is refused too.
  A directory is judged only by where it is.
- **A path outside the roots looks the same whether or not it is there.** The error does not say the configured
  folders, and it does not say that an outside folder is missing.
- **Links are followed before a path is judged.** Both the roots and every path are resolved first, so `..`, a link to a
  file or folder outside, and a link that leads nowhere are refused before anything is opened. A file that doesn't exist
  yet is judged by its nearest existing folder. Comparison is by whole path components (`/work2` isn't inside `/work`)
  and exact: a folder that exists counts in its real case, so `C:\WORK` finds `C:\Work` on an ordinary Windows volume, and a
  name that doesn't exist yet has to match. Reserved Windows device names (`CON`, `NUL`, `COM1`…) are refused too.
- **Writing to a link acts on the link.** A write can replace, rename or remove the link itself, so a link's own folder
  has to be inside the write root too, whatever the link points to. A file under a linked folder is judged where it
  really is. A write root that is itself a link is used through paths below it: the link alone isn't writable unless its
  own folder is in the root.
- **Headless only.** `--connect` is refused together with a root: a running app isn't confined by them.
- **Every command is classified.** `Session::execute` checks the paths among a command's parameters before it runs
  (`crates/engine/src/file_access/commands.rs`). A command not in that list is refused, and a test walks the command
  registry so a new command can't land without a line there. `app.export` and the helper tools go through the same check.
- **What a command reaches indirectly is checked where it is opened**: a photo's original and sidecars through the
  library, a watermark or identity plate graphic, a print or proof profile. A catalog or preset can name a file outside
  the roots: it counts as missing and isn't read, and nothing says whether it exists.
- **The check comes before anything else a command looks at.** With roots in force, `photo.convertToDng` on a photo that
  isn't raw and sits in a read-only place answers with an error, not a skip entry. `app.export` is the exception:
  a preset is merged into the parameters first, and those merged parameters are what is checked.

Refused while roots are in force: `export.addToPhotos` and `addToPhotos` on an export (hand files to another application) and `library.devices` (lists
the mounted volumes).

Not confined, on purpose:

- What the operator chose at startup: the `--library` folder with its catalog, previews and archives.
  A smart preview is written there from a photo the roots can read. A photo the catalog names outside
  the roots counts as missing and is not read, including as the source of a smart preview.
- The app's own folders: settings, user presets and templates, the denoise and face models and the lens and place-name
  data it downloads. No path an agent gives reaches them, but a shared server should give each client its own `HOME`.
- Other programs. A path is checked and then opened, so a process that swaps in a link in between can still win. A file
  with more than one name is refused, because another name may lie outside the roots; a name count that cannot be read
  is refused too. A file with one name is an ordinary file. The roots keep an agent to the files it was given; they
  don't replace an operating-system sandbox.

Where the file-touching commands are checked, besides the check of their parameters:

| Commands | Access | Checked in |
|---|---|---|
| `import`, `library.import`, `library.importPreview`, `library.browse`, `library.findMissing`, `library.autoImport*`, `folder.synchronize`, `folder.scanChanges` | Read (`move` also needs write on the source; `destination` needs write) | `walk.rs`, `files/fs.rs`, `import_move.rs`, `cmd/library/`, `cmd/browse.rs`, `sync.rs` |
| Originals and sidecars, read | Read | `files/fs.rs`, `files/preview.rs`, `sidecar.rs`, `import/job.rs`, `focus_scan.rs`, `cmd/query.rs` |
| `photo.saveMetadataToFile`, automatic sidecars | Write | `sidecar.rs` |
| `export`, `app.export`, `render_photo` with a `path`, `export.contactSheet`, `print.exportPdf`, `slideshow.export`, every `*.export*` of presets and templates | Write (a render without a `path` stays in memory) | `export/place.rs`, `originals.rs`, `slideshow_export.rs` |
| `folder.create`, `folder.rename`, `folder.move`, `photo.moveToFolder`, `photo.rename` | Write on both ends | `cmd/browse.rs`, `rename/fs.rs`, `cmd/manage.rs` |
| `photo.duplicate`, `photo.convertToDng`, `photo.editExternal` | Read the source, write the copy | `cmd/convert.rs`, `export/place.rs` |
| `library.smartPreviewsLocation` | Write | `cmd/previews.rs` |
| `photo.relink`, `photo.autoTagTracklog`, `keyword.import`, `curve.importPresets`, `preset.import`, `mask.importEffects`, `profile.import`, `print.template.import`, `slideshow.templates.import`, `denoise.models.install`, `faces.models.install` | Read | `file_access/commands.rs`, and the command's own file where it opens one |
| `library.inspectLightroom`, `library.importLightroom` | Read, the `-wal` file too | `lightroom_catalog.rs` (`read_source` and `read_file`) |
| Watermark and identity plate graphics, print and proof profiles | Read | `export/watermark.rs`, `print_color.rs`, `proof.rs`, `display.rs` |

## Wiring it into a client

Build once: `cargo build --release -p lightcraft-cli` (binary: `target/release/lightcraft-cli`).

### Claude Code

```sh
# headless, with a folder of photos imported at start
claude mcp add lightcraft -- /path/to/lightcraft/target/release/lightcraft-cli mcp ~/Pictures/shoot

# or: drive the running desktop app (start it with `lightcraft --control 7980`)
claude mcp add lightcraft-app -- /path/to/lightcraft/target/release/lightcraft-cli mcp --connect 127.0.0.1:7980
```

Or check a project-scoped `.mcp.json` into your repo:

```json
{
  "mcpServers": {
    "lightcraft": {
      "command": "/path/to/lightcraft/target/release/lightcraft-cli",
      "args": ["mcp", "--connect", "127.0.0.1:7980"]
    }
  }
}
```

### Codex

The macOS release includes a separate `lightcraft-cli-<version>-macos-universal.zip`;
unpack it alongside the desktop app if you do not want to build from source. Use the CLI
and app from the same release. Verify both downloads against that release's `SHA256SUMS.txt`.

Register the CLI's stdio server with Codex using its **absolute** installed path:

```sh
# Headless: a persistent library whose edits survive MCP restarts.
codex mcp add lightcraft -- "/absolute/path/lightcraft-cli" mcp --library "/absolute/path/library"

# Live desktop: start the app on the matching loopback port first.
"/absolute/path/LightCraft.app/Contents/MacOS/LightCraft" --library "/absolute/path/library" --control 7980
codex mcp add lightcraft-app -- "/absolute/path/lightcraft-cli" mcp --connect 127.0.0.1:7980
```

Choose one mode for a library: a headless server cannot open a library already locked by
the desktop app. Connect mode edits the window's actual library and reconnects after the
app restarts. An external library path keeps the catalog on that drive. Copy/move imports
default to the library's `Originals/` folder; add-mode imports keep the originals at their
existing paths. The library path does not relocate settings, logs or optional models (see
the README's settings/log locations). Make sure an external drive is mounted before
starting either process.

`codex mcp list` confirms registration, not a successful tool call. After adding the server,
restart the client's MCP connections and verify `doc_inspect` or `query_photos` against the
expected library before editing. Configuration options are in the
[Codex MCP documentation](https://developers.openai.com/codex/mcp/).

### From an installed release

The release packages ship `lightcraft-cli` alongside the desktop app, so no build is needed:

| Install | CLI |
|---|---|
| Windows (MSI) | `%LOCALAPPDATA%\Programs\ArtCraft\LightCraft\lightcraft-cli.exe` by default (`C:\Program Files\ArtCraft\LightCraft\lightcraft-cli.exe` for an all-users install, under `Program Files (x86)` instead for the 32-bit build, or wherever you installed it otherwise), not on `PATH` |
| Linux (deb, rpm) | `/usr/bin/lightcraft-cli` |
| macOS | the separate `lightcraft-cli-<version>-macos-<arch>.zip` release asset (the `.app` holds only the desktop app) |

```sh
# Windows (PowerShell), default install folder
claude mcp add lightcraft -- "$env:LOCALAPPDATA\Programs\ArtCraft\LightCraft\lightcraft-cli.exe" mcp
# Linux, or macOS with the CLI unzipped onto PATH
claude mcp add lightcraft -- lightcraft-cli mcp
```

### Other clients (Claude Desktop, Cursor, …)

Every stdio MCP client takes the same shape: a `command` plus `args`. For example
`claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "lightcraft": {
      "command": "/path/to/lightcraft/target/release/lightcraft-cli",
      "args": ["mcp", "--demo"]
    }
  }
}
```

During development you can also point the client at `cargo run --release -p lightcraft-cli -- mcp`
(with `"cwd"` set to the repository), at the cost of a slower start.

## Shared core tools

These follow the same conventions as [filmcraft #28](https://github.com/storytold/filmcraft/pull/28).
They are listed in both full and compact mode; existing documented helpers remain listed too.
There are no hidden compatibility aliases.

| Tool | Arguments | Result |
|---|---|---|
| `command_list` | `filter?`, `enabled_only?` | Command catalog |
| `command_run` | `id`, `params?` | Command result |
| `command_batch` | `steps: [{id, params?}]`, `stop_on_error?` | Counts and per-step results; stops on error by default |
| `doc_inspect` | none | Library state and catalog counts |
| `render_preview` | `id?`, `max_side?`, `format?` | Image; neither changes selection nor saves a file |

Each edit in a batch has its own undo step. Connect mode also lists `ui_inspect` and
`ui_screenshot` (the latter has no output-path argument). The existing `inspect_ui`,
`screenshot`, `list_commands`, `run_command` and `render_photo` keep their documented arguments.
Ports and connect mode are unchanged.

Every listed tool has a title and all four MCP hints. Generated command tools are conservatively
marked as edits. `export`, `render_photo` and `screenshot` have `readOnlyHint: false` and
`destructiveHint: true`: their caller-supplied output paths can replace existing files, even
when a path is optional. The image-only `render_preview` and `ui_screenshot` remain read-only
and non-destructive. These annotations are client hints, not a file-access sandbox.
`export` is not idempotent because the default conflict policy chooses a new filename on repeated calls.
Unknown helper argument keys return JSON-RPC `-32602` naming the key and accepted arguments.
Generated `cmd_*` tools and nested command params remain free-form: the registry has prose
parameter docs, not machine-readable schemas. Existing command validation (including strict
export options) is preserved. Escaped tool panics return `isError: true`, and the session keeps
serving; backend panics during resource reads or tool listing return an internal error.
The headless backend owns its session directly, without a session mutex.

`lightcraft://document` and `lightcraft://commands` contain JSON matching `doc_inspect` and
`command_list`. Existing resources remain available. Requests declaring MCP 2026-07-28 in
per-request `_meta` receive `resultType: complete` and list/read cache hints; reads are not cached.

## Tools

### Helpers

| Tool | Does |
|---|---|
| `list_commands {filter?}` | Every command: id, label, menu, shortcut, parameter doc, enabled now |
| `run_command {command, params?}` | Run any command by id |
| `import {paths, album?, mode?, destination?, organize?, rename?}` | Import files/folders (folders are scanned recursively); the first new photo becomes active. `mode: "copy"` copies into `destination` (default: the library's Originals/), `mode: "move"` moves there (each original and its XMP sidecars are removed from the source only after the copy is verified and catalogued; duplicates and failures keep their sources; the result's `moved` / `kept` list what moved and what stayed, and why; undo leaves the moved files at the destination), filed by `organize`: `date` (YYYY/YYYY-MM-DD), `month`, `flat` or a folder template such as `{date:%Y}/{date:%Y%m%d}` (→ `2026/20260114`) or `{date:%Y}/{date:%B}` (→ `2026/January`; capture date, else the import date; always inside the destination), and named by the `rename` template (`run_command photo.renameTokens` lists the tags) |
| `query_photos {filter?, sort?, offset?, limit?}` | Photos in the current view (or matching a catalog `Filter`) |
| `select_photos {ids, active?, mode?}` | Set the selection / active photo. Every id (and `active`) must be in the library: an unknown id is a tool error (`no such photo 9999`) and the selection and active photo are left as they were |
| `list_controls {section?}` | Every develop slider: id (`light.exposure`…), range, default, current value |
| `get_develop {id?}` | Full develop-settings JSON, with `process`: the rendering process the photo is on ([process-versions.md](process-versions.md)) |
| `set_develop {id?, values?, settings?, label?}` | `values`: `{controlId: number}`; `settings`: partial develop JSON deep-merged. Undoable. A `crop.angle` value turns the crop as `crop.straighten` does: kept if it still fits, else shrunk to the largest crop of its aspect inside the photo (`settings` is merged as given, unfitted). White balance: `values` with `wb.temp` / `wb.tint` is Kelvin on raws with colour matrices (DNG, and cameras LightCraft has matrices for) and relative (6500 = as shot) elsewhere; a `settings.wb` with `temp` / `tint` and no `"scale": "kelvin"` is read on the relative scale on every raw, and one that names only `temp` or only `tint` resolves the field it leaves out against the photo, so that field keeps its look; on a photo still on As Shot (or on a named white balance preset, on a raw with matrices) the white it really renders is written out as Custom values first, so the number named takes effect instead of being stored unread (#2910, and its review); `apply_preset` resolves a preset that names one number the same way (see [control-protocol.md](control-protocol.md)) |
| `apply_preset {preset, amount?, ids?}` | Apply a preset (ids from `cmd_presets_list`) |
| `crop {id?, rect?, angle?, reset?}` | Normalized crop rect `[x0,y0,x1,y1]` and straighten angle; at least one of `rect`, `angle`, `reset: true`. The crop always ends inside the photo: an `angle` alone keeps the crop if it still fits, else shrinks it to the largest of its aspect that does |
| `render_photo {id?, size?, format?, path?}` | Render with current settings → **image content** (PNG, or JPEG with `format: "jpeg"`), long edge `size` (default 1024) |
| `export {path \| dir, id? \| ids?, format?, longEdge? \| shortEdge? \| width?/height? \| megapixels? \| percent?, dontEnlarge?, ppi?, quality?, colorSpace?, bitDepth?, …}` | Full-quality render to `.png` / `.jpg` / `.tif` / `.webp` / `.avif` / `.jxl` (`jxlLossless`, `jxlEffort`, `jxlProgressive`); `format: "original"` copies the file + an XMP sidecar with the edits, `format: "dng"` writes raw photos as DNG with the edits embedded. No size param = 3000 px long edge; `longEdge: 0` = full size (cropped, native resolution); `width` + `height` fit either orientation (Lightroom's Dimensions); with `fit: "widthHeight"` they limit the width and the height as the photo stands (Width & Height); `dontEnlarge` defaults to true |

`get_develop {id}` and `render_photo {id}` use that photo without changing the selection.
`set_develop`, `crop` and `export` with a supplied photo `id` end the current edit interaction,
replace the selection with that one photo and make it active. Without `id`,
`get_develop`, `set_develop` and `crop` use the active photo; `render_photo` also falls back to the first
selected photo, and `export` uses the selection for batch export, as above.

### Connect mode only

| Tool | Does |
|---|---|
| `screenshot {maxSize?, format?, path?}` | The app window as an image, after pending renders finish |
| `inspect_ui` | View, panel, window/image rects, selection, status |
| `set_ui {state}` | Merge UI state, e.g. `{"view": "detail"}` |
| `list_widgets {filter?}` / `click {widget \| x,y, count?, button?}` | Widgets by automation id; real egui clicks (`button: "right"` right-clicks) |
| `press_key {key, cmd?, shift?, alt?}` / `type_text {text}` | Keyboard input (shortcuts) |
| `clipboard {action, text?}` | Cut, copy or paste in the focused text field (⌘X / ⌘C / ⌘V; refused when none has the focus; cut and copy write the system clipboard); `inspect_ui` → `copied` is what was copied |
| `pointer_gesture {events}` | Gestures in normalized image coordinates (brush strokes, gradients, crop handles) |

In headless mode these return a tool error explaining how to start the app.

### One tool per command

Everything is a command in LightCraft, so `tools/list` also contains one tool per entry of the
command registry (engine commands, plus the app's UI commands such as `view.detail` when
connected): the id with `.` replaced by `_` and a `cmd_` prefix — `photo.rate` → `cmd_photo_rate`,
`develop.set` → `cmd_develop_set`, `edit.undo` → `cmd_edit_undo`. Arguments are the command's
JSON params (documented in each tool's description and by `list_commands`). Pass `--compact` to
leave these per-command tools out when a client struggles with a large tool list; `run_command`
still reaches every command.

Slideshow slides, for example: `cmd_slideshow_layout` reads or changes the slide layout (margins,
zoom to fill, stroke border, cast shadow, backdrop colour, colour wash, background image; saved with
the library) and `cmd_slideshow_preview {id?, width?, height?, path?}` composes one slide with it —
written as a PNG to `path`, or returned as base64 PNG in `png` — so an agent can look at a layout
before playing it. `cmd_slideshow_playback` sets the timing and fades. `cmd_slideshow_overlays` sets the
overlays drawn on every slide (identity plate text or image, rating stars, text overlays with tokens such as
`{caption}` or `{exposure}`, the export watermark); its reply lists `warnings` (an unreadable plate image, tags that
aren't tokens), and `cmd_slideshow_preview` returns each overlay's box. `cmd_slideshow_titles` sets the intro and ending
screens (colour, the identity plate or a line of text, scale, duration), and `cmd_slideshow_preview {title: "intro"}`
renders one. Templates bundle all four: `cmd_slideshow_templates_list` lists the built-in and user templates (and files
it skipped), `cmd_slideshow_templates_apply {id}` replaces the settings with one (the replaced ones stay available as
`{id: "previous"}` for the session), `cmd_slideshow_preview {template: id}` previews one without applying it, and
`cmd_slideshow_templates_save` / `update` / `rename` / `delete` / `import` / `export` manage the user's own (JSON files
in `<settings>/slideshow-templates`).
`cmd_slideshow_export {format: "pdf"|"jpeg"|"video", path, size?, quality?, fps?, container?: "mp4"|"webm",
fullScreen?, name?, ids?, wait?}` writes the slideshow (titles included) as a PDF (a page per slide), a numbered JPEG
sequence in a folder, or an AV1 video (no audio). It runs in the background and returns `{taskId, total, …}`:
`cmd_activity_list` shows its progress, `cmd_activity_cancel {id: taskId}` stops it (nothing is left on disk) and
`cmd_slideshow_exportStatus` gives the result (`pages`, `files`, or `frames` / `samples` / `seconds` for video) or the
error. With `wait: true` the call blocks and replies with the result.

### Preview-only raws after an update

A raw LightCraft couldn't decode when it was imported is shown from its embedded preview
(`previewOnly` in `catalog.query`). The desktop app rechecks such photos in the background when a
library opens in a newer (or older) LightCraft version, so a raw the new decoder handles develops
from its raw data without a manual Reload. Headless sessions don't do this on their own: call
`cmd_library_recheckPreviewOnly` (`{all: true}` ignores the version stamps, e.g. to test a decoder
build) → `{checked, updated, cleared}`. Like the app's recheck, it isn't an undo step and keeps
develop settings and history.

## Resources

`resources/list` / `resources/read` serve JSON snapshots:

| URI | Content |
|---|---|
| `lightcraft://library` | Source, filter, sort, selection, undo/redo labels (`library.state`) |
| `lightcraft://photos` | Photos in the current view (`catalog.query`) |
| `lightcraft://photo/active` | Everything about the active photo (`photo.inspect`), including the camera's focus point as `meta.focus` when the file records one ([focus-points.md](focus-points.md); `run_command photo.readFocusPoints {wait: true}` reads it for photos imported before it was read, a few KB per file) |
| `lightcraft://develop/active` | The active photo's develop settings (`develop.get`) |
| `lightcraft://controls` | Every develop control with its current value (`develop.controls`) |

Read these resources again to poll current state; they are snapshots, and `initialize` advertises
`resources.subscribe: false`. `lightcraft://photo/active` includes the photo's `id` and `develop`
settings together; `lightcraft://controls` contains slider values for the active photo at that read.
Separate reads can observe different active photos. For controller feedback from GUI edits, use
connect mode; headless mode owns a separate session. See [External controllers](control-protocol.md#external-controllers)
for command mapping, live slider values and polling semantics.

## Example session

```text
→ {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"me","version":"1"}}}
← {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{…},"resources":{…}},"serverInfo":{"name":"lightcraft",…},"instructions":"…"}}
→ {"jsonrpc":"2.0","method":"notifications/initialized"}
→ {"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"set_develop","arguments":{"values":{"light.exposure":0.7}}}}
← {"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"{…}"}],"isError":false,"structuredContent":{"ok":true,"controls":[{"id":"light.exposure","value":0.7}]}}}
→ {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"render_photo","arguments":{"size":768}}}
← {"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"image","data":"iVBORw0…","mimeType":"image/png"},{"type":"text","text":"{\"id\":21,\"width\":512,\"height\":768}"}],"isError":false}}
```

Errors from commands (unknown control, nothing selected, app not reachable) come back as tool
results with `isError: true` so the model can read and correct them; malformed JSON-RPC gets the
standard error codes (-32700 parse, -32600 invalid request, -32601 method not found, -32602
invalid params, -32002 resource not found).

With `--library`, a command whose change can't be written to disk (disk full, …) is an error too:
`saved in memory but not written to disk: <reason>; LightCraft will retry` — the change is applied in the session and
written by the next successful save (see [control-protocol.md](control-protocol.md#when-the-library-cant-be-saved)).

## One-shot commands: `lightcraft-cli run`

For agents that prefer a shell over an MCP session: run any chain of commands in one process and read one JSON
line per command (`{"command", "ok", "result" | "error", "ms"}`; non-zero exit status on failure). A word without
`=` starts the next command; `key=value` values are JSON when they parse, else strings; a `'{…}'` argument merges a
JSON object into the params. Quote values with brackets or spaces for your shell (`'ids=[3,4]'`; zsh treats `[…]` as a glob).

```sh
# headless: import, edit, export
lightcraft-cli run --import ~/Pictures/a.dng develop.set control=light.exposure value=0.7 \
    develop.auto app.export path=/tmp/a.jpg shortEdge=1080 colorSpace=displayP3
# a persistent library: edits are saved, later invocations see them
lightcraft-cli run --library ~/lc-lib --import ~/Pictures/shoot library.info
lightcraft-cli run --library ~/lc-lib library.select ids=[3] develop.get
# the running app (same commands, plus ui.* methods)
lightcraft-cli run --connect ui.set view=detail ui.screenshot path=/tmp/ui.png
# JSON lines from a file or stdin: {"command": id, "params": {…}} or {"method": "ui.inspect"}
lightcraft-cli run --demo --script steps.jsonl --keep-going
```

## Other CLI subcommands

```sh
lightcraft-cli render in.dng -o out.tif --opt colorSpace=displayP3 --opt bitDepth=16 --opt percent=50
lightcraft-cli render in.dng -o out.jpg --set light.exposure=0.5 --set light.contrast=20 --size 2048
lightcraft-cli render in.jpg -o out.png --settings look.json --preset <presetId>
lightcraft-cli render in.dng -o out.jpg --opt addToPhotos=true --opt photosAlbum=Trip   # macOS: then into Apple Photos
lightcraft-cli commands [--json]   # the command registry
lightcraft-cli controls [--json]   # develop control ids and ranges
lightcraft-cli calibrate --max 300 ~/Pictures/2026   # camera colour profiles (docs/camera-preview-colour.md)
```

## Tests

- `crates/mcp/tests/it/e2e.rs` — M0.9 acceptance: over the stdio framing, set exposure and render;
  checks the decoded PNG gets brighter/darker. Runs headless and through the TCP transport
  (`Remote`) against a stand-in control server; also import → render → JPEG export of a real file.
- `apps/lightcraft-cli/tests/it/cli.rs` — spawns `lightcraft-cli mcp` with real pipes; `render`;
  `commands`.

## Add to Apple Photos (macOS)

`app.export` (MCP `export` tool, `lightcraft-cli run`, the desktop app) takes `addToPhotos: true` and
`photosAlbum` to hand the files it wrote to Apple Photos, into that album (a top-level album of exactly that
name, made when missing). The result's `applePhotos` is the import job: `{job, running, requested, album}`
plus `{imported, ids, warning?}`, `{error}` or `{skipped}` (with `cancelled: true` when the export was cancelled) once Photos is done; the files are written either
way. Headless (this server, `lightcraft-cli`) waits for Photos by default; the desktop app (connect mode)
answers with the running job and `export.photosImports {job}` gives the outcome. `export.addToPhotos {paths,
album?, wait?}` does the same for files already on disk (`wait` defaults to true headless, false in the desktop
app, which refuses `wait: true`; with `wait: false` headless, the server waits for the import before it exits at end of input). One import
runs at a time, and an export adding to Photos reserves it before it writes: another is refused, naming the
job in the way. On other platforms `addToPhotos: true` is refused before anything is written, and
`export.addToPhotos` is disabled with the reason. The first time, macOS asks the user to allow LightCraft (or
the terminal running `lightcraft-cli`) to control Photos. After that, any client can add any file LightCraft can
read to the user's Photos library (and so to iCloud Photos when that is on), not only exported ones: see [Who
can add what](apple-photos.md#how-it-works).

## Export progress and cancellation

Headless direct `export`, `command_run` / `run_command` with `app.export`, and `cmd_app_export`
calls report photo-count progress when `params._meta.progressToken` is a string or number.
Notifications are strictly increasing, at most ten per second plus the final total. No token
means no notifications. `ping` is answered at photo boundaries; other requests wait in order
until the export ends. EOF lets a pending export finish and preserves queued requests.

`notifications/cancelled` with `params.requestId` stops the matching export before its next
photo, suppressing its response. Unknown/completed request ids are ignored. A photo already
being processed finishes first: completed photos remain, each written with the existing atomic
file writer, and there are no partial files to delete. Unrelated outputs and earlier exports
are untouched. A failed export is an `isError` tool result. Transport mutexes recover poisoning.

If a protocol write or flush fails, the CLI reports the output error and shuts down without
waiting for stdin to close. Output failures are observable only when writing. An active direct
headless export stops at the next photo boundary; a synchronous command and normal backend
cleanup must finish first. Completed exports and library cleanup retain their usual guarantees.

Library callers serving process stdin use `Server::serve_stdio(stdout())` once per process and
exit after it returns. Its single stdin worker may remain blocked until process exit. The generic
`Server::serve(input, output)` keeps borrowed input scoped: its caller must arrange input EOF or
an input error, even when output fails.

Connect mode and exports inside `command_batch` remain synchronous and do not report MCP
progress or cancellation. Use a direct headless export call for this behavior.
