# Long-running tasks and the activity stack

Everything that takes a while runs off the UI thread and shows up in **one** place: the bell at the left of the
top bar, after back / forward (issue #345). Nothing floats over the photos or the sidebar while a task runs: a ring around the bell shows
how far the tasks have got (a segment sweeps while the amount of work isn't known) and, where the bar has room, the
task's name and count sit beside it. Clicking the bell (or `view.activity`) opens a popover with a row per task — its
name, a progress bar, the count and what it's working on, and ✕ if it can stop — and the recent notifications below.
Quitting asks first while a task that can stop is running. Agents see the same list through `activity.list` /
`activity.cancel`, and `ui.inspect` reports it as `activity` (`activityOpen`: the popover is open).

## Notifications

How a job ended is a toast at the bottom of the canvas, a few lines at most. `LightcraftApp::notify` also keeps it in
the bell's Recent list (`crate::notifications`; errors from `toast_error` go there too) and puts a dot on the bell
until the popover is opened. A result too long for a toast (every photo an import warned about) goes in a `Report`:
the toast and the list entry then have a **Show Details** link that opens it as a dialog (`Dialog::Report`, grouped,
with Copy to Clipboard). Never put a list in the toast itself. `ui.inspect` → `notifications` lists them, newest
first (`details`: has a report; ids `toast:details`, `notification:details:<id>`).

The registry is `lightcraft_engine::activity` (`crates/engine/src/activity.rs`, `Session::activity`). The bell and its popover are
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
| Reading focus points | `crates/engine/src/focus_scan.rs` | a worker paced by the UI's per-frame pump (`focus.pump {pace}`); `Cancel::Flag`; commits in batches without undo steps |

## Testing

Assert the row (`session.activity.list()`: kind, label, total, `cancellable`), that `activity.cancel(id)` stops the
job, and that the row is gone once the job ends. `crates/ui-egui/src/tests_activity.rs` has the helpers, and the
job-specific tests show the pattern (`tests_sync.rs`, `export_task.rs`, `tests_denoise.rs`…). A row only appears
in the popover once its task is 0.5 s old and the popover is open (`view.activity {"open": true}` or
`panels::activity::set_open`), so tests that click its ✕ open it and wait for `activity:cancel:<id>` first.
