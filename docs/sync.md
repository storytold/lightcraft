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
photo yet is still understood. A photo sent under its SHA-1 key carries its content hash; a device that has
that content hash but no SHA-1 takes the SHA-1 (the same bytes) and moves to the new key, so the two never
become two photos. A link-only photo is sent without its content hash, SHA-1 or size, which are the preview's.

A photo that arrives from another device and has a SHA-1 key is looked up on the connected Immich servers by
checksum straight away (the same lookup as for a new import), so an original Immich already has is linked, not
uploaded again. A link-only photo doesn't ask other devices for its original: it comes from Immich.

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
- Next stages: Immich collections (owner `immich:`) and the Android camera roll as collections.
