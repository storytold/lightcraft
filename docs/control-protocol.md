# Control protocol

`lightcraft --control 7980` (or `LIGHTCRAFT_CONTROL_PORT=7980`) starts a JSON-lines server on
`127.0.0.1:7980` (loopback only). One request per line, one reply per line, in order:

```text
→ {"id": 1, "method": "engine.execute", "params": {"command": "photo.rate", "params": {"rating": 4}}}
← {"id": 1, "ok": true, "result": null}
← {"id": 2, "ok": false, "error": "unknown command `nope`"}
```

Requests are answered on the UI thread between frames (timeout 60 s). A request is given up
if the app has not started it and draws no frame for 5 s. Synthetic pointer input is fed one event
per frame; the same 5 s limit is a delivery stall, restarted whenever the app consumes a batch, so a
many-step drag on a slow frame is not cut off while an app that stops drawing still fails. The reply is then `{"ok": false, "error": "the app drew no frame for 5 s, …"}`,
and the app never runs that request later, so a retry doesn't do it twice. This happens on
Wayland when the window is on a hidden workspace or minimized: the compositor sends no frame
callbacks, so the app's loop is paused. Keep the window on screen while you drive it, or use
headless snapshots or `lightcraft-cli mcp` without `--connect`. A request that has already started
(a long command blocks frames) still gets the full 60 s.

Connections close after five minutes without incoming bytes while the server is waiting for
input, including in the middle of an unfinished request line. This allows pauses between
automation commands while releasing abandoned connection slots. An idle read timeout gets one
reply without an `id`, then the connection closes; incomplete input is never dispatched:

```text
← {"ok": false, "error": "control connection idle read timed out; closing the connection", "closed": "idle"}
```

This is a socket read inactivity limit, not a total connection lifetime: a peer that keeps
sending bytes can keep it open. Waiting for an already-dispatched command's reply does not
count as idle reading and retains the request timeout above.

Reconnect before sending another request after an idle close. A request sent just as the
connection expires crosses the notice: the client reads the notice where it expects the reply.
The server reads nothing after its idle timeout, so that request never ran, and `"closed": "idle"`
(only on this notice) says it is safe to send it again, once, on a new connection. Apps from
before the marker send the same notice without it; match its exact `error` text for them. MCP
connect mode does both: it detects closed connections before sending and reconnects, and
resends a request that crossed the idle notice once. Any other lost connection after sending a
request leaves its outcome unknown, and the client must not blindly replay it.

**Only requests are read.** Every line must be a JSON object with a string `method` (`id` and `params` are
optional; blank lines are skipped). Anything else — text that isn't JSON, a JSON array or number, an object without
`method`, invalid UTF-8, or a line longer than 4 MiB — gets one error reply
(`{"ok": false, "error": "… closing the connection"}`) and the server **closes the connection**; nothing sent after
it on that connection runs. This keeps an HTTP request (for example a web page's cross-origin `fetch` to
`127.0.0.1:<port>`) from smuggling a command in its body: its request line is rejected first. Junk never reaches the
UI thread, and at most 16 connections are served at once (further ones get an error line and are closed). Clients
that hit an error reply should reconnect. The port has no authentication, so only enable it when you need it. The MCP server's connect
mode ([mcp.md](mcp.md)) is a thin layer over this channel. Implementation:
`crates/ui-egui/src/control.rs` (methods) and `apps/lightcraft/src/control_server.rs` (transport).

## Isolated automation profiles

Set `LIGHTCRAFT_CONFIG_DIR` to a fresh directory to isolate UI preferences, app logs, the emergency
panic log, the GPU initialization marker, camera profiles and default model folders. The nonempty
value is used as the complete settings directory. Unset retains the normal platform paths; an explicit
empty value disables the settings directory, while the emergency panic log retains its system-temp path.
Explicit model-directory environment variables still take precedence over their defaults.
Also pass `--library DIR` with a separate scratch library: the configuration override does not change
the default photo-library location. Do not use `--memory` or `LIGHTCRAFT_NO_PREFS` when testing persistence.

A native acceptance run must reject the stderr message `rendering headlessly`: `ui.screenshot`
automatically uses the CPU fallback after two seconds without a presented frame. That fallback is useful
for headless scripts, but cannot establish native-window acceptance. Control input injects egui events;
it does not establish operating-system input delivery or screen-reader acceptance.

On macOS, the native menu owns its keyboard accelerators. `ui.key` injects egui key events and
does not synthesize the Cocoa menu events for those shortcuts. Use `ui.menu.invoke` with the
command ID (for example `edit.undo`, `edit.redo`, `view.filmstrip` or `view.leftPanel`) to test
their command behavior. That API invokes the app command dispatcher directly; it does not traverse
Cocoa menu dispatch. Native menu activation and keyboard accelerator delivery require a separate test.

Before capturing a finished Detail view, poll `ui.inspect` until the expected `loupe.photo` is active,
`loupe.source` is `render` or `cached`, `loupe.pending` is false, and the render queues are empty — or until
`loupe.failed` is set: that photo's render failed, and what is on screen is all there will be.
For the loupe, `cached` is a prior view render keyed by the current photo and develop settings;
`thumb`, `small` and `embedded` are separate fallback sources. A presented frame can otherwise
contain an in-progress rendering placeholder. Require loaded thumbnail textures only when the
filmstrip is visible.

## Methods

