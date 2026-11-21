# Optional web library providers

This integration is disabled by default. The web app publishes window.lightcraftFileProviders and lightcraft-file-provider events (protocolVersion 1), but no provider is registered. Desktop and CLI do not install the browser API. Catalog autosave and desktop/CLI backup, restore and export routes remain available. The browser-local Restore safety corrections below also apply without a provider.

LightCraft edits a library rather than individual project documents. Its provider resource is a complete native library ZIP backup: catalog snapshot/journal, presets, view state, preferences and the original photographs referenced by that captured catalog. It is the same archive layout as File > Back Up Library, not a new private document format.

## Window event payload

The page dispatches a `CustomEvent` named `lightcraft-file-provider` on `window`. Its payload is
`event.detail`, an object; `event.type` is always the event name. Hosts select the operation using
`event.detail.type`, and read captured save data from `event.detail.bytes`, a `Uint8Array` containing
the complete native library ZIP. Those bytes are not a JSON string, Blob, URL or live document view.
They are present only on `save`; metadata-only events omit `bytes`.

| `event.detail.type` | Payload fields |
|---|---|
| `ready` | `protocolVersion: 1`; the API is installed, with no provider registered automatically. |
| `saveDialog` | `requestId`, `documentId`, numeric `revision`, `name`, boolean `saveAs`, and `resourceId` (string or null). |
| `save` | `requestId`, `documentId`, numeric `revision`, `name`, `resourceId` (string or null), `destination: "provider"`, numeric `bytesLength`, and typed `bytes`. |
| `openRequest` | `requestId` and `resourceId`; answer through the matching `openResource` or `cancelOpen` callback. |
| `opened` | `requestId`, `resourceId`, string `documentId` and numeric `revision` of the admitted library. |
| `openFailed` / `saveFailed` | `requestId` and `error`; read failures also carry `resourceId`. |

Provider-owned events also include string `providerId`, `namespace` and `connectionId`. Request,
document and resource IDs are opaque strings. Correlate responses with the exact request and
connection; the presence of save bytes or successful event dispatch is never a durable-save ACK.
The API and ready event retain protocol version 1; this describes the existing payload.

## Registration and Recent libraries

A trusted same-origin host explicitly calls registerProvider(id, label, namespace) and receives a new connectionId. The opaque namespace identifies the signed-in account; it is not a credential. setResources(connectionId, [{id,name,updatedAt}]) supplies up to 100 external libraries. File > Open Recent Library separates Recent — provider label from Recent on this device. Provider IDs are opaque and never become local filesystem paths.

Provider IDs, account namespaces and resource IDs must be nonempty, at most 160 bytes, and contain
only ASCII letters, digits, `_` or `-`. Map email addresses, filesystem paths and other external
identifiers to stable IDs that satisfy this contract; display labels and filenames are separate.

Each resource `name` must be nonblank after trimming, contain no control characters, be neither
`.` nor `..`, and contain no slash or backslash. Its native limit is 255 UTF-8 bytes. Before
native validation, the browser callback also rejects names over 255 UTF-16 code units; passing
that precheck does not relax the UTF-8 byte limit. A single invalid name rejects the whole
`setResources` list, retaining the previous list. Hosts should map incompatible external display
names to stable valid filenames before calling `setResources`.

`updatedAt` is optional. When supplied, it must be a primitive string with no control characters
and at most 100 UTF-8 bytes. The browser also bounds its UTF-16 length before copying it into
WASM. A numeric timestamp, `Date` object, `null` or any invalid value rejects the entire resource
list without replacing the previous list. For example:

```js
api.setResources(connectionId, [{id: "library_1", name: "Library.zip", updatedAt: "2026-11-02T19:00:00Z"}]);
```

requestOpen(resourceId) emits openRequest. The host replies using openResource(requestId, filename, Uint8Array), or cancelOpen(requestId,error). The supplied open filename must end in .zip (case insensitive, so .ZIP is accepted). A valid name without that suffix can pass the callback checks but is rejected later during library admission. ZIPs restore into a fresh browser-storage folder; the old library is retained. CRCs, original content hashes and required catalog originals are checked. The new catalog must load through the engine before its browser pointer changes.

The `cancelOpen` error argument must be a primitive string with no control characters and at
most 1024 UTF-8 bytes. An empty string is accepted. Passing a JavaScript `Error` object rejects
the callback and leaves the read pending. Convert caught errors to an appropriately bounded,
control-free message before calling it, for example `api.cancelOpen(requestId, "Download failed")`.

Dropped-photo reads also block library replacement from scheduling, before their first asynchronous poll, through byte reading and storage. Success keeps the stored original pending until the ordinary import consumer admits it; read failure or cancellation releases the read owner.

