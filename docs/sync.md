# Device sync

Paired devices on the same network keep a catalog, or chosen collections of it, in step with no server in
between. This page describes what is built, how it works, and its known limits. The user's guide is the manual's
*Device sync* page (`docs/manual-site/src/content/docs/sync/devices.mdx`).

## Parts

| Part | Where |
|---|---|
| Discovery (mDNS service discovery), pairing with a six-digit code both people confirm, an encrypted mutually authenticated channel (commit-then-reveal handshake, X25519, Ed25519, ChaCha20-Poly1305) | `crates/peer` (`lcc-peer`) |
| The replicated model: group, collections, grants and takes, per-photo records, hybrid clock, pills, wire messages. Pure data and merge rules, no sockets or files | `crates/sync` (`lcc-sync`) |
| The catalog side: the replica of the open catalog, what changed here, applying what changed elsewhere, the exchange threads, files | `crates/engine/src/device_sync/` |
| `sync.*` commands (UI, CLI, MCP) | `crates/engine/src/cmd/device_sync.rs` |
| Settings ▸ Sync, the new-collection question, the top bar's waiting count | `crates/ui-egui/src/panels/peer_sync.rs` |
| The camera roll's review (in place, Keep adding new photos), the Modern library's collection picker | `crates/ui-egui/src/import_screen.rs`, `crates/ui-egui/src/modern_sources.rs` |
| Pills on grid cells and collection rows | `crates/ui-egui/src/panels/sync_pill.rs` |

## Model

- A **group** is the set of devices syncing one catalog, in one of two **modes**:
  - **Catalogue** (the whole catalog): one device's catalog is kept (the *keeper*), every other device joins with an
    empty one. Every collection is listed on every device, whether its photos travel or not.
  - **Collections**: each device keeps its own catalog and lists only the collections it was allowed.
- Every collection has one **owner** (a device; later an Immich server). The owner sets its **grant**:
  `none` (not shared), `preview` (others get previews to develop on, can't add), `serve` (originals too, can't
  add), `sync` (any device adds; originals go both ways). The photos in no collection are a collection of their
  own per device (`unfiled:<device>`).
- Every other device sets its **take**, never more than the grant allows: `none`, `previews` (edits go back),
  `fullQuality` (originals; edits go back), `fullSync` (photos added there go to the others too).
- A collection created while the whole catalog syncs is *undecided* until the person answers (`sync.allow` or
  `sync.skip`); the UI asks at once.
- A photo's record (develop settings, rating, flag, label, metadata) merges field by field, the later change
  winning by a hybrid logical clock.

## Photo identity

One rule names a photo on every device (`device_sync::record::key_of`):

- `sha1:<hex>`, the SHA-1 of the original's bytes, as soon as it is known. Imports hash the original, and every
  catalog back-fills the SHA-1 of older photos in the background. An Immich link-only photo, whose original isn't
  on the device, uses the checksum Immich reports for the asset, which is the same SHA-1. So a device holding the
  file and a device holding only the Immich link give the same photo the same key, and Immich and device sync
  agree on what "the same photo" is.
- Until then, the catalog's content hash of the original (older replicas used only this).
- Virtual copies, a second import of the same bytes and link-only photos without a checksum have no key.

When a photo's SHA-1 becomes known its key moves (`Replica::rekey`): its record, its places in collections and
what is queued for each device move to the new key, and if another device already told of the new key, the two
merge field by field. Opening a catalog whose replica still uses content-hash keys moves them the same way.
Each catalog keeps a table of content-hash keys of photos it knows by SHA-1, so a device that hasn't hashed a
photo yet is still understood. A photo sent under its SHA-1 key carries its content hash. A device that has
a photo with that content hash but no SHA-1 does not take the other device's word for it: it merges the record
under its own key for now, hashes that photo next, and moves to the key it computed. A wrong claim so changes
nothing; a right one ends as one photo with one key on both. A link-only photo is sent without its content hash, SHA-1 or size, which are the preview's.

A photo that arrives from another device and has a SHA-1 key is looked up on the connected Immich servers by
checksum straight away (the same lookup as for a new import), so an original Immich already has is linked, not
uploaded again. A link-only photo doesn't ask other devices for its original: it comes from Immich.

## Immich collections

- **Immich ▸ Sync Immich albums** (`immich.collections {account, on}`) on a device that holds the
  account's key: each of its albums becomes a shared collection `immich:<user id>:<album id>` inside the set
  `immich:<user id>` named "Immich", owner `immich:<user id>`, grant fixed at Sync. Every device with the account
  writes the same ids, so their copies merge and a renamed album is renamed everywhere.
- Each device takes **None**, **Previews** (link-only previews from Immich) or **Full Quality** (originals from
  Immich); Full Sync isn't offered (`Replica::set_take` refuses it). A device that takes an album lists it in the
  catalog under the album folder "Immich", created on demand.
