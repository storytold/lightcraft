# Immich research note

[Immich](https://immich.app) is a self-hosted photo and video server. The app integrates with it (tracker section
"IMM. Immich integration", PLAN.md §2.14) so the desktop catalog and an Immich server act as one library.

**Checked against:** Immich **v3.3.1** (released 2026-10-08, the latest release on 2026-10-10), using the OpenAPI
document published with that tag (`open-api/immich-openapi-specs.json`, `info.version` 3.3.1) and a live v3.3.1
server started by `cargo xtask immich up`. Facts marked *(verified)* were confirmed against that server.

## Licence note

- The Immich server, web app and mobile apps are **AGPL-3.0**.
- We only talk to a server over HTTP. Our client is written from the public API documentation and the OpenAPI
  description; we do **not** vendor Immich code, its generated TypeScript/Dart SDKs, or code generated from its
  OpenAPI file. Hand-written request/response structs for the endpoints we use are our own work.
- The test server in `xtask/immich/compose.yml` runs Immich's published container images unmodified; nothing from
  them is distributed with the app.

## Versions

- **Minimum supported: v3.0.0.** The v3 line is what we test. Endpoints we rely on were all marked stable in v2,
  but behaviour we depend on (rating −1 = rejected, the v3 deprecations below) is checked only on v3. Older servers
  get a clear "please upgrade" message from IMM-CONNECT rather than partial behaviour.
- **Version check:** `GET /api/server/version` → `{major, minor, patch}` (no auth needed); `GET /api/server/ping` →
  `{"res":"pong"}` for reachability. Store the version per server and gate features on it.
- **API history metadata:** every operation in the OpenAPI file carries `x-immich-history` (added / beta / stable /
  deprecated per version) and `x-immich-permission`. Use them when moving to a new release: diff the operations we
  call and fail the integration tests on a removed one.
- **Deprecations to watch (v3):** `PUT` updates of single objects (`PUT /assets/{id}`, `PUT /assets`,
  `PUT /tags/{id}`, `PUT /people/{id}`, `PUT /stacks/{id}`, `PUT /libraries/{id}`,
  `PUT /api-keys/{id}`) are marked deprecated with a replacement of the same operation id; they still work in
  v3.3.1 *(verified for `PUT /assets/{id}`)*. Wrap each update behind one function so the switch is local.
  `GET /server/config` and `/server/features` are deprecated in favour of a public-config endpoint (v3.2.0).
- **Moving to a new release:** update the four images in `xtask/immich/compose.yml` from that release's own compose
  file, re-run the integration tests, then update "Checked against" here.

## Authentication, API keys and permissions

- **API key** (what the app stores): header `x-api-key: <secret>`. Created by a logged-in user with
  `POST /api/api-keys {name, permissions[]}`; the response carries `secret` once *(verified)*. Keys are per user.
- **Permissions** are fine-grained strings (`asset.read`, `asset.upload`, `asset.update`, `asset.download`,
  `album.create`, `albumAsset.create`, `tag.asset`, `person.read`, `sharedLink.create`, `library.read`,
  `stack.create`, …) or `all`. IMM-CONNECT should ask for the minimal set per feature and explain which features a
  key with fewer permissions disables. `GET /api/api-keys/me` reads the current key's permissions.
- **Session login** (test tooling only): `POST /api/auth/login {email, password}` → `accessToken`, used as
  `Authorization: Bearer`. The first user is created with `POST /api/auth/admin-sign-up {email, password, name}`,
  which is refused once an admin exists *(verified)*.
- The key is a secret: stored in the OS keychain (`credentials` crate), never in the catalog, logs or XMP.

## Concepts

