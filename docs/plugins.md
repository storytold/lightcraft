# Plug-ins (SDK, ABI v1)

The app runs plug-ins as WebAssembly modules in a sandbox: `wasmi`, a pure-Rust interpreter (crate
`crates/plugins`, `lcc-plugins`). A plug-in can only do what the user grants it. Lightroom Lua
plug-ins are not supported.

## What a plug-in can add

| Contribution | Manifest | Request it receives |
|---|---|---|
| Menu commands, optionally with a dialog | `commands: [{id, label, menu, dialog}]` | `{"hook":"command","command","args","selection"}` |
| Export post-processing | `hooks.export: true` | `{"hook":"export","files":[{path, photo}]}`; the exported files are lent to the call for reading and writing |
| Metadata and keyword suggestions | `hooks.metadata: true` | `{"hook":"metadata","photos":[photo.inspect JSON]}` → `{"suggestions":[{photo, fields?, keywords?}]}` |
| A publish service (kind `plugin:<id>` in `publish.addService`) | `publish: {id, name}` | `{"hook":"publish.upload","service","item":{path, photo, title, caption, keywords, remoteId?}}` → `{remoteId, url?}`; `{"hook":"publish.delete","service","remoteId"}` |
| Export destinations ("Then send to" in the Export dialog) | `exportDestinations: [{id, name, dialog}]` | `{"hook":"export.destination","destination","args","files":[{path, photo}]}` after the files are written and the export hooks ran; the files are lent to the call; a `message` in the reply is shown |
| Custom metadata fields (Metadata panel ▸ Plug-in fields) | `metadataFields: [{key, label, type, …, readOnly?}]` | none: the values are the plug-in's namespace entries under each field's `key` (`ns.set {photo, key, value}`); the user edits them in the panel on every selected photo (`plugin.setField {ids}`; differing values read `< mixed >`), read-only ones only the plug-in sets. The plug-in's data stays the source; the app keeps an in-memory index of it (`Photo.plugin_fields`, never saved with the catalog, so its format is unchanged: rebuilt when a library opens and after every plug-in command and export hook): Library filter text search finds the values (`key:value` for one field), smart-album and filter rules test them as `plugin:<id>/<key>` (Rules ▸ Plug-in Fields; text operators, number fields compare as numbers: is ≥, is between, …), and XMP sidecars carry them as `lcpf:fields` (a bag of `{lcpf:plugin, lcpf:key, lcpf:value}`, namespace `<homepage>/ns/plugin-fields/1.0/`, from `homepage` in `brand.toml`) when metadata is saved, and give them back to the plug-in's data on import, Synchronize Folder and Read Metadata from File (installed, enabled plug-ins only; read-only fields included; values the field doesn't take are skipped) |

Commands appear under File ▸ Plug-in Extras and run as the engine command `plugin.run {plugin, command, args?, ids?}`. The UI, CLI,
control channel and MCP all use this command. `plugin.commands` lists the commands so front
ends can build menus and dialogs. Dialog fields are `{key, label, type}`, where `type` is one of
`text` (`default`), `number` (`min`, `max`, `default`), `bool` (`default`) or `choice`
(`options`, `default`). The host checks the values against the fields and clamps numbers before
the plug-in sees them.

## Permissions

A plug-in gets nothing it did not ask for in `permissions`, and nothing the user did not approve.
Installing shows every requested permission with a checkbox, unticked, and grants only the ones the
user ticks; an update shows what it newly asks for and keeps nothing beyond the previous grant
unless approved. Through the engine, `plugin.install` without `grant` grants nothing new. The Plugin Manager can revoke a permission at any
time, and the change applies from the next call. In every call, the plug-in's effective rights
are the granted permissions intersected with the requested ones.

| Permission | Allows |
|---|---|
| `catalog: true` | `catalog.query`, `photo.inspect`, `catalog.stats`, `keyword.list` (read-only) |
| `metadataWrite: true` | `metadata.setStandard {ids, title?, caption?, keywords?, addKeywords?, …}` |
| `network: ["api.example.com", "*.example.org"]` | `http.request` to those hosts only. Redirects are not followed: the plug-in has to request the new URL itself, and that request is checked again. A binary body goes as `bodyBase64` (with `contentType`) instead of `body`. |
| `fs: [{path: "/abs/dir", write?}]` | `fs.readText`, `fs.readBytes`, `fs.list`, `fs.exists` below each root, and `fs.writeText`, `fs.writeBytes`, `fs.remove` (files only) below writable roots. Bytes travel as base64 (`{path}` → base64 text; `{path, base64}`). `..` is rejected, and symbolic links are resolved so a link cannot lead out of a root. Files lent to a call (an export hook's or destination's exported files, a publish upload's rendered file) can be read and rewritten without any root, never removed. |

Every plug-in can always use its own **metadata namespace** (`ns.get {photo}`,
`ns.set {photo, key, value}`, `ns.photos`). The namespace is stored next to the module, never in
the catalog. Photo ids are only unique within one library, so the namespace is kept per library:
a plug-in always sees the data of the library that is open (each library gets a stable
`libraryId` in its `catalog-settings.json` the first time; a library without a folder, e.g. one
held in memory, shares one unnamed namespace).

## Limits

Each call runs in a fresh instance. It has an instruction budget, a wall-clock budget (time spent
in host requests counts), a memory cap of 256 MiB by default, a recursion cap, a 16 MiB cap on
each message, and at most 10 000 host calls. A module that breaks any of these, traps, or returns
bad JSON fails with an error and is logged; the app keeps running. A module can have no imports
other than the three below (no WASI).

## The ABI

All messages are UTF-8 JSON. An `i64` result packs `len << 32 | ptr`, pointing into the module's
memory.

Exports: `memory`, `lcc_abi_version() -> i32` (returns 1), `lcc_manifest() -> i64`,
`lcc_alloc(len: i32) -> i32`, `lcc_handle(ptr: i32, len: i32) -> i64`. `lcc_handle` replies
`{"ok": value}` or `{"error": "message"}`.

Imports (module `host`):

- `call(ptr, len) -> i32`: sends `{"method", "params"}` and returns the length of the reply,
  which is `{"ok": value}` or `{"error": "message"}`;
- `response(ptr)`: copies that reply into a buffer of that length;
- `log(ptr, len)`: adds one line to the plug-in's log, shown in the Plugin Manager.

## Writing one in Rust

The guest SDK is `sdk/plugin` (`lcc-plugin-sdk`). Five examples, each a complete plug-in:

| Example | Shows |
|---|---|
| `sdk/examples/catalog-tools` | every hook in one place: a menu command with a dialog, the plug-in's own namespace, a metadata provider, an export hook and a publish service |
| `sdk/examples/batch-metadata` | a small useful tool (`catalog` + `metadataWrite`): *Title / Caption from Template…* fills the title or caption of the selected photos from a template with tokens (`{name}`, `{file}`, `{date}`, `{year}`, `{camera}`, `{n}` with a start number); *Add Keywords…* adds comma-separated keywords. Every write goes through `metadata.setStandard` |
| `sdk/examples/export-tools` | **export hooks**: a post-process hook that reads each exported file (binary, `host::read_bytes`) and records its CRC-32; an **export destination** "WebDAV Upload" that `PUT`s the files to a server (`host::http_bytes`, basic auth, a dry-run switch) |
| `sdk/examples/publish-folder` | a **publish service** "Local Gallery": copies each rendered photo into a gallery folder (one writable `fs` root), keeps an `index.json`, replaces on re-publish and deletes on removal |
| `sdk/examples/custom-fields` | **custom metadata fields** Client / Job / Usage (editable in the Metadata panel) and Delivered (read-only), a *Set Job Fields…* command for the selection, an export hook that marks exported photos delivered, and a metadata provider that suggests the client as a keyword |

```sh
rustup target add wasm32-unknown-unknown
cd sdk/examples/catalog-tools
cargo build --release --target wasm32-unknown-unknown
# install target/wasm32-unknown-unknown/release/catalog_tools.wasm:
#   File ▸ Plug-in Manager…, or  <cli> commands plugin.install '{"path": "…"}'
```

`lcc-plugins`' test suite (`crates/plugins/tests/example.rs`) builds every example to wasm32 and
runs each hook and command of them. Without the wasm32 target installed, those tests are skipped.

## Guide for plug-in authors

### 1. Start a crate

A plug-in is a `cdylib` of its own (outside the app's workspace) that depends on the SDK:

```toml
[package]
name = "hello"
edition = "2024"

[workspace]

[lib]
crate-type = ["cdylib", "rlib"]   # rlib: unit tests of your logic run natively

[dependencies]
lcc-plugin-sdk = { path = "…/sdk/plugin" }   # or a git dependency on this repository

[profile.release]
opt-level = "s"
panic = "abort"
lto = true
strip = true
```

### 2. Declare a manifest and handle requests

```rust
use lcc_plugin_sdk::{export_plugin, host, Request};
use lcc_plugin_sdk::serde_json::{json, Value};

const MANIFEST: &str = r#"{"id": "org.example.hello", "name": "Hello", "version": "1.0.0",
  "permissions": {"catalog": true}, "commands": [{"id": "count", "label": "Count Photos"}]}"#;

fn handle(req: &Request) -> Result<Value, String> {
    match req.hook() {
        "command" => {
            let stats = host::call("catalog.stats", json!({}))?;
            Ok(json!({"message": format!("{} photos", stats["photos"])}))
        }
        other => Err(format!("unsupported hook {other}")),
    }
}
export_plugin!(MANIFEST, handle);
```

`export_plugin!` exports the ABI; `handle` gets every request. Reply with `Ok(value)` or
`Err(message)`: an error is shown to the user and written to the plug-in's log. A `message` in a
command's or destination's reply is shown as a notice.

The manifest fields:

| Field | Meaning |
|---|---|
| `id` | stable, `[A-Za-z0-9._-]{1,64}`, reverse-DNS style; an install with the same id is an update |
| `name`, `version`, `description`, `author` | shown in the Plug-in Manager |
| `permissions` | what the plug-in asks for (see [Permissions](#permissions)); ask for as little as possible |
| `commands` | `[{id, label, menu?, dialog?}]`, File ▸ Plug-in Extras ▸ `name` ▸ `menu` ▸ `label` |
| `hooks` | `{export?: true, metadata?: true}` |
| `publish` | `{id, name}`: one publish service |
| `exportDestinations` | `[{id, name, dialog?}]`, at most 16 |
| `metadataFields` | `[{key, label, type, …, readOnly?}]`, at most 32; `type` and its options as for dialog fields |

### 3. Requests by hook

| `req.hook()` | Fields (`req.get(…)`, helpers) | Reply |
|---|---|---|
| `command` | `req.command()`, `req.arg(key)`, `req.selection()` (photo ids as strings) | anything; `message` is shown |
| `export` | `req.files()` → `(path, photo)` of the written files | anything |
| `export.destination` | `req.destination()`, `req.arg(key)`, `req.files()` | anything; `message` is shown |
| `metadata` | `photos`: `photo.inspect` JSON | `{suggestions: [{photo, fields?, keywords?}]}` |
| `publish.upload` | `item: {path, photo, title, caption, keywords, remoteId?}` | `{remoteId, url?}`; the same `remoteId` comes back on a re-publish and on removal |
| `publish.delete` | `remoteId` | anything |

### 4. Call the host

| SDK | Host method | Needs |
|---|---|---|
| `host::call(method, params)` | any of the below | |
| `host::query(params)` | `catalog.query` (also `photo.inspect`, `catalog.stats`, `keyword.list` via `call`) | `catalog` |
| `host::call("metadata.setStandard", {ids, title?, caption?, keywords?, addKeywords?, …})` | writes standard fields | `metadataWrite` |
| `host::ns_get(photo)`, `host::ns_set(photo, key, value)` | `ns.get`, `ns.set`, `ns.photos` | nothing (own namespace; also holds `metadataFields` values) |
| `host::read_text`, `host::write_text`, `host::read_bytes`, `host::write_bytes`, `host::remove`, `host::exists` | `fs.*` | an `fs` root (or a lent file) |
| `host::http(method, url, headers, body)`, `host::http_bytes(…)` | `http.request` → `{status, contentType, location, body}` | the URL's host in `network` |
| `host::log(line)` | `host.log` | nothing; Plug-in Manager ▸ Log |
| `lcc_plugin_sdk::base64::{encode, decode}` | | |

### 5. Rules the host enforces

- Every call runs in a **fresh instance**: keep state in the namespace, not in statics.
- **Budgets per call**: instructions (fuel), wall-clock time (network time counts), 256 MiB of
  memory, call depth, 16 MiB per message, 10 000 host calls, 8 MiB per file or HTTP body. Batch
  work in chunks the user can re-run rather than one huge call.
- Everything a plug-in returns is **checked**: dialog values are coerced to their fields before
  the plug-in sees them, `metadataFields` values that no longer fit the field show as empty, and a
  reply that is not JSON, too large or late is an error. Nothing a plug-in does can crash the app.
- **Permissions are the user's**: each requested capability is a separate checkbox on install,
  unticked; an update that asks for more gets nothing new until approved; the user can revoke any
  permission later. Handle `permission … not granted` errors with a clear message.
- Paths must be **absolute** without `..`; links can't lead out of a root.

### 6. Test and ship

- Unit-test your logic natively (`cargo test`; the host calls answer "no host" off wasm).
- Build `--release --target wasm32-unknown-unknown`, then install from File ▸ Plug-in Manager…
  (*Install…* shows every permission for approval) or `<cli> commands plugin.install`.
- Drive it without the UI: `plugin.run`, `plugin.exportTo`, `plugin.setField`, `plugin.log` from
  the CLI, the control channel or MCP.
- Distribute the `.wasm` file. There is no store and no signing yet: tell users what each
  permission is for.

## Engine commands

`plugin.list`, `plugin.inspectFile {path}`, `plugin.install {path, grant?}`,
`plugin.uninstall {id}`, `plugin.enable {id, enabled}`, `plugin.grant {id, grant}`,
`plugin.log {id}`, `plugin.commands`, `plugin.run {plugin, command, args?, ids?}`,
`plugin.suggestMetadata {ids?}`, `plugin.postExport {files}`, `plugin.publishServices`,
`plugin.exportDestinations`, `plugin.exportTo {plugin, destination, files, args?}`,
`plugin.metadataFields {id?}`, `plugin.setField {plugin, field, value, id?}`.

`app.export` runs the enabled export hooks after every export (the Export dialog, Export with
Preset / Previous, the CLI, the control channel and MCP) and, with
`pluginDestination: {plugin, destination, args?}`, hands the written files to that destination;
the results come back as `plugins: [{plugin, destination?, ok | error}]`.

In the app, a background export (the Export dialog, Export with Preset / Previous) runs them on a
worker thread once the files are written, with a row in the activity stack ("Running plug-in
export hooks", kind `plugins`; detail: the plug-in in progress), so a long upload doesn't hold
the window. ✕ stops before the next plug-in: a call that has started (a plug-in's code) runs to
its end or its time limit. A plug-in's catalog reads and metadata writes are answered on the UI
thread, with the same permission checks. The Plugin Manager stays usable meanwhile; a plug-in that
is still busy with the export refuses other calls until it's done ("busy"). The results go to the
toast and to `ui.inspect` → `export.last.plugins` when they finish. An export run synchronously
(`app.export` without `background`, the CLI, MCP) runs them before it returns, as before.

Plug-ins are installed per user in `<settings_dir>/plugins`, or in `<PREFIX>_PLUGINS` if that
is set. That folder holds `<id>.wasm`, `plugins.json` (enabled state and grants) and
`data/<id>.json` (the plug-in's namespace: `{"format": 2, "libraries": {<library id>: {<photo id>:
{…}}}}`; a file written before namespaces were kept per library is claimed whole by the first
library opened from a folder, so no value is lost). A module copied into the folder by hand loads
disabled and with no permissions granted.

## Pixel filters (PhotoCraft ABI)

Besides the plug-ins above, <app> runs **pixel filters** written for PhotoCraft's plug-in ABI v1
(the `lcc-filters` crate, `crates/filters/README.md`): a module with no imports exporting
`memory`, `pc_abi_version`, `pc_manifest`, `pc_alloc` and `pc_filter`. They install the same way
(File ▸ Plug-in Manager…, or `<cli> commands plugin.install '{"path": "film.wasm"}'`): the host
sniffs the exports and keeps filter modules in the same folder, in a registry of their own.
`plugin.list` shows each as `{id, name, version, kind: "filter", params, moduleHash, analyze}`
(`params` is the manifest's spec); `plugin.uninstall` removes one. Filters ask for no permissions.

A photo's filter stack is part of its develop settings (`develop.pluginFilter.add {plugin,
stage?, params?}` — `params` are checked against the installed manifest and defaults filled in —
`.set`, `.move`, `.remove`, `.list`); every change is one History step, and the stack is copied,
synced and written to XMP with the other settings. Two stages:

| stage | runs | samples handed over |
| --- | --- | --- |
| `source` | after geometry (crop, lens, perspective), before white balance, tone and every other step | linear light, sRGB primaries, `"encoding": "linear"` |
| `display` (default) | after the develop adjustments, before quantisation | sRGB-curve encoded in the output's primaries, `"encoding": "srgb"`; linear for HDR renders |

Each filter gets its resolved parameters plus `encoding`, `_analysis` (below) and `_image` (the
band's place in the whole output frame, so a zoomed view and an export line up). Source results
are cached: dragging other sliders doesn't re-run the filter; a draft while a filter's own
parameter changes runs it on a reduced copy (at most 0.5 MP), the full render after it is exact.
Renders with plug-in filters use the CPU pipeline. A filter that isn't installed or fails is
skipped: the render goes on and reports a warning (logged as `render: …`).

**Whole-image measurement (ABI v1.1 proposal).** A module may export
`pc_analyze(buf, len, w, h, channels, format, params, params_len) -> i64` returning
`len << 32 | ptr` of a JSON object (0 = nothing). `develop.pluginFilter.analyze {uid}` runs it
once on the whole uncropped photo (long edge 1024, linear sRGB primaries) and stores the result
with the photo (`setAnalysis`). It runs in the background as an activity row (kind
`pluginAnalyze`, cancellable) and returns at once; `wait: true` runs it to the end and returns the
result. Every later `pc_filter` call gets it as `_analysis`, so crop and
zoom don't move the measurement. Modules without the export give an "unsupported" error.

**Pixel scale.** A Source filter that reads neighbours (manifest `overlap` > 0: an unsharp mask,
grain, a blur) would look different at each render size, its kernel spanning more of the photo in
a small preview than in a full-size export. Such a stack runs once on the whole frame at the
source's own resolution (capped at 100 MP, 24 MP on the web) and every render (preview, loupe,
zoomed window, export at any size) is resampled from that result, so they agree. The result is
kept per photo, frame and stack: other sliders, zooming and a later export reuse it; a draft that
finds none shows the reduced-copy proxy until the full render replaces it. Per-pixel filters
(`overlap` 0) run on the render's own pixels.

Auto (tone), Auto B&W Mix, Upright (and Auto straighten), the auto masks (with or without a
model), automatic spot sources and a zoomed view's dehaze estimate read the photo as the Source
filters turn it (long edge 2048, cached per photo and stack; a change of the stack re-runs them).

## Not done yet

- A plug-in call that has started can't be interrupted (✕ takes effect between plug-ins), and one
  export's hooks run at a time: hooks for an export that ends while the previous ones still run
  are skipped with a message.
- Network and filesystem permissions are fixed in the manifest: a WebDAV destination for "any
  server the user types" isn't possible; the author names the hosts.
- No plug-in store, signing or automatic updates; Lightroom Lua plug-ins don't run.
- Pixel filters: a Display filter with `overlap` > 0 still works in output pixels (it runs after
  every develop step, so a native-resolution pass would mean a full-size develop per preview), so
  a preview and a larger export of one differ.
