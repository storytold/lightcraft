# Long-running tasks and the activity stack

Everything that takes a while runs off the UI thread and shows up in **one** place: the activity stack, the list
under the bell in the top bar (issue #345). While something runs the bell wears a ring (how far along the work is, a
sweep while that isn't known) and a line beside it names the task ("Exporting · 40 %") or says how many run
("3 tasks"). The line takes the room between the filter icon and the bell, so it depends on the window's width: a
wide window has the name (cut short where it must be) and the percentage, a narrower one only the percentage or the
count, and a narrow one the ring alone, with the count on the bell when several run and the rest in the bell's
tooltip. A click on the bell or the line opens
the list (Window ▸ Tasks, `view.tasks`): each task has a row with its name, a progress bar, the count and what it's
working on, and ✕ if it can stop, the task started last at the top. "No tasks are running" when none is. Esc, a click
elsewhere or a dialog closes it. Nothing floats over the panels: the list covers them only while it is open.
Quitting asks first while a task that can stop is running. Agents see the same list through `activity.list` /
`activity.cancel`, and `ui.inspect` reports it as `activity` (and whether the list is open as `activityOpen`).

The registry is `lightcraft_engine::activity` (`crates/engine/src/activity.rs`, `Session::activity`). The stack is
`crates/ui-egui/src/panels/activity.rs`. Every job that can last more than a moment uses it; **don't add a progress
window, panel or progress toast of your own**. A dialog may still show progress inline (the Synchronize Folder
dialog counts files while it scans), and a toast at the end saying what happened is fine. A view saying its pixels
aren't final isn't progress either: the loupe's "Rendering…" pill (`loupe:rendering`) marks a stand-in or an
out-of-date render on the photo itself, and the render behind it holds no row (a slider drag replaces it many times a
second).

## Adding a task

```rust
use lightcraft_engine::activity::{Cancel, Unit};

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
  `download`, `faces`, `cache`, `focus`…). **`label`** is a short English name in sentence case ("Building previews"). The stack shows it
  translated, so add it to every catalog in `crates/ui-egui/locales/`.
- **Progress:** `progress(done, total)`. A `total` of 0 means the amount of work isn't known yet, and the bar sweeps.
  `set_unit(Unit::Bytes)` makes the count read "12 of 340 MB"; `Unit::Percent` reads "40 %". The default `Unit::Count`
  reads "3 of 25".
- **Detail:** what the job is on (a file name, a model's name). Detail set by engine code is a name, never a sentence.
  UI code may set translated text.
- **Cancel:**
  - `Cancel::Flag(Arc<AtomicBool>)` adopts the flag the job already checks. The row's ✕, `activity.cancel` and the
    job's own Cancel then all set the same atomic.
  - `Cancel::Yes`: the job polls `guard.is_cancelled()` itself.
  - `Cancel::No`: the job can't stop; it gets no ✕ and doesn't make Quit ask.
  - `set_cancellable(false)` for a phase that has to finish once started (a Lightroom import adding its photos).
- **Quiet periodic work** shows no row: the auto-import folder listing runs every few seconds through
  `tasks::spawn(.., None, ..)`.

## Examples in the code

| Job | Where | Shows how to |
|---|---|---|
| Export, contact sheet | `crates/ui-egui/src/export_task.rs` | adopt the job's flag; update the row each frame |
| Build Previews | `crates/engine/src/cmd/previews.rs` | move the guard into the worker thread |
| Slideshow export | `crates/engine/src/cmd/slideshow.rs`, `crates/engine/src/slideshow_export.rs` | an engine command that owns its worker and row; progress from the worker through a `TaskHandle`; results read back by command |
| Model downloads | `crates/engine/src/face_download.rs` | report progress through a `TaskHandle` in shared state |
| Lightroom catalog import | `crates/ui-egui/src/lightroom_import.rs` | stop being cancellable for a phase that must finish |
| Face scan, AI Denoise | `crates/engine/src/cmd/face_recognize.rs`, `crates/engine/src/denoise.rs` | a continuous job with no ✕, kept in session state |
| Find Missing Photos | `crates/ui-egui/src/tasks.rs` | a generic background task with a row (`kind: Some(..)`) |
| Reload from Disk | `crates/ui-egui/src/reload.rs`, `crates/engine/src/cmd/reload.rs` | a command split into plan / probe / apply so the app reads the files on a worker thread (`tasks::spawn_tracked`: progress and `Cancel::Flag`); the result is committed through `Session::execute_fn` (undo step, save, XMP auto-write), and dropped for a photo a later-started reload has already updated; `wait: true` runs it in place |
| Reading focus points | `crates/engine/src/focus_scan.rs` | a worker paced by the UI's per-frame pump (`focus.pump {pace}`); `Cancel::Flag`; commits in batches without undo steps |

## Testing

Assert the row (`session.activity.list()`: kind, label, total, `cancellable`), that `activity.cancel(id)` stops the
job, and that the row is gone once the job ends. `crates/ui-egui/src/tests_activity.rs` has the helpers, and the
job-specific tests show the pattern (`tests_sync.rs`, `export_task.rs`, `tests_denoise.rs`…). The bell shows a
task once it is 0.5 s old (`activity:ring`; `activity:readout` is the line beside it, `activity:count` the count on
the bell), and its row is on screen only while the list is open: tests that click a row's ✕ open the list first
(`view.tasks` with `{"show": true}`, or a click on `icon:bell`) and wait for `activity:cancel:<id>`.