A read may be requested while the editor is idle and finish after editing starts. Both the Recent command and byte-delivery admission recheck real text-edit/slider drafts, interactions, photo/control drags, hover previews and the existing dialog/import/export/storage-error guards. Delivery additionally refuses queued or unconsumed editing input and held presses before a UI pass, without treating idle pointer movement, raw mouse motion, idle pointer leave or its own provider read as editing. Motion or pointer leave while a button is held still blocks admission until release or actual focus loss/pointer cancellation. Numeric draft tracking references the widgets' actual resident buffers, including hidden controls. Refusal does not commit/cancel text or gestures, capture UI files, decode bytes, create staging data or change the active-library pointer. The user can commit/cancel the edit and request the library again.

An open waits for the current library's local writes. Imports, unsaved storage failures, exports, dialogs and pending provider saves prevent a replacement. While the new library is restored, editor input and automation processing pause so edits cannot land in the wrong library. Only then is the new library admitted, assigned a fresh instance ID and acknowledged with opened. No page reload or second click is required. Invalid archives keep the old library active.

Library replacement clears the previous People page and its navigation history before restoring saved UI state. A missing or unusable ui.json cannot retain the former library's person navigation.

Local capture and ZIP encoding use the existing activity stack (`providerSave`, translated
"Exporting"); opening uses `providerOpen` (translated "Importing"), including the wait for the
old library's local writes. The ZIP filename is the detail. The guard remains owned until the
application consumes success or failure and is dropped on early refusal or abandoned waiting
work. These operations have no safe interruption point and show no Cancel button. The save row
ends after capture/delivery; it does not claim that the host's later remote write has completed.

## Browser-local Restore and reload

File > Restore Library from Backup stages and verifies the ZIP before selecting its new browser
library folder. Once activation starts, saving in the previous live session stays paused. If the
browser asks about leaving and you stay, or if reloading fails, reload the page to open the selected
library; do not continue editing the previous page expecting its autosave to resume. No timer
re-enables that old writer. The previous library and shared originals are not deleted.

A staging failure leaves the previous saving policy unchanged, including an existing pause caused
by a storage-read failure. An activation-write error can leave its commit uncertain, so saving
remains paused and the page asks for a reload instead of claiming the old pointer is still active.
Provider ZIP admission remains an in-page operation with its existing draft, byte and revision
ownership guards; it does not use this reload-based browser-local Restore flow.

Archive bytes must pass both ZIP integrity and original-content hash verification before they
can repair a corrupted or oversized stored original. A verified same-hash stored copy is kept.
Existing originals are read with a size cap; unrelated backend I/O errors abort restore rather
than being treated as missing or corrupt data. Invalid archive bytes never replace an original.
The complete directory is checked for duplicate destinations and payload totals larger than the
archive before any files are written, preserving the current browser Restore checks.

The natural review boundary is browser-local recovery versus optional provider transport.
`backup::restore_for_reload` and `safety::restore_staged` / `restore_entry` implement the pause,
staging and verified-original repair policy independently of provider registration. The provider
adapter reuses staging, validates the catalog and admits it without reloading. Separating those
recovery fixes into an earlier change would need both the native backup-policy fixtures and the
browser storage fixtures; removing them from the provider path would weaken its failure behavior.
This document describes that boundary without requiring a new storage framework or a second API.

## Save and acknowledgement

Registering a provider adds Save Library (Ctrl/Cmd+S) and Save Library As (Ctrl/Cmd+Shift+S). These entries and their default or customized keyboard shortcuts are unavailable without an enabled provider. Without one, the existing Ctrl/Cmd+S Save Metadata to File and Ctrl/Cmd+Shift+S Create Version defaults remain available. An enabled provider borrows those keys only while its effective bindings use the same keys. Custom mappings and explicit removals keep their existing collision rules. Ordinary local commands and unrelated shortcuts keep their existing behavior. Each Save emits saveDialog with requestId, documentId, revision, name, saveAs and an optional resourceId. The host calls resolveSaveDialog(requestId, provider|device|cancel, filename). A .zip filename is required.

The capture is an immutable atomic snapshot of the six small library files, paired with the originals referenced by that snapshot. An absent, corrupted or non-browser original fails the save. Unrelated originals left in browser storage are never uploaded. Subsequent edits can continue while the archive is prepared and persisted.

The device destination uses the existing browser download writer. The provider destination emits save with destination=provider and native ZIP bytes. Only after a durable write may the host call saveResult(requestId,saved,resourceId). failed, cancel and download never advance the provider checkpoint. Acknowledgement records only the captured file-generation counter; later changes remain dirty. The catalog's independent local autosave is never marked as a successful remote save. Save As does not reuse the old resource ID.

