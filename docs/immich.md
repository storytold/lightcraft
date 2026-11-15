# Immich

The app links its library with one or more [Immich](https://immich.app) servers (v3.0 or later). Research notes,
endpoints and the test server: the local research note `plan/immich.md` (not published). Code: `crates/immich` (`lcc-immich`, the typed
client and the pure matching / mapping logic), `crates/engine/src/cmd/immich.rs` (commands),
`crates/engine/src/remote.rs` (background work), `crates/ui-egui/src/panels/connections.rs` (UI).

## Connect (IMM-CONNECT)

Settings → Connections → *Add Immich server*: the server URL and an API key (the button opens the server's API-key
page). **Test** shows the version, user and the key's permissions, and which features a key with fewer permissions
can't use; **Connect** stores the key and starts a link pass. Several servers and accounts can be connected.

- The key is kept in the system keychain (`lcc-credentials`: Secret Service on Linux and the BSDs). It is never written
  to the catalog, `connections.json`, logs, the command journal or command results. Where no keychain is reachable
  (macOS and Windows until a safe backend exists, or a Linux session without a Secret Service), connecting reports it;
  a passphrase-protected key file is not wired into the UI yet.
- Accounts (URL, user, version, permissions, a confirmed certificate fingerprint, path mappings, link progress) live in
  `<settings_dir>/connections.json`.
- Servers older than 3.0 are refused with an upgrade message.
- A self-signed certificate (common on home servers) is shown with its SHA-256 fingerprint; *Trust This Certificate*
  pins it for that account (trust on first use). A changed certificate is refused again.
- Errors say what to do: offline (retry), bad key (create a new one), missing permission, server error (retry later),
  TLS failure. Idempotent calls are retried twice on offline / 5xx before the error is shown.

Commands: `immich.test`, `immich.connect`, `immich.disconnect`, `immich.status` (`{check: true}` contacts the servers).
`immich.test` and `immich.connect` take the key as a parameter and are never journaled.

### Locked folders

Immich can keep photos in locked folders, and the server lists them only to an *elevated* session. The app's API-key
session starts locked, so link, import and sync simply never see those assets. Settings → Connections → an account
card → *Locked Folders…* shows whether the session is elevated and, when the account has one, its PIN or password
state; *Unlock…* asks for the secret and elevates the session (`immich.unlockSession`): 4–6 digits go as the PIN
(`pinCode`), anything else as the password. Like the API key, the secret is only a command parameter: `immich.authStatus`
and `immich.unlockSession` are never journaled, and no result or log carries it. The unlock holds for the server
session: Immich locks it again later, so locked assets disappear from link, import, sync and sidecar runs until the
session is unlocked again (`immich.authStatus {account}` reports `{isElevated, pinCode, password, pinExpiresAt?}`).

## Link (IMM-LINK)

`immich.link` starts a background pass that lists the server's images page by page (incremental by `updatedAt` after
the first complete pass; `{full: true}`, the card's *Full Link Pass*, lists everything again) and links each asset to
a catalog photo:

1. by **SHA-1**: Immich's checksum is the SHA-1 of the original; the app computes it in the import read pass and a
   background job (`remote.pump`) back-fills it for photos imported before;
2. else as **probable**: same file name (any case), capture time to the second and file size, when exactly one photo
   matches. Probable links show a dashed badge and an Info row with *Confirm* (`immich.confirmLink`).

Links are catalog remote identities (service `immich`, account `<url>#<user id>`; see [catalog.md](catalog.md)).
Results: the grid badge "Immich", the filter bar's Immich picker (`library.filter {immich: linked|notLinked|probable}`),
Info's *Open in Immich* link (`immich.links`), and the smart-collection rule field `immich`. `immich.unlink` removes
links.

The pass's cursor (the newest `updatedAt` a complete pass has seen) belongs to the catalog and is kept with it, in
`<library>/Immich/link/<account>.json`: a second catalog's first pass lists everything. A cancelled or failed pass
does not move it. Because an incremental pass only lists assets that changed, it never finds an asset whose file
reaches the catalog later; so photos that were just imported, or just got their SHA-1 from the back-fill, are looked
up by checksum on the server (`POST /assets/bulk-upload-check`, on a worker started by `remote.pump`) and linked when
it has them. If the server could not be reached then, *Full Link Pass* catches up. The scheduled sync
(`intervalMinutes`) runs an incremental link pass too. The pass and the lookup hold an activity row; the pass can be
cancelled.

## Import (IMM-IMPORT)

File → *Import from Immich…* (`file.importImmich`): browse the timeline, favourites, albums and people as thumbnails,
select, and import (`immich.import`):

- **Copy** downloads the originals into a dated folder of the destination and imports them in place;
- **Link only** catalogs a preview from the server; the original is downloaded when the photo is opened in Develop
  (`immich.fetchOriginal`), and the photo then points at it.

Rating (1–5; −1 = reject), favourite (pick), description (caption), tags (hierarchical keywords), GPS and place names
are taken over, one way. Importing from an album puts the photos into a catalog album of the same name. Assets already
in the catalog (same checksum, or already linked) are not downloaded again, but they are part of what was asked for:
each is linked to its photo (by checksum) and added to the album with the new ones (`remote.pump` → `import.present`
counts them). The import and the download of a link-only original hold an activity row with progress and can be
cancelled (what was downloaded before the stop is still imported); a failed original download is reported, and
opening the photo in Develop again tries again.

A file imported from disk whose SHA-1 is the checksum of a link-only photo's asset doesn't stay a second
photo: the link-only photo takes the file as `immich.fetchOriginal` would (`cmd::immich::merge_into_link_only`,
called from `library.import`), the new entry is removed, and it joins the import's collection in its place.

## External libraries (IMM-EXTLIB)

When Immich indexes the same folders as the catalog as an *external library*, nothing is uploaded. Immich does not
hash external-library files (their `checksum` is the SHA-1 of `path:` + the container path, checked against v3.3.1), so
the link pass links them **by path**: the asset's `originalPath`, translated by the account's path mapping, is the
photo's file. Settings → Connections → *External Libraries…* (`immich.libraries`) lists the server's libraries and
a path mapping table (`immich.setPathMaps`) between Immich's container paths and local folders (a suggestion is made
from matching folder names), and shows which catalog folders each library covers. *Write XMP Sidecars*
(`immich.writeSidecars`, and *Push* below with `externalOnly`) writes sidecars for the photos in mapped folders so
Immich reads ratings, descriptions and keywords, then asks Immich to re-read them: the sidecar discovery job finds
new sidecars (admin key) and `refresh-metadata` re-reads the linked assets, because a library scan skips files that
did not change. *Rescan in Immich* (`immich.scanLibraries`) starts a scan of the libraries covering mapped folders
(new files).

