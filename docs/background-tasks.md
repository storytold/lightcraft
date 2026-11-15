# Long-running tasks and the activity stack

Everything that takes a while runs off the UI thread and shows up in **one** place, the activity stack (issue #345).
Each task has a row with its name, a progress bar (a sweep while the amount of work isn't known), the count and what
it's working on, and ✕ if it can stop. A task shows once it is half a second old. Quitting asks first while a task
that can stop is running. Agents see the same list through `activity.list` / `activity.cancel`, and `ui.inspect`
reports it as `activity`.

Where the tasks show depends on the layout:

- **Classic**, as Lightroom Classic's activity centre: in the module bar, in the identity plate's place, while tasks
  run. One task shows its name, its bar and ✕ (`activity:cancel:<id>`); several show "N operations in progress…" over
  a thin bar each (`activity:bar:<id>`), oldest first, as many as fit in the bar, with no ✕. A batch is one task: its
  per-photo rows add no bar. A click on the area (`activity:plate`) opens the list of every task under it
  (`activity:list`, rows `activity:row:<id>` with their ✕), as does `view.activityList {open?}`; `ui.inspect` →
  `activityList` says whether it is open. Esc, a click outside or the last task ending closes it. The plate and the
  status line come back once nothing runs. With the module bar hidden, or Edit ▸ Identity Plate Setup's Show Status
  and Activity off, no activity shows: the tasks run on, in `activity.list`, and Quit still asks.
- **Compact** (`view.compactLayout`) is Classic drawn denser, with the same module bar: the tasks show as in Classic,
  and the list stays open across a switch between the two. The list's frame takes Compact's sharper corners
  (`Tokens::radius`).
- **Modern**: a floating card centred under the Library / Develop switch, three rows and "+N more"
  (`activity:more`).
- **Full-screen preview** (`F`): a quiet card top right over the black, one task's name over a thin bar, several as
  "N operations in progress…" over a bar each (`activity:fullscreen`, bars `activity:fullscreen:bar:<id>`), never during a
  slideshow. The same stack's data drawn small — not a second progress system.

The registry is `lcc_engine::activity` (`crates/engine/src/activity.rs`, `Session::activity`). The stack (every layout) is
`crates/ui-egui/src/panels/activity.rs`; the module bar calls it from `crates/ui-egui/src/module.rs`. Every job that can last more than a moment uses it; **don't add a progress
window, panel or progress toast of your own**. A dialog may still show progress inline (the Synchronize Folder
dialog counts files while it scans), and a toast at the end saying what happened is fine. A view saying its pixels
aren't final isn't progress either: the loupe's "Rendering…" pill (`loupe:rendering`) marks a stand-in or an
out-of-date render on the photo itself, and the render behind it holds no row (a slider drag replaces it many times a
second).

## Adding a task

```rust
use lcc_engine::activity::{Cancel, Unit};

// when the job starts, with the cancel flag the worker already checks between files
let guard = app.session.activity.start("export", "Exporting", Cancel::Flag(cancel.clone()));
guard.progress(0, total as u64);

// every frame (or from the worker, through `guard.handle()`)
guard.progress(done as u64, total as u64);
guard.detail(&file_name);
```

- **Keep the guard for exactly as long as the job runs.** Put it in the struct the UI polls (`ExportTask`, `ScanTask`,
  `SyncRun`…), or move it into the worker thread. Dropping it removes the row, also when the worker panics, so a
  dead job never leaves a stuck bar. A worker that only reports progress takes a `TaskHandle` (`guard.handle()`,
  `Clone + Send`).
- **`kind`** is a stable camelCase id that agents filter by (`export`, `import`, `scan`, `sync`, `previews`,
  `download`, `faces`, `cache`, `focus`, `fill`, `sidecars`, `saveMetadata`, `readMetadata`, `mlIndex`…). **`label`** is a short English name in sentence case ("Building previews"). The stack shows it
  translated, so add it to every catalog in `crates/ui-egui/locales/`.
- **Progress:** `progress(done, total)`. A `total` of 0 means the amount of work isn't known yet, and the bar sweeps.
  `set_unit(Unit::Bytes)` makes the count read "12 of 340 MB"; `Unit::Percent` reads "40 %". The default `Unit::Count`
  reads "3 of 25".
- **Detail:** what the job is on (a file name, a model's name). Detail set by engine code is a name, never a sentence,
  unless it is a translatable text with names (below). UI code may set translated text.
- **Names in the text:** a label or detail that names something no catalog translates (a program, a plug-in) says
  `{key}` where the name goes, and `guard.name(key, value)` gives the name: "Waiting for {program} to finish
  ({plugin})". `activity.list` shows the English with the names in; the app translates the text as written and puts
  the names in after (`TaskInfo::templates`, `activity::fill`). Add the text with its `{key}`s to every catalog.
  Never `format!` a name into a label: the label then matches no catalog entry and is never translated.
  Once a task has names, the app translates its detail too, so give it a text with `{key}`s, not a bare value such as
  a file name. Batch end summaries don't carry names yet: a batch with names gets an untranslated summary.
- **Cancel:**
  - `Cancel::Flag(Arc<AtomicBool>)` adopts the flag the job already checks. The row's ✕, `activity.cancel` and the
    job's own Cancel then all set the same atomic.
  - `Cancel::Yes`: the job polls `guard.is_cancelled()` itself.
  - `Cancel::No`: the job can't stop; it gets no ✕ and doesn't make Quit ask.
  - `set_cancellable(false)` for a phase that has to finish once started (a Lightroom import adding its photos).
  - **A stop that throws work away:** `guard.warn_on_stop(text)` for a task that waits on something outside the app
    and loses what it would still bring back when stopped (a plug-in's wait for a program: what the program makes
    afterwards never comes back). The row then has no ✕ anywhere; the Classic list and the Modern card give it
    "Stop Waiting…", which shows `text` (translated, names in) and asks first. `activity.list` reports it as
    `stopWarning`, and `activity.cancel` and Quit Anyway still stop it at once. A job that stops between files
    loses nothing already done and needs none.
- **Pause and settings:** a continuous job the user may want to hold (the face scan, search indexing) calls
  `guard.set_controls(Some(Controls { paused, pause, resume, settings_tab, settings_section }))`: the engine commands
  that pause and resume it, and the Settings page with its options. Its row then has a pause button (resume while
  `paused`) and a gear (`activity:pause:<id>` / `activity:resume:<id>`, `activity:settings:<id>`); `activity.list`
  reports them as `controls` and `activity.pause {id, paused?}` runs the same commands. Keep the row while paused, with
  "Paused" as its detail, so resume is there.
- **Quiet periodic work** shows no row: the auto-import folder listing runs every few seconds through
  `tasks::spawn(.., None, ..)`.

## Batches: one row for a job over many photos

A job that works through many photos (AI Denoise, export) shows **one** row for the whole batch: "AI Denoise · 37 of 200 · about 6 min left · 2 failed".

```rust
let batch = session.activity.start_batch("denoise", "AI Denoise", Cancel::Yes);
batch.progress(0, photos as u64);        // the total; `item_done` keeps counting past a short one
batch.item_done();                       // a photo finished
batch.item_failed("IMG_2.CR3: why");     // a photo failed (counted in done too; the reason is kept)
let photo = batch.child("denoise", "AI Denoise"); // the photo in progress, under the batch
```

- **Counts:** `item_done` / `item_failed` count one photo each. A job that sets `progress(done, total)` itself counts
  a failure with `count_failed(why)` (it doesn't add to `done`).
- **Time left:** `etaMs` comes from the time per photo measured since the first one was done (a slow start, such as
  loading a model, doesn't stretch it). The stack words it ("about 6 min left").
- **Per-photo rows:** `child` (or `Activity::start_child(parent_id, ..)` from a worker that only has the id) puts a
  row under the batch. The stack folds them away ("Show each photo (3)"). A child of `child` shares the batch's cancel
  flag: ✕ on the batch stops them too.
- **The end:** dropping the batch's guard leaves a `BatchSummary` (`activity.list` → `finished`): done, failed, the
  first reasons, cancelled. The desktop app says it once ("AI Denoise: 198 done, 2 failed: IMG_2.CR3: why"), unless
  the job reports itself (`reports_itself()`: an export's own message).

## Examples in the code

| Job | Where | Shows how to |
|---|---|---|
| Export, contact sheet | `crates/ui-egui/src/export_task.rs` | adopt the job's flag; update the row each frame; an export is a batch that reports itself, its failures counted when it ends |
| Build Previews | `crates/engine/src/cmd/previews.rs` | move the guard into the worker thread; a build asked for while another runs waits in `Session::preview_queue` and the next one starts (a fresh row) when the running build ends — `library.cancelPreviews` stops the running one and drops the waiting ones |
| Preparing photos in the background | `crates/engine/src/prep.rs`, `crates/engine/src/cmd/previews.rs` | a background build in one row ("Preparing photos in the background") whose ✕ sets the build's flag; it waits while the photo on screen is being made and says "Paused while exporting" in its detail while the user's own work runs (`Activity::user_work`); a build asked for replaces it |
| Slideshow export | `crates/engine/src/cmd/slideshow.rs`, `crates/engine/src/slideshow_export.rs` | an engine command that owns its worker and row; progress from the worker through a `TaskHandle`; results read back by command |
| Folder metadata save / read | `crates/engine/src/cmd/folder_metadata.rs` | a job that needs the session, so a per-frame pump (`folder.metadataPump`) does a few photos at a time and the row's flag stops it |
| Model downloads | `crates/engine/src/face_download.rs` | report progress through a `TaskHandle` in shared state |
| Lightroom catalog import | `crates/ui-egui/src/lightroom_import.rs` | stop being cancellable for a phase that must finish |
| Face scan | `crates/engine/src/cmd/face_recognize.rs` | a continuous job with no ✕, kept in session state; pause / resume and a gear (`set_controls`), the row kept while paused |
| Filling spots | `crates/ui-egui/src/render.rs` (`Renderer::track_fills`) | a row held while a render that solves a content-aware fill is out, with no ✕ (`Cancel::No`); solved fills are cached, so it shows for a new fill only |
| AI Denoise | `crates/engine/src/denoise.rs` (`denoise_track`) | a continuous queue shown as a batch while busy (photos counted since it was last idle), the running photo's tiles as its child; ✕ empties the queue and holds those photos until asked for again |
| Find Missing Photos | `crates/ui-egui/src/tasks.rs` | a generic background task with a row (`kind: Some(..)`) |
| Background XMP sidecar writes | `crates/engine/src/sidecar_writer.rs` | one row per batch from a worker thread, no ✕; the stack's 0.5 s delay keeps a quick write after an edit from showing |
| Reload from Disk | `crates/ui-egui/src/reload.rs`, `crates/engine/src/cmd/reload.rs` | a command split into plan / probe / apply so the app reads the files on a worker thread (`tasks::spawn_tracked`: progress and `Cancel::Flag`); the result is committed through `Session::execute_fn` (undo step, save, XMP auto-write), and dropped for a photo a later-started reload has already updated; `wait: true` runs it in place |
| Automatic preview-only RAW recheck | `crates/ui-egui/src/recheck.rs`, `crates/engine/src/cmd/recheck.rs` | the app-version recheck uses `tasks::spawn_tracked`, reports completed files out of the known total, and checks a shared `Cancel::Flag` between files; completed successful probes may be applied while failed or unstarted files remain unchanged and stale; cancellation is cooperative: a file already being read may finish, and the once-per-library guard keeps a cancelled recheck from restarting every frame |
| Reading focus points | `crates/engine/src/focus_scan.rs` | a worker paced by the UI's per-frame pump (`focus.pump {pace}`); `Cancel::Flag`; commits in batches without undo steps |
| AI jobs (models) | `crates/ml/src/queue.rs`, `crates/engine/src/ml.rs` | a queue in a crate below the engine that holds its row through a small trait (`lcc_ml::Rows`); the worker makes the batch row when work arrives and drops it when idle; the row's ✕ is the batch's `Cancel::Flag` (`docs/ml-backend.md`) |
| Camera sensor maps, Remove Dust on a selection | `crates/engine/src/cmd/sensor.rs` | one job in session state; results applied on the UI thread by a pump (`dust.pump`) |

## Testing

Assert the row (`session.activity.list()`: kind, label, total, `cancellable`), that `activity.cancel(id)` stops the
job, and that the row is gone once the job ends. `crates/ui-egui/src/tests_activity.rs` has the helpers, and the
job-specific tests show the pattern (`tests_sync.rs`, `export_task.rs`, `tests_denoise.rs`…). A row only appears
on screen once its task is 0.5 s old, so tests that click its ✕ wait for `activity:cancel:<id>` first.
