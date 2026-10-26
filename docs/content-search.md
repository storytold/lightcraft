# Search by content

LightCraft is learning to find photos by what they show: type `beach at sunset` or `red car` and get the photos that
match, with no keywords needed. It runs a content model (an image and text embedding model) on this computer; no photo,
embedding or search ever leaves it.

**Where it stands (phase 3 of 6):** the models and their download are done (Settings ▸ Search lists them, downloads
one behind its licence, checks it and tests it), and so is the index: with **Index photo contents** on, LightCraft
embeds every photo of the library in the background and `content.search` / `content.similar` rank photos by what they
show, from the CLI, the control channel and MCP. The search bar's Contents mode comes next (phase 4).

## Models

A content model is a folder of files described by a manifest (`content-model.json`, see `crates/vision/src/manifest.rs`):
what it is, its licence, every file pinned by size and SHA-256, and how it wants its input. Models are interchangeable
behind it. LightCraft knows three, all Google's SigLIP 2 under the **Apache License 2.0** (any use, commercial included):

| Id | Model | Size | Notes |
|---|---|---|---|
| `siglip2-b16-256` | [SigLIP 2 Base, patch 16, 256 px](https://huggingface.co/google/siglip2-base-patch16-256) | 1.5 GB | the default; ~200 ms per photo on a 16-core CPU |
| `siglip2-b16-384` | [SigLIP 2 Base, patch 16, 384 px](https://huggingface.co/google/siglip2-base-patch16-384) | 1.5 GB | finer detail, about 2.4× the work per photo |
| `siglip2-so400m-16-384` | [SigLIP 2 So400m, patch 16, 384 px](https://huggingface.co/google/siglip2-so400m-patch16-384) | 4.6 GB | the large model (1152-dim embeddings), several times slower |

All three run on the same towers (`crates/vision`); the text tower is multilingual, so searches can be typed in the UI
languages. The weights are not part of LightCraft and are never bundled: each is an opt-in download.

## Download and licence

Settings ▸ Search ▸ **Download** shows the model's terms first: its licence, who made it, links to the licence and the
model's page, and an **I have read these terms and accept them** box. Nothing is fetched until it is ticked. Then:

- The files come from **Hugging Face**, at the repository revision the built-in manifest pins (e.g.
  `https://huggingface.co/google/siglip2-base-patch16-256/resolve/3f9f96cb…/model.safetensors`), over LightCraft's
  pure-Rust HTTPS client (`crates/fetch`). No token, no account.
- Every file is written as `<name>.part`, checked against its exact size and SHA-256, and only then given its name. A
  file that does not match is deleted.
- The download shows in the activity stack (bytes of total) and in the model's row; ✕ or **Cancel** stops it and
  deletes everything it fetched. A download that fails (no network, a server error) keeps its partial files, so **Try
  Again** resumes; clearing the failure (`content.models.downloadCancel`) deletes them.
- Once every file checks out, the model is installed (`installed.json` records when the terms were accepted) and, when
  no other model is in use, chosen.
- One model job (download, check or test) runs at a time.

**Mirrors.** Hugging Face is the only default source. Other places are used only when you list them: base URLs in
`LIGHTCRAFT_VISION_MIRRORS` (separated by commas, spaces or newlines) or in `content-mirrors.txt` beside the content
models folder (one per line, `#` comments). File `f` of model `id` is fetched from `<mirror>/<id>/<f>`; mirrors are
tried before Hugging Face, and their files are checked against the same sizes and SHA-256.

**Files put there by hand.** A folder `<models>/content/<id>/` holding all of a known model's files at their exact sizes
(for example from `huggingface-cli download`) shows as *found on this computer*. **Check and Use** shows the terms, then
checks every file's SHA-256 and installs it; nothing is downloaded. `content.models.install {path}` does the same for
any folder (copying it into place when it is elsewhere).

## Where things are kept

`<config>/models/content/` (`~/.config/lightcraft` on Linux, `~/Library/Application Support/LightCraft` on macOS,
`%APPDATA%\LightCraft` on Windows; `LIGHTCRAFT_CONTENT_MODELS` overrides it):

- `<id>/`: the model's files, `content-model.json` (its manifest) and `installed.json` (when its licence was accepted,
  and its last self-test with the speed measured on this computer).
- `settings.json`: `{"enabled": false, "model": "siglip2-b16-256", "pace": "low"}` (`pace` only when not normal).
- `catalog.json` (optional): `{"models": [<manifest>, …]}`, more models to offer; they download only from your mirrors.

Folders and manifests are treated as hostile input: ids are checked before they become folder names, manifests are
size-capped and validated, a folder whose files are missing or the wrong size is shown as *damaged* and never used,
and nothing outside a model's own folder is ever deleted. A model removed while in use hands over to another installed
one, or switches indexing off.

## Self-test

**Test** (`content.models.test`) loads the model and embeds three procedurally generated scenes (a beach, a misty
forest, a flower close-up from `lightcraft-scenes`) and three descriptions; each scene must match its own description
best. It records the load time and the time per photo and per text in `installed.json`. On a Ryzen 9 9950X (CPU):
SigLIP 2 Base 256 px loads in ~0.6 s, ~200–220 ms per photo, ~50–75 ms per text.

## Indexing

With **Index photo contents** on (Settings ▸ Search, `content.enable {on: true}`), every photo of the library is run
once through the model's image tower and its embedding (768 numbers for the base models) is kept in the library's
index. It runs in the background:

- **What is embedded:** the unedited photo, upright and uncropped, so edits never make a photo be indexed again. A raw's
  embedded JPEG is used when its long edge is at least 256 px (no raw decode); anything else is rendered with default
  settings at 512 px. Videos are not indexed.
- **One model, one thread:** the image tower (about 400 MB) is loaded once, on its own thread; photos go through it in
  batches of 8, 16 or 32 (by pace). The pictures of a batch are read in parallel, on the same threads.
- **Pace** (Settings ▸ Search, `content.pace`): **Low** uses a quarter of the processor's threads (at most 4),
  **Normal** half of them, **Fast** four fifths. The work runs on a thread pool of that size, separate from the one the
  window renders with; while you drag, type or scroll no new batch starts.
- **Progress:** a row in the activity stack, *Indexing photo contents*, counts indexed photos of all; Settings ▸ Search
  shows the same with the time left (from the measured speed). The row's ✕ (or `activity.cancel`) stops the scan once
  the batch it is on is done; everything embedded so far is kept. Turning the switch on again (or **Resume**) carries
  on from there. Quitting does not ask about it: what is done is saved and the rest is done next time.
- **New photos** are picked up when the catalog changes (an import, a reload); deleted ones are dropped. A photo whose
  original was replaced (Reload from Disk after the file changed) has a new content hash, so its old embedding is stale
  and it is embedded again.
- **Offline and unreadable photos:** a photo whose file is missing (a drive that is not connected) is skipped and
  tried again a minute later; one that cannot be read is skipped for the rest of the session (`failed` in the status).
  `content.reindex` tries them again at once.
- **Model switch:** choosing another model stops the scan and indexing goes on with that model's own index; the old
  index stays in its file, so switching back costs nothing. A model that does not load stops indexing with the reason
  in Settings ▸ Search (`error` in the status); it is tried again when indexing is switched on again, never in a loop.

Measured on a Ryzen 9 9950X (16 cores, CPU only), SigLIP 2 Base 256 px: see the results in the plan; roughly
150–300 ms per photo end to end, so a library of 10,000 photos takes about half an hour to an hour, once.

## The index file

`content-embeddings-<model id>.bin` in the library folder, one per model, rebuildable at any time (**Clear Index**,
`content.clearIndex`); it is not part of the catalog, its snapshot or its log, and nothing of it is written to XMP. A
library that is not on disk keeps its index in memory only.

- Layout (little-endian): `"LCCE"`, version `u8` (1), tag length `u16`, the tag `<model id>@<SHA-256 of the
  weights>`, dimension `u32`, element type `u8` (1 = f16); then one record per photo: photo id `u64`, the photo's
  content hash (16 bytes: a digest of its file's hash) and the embedding as `dim` f16 values. 1,560 bytes per photo
  for the base models: 80,000 photos take about 125 MB, on disk and in memory.
- Records are appended while the scan runs (every few seconds, and when the library closes or the app quits). A record
  cut short by a crash is ignored, and the file is cut back to its last whole record before anything else is appended.
- A photo indexed again or deleted leaves a dead record; when more than a fifth of the records are dead, the file is
  rewritten without them (to `.part`, then renamed, so a crash never leaves half a file).
- The file is untrusted when read: another model's or another version's file, a damaged header, a dimension over 4096,
  vectors that are not numbers or a size claiming more records than memory allows (3 GB of vectors, 4 million photos)
  are never trusted, and the index is rebuilt. A full disk or a read-only folder leaves the file as it was, keeps the
  embeddings in memory (searches work), and says so in Settings ▸ Search (`index.saveError`).

## Searching

`content.search {text}` embeds the text with the model's text tower (loaded on the first search, about half a second,
and unloaded after five idle minutes), scores every indexed photo against it and returns them best first. For SigLIP the
score is the model's match probability (a sigmoid of the scaled cosine plus the model's bias): on the base model the
right description of a photo typically scores 0.05–0.3 and unrelated ones well under 0.01, so `min` can cut the list.
Searches work while the index is incomplete; the answer says how many photos were searched (`indexed` of `total`,
`complete`). `content.similar {id}` ranks the other photos by how close their embeddings are to this one's (cosine).
Scoring 80,000 photos takes a few milliseconds.

## Privacy

Everything runs on this computer. The model is downloaded once from Hugging Face (or your mirror); after that nothing
is sent anywhere: not photos, not embeddings, not what you search for. The index lives in the library folder, next to
the catalog.

## Commands

Reachable from the control channel, `lightcraft-cli run` and MCP (`cmd_content_models_list`, …):

| Command | Params | Does |
|---|---|---|
| `content.models.list` | | the known and installed models: licence, publisher, size, state (`available`, `found`, `installed`, `damaged`), in use, self-test, download host, running job |
| `content.models.download` | `{id, acknowledged: true}` | download (or, for a found folder, only check) in the background; refused without `acknowledged` |
| `content.models.downloadCancel` | `{id}` | stop a download (leaves nothing), or clear a finished or failed one |
| `content.models.install` | `{path, acknowledged: true, activate?}` | add a model folder: validated, every file checked |
| `content.models.select` | `{id \| null}` | the model in use |
| `content.models.remove` | `{id}` | delete a model's folder |
| `content.models.test` | `{id, wait?}` | the self-test (in the background, or `wait: true` to get the result) |
| `content.enable` | `{on}` | the Index photo contents setting (needs a model in use); on again resumes a stopped scan |
| `content.status` | | the model in use, whether indexing is on, `indexed` of `total`, `stale`, `failed`, `offline`, `pending`, `indexing`, `stopped`, `pace`, `msPerPhoto`, `etaSeconds`, `error`, `index {file, bytes, photos, records, deadRecords, saveError}` |
| `content.pace` | `{pace: low \| normal \| fast}` | how much of the computer indexing uses |
| `content.pump` | `{pause?, wait?, timeoutMs?}` | one step of the background scan (the app calls it a few times a second); `wait: true` runs it to the end |
| `content.search` | `{text, limit?: 100, min?}` | `{results: [{id, score}], indexed, total, complete, ms}`, best first |
| `content.similar` | `{id, limit?: 50, min?}` | the photos whose contents are most like this one's |
| `content.reindex` | `{ids?}` | embed these photos (or all) again, failed ones included |
| `content.clearIndex` | | forget the index of the model in use and delete its file |

The other `content.*` commands move the scan along too, so a headless session (CLI, MCP) indexes as it is used;
`content.pump {wait: true}` indexes the whole library and returns when it is done:

```text
lightcraft-cli run --library ~/Pictures/MyLibrary content.enable on=true content.pump wait=true \
  content.search text="a dog on a beach" limit=10
lightcraft-cli run --demo content.models.install path=~/.config/lightcraft/models/content/siglip2-b16-256 acknowledged=true \
  content.models.test id=siglip2-b16-256 wait=true content.status
```

## Not yet

The Contents search mode and `contents:` field in the search bar (phase 4), auto-tags (phase 5), and a second model
family, CLIP (phase 6). The web build has no content search (the towers run natively only).