- Which photos are in an album comes from the server, on each device that holds the key: the account's albums
  are listed on a worker about once a minute (`sync.immichRefresh` at once), and what a device takes is brought in
  with the ordinary Immich import (`link` or `copy`) and `immich.fetchOriginal`. Members of Immich collections are
  never told to another device (`Replica::tells`) and their files are never fetched from one (`may_fetch`,
  `depth_from_peers`); the collection items and the photos' records (develop settings, rating, …) travel as usual.
- A photo added to an Immich collection on any device that takes it is looked up on the server by checksum and
  linked if Immich has it (the phone's own upload, another device's), else uploaded once, then put into the album.
  A photo taken out of the collection is taken out of the album. An album removed on the server is removed from
  every device's list (its photos stay).
- An album folder named "Immich" holds the account's albums; with two accounts there are two such folders.
- **Shared or private connection.** An account is private to its device unless **Share with synced devices**
  (`immich.share {account, on}`) is on. Private: its collections and their photos' records are never told
  (`Replica::private_immich`, local state). Shared: the account (URL, user id, name, pinned certificate) and its
  API key go to each device of the group in the next exchange (`Batch::accounts`, over the encrypted,
  authenticated peer channel only, once per session per device), where the key goes into that device's key
  storage (`lcc-credentials`) and the account into its settings, shared and shown as collections. The key is
  never written to the replica, journal, logs or command results. A device that already has an account for the
  same Immich user keeps its own. Turning sharing off doesn't take the key back from devices that got it:
  revoke the key in Immich for that.
- **Account identity.** The catalog's links to Immich stay under the local `<url>#<user id>` account id. Device sync
  names the account by Immich's user id alone (a UUID), so two devices that reach one server by different URLs
  (LAN and public) share its collections.

## Camera roll

- **Settings ▸ Sync ▸ This Device ▸ Sync this device's camera roll** (`sync.cameraRoll {on, folder?}`) makes the
  collection `Camera Roll - {Device Name}` (id `roll:<device>`, kind `cameraRoll`), owned by the device and shared
  like any other collection (grant None until the person allows it). It follows the device's name.
- The folder is watched: a worker takes its stamp about every 30 s; when it changed, the folder is imported in place
  (the ordinary import, which skips what the catalog has) and the collection holds every photo whose file is in the
  folder (`sync.cameraRollScan` does it now). On a desktop the folder is chosen; on Android the app's camera-roll
  watch (MediaStore listing of DCIM/Camera through `crates/android-host`) tells the folder with
  `sync.cameraRollSeen`, so no new platform code was needed.
- No duplicates with the Immich app: a roll photo is keyed by its SHA-1, as is the Immich app's upload of it. If
  Immich has it first, the checksum lookup after import links it; if the app uploads it first (added to an Immich
  collection), the Immich app's later upload is a duplicate Immich refuses; a device that sees it only through
  Immich (link-only) and a device that gets the file from a peer hold the same key, so the photo is one photo
  everywhere, and a later import of the file onto a link-only photo makes it take the file.

## Pills

Two letters and an arrow (`RF ↓`), no colour: where the original is (**L**ocal, **R**emote, **I**mmich,
**E**qual: every device), how much of it is here (**F**ull, **P**review, **N**one), and which way changes still
travel (`↑` this device is ahead, `↓` another is, none in step). The arrow pulses while an exchange moves it.
`Session::sync_pill` and `Session::sync_collection_pill` are cached per replica generation and cheap per frame.

## Exchange in brief

1. The service starts on demand (`sync.find`, or at launch when devices are paired); a session that never asks
   opens no sockets.
2. Each exchange with one device runs on its own thread: both sides say hello (app version, clock, the group,
   any invitation), then trade record batches until nothing moves, then each asks for the files it lacks
   (smart previews or originals) in 1 MiB frames into a `.part` file that a later exchange resumes.
3. The threads never touch the catalog: they ask the session, which answers from `sync.tick` (the app calls it
   once a second while sync runs, Settings open or not). The catalog keeps one writer.
4. Exchanges run about 2 s after a change here and every 20 s otherwise; **Sync Now** runs them at once.

## Files on disk

- `<settings_dir>/sync/peers.json`: this device's identity and its paired devices.
- `<catalogue>/sync/replica.json`: the replica of the catalog (group, collections, records).
- `<catalogue>/Synced/`: originals and previews that came from other devices.

## Known limits

Tracked in [gaps.md](gaps.md) (gaps 31–33).

- Virtual copies, versions and history, and keywords as a tree don't travel.
- Both devices need the same app version (the protocol is checked in the hello; a found device of another
  version can't be paired).
- Clocks must be within 5 minutes of each other, or the exchange is refused with a message.
- LAN only; mDNS is IPv4, on one interface, with no RFC 6762 probing; not tested against Apple Bonjour or
  Android NSD.
- A second import of identical bytes is the same photo, so it doesn't sync as a new one.
- Not yet tested between real devices; the tests run two devices over loopback on one machine.