| Method | Params | Result |
|---|---|---|
| `engine.execute` (alias `ui.menu.invoke`) | `{command, params?}` | Run any engine or UI command (see `engine.commands`) |
| `engine.commands` | — | Engine + UI commands: id, label, menu, shortcut, params doc, enabled |
| `ui.menu.list` | — | Menu entries (flat: id, label, menu path, shortcut, enabled) |
| `ui.menu.tree` | — | The menu bar as shown (File … Help): items `{id, params?, label, shortcut?, enabled, checked?}`, separators, submenus — the model behind the native macOS menu bar and the in-window menus |
| `ui.inspect` | — | UI state, window, `loupe` (`source`, `pending`, `failed` = why its render failed, else `null`, and `region: {full, window, pending}` when a zoomed view also renders the window on screen at no more than 100 % — `full` is the frame it is cut from, `window` its `[x, y, w, h]` — and `regionBefore` for the Before side of a Before/After view), canvas/image rects, `scroll: {grid, filmstrip}` (scroll offsets in points, `null` until drawn), active photo, selection, perf (`frameMs` = layout, `logicMs` = per-frame logic before it, `updateMs` = both, `maxUpdateMs`, `fps`, render queue …, `gpu` = adapter in use, `gpuReason` = why renders don't use the GPU, `gpuFallback` = latest render redone on the CPU and why — see `docs/gpu-pipeline.md`), status, memory (bytes per cache, see `library.memory`; plus stage caches — `budgetBytes`, `trimmed`, `sharedSourceBytes` (the one device copy of a big original) — and textures), `export: {running: {total, done, current} \| null, last}`, `notices` (warnings waiting to be shown, e.g. a damaged settings file; OK = `button:noticeOk`), `quitPrompt` (why quitting was stopped: tasks that would be cut short are running — `button:quitAnyway` cancels them and quits, `button:quitCancel` keeps working — or unsaved changes: `button:quitRetry` / `button:quitAnyway` / `button:quitCancel`), `import: {done, total, imported, cancelled} \| null` (an import runs on a worker thread), `tasks` (other background work: `Find Missing Photos`, `Auto Import`), `activity` (every long-running task in flight as the activity stack in the top-left corner shows it, same shape as `activity.list`; a row appears once its task is 0.5 s old, ids `activity:row:<id>`, ✕ = `activity:cancel:<id>`, `activity:more` expands past 3 rows), `fileDialogs` (commands waiting on a native file dialog, which runs off the UI thread; they run again with the answer when it closes), `copied` (the text the UI last put on the clipboard, `null` until something is copied) |
| `ui.widgets` | `{filter?}` | On-screen widgets `{id, rect: [x, y, w, h]}` (screen points) |
| `ui.clickWidget` / `ui.dragWidget` / `ui.hoverWidget` | `{id, count?, button?, fx?, fy?}` (`button: "right"` right-clicks; `count` is capped at 16) / `{id, toX?, toY?, dx?, dy?, steps?}` (steps are capped at 1000) / `{id, fx?, fy?}` | Real egui input on a widget (hover: the pointer rests on it, e.g. for preset/profile previews). `fx`/`fy` are fractions of the registered rect and may lie outside `[0,1]`; the resulting point must still be inside egui's clipped interaction rect, and a non-finite fraction or resolved point is rejected before any input is queued. Geometry comes from the latest completed layout; if the widget moves before the first event or before a press, or its latest clipped rect no longer contains the point, the request fails with an error instead of clicking another widget or retargeting. A target below an ordinary modal dialog may succeed as a no-op or a backdrop click, because egui routes that input to the dialog; a target covered by a non-modal overlay fails. The blocking library-problem/launch-list window is explicit: its dim takes the modal layer, but a background target fails instead of reporting a no-op success, and only that window's own controls (`button:library…`) pass. Its creation frame is shielded and redrawn with empty input like a prompt's, so background text/pointer input cannot act while the new modal areas settle. A quit prompt or settings notice is a real egui modal scope with a foreground backdrop. Only its own buttons (`button:quit…`, `button:noticeOk`) accept a `ui.clickWidget`; background targets fail, and background keystrokes, text/clipboard input, wheel/zoom input and AccessKit actions are dropped before panels see them. The prompt itself takes Tab/Shift+Tab, Enter/Space on the focused button, and Escape/Enter for the safe default Cancel; AccessKit Focus/Click works when targeted at a prompt button. Its first creation frame (including a Tasks→Unsaved or notice→next-notice transition) is shielded and redrawn with empty input, so a press/release in that frame, or a held press from before it, cannot act underneath it; a synthetic drag already in progress is canceled. Once settled, raw `ui.click`/`ui.drag`/`ui.pointer` and backdrop clicks are still delivered to the prompt backdrop as no-ops, and the prompt's own button clicks work normally. The area/layer check cannot see a different widget covering the point in the same layer. A `ui.clickWidget` whose widget disappears before its release also fails, after canceling the pointer press without a click or drag (the same rules apply when a click spans several host frames) |
| `ui.move` / `ui.click` / `ui.drag` | `{x, y, count?, button?}` / `{x, y, toX, toY, steps?}` | Raw pointer input, screen points (the same click and step caps apply). In every pointer and scroll gesture a number other than `count` and `steps` must lie between −1,000,000 and 1,000,000, or the request is refused and queues nothing |
| `ui.pointer` | `{events: [{kind: down\|drag\|up, x, y}], alt?, shift?, cmd?}` | Gesture in normalized image coordinates (Detail view) |
| `ui.key` | `{key, action?: press\|down\|up, cmd?, shift?, alt?, ctrl?}` | Key press (default), or the key going `down` and staying held until an `up` (Space held over a `ui.pointer` drag); while a prompt is up only its Tab/Shift+Tab, Enter, Space and Escape are delivered, every other key is dropped |
| `ui.text` | `{text}` | Text input |
| `ui.clipboard` | `{action: cut\|copy\|paste, text?}` | Cut, copy or paste in the focused text field, as the system does for ⌘X / ⌘C / ⌘V; `paste` pastes `text`, or the system clipboard without it. Refused when no text field has the focus (outside one, ⌘C / ⌘V are the commands `develop.copy` / `develop.paste`). In the desktop app cut and copy write the system clipboard. Refused while a prompt or the blocking library window owns input, including a window in its creation frame. A `paste` without `text` queues egui's `RequestPaste` viewport command for the host: egui 0.37 gives that command no completion/correlation id, and egui-winit emits a later `Event::Paste` only for non-empty text (or a `PasteImage` fallback; asynchronous clipboard reads finish later), so the reply confirms the request was queued in egui, not that the host dispatched it or that it was delivered; a barrier created after the request can drop that later event. `paste` with `text` is synthetic and is canceled like other injected input. `ui.inspect` → `copied` is the text the UI last put on the clipboard |
| `ui.scroll` | `{dx, dy, source?, unit?, phase?, cmd?, ctrl?, shift?, alt?}` | Wheel / two-finger scroll at the current pointer; pans over the image, modifier-scroll zooms. `source` is `unknown` (default), `wheel`, `trackpad` or `momentum`; `unit` is `point` (default), `line` or `page`; `phase` is `move` (default), `start`, `end` or `cancel`. Wheel/line events drive the grid/filmstrip one-third wheel step and the loupe's notch zoom; trackpad/momentum phases drive kinetic scrolling and rubber-band boundaries. Invalid values are refused and queue nothing |
| `ui.zoom` | `{factor}` | Pinch zoom at the current pointer (positive scale multiplier; 1 = unchanged). Position it first with `ui.move` or `ui.hoverWidget` |
| `ui.set` | partial UI state, e.g. `{"view": "detail"}` | Resulting UI state |
| `ui.dialog.confirm` / `ui.dialog.cancel` | — | Close the open dialog (confirm runs its action; Synchronize Folder on a folder that is up to date answers `{"upToDate": true}` and starts nothing) |
| `ui.resize` | `{width, height}` | Resize the window |
| `ui.dropFiles` | `{paths: [..], hover?}` | Files dragged from the file manager. `hover: true` makes every following frame report `paths` as hovered over the window (the drop card appears, `ui.widgets` lists `dropOverlay`) until another `ui.dropFiles` replaces them; `{paths: [], hover: true}` ends the hover. Without `hover` the files are dropped: the hover ends and the next frame receives them as dropped files, exactly as an OS drop arrives (photos and folders open the import review, or import at once with Settings ▸ Import ▸ Import dropped files without review). An empty path stands for a file whose path the platform doesn't tell |
| `ui.zoomFactor` | `{factor}` | Set the interface scale (0.5–3; egui's zoom keys are off, Cmd+= / Cmd+- zoom the photo) |
| `ui.screenshot` | `{path?, headless?}` | `{path, width, height}` once the frame (with finished renders) is captured. A `path` that did not exist when the request was admitted is written with create-new semantics and is never replaced if a file (including a newly imported original) appears there while the capture is queued; an existing ordinary file may be refreshed. `headless: true` draws the UI on the CPU (no compositor needed); a windowed capture that gets no frame within 2 s falls back to headless automatically. While the loupe still waits for a render, `ui.widgets {filter: "loupe:rendering"}` lists its "Rendering…" pill; `loupe:renderFailed` is the pill saying a render failed behind a stand-in; Compare and Reference panes have their own (`compare:rendering:<pane>`, `compare:renderFailed:<pane>`) |
| `engine.execute {command: "app.export", params}` | export params (see `docs/mcp.md`), plus `preset`, `dir` / `path`, `ids`, `background`; `destinationMode: "sameAsOriginal"` writes each photo into its original's folder (+ `subfolder`) instead of `dir` | Writes the files and returns `{files}` (one `{path, width, height, bytes, sidecars}` per file, plus `gpuFallback` — why that render fell back from the GPU to the CPU — when it did; see `docs/gpu-pipeline.md`); with `background: true` (what the Export dialog and menus use) it returns `{background: true, total}` at once and the batch runs on a worker thread — poll `ui.inspect` → `export` |
| `engine.execute {command: "activity.list"}` | — | The long-running tasks in flight (imports, exports, preview builds, downloads, the face scan…), oldest first: `{tasks: [{id, kind, label, done, total, unit, detail, cancellable, cancelling, ageMs}]}`; `total` 0 = not known yet; `unit` is `count`, `bytes` or `percent`; `label` is English (the app shows it translated) |
| `engine.execute {command: "activity.cancel"}` | `{id}` or `{all: true}` | Asks the task (or every cancellable one) to stop at its next file or photo → `{cancelled: n}`; an unknown id (it may have just finished) or a task that can't be cancelled is an error |
| `ui.render` | `{id?, size?, path?}` | Render a photo (PNG to `path`), `{width, height}` |
| `engine.execute {command: "slideshow.layout"}` | `{margins?, marginLeft?, …, zoomToFill?, strokeBorder?, castShadow?, backdropColor?, colorWash?, backgroundImage?, …}` (see `engine.commands`) | The slideshow's slide layout (Layout, Options, Backdrop), saved with the library; omitted keys keep their value, `{}` reads it; out-of-range numbers are clamped, a wrong type or unknown key is refused and nothing changes |
| `engine.execute {command: "slideshow.overlays"}` | `{identityPlate?: {enabled?, text?, image?, size?, color?, opacity?, anchor?, offsetX?, offsetY?, relativeTo?, behind?}, ratingStars?: {enabled?, color?, size?, anchor?, …}, texts?: [{text?, size?, color?, opacity?, anchor?, offsetX?, offsetY?, relativeTo?, shadow?, shadowOpacity?, shadowOffset?, shadowRadius?, shadowAngle?}], addText?, removeText?, showWatermark?, watermark?}` (see `engine.commands`) | The overlays drawn on every slide, saved with the library: sizes and offsets are fractions of the slide's shorter side, anchors `topLeft`…`bottomRight` of the `slide` or the `photo`; text overlays expand tokens (`{caption}`, `{exposure}`, `{date}`, the file-naming tokens…; unknown tags show as typed); `watermark` takes the export watermark's keys. Replies with the settings and `warnings` |
| `engine.execute {command: "slideshow.titles"}` | `{intro?: screen, ending?: screen}`, a screen being `{enabled?, color?, content?: "none"\|"identityPlate"\|"text", text?, overrideColor?, textColor?, scale?, duration?: seconds\|null}` | The intro and ending screens, saved with the library: a background colour and, centred, the Overlays identity plate or the screen's own text, in the plate's colour or `textColor` when `overrideColor` is on, `scale` = its height as a fraction of the slide's shorter side; `duration` null = the slide duration. The intro plays once before the first photo, the ending after the last when repeat is off. Replies with the settings and `warnings` |
| `engine.execute {command: "slideshow.preview"}` | `{id?, title?, template?, width?, height?, path?}` | One slide composed with the layout and overlays, as the slideshow plays it (with `template: id`, with that template's settings instead, leaving the library's alone: the Template Browser's hover preview) (with `title: "intro"\|"ending"`, that title screen instead → `{title, enabled, width, height, plate}`): a PNG written to `path`, or returned base64 in `png` without one; `{id, width, height, cell, photo, overlays}` (`cell` and `photo` are `[x0, y0, x1, y1]` in slide pixels; `overlays` lists `{kind, box}` for each drawn overlay). Sides 16–4096; one side given takes the other from the aspect preview |
| `engine.execute {command: "slideshow.templates.list"}` | `{}` | The Template Browser: `{templates: [{id, name, builtin, group, description?, path?}], previous, folder, problems}` — built-ins (`builtin:…`, our own designs) then user templates (`user:<file>`, files in `<settings>/slideshow-templates`, at most 1000); `problems` lists files skipped and why |
| `engine.execute {command: "slideshow.templates.apply"}` | `{id}` or `{name}`; `{id: "previous"}` | Replaces the playback (prepare previews kept), layout, overlays and titles settings with the template's, saved with the library. No undo: the replaced settings become `previous` for the session → `{applied, name, warnings, previous, settings}` (`warnings`: a background photo not in this library, an unreadable plate or watermark image…) |
| `engine.execute {command: "slideshow.templates.save"}` / `update` / `rename` / `delete` | `{name, replace?}` / `{id}` / `{id, name}` / `{id}` | Save the current settings as a user template (`<name>.json`; path separators and reserved characters become `_`, a taken or built-in name is refused unless `replace`), update one with the current settings, rename (its file follows), delete. Built-ins can't change |
| `engine.execute {command: "slideshow.templates.import"}` / `export` | `{path}` / `{id, path}` | Import a template `.json` (at most 256 KB; format, version and every setting checked; a taken name gets " (2)") → `{id, name, renamed, warnings}`; export any template as `.json` |
| `engine.execute {command: "slideshow.export"}` | `{format: "pdf"\|"jpeg"\|"video", path, size?, quality?, fps?, container?: "mp4"\|"webm", fullScreen?, speed?, name?, ids?, wait?}` | The slideshow (the photos in view, or `ids`, as composed slides between the intro and ending screens that are on) as a PDF (one page per slide; `size` = long side 64–8192 px, default 1920), a JPEG sequence (`path` = the folder, files `<name>-001.jpg`…) or AV1 video in MP4 / WebM (`size` = frame height 64–2160 or `"720p"` / `"1080p"`, `fps` 24 / 25 / 30 / 60; fades rendered as frames; no audio; at most 2 hours). Runs in the background → `{taskId, background, slides, seconds, total, width, height, warnings}`; its row is in `activity.list` (`kind: "export"`, label "Exporting slideshow"), `activity.cancel {id: taskId}` stops it and leaves no file (outputs are written as `.part` and renamed when complete). `wait: true` runs it in the call and replies with the result. One at a time |
| `engine.execute {command: "slideshow.exportStatus"}` | — | The last slideshow export: `{taskId, format, path, running, done, total, cancelled, result?, error?}`; `result` has `pages` (PDF), `files` (JPEG) or `frames`, `samples`, `keyFrames`, `seconds`, `fps`, `container`, `codec: "av1"`, `audio: false` (video), and `skipped` / `warnings` for photos that could not be rendered |
| `engine.execute {command: "dialog.slideshowExport"}` / `app.slideshowExport` | `{format?}` / `{format, size?, quality?, fps?, container?, fullScreen?, path?}` | View ▸ Export Slideshow…: opens the dialog; `app.slideshowExport` asks for the path with a save dialog when none is given, then starts `slideshow.export` |
| `app.quit` | — | Close the app |

White balance (`develop.set` with `wb.temp` / `wb.tint`, `develop.wb`, `develop.wbPick`) is in Kelvin and tint on
raws with colour matrices — in the file (DNG) or measured for the camera model (`lightcraft_raw::spectral`; issue
#730) — and on raws whose white-balance presets give the scale (`lightcraft_raw::kelvin_presets`), where As Shot
reads the file's as-shot white; rendered files and raws without matrices use a relative scale
whose 6500 K / 0 is the as-shot look (the Edit panel shows it as −100..+100). The stored `wb` of such a Kelvin raw
says `"scale": "kelvin"`; a partial settings JSON given to `develop.merge`, to a preset's `wb`, or to
`lightcraft-cli render --settings` and which sets `temp` / `tint` **without** `scale` keeps the relative meaning it
always had (6500 = as shot) on every raw, so agents that mean Kelvin on a Kelvin raw pass `"scale": "kelvin"` or use
`develop.set` / `develop.wb`. A partial that names only one of `temp` / `tint` is resolved against the photo
(issue #2910): the photo's white balance is first brought onto the partial's scale with the photo's own as-shot
white, so the field the partial does not name keeps the look it had instead of being re-read on the partial's scale,
and the stored result is back on the scale the photo is edited on (`"scale": "kelvin"` on a Kelvin raw). The same
holds for `preset.apply` when the preset names only one of the two numbers; a preset written by the app, or imported
from crs / XMP, names `mode`, `temp`, `tint` and `scale` together and behaves exactly as before. A photo whose white
balance mode answers its own white — `As Shot`, or a named preset on a raw edited in Kelvin — has that white written
out as `Custom` values first, because a number merged into such a mode would otherwise be stored where no render
reads it (the panel would show 5800 while the photo stayed at as shot); a partial that names a `mode` itself stays
authoritative. Partials naming both fields mean the same as before.

Image navigation is also available directly as the UI command `view.navigate`, with
`{zoom?: "fit" | "fill" | {"percent": number}, pan?: [x, y]}`. Percentage zoom accepts fractional
values greater than 0 and at most 800; pan is the normalized image centre, with coordinates from 0 to 1.
Pinching keeps the image point under the pointer steady and zooms between Fit and 800%; two-finger
scrolling pans in both axes and respects the operating system's scrolling direction and momentum.
These gestures work in Detail (including editing tools and full-screen preview), Compare and Reference
views, and only apply over their image areas. Panning stops at the image edges.

The camera's focus point ([focus-points.md](focus-points.md)) is `meta.focus` in `photo.inspect`: `{rect: {x0, y0,
x1, y1}, mode?, area?, tracking?}`, normalized to the upright photo (absent when the file records none).
`view.focusPoint {on?}` shows it over the photo (no params toggles; replies `{on}`), and `view.zoomToFocus {on?}`
(on by default) makes `view.zoomToggle` zoom in centred on it, replying `{focus: [x, y]}` (the `pan` it set).
`photo.readFocusPoints {ids?, wait?}` reads the point for photos imported before it was read, from a few KB of each
file, on a background job (activity kind `focus`); `wait: true` returns the run's counts when done. The app calls
`focus.pump {pace}` every frame to drive it.

With several photos selected, the Info panel's Show Metadata For is `view.infoSelectedPhotos {on?}` (no `on`, or
`on: null`, toggles; any other value that is not true or false is refused; replies `{on}`;
`settings.infoSelectedPhotos`): off (Target Photo) shows the active photo's values and saves edits to it alone, on (Selected Photos, the default)
shows the values the selected photos share, leaves each text field and GPS where they differ empty with a Mixed hint,
and shows Mixed as the Copyright Status when the statuses differ.

The interface theme is `view.appearance {mode?: "auto" | "light" | "dark"}` (no mode: the next one,
Auto → Light → Dark → Auto, like the top bar's `icon:appearance` button) and `view.theme {theme:
"charcoal" | "midnight" | "silver" | "paper"}`, which picks a theme and fixes the mode to its family.
Both reply `{mode, darkTheme, lightTheme}`; the same values are `settings.appearanceMode`,
`settings.darkTheme` and `settings.lightTheme` in `ui.set`. Auto follows the system (on Linux the
XDG desktop portal, watched for changes rather than polled) and is dark when the system says nothing.
Mask brushing uses the UI command `tool.brush` through `engine.execute`. With `{}` it activates
painting on the current mask. `{"new": true}` creates a separate brush mask; `{"op": "add"}` or
`{"op": "subtract"}` appends a brush component to the selected mask. Creating either starts in
paint mode with the overlay visible. Brush Erase (or Alt while painting) removes strokes' coverage
within that brush component; it is distinct from subtracting the component from the whole mask.

### When the library can't be saved

With a persistent library, every command that changes something is written to the catalog journal (fsynced) before
it replies. If that write fails (disk full, volume gone, permissions), the command replies `ok: false` with
`"saved in memory but not written to disk: <reason>; LightCraft will retry"`. The change itself **is** applied (and
undoable) and stays queued: the next command, and the app's frame loop every couple of seconds, retry the write, so
nothing is lost once the disk is writable again — unless the app quits first. Meanwhile `ui.inspect` → `unsaved`
is `{ops, error}` (else `null`), `library.info` reports `unsavedOps` / `unsavedError`, and the top bar's cloud icon
shows a warning (widget `indicator:unsaved`). Queries and commands that change nothing still succeed. A failed
compaction (snapshot) is not a failed command — the log is kept whole — and only shows in `library.info` →
`lastError`.

### When the library can't be opened

If the desktop app can't open its library at launch (another program has it open, an unreadable or newer-format
catalog, a missing drive), the session starts empty and in memory — never with demo photos — and a window asks what
to do: `ui.inspect` → `libraryProblem` is `{path, error, temporarySession, pendingImport}` (else `null`); its buttons
are `button:libraryRetry`, `button:libraryChoose`, `button:libraryTemporary` (Continue Without Saving) and
`button:libraryQuit`. A temporary session shows a banner (`indicator:temporarySession`, `button:libraryReopen`) and
writes nothing. `app.openLibrary` opening a library ends it. `pendingImport` counts the files waiting for a
library: those given on the command line, and photos dropped on the window while it (or the launch list below) is
up (desktop app); they are imported into the library that opens, or into the temporary session. Presets dropped
then are refused with a toast.

### Choosing the library at launch

With Settings ▸ General ▸ At launch set to Ask me (`ui.set {"settings": {"atLaunch": "ask"}}`; `"last"`, the
default, opens the last library), or Alt/Option held during the first 2 s after the window appears, the desktop app
shows a window listing the recent libraries (issue #1290). `--library` and `LIGHTCRAFT_LIBRARY` skip it, and so does
`--memory`; `lightcraft-cli` never shows it. `ui.inspect` → `libraryProblem.chooser` is `{libraries: [{path,
lastOpened, available, checking, reason}], selected, libraryOpen}` (else `null`; `libraryOpen: true` when it was opened
over the last library with Alt). Each library is checked read-only on a worker thread of its own, never on the UI
thread (the folder and its catalog file are looked at, never locked or written, so a library another program has
open shows as available and Open reports who has it in `libraryProblem.error`). `checking: true` until its check
answers (the row says *Checking…*, then that the drive isn't answering after 5 s); `available` is `false` only
once a check found a `reason`, so a row still checking can be opened. The selection starts on the last library and
moves to the first one that can be opened once every check has answered, unless it was moved meanwhile. Each row is the widget `libraryRecent:<index>`; the buttons are `button:libraryOpen`,
`button:libraryOpenAnother` (the folder dialog of `app.openLibrary`), `button:libraryNew` (`app.newLibrary`),
`button:libraryForget` (Remove from List) and `button:libraryQuit`; `ui.key` ArrowUp / ArrowDown move the selection,
Enter opens it and Escape quits (over an open library: closes the window and keeps it). `app.openLibrary {path}`
opening a library ends it. While it shows, the settings are saved as usual (a removal is kept).

`app.recentLibraries` returns `[{path, name, exists, checking, open, lastOpened}]`. `exists` is what the last
background check found (`false`: moved, on a disconnected drive, not a folder or no longer a library; `null`: not
checked yet, or the check couldn't tell, e.g. a folder that can't be listed, an unreadable catalog or a lookup that
failed) and `checking` whether one
is running: the command never looks at a folder itself. A failed open never marks a library missing (another program
may have it). Recent libraries are checked in the
background when the app starts (the first 8) and when the launch window opens; one not found is looked for again every
30 s (a drive plugged back in), the others only when the window comes back to the front (a timer would wake external
drives and shares). File ▸ Open Recent Library greys out only an entry a check found missing (not there, not a folder,
no library in it); an unreadable catalog, a folder that refuses to be listed, a failed open or a lookup that failed
(a share that timed out) doesn't grey it out. `lastOpened`: ISO 8601, `null` when not known; kept in `ui.json` →
`settings.recentLibrariesOpened`. `{remove: path}` forgets one entry (an error if the list
doesn't hold it) and `{clear: true}` (or `app.clearRecentLibraries`) all of them; nothing on disk is touched.

### Open Library on a Lightroom catalog

`app.openLibrary` never makes a LightCraft library inside a Lightroom Classic catalog (issue #891): given the catalog
folder (it holds a `*.lrcat`), one of its `*.lrdata` / `*.lrcat-data` folders or `Backups/`, the `.lrcat` itself or
its `-wal` / `-shm` / `.lock` files, it keeps the library open now and returns `{opened: false, lightroomCatalog:
<the .lrcat found, or null>, path}`; the window shows a dialog whose `button:dialogOk` (Import Lightroom Catalog…, or
`ui.dialog.confirm`) runs `file.importLightroom` with that catalog (no catalog found: it asks for one), which then
asks where the catalog goes when `settings.lightroomImport` is `"ask"` (next section), and `button:dialogCancel` (`ui.dialog.cancel`) does nothing. A folder that is a LightCraft library (`catalog.snap` / `catalog.log`) always
opens as one. With no library open (the library problem window) it is an error instead. The engine refuses the same
places (`EngineError::LightroomCatalog`, so `lightcraft-cli --library` and `mcp --library` too). `library.importLightroom
{status: true}` reports the catalog's `path` after the task ends as well as while it runs.

### Import Lightroom Catalog: where the catalog goes

`file.importLightroom {path?, into?, name?, location?}` (File ▸ Import Lightroom Catalog…, issue #2672). With `into`
it acts without asking: `"open"` imports into the library open now (`library.importLightroom`); `"new"` creates the
library `name` (default: the catalog's file name without `.lrcat`, or `Travel 2`… when taken) in the folder
`location` (a full path; default: the open library's parent folder) under `app.newLibrary`'s rules, opens it and
imports there, returning the import's start with `library: {path, name, photos}`. A catalog file that isn't there is
refused before anything is made; so is a new library while an import runs. Without `into`, a library on disk with
`settings.lightroomImport` `"ask"` (the default) opens a window and returns `{asking: true, path, into}` (`into`: the
choice it opens with, `"open"` when the library is empty, else `"new"`). Its widgets:
`radio:lightroomDestination-open`, `radio:lightroomDestination-new`, `field:lightroomLibraryName`,
`field:lightroomLibraryLocation`, `button:lightroomLibraryChoose` (where there is a folder dialog), `check:lightroomDontAsk` (with Merge only; sets
`"alwaysMerge"`), `button:dialogOk` / `ui.dialog.confirm` (which stays open on an error such as a folder that isn't
empty) and `button:dialogCancel`. With `"alwaysMerge"` (`ui.set {"settings": {"lightroomImport": "alwaysMerge"}}`,
or Settings ▸ General `button:settingsLightroomImport-0|1`), or in a session in memory, it imports into the open
library without asking.

## External controllers

External apps can use this channel to send commands and poll the running desktop app for feedback
(e.g. knob values or LEDs). Start the app with `--control PORT`, then keep a JSON-lines connection to
`127.0.0.1:PORT`. Discover command ids, parameters and current enablement with `engine.commands`;
use control ids and ranges from `develop.controls` rather than translated labels or widget positions.

| Task | Command through `engine.execute` | Result / effect |
|---|---|---|
| Read selection and active photo | `library.state {}` | `selection: {ids, active}`; `active` is `null` when no photo is active |
| Read photo identity and settings together | `photo.inspect {}` | Active photo's `id`, `develop`, rating, flag and history in one reply |
| Read a specific photo without selecting it | `photo.inspect {id}` / `develop.get {id}` | Photo details / full develop JSON; omitting `id` reads the active photo |
| Read global slider metadata and values | `develop.controls {section?}` | Array of `{id, label, section, min, max, default, step, decimals, track, value}` for the active photo |
| Set absolute slider values | `develop.set {values: {"light.exposure": 0.7}}` | Undoable edit of the active photo; also accepts `{control, value}` |
| Move a relative control | `develop.adjust {control: "light.exposure", delta: 0.1}` | Adds to the current value, clamped to the control's range |
| Change photo | `library.select {ids: [photoId]}` / `library.next {}` / `library.previous {}` | Changes the active photo; next/previous follow the current view's order |
| Undo / redo | `edit.undo {}` / `edit.redo {}` | Restore settings; poll again for feedback |

With Auto Sync enabled, `develop.set` and `develop.adjust` can also change the other selected photos.
Only changed settings propagate; spot removal, red eye and the rendering process stay per photo.
During an interaction, propagation occurs when it ends. Re-read affected photos after the edit.
`develop.autoSync {}` toggles the mode; it is not a state query.

For example, send these requests one at a time and read each reply before sending the next:

```json
{"id":1,"method":"engine.commands"}
{"id":2,"method":"engine.execute","params":{"command":"photo.inspect"}}
{"id":3,"method":"engine.execute","params":{"command":"develop.controls","params":{"section":"light"}}}
{"id":4,"method":"engine.execute","params":{"command":"develop.set","params":{"values":{"light.exposure":0.7}}}}
{"id":5,"method":"engine.execute","params":{"command":"photo.inspect"}}
```

Replies use `{id, ok, result}` or `{id, ok: false, error}`. In the photo snapshot, exposure is
`result.develop.light.exposure`; the controls array's matching `light.exposure` entry has `value`,
`min: -5`, `max: 5` and `step: 0.01`. `wb.temp` is Kelvin on raws with colour matrices and relative
(6500 = as shot) elsewhere, as described above. Re-read values after edits to reflect the stored result.
Local mask adjustments are in the photo snapshot's `develop.masks`; use the mask commands from the
registry (e.g. `mask.adjust`) for those, rather than treating global control values as mask values.

Feedback uses polling: the channel sends replies to requests, with no unsolicited change events.
Compare successive `photo.inspect` snapshots for photo-id or develop-setting changes, including GUI
edits, photo navigation, presets and undo/redo. GUI slider updates are visible while a drag is in
progress; ending a changed interaction commits it as one undo step (unchanged drags add none).
A controller can group `develop.set` / `develop.adjust` updates between
`develop.beginInteraction {label}` and `develop.endInteraction {}`. The GUI and controllers share
one interaction: another `develop.beginInteraction` or `library.select` ends the current one.
Update controller feedback from reads without sending the same values back as new edits.

Each request is a separate snapshot. `photo.inspect` keeps identity and settings together; a
`develop.controls` reply has no photo id, so do not associate it with an earlier cached selection
when the user is changing photos. Refresh the selection and retry those reads after navigation.
With no active photo, `develop.controls` returns default values and `photo.inspect` / `develop.get`
without `id` return an error; check `library.state` before treating defaults as photo feedback.
Photo ids are local to the current library/session. After reconnecting or opening another library,
discard cached photo mappings and re-read state; matching photo ids alone do not identify the library.
For persistent libraries, poll `library.info` for the current `path` as well. Polling observes the state
at each read, not every intermediate change.

The same queries and edits are available through the connected CLI:

```sh
lightcraft-cli run --connect 127.0.0.1:7980 library.state photo.inspect develop.controls section=light
lightcraft-cli run --connect 127.0.0.1:7980 develop.adjust control=light.exposure delta=0.1 photo.inspect
```

For MCP, use `lightcraft-cli mcp --connect 127.0.0.1:7980`: `run_command` reaches the commands above,
`list_controls` / `set_develop` provide slider helpers, and `resources/read` polls
`lightcraft://library`, `lightcraft://photo/active`, `lightcraft://develop/active` and
`lightcraft://controls` ([mcp.md](mcp.md#resources)). Headless CLI/MCP mode owns a separate engine
session; use connect mode for feedback from edits in the desktop app.

The import review's **Thumbnail size** slider is `slider:importThumbSize` in `ui.widgets`.
`engine.execute {command: "view.importThumbSize", params: {size: 240}}` sets its size in points
(80–320, clamped); omitting `size` reads it. Both return `{size}`. Non-numeric or non-finite
values are refused. The preference is `importThumbSize` in the UI state, independent of the
library grid's `thumbSize`; resizing keeps the import checkboxes and duplicate markers.
The shared slider also exposes `sliderValue:importThumbSize` for exact entry and
`sliderLabel:importThumbSize` for double-click reset to 116 pt. Up/Down nudges the selected
slider by 1 pt; Return commits typed values and Escape cancels them. Slider and command
labels, including edit/reset feedback, are localized in every shipped catalog. Previews use bounded physical-pixel buckets through 1024 px. A smaller source is tried once per bucket; scrolling releases offscreen textures, queued work and request identities. A renderable raw uses a render when its camera JPEG cannot cover the tile; preview-only raws, unavailable renders and failed renders retain the available camera JPEG.

## Headless rendering (no window, no GPU)

The egui UI can be rasterized on the CPU (`crates/ui-egui/src/softpaint.rs`, driven by
`crates/ui-egui/src/headless.rs`): same tessellation, gamma-space premultiplied blending and
scissor clipping as egui's GPU backends, so the image matches the window (minus GPU dithering).

- **In the running app:** `{"method": "ui.screenshot", "params": {"path": "a.png", "headless": true}}`.
  The UI is drawn into an offscreen context from the app's logic tick, which keeps running when the
  window is occluded or the display sleeps/locks. Windowed screenshots fall back to this after 2 s.
- **Without any app window:** `lightcraft-cli snapshot` runs a whole app session headlessly.
  Photo development also defaults to the CPU, so the initial frames do not discover a GPU adapter
  or load a graphics driver. `LIGHTCRAFT_GPU=1` does not opt snapshots back into GPU rendering.
  This applies only to the snapshot session: even `--library DIR` leaves the desktop app's saved
  `ui.json` preferences untouched. It answers the same control requests (same handler) from a JSON-lines script:

```text
lightcraft-cli snapshot --demo -o grid.png --size 1600x1000 [--scale 2]
lightcraft-cli snapshot --library DIR --script tour.jsonl -o shot.png
```

Headless screenshot dimensions must be finite and at least one logical point, with a finite,
positive scale. Rounded output must be at least one pixel per edge, at most 16,384 pixels
per edge and 33,554,432 pixels in total (128 MiB of RGBA, the per-image screenshot
budget). `snapshot` and `ui.resize` reject requests outside these limits before layout or
rasterization; supplied resize dimensions must be numbers.
An interface zoom (egui's zoom factor; the keyboard never changes it — ⌘= / ⌘− / ⌘0 zoom the
photo, issue #566) changes text and control sizes while preserving the snapshot viewport's requested
physical pixel dimensions. Later `ui.resize` requests use current egui points; validation
and rasterization both convert them to native viewport points before rounding the output
pixels. A native edge below one point after zooming out is valid if it rounds to at least
one pixel.

  `tour.jsonl` holds one request per line (`#` comments allowed), e.g.
  `{"method": "ui.set", "params": {"view": "detail", "right": "edit", "openSections": ["optics"]}}`
  then `{"method": "ui.screenshot"}`. Replies are printed to stdout. A failed request
  (`"ok": false`) does not stop the script — the remaining lines and the final screenshot still
  run — but the exit status is non-zero when any request failed, as with `run --keep-going`, so
  CI and nightly runs can judge a snapshot by its exit status. A `ui.screenshot` without
  `path` writes `-o` (then `OUT-2.png`, `OUT-3.png`, …); `ui.settle {timeoutMs?}` waits until no
  renders are in flight. Each request runs frames until it is answered and its injected input
  (clicks, keys, drags) has played out. Widget ids for `ui.clickWidget` come from `ui.widgets`
  (e.g. `button:upright-auto`). A 1600×1000 demo snapshot takes ~1–2 s (debug build).

## Import album picker

In the Import Photos review, `albumPicker:import-album` opens the destination picker.
`albumPickerFolder:<folder-id>:import-album` expands or folds a folder; it never chooses a destination.
`albumPickerItem:<album-id>:import-album` selects a regular album. The picker follows sidebar order,
opens the current choice's ancestors and shows its full path on the closed button. Smart albums
are not import destinations. Type into `albumPickerSearch:import-album` to search folder/album paths;
↑ / ↓ and Enter pick a search result, and Escape closes the picker while keeping the review open.
If the chosen album is deleted while the review is open, the destination returns to None before
confirmation instead of submitting the deleted ID.

`importAlbum:none` clears the destination and any pending new album name. `importAlbum:new` keeps
or initializes that name, edited through `field:importAlbumName`. Choosing an existing album clears
the pending name. `ui.dialog.confirm` starts the same import into the selected album ID; engine
`library.import` parameters `album` and `albumName` are unchanged.

## Panel docking

Library, Tools, and Presets panels can be regrouped, split, floated within the app window, closed, and reopened. Tools keeps the current Edit/Crop/Masking mode; the photo view and filmstrip stay together. The document/session view is protected. Layouts, active tabs, panel visibility, and floating rectangles are saved with the app's existing UI preferences as a craft-ui docking workspace (`docking`: `version`, `layout`, `hidden`, `saved`, `locked`); settings written before workspaces had a `version` load as version 1. These operations do not edit document contents.

Panel IDs: `library`, `tools`, `presets`. The protected center is `canvas`.

`ui.dock` accepts `operation` and `panel`. Operations are `open`, `close`, `activate`, `float` (with finite positive `rect: [x, y, width, height]`), or `move` (with `target` and `zone: center|tab|left|right|top|bottom|above|below`; `Center`, `Left`, `Right`, `Top` and `Bottom` also parse, while `tab`, `above` and `below` are lowercase only). Center and tab group with the target; side zones split beside it; above and below put the panel in a new group above or below the target's group. `ui.dock.reset` restores the default arrangement. Invalid moves return an error without changing the layout. Both commands return `{"docking": workspace}`.

Invoke these UI commands through `engine.execute`, for example `{"command":"ui.dock","params":{"operation":"float","panel":"tools","rect":[100,100,320,440]}}`.

Advanced `ui.dock` operations use the same validated transaction as pointer gestures:
`moveFloating` updates `rect`, `resizeSplit` accepts a boolean child `path` and
`size` (`{"Ratio":0.5}`, `{"FixedFirst":280}`, or `{"FixedSecond":280}`),
`setStackOpen` takes `open`, and `resizeStack` takes a point `height` or null.
A center `move` can include `before` (panel ID or null to append) for tab order.
The equivalent `{"action": ...}` form accepts a serialized shared docking action;
this is also the form emitted by the renderer. Protected-canvas and app-specific panel
rules apply equally to both forms.