Every saveResult call requires a string resourceId, including non-success results. For `failed`,
`cancel` or `download`, the host may pass an empty string, for example
`saveResult(requestId, "failed", "")`. For `saved`, resourceId must be a nonempty valid ID under
the restrictions above. Any nonempty resourceId must satisfy those same restrictions; `null`
and an omitted argument are rejected. A non-success result never claims persistence.

Recoverable JavaScript payload-copy and event-dispatch errors are logged. If a taken save event
cannot be delivered, its exact captured request becomes a failed result and the application is
scheduled to consume it on another frame. It does not acknowledge persistence or advance the
saved checkpoint. Later events still dispatch. A result already accepted from a synchronous host
listener is retained instead of being replaced by the delivery failure. This recovery covers
returned JavaScript exceptions, not unrecoverable WASM traps or process termination.
A failed openRequest or saveDialog delivery releases only its still-owned read or choice so it can
be retried. A synchronous listener that has already supplied bytes or resolved the destination
keeps that admitted operation; delivery recovery does not cancel the accepted callback.

snapshot() describes the active library, generation, dirty/pending/loading state and separate provider resources. Library metadata refreshes only while a provider is registered and enabled. Disconnect clears the cached document snapshot; disconnected frames cannot repopulate it, and pausing stops refresh until re-enabled. Library IDs are session-local and change on replacement even if catalog photo IDs or generations repeat. Before-unload protection applies to pending provider work and unsaved provider revisions only after explicit registration; publishing the API alone does not change the default page behavior. Each new connection is conservatively protected immediately, before its first library publication. Published dirty/clean state belongs to that exact connection ID: disconnect/reconnect cannot inherit an earlier connection's clean state, even without an intervening publication. Re-registering the same connected identity is idempotent and retains its acknowledged clean state. Pausing with setEnabled(false) retains this protection, including revisions edited while paused. A clean acknowledged revision does not add a prompt, and unregistering restores ordinary unload behavior.

## Account boundaries and limits

unregisterProvider rejects while choices, encoding, reads or acknowledgements are pending, or completion events remain queued for the publisher. This includes both undelivered terminals in the currently taken batch and retained terminals moved into the next batch while the earlier batch dispatches. setEnabled(false) pauses new work while retaining the identity of existing requests. Old resource lists, duplicate completions and replies from earlier page sessions are rejected using bounded IDs, a random page nonce and a checked request counter.

Each transfer, retained snapshot and bounded queue is limited to 80 MiB, 16 pending operations, 64 events and 100 Recent resources. Original files are size-checked before copying from browser storage into WASM. ZIP headers and the final central directory count toward the limit. Filename limits apply separately to host-visible resource names and embedded ZIP paths.

The host owns authentication, per-user authorization, durable writes, conflicts, transport, retries and destination UI. No backend, service URL, cloud widget, token storage or cross-origin messaging is installed here. Browser-local libraries remain local data; the host must define its own shared-device/logout policy. Large libraries and catalogs referring to external desktop paths cannot be uploaded through this bounded browser adapter; import the originals or use the native backup workflow.

