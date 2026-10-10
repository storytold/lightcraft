# Plug-ins (SDK, ABI v1)

The app runs plug-ins as WebAssembly modules in a sandbox: `wasmi`, a pure-Rust interpreter (crate
`crates/plugins`, `dac-plugins`). A plug-in can only do what the user grants it. Lightroom Lua
plug-ins are not supported.

## What a plug-in can add

| Contribution | Manifest | Request it receives |
|---|---|---|
| Menu commands, optionally with a dialog | `commands: [{id, label, menu, dialog}]` | `{"hook":"command","command","args","selection"}` |
| Export post-processing | `hooks.export: true` | `{"hook":"export","files":[{path, photo}]}`; the exported files are lent to the call for reading and writing |
| Metadata and keyword suggestions | `hooks.metadata: true` | `{"hook":"metadata","photos":[photo.inspect JSON]}` → `{"suggestions":[{photo, fields?, keywords?}]}` |
| A publish service (kind `plugin:<id>` in `publish.addService`) | `publish: {id, name}` | `{"hook":"publish.upload","service","item":{path, photo, title, caption, keywords, remoteId?}}` → `{remoteId, url?}`; `{"hook":"publish.delete","service","remoteId"}` |

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
| `network: ["api.example.com", "*.example.org"]` | `http.request` to those hosts only. Redirects are not followed: the plug-in has to request the new URL itself, and that request is checked again. |
| `fs: [{path: "/abs/dir", write?}]` | `fs.readText`, `fs.list`, `fs.exists` below each root, and `fs.writeText` below writable roots. `..` is rejected, and symbolic links are resolved so a link cannot lead out of a root. |

Every plug-in can always use its own **metadata namespace** (`ns.get {photo}`,
`ns.set {photo, key, value}`, `ns.photos`). The namespace is stored next to the module, never in
the catalog.

## Limits

Each call runs in a fresh instance. It has an instruction budget, a wall-clock budget (time spent
in host requests counts), a memory cap of 256 MiB by default, a recursion cap, a 16 MiB cap on
each message, and at most 10 000 host calls. A module that breaks any of these, traps, or returns
bad JSON fails with an error and is logged; the app keeps running. A module can have no imports
other than the three below (no WASI).

## The ABI

All messages are UTF-8 JSON. An `i64` result packs `len << 32 | ptr`, pointing into the module's
memory.

Exports: `memory`, `dac_abi_version() -> i32` (returns 1), `dac_manifest() -> i64`,
`dac_alloc(len: i32) -> i32`, `dac_handle(ptr: i32, len: i32) -> i64`. `dac_handle` replies
`{"ok": value}` or `{"error": "message"}`.

Imports (module `host`):

- `call(ptr, len) -> i32`: sends `{"method", "params"}` and returns the length of the reply,
  which is `{"ok": value}` or `{"error": "message"}`;
- `response(ptr)`: copies that reply into a buffer of that length;
- `log(ptr, len)`: adds one line to the plug-in's log, shown in the Plugin Manager.

## Writing one in Rust

The guest SDK is `sdk/plugin` (`dac-plugin-sdk`). An example that uses every hook is in
`sdk/examples/catalog-tools`.

```sh
rustup target add wasm32-unknown-unknown
cd sdk/examples/catalog-tools
cargo build --release --target wasm32-unknown-unknown
# install target/wasm32-unknown-unknown/release/catalog_tools.wasm:
#   File ▸ Plug-in Manager…, or  <cli> commands plugin.install '{"path": "…"}'
```

```rust
use dac_plugin_sdk::{export_plugin, host, Request};
use dac_plugin_sdk::serde_json::{json, Value};

const MANIFEST: &str = r#"{"id": "org.example.hello", "name": "Hello",
  "permissions": {"catalog": true}, "commands": [{"id": "count", "label": "Count Photos"}]}"#;

fn handle(req: &Request) -> Result<Value, String> {
    match req.hook() {
        "command" => host::call("catalog.stats", json!({})),
        other => Err(format!("unsupported hook {other}")),
    }
}
export_plugin!(MANIFEST, handle);
```

`dac-plugins`' test suite (`crates/plugins/tests/example.rs`) builds the example to wasm32 and
runs every hook of it. Without the wasm32 target installed, that test is skipped.

## Engine commands

`plugin.list`, `plugin.inspectFile {path}`, `plugin.install {path, grant?}`,
`plugin.uninstall {id}`, `plugin.enable {id, enabled}`, `plugin.grant {id, grant}`,
`plugin.log {id}`, `plugin.commands`, `plugin.run {plugin, command, args?, ids?}`,
`plugin.suggestMetadata {ids?}`, `plugin.postExport {files}`, `plugin.publishServices`.

Plug-ins are installed per user in `<settings_dir>/plugins`, or in `<PREFIX>_PLUGINS` if that
is set. That folder holds `<id>.wasm`, `plugins.json` (enabled state and grants) and
`data/<id>.json` (the plug-in's namespace). A module copied into the folder by hand loads
disabled and with no permissions granted.

## Not done yet

- Export hooks run after exports from the app's Export dialog (synchronously, on the UI thread);
  the CLI's and MCP's exports don't call them yet (`plugin.postExport` does it by hand).
- No binary file I/O for plug-ins yet (only text), and no WebDAV example plug-in.
