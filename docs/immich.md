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

## Link (IMM-LINK)

`immich.link` starts a background pass that lists the server's images page by page (incremental by `updatedAt` after
the first complete pass; `{full: true}` starts over) and links each asset to a catalog photo:

1. by **SHA-1**: Immich's checksum is the SHA-1 of the original; the app computes it in the import read pass and a
   background job (`remote.pump`) back-fills it for photos imported before;
2. else as **probable**: same file name (any case), capture time to the second and file size, when exactly one photo
   matches. Probable links show a dashed badge and an Info row with *Confirm* (`immich.confirmLink`).

Links are catalog remote identities (service `immich`, account `<url>#<user id>`; see [catalog.md](catalog.md)).
Results: the grid badge "Immich", the filter bar's Immich picker (`library.filter {immich: linked|notLinked|probable}`),
Info's *Open in Immich* link (`immich.links`), and the smart-collection rule field `immich`. `immich.unlink` removes
links.

## Import (IMM-IMPORT)

File → *Import from Immich…* (`file.importImmich`): browse the timeline, favourites, albums and people as thumbnails,
select, and import (`immich.import`):

- **Copy** downloads the originals into a dated folder of the destination and imports them in place;
- **Link only** catalogs a preview from the server; the original is downloaded when the photo is opened in Develop
  (`immich.fetchOriginal`), and the photo then points at it.

Rating (1–5; −1 = reject), favourite (pick), description (caption), tags (hierarchical keywords), GPS and place names
are taken over, one way. Importing from an album puts the photos into a catalog album of the same name. Assets already
in the catalog (same checksum, or already linked) are skipped before downloading.

## External libraries (IMM-EXTLIB)

When Immich indexes the same folders as the catalog as an *external library*, nothing is uploaded. Immich does not
hash external-library files (their `checksum` is the SHA-1 of `path:` + the container path, checked against v3.3.1), so
the link pass links them **by path**: the asset's `originalPath`, translated by the account's path mapping, is the
photo's file. Settings → Connections → *External Libraries…* (`immich.libraries`) lists the server's libraries and
a path mapping table (`immich.setPathMaps`) between Immich's container paths and local folders (a suggestion is made
from matching folder names), and shows which catalog folders each library covers. *Write XMP Sidecars*
(`immich.writeSidecars`) writes sidecars for the photos in mapped folders so Immich reads ratings, descriptions and
keywords, then asks Immich to re-read them: the sidecar discovery job finds new sidecars (admin key) and
`refresh-metadata` re-reads the linked assets, because a library scan skips files that did not change. *Rescan in
Immich* (`immich.scanLibraries`) starts a scan of the libraries covering mapped folders (new files).

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
{action: apply|keep}`; Immich assets go to its trash, never a hard delete). The state between runs and the activity
log live in `<library>/Immich/sync/<account>.json`; offline / bad-key failures change nothing and a scheduled sync
retries with backoff.

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
(including external-library files) is never uploaded. `both` stacks the render on top of the original. Re-publishing
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