| Concept | What it is | How we map it |
|---|---|---|
| **Asset** | one photo or video owned by one user (`id` UUID, `type` IMAGE/VIDEO, `originalFileName`, `originalPath`, `fileCreatedAt`, `localDateTime`, `exifInfo`, `isFavorite`, `visibility`, `isTrashed`, `libraryId`, `stack`, `people`, `tags`) | a catalog photo, linked through the `remote_identity` table (server, asset id, checksum) |
| **Checksum** | **SHA-1 of the original file, base64 encoded** in `AssetResponseDto.checksum` *(verified: equals `openssl sha1 -binary | base64`)*; inputs to the duplicate check also accept hex | computed at import for every original (Phase 1.7); the link key for IMM-LINK |
| **Duplicate detection** | an upload whose checksum matches an existing asset of the same user is not stored again: `POST /assets` answers `status: "duplicate"` with the existing id *(verified)*; `POST /assets/bulk-upload-check` answers `action: reject, reason: duplicate, assetId` per item without uploading *(verified)* | link before upload; never upload twice |
| **Visibility** | `timeline` (normal), `archive` (hidden from the timeline), `hidden` (e.g. the video half of a live photo), `locked` (locked folder, needs a PIN session) | archive ↔ an "archived" flag in sync; never touch `locked` assets |
| **Album** | a named, ordered set of assets, possibly shared with other users | collection ↔ album (IMM-PUBLISH, IMM-SYNC) |
| **Tag** | hierarchical tags (`parent/child` values), assigned to assets | keyword hierarchy ↔ tags |
| **Person / face** | people with names and face boxes per asset (`/people`, `/faces`), found by Immich's ML service | IMM-PEOPLE: import names and regions; push names back |
| **Stack** | a group of assets with a primary (`POST /stacks {assetIds}`, first id = primary) | our stacks; IMM-PUBLISH stacks a render with its original |
| **External library** | an admin-defined set of server-side folders (`importPaths`, `exclusionPatterns`) that Immich scans and indexes **in place** (read-only originals; `isOffline` when a file disappears) | IMM-EXTLIB: the catalog and Immich index the same folders; link by checksum or by path mapping, never upload |
| **Shared link** | a public URL for an album (`type: ALBUM`) or for chosen assets (`INDIVIDUAL`), with optional expiry, password, download/upload permission, metadata visibility and custom slug | IMM-SHARELINK |
| **Trash** | deleted assets go to trash first (`isTrashed`), restorable via `/trash/restore/assets` | IMM-PUBLISH removes only by trashing, never force-deletes |
| **Smart search** | CLIP embedding search computed by the ML service | IMM-SEARCH |

## Ratings and favourites

- **Rating** lives in the asset's EXIF info (`exifInfo.rating`) and is set with `UpdateAssetDto.rating`.
  Valid values: **1–5** (stars), **−1** (rejected), or **null** (unrated). **0 is refused** with a validation error
  *(verified on v3.3.1)*. Mapping: our 0 stars ↔ null; our reject flag ↔ −1 *only when the photo has no star
  rating* (one field cannot hold both, so the conflict rule in IMM-SYNC is: a reject wins on push, and a −1 from
  Immich sets reject and clears stars).
- Immich reads the rating from the file's metadata (XMP/EXIF) on upload; the web UI shows ratings only when the
  user enables them in their preferences, but the API accepts them regardless.
- **Favourite** (`isFavorite`) is a separate per-asset boolean, independent of rating. Mapping: favourite ↔ **pick
  flag** (configurable: pick or a chosen colour label), as in PLAN.md §2.14.
- **Description** (`UpdateAssetDto.description`, shown in `exifInfo.description`) ↔ our caption. Location
  (`latitude`, `longitude`) and capture time (`dateTimeOriginal`) are writable the same way.
- Immich has no colour labels and no title field separate from description; those stay local (or go to tags if the
  user opts in).

## Endpoints we need

All paths are under `/api`. Permission in brackets.

### Connect (IMM-CONNECT)
- `GET /server/ping`, `GET /server/version`, `GET /server/about` [server.about], `GET /server/media-types`
- `GET /users/me` [user.read], `GET /api-keys/me`

### Link and import (IMM-LINK, IMM-IMPORT)
- `POST /assets/bulk-upload-check` [asset.upload]: checksum → existing asset id, without uploading
- `POST /search/metadata` [asset.read]: filter by `checksum`, `originalPath`, `libraryId`, `albumIds`, `personIds`,
  `tagIds`, `isFavorite`, `rating`, dates, `visibility`; paged with `page` / `size`
