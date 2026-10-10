# Immich

The app links its library with one or more [Immich](https://immich.app) servers (v3.0 or later). Research notes,
endpoints and the test server: the local research note `plan/immich.md` (not published). Code: `crates/immich` (`dac-immich`, the typed
client and the pure matching / mapping logic), `crates/engine/src/cmd/immich.rs` (commands),
`crates/engine/src/remote.rs` (background work), `crates/ui-egui/src/panels/connections.rs` (UI).

## Connect (IMM-CONNECT)

Settings → Connections → *Add Immich server*: the server URL and an API key (the button opens the server's API-key
page). **Test** shows the version, user and the key's permissions, and which features a key with fewer permissions
can't use; **Connect** stores the key and starts a link pass. Several servers and accounts can be connected.

- The key is kept in the system keychain (`dac-credentials`: Secret Service on Linux and the BSDs). It is never written
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

- `cargo test -p dac-immich`: the client against recorded v3.3.1 responses (`crates/immich/tests/fixtures`), and the
  error paths (offline, bad key, old server, 5xx with retries, TLS to a plain server, hostile ids).
- `cargo test -p dac-engine tests_immich`: SHA-1 at import and its back-fill, connect / link / import / link-only /
  sidecars through the commands, against an in-process server.
- Nightly, with Docker: `cargo xtask immich up && cargo xtask immich seed`, then
  `cargo test -p dac-immich --test live -- --ignored` (lists, downloads an original and checks its SHA-1 equals
  Immich's checksum), and `cargo test -p dac-engine live_external -- --ignored`: an external library over a folder of
  generated photos (the compose file mounts `target/immich/extlib` at `/mnt/extlib`, override with
  `IMMICH_EXTLIB_DIR`), scanned, linked by path, then rating, description and a keyword written as XMP and read back
  from Immich.

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
