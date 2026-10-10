# Add to Apple Photos (macOS)

An export can hand the files it wrote to Apple Photos, optionally into an album (issue #236, parity row
`LR-EXP-PHOTOS`). macOS only: on other platforms the option is hidden in the Export dialog and refused by
`app.export`. The web build doesn't include it.

> **Tested end to end** on macOS 26.6.2 with Photos 11.0, using the ad-hoc-signed packaged app and a new, empty
> Photos library: the [manual test](#manual-test) below, except the two `lightcraft-cli` steps (11 and 12). Please
> run it again after changing the script or the packaging.

## Using it

- **Export dialog:** tick **Add to Apple Photos** under *If file exists* and, if you like, type an **Album**
  name (empty = no album; see [Which album](#which-album)). The files are exported as usual, into the folder
  you chose, and then added to Photos. The toast says how many Photos took, or why it took none.
- **Export with Previous** and saved export presets repeat it (the setting is part of the export options).
- **Agents and scripts:** `app.export {…, addToPhotos: true, photosAlbum?: "Name"}` (desktop app, MCP `export`
  tool, `lightcraft-cli run`); `lightcraft-cli render in.dng -o out.jpg --opt addToPhotos=true --opt
  photosAlbum=Name`. The export's result carries `applePhotos` (below). The files are written either way: a
  failed Photos step doesn't fail `app.export` (its `applePhotos` has the `error`), while `lightcraft-cli render`
  says the file was written and exits with the error.
- **Files already on disk:** `export.addToPhotos {paths: [absolute paths], album?, wait?}`.
- **How an import went:** `export.photosImports` lists this session's imports, oldest first;
  `export.photosImports {job}` gives one.

### Waiting, or not

An import can take minutes, and the first one waits for the user to answer macOS's permission prompt. Each
import is a job: `{job, running, requested, album}`, plus, once Photos is done, `{imported, ids, warning?}`,
`{error}` or `{skipped}` (`exporting: true` while an export that will add its files is still writing them).

- In the **desktop app** nothing waits for Photos on the window's thread. The Export dialog's background export
  adds the files on its worker and reports them in its toast. `app.export` without `background` and
  `export.addToPhotos` answer at once with the running job (`running: true`); the app shows a toast when Photos
  is done, and `export.photosImports {job}` gives the outcome. (`export.addToPhotos {wait: true}` waits there
  too, holding the window until Photos is done.)
- **Without a window** (the MCP server, `lightcraft-cli`, the headless snapshot) everything waits for Photos by
  default: `app.export` and `export.addToPhotos` return the finished job, so a one-shot command never ends in
  the middle of an import. `export.addToPhotos {wait: false}` returns the running job instead (for a
  long-running MCP server that polls `export.photosImports`); a process that is about to end (the CLI after
  its last command, the MCP server at end of input, a snapshot script, also when they end with an error)
  first waits for such an import to finish. The wait follows the import: while an export is still writing the
  files it will add, up to an hour more of writing; once Photos is asked, the import's own time limit (see the
  table below) and half a minute; never more than three hours and one minute in all.
- **One import at a time.** An export that will add to Photos reserves the import before it writes anything
  and holds it until it hands its files over (or is cancelled, fails or writes nothing). While an import is
  reserved or running, another (`export.addToPhotos`, or an export asking for Photos, refused before it writes
  anything) is refused with the number of the one in the way, so a retry never sends the same files twice and
  an export is never turned away after writing its files. When LightCraft stops waiting (see the table below),
  Photos may still be importing: look in Photos before trying again.

### Which album

The album is looked for by its exact name (case included) among the albums at the **top level** of the
Photos library. Albums inside folders are never used: when the only album of that name is in a folder, a new
top-level album is made. When none is there, a new top-level album is made. When **more than one** top-level
album has the name, nothing is imported and LightCraft says so: rename one of them in Photos, or use another
name. (Photos' own list of albums includes those in folders, in no particular order, so the script checks
each one's parent folder.)

Photos either copies the files into its library or references them where they are, as set in Photos ▸
Settings ▸ General ▸ *Copy items to the Photos library*. When it references them, keep the exported files.

## How it works

LightCraft runs `/usr/bin/osascript` with a constant AppleScript (`SCRIPT` in
`crates/engine/src/apple_photos.rs`) that asks Photos, through its scripting interface, to import the files:
`import … into album … skip check duplicates false`. The album is found or made first ([Which
album](#which-album)). The script returns the new media items' ids. Files go to Photos 200 at a time (each path is an osascript argument, and
macOS limits a program's arguments to 1 MiB), one run after another within the import's time limit; the first
run makes the album when it is missing.

This is Apple Events to Photos' own import command. It is not the OS-level synthetic input that AGENTS.md
forbids: nothing is typed or clicked, and no other window is touched.

The album name and the file paths are never written into the script. They follow it as separate arguments
(`on run argv`), each passed to osascript unchanged, so quotes, backslashes, line breaks or a leading `-` in a
name or path can't change what the script does. A constant first argument keeps osascript from reading a
name that starts with `-` as one of its own options.

The first time, macOS asks whether LightCraft may control Photos. The answer is kept in System Settings ▸
Privacy & Security ▸ Automation. macOS asks about the app that started LightCraft's work: the LightCraft app
when it was opened from the Finder or with `open`, but the **terminal** for `lightcraft-cli` and for a
LightCraft started from a terminal (`cargo run`).

**Packaged app.** The release app is signed with the hardened runtime. For it to be allowed to ask, it carries
the `com.apple.security.automation.apple-events` entitlement (`packaging/macos/entitlements.plist`, also used
for `lightcraft-cli`), and its `Info.plist` has the `NSAppleEventsUsageDescription` text macOS shows in the
prompt (`packaging/macos/Info.plist.in`). Without them macOS refuses the events (`-1743`) without asking.
`cargo run` doesn't test this configuration; the manual test below uses the packaged app.

**Who can add what.** `export.addToPhotos {paths}` adds any file LightCraft can read, given its absolute path,
not only files it exported. So any client of the control port or the MCP server can put any readable image or
video on the Mac into the user's Photos library, and from there into iCloud Photos when that is on. macOS asks
for the Automation permission once per app, not per import, so after the first Allow nothing asks again. The
control port listens on 127.0.0.1 only and has no authentication ([control-protocol.md](control-protocol.md)),
so any program on this Mac that can connect to it can do this, including programs of other users logged in to
the Mac. The MCP server talks only to the program that started it (stdio). Treat giving an agent the MCP server
or the control port as giving it this too, and turn the control port on only when you need it.

## When it doesn't work

| What you see | Why, and what to do |
|---|---|
| "LightCraft isn't allowed to control Photos…" | Permission refused (`-1743`). Turn on Photos under LightCraft (or your terminal app) in System Settings ▸ Privacy & Security ▸ Automation, then try again. |
| "Apple Photos couldn't be found…" | Photos isn't installed or couldn't be opened (`-600`, `-10810`, `-10814`, or no `/System/Applications/Photos.app`). |
| "The import was cancelled in Photos." | Someone pressed Cancel in a Photos dialog (`-128`). |
| "More than one album at the top level of Photos is named …" | Several top-level albums have that name; nothing was added. Rename one in Photos, or use another name. |
| "Photos didn't finish adding the photos within N s…" / "didn't answer in time" | LightCraft waits ten minutes plus five seconds a file (at most two hours), then stops osascript. The ten minutes leave time to answer macOS's permission prompt and Photos' duplicates question. Photos may still be importing: look in Photos before trying again. |
| "Apple Photos is still adding N file(s) from an earlier request (import K)…" / "An export is about to add its files to Apple Photos (import K)…" | One import at a time: wait for import K (`export.photosImports {job: K}`), then try again. |
| "Photos added 2 of 3 files…" | A partial import: Photos skipped files it found to be duplicates (it may ask first) or couldn't read. |
| "Apple Photos is only available on macOS" | The setting came from a Mac (a preset or Export with Previous); the Export dialog drops it on other platforms. |

## Manual test

Run this once on a Mac whose Photos library you are happy to add a few test images to (or switch Photos to a
new empty library first: hold ⌥ while opening Photos ▸ Create New).

1. Package the app (ad-hoc signed unless `MACOS_SIGN_IDENTITY` is set): `packaging/macos/package.sh --arch
   aarch64` (or `x86_64` on an Intel Mac). Check its entitlement: `codesign -d --entitlements -
   target/macos-package/LightCraft.app` lists `com.apple.security.automation.apple-events`. With a Developer ID
   build, also run the notarized DMG from `dist/release/`.
2. Open it from the Finder, or with `open target/macos-package/LightCraft.app --args --demo` (not by running
   the binary in a terminal, which makes the terminal the app macOS asks about).
3. Select three photos, choose File ▸ Export…, set a folder (e.g. `~/Pictures/LightCraft Exports`), tick **Add
   to Apple Photos**, type the album `LightCraft test "1"` and press OK.
4. macOS asks whether **LightCraft** may control Photos, showing the usage text from `Info.plist`: allow it.
   Expected: the toast reads "Exported 3 of 3 photos · 3 added to Apple Photos album “LightCraft test "1"”",
   and a top-level album of that name holds the three images.
5. Export the same photos again with the same settings. Expected: Photos asks about duplicates or imports them
   again; the toast reports the count Photos returned (a partial import shows its warning).
6. Albums: in Photos make a folder holding an album `Nested`, then export into `Nested`. Expected: a new
   top-level album `Nested` gets the images, the one in the folder doesn't. Then make a second top-level album
   `Nested` and export again. Expected: nothing is added, and the toast says more than one album has the name.
7. Permission refused: in System Settings ▸ Privacy & Security ▸ Automation turn off Photos under LightCraft,
   export again. Expected: the files are written, and the toast says to allow LightCraft there. Turn it back
   on.
8. No album: keep Add to Apple Photos ticked, clear the Album field and export. Expected: the images land in the
   library, in no album.
9. Hostile names: album `-e`, then `a\b`, and an export folder whose name contains `"` and `'`; a name with a
   line break through the CLI: `lightcraft-cli run --demo app.export dir=/tmp/lc-photos addToPhotos=true
   "photosAlbum=$(printf 'two\nlines')"`. Expected: albums named exactly so; nothing else happens.
10. The window stays responsive: start the app with `open target/macos-package/LightCraft.app --args --demo
    --control 18999`, then send `{"method": "engine.execute", "params": {"command": "export.addToPhotos",
    "params": {"paths": ["/tmp/lc-photos/<file>.jpg"]}}}` to port 18999 (`docs/control-protocol.md`).
    Expected: an immediate `{"job": …, "running": true}`; the app keeps responding; a toast when Photos is done;
    `export.photosImports` then shows the outcome.
11. Agents: `lightcraft-cli run --demo app.export dir=/tmp/lc-photos addToPhotos=true photosAlbum=CLI` (allow
    the terminal under Automation when asked). Expected: `applePhotos.imported` in the printed result.
12. Files on disk: `lightcraft-cli run export.addToPhotos 'paths=["/tmp/lc-photos/<file>.jpg"]' album=CLI`.
    Expected: the command waits for Photos and prints `{"job": …, "running": false, "imported": 1, …}`.

Report the toast or JSON, and the macOS and Photos versions.

## Limitations

- Albums inside folders are never used ([Which album](#which-album)).
- Photos' duplicate check stays on, so Photos may ask about duplicates; what it skips shows as a partial import.
- Cancelling a running export cancels the Photos step too; a Photos import already started can't be stopped
  from LightCraft.
- One import runs (or is reserved by an export) at a time; a session remembers its last 32.
- Quitting the desktop app while Photos is importing doesn't wait for it: Photos may still finish the import,
  but LightCraft won't show the outcome.