The existing embedding bridge documented in [web.md](web.md) still handles individual-photo
Open messages and forwards ordinary download exports/backups to its opted-in page or parent.
It is separate from these complete-library resources and cannot acknowledge a provider save.
Queued embedding work blocks provider admission. During a provider replacement the embedding
bridge refuses new photo/command messages before queueing them, so they cannot land in the
replacement library; accepted photo storage owns its import guard before scheduling.
Related [#3046](https://github.com/storytold/lightcraft/issues/3046) concerns individual-photo
storage; closed, unmerged [#3047](https://github.com/storytold/lightcraft/pull/3047) proposed a
different shared storage integration. Closed [#2060](https://github.com/storytold/lightcraft/pull/2060)
concerned CollabCraft, and the suite direction is discussed in
[craft-libs#123](https://github.com/storytold/craft-libs/issues/123). None is approval of this API.
Maintainer agreement is still needed on the complete-library page-script trust boundary and
how this optional capability relates to the shared storage direction; this PR closes none of them.

Generation checkpoints cover only the six files included in a library snapshot; auxiliary
recovery copies do not make an unchanged backup dirty. A failed restore or post-stage validation
removes only the fresh staging library's private metadata and empty directory. Shared originals
and the active library are preserved; a cleanup error is reported with the original failure.

Terminal `opened`, `openFailed` and `saveFailed` events survive a full event queue. Their
already-admitted read/save owners remain pending until the terminal can be enqueued, with at
most 16 retained read completions and 16 save failures. The publisher drains bounded batches
twice in order; the queue and each returned batch remain limited to 64 events.
Delivery-failure recovery consumes only already-admitted captures. If the ordinary command queue
is full, at most 16 such captures can append their correlated failures before the next logic
drain. This does not admit new operations or relax the public command/result queue checks.

## Validation

Source regression fixtures cover opt-in defaults, save choices, separate resource identities, atomic library snapshots, content generations, overflow handling and archive bounds including the central directory. Execution results are recorded only after validation of the published source candidate.

Native publisher fixtures exercise the same bounded event pump used by the web publisher with
an injected returned delivery error, including a full command queue, normal delivery, continued
event order and a synchronous result. They do not reproduce a browser allocation exception.
Native input fixtures exercise real resident slider drafts and the admission guard directly.
The Host closed-document fixture verifies result correlation and Host state, not the web
application's closed-document binding guard. Neither fixture runs WebApp::provider_tick or
begin_provider_open.

One WASM-only browser fixture, `web::provider::browser_tests::page_local_provider_boundary_preserves_live_draft_and_recovers_failed_dispatch`,
adds source assertions for two integration boundaries. It constructs the real private WebApp
with a real tiny library, Files/Originals and IndexedDB backend, without WebRunner, graphics,
render workers or a large procedural demo. Requests begin while idle; real public slider widgets
then receive egui input and retain invalid numeric drafts, both focused and parked. Delivery follows
an event-empty frame, with no held keys or pointer buttons and settled writes.
Parking hides the slider while retaining its draft; explicit cancellation also releases Escape
before the positive admission control. Host::open queues the actual Command::Open for
WebApp::provider_tick. Correlated window openFailed,
unchanged library/selection/develop state, identical file snapshot/draft and no staging are
required. The guard-deletion mutation must fail the explicit assertion `live draft must keep
provider admission closed`. After explicit draft cancellation, the same valid tiny native ZIP
must load and bind through the real asynchronous admission path.

The same single fixture temporarily makes JavaScript Uint8Array.from throw a deterministic
RangeError while calling the actual WASM catch binding and production publisher. JavaScript
try/finally restores the original Uint8Array.from, console.error and console.warn descriptors even on an
escaping error. The pinned eframe logger sends Error records through console.warn with an ERROR: prefix;
the fixture observes both console channels without changing their forwarding behavior. Assertions
require the real error log, consumption of the exact failed capture,
continued terminal dispatch, unchanged provider checkpoint and actual application consumption
of the correlated failure. A subsequent save must deliver its exact bytes normally and remain
pending until the test host explicitly reports saved. This is an injected JavaScript exception,
not a true out-of-memory experiment or proof of backend durability.

The fixture also declares activity-row ownership checks for synchronous save refusal, successful
capture, a host cancellation after delivery, failed local flush, invalid ZIP and successful library
replacement. It exercises real page-mode embedding events queued before and received during an
open, requiring refusal instead of applying commands to a different library. The task rows remain
noncancellable and disappear after the application consumes their completion; these source
assertions require a fresh real-browser run on the merged candidate.

Validate this fixture with a reviewed lock, WASM compilation, a real browser run and causal
guard-deletion/restoration checks before attributing runtime coverage. Its presence alone does
not establish an execution result.
It does not exercise WebApp::new/eframe startup, DOM keyboard routing or rendering. A saved
result for an older library after another becomes active remains outside this browser fixture;
the closed-document Host test continues to claim only Host correlation/state coverage.

The same browser fixture dispatches synthetic cancelable beforeunload events through the actual
registered window listener. It calls the actual public registerProvider callback and checks unload
immediately before the first library publication, clean idempotent registration and disconnect/reconnect
without an intervening publication. It retains default-off, registered dirty, paused dirty, acknowledged
clean, edits made while paused and disconnected controls without navigating away from the page.
This proves listener cancellation, not the browser's user-activation-dependent navigation dialog.

Two separate WASM-only `safety::tests` storage regressions declare actual File/ZIP and isolated
IndexedDB restore coverage: keeping a verified original, repairing hash-invalid or oversized
copies, propagating a closed-database read error, and rejecting CRC/hash-invalid archive data
while preserving shared originals, the active pointer and failed-stage cleanup. The invalid-archive
fixture also asserts the merged complete-directory duplicate check before any staged write. They do not
exercise OPFS or page navigation. These declarations require real browser execution before
any runtime result can be claimed.

## Native UI preview

The existing offscreen snapshot caller can explicitly install synthetic provider Recents:

```sh
cargo run -p lightcraft-cli -- snapshot --demo --file-provider-preview -o provider.png --size 1440x900
```

Use its normal --script control input to open File > Open Recent Library for a screenshot.
Omitting --file-provider-preview keeps the original provider-free state. The flag supplies
only public Host data to this snapshot run; it adds no transport, authentication or storage.
