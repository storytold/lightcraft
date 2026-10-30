# Long-running tasks and the activity stack

Everything that takes a while runs off the UI thread and shows up in **one** place: the activity stack in the
top-left corner under the top bar (issue #345). Each task has a row with its name, a progress bar (a sweep while
the amount of work isn't known), the count and what it's working on, and ✕ if it can stop. Quitting asks first while
a task that can stop is running. Agents see the same list through `activity.list` / `activity.cancel`, and
`ui.inspect` reports it as `activity`. A task stuck on something that doesn't answer (an import's file on a network
drive that stopped responding) names it on its row and offers Skip (`activity.skip`).

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
- **Something that may never answer** (a file on a network drive that dropped its connection): `guard.waiting_on(name,
  waited)` puts it in the row's `stalled` (`{what, secs}`), which the stack words as "Waiting for IMG_0457.jpg · 14 s"
  in the caution colour and answers with a Skip button next to ✕ (`activity:skip:<id>`, `activity.skip {id}`).
  `guard.clear_waiting()` when it answers; `guard.take_skip()` says whether Skip was asked for (once). What skipping
  does is the job's: an import fails that file ("skipped: no answer after N s"), a scan lists it as unreadable, and
  the work goes on. A blocked read can't be interrupted, so such a job reads on threads it can abandon, and ✕ must not
  wait for them. `crates/engine/src/import/reads.rs` is that pool: it reports a read as stalled after 10 s *without
  progress* (a chunk read or written; code that works long without touching the disk calls `import::progressed()`), a
  skip gives up only the read the row names, and an abandoned read keeps its memory permit
  (`memory::work_gate`) until the drive answers, so later large jobs can wait for it. The fourth skipped read of a scan
  or import ends it with an error (and so does a run that starts while 32 or more abandoned reads, of any drive, are still blocked in the system: `StuckReaders` (`Session::stuck_readers`, the process-wide count by default) counts them across cancel and restart and is looked at when a run starts and after each Skip, so repeated ✕ and rescans cannot pile up blocked threads; ✕ can still add the readers of the runs under way. The error says so rather than naming the drive being read, and goes when the system lets the reads go or LightCraft restarts): an import says so in its toast (`Prepared::stopped`), a scan in an error toast (`ScanOutput::stopped`,
  `FolderChanges::stopped`), and a Synchronize run stops there and reports it (`SyncReport::stopped`). A run made from a scan that stopped imports the new files the scan listed, then stops: no relink, remove or sidecar step (what the scan lists is only what it reached), and the dialog disables those choices. A wait for memory at
  the gate counts as progress (the gate reports once a second) only while a holder that was not given up is working: a
  wait blocked only by abandoned reads, which keep their memory (`WorkGate::abandoned_bytes`), is named and can be
  skipped too. `ScanProgress::reads` / `SyncProgress::files.reads` / `ImportJob::reads`
  are the `ReadWatch` a frontend mirrors onto the row each frame (`panels::activity::follow_reads`). Not covered:
  listing folders (the walk), a few path checks before a copy, and Synchronize Folder's sidecar reads and file checks
  after the scan.
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
| Import reads that stop answering | `crates/engine/src/import/reads.rs`, `crates/ui-egui/src/dialogs/import/task.rs` | publish what the job waits for each frame (`waiting_on`, `clear_waiting`) and take Skip (`take_skip`); the reads themselves run on detached threads so that a blocked one never holds the job |

## Testing

Assert the row (`session.activity.list()`: kind, label, total, `cancellable`), that `activity.cancel(id)` stops the
job, and that the row is gone once the job ends. `crates/ui-egui/src/tests/activity.rs` has the helpers, and the
job-specific tests show the pattern (`tests_sync.rs`, `export_task.rs`, `tests_denoise.rs`…). A row only appears
on screen once its task is 0.5 s old, so tests that click its ✕ wait for `activity:cancel:<id>` first.
