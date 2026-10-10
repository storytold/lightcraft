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

Logs go to stderr; stdout carries only protocol messages.

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
| Windows (MSI) | `C:\Program Files\LightCraft\lightcraft-cli.exe` by default (wherever you installed it otherwise), not on `PATH` |
| Linux (deb, rpm) | `/usr/bin/lightcraft-cli` |
| macOS | the separate `lightcraft-cli-<version>-macos-<arch>.zip` release asset (the `.app` holds only the desktop app) |

```sh
# Windows, default install folder
claude mcp add lightcraft -- "C:\Program Files\LightCraft\lightcraft-cli.exe" mcp
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
marked as edits. Helpers with optional output paths are marked as writers; `export` is not
idempotent because the default conflict policy chooses a new filename on repeated calls.
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
| `import {paths, album?, mode?, destination?, organize?, rename?}` | Import files/folders (folders are scanned recursively); the first new photo becomes active. `mode: "copy"` copies into `destination` (default: the library's Originals/), `mode: "move"` moves there (each original and its XMP sidecars are removed from the source only after the copy is verified and catalogued; duplicates and failures keep their sources; the result's `moved` / `kept` list what moved and what stayed, and why; undo leaves the moved files at the destination), filed by `organize`: `date` (YYYY/YYYY-MM-DD), `month`, `flat` or a folder template such as `{date:%Y}/{date:%Y%m%d}` (→ `2026/20260114`; capture date, else the import date; always inside the destination), and named by the `rename` template (`run_command photo.renameTokens` lists the tags) |
| `query_photos {filter?, sort?, offset?, limit?}` | Photos in the current view (or matching a catalog `Filter`) |
| `select_photos {ids, active?, mode?}` | Set the selection / active photo. Every id (and `active`) must be in the library: an unknown id is a tool error (`no such photo 9999`) and the selection and active photo are left as they were |
| `list_controls {section?}` | Every develop slider: id (`light.exposure`…), range, default, current value |
| `get_develop {id?}` | Full develop-settings JSON, with `process`: the rendering process the photo is on ([process-versions.md](process-versions.md)) |
| `set_develop {id?, values?, settings?, label?}` | `values`: `{controlId: number}`; `settings`: partial develop JSON deep-merged. Undoable. White balance: `values` with `wb.temp` / `wb.tint` is Kelvin on raws with colour matrices (DNG, and cameras LightCraft has matrices for) and relative (6500 = as shot) elsewhere; a `settings.wb` with `temp` / `tint` and no `"scale": "kelvin"` is read on the relative scale on every raw (see [control-protocol.md](control-protocol.md)) |
| `apply_preset {preset, amount?, ids?}` | Apply a preset (ids from `cmd_presets_list`) |
| `crop {id?, rect?, angle?, reset?}` | Normalized crop rect `[x0,y0,x1,y1]` and straighten angle; at least one of `rect`, `angle`, `reset: true` |
| `render_photo {id?, size?, format?, path?}` | Render with current settings → **image content** (PNG, or JPEG with `format: "jpeg"`), long edge `size` (default 1024) |
| `export {path \| dir, id? \| ids?, format?, longEdge? \| shortEdge? \| width?/height? \| megapixels? \| percent?, dontEnlarge?, ppi?, quality?, colorSpace?, bitDepth?, …}` | Full-quality render to `.png` / `.jpg` / `.tif` / `.webp` / `.avif`; `format: "original"` copies the file + an XMP sidecar with the edits, `format: "dng"` writes raw photos as DNG with the edits embedded. No size param = 3000 px long edge; `longEdge: 0` = full size (cropped, native resolution); `width` + `height` fit either orientation; `dontEnlarge` defaults to true |

Tools taking `id` make that photo active first; without it they act on the active photo.

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

## Resources

`resources/list` / `resources/read` serve JSON snapshots:

| URI | Content |
|---|---|
| `lightcraft://library` | Source, filter, sort, selection, undo/redo labels (`library.state`) |
| `lightcraft://photos` | Photos in the current view (`catalog.query`) |
| `lightcraft://photo/active` | Everything about the active photo (`photo.inspect`) |
| `lightcraft://develop/active` | The active photo's develop settings (`develop.get`) |
| `lightcraft://controls` | Every develop control with its current value (`develop.controls`) |

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
lightcraft-cli commands [--json]   # the command registry
lightcraft-cli controls [--json]   # develop control ids and ranges
lightcraft-cli calibrate --max 300 ~/Pictures/2026   # camera colour profiles (docs/camera-preview-colour.md)
```

## Tests

- `crates/mcp/tests/e2e.rs` — M0.9 acceptance: over the stdio framing, set exposure and render;
  checks the decoded PNG gets brighter/darker. Runs headless and through the TCP transport
  (`Remote`) against a stand-in control server; also import → render → JPEG export of a real file.
- `apps/lightcraft-cli/tests/cli.rs` — spawns `lightcraft-cli mcp` with real pipes; `render`;
  `commands`.

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

Connect mode and exports inside `command_batch` remain synchronous and do not report MCP
progress or cancellation. Use a direct headless export call for this behavior.
