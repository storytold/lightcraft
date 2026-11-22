# Plug-ins (plugins): SDK, ABI v1

The app runs plug-ins as WebAssembly modules in a sandbox: `wasmi`, a pure-Rust interpreter (crate
`crates/plugins`, `lcc-plugins`). A plug-in can only do what the user grants it. Lightroom Lua
plug-ins are not supported.

## What a plug-in can add

| Contribution | Manifest | Request it receives |
|---|---|---|
| Menu commands, optionally with a dialog | `commands: [{id, label, menu, dialog, prepare?, job?}]` (`job: true`: see [Jobs](#jobs-long-commands-with-progress)) | `{"hook":"command","command","args","selection","source"}`; with `prepare: true`, first `{"hook":"command.prepare","command","selection","source"}` (`source`: see [Adding files to the library](#adding-files-to-the-library); see [Settings, remembered values and prepare steps](#settings-remembered-values-and-prepare-steps)) |
| Quick settings bar buttons | `quickAction: true` on a command (at most 4 a plug-in; host feature `quickActions`) | the command's own `{"hook":"command",…}` request, exactly as from its menu item (see [Quick actions](#quick-actions)) |
| Settings (the plug-in's page in the Plug-in Manager) | `settings: [{key, label, type, …, prepare?}]` | none, or `{"hook":"settings.prepare"}` when a field has `prepare: true`; the plug-in reads them with `settings.get` |
| Export post-processing | `hooks.export: true` | `{"hook":"export","files":[{path, photo}]}`; the exported files are lent to the call for reading and writing |
| Metadata and keyword suggestions | `hooks.metadata: true` | `{"hook":"metadata","photos":[photo.inspect JSON]}` → `{"suggestions":[{photo, fields?, keywords?}]}` |
| A publish service (kind `plugin:<id>` in `publish.addService`) | `publish: {id, name}` | `{"hook":"publish.upload","service","item":{path, photo, title, caption, keywords, remoteId?}}` → `{remoteId, url?}`; `{"hook":"publish.delete","service","remoteId"}` |
| Export destinations ("Then send to" in the Export dialog) | `exportDestinations: [{id, name, dialog}]` | `{"hook":"export.destination","destination","args","files":[{path, photo}]}` after the files are written and the export hooks ran; the files are lent to the call; a `message` in the reply is shown |
| Programs it starts (an installed tool, such as a raw converter) | `permissions.programs: [{id, name, macBundleIds?, windowsExe?, linuxExe?}]` | `{"hook":"program.end","run","program","status","code","tmpDir"}` when a run ends (see [Programs](#programs)) |
| Custom metadata fields (Metadata panel ▸ Plug-in fields) | `metadataFields: [{key, label, type, …, readOnly?}]` | none: the values are the plug-in's namespace entries under each field's `key` (`ns.set {photo, key, value}`); the user edits them in the panel on every selected photo (`plugin.setField {ids}`; differing values read `< mixed >`), read-only ones only the plug-in sets. The plug-in's data stays the source; the app keeps an in-memory index of it (`Photo.plugin_fields`, never saved with the catalog, so its format is unchanged: rebuilt when a library opens and after every plug-in command and export hook): Library filter text search finds the values (`key:value` for one field), smart-album and filter rules test them as `plugin:<id>/<key>` (Rules ▸ Plug-in Fields; text operators, number fields compare as numbers: is ≥, is between, …), and XMP sidecars carry them as `lcpf:fields` (a bag of `{lcpf:plugin, lcpf:key, lcpf:value}`, namespace `<homepage>/ns/plugin-fields/1.0/`, from `homepage` in `brand.toml`) when metadata is saved, and give them back to the plug-in's data on import, Synchronize Folder and Read Metadata from File (installed, enabled plug-ins only; read-only fields included; values the field doesn't take are skipped) |

**Its own pictures.** A plug-in's manifest may carry an `"icon"` and a `"header"`, as a pixel
filter's does ([A filter's own pictures](#pixel-filters-photocraft-abi)): the icon, a square SVG,
in place of the one the Plug-in Manager draws from the id; the header (`"<svg…>"`, or `{"dark":
"<svg…>", "light": "<svg…>"}`) in place of the plug-in's name on its page in the Plug-in Manager,
and above the title of its command dialogs. The same rules apply: at most 24 KiB each, within the
manifest's 64 KiB, drawn as untrusted documents (their own shapes only), and a picture that isn't
an SVG document is dropped, and named in a plug-in's log and in `plugin.inspectFile`'s `dropped` (`icon: not an SVG
document of at most 24 KiB`, `header.light: …`). `plugin.list` shows `icon` and `header` as booleans, and
`plugin.picture {id, picture?, theme?}` gives the SVG text of an installed one, for plug-ins and pixel filters alike.
`plugin.inspectFile` gives a file's pictures before it is installed, as `manifest.icon` (the SVG text or `null`) and
`manifest.header` (`{dark, light}`, each the SVG text or `null`; `null` for none).
The host feature `pictures` names them (a plug-in may require it; one that just looks better with
them needn't, as an older host ignores them).

**Where it comes from.** A manifest may carry `"homepage"`, `"repository"` and `"changelog"`
(`https://` links) and `"license"` (text, an SPDX expression such as `MIT OR Apache-2.0`), named as
in `Cargo.toml`. The Plug-in Manager's About tab shows each one present: Homepage, Source Code and
Changelog as links named by their host (`github.com`), so the user sees where a link goes before
clicking (the whole link shows on hover), and Licence as text. A `repository` on
`https://github.com/<owner>/<repo>`, or a page inside one (`…/tree/main/plugins/geo`), adds *Report
a Problem*, the repository's `/issues` page; any other host doesn't. Nothing is fetched to show the
tab: a link opens in the browser only when clicked, or through `plugins.openLink`. A value the host
can't use is dropped, never an install error: one that isn't a string, has control or invisible
characters (direction overrides, zero-width spaces), is over its cap (2 KiB a link, 256 bytes the
licence), or a link that isn't `https://` with a plain host (letters, digits, dots and dashes, an
optional port, no user name: `https://a.com@b.com/` is refused, as its real host is hidden). The
plug-in's log names each one dropped and why (`manifest value dropped: homepage: not an https://
link`), and so do the install dialog and `plugin.inspectFile` (`dropped`). `plugin.list` and
`plugin.inspectFile` (`manifest.about`) give them as `about: {homepage?, repository?, changelog?,
license?, issues?}`, `issues` being Report a Problem's page. The host feature `about` names them.
The SDK can fill them from the plug-in crate's `Cargo.toml` ([Writing one in
Rust](#2-declare-a-manifest-and-handle-requests)).

### Quick actions

A command with `"quickAction": true` also gets a button in the **quick settings bar**, the row of
switches that fades in when the pointer is in the blank stretch of the module bar between the
identity plate and the module names. Its buttons come after the app's own (info overlay,
on-screen overlay, eye focus strip, …), in install order, drawn with the plug-in's icon (its
`icon` picture, else the identicon the Plug-in Manager draws) and named by the command's `label`
on hover. A click runs the command as its File ▸ Plug-in Extras item does (`plugins.runItem
{plugin, command}`): a `prepare` step, a dialog and a job all work as from the menu. A plug-in
may flag at most 4 commands (`lcc_plugins::MAX_QUICK_ACTIONS`); more is a manifest error. Only
enabled plug-ins show; a bar too narrow for every button drops plug-ins' buttons first.
`view.quickSettings` lists the bar's entries, plug-ins' included (`plugin:<id>/<command>`), and
`plugin.commands` gives each command's `quickAction`.

The field is additive (ABI v1 is unchanged): a host without the bar ignores it as an unknown field,
and the command stays in the menu. A plug-in that needs the button to be useful says
`"requires": ["quickActions"]`.

```json
{"id": "org.example.tools", "name": "Tools", "version": "1.0.0",
 "commands": [{"id": "stamp", "label": "Stamp Copyright", "quickAction": true}]}
```

Commands appear under File ▸ Plug-ins and run as the engine command `plugin.run {plugin, command, args?, ids?, source?, wait?}`. The UI, CLI,
control channel and MCP all use this command. `plugin.commands` lists the commands so front
ends can build menus and dialogs. Dialog fields are `{key, label, type}`, where `type` is one of
`text` (`default`), `number` (`min`, `max`, `default`), `bool` (`default`) or `choice`
(`options`, `default`). The host checks the values against the fields and clamps numbers before
the plug-in sees them. A dialog opens with the values it last ran with (see below).

## Settings, remembered values and prepare steps

These need the host feature **`settings`**: a plug-in that uses any of them says
`"requires": ["settings"]` (see [Compatibility](#compatibility-requires-and-version)).

- **Settings** (`settings: [{key, label, type, …}]`, at most 32, the same field types as dialogs)
  are the plug-in's own options, kept for the whole app (not per library). The plug-in's page in
  the Plug-in Manager has a Settings tab with the fields, *Save* and *Revert*; a save that fails
  keeps the values typed and says why. The plug-in reads them with `settings.get` (every key; the default
  for one never set or no longer fitting its field) and may change one with
  `settings.set {key, value}` (checked against its field; `null` goes back to the default).
  Engine commands: `plugin.settings {id}` and `plugin.setSettings {id, values}`. Saving is
  all-or-nothing: an unknown key or a value that doesn't fit changes nothing.
- **Remembered dialog values.** A command's or an export destination's dialog opens with the
  values it last ran with (`values` in `plugin.commands` and `plugin.exportDestinations`). A field
  with `"remember": false` always opens with its default (a "delete instead" switch, say). The
  values are remembered whenever the command is run with arguments, from the UI or not, and stored
  with the settings.
- **Prepare steps.** A command with `"prepare": true` is asked first, with
  `{"hook":"command.prepare","command","selection"}`, how to run on this selection. It answers
  with exactly one of:
  - `{"dialog": {<key>: {options?, default?}}}`: open the dialog with these options and defaults
    (only the keys given change);
  - `{"run": {<key>: value}}`: run at once with these arguments, no dialog (checked like dialog
    values);
  - `{"message": "…"}`: nothing to do; the text (at most 4096 characters) is shown and nothing
    runs.

  A prepare step **can only read**, whatever the plug-in is granted: there, `metadata.setStandard`,
  `ns.set`, `settings.set`, `fs.writeText`, `fs.writeBytes`, `fs.remove`, `program.start` and any
  `http.request` other than `GET` or `HEAD` fail with "a prepare step can only read". The command
  itself may write as usual.

  A `choice` field with no `options` (`"options": []`) gets its options only from the prepare
  step; it is allowed only in a command with `prepare`. A settings field with `"prepare": true`
  is prepared the same way when the page opens: `{"hook":"settings.prepare"}` → `{"fields":
  {<key>: {options?, default?}}}`. A prepare step that fails (an error, a trap, a time-out, an
  answer that doesn't fit) is logged and the dialog opens as declared, with the reason shown; when
  a field's options can only come from it, the command doesn't run. At run time the command's
  arguments are checked against the prepared fields again. Engine command:
  `plugin.prepare {plugin, command, ids?}` → `{dialog: [field + value], error}` | `{run}` |
  `{message}`; it runs nothing. `plugin.run` asks the prepare step too: on a `message` answer it
  returns `{message, ran: false}` and runs nothing; on a `run` answer without `args` it runs with
  the plug-in's arguments; when the prepare step failed, it refuses to run without `args`;
  otherwise `args` are checked against the fields as prepared (as declared after a `run` answer).
  The UI asks the prepare step once to decide what to show, and `plugin.run` asks it again, so a
  prepare step should be cheap and give the same answer for the same selection.

## Permissions

A plug-in gets nothing it did not ask for in `permissions`, and nothing the user did not allow.
The manifest lists everything a plug-in may ask for; installing grants none of it. The install
dialog lists those requests for information, with nothing to tick. The first call that needs one
asks the user:

- **From a command the user started** (a menu item, a plug-in's dialog, the Plug-in Manager),
  its prepare step included, the call fails, the command ends, and a prompt names the plug-in and
  the permission: *Allow* grants it and runs the same command again, once (after a prepare step,
  the command starts again as from its menu, so its dialog may open); *Deny* is remembered with
  its date, and later calls that need it fail with "permission `X` not granted to this plug-in",
  asking nothing. A command that does something before the call that fails does it twice when
  the user allows: ask early (make the call that needs the permission first).
- **With nobody to ask** (the command line, `mcp` without `--connect`, an agent's `plugin.run`, a
  [job](#jobs-long-commands-with-progress) on a worker, and every hook: export hooks and
  destinations, publish services, metadata providers, `program.end`, settings' prepare steps,
  even after an export the user started) the call fails with "needs `X`, not granted to this
  plug-in; allow it in the Plug-in Manager (File ▸ Plug-ins ▸ Plug-in Manager…)", and nothing is
  recorded as asked.

The Plug-in Manager's Permissions tab shows each request as allowed, denied on a date, not asked
yet, or refused (below), with *Allow* and *Revoke*. Only the user allows: commands
(`plugin.install {grant}`, `plugin.grant`), whoever calls them (MCP, the control channel, an
action, the command line), may repeat or leave out what is granted, and refuse a grant that adds
anything, changing nothing; `plugin.revoke {id, group, value?}` takes one permission (or one
host, folder or program) away. The control channel's input requests (`ui.pointer`,
`ui.clickWidget`, `ui.key`) can't allow: *Allow* in the prompt or on the Permissions tab ignores
them and says so, and through them *Install* and *Go Back* act as `plugin.install` and
`plugin.rollback`. Revoking, disabling and removing work from anywhere, and a change applies from
the next call. An update or `plugin.rollback` keeps what was allowed for what the new version
still asks for, never more, and keeps the denials. `plugins.json` from before this model loads
unchanged, its grants as allowed.

A folder (`fs`) root that contains the settings folder or the plug-ins folder, or lies inside the
plug-ins folder, is never granted, by any path: the Permissions tab shows it as refused, with the
reason. A plug-in with such a root would be able to rewrite its own grant or another plug-in.

| Permission | Allows |
|---|---|
| `catalog: true` | `catalog.query`, `photo.inspect`, `catalog.stats`, `keyword.list` (read-only) |
| `metadataWrite: true` | `metadata.setStandard {ids, title?, caption?, keywords?, addKeywords?, …}` |
| `libraryAdd: true` | `library.add {path, from, stack?, album?}`: adds a file a tool made from a photo to the library, from that photo's folder only (see [Adding files to the library](#adding-files-to-the-library)). Requires `libraryAdd`. |
| `pixels: true` | `photo.pixels {photo, longEdge?}`: a downsized copy of a photo, the whole uncropped frame (see [Pixels and crop](#pixels-and-crop)). Requires `pixels`. |
| `virtualCopy: true` | `photo.virtualCopy {photo, name?}`: makes a virtual copy of a photo, stacked with it (see [Pixels and crop](#pixels-and-crop)). Requires `virtualCopy`. |
| `crop: true` | `develop.setCrop {photo, rect, angle?}`: sets a photo's crop, one History step, editable afterwards like any crop (see [Pixels and crop](#pixels-and-crop)). Requires `crop`. |
| `network: ["api.example.com", "*.example.org"]` | `http.request` to those hosts only. Redirects are not followed: the plug-in has to request the new URL itself, and that request is checked again. A binary body goes as `bodyBase64` (with `contentType`) instead of `body`. |
| `fs: [{path: "/abs/dir", write?}]` | `fs.readText`, `fs.readBytes`, `fs.list`, `fs.exists` below each root, and `fs.writeText`, `fs.writeBytes`, `fs.remove` (files only) below writable roots. Bytes travel as base64 (`{path}` → base64 text; `{path, base64}`). `..` is rejected, and symbolic links are resolved so a link cannot lead out of a root. Files lent to a call (an export hook's or destination's exported files, a publish upload's rendered file) can be read and rewritten without any root, never removed. A program run's folder is lent to its `program.end` call for reading. |
| `programs: [{id, name, macBundleIds?, windowsExe?, linuxExe?}]` | `program.start` of those programs, with the arguments the plug-in chooses on each run (see [Programs](#programs)); `program.list` needs nothing. Each program is allowed on its own, by the user, as any other permission. |

Every plug-in can always use its own **metadata namespace** (`ns.get {photo}`,
`ns.set {photo, key, value}`, `ns.photos`). The namespace is stored next to the module, never in
the catalog. Photo ids are only unique within one library, so the namespace is kept per library:
a plug-in always sees the data of the library that is open (each library gets a stable
`libraryId` in its `catalog-settings.json` the first time; a library without a folder, e.g. one
held in memory, shares one unnamed namespace).

## Programs

A plug-in can hand photos to a program installed on the computer (a raw converter, a denoiser) and
pick up what it made when it ends. It requires the host feature `programs` (`"requires":
["programs"]`), and each program is an entry in `permissions.programs`:

| Field | |
|---|---|
| `id` | the program's id in this plug-in (1–64 of `A-Z a-z 0-9 . _ -`) |
| `name` | shown to the user (install dialog, Plug-in Manager, activity stack) |
| `macBundleIds` | macOS: the app's bundle ids, newest version first (at most 8); each is a version the user may have installed |
| `windowsExe` | Windows: the executable's file name, looked up under `App Paths` in the registry (per user, per machine, then the machine's 32-bit view), then in the absolute folders on `PATH` |
| `linuxExe` | Linux and the BSDs: the executable's file name, looked up in the absolute folders on `PATH` |

An entry names at least one of the three, and at most 8 programs per plug-in. Executable names are
plain file names that are valid on every system (no folder, none of `<>:"/\|?*`, no device name
such as `CON` or `NUL.txt`, no trailing dot or space). On macOS the app finds each bundle id with
Spotlight and reads its version from the app's `Info.plist`; elsewhere the version is not read.
When Spotlight knows several copies, the one in `/Applications` or `~/Applications` is used, then
one elsewhere on the startup disk, then one on another volume (a mounted disk image, an external
drive); a copy in the Trash never is. A
lookup that takes more than 3 seconds is stopped and finds nothing. The install dialog and the
Plug-in Manager show each entry as a permission row with what it names on this system (the bundle
ids, or the executable) and where it is installed ("not installed" if nowhere, "checking…" while
the app looks; the dialog never waits for a lookup). An update keeps an allowed entry when
it names no more than before: renaming it, reordering or removing bundle ids and dropping an
executable keep it. An update that adds a bundle id or changes an executable drops the entry, with
a line in the plug-in's log, until the user allows it again: the user allowed particular programs,
not whatever the entry names later. An install from a command (`plugin.install`) whose module
differs from the installed one keeps none of the plug-in's programs, with a line in its log: the
user allowed them for the code they had. Installing the same module again keeps them.

**What a grant covers:** the user allows a program, not a particular use of it. The plug-in chooses
the arguments and the files it hands the program on every run (`program.start {args, files}`); the
host checks only their names and sizes, never what they ask the program to do. So a grant gives the
plug-in whatever that program can do with the arguments it accepts, and the install dialog and the
Plug-in Manager show which program it is, not how the plug-in will use it.

- **`program.list`** → `[{program, name, granted, installed: [{bundleId?, path?, version?}]}]` for
  every declared program, in manifest order (on macOS, installs in the entry's order). It needs no
  permission, so a plug-in can tell the user what to install or allow; `path`, where the program
  is installed, comes only once the program is granted.
- **`program.start {program, args?, files?, bundleId?}`** → `{run, tmpDir}`:
  1. it checks everything first: the program is declared and granted; `files` (`{name: text}`, at
     most 64 plain file names, 8 MiB in all) and `args` (at most 256 strings of 4 KiB) are valid;
     `bundleId` (macOS only) is one of the entry's;
  2. it makes a new folder for the run (private to the user) and writes `files` into it; every
     `{tmpDir}` in `args` becomes that folder's path;
  3. it starts the program with `args`, `TMPDIR` set to the folder (elsewhere `TEMP` and `TMP`
     too) and its standard input and output discarded: on macOS the first installed bundle id in the entry's order (or `bundleId`),
     as a new instance (`open -n -W -a`, with `--env` for `TMPDIR`); elsewhere the executable
     directly (on Windows with no console window).

  Nothing is written or started when a check fails, and a program that exits with an error at once
  is reported as not started. One run per program and plug-in at a time: a second start while a
  run is active is refused. A failed start adds a line to the plug-in's log (at most 20 per call).

  Only a plug-in command (`plugin.run`, a menu item) starts programs: export hooks, export
  destinations, metadata providers and `program.end` can list programs but get an error from
  `program.start` ("start the program from a command"), and so do a command's and the settings'
  prepare steps ("a prepare step can only read"). Publish services have neither (no
  catalog, no programs). A command's lookups (at most 3 seconds per lookup) and the check that the
  program didn't fail at once (300 ms) run on the thread that runs the command, in the app the UI
  thread, so a start can hold the window that long.
- **`program.end`**: when the program exits, the plug-in gets `{"hook":"program.end","run",
  "program","status","code","tmpDir"}` once, on a worker thread, with the run's folder lent for
  reading (`status`: `exited`, `cancelled` or `failed`; `code`: the program's exit code, or null on
  macOS, where `open` starts it, after a cancel, and for a program ended by a signal). Its catalog
  reads and writes are answered on the UI thread, as for export hooks; a session with no UI
  (headless MCP, the CLI) refuses them. `program.list` works there; `program.start` doesn't (start
  programs from a command, where the user is).
- **The activity stack** shows each run, named after the program and the plug-in ("Waiting for
  DxO PureRAW to finish (Edit in DxO PureRAW)", kind `pluginProgram`), with the detail "The results
  come back when DxO PureRAW closes". A run has no ✕: stopping the wait throws its results away, so
  the activity list (or the Modern layout's card) offers "Stop Waiting…", which asks first and says
  that what the program makes from then on won't come back and that the program keeps running.
  Confirming ends the run (`program.end` with `cancelled`) but leaves the program running: it
  belongs to the user now. `activity.list` reports the run's `stopWarning`; `activity.cancel` and
  Quit Anyway stop the wait without asking.
- **The run's folder** is deleted once `program.end` has returned and the program has exited, also
  when the hook fails. If the app quits first, the folder is left for the program; the next start
  of the app removes run folders more than a day old, except those of runs another copy of the app
  still watches (each run holds a lock file next to its folder).
- The web build has no programs: `program.start` returns an error there.

## Adding files to the library

A tool such as a raw converter or a noise reducer makes new files from the photos it is sent. A
plug-in with the **`libraryAdd`** permission adds them to the library, stacked on the photo each
came from or in the album the user sent them from. It requires the host feature `libraryAdd`
(`"requires": ["libraryAdd"]`, see [Compatibility](#compatibility-requires-and-version)), which
covers the permission, `library.add` and `source`.

- **`source`**: the `command` and `command.prepare` requests say where the command was started:
  the Library source shown in the left panel, whatever search or filter is on top of it.

  | `source` | Started from |
  |---|---|
  | `{kind: "album", id, name, quick?}` | a regular album: also the Quick Collection (`quick: true`) and saved creations |
  | `{kind: "smartAlbum", id, name}` | a smart album |
  | `{kind: "folder", path}` | a folder of the library, or a folder on disk being browsed |
  | `{kind: "allPhotos"}` | All Photographs |
  | `{kind: "other", name}` | another view: `picks`, `recentlyAdded`, `previousImport`, `previousExport`, `missing`, `recentlyDeleted`, and `albumFolder` (an album folder, which holds albums, not photos) |

  It is the session's source, as the selection is the session's selection, so the menu, the
  control channel and MCP all send it; `plugin.run` and `plugin.prepare` take a `source` of their
  own to replace it. A plug-in that hands photos to a tool keeps it (in the run's files, say) to
  use when the results come back, as the user may have moved on by then. In Rust:
  `req.source()`, and `req.source_album()` for the id of an album that can hold photos.
- **`library.add {path, from, stack?, album?}`** (`host::library_add(path, from, stack, album)`)
  → `{photo, added, reloaded}`:
  - `from` is the photo the file was made from: a photo of the library (not in Recently Deleted)
    with a file. `path` is an absolute path to a file in the folder of `from`'s original or below
    it, after symbolic links are resolved; that is where such tools write by default. It can't
    be `from`'s original itself. A path reached through a link is added under the folder's own
    path, so the same file is never in the library twice.
  - The file is imported in place, as a new photo with its own settings: `from`'s develop
    settings are not copied. A copy of a file the library has elsewhere is added too.
  - `stack: true` stacks it directly above `from`, in `from`'s stack or a new one, as Edit In
    does. Without it, `library.add` doesn't stack it (the import may still stack a file by its
    own rules, such as a DNG's link to the file it was derived from).
  - `album` (an id from `source`) adds it to that album, which must be a regular album that
    holds `from`, so a plug-in can only file a result next to its source. A photo that left the
    album while the tool ran gives an error: try again without `album`.
  - A file the library has already returns that photo with `added: false`; `stack` and `album`
    still apply, and nothing is listed twice. If the file changed since the library read it (a
    tool wrote over its earlier result), it is read again as *Reload from Disk* (`photo.reload`)
    does, with `reloaded: true`: new size, dimensions and content hash, cached previews dropped;
    the photo keeps its id, settings, stack and albums. A photo the user had deleted comes back
    from Recently Deleted (`added: true`), read again the same way when the file changed.
  - Everything is checked before the library changes; an error adds nothing (should a later step
    fail after all, what was done is undone). What does change is one undo step ("Add from
    Plug-in"), and the user's selection stays as it was.
  - It is a write: prepare steps can't call it. In `program.end`, the call is answered on the UI
    thread, like the hook's catalog reads.

The `program-tools` example (below) does the whole round trip.

## Jobs: long commands with progress

A command that works through many photos, or waits on a server, runs as a **job**: on a worker of
its own, with a row in the activity stack, so the window stays responsive. This needs the host
feature **`jobs`**: a command with `"job": true` in a manifest without `"requires": ["jobs"]` is a
manifest error.

```json
"requires": ["jobs"],
"commands": [{"id": "check", "label": "Check Photos", "job": true}]
```

- **Starting:** from its menu item, a shortcut or `plugin.run`, a job returns at once. Its
  prepare step and dialog work as for any command. Its row is named after the command and the
  plug-in ("Check Photos (Job Tools)"), of kind `pluginJob`, and shows wherever the app shows
  activity (`activity.list` for agents). `plugin.run` answers
  `{job: <activity id>, running: true, plugin, command}`.
- **Progress:** the job reports through host calls that only a job may make (anything else gets an
  error saying so):

  | Host call | SDK | Effect |
  |---|---|---|
  | `job.progress {done, total, unit?}` | `job::progress(done, total)`, `job::progress_in(done, total, unit)` | the bar and its count; `total` 0 makes the bar sweep; `unit` is `count` (default), `bytes` or `percent` |
  | `job.detail {text}` | `job::detail(text)` | the detail line: what it is working on, a name, not a sentence (cut at 200 characters) |
  | `job.itemDone` | `job::item_done()` | one photo done |
  | `job.itemFailed {why}` | `job::item_failed(why)` | one photo failed; `why` names it and the reason |
  | `job.cancelled` | `job::cancelled()` | `true` once the user stopped the job; never fails |

  A job that counts photos (`itemDone`, `itemFailed`) gets a batch row: the time left, the
  failures, and a summary when it ends ("Check Photos (Job Tools): 24 done, 1 failed"), as the
  app's own batches do.
- **Continuing:** every call of a job has the per-call budget ([Limits](#limits)). A job that needs
  longer replies `{"continue": state}` (SDK: `job::continue_with(state)`): the host calls it again,
  in a fresh instance with a fresh budget, under the same row, with the same request plus
  `"state": state` (`req.state()`; null on the first call). The next request, state included, must
  fit in a message (16 MiB), or the job fails saying so. Between calls the plug-in's namespace data
  is saved, so a quit or a crash loses only what the call then running wrote.
- **Stopping:** the row's ✕, `activity.cancel {id}` and Quit Anyway stop a job between calls.
  From then on `job.cancelled` answers `true`, every other host call fails with "cancelled", and a
  reply asking to continue is not called again. A plug-in that checks `job.cancelled` can stop
  cleanly and say how far it got in its reply. Quit asks first while a job runs.
- **Ending:** the row goes. The last reply's `message` is shown (a toast in the desktop app); an
  error is shown and written to the plug-in's log, like any failed call. A stopped job shows
  nothing and logs "command cancelled".
- **Busy:** while a job runs, other calls of the same plug-in (its other commands, its hooks) are
  refused with an error that names the job.
- **Reading, not writing:** a job may read the catalog (`catalog.query`, `photo.inspect`, …), and
  read and write its own namespace (`ns.set`) and settings, files and the network as granted. It
  may not change the catalog:
  `metadata.setStandard`, `library.add`, `develop.setCrop` and `photo.virtualCopy` fail with "writes to the catalog aren't available in a
  job yet", and `program.start` fails too. The catalog reads are answered by the window between
  frames; a session with no window (headless CLI or MCP) refuses them, so run a job there with
  **`plugin.run {…, wait: true}`**: it runs to its end on the caller's thread and returns the
  job's last reply (a stopped job is an error, "cancelled"). Without `wait`, a one-shot CLI run
  returns the job's id and exits, which ends the job with it. Sent to the running app (its control
  channel, `mcp --connect`), `wait` holds the window until the job ends, and its ✕ can't be
  clicked meanwhile: leave `wait` out there and follow the row (`activity.list`).

The `job-tools` example works through the selected photos ten per call, reporting each one.

## Pixels and crop

A plug-in can look at a photo and set its crop: find a film frame's picture area inside its
rebate, square up a scan, trim a border. The crop it sets is an ordinary one: one History step,
undone with Undo, and adjusted afterwards in the crop tool like any other. Two permissions, each
with a host feature of the same name (`"requires": ["pixels", "crop"]`):

- **`photo.pixels {photo, longEdge?}`** (`pixels`; `host::photo_pixels(photo, long_edge)`) →
  `{width, height, channels: 3, encoding: "srgb8", data}`: the photo as the crop tool shows it
  before cropping (orientation, flips, lens corrections and perspective applied; no crop, no
  straighten), downsized to a long edge of `longEdge` (default 1024, kept within 64..=1536, never
  larger than the photo), as 8-bit sRGB, `data` the interleaved RGB rows, top to bottom, as base64.
  The pixels are the decoded photo before tone and colour adjustments (a raw's are its linear
  values with the sRGB curve on top, so they may look dark): enough to find edges, not a render.
  It only reads, so a prepare step may call it.
- **`develop.setCrop {photo, rect, angle?}`** (`crop`; `host::set_crop(photo, rect, angle)`) →
  `{rect, angle}`: sets the crop as `crop.set` does. `rect` is `[x0, y0, x1, y1]`, normalized (0..1)
  in the frame `photo.pixels` gives, so a box found in those pixels is set as it is; `angle` is the
  straighten angle in degrees, clockwise (clamped to ±45°; left out, 0, whatever the photo was
  straightened by before). With an angle, `rect` is in that frame turned by `angle` about its
  centre: the frame `crop.set` reads, so a box found in the pixels after turning them level is set
  as found. The host fits the crop inside the photo (a straightened crop may come out smaller) and
  returns it as set; a rect smaller than the crop tool's smallest crop is refused. It writes, so
  prepare steps and jobs can't call it.

- **`photo.virtualCopy {photo, name?}`** (`virtualCopy`, with a host feature of the same name;
  `host::virtual_copy(photo, name)`) → `{photo}`: a virtual copy, as *Create Virtual Copy*
  makes one: a new catalog entry sharing the photo's file, with a copy of its settings and
  metadata, stacked right after it and added to the albums that hold it, named `name` (at most
  256 characters) or the next "Copy N". One History step; the user's selection stays as it was.
  A photo in Recently Deleted is refused. It writes, so prepare steps and jobs can't call it. With
  `develop.setCrop` it lets a plug-in split one capture into several photos: two half-frame
  exposures in one 35 mm scan, say, the original cropped to one and a copy to the other.

From a hook running on a worker (an export hook, `program.end`) all three are answered on the UI
thread, and from a job `photo.pixels` is, like the catalog reads. Each call loads the photo's
source at the smallest level that covers `longEdge` (the 512 px thumbnail level up to 520, the
preview level above), as the library grid and the loupe do.

## Limits

Each call runs in a fresh instance. It has an instruction budget, a wall-clock budget (time spent
in host requests counts), a memory cap of 256 MiB by default, a recursion cap, a 16 MiB cap on
each message, and at most 10 000 host calls. A module that breaks any of these, traps, or returns
bad JSON fails with an error and is logged; the app keeps running. A module can have no imports
other than the three below (no WASI).

## Compatibility: `requires` and `version`

The ABI version (below) only changes for a breaking change. Everything added to it since is
named, and a plug-in lists the names it uses in **`requires`**, so a version of the app that lacks
one refuses it clearly instead of failing in confusing ways (a permission that never appears, a
hook that's never called, a host call that fails as an unknown method).

- **The host's list** is `lcc_plugins::HOST_FEATURES`, and `plugin.list` returns it as
  `host: {abi, features}`. Everything the host had when `requires` arrived is the baseline and
  has no name. Names so far: `programs` ([Programs](#programs)), `settings` ([below](#settings-remembered-values-and-prepare-steps)), `libraryAdd` ([Adding files to the library](#adding-files-to-the-library)), `jobs` ([Jobs](#jobs-long-commands-with-progress)), `pixels`, `crop` and `virtualCopy` ([Pixels and crop](#pixels-and-crop)), `pictures` (`icon` and `header`) and `about` (`homepage`, `repository`, `changelog`, `license`; both in [What a plug-in can add](#what-a-plug-in-can-add)), `updates` (`updateUrl`, [Updates](#updates)), and `quickActions` (`quickAction` on a command, [Quick actions](#quick-actions)). Every manifest field, permission group or host call added
  from then on gets a name there in the change that adds it, and its documentation here says
  which name to require.
- **Names** are `[A-Za-z0-9._-]{1,64}`, at most 64, none repeated; anything else is a manifest
  error. A name is a minimum, never a maximum.
- **Installing** a plug-in that requires a name the host doesn't know is refused, naming each
  missing feature ("needs `cache`, which this version of the app doesn't have"), and nothing is
  written. `plugin.inspectFile` lists them as `missing`, and the install dialog shows them with
  no Install button.
- **An installed plug-in** that requires a name the host no longer knows (the app was
  downgraded) doesn't load: `plugin.list` lists it under `failed` with `missing`. Its entry in
  `plugins.json` (enabled state and grant) and its data are kept, so it loads as before once the
  app is updated again.
- **Removing** a plug-in that didn't load (it needs missing features, or its module can't be read)
  works like any other: each `failed` row gives its `id`, `plugin.uninstall {id}` deletes its
  module, its `plugins.json` entry and its data (`keepData: true` keeps its data and settings), and the Plug-in Manager lists it with a
  didn't-load mark (under Pixel Filters for a filter module) and a page that says why, with a
  Remove button.
- Versions of the app from before `requires` ignore it, like any field they don't know: the check
  protects from this version on.

**`version`** is required and is semver: `MAJOR.MINOR.PATCH`, optionally with a pre-release
(`2.0.0-beta.1`) and build metadata (`+build.7`). Installing refuses a missing or invalid one
(`plugin.inspectFile` gives the reason as `versionError`). A module installed before this rule
without a valid version still loads, and its log says so.

**Downgrades** need confirming. Versions compare by semver precedence: a pre-release comes before
its release, and build metadata is ignored. Installing a lower version than the installed one is
refused unless `plugin.install` gets `allowDowngrade: true`; `plugin.inspectFile` says
`downgrade: true`, and the install dialog shows both versions and an *Install Older Version*
button. The older version keeps the grant, cut back to what it asks for, and its log says
"downgraded from X to Y". The same version again is a repair and installs without asking.

**Going back.** An update with other code keeps the version it replaces, its module and its
settings as they were before the update, in `previous/<id>.wasm` and `previous/<id>.json` (one
level deep; nothing loads from that folder). `plugin.list` gives that version as
`previousVersion`, the plug-in's page offers *Go Back to Version X*, and `plugin.rollback {id}`
does the same. Going back restores the module and the settings, keeps the grant cut back to what
that version asks for, and logs "rolled back from Y to X". Programs the user granted stay when
the user goes back from the page; the command drops them, since that version's code is different
(as for an install). A job running meanwhile keeps its permissions, as through an update. When the
old module or its settings can't be staged and written, nothing changes and the error says why;
if only putting the staged settings in place fails afterwards, the old version runs with the
current settings, and its log says so.

**Unknown manifest fields** are ignored, but not silently: `plugin.inspectFile` lists them by path
(`commands[2].lable`) as `unknownFields`, the install dialog shows them, and installing writes
them to the plug-in's log (a pixel filter's to the app's log). The install still goes ahead.

**One id, one module.** A plug-in and a pixel filter can't share an id: installing one whose id a
module of the other kind has is refused (`plugin.inspectFile` gives why as `conflict`). Of the
modules in the folder with one id, the first by file name that loads is the one used (a module
that needs features this version lacks doesn't hold its id, and keeps that error); each other one
is listed under `failed` ("another module has this id"), and `plugin.uninstall {file}` deletes
only that file. Removing a plug-in by its id is refused while a pixel filter module that didn't
load sits at `<id>.wasm` (the file removing the plug-in would delete): remove that module first,
by its file or its row in the Plug-in Manager.
Plug-ins load from the first 256 modules in the folder by file name, pixel filters' modules
counted too, and pixel filters from the first 1,000; `plugin.list` gives how many modules neither
loaded as `skipped` (counted only, not listed one by one), and the Plug-in Manager says so.

Pixel filters (below) have their own manifest, shared with PhotoCraft: `requires` and the semver
rule don't apply to them. Installing an older filter needs confirming as above (versions that
aren't semver compare as dotted numbers, `1.9` before `1.10`), and a filter keeps no version to go
back to.

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
- `log(ptr, len)`: adds one line to the plug-in's log, shown in the Plug-in Manager.

A module may also carry its manifest in a custom section named `photo-plugin.manifest`: the exact
bytes `lcc_manifest` returns, written by the SDK's stamp tool so a tool can read what a plug-in is
and asks for without running it (Stamping your plug-in, below). The section is optional and only a
copy: `lcc_manifest` stays the authority, a module without the section (absent means it wasn't
stamped) works as before, and the ABI version doesn't change. When a module has the section, the
app checks it against `lcc_manifest` every time it loads the plug-in (install, update, start-up)
and refuses a module whose section differs, naming the fields that do; so an installed plug-in's
section is always true, and only then.

## Writing one in Rust

The guest SDK is `sdk/plugin` (`lcc-plugin-sdk`). Seven examples, each a complete plug-in:

| Example | Shows |
|---|---|
| `sdk/examples/catalog-tools` | every hook in one place: a menu command with a dialog, the plug-in's own namespace, a metadata provider, an export hook and a publish service |
| `sdk/examples/batch-metadata` | a small useful tool (`catalog` + `metadataWrite`): *Title / Caption from Template…* fills the title or caption of the selected photos from a template with tokens (`{name}`, `{file}`, `{date}`, `{year}`, `{camera}`, `{n}` with a start number); *Add Keywords…* adds comma-separated keywords. Every write goes through `metadata.setStandard` |
| `sdk/examples/export-tools` | **export hooks**: a post-process hook that reads each exported file (binary, `host::read_bytes`) and records its CRC-32; an **export destination** "WebDAV Upload" that `PUT`s the files to a server (`host::http_bytes`, basic auth, a dry-run switch) |
| `sdk/examples/publish-folder` | a **publish service** "Local Gallery": copies each rendered photo into a gallery folder (one writable `fs` root), keeps an `index.json`, replaces on re-publish and deletes on removal |
| `sdk/examples/program-tools` | **programs and adding files** (`requires: ["programs", "libraryAdd"]`): *Send to Converter* writes the selected photos' ids and originals, and the album it was started from, into the run's folder and starts a granted program on that list (`host::program_start`); its **`program.end`** hook reads the program's `result.txt` and adds each result to the library, stacked on its photo and in that album (`host::library_add`), or logs the exit code when the program failed |
| `sdk/examples/keyword-picker` | **settings, remembered values and a prepare step** (`requires: ["settings"]`): *Apply Keyword…* adds (or, with a switch that is never remembered, removes) one keyword on the selected photos, picked from the keywords in use. Its prepare step offers the keywords used on at least *n* photos (a setting), most used first or A to Z (another), runs at once when only one qualifies and explains when none does |
| `sdk/examples/job-tools` | **a job** (`requires: ["jobs"]`): *Check Photos* works through the selected photos ten per call on a worker, reads each from the catalog, counts it done or failed (`job::item_done`, `job::item_failed`), shows the photo it is on (`job::detail`) and its progress, goes on with `job::continue_with(next)`, and stops cleanly when the row's ✕ is clicked (`job::cancelled`) |
| `sdk/examples/custom-fields` | **custom metadata fields** Client / Job / Usage (editable in the Metadata panel) and Delivered (read-only), a *Set Job Fields…* command for the selection, an export hook that marks exported photos delivered, and a metadata provider that suggests the client as a keyword |

```sh
rustup target add wasm32-unknown-unknown
cd sdk/examples/catalog-tools
cargo build --release --target wasm32-unknown-unknown
# install target/wasm32-unknown-unknown/release/catalog_tools.wasm:
#   File ▸ Plug-ins ▸ Plug-in Manager…, or  <cli> commands plugin.install '{"path": "…"}'
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

`export_plugin!` exports the ABI; `handle` gets every request. `export_plugin!(package: MANIFEST,
handle)` also fills `version`, `description`, `author`, `homepage`, `repository` and `license`
from the crate's own `Cargo.toml` wherever the manifest JSON leaves them out (`author` joins
`authors` with ", "; a field `Cargo.toml` doesn't set stays out), so the plug-in's version can't
drift from its crate's. It reads them when the plug-in is compiled (`lcc_plugin_sdk::package!()`);
`lcc_plugin_sdk::with_package(json, &package)` does the same for a manifest built another way. The
examples use it. Reply with `Ok(value)` or
`Err(message)`: an error is shown to the user and written to the plug-in's log. A `message` in a
command's or destination's reply is shown as a notice.

The manifest fields:

| Field | Meaning |
|---|---|
| `id` | stable, `[A-Za-z0-9._-]{1,64}`, reverse-DNS style; an install with the same id is an update |
| `version` | required, semver `MAJOR.MINOR.PATCH` (see [Compatibility](#compatibility-requires-and-version)) |
| `requires` | `[names]`: the host features the plug-in uses beyond the baseline (see [Compatibility](#compatibility-requires-and-version)) |
| `name`, `description`, `author` | shown in the Plug-in Manager |
| `homepage`, `repository`, `changelog`, `license` | optional, shown on the About tab: `https://` links and the licence as text (see [Where it comes from](#what-a-plug-in-can-add)); host feature `about` |
| `updateUrl` | optional `https://` link to the plug-in's update feed ([Updates](#updates)); another value is dropped and named in `dropped`; host feature `updates` |
| `icon`, `header` | optional SVG pictures (see [Its own pictures](#what-a-plug-in-can-add)); host feature `pictures` |
| `permissions` | what the plug-in asks for (see [Permissions](#permissions)); ask for as little as possible |
| `commands` | `[{id, label, menu?, dialog?, prepare?, job?, quickAction?}]`, File ▸ Plug-ins ▸ `name` ▸ `menu` ▸ `label`; dialog fields may say `remember: false`; `quickAction: true` adds a quick settings bar button ([Quick actions](#quick-actions)) |
| `settings` | `[{key, label, type, …, prepare?}]`, at most 32; needs `requires: ["settings"]` |
| `hooks` | `{export?: true, metadata?: true}` |
| `publish` | `{id, name}`: one publish service |
| `exportDestinations` | `[{id, name, dialog?}]`, at most 16 |
| `metadataFields` | `[{key, label, type, …, readOnly?}]`, at most 32; `type` and its options as for dialog fields |

### 3. Requests by hook

| `req.hook()` | Fields (`req.get(…)`, helpers) | Reply |
|---|---|---|
| `command` | `req.command()`, `req.arg(key)`, `req.selection()` (photo ids as strings), `req.source()`; a job also `req.state()` | anything; `message` is shown; a job may reply `job::continue_with(state)` |
| `command.prepare` | `req.command()`, `req.selection()`, `req.source()` | exactly one of `{dialog: {<key>: {options?, default?}}}`, `{run: args}`, `{message}`; host calls may only read |
| `settings.prepare` | | `{fields: {<key>: {options?, default?}}}`, for fields with `prepare: true`; host calls may only read |
| `export` | `req.files()` → `(path, photo)` of the written files | anything |
| `export.destination` | `req.destination()`, `req.arg(key)`, `req.files()` | anything; `message` is shown |
| `metadata` | `photos`: `photo.inspect` JSON | `{suggestions: [{photo, fields?, keywords?}]}` |
| `publish.upload` | `item: {path, photo, title, caption, keywords, remoteId?}` | `{remoteId, url?}`; the same `remoteId` comes back on a re-publish and on removal |
| `publish.delete` | `remoteId` | anything |
| `program.end` | `req.run()`, `req.program()`, `req.status()` (`exited`, `cancelled`, `failed`), `req.code()` (the exit code, when known), `req.tmp_dir()` (readable during the call) | anything; failures go to the log |

### 4. Call the host

| SDK | Host method | Needs |
|---|---|---|
| `host::call(method, params)` | any of the below | |
| `host::query(params)` | `catalog.query` (also `photo.inspect`, `catalog.stats`, `keyword.list` via `call`) | `catalog` |
| `host::call("metadata.setStandard", {ids, title?, caption?, keywords?, addKeywords?, …})` | writes standard fields | `metadataWrite` |
| `host::library_add(path, from, stack, album)` | `library.add {path, from, stack?, album?}` → `{photo, added, reloaded}` | `libraryAdd` |
| `host::photo_pixels(photo, long_edge)` → `Pixels {width, height, rgb}` | `photo.pixels {photo, longEdge?}` | `pixels` |
| `host::set_crop(photo, rect, angle)` | `develop.setCrop {photo, rect, angle?}` → `{rect, angle}` | `crop` |
| `host::virtual_copy(photo, name)` → the copy's id | `photo.virtualCopy {photo, name?}` → `{photo}` | `virtualCopy` |
| `host::ns_get(photo)`, `host::ns_set(photo, key, value)` | `ns.get`, `ns.set`, `ns.photos` | nothing (own namespace; also holds `metadataFields` values) |
| `host::read_text`, `host::write_text`, `host::read_bytes`, `host::write_bytes`, `host::remove`, `host::exists` | `fs.*` | an `fs` root (or a lent file) |
| `host::http(method, url, headers, body)`, `host::http_bytes(…)` | `http.request` → `{status, contentType, location, body}` | the URL's host in `network` |
| `host::program_list()` | `program.list` | nothing |
| `host::program_start(program, args, files, bundle_id)` | `program.start` → `{run, tmpDir}` | the program in `programs` (granted by the user) |
| `host::settings()`, `host::setting(key)`, `host::set_setting(key, value)` | `settings.get`, `settings.set {key, value}` | nothing (own settings) |
| `host::log(line)` | `host.log` | nothing; Plug-in Manager ▸ Log |
| `job::progress`, `job::progress_in`, `job::detail`, `job::item_done`, `job::item_failed`, `job::cancelled` | `job.*` ([Jobs](#jobs-long-commands-with-progress)) | a command with `job: true` |
| `lcc_plugin_sdk::base64::{encode, decode}` | | |

### 5. Rules the host enforces

- Every call runs in a **fresh instance**: keep state in the namespace, not in statics.
- **Budgets per call**: instructions (fuel), wall-clock time (network time counts), 256 MiB of
  memory, call depth, 16 MiB per message, 10 000 host calls, 8 MiB per file or HTTP body. Long
  work is a [job](#jobs-long-commands-with-progress) that goes on with `{continue: state}`.
- Everything a plug-in returns is **checked**: dialog values are coerced to their fields before
  the plug-in sees them, `metadataFields` values that no longer fit the field show as empty, and a
  reply that is not JSON, too large or late is an error. Nothing a plug-in does can crash the app.
- **Permissions are the user's**: nothing is granted at install; the first call that needs one
  asks the user, from a command they started, and fails with nobody to ask. Make that call early
  (Allow runs the command again), and handle `not granted` errors with a clear message: the user
  may have said no, and may revoke any permission later.
- Paths must be **absolute** without `..`; links can't lead out of a root.
- **Programs** start only once the user has allowed them, one run per program at a time, and their
  folder lives only until the run is over: copy what you need out of it in `program.end`.

### 6. Test and ship

- Unit-test your logic natively (`cargo test`; the host calls answer "no host" off wasm).
- Build `--release --target wasm32-unknown-unknown`, then install from File ▸ Plug-ins ▸ Plug-in Manager…
  (*Install…* lists what it may ask for) or `<cli> commands plugin.install`.
- Drive it without the UI: `plugin.run`, `plugin.exportTo`, `plugin.setField`, `plugin.log` from
  the CLI, the control channel or MCP.
- Raise `version` with every release you hand out, and list in `requires` each host feature you
  use beyond the baseline.
- Stamp and sign the release build (Stamping your plug-in and Signing your plug-in, below) and
  distribute that `.wasm` file. There is no store yet: tell users what each permission is for.

## Stamping your plug-in

`plugin-stamp` (`sdk/stamp`, a small program of its own) copies your plug-in's manifest into its
`.wasm` as a custom section, `photo-plugin.manifest`. It runs the module once in a sandbox of its
own (every import traps, fuel and memory are capped), calls `lcc_manifest` and writes what it
returns, byte for byte. So the section matches however you build the manifest: a literal, a string
filled in at run time, or `export_plugin!(package: …)`. It needs no Chromaforge install (its only
dependencies are `wasmi` and `serde_json`) and runs in any CI. Get it at the same `rev` as your
`lcc-plugin-sdk` dependency, so the tool and the SDK always match:

```sh
cargo install --locked --git <this repository's URL> --rev <the SDK's rev> plugin-stamp
```

1. **Build** as usual: `cargo build --release --target wasm32-unknown-unknown`. Nothing in your
   code or `Cargo.toml` changes, and a plug-in that is never stamped keeps working.
2. **Stamp** the built file:
   `plugin-stamp target/wasm32-unknown-unknown/release/my_plugin.wasm -o my_plugin.wasm` (in place
   without `-o`). It refuses a module the app wouldn't load (an import other than the host's three, a
   missing ABI export) or that can't answer (a trap, a call to a host import, out of fuel, an answer
   over 64 KiB or not a UTF-8 JSON object), and then leaves both files as they were. Stamp after
   every build that changes the manifest or the version; stamping again replaces the section, and
   the same input always gives the same bytes. Stamp after any `wasm-opt`
   or strip step, which may drop custom sections (the release profile's `strip = true` runs during
   the build, so it can't).
3. **Publish the stamped file**, not the raw build: it is the file you sign (Signing your plug-in,
   below), checksum and attach to the release.
4. **Check it in CI**: `plugin-stamp --check my_plugin.wasm` exits 0 when the file has exactly one
   section and it equals what `lcc_manifest` returns, and 1 when the section is missing, stale
   (naming the fields that differ) or duplicated. It writes nothing. Run it after the stamp, so a
   release can't go out unstamped.
5. **Install** it as before. The app checks the section against `lcc_manifest` when it loads the
   plug-in (install, update, every start) and refuses a stale stamp, naming the fields that differ:
   at start-up the plug-in is listed as failed with that reason. `plugin.inspectFile` reads a
   stamped file from the section without running it (Engine commands, below).

Plug-in updates from a feed (not done yet) will be offered only for stamped plug-ins, which is the
reason to add step 2 to your release build now.

## Signing your plug-in

`plugin-stamp` also signs a stamped plug-in with your own author key, so the app can tell that a
new version comes from whoever made the one installed. The signature is a custom section,
`photo-plugin.signature`, at the end of the `.wasm`: one file, so it survives a download, a copy
and a hand install. It holds `{"v": 1, "key", "sig"}`, an ed25519 public key and its signature
over every byte of the file before the section (the code and the stamped manifest), in standard
base64. No certificate or authority is involved: the key is the author's identity.

1. **Make a key, once:** `plugin-stamp keygen my-author.key`. It writes a new secret key (readable
   by you only on macOS and Linux; on Windows it takes the folder's permissions, so make it in a
   folder only you can read) and never overwrites a file. It prints the key's **fingerprint**, the first 16 hex digits
   of the public key's SHA-256 in groups of four (`3f2a 9c41 07be d5e8`). Users see that
   fingerprint in the install dialog; put it on your plug-in's page so they can compare.
2. **Keep the secret key out of the repository**: in a password manager, or your CI's secrets. A
   release tool can sign without writing it to disk: the `plugin-stamp` library's `sign` takes the
   key's 32 bytes, and `secret_key_from_text` reads the key file's text from a variable.
3. **Sign after stamping**, as the last step that changes the file:
   `plugin-stamp sign --key my-author.key my_plugin.wasm` (in place, or `-o signed.wasm`). It
   refuses a file whose stamp is missing or stale, and replaces an earlier signature. Stamping a
   signed file drops its signature and says so: sign it again after. Any later rewrite of the file
   (`wasm-opt`, a strip) breaks the signature, as it should.
4. **Check it:** `plugin-stamp --check my_plugin.wasm` also reports the signature (none, valid
   with its fingerprint, or invalid with why) and exits 1 on an invalid one.

What the app does with it:

- **The first signed install pins its key:** `plugins.json` keeps it as the plug-in's `signer`.
  An update or a rollback signed by the same key keeps the pin; uninstalling removes it.
- **Going back checks the kept file:** an update records the signer of the version it replaces
  beside it (`previous/<id>.json`), and `plugin.rollback` refuses that file if it is no longer
  signed that way, unless it gets the same `acceptSigner` / `acceptUnsigned` answer. A version kept
  before signers were recorded goes back as before.
- **Another key, or no signature, over a pinned plug-in asks the user first**, naming both
  signers; a yes pins the new key (or removes the pin, for an unsigned file). `plugin.install` takes
  that answer as `acceptSigner` (the file's full key, `signerKey` from `plugin.inspectFile`, or its
  fingerprint) or `acceptUnsigned: true`; the dialog passes the full key, so its yes covers exactly
  the key it showed, and without it refuses
  the install, saying which to pass. A pin the app can't read asks the same way.
- **An invalid signature is never "unsigned"**: a signature that doesn't match the bytes, a
  section that isn't last, two sections, one over 1 KiB, a key or signature of the wrong length,
  bad base64 or JSON, or another format version. The file isn't installed or loaded, and
  `plugin.inspectFile` says why.
- **At start-up** a plug-in whose file no longer matches its pin (the signature gone, invalid or by
  another key) isn't loaded: it is listed under `failed` with why, and keeps its grants, data and
  settings until it is reinstalled or removed. A plug-in installed before signatures existed pins
  its signer at the next start, written to `plugins.json` then.
- **An unsigned plug-in installs as before**; the dialog says nobody signed it.

**A lost or changed key** has no recovery step: sign the next version with the new key and tell
users to expect the question. They install it by hand, see both fingerprints, and say yes.

## Updates

A plug-in can name an **update feed** in its manifest, `"updateUrl": "https://…"` (host feature
`updates`). The app finds a newer version there and shows it in the Plug-in Manager; the user
installs it. Nothing installs by itself.

**Which plug-ins can have update checks:** a plug-in whose manifest names a feed, whose installed
file is stamped ([Stamping your plug-in](#stamping-your-plug-in)) and signed by its pinned key
([Signing your plug-in](#signing-your-plug-in)). The update's permissions are then read from the
downloaded file's section before anything runs, and its signature shows it comes from the same
author. An unstamped or unsigned plug-in installs and runs as before, with no update checks: its
first stamped, signed version is a hand install. Pixel filters get none.

**When the app checks:** only when the user turns a plug-in's check on (*Check for updates* on its
page; off by default), and then only **when the Plug-in Manager opens**, at most once a day per
plug-in. A result from within the day is shown as it was. Nothing contacts a feed while the
Plug-in Manager is closed; *Check Now* (or `plugin.checkUpdates {id}`) checks at once. This is
separate from the app's own update check.

### The feed

One JSON file with an entry per plug-in, so one author's file can serve several plug-ins, each
released on its own. An entry is matched to a plug-in by its **`id` field** (the manifest `id`);
its key is free, such as a folder name:

```json
{
  "geotag": {
    "id": "org.example.geotag",
    "version": "1.3.0",
    "url": "https://example.org/releases/geotag-1.3.0.wasm",
    "sha256": "<64 hex digits: the .wasm file's SHA-256>",
    "requires": ["settings", "updates"],
    "permissions": ["catalog", "network"],
    "changelog": "https://example.org/geotag/CHANGELOG.md",
    "notes": "Faster lookups; the cache now survives a restart."
  }
}
```

| Field | Meaning |
|---|---|
| `id` | the plug-in's manifest `id`; two entries with one id offer nothing for it |
| `version` | semver; offered only when above the installed version (a stale or replayed feed offers nothing), and an entry whose version can't be read is ignored |
| `url` | the `.wasm` to download, `https://` only |
| `sha256` | the file's SHA-256 in hex; a download that doesn't match isn't installed |
| `requires` | the host features that version needs, as its manifest names them (there is no separate minimum app version); an update needing one this app lacks is shown with what's missing, and can't be installed |
| `permissions` | the permission groups that version asks for (`catalog`, `metadataWrite`, `libraryAdd`, `pixels`, `crop`, `virtualCopy`, `network`, `fs`, `programs`), shown before the download, marked as declared by the feed |
| `changelog` | optional `https://` link, shown next to the note |
| `notes` | optional short note on what's new: plain text, at most 280 characters (cut on a word boundary with "…"); markup and links show as the text they are |

Take `notes` from the changelog when the feed is built: the entry whose heading names the version
being published (never "Unreleased"), stripped to plain text and cut to the cap. With no such
entry, leave `notes` out. So there is one file to keep, and the app makes no request to the
changelog's host.

The feed is fetched over `https` only (a redirect to `http` is refused), and at most 1 MB is read;
a feed or a download that takes longer than 5 minutes in all is stopped.
A feed that can't be fetched or read offers nothing; why is shown on the plug-in's page and
written to its log, as is why an entry was ignored.

**The feed isn't signed**, so the app trusts nothing in it beyond what it shows: the version must
go up, the file must match the feed's SHA-256, be stamped, carry the offered id and version, and
be signed by the pinned key, and its own stamped manifest decides what it asks for. When that
asks for a permission group the feed didn't declare, the update stops and the user is asked
first (a taken-over feed would hide exactly that). New permissions aren't granted, as with any
update, and the version replaced is kept for *Go Back to Version X*.

## Engine commands

`plugin.list`, `plugin.inspectFile {path, verify?}`, `plugin.install {path, grant?, allowDowngrade?, acceptSigner?, acceptUnsigned?}`,
`plugin.uninstall {id | file, keepData?}`, `plugin.rollback {id, acceptSigner?, acceptUnsigned?}`, `plugin.enable {id, enabled}`, `plugin.grant {id, grant}` (revokes only), `plugin.revoke {id, group, value?}`,
`plugin.log {id}`, `plugin.commands`, `plugin.prepare {plugin, command, ids?, source?}`,
`plugin.run {plugin, command, args?, ids?, source?, wait?}` (a job: `{job}` at once, or its result with `wait`), `plugin.settings {id}`, `plugin.setSettings {id, values}`,
`plugin.suggestMetadata {ids?}`, `plugin.postExport {files}`, `plugin.publishServices`,
`plugin.exportDestinations`, `plugin.exportTo {plugin, destination, files, args?}`,
`plugin.metadataFields {id?}`, `plugin.setField {plugin, field, value, id?}`,
`plugin.setUpdateCheck {id, on}`, `plugin.checkUpdates {id?, due?}`, `plugin.update {id, acceptUndeclared?}`.

The update commands ([Updates](#updates)): `plugin.setUpdateCheck` turns a plug-in's check on or
off. `plugin.checkUpdates` checks plug-in `id` now (an error when it can't have checks), or without
`id` every plug-in whose check is on; with `due: true` only those whose last check is a day old
(what the Plug-in Manager runs when it opens). It returns `{checked: [ids], plugins: [{id, name,
version, updates}]}`. `plugin.update` downloads the version found and installs it through
`plugin.install`'s path; when the file asks for a permission group the feed didn't declare it
installs nothing and returns `{updated: false, reason, undeclared: [groups], requests: [{group,
value?}]}`, and installs only when called again with `acceptUndeclared: true`. Installed, it returns
`{updated: true, plugin}`. Both wait for the network (each fetch at most 5 minutes) before they
return; the Plug-in Manager runs them on a worker with a row in the activity stack.

`plugin.inspectFile {path, verify?}` says what a `.wasm` is and what installing it would do,
without installing it. Its result says where the manifest came from:

- `source: "section"`, `verified: false`: the file is stamped, and the manifest was read from its
  `photo-plugin.manifest` section without running the module (it is validated, its imports and
  ABI exports checked by name and type, but never instantiated; so the ABI version
  `lcc_abi_version` returns is checked only at install or with `verify: true`). That makes it safe on any file an agent or script
  was handed, but the section is only **the file's claim**: nothing has compared it with
  `lcc_manifest` yet. Installing does, and refuses the file if they differ.
- `source: "export"`, `verified: true`: the module was loaded in the sandbox and the manifest is
  `lcc_manifest`'s answer, as at install. That's how a file without the section is read, and any
  file with `verify: true`; a stamped file whose section differs is then an error naming the
  fields. The install dialog inspects with `verify: true`, so the permissions it lists are
  `lcc_manifest`'s.

Every other field (`installed`, `missing`, `versionError`, `downgrade`, `conflict`,
`unknownFields`, `dropped`) is the same either way. So is the author signature
([Signing your plug-in](#signing-your-plug-in)), read without running the module: `signature`
(`"none"`, `"valid"` or `"invalid"`), `signer` (its fingerprint, or `null`), `signerKey` (the full
key in base64, or `null`), `signatureError` (why an invalid one is, or `null`), `pinned` (the
installed plug-in's pin: `{signer, readable}`, `signer` being `null` when it was installed unsigned;
`null` when it isn't installed) and `matchesPin` (whether installing needs no `acceptSigner` /
`acceptUnsigned`: `true` over an unsigned install; `null` when it isn't installed). A pixel filter's result has neither field: its
manifest is always read from `pc_manifest`.

`plugin.list` gives `{dir, host, plugins, failed, skipped}`: `dir` is the plug-ins folder (`null`
when plug-ins are kept in memory), each plug-in has its module's `moduleHash` (sha256), a
`settingsRevision` that changes when one of its settings does, `previousVersion` (the version
`plugin.rollback` goes back to, or `null`), `signer` (the pinned signer's fingerprint, or
`null`) and `updates` (`{check, available, reason, checkedAt?, error?, offer?}`: whether the user
turned its check on, whether it can have one and why not, and the last check's time, error and
offer: `{version, url, sha256, requires, permissions, changelog?, notes?}` from the feed, with `adds`, the declared
groups the installed version doesn't have, and `missing`, the host features this app lacks), each `failed` row has `file`, `error`, `kind` (`plugin` or
`filter`) and, when the module named one, `id`, and `skipped` counts the modules past the most the
folder loads.

`plugin.uninstall {id}` removes a plug-in, its module, data and settings; with `keepData: true`
its data and settings stay (and its version to go back to), for a later install of the same id,
and a call still running can't write its settings back. Its grant goes either way. A pixel filter module the filter loader refuses
gives the loader's error, not the plug-in host's. `plugin.enable` refuses a pixel filter: filters can't be turned off,
only removed.

### The Plug-in Manager (Plugin Manager)

File ▸ Plug-ins ▸ Plug-in Manager… (Cmd+Alt+Shift+, on macOS, Ctrl+Alt+Shift+, elsewhere; it can
be rebound in Keyboard Shortcuts) lists the plug-ins on the left, in two sections, Plug-ins and
Pixel Filters, each with an icon drawn from its id (the same id always gets the same icon; the
plug-in's own icon when its manifest carries one), its name, version and a mark: enabled, disabled or didn't load. The selected entry's page is on the
right:

- **A plug-in:** Enabled, Remove, *Go Back to Version X* (after an update), *Check for updates*
  and *Check Now* (a plug-in that can have update checks, [Updates](#updates)) with the update
  found: its version, note, changelog link and the permission groups its feed says it adds, and
  **Update**, which asks first when the file wants groups the feed didn't declare; and tabs: Settings (when it declares settings), Permissions (each
  one it may ask for: allowed, denied on a date, not asked yet or refused, with Allow / Revoke), About (description, what it adds, its commands
  to run, where it comes from: Homepage, Source Code with *Report a Problem*, Changelog, Licence; the module's size) and Log.
- **A pixel filter:** Remove, its stage, whether it measures the photo (the cropped photo or the
  whole one), its base tone, parameters, the module's size and hash, and where it comes from (as a plug-in's About). Filters have no Enabled
  box.
- **A module that didn't load:** why, and Remove.

Remove asks first, on every page, and Cancel changes nothing; for a plug-in the question offers to
keep its data and settings for a later install. `plugin.uninstall` doesn't ask.

*Install from File…* opens the file picker for a `.wasm` module, and dropping one `.wasm` file on
the app while the window is open installs it too; either way the install dialog lists the
permissions it may ask for first. One install at a time: while the dialog waits, another module is refused with a
note to finish or cancel the first. A module that is exactly what is installed (the same id,
version and code) opens no dialog: its entry is selected, with a note that it is already
installed. Other code under the installed version (a repair) and a newer version open the dialog.
An open Settings tab follows its plug-in: after an update, or a value the plug-in or an agent set,
it shows the current fields and values, keeping what the user typed and hasn't saved. The window
can be dragged narrower or wider; a page's text wraps to it. The footer names the plug-ins folder,
with *Show Folder* to open it.

The window's own commands, for the control channel and `<cli> mcp --connect` (they exist only
while the app runs): `plugins.manager {open?}` (toggles the window), `plugins.install {path?}` (the
install dialog for `path`, or the file picker without one: `{pending: path}`, or `{installed: id,
message}` when that module is already installed; an error while another install waits), `plugins.page {id, tab?, open?}` (the
window with that entry selected, on `tab`: `settings`, `permissions`, `about` or `log`; it fails
for a tab the entry doesn't have), `plugins.showFolder` (the folder in the system file manager),
`plugins.openLink {id, link}` (opens `link`, one of `homepage`, `repository`, `issues` or
`changelog`, of that plug-in's or filter's About, or `updateChangelog`, the changelog of the update
found, in the browser, which opens only `https://`
links; an unknown id or link, or one the manifest doesn't have, is an error that opens nothing) and
`plugins.runItem {plugin, command}`.

`app.export` runs the enabled export hooks after every export (the Export dialog, Export with
Preset / Previous, the CLI, the control channel and MCP) and, with
`pluginDestination: {plugin, destination, args?}`, hands the written files to that destination;
the results come back as `plugins: [{plugin, destination?, ok | error}]`.

In the app, a background export (the Export dialog, Export with Preset / Previous) runs them on a
worker thread once the files are written, with a row in the activity stack ("Running plug-in
export hooks", kind `plugins`; detail: the plug-in in progress), so a long upload doesn't hold
the window. ✕ stops before the next plug-in: a call that has started (a plug-in's code) runs to
its end or its time limit. A plug-in's catalog reads and metadata writes are answered on the UI
thread, with the same permission checks. The Plug-in Manager stays usable meanwhile; a plug-in that
is still busy with the export refuses other calls until it's done ("busy"). The results go to the
toast and to `ui.inspect` → `export.last.plugins` when they finish. An export run synchronously
(`app.export` without `background`, the CLI, MCP) runs them before it returns, as before.

Plug-ins are installed per user in `<settings_dir>/plugins`, or in `<PREFIX>_PLUGINS` if that
is set. That folder holds `<id>.wasm`, `plugins.json` (enabled state, grants, the pinned signer, whether its update check is on and the last check's result) and
`data/<id>.json` (the plug-in's namespace: `{"format": 2, "libraries": {<library id>: {<photo id>:
{…}}}}`; a file written before namespaces were kept per library is claimed whole by the first
library opened from a folder, so no value is lost), and `settings/<id>.json` (its settings and
remembered dialog values: `{"format": 1, "settings": {…}, "dialogs": {"command/<id>" |
"destination/<id>": {…}}}`; a damaged file reads as empty, says so in the plug-in's log, and is
kept as `<id>.json.bad` when the next save replaces it; an update drops the keys the new version no
longer declares, and removing the plug-in deletes the file unless its data is kept), and
`previous/<id>.wasm` and `previous/<id>.json` (the version an update replaced, its settings and its signer,
[Going back](#compatibility-requires-and-version)). A module copied into the folder by hand loads
disabled and with no permissions granted.

## Pixel filters (PhotoCraft ABI)

Besides the plug-ins above, <app> runs **pixel filters** written for PhotoCraft's plug-in ABI v1
(the `lcc-filters` crate, `crates/filters/README.md`): a module with no imports exporting
`memory`, `pc_abi_version`, `pc_manifest`, `pc_alloc` and `pc_filter`. They install the same way
(File ▸ Plug-ins ▸ Plug-in Manager…, or `<cli> commands plugin.install '{"path": "film.wasm"}'`): the host
sniffs the exports and keeps filter modules in the same folder, in a registry of their own.
`plugin.list` shows each as `{id, name, version, kind: "filter", params, moduleHash, size, analyze, merge, sections, shortcut, icon, header}`
(`params` is the manifest's spec); `plugin.uninstall` removes one. Filters ask for no permissions.

A photo's filter stack is part of its develop settings (`develop.pluginFilter.add {plugin,
stage?, params?}` — `params` are checked against the installed manifest and defaults filled in —
`.set`, `.move`, `.remove`, `.list`); every change is one History step, and the stack is copied,
synced and written to XMP with the other settings. Two stages:

| stage | runs | samples handed over |
| --- | --- | --- |
| `source` | after geometry (crop, lens, perspective), before white balance, tone and every other step | linear light, sRGB primaries, `"encoding": "linear"` |
| `display` (default) | after the develop adjustments, before quantisation | sRGB-curve encoded in the output's primaries, `"encoding": "srgb"`; linear for HDR renders |

Each filter gets its resolved parameters plus `encoding`, `_analysis` (below) and `_image`:

| `_image` field | meaning |
| --- | --- |
| `x`, `y`, `width`, `height`, `overlap` | the band's top-left and size in the whole output frame (overlap included) |
| `canvasWidth`, `canvasHeight` | the whole output frame's size (cropped frame bounds when cropped); scaled during a draft |
| `mode`, `depth`, `alpha`, `channels` | `"rgb"`, `32`, `false`, `3` |
| `draft` | `true` for interactive drafts (always `false` on `pc_analyze`, exports, 1:1 and native runs) |
| `draftScale` | present when `draft` is `true` on Source runs: downscale factor `<= 1.0` (`1.0` when not downsampled) |
| `nativeCanvasWidth`, `nativeCanvasHeight` | present alongside `draftScale`: the unscaled canvas size before draft downsampling |
| `source` | present when known, on both `pc_filter` and `pc_analyze`: `{origin, bitsPerSample, float?}` (`origin`: `"untagged"`, `"container"`, `"iccMatrixTrc"`, `"iccCms"`, `"iccUnsupported"`, `"naive"`, `"raw"`, or `"demo"`; `bitsPerSample`: `8`, `12`, `14`, `16`, or `32`; `float`: `true` when the source file held floating-point samples, omitted when `false`) |

Source results are cached: dragging other sliders doesn't re-run the filter; a draft while a filter's own
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
With `ids: [photo, …]` it measures every photo named, one after the other, each getting its own
measurement; the entry is `uid` on the active photo and the first entry of the same plug-in on
the others, and a photo without one first gets a copy of the active entry (parameters, stage,
on/off; no measurement), in one History step for all of them. `develop.pluginFilter.applyTo
{uid, ids?, measurement?}` does that copy on its own, with the measurement unless `measurement:
false`, replacing an entry of the same plug-in; the panel's Measure… menu offers the batch for
the selection, and *Apply to N Selected* the copy.

**A roll (ABI v1.1 proposal).** A module may also export
`pc_merge(buf, len, count, 0, 0, 0, params, params_len) -> i64`, where `buf` holds `len` bytes of
UTF-8 JSON: a list of `count` objects its `pc_analyze` returned, one per photo. It returns, like
`pc_analyze`, one JSON object: the measurement of all of them together (a film converter takes
the roll's thinnest and densest points, say, so every frame is converted alike and keeps its own
exposure). `develop.pluginFilter.analyze {uid, ids, roll: true}` measures each photo, hands the
list to `pc_merge`, and stores the result on every photo with `_roll`: how many it came from
(the panel's *Measure N Selected as a Roll*; "Measured as part of a roll of N" under the tools).
A roll with one photo unmeasured has no measurement: the error is reported for all of them.
Modules without `pc_merge` give an "unsupported" error for `roll`; `plugin.list` shows the
export as `merge`. At most 1024 photos and 16 MiB of measurements go in one call.

A manifest may set `"analyzeArea": "crop"` (default `"frame"`; other values mean the frame) to be
measured on the photo as its develop geometry frames it instead: orientation, crop, straighten,
flips, lens profile and perspective, what the Source stage sees (a negative converter measuring
inside the user's crop, say). The measurement still doesn't move with later crops; measuring again
picks up a new one. Hosts without the field measure the frame. `plugin.list` shows it as
`analyzeArea`.

**Tiles (ABI v1.1 proposal).** A filter with an `overlap` is run in bands, each read with that many
pixels of margin on every side. By default a band is a strip as wide as the frame; a manifest may set
`"bands": "tiles"` (default `"rows"`; other values mean rows) to be run on near-square tiles instead,
which repeat less of the frame in their margins: with a 48 px margin, strips of a 2624 px-wide frame
read about four times what they keep, tiles under one and a half times. Every pixel still sees the
same neighbourhood, so the result is the same; only the work changes. A filter without an `overlap`
runs in strips whatever it asks (there is no margin to save). Hosts without the field cut strips.

**Menu groups (ABI v1.1 proposal).** A choice parameter may carry `"groups"`, one name per option
(`""` = not in a group): `{"type": "choice", "options": ["Generic", "Portra 400", "Superia 400"],
"groups": ["", "Kodak", "Fujifilm"]}`. Develop ▸ Plug-in Filters shows each group as a collapsible
section of the dropdown, in order of first appearance, the one holding the current choice open;
options without a group stay at the top level, in place. A list of another length, or with a name
that isn't 1-64 printable characters, is ignored (a flat menu), as hosts without the field do.
`plugin.list` gives each grouped parameter's `groups`. The dropdown is widget
`pluginFilterChoice:<uid>:<param>`.
**Sections (ABI v1.1 proposal).** A manifest may group its parameters under headings:
`"sections": [{"label": "Tone", "params": ["exposure", "contrast"], "collapsed": false}, …]`.
Develop ▸ Plug-in Filters shows each as a heading with a fold arrow (widget
`pluginFilterSection:<uid>:<label>`; the fold is remembered for the session), the parameters
under it in the section's order; parameters in no section come first, in the manifest's order.
A section without a printable 1-64 character label or without a parameter of the manifest is
dropped, a parameter named twice stays in the first section, and at most 16 sections are read.
Hosts without the field show a flat list. `plugin.list` gives them as `sections`.

**A key to add the filter (ABI v1.1 proposal).** A manifest may name a shortcut, `"shortcut":
"Ctrl+Alt+N"` (`Cmd`, `Ctrl`, `Alt`, `Shift` and a key, joined with `+`; `Cmd` is ⌘ on macOS and
Ctrl elsewhere, `Ctrl` the Control key everywhere). Every installed filter is a command *Add
<name>* in Help ▸ Keyboard Shortcuts (id `pluginFilter.add:<plug-in id>`), with the manifest's
key as its default when the key is free on this platform's keyboard, else none (on Windows and
Linux `Ctrl` is `Cmd`, so a key another command has as `Cmd+…` is taken there); the user binds or
changes it like any other, and giving the filter a taken key moves the key to it.
The key runs `develop.pluginFilter.addTo {plugin, ids?}`: a new entry of the plug-in (its
defaults, its stage) on every selected photo that has none, in one History step, then opens
Develop ▸ Plug-in Filters and says what it did. `plugin.list` shows the manifest's key as
`shortcut`.

**Parameters that remember (ABI v1.1 proposal).** A parameter may carry `"remember": true` (a film
type, a border width: what a user sets the same way on photo after photo). A newly added copy of
the filter (`develop.pluginFilter.add`, `addTo`, the filter's key) then starts with the value
last set for it, by `develop.pluginFilter.set` or given to `add`, instead of its default; values
given to `add` still win, and the other parameters start at their defaults. The values are kept
per plug-in for the whole app (`filter-remembered.json` in the settings folder) and survive a
restart; one the manifest no longer takes (a plug-in update changed the parameter) leaves the
remembered values out for that copy. Hosts without the field ignore it.

**Base tone (ABI v1.1 proposal).** A raw is toned after the Source stage: by its camera's own
curve when the file has one, else by the default raw curve. A Source filter whose output is
already a finished picture (a negative conversion with its own tone curve) may set
`"baseTone": "neutral"` (default `"camera"`; other values mean the camera's): the photo is then
shown as it is, as a JPEG would be, so the filter's tones aren't toned a second time. Contrast,
Whites and Blacks still apply. Hosts without the field use their base tone. `plugin.list` shows it as
`baseTone`.

**A filter's own pictures (ABI v1.1 proposal).** A manifest may carry `"icon"`, a square SVG
document the Plug-in Manager shows in place of the icon it draws from the id, and `"header"`, an
SVG the Develop panel shows at the top of the filter's controls in place of its name (`"header":
"<svg…>"`, or `{"dark": "<svg…>", "light": "<svg…>"}` for the dark and light interface themes;
with one of the two the other theme gets it as well). Each is at most 24 KiB and is drawn as an
untrusted document: only its own shapes, never an image it links to (a file on disk) or embeds (a
`data:` URL); a picture that isn't square is fitted into the icon's square with its aspect kept;
one that isn't an SVG document is dropped (the icon from the id, the name). Hosts without the fields ignore them.
`plugin.list` shows `icon` and `header` as booleans; `plugin.picture {id, picture?: header|icon,
theme?: dark|light}` gives the SVG text.

**Where a filter comes from (ABI v1.1 proposal).** A manifest may carry `"homepage"`,
`"repository"`, `"changelog"` and `"license"`, with the same checks as a plug-in's
([Where it comes from](#what-a-plug-in-can-add)); the filter's page in the Plug-in Manager shows
them the same way, and `plugin.list` gives them as `about`. A filter has no log: a value dropped is
named in the install dialog, in `plugin.inspectFile` (`dropped`) and as a warning in the app's log.
Hosts without the fields ignore them.

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

- Jobs can't change the catalog yet: writes held for the job and applied as one undo step when it
  ends come later. `job.progress` moves only a job's row, not an export hook's or a program wait's.
- A job that keeps replying `continue` runs until it is stopped; nothing caps the number of its
  calls. With `wait` from the CLI, only ending the process stops it.
- While a job holds its plug-in busy, a `program.end` hook of that plug-in waits up to ten
  minutes, as for any busy plug-in; a job still running then drops the hook, with a warning in the
  app's log.
- Quit Anyway stops a job between calls but doesn't wait for the call then running: the app
  exits, and what that call wrote to its namespace is lost.
- A plug-in call that has started can't be interrupted (✕ takes effect between plug-ins, and
  between a job's calls), and one
  export's hooks run at a time: hooks for an export that ends while the previous ones still run
  are skipped with a message.
- Network and filesystem permissions are fixed in the manifest: a WebDAV destination for "any
  server the user types" isn't possible; the author names the hosts.
- No plug-in store, and updates aren't installed automatically or checked while the Plug-in
  Manager is closed; Lightroom Lua plug-ins don't run. Signatures say who made
  a plug-in, not whether it is safe: the permissions still decide what it may do.
- Programs: a plug-in can't read a program's output while it runs, or stop it; ✕ only stops
  waiting. It adds what a program made only from the folder of the source photo, and can't make
  an album for the results. Versions are read on macOS only.
- Pixel filters: a Display filter with `overlap` > 0 still works in output pixels (it runs after
  every develop step, so a native-resolution pass would mean a full-size develop per preview), so
  a preview and a larger export of one differ.