## XMP sidecars (IMM-SIDECARS)

Immich keeps an `.xmp` sidecar beside every asset it stores (and, for an external library, beside the original on
disk), and serves it through `GET /asset-files` (v3.2.0). The app uses that as a bridge between libraries:

- **Pull** (`immich.pullSidecars {account?, ids?, all?, overwrite?, writeFiles?, dryRun?, background?}`) downloads
  each linked asset's sidecar, applies it to the catalog as one undo step (metadata, GPS, keywords and the
  `lc:settings` develop settings another instance published) and writes it beside the original. A photo edited here
  after the asset changed is skipped (`skipped[].reason`: `changedInCatalog`, `conflict`, `editedLocally`) unless
  `overwrite`; `writeFiles` defaults to true. With no `ids`/`all` it covers the selection, or every linked photo when
  nothing is selected (at most 5000 per run, reported as `truncated`; pull again to continue). Needs the key's
  `assetFile.read` permission and a server ≥ 3.2.0 (older servers say so and pushing still works).
- **Push** (`immich.pushSidecars {account?, ids?, externalOnly?, full?, refresh?, dryRun?, background?}`) writes
  the whole XMP sidecar for photos in mapped external-library folders (then asks Immich to read it back) and sends
  the fields Immich keeps in its own sidecar for managed assets: rating, description, capture time, location and
  tags (it only adds tags, never removes one). Immich merges those into the sidecar it holds. `full` replaces a
  managed asset's whole sidecar instead: Immich's current sidecar is read, the app's packet merged into it (our
  properties replace, everything else — another app's `crs:` settings, unknown namespaces — stays), the merged XMP
  uploaded on a tiny temporary asset, copied onto the real asset (`PUT /assets/copy`), and the temporary asset
  removed. This is the only way to add develop settings to an asset uploaded without a sidecar: Immich has no
  direct write endpoint, so `full` needs `assetFile.read`, `asset.upload`, `asset.copy`, `asset.delete` and
  `job.create`, and creates and removes one tiny asset per photo. The original is never deleted or re-uploaded.
  *Write XMP Sidecars* in the external-library helper is the `externalOnly` form.

  Publishing an original (`send: original|both`) also sends its whole XMP with the upload, so another library can
  pull it back.

