# Plug-ins

LightCraft runs **command plug-ins**: small WebAssembly modules that edit the library through the
same commands as the menus, the CLI, the control channel and the MCP server. They run in the
sandbox of the shared Craft plug-in runtime, [`craft-plugin`](https://github.com/storytold/craft-libs/tree/main/crates/plugin)
([RFC-0046](https://github.com/storytold/craft-contrib/blob/main/rfcs/0046-shared-plugin-runtime.md)):
no file, no network, no clock, and a budget of instructions, memory and calls. The same module can
work in several Craft apps when it was written for each (`apps` in its manifest); LightCraft runs
only plug-ins whose manifest names `lightcraft`.

## Using plug-ins

- **File ▸ Plug-in Manager…** lists the installed plug-ins (what they may do and where they came
  from), removes them, and installs new ones: **Install from File…** (a `.craftplugin` bundle), from
  a **GitHub repository** (`owner/repository`, or `owner/repository@tag` for a given release), or
  from a **catalog** (an `index.json` added with its URL; **Show Packages** lists what it offers).
- Before anything is installed a confirmation shows the plug-in's permissions, its kind, where it
  comes from and its SHA-256, and says plainly when it is in no catalog you use. Downloads run in the
  background, with a row in the activity stack that can be cancelled; the confirmation opens when the
  bundle has arrived and passed its checks.
- **File ▸ Plug-in Extras** lists the commands of the installed plug-ins. A command with parameters
  asks for them first, in a dialog made from its manifest.
- A run is **one undo step** named after the plug-in: Edit ▸ Undo takes all of it back. A run that
  fails (a crash, an endless loop, a refusal it doesn't handle) leaves the library, the selection and
  the view as they were. A plug-in acts on the photos it names or on the selection; its input lists
  the selected photo ids (`selection`) and the active one (`active`).
- The desktop app keeps plug-ins in `Plug-ins/` in its settings folder (one folder per plug-in,
  named by a hash of its id, with the bundle and where it came from), and each plug-in's settings
  (at most 64 KB each) and the catalogs added in `plugin-settings.json` beside it. The CLI, the MCP
  server and `--memory` sessions keep plug-ins for the session only.

## Commands

| Command | Parameters |
|---|---|
| `plugin.list` | `{}` → the installed plug-ins, problems (damaged folders, plug-ins this version can't run), catalogs, whether they are stored |
| `plugin.install` | `path` \| `data` (base64) \| `repo` (`owner/repo[@tag]`) \| `catalog` + `id` (+ `version`); `review: true` checks and describes the bundle without installing (the confirmation), `sha256` installs only the bundle reviewed (without downloading it again) |
| `plugin.update` | `{id?, apply?}`: checks each plug-in's catalog or repository; `apply` installs the updates that ask for no other permissions (the others need `plugin.install` again) |
| `plugin.remove` | `{id}`: the plug-in and its settings |
| `plugin.run` | `{id, command?, params?, fuel?}` |
| `plugin.catalog.list` | `{url?}`: the packages LightCraft can run in one catalog, or in every catalog added |
| `plugin.catalog.add` | `{url, remove?}`: an `https://` URL on GitHub's hosts, or `file://` |

Downloads are `https://` on GitHub's hosts only, every redirect included; a catalog pins each
release by URL, size and SHA-256, and a repository release must publish its bundle's SHA-256 in
`SHA256SUMS`.

## Permissions

A manifest asks for `read` or `write` access, and may narrow the commands it calls
(`permissions.commands`, exact ids or prefixes ending in `*`). A `read` plug-in that edits anything
has that call undone and refused.

Whatever the manifest says, a plug-in may call only the commands on LightCraft's list
(`ALLOWED` in `crates/engine/src/plugin.rs`); every other command, including any added to
LightCraft later, is refused until it is put on the list. What is on it:

- reading: `catalog.query`, `photo.inspect`, `develop.get`, `albums.list`, `keyword.list`,
  `history.list` and the other lists;
- the selection and the view: `library.select*`, `library.next` / `previous`, `library.source`,
  `library.filter`, `library.sort`;
- ratings, flags, colour labels, rotation, metadata (`photo.setMeta`, `photo.setCaptureTime`,
  `metadata.applyPreset`), face regions, virtual copies, keywords (`keyword.create`, `keyword.edit`);
- albums (create, add and remove photos, rename, rules, cover) and stacks;
- develop settings (`develop.*` but the interactive drags, Auto Sync and the clipboard), applying
  presets, crop and geometry, masks, spots, red eye, versions and history steps.

Refused, and why:

- **files and folders**: importing, exporting, printing, slideshow export, renaming, moving,
  deleting (also to Recently Deleted), relinking, reloading, converting to DNG, XMP sidecars,
  external editors, `folder.*` and the library's own location and housekeeping;
- **the undo history** (`edit.*`, `history.clear`) and the interactive drags, which would split a
  run into several steps;
- **the person's own things**: the develop and metadata clipboards, presets, profiles, effect
  presets, keyword and label sets, the target album, Auto Sync, preferences;
- **downloads, models and long background work** (AI denoise, faces, AI masks, Photo Merge, the lens
  database, `photo.analyze`, reverse geocoding) and the activity stack;
- commands that change every photo that has something rather than the photos asked for (renaming,
  merging, moving or deleting a keyword or a person);
- **plug-ins themselves** (`plugin.*`), and any parameter that names a file or folder (`path`,
  `file`, `folder`, `destination`…), at any depth.

A run may make at most 500 edits, so all of them can still be put back. Plug-in panels (`ui_show`)
are not shown in LightCraft yet: `plugin.run` says so in its answer.

## Writing a plug-in

The ABI, the manifest (`craft-plugin.json`), the bundle format and publishing a release are
described in the [craft-plugin README](https://github.com/storytold/craft-libs/tree/main/crates/plugin).
Name `lightcraft` in `apps` and call LightCraft's command ids, listed with their parameters by
`lightcraft-cli commands`.