- `GET /assets/{id}` [asset.read], `GET /assets/{id}/original` [asset.download], `GET /assets/{id}/thumbnail`
  [asset.view]
- `GET /albums` / `GET /albums/{id}` [album.read]
- Timeline browsing uses `/timeline/buckets` and `/timeline/bucket`, but they are marked **internal** (v1); prefer
  `POST /search/metadata` with date ranges and only fall back to the buckets for speed.

### External library (IMM-EXTLIB)
- `GET /libraries`, `GET /libraries/{id}` [library.read], `POST /libraries` [library.create, admin],
  `POST /libraries/{id}/scan` [library.update], `POST /libraries/{id}/validate`

### Publish (IMM-PUBLISH)
- `POST /assets` [asset.upload] multipart: `assetData`, `fileCreatedAt`, `fileModifiedAt` (required), `filename`,
  `isFavorite`, `visibility`, optional `sidecarData`; header `x-immich-checksum` (SHA-1) lets the server reject a
  duplicate before reading the body *(upload verified)*
- `POST /albums` [album.create], `PUT /albums/{id}/assets` and `PUT /albums/assets` [albumAsset.create],
  `DELETE /albums/{id}/assets` [albumAsset.delete], `PATCH /albums/{id}` [album.update]
- `POST /stacks` [stack.create], `DELETE /stacks/{id}/assets/{assetId}` [stack.update]
- `DELETE /assets` [asset.delete] (to trash), `POST /trash/restore/assets`

### Sync (IMM-SYNC)
- `PUT /assets/{id}` / `PUT /assets` [asset.update]: `rating`, `isFavorite`, `description`, `latitude`,
  `longitude`, `dateTimeOriginal`, `visibility` (deprecated method, see Versions)
- `GET /tags`, `PUT /tags` (upsert by value) [tag.create], `PUT /tags/assets` (bulk) / `PUT /tags/{id}/assets` /
  `DELETE /tags/{id}/assets` [tag.asset]
- Change feed: `POST /sync/stream` [sync.stream] with `POST /sync/ack` checkpoints is what Immich's own mobile app
  uses for incremental sync. Prefer it over polling once evaluated; until then poll with `updatedAfter` in
  `POST /search/metadata`.

### People (IMM-PEOPLE)
- `GET /people`, `GET /people/{id}`, `GET /people/{id}/thumbnail` [person.read], `GET /faces?id=<assetId>`
  [face.read], `PUT /people` (bulk rename) [person.update], `POST /people/merge` (v3.2.1)

### Search (IMM-SEARCH)
- `POST /search/smart` [asset.read]: `query` (natural language), or `queryAssetId` (find similar), plus the same
  filters as metadata search; `GET /search/suggestions`, `GET /search/explore`, `GET /search/places`

### Shared links (IMM-SHARELINK)
- `POST /shared-links` [sharedLink.create]: `type` ALBUM / INDIVIDUAL, `albumId` or `assetIds`, `expiresAt`,
  `password`, `allowDownload`, `allowUpload`, `showMetadata`, `description`, `slug`;
  `GET /shared-links`, `PATCH /shared-links/{id}`, `DELETE /shared-links/{id}`

## Test server

`cargo xtask immich up | seed | down [--volumes]` (`xtask/src/immich.rs`):

- `xtask/immich/compose.yml` pins `immich-server` and `immich-machine-learning` to `v3.3.1`, and the Valkey 9 and
  `immich-app/postgres:14-vectorchord0.4.3-pgvectors0.2.0` images by digest, exactly as that release's compose
  file does. Data lives in docker volumes; the server listens on `127.0.0.1:2284` (not the default 2283).
- `seed` creates `admin@example.invalid`, logs in, creates an API key with `all` permissions (written to
  `target/immich/api-key`, mode 0600), uploads 8 small PNGs generated by the xtask (our own work, CC0) and puts them
  in the album "xtask fixtures". It is idempotent: later runs see the uploads as duplicates.
- `IMMICH_URL` points the seed at another test server.