Both run on a worker (`background: true`) with an activity row, and their result comes back through `remote.pump`
(`sidecars`); the account card's **XMP Sidecars** section has **Push** and **Pull** buttons and the last run's
counts. Two libraries can therefore share one Immich server: one publishes (or writes the external-library files),
the other pulls the sidecars.

Tests: `cargo test -p lcc-immich sidecar` (the client, the version gate, the copy call and the pull's conflict
rules), the engine's in-process server tests (`pull_sidecars_*`, `push_sidecars_*`,
`full_sidecar_push_merges_immichs_sidecar_and_copies_it_back`,
`pulling_sidecars_needs_a_server_that_lists_them`), and the live `live_asset_sidecar_round_trip` (an upload's
sidecar, `lc:settings` included, is served back).

## Tests

- `cargo test -p lcc-immich`: the client against recorded v3.3.1 responses (`crates/immich/tests/fixtures`), and the
  error paths (offline, bad key, old server, 5xx with retries, TLS to a plain server, hostile ids).
- `cargo test -p lcc-engine tests_immich`: SHA-1 at import and its back-fill, connect / link / import / link-only /
  sidecars through the commands, against an in-process server.
- Nightly, with Docker: `cargo xtask immich up && cargo xtask immich seed`, then
  `cargo test -p lcc-immich --test live -- --ignored` (lists, downloads an original and checks its SHA-1 equals
  Immich's checksum), and `cargo test -p lcc-engine live_external -- --ignored`: an external library over a folder of
  generated photos (the compose file mounts `target/immich/extlib` at `/mnt/extlib`, override with
  `IMMICH_EXTLIB_DIR`), scanned, linked by path, then rating, description and a keyword written as XMP and read back
  from Immich.
- Version matrix: `cargo xtask immich up --version min|pinned|latest|vX.Y.Z [--port N]` picks the server image
  (`min` = v3.0.0, the oldest supported; `pinned` = v3.3.1, the default; `latest` = Immich's `release` tag); another
  port runs a second server as its own compose project. `.github/workflows/immich.yml` (nightly, manual, and PRs
  touching the Immich code; no secrets) runs the live tests against `min` and `latest`.

## Sync (IMM-SYNC)

`immich.sync {dryRun?, full?, background?}` syncs the metadata of linked photos both ways: stars ↔ rating, pick (or
5★) ↔ favourite, reject ↔ archived (off by default), caption (or title) ↔ description, keywords ↔ tags (merged as
sets; export-excluded keywords are never sent), GPS ↔ location and capture time ↔ date (these two ask). Each field
is compared against the value both sides had at the last sync: a one-sided change flows to the other side; when both
changed, the field's policy decides (`catalog`, `immich`, `newest` — the default —, or `ask`). Settings:
`immich.setSync {config}` (per-field `direction` and `policy`, `favorite`, `description`, `intervalMinutes`).
Unresolved conflicts: `immich.syncConflicts`, decided with `immich.resolveConflict {keep}`. Deletions are never
synced on their own: they wait in a queue (`immich.syncStatus` → `deletions`, `immich.resolveDeletion
{action: apply|keep, assets?}` — `assets` omitted resolves every queued deletion, `[]` resolves none; Immich assets go to its trash, never a hard delete). The state between runs and the activity
log live in `<library>/Immich/sync/<account>.json`; offline / bad-key failures change nothing and a scheduled sync
retries with backoff.

What a run records as "both sides had this" (the base) is only what both sides really hold afterwards:

- **A run cut short** (the server went away, a tag could not be set): a photo whose update or tag changes were not
  sent keeps its old base, so the next run sends them. Nothing counts as synced because the run got as far as
  planning it.
- **A photo edited while a run was out** is not written over, and the fields Immich had changed are not recorded as
  taken: the next run takes them. The same holds when the catalog write fails.
- **A location or capture time cleared in the catalog** can't be cleared in Immich (its update has no "none"). The
  cleared value stays in the catalog as a standing local choice: nothing is sent, nothing is pulled back, and it is
  not a conflict. If the value later changes in Immich, that is a change on both sides and the field's policy decides.
- **Newest wins** compares Immich's `updatedAt` with when the catalog was edited: the time the photo's metadata was
  last written to its XMP sidecar, when that sidecar is in step with the catalog (automatic XMP writing keeps it so);
  otherwise the time a sync first saw the change. The catalog keeps no per-field edit time, so without a sidecar an
  edit made days before the sync still counts as made at the sync.
- **The sync state is one catalog's.** Opening another catalog loads that catalog's own state; a run that comes back
  after the library changed is dropped.
- **The deletion queue only holds deletions that are still true.** Every run looks at the queued assets again and
  drops one whose asset is back out of Immich's trash (or whose photo is back out of Recently Deleted);
  `immich.resolveDeletion {action: apply}` checks each one first and reports those as `restored` instead of deleting.

*Preview Changes* (`immich.sync {dryRun: true, background: true}`) runs on a worker and lists what would change in the
Sync section (`immich.syncStatus` → `preview`).

**Background calls.** The Connections screen never waits for a server. `immich.libraries`, `immich.authStatus`,
`immich.unlockSession`, `immich.scanLibraries`, `immich.importPeople` and `immich.pushPeople` take `background: true`:
they return `{started, key}`, hold an activity row while they are out, and their result is taken with
`immich.answer {key}` (`remote.pump` lists the keys that wait in `answers`).

## People and smart search (IMM-PEOPLE, IMM-SEARCH)

`immich.importPeople` reads Immich's faces into face regions marked `immich:<personId>` (the People view shows their
names); `immich.confirmFaces` confirms them, `immich.pushPeople {merge?}` sends names back (renames, merges).
`immich.smartSearch {query, mode: smart|ocr|description|place, show?, saveAs?}` maps the server's results to linked
photos; `show` makes them a temporary collection, `saveAs` an album.

## Edit In versions and stacks (RT.C)

- **Edit versions** (`immich.uploadVersion`): when the external editor saves an Edit In version
  whose catalog stack holds an Immich-linked photo, the app uploads the version, links it, and
  stacks it on Immich with the version as the primary (`POST /stacks` with the source's stack
  primary, which Immich v3.3.1 merges in). Runs on a worker with an activity row; errors are toasts.
- **Saving again**: Immich v3.3.1 has no endpoint that replaces an asset's file
  (`PUT /assets/{id}/original` is gone), so each save is a new asset on top of the stack. The app
  asks **Keep both** (default) or **Replace** (the previous upload's albums are copied to the new
  asset, then it goes to Immich's trash), with "remember my choice"
  (`immich.versionSettings {resave: ask|keepBoth|replace}`, kept in `<library>/Immich/versions.json`).
- **Stack sync** (part of `immich.sync`): catalog stacks of linked photos ⇄ Immich stacks,
  three-way against the last sync (`lcc_immich::stacks`). The top photo is the primary, and the order
  below it stays local. Unlinked photos keep their place in the catalog stack. When both sides
  changed the same stack, the catalog's version wins. Needs `stack.read`, `stack.create` and
  `stack.delete`; a key without them skips the stack part and says so in the sync's errors.
  `PUT /stacks/{id}` (deprecated in v3) is not used.
- Tests: `edit_versions_upload_and_stack_on_immich` and `stacks_mirror_both_ways` (in-process
  server), `live_stacks_round_trip` (real server, `--ignored`).

## Publish (IMM-PUBLISH) and albums

An Immich publish service (`publish.createService {kind: "immich", settings: {account, send: rendered|original|both,
deleteRemoved}}`) publishes each collection to the Immich album of the same name (created when missing). Every
upload is checked by SHA-1 first, so Immich never stores a duplicate; an original already linked to an Immich asset
(including external-library files) is never uploaded. An uploaded original carries the photo's whole XMP sidecar
(metadata and develop settings), so another library can pull it back (IMM-SIDECARS). `both` stacks the render on top
of the original. Re-publishing
uploads the new render, moves album membership and sends the old render to Immich's trash; removing a photo from the
collection takes it out of the album (its render goes to the trash only with `deleteRemoved`).

The album is remembered by id (`<library>/Immich/publish/<service>-<collection>.album`): renaming the collection, or
moving it into another collection set, renames the same Immich album on the next publish run. Collection sets inside
the service's set become name prefixes (`Trips / Paris`; Immich albums are flat); make one with `album.create
{folder: true, parent: <service set>}` and pass it as `publish.createCollection {parent}` (or move the collection
there). Failed uploads (offline, 5xx, dropped connection) are retried three times with backoff; each attempt asks the
server by checksum first, so an upload whose answer was lost is never stored twice, and a run cut short resumes on
the next run (photos already sent are found by checksum). In the app, Publish Services ▸ Set Up / Edit Settings…
has the Immich form: account, what to send (rendered / original / both) and "trash removed renders".

`immich.linkAlbum {album}` links a catalog album with an Immich album; `immich.syncAlbums` then keeps the same linked
photos in both (added or removed on one side since the last sync: the same on the other).

**Album rule.** Settings → Connections → Sync… → *Albums ↔ albums* (`immich.setSync {config: {albums: {direction,
policy}}}`) maps every plain catalog album to the Immich album of the same name:

- *Off* (default): only albums linked by hand sync, as above.
- *To Immich* / *From Immich*: one way; the other side's changes are not followed (an album deleted on the source-only
  side is put back).
- *Two-way*: creates, renames, photos added or removed, and album deletes travel both ways.

Deleting an album never deletes photos, only the album on the other side, and only as the policy says: *Catalog wins*
/ *Immich wins* decide, *Newest wins* deletes unless the other side changed the album since the last sync, *Ask* (the
default) leaves it in the Sync… panel's *Albums deleted on one side* list (`immich.resolveAlbumDeletion {album,
action: apply|keep}`). A name changed differently on both sides follows the policy, or is left alone. Published
collections, smart albums, folders and the Quick Collection never map. Sync Now and the background schedule run the
album rule too; runs show in the activity stack. The key needs `album.create`, `album.update`, `album.delete`,
`albumAsset.create` and `albumAsset.delete` for this.

Two rules keep a failed run from costing photos or collections:

- A link remembers which photos both albums held after the last run. Only photos **known to be in the Immich album**
  go in there: Immich answers an album add with one result per asset, and a photo it refused (or an add that failed
  altogether, also right after creating the album) is simply added again by the next run. It is never read as
  "removed in Immich" and taken out of the collection.
- A collection that holds **photos Immich does not have** (not linked to an asset) is never deleted because its
  Immich album was deleted, whatever the policy. It is kept, taken out of the album sync (no album is made or
  matched for it again until it is linked by hand with `immich.linkAlbum`), and the run lists it in `kept`; the Sync
  section says how many were kept. *Ask* still asks, and the user may delete it knowingly.
