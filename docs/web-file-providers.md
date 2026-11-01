# Optional web library providers

This integration is disabled by default. The web app publishes window.lightcraftFileProviders and lightcraft-file-provider events (protocolVersion 1), but no provider is registered. Desktop and CLI do not install the browser API. The original catalog autosave, local library paths, native backup/restore and image exports keep their existing behavior.

LightCraft edits a library rather than individual project documents. Its provider resource is a complete native library ZIP backup: catalog snapshot/journal, presets, view state, preferences and the original photographs referenced by that captured catalog. It is the same archive layout as File > Back Up Library, not a new private document format.

## Registration and Recent libraries

A trusted same-origin host explicitly calls registerProvider(id, label, namespace) and receives a new connectionId. The opaque namespace identifies the signed-in account; it is not a credential. setResources(connectionId, [{id,name,updatedAt}]) supplies up to 100 external libraries. File > Open Recent Library separates Recent — provider label from Recent on this device. Provider IDs are opaque and never become local filesystem paths.

requestOpen(resourceId) emits openRequest. The host replies using openResource(requestId, filename, Uint8Array), or cancelOpen(requestId,error). ZIPs restore into a fresh browser-storage folder; the old library is retained. CRCs, original content hashes and required catalog originals are checked. The new catalog must load through the engine before its browser pointer changes.

An open waits for the current library's local writes. Imports, unsaved storage failures, exports, dialogs and pending provider saves prevent a replacement. While the new library is restored, editor input and automation processing pause so edits cannot land in the wrong library. Only then is the new library admitted, assigned a fresh instance ID and acknowledged with opened. No page reload or second click is required. Invalid archives keep the old library active.

## Save and acknowledgement

Registering a provider adds Save Library (Ctrl/Cmd+S) and Save Library As (Ctrl/Cmd+Shift+S). These entries are hidden without an enabled provider. Each Save emits saveDialog with requestId, documentId, revision, name, saveAs and an optional resourceId. The host calls resolveSaveDialog(requestId, provider|device|cancel, filename). A .zip filename is required.

The capture is an immutable atomic snapshot of the six small library files, paired with the originals referenced by that snapshot. An absent, corrupted or non-browser original fails the save. Unrelated originals left in browser storage are never uploaded. Subsequent edits can continue while the archive is prepared and persisted.

The device destination uses the existing browser download writer. The provider destination emits save with destination=provider and native ZIP bytes. Only after a durable write may the host call saveResult(requestId,saved,resourceId). failed, cancel and download never advance the provider checkpoint. Acknowledgement records only the captured file-generation counter; later changes remain dirty. The catalog's independent local autosave is never marked as a successful remote save. Save As does not reuse the old resource ID.

snapshot() describes the active library, generation, dirty/pending/loading state and separate provider resources. Library IDs are session-local and change on replacement even if catalog photo IDs or generations repeat. Before-unload protection applies to pending provider work and unsaved provider revisions only after explicit registration; publishing the API alone does not change the default page behavior.

## Account boundaries and limits

unregisterProvider rejects while choices, encoding, reads or acknowledgements are pending. setEnabled(false) pauses new work while retaining the identity of existing requests. Old resource lists, duplicate completions and replies from earlier page sessions are rejected using bounded IDs, a random page nonce and a checked request counter.

Each transfer, retained snapshot and bounded queue is limited to 80 MiB, 16 pending operations, 64 events and 100 Recent resources. Original files are size-checked before copying from browser storage into WASM. ZIP headers and the final central directory count toward the limit. Filename limits apply separately to host-visible resource names and embedded ZIP paths.

The host owns authentication, per-user authorization, durable writes, conflicts, transport, retries and destination UI. No backend, service URL, cloud widget, token storage or cross-origin messaging is installed here. Browser-local libraries remain local data; the host must define its own shared-device/logout policy. Large libraries and catalogs referring to external desktop paths cannot be uploaded through this bounded browser adapter; import the originals or use the native backup workflow.

## Validation

Source regression fixtures cover opt-in defaults, save choices, separate resource identities, atomic library snapshots, content generations, overflow handling and archive bounds including the central directory. Execution results are recorded only after validation of the published source candidate.

## Native UI preview

The existing offscreen snapshot caller can explicitly install synthetic provider Recents:

```sh
cargo run -p lightcraft-cli -- snapshot --demo --file-provider-preview -o provider.png --size 1440x900
```

Use its normal --script control input to open File > Open Recent Library for a screenshot.
Omitting --file-provider-preview keeps the original provider-free state. The flag supplies
only public Host data to this snapshot run; it adds no transport, authentication or storage.
