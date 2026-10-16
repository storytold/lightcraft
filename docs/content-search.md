# Search by content

LightCraft finds photos by what they show: type `beach at sunset` or `red car` and get the photos that match, with no
keywords needed. It runs a content model (an image and text embedding model) on this computer; no photo, embedding or
search ever leaves it.

In short: pick a model in Settings ▸ Search (downloaded once, behind its licence), switch on **Index photo contents**,
and LightCraft embeds every photo of the library in the background. Then **search by contents from the search bar**
(its Contents mode, or `contents:` words in any search), mixed with every other filter and sorted by Relevance; **find
photos with similar contents** from any photo; and use **auto-tags** from a vocabulary of about 1,500 things a photo can
show: the Library Filter's Contents column, suggestions in the Keywording panel, and a Smart Collection criterion.
Two model families work behind the same interface: Google's SigLIP 2 (the default) and OpenAI's CLIP.

## Models

A content model is a folder of files described by a manifest (`content-model.json`, see `crates/vision/src/manifest.rs`):
what it is, its licence, every file pinned by size and SHA-256, and how it wants its input. Models are interchangeable
behind it. LightCraft knows four, in two families:

| Id | Model | Licence | Size | Settings ▸ Search says | Indexing (per photo, vs Base) |
|---|---|---|---|---|---|
| `siglip2-b16-256` | [SigLIP 2 Base, patch 16, 256 px](https://huggingface.co/google/siglip2-base-patch16-256) (Google) | Apache-2.0 | 1.5 GB | Base · fast · any language (recommended) | 141–167 ms (1×) |
| `siglip2-b16-384` | [SigLIP 2 Base, patch 16, 384 px](https://huggingface.co/google/siglip2-base-patch16-384) (Google) | Apache-2.0 | 1.5 GB | Base · finer detail, ~2.4× slower · any language | not measured (~2.4× the work) |
| `siglip2-so400m-16-384` | [SigLIP 2 So400m, patch 16, 384 px](https://huggingface.co/google/siglip2-so400m-patch16-384) (Google) | Apache-2.0 | 4.6 GB | Large · best search results, ~8× slower · any language | 1,175–1,179 ms (8×) |
| `clip-vit-b32-openai` | [CLIP ViT-B/32](https://huggingface.co/openai/clip-vit-base-patch32) (OpenAI) | MIT | 0.6 GB | Small · fastest, ~2× faster · English only | 33–101 ms (~0.5×) |

Indexing times are end to end (reading the picture included) at Normal pace on a Ryzen 9 9950X (16 cores, CPU only,
load average 11–20 from other work), on the same two scratch libraries (352 mixed JPEG/raw photos, 235 Sony raws with
embedded previews).

- **SigLIP 2** (`crates/vision/src/siglip.rs`): a ViT image tower with an attention-pooling head, a text tower with
  Gemma's SentencePiece tokenizer (256k tokens, 64-token context). Trained with a sigmoid loss, so its score is a match
  probability. The text tower is multilingual: searches can be typed in the UI languages.
- **CLIP** (`crates/vision/src/clip.rs`): a ViT-B/32 image tower (224 px, centre crop, class-token pooling and a
  projection), a 12-layer causal text tower with CLIP's byte-level BPE tokenizer (`vocab.json` + `merges.txt`, 77
  tokens, pooled at the end-of-text token). English only. The tokenizer is shared with SAM 3's text encoder
  (`crates/segment`). Trained with a softmax over a batch, CLIP has a learned scale (100) but no bias, so its scaled
  cosine only ranks; LightCraft gives it a calibrated offset (`score.offset` in the manifest, −31 for ViT-B/32) and
  passes it through the same sigmoid, so a cut-off, a vocabulary tag's own cut-off and a Smart Collection's `min` read
  the same way for both families (see *Cut-offs, measured*).
- The ports agree with the Hugging Face `transformers` reference (fp32): SigLIP 2 token ids exact on 22 cases, image
  embeddings cosine ≥ 0.999999, text 1.000000; CLIP token ids exact on 27 cases (accents, emoji, Cyrillic, Japanese,
  contractions, the markers written out, truncation at 77), image embeddings ≥ 0.999999, text 1.000000, cosines within
  4e-7 (`tools/siglip_reference.py`, `tools/clip_reference.py`, `crates/vision/tests/`). CLIP's tokenizer does not
  compose accents first (Unicode NFC); typed text already is.
- **Which to choose:** SigLIP 2 Base for most libraries; So400m when finding more matters more than time (it
  finds far more in searches, at about 8 times the indexing time, but tags no better with the built-in vocabulary); CLIP when indexing time or download size matters most and searches are in English: it finds clearly less
  (tags: precision 0.56, recall 0.52 vs Base's 0.74 / 0.69).

The weights are not part of LightCraft and are never bundled: each is an opt-in download. CLIP's file is the
`model.safetensors` of the `openai/clip-vit-base-patch32` repository's safetensors conversion (revision `c237dc49…`,
the same tensors as its `pytorch_model.bin`), checked by size and SHA-256 like the others; the weights are OpenAI's,
released under the MIT licence with [their code](https://github.com/openai/CLIP).

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
- `settings.json`: `{"enabled": false, "model": "siglip2-b16-256", "pace": "low", "tagParent": "Contents",
  "tagThreshold": 0.02}` (`pace`, `tagParent` and `tagThreshold` only when changed).
- `vocabulary.txt` (optional): your own auto-tag vocabulary (see *Auto-tags*); without it the built-in one is used.
- `content-vocab-<model id>.bin`: the vocabulary's text embeddings for that model (see *Auto-tags*).
- `catalog.json` (optional): `{"models": [<manifest>, …]}`, more models to offer; they download only from your mirrors.

Folders and manifests are treated as hostile input: ids are checked before they become folder names, manifests are
size-capped and validated, a folder whose files are missing or the wrong size is shown as *damaged* and never used,
and nothing outside a model's own folder is ever deleted. A model removed while in use hands over to another installed
one, or switches indexing off.

## Self-test

**Test** (`content.models.test`) loads the model and embeds three procedurally generated scenes (a beach, a misty
forest, a flower close-up from `lightcraft-scenes`) and three descriptions; each scene must match its own description
best. It records the load time and the time per photo and per text in `installed.json`. On a Ryzen 9 9950X (CPU):
SigLIP 2 Base 256 px loads in ~0.6 s, ~200–220 ms per photo, ~50–75 ms per text; CLIP ViT-B/32 loads in 0.1 s, 44 ms
per photo, 5 ms per text; So400m loads in 2.4 s, ~3.8 s per photo and ~290 ms per text (measured under load).

## Indexing

With **Index photo contents** on (Settings ▸ Search, `content.enable {on: true}`), every photo of the library is run
once through the model's image tower and its embedding (768 numbers for the base models) is kept in the library's
index. It runs in the background:

- **What is embedded:** the unedited photo, upright and uncropped, so edits never make a photo be indexed again. A raw's
  embedded JPEG is used when its long edge is at least 256 px (no raw decode); anything else is rendered with default
  settings at 512 px. Videos are not indexed.
- **One model, one thread:** the image tower (about 400 MB) is loaded once, on its own thread; photos go through it in
  batches of 8, 16 or 32 (by pace). The pictures of a batch are read in parallel, on the same threads, but at most 4
  embedded previews and 1 rendered photo at a time, whatever the pace: a render can hold a whole file and a full-size
  decode. Raws with a full-size JPEG and a small one (a Sony α1: 8640 × 5760 and 1616 × 1080) are read from the
  smallest that covers 512 px, and big TIFFs (a layered Photoshop TIFF can be gigabytes) are reduced strip by strip as
  they are read. Indexing 502 Sony α1 raws (and one 2.3 GB TIFF) peaks at about 1.4 GB of memory.
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
- **Model switch:** choosing another model stops the scan (the photos still waiting in the batch it was on are
  skipped, so it stops after the photo being read) and indexing goes on with that model's own index; the old index
  stays in its file, so switching back costs nothing. The vocabulary's embeddings are kept per model too. Settings ▸
  Search shows, for each model, how many photos of the open library its index holds. A model that does not load stops indexing with the reason
  in Settings ▸ Search (`error` in the status); it is tried again when indexing is switched on again, never in a loop.

Measured on a Ryzen 9 9950X (16 cores, CPU only): SigLIP 2 Base 256 px roughly 140–300 ms per photo end to end, so a
library of 10,000 photos takes about half an hour to an hour, once; CLIP about half that; So400m 8 times as
long (see *Models*).

## The index file

`content-embeddings-<model id>.bin` in the library folder, one per model, rebuildable at any time (**Clear Index**,
`content.clearIndex`); it is not part of the catalog, its snapshot or its log, and nothing of it is written to XMP. A
library that is not on disk keeps its index in memory only.

- Layout (little-endian): `"LCCE"`, version `u8` (1), tag length `u16`, the tag `<model id>@<SHA-256 of the
  weights>`, dimension `u32`, element type `u8` (1 = f16); then one record per photo: photo id `u64`, the photo's
  content hash (16 bytes: a digest of its file's hash) and the embedding as `dim` f16 values. 1,560 bytes per photo
  for the base models (768 numbers), 2,328 for So400m (1152), 1,048 for CLIP (512): 80,000 photos take 84–186 MB, on
  disk and in memory.
- Records are appended while the scan runs (every few seconds, and when the library closes or the app quits). A record
  cut short by a crash is ignored, and the file is cut back to its last whole record before anything else is appended.
- A photo indexed again or deleted leaves a dead record; when more than a fifth of the records are dead, the file is
  rewritten without them (to `.part`, then renamed, so a crash never leaves half a file).
- The file is untrusted when read: another model's or another version's file, a damaged header, a dimension over 4096,
  vectors that are not numbers or a size claiming more records than memory allows (3 GB of vectors, 4 million photos)
  are never trusted, and the index is rebuilt. A full disk or a read-only folder leaves the file as it was, keeps the
  embeddings in memory (searches work), and says so in Settings ▸ Search (`index.saveError`).

## Searching

### From the search bar

- **Text / Contents.** The magnifier at the left of the search field opens a menu: **Search Text** (file names,
  keywords, metadata, fielded words as before) or **Search Contents**. In Contents mode the field reads *Search photo
  contents (e.g. beach at sunset)* and what you type is one phrase, searched a quarter of a second after you stop
  typing (or at once with Return). Choosing Contents starts loading the model's text tower in the background, so the
  first search does not wait for it; while it loads the chip says *Preparing search…* and the window keeps drawing.
  Searches typed meanwhile run as soon as it is ready (only the last one).
- **`contents:` words, in any mode.** `contents:scorpion`, `contents:"red car"` (or `shows:`) in the ordinary search
  mix with every other word and filter: `rating:>=3 contents:beach`, `camera:x100v shows:"a dog"`. Several are joined
  into one phrase.
- **The result** is the photos that match, ANDed with everything else in view (the source, the filter bar, ratings,
  dates, folders, albums, keywords). Photos match when the model's score reaches its cut-off (`matchThreshold` in the
  model's manifest: 0.02 for SigLIP 2 Base, 0.003 for So400m, 0.005 for CLIP; see *Cut-offs, measured*); when
  none does, the best 200 are shown and the chip says *Best 200 matches*.
- **The chip** above the grid: *Contents: beach* with ✕ (ends the search, the field is cleared, the sort goes back),
  **Fewer** and **More** (each halves or doubles the cut-off, or the number of best matches; Fewer never goes down to
  nothing), and how many photos matched. Hovering a photo shows a thin match bar against the best match and its score
  (*match 0.08*).
- **Hints.** While the library is still being indexed: *Indexed 41 % of photos* (the search covers what is indexed).
  With no model installed, or indexing off and the library not all indexed: *Turn on Index photo contents in Settings →
  Search* and an **Open Settings…** button.
- **Relevance** (View ▸ Sort, the sort menu): best match first. A search by contents switches to it; the other sorts
  can still be chosen while it is on, and ending the search gives back the sort from before. Relevance is offered only
  while searching by contents.
- **Find Similar Contents** (the grid's and the loupe's context menu): the photos whose contents are most like this
  photo's (the closest 100 embeddings, the photo itself first), as a ranked view with the chip *Similar to IMG_0001.JPG*
  (More / Fewer: twice or half as many). It complements **Find Similar Photos**, which compares composition and tones.
- **When it cannot run** (no model, the model removed or switched while a search is in view, a photo deleted, a text
  the model cannot read) the grid shows nothing for it and the chip says why: never a ranking left over from another
  model. Blank searches are no search; long ones are cut to 500 characters (SigLIP reads 64 tokens, CLIP 77).

### How it works

The engine resolves the content part of the view (`Session::content_query`, `crates/engine/src/content_query.rs`):
the text is embedded by the model's text tower on a thread of its own, every indexed photo is scored against it (a few
milliseconds for 80,000 photos), and the photos that pass go to the catalog as ids with scores
(`Filter::ranked`), which it ANDs with the rest of the filter like `only` and sorts by with `SortKey::Relevance`. The
catalog never sees the index. The ranking is worked out again when the index grows, the model changes, or More / Fewer
is pressed; texts already searched are remembered (64 of them) while the model stays loaded.

For SigLIP the score is the model's match probability (a sigmoid of the scaled cosine plus the model's bias): on the
base model the right description of a photo typically scores 0.03–0.4 and unrelated ones well under 0.01. For CLIP it is
the scaled cosine plus the calibrated offset through the same sigmoid (`sigmoid(100 · cos − 31)`): it ranks exactly as
the cosine does, and its cut-offs were chosen on the same photos.

Latency (Ryzen 9 9950X, CPU): the text tower loads in about half a second, once; then a new search embeds the text in
30–70 ms and scores the index in a few ms. Under heavy load from other work (load average ~75) a new search took
0.6–1.2 s and More / Fewer 20–30 ms.

### For agents and scripts

`library.filter {contents: "beach at sunset"}` (or `{text: "contents:beach rating:>=3"}`) does what the search bar
does and answers `{count, contents: {state, label, matches, bestMatches, step, canMore, canFewer, min, indexed, total,
hint}}`; it waits for the ranking unless `wait: false`. `content.more` / `content.fewer` / `content.clearQuery` /
`content.query` act on it, `content.similar {id, show: true}` shows a photo's look-alikes. `content.search {text}`
returns the ranking itself (`[{id, score}]`, any `min`, `limit`) without touching the view; `catalog.query {filter}`
does not resolve `contents:` words (use `library.filter`, then `catalog.query` without a filter for the view).
`content.search` / `content.similar` work while the index is incomplete; their answer says how many photos were
searched (`indexed` of `total`, `complete`).

## Auto-tags

Every indexed photo gets tags from a **vocabulary** of things a photo can show: about 1,500 hierarchical tags
(`scene|beach`, `nature|rocks`, `plant|flower|tulip`, `people|crowd`, `animal|bird|heron`, `animal|arachnid|scorpion`,
`vehicle|car|camper van`, `object|kitchen|teapot`, `event|wedding`, `time|sunset`, `weather|snow`, `food|pizza`,
`architecture|bridge`, `style|aerial view`…). The list (`crates/vision/vocab/en.txt`) is original work written for
LightCraft, not taken from any published label set. Tags are suggestions: nothing is written to a photo until you accept
one.

### Where you see them

- **Library Filter ▸ Contents** (the filter bar, Shift+F): the library's tags with their photo counts, each level above
  its tags (`animal` counts every bird, dog and insect). Picking one shows its photos, ANDed with every other filter; a
  chip says *Contents: heron* and its ✕ ends it (`library.filter {contentTag: "animal|bird"}`).
- **Keywording ▸ Suggested from contents** (the Keywords panel): the tags of the selected photos that are not keywords
  yet, most photos first. Click one to make it a keyword of the photos it is suggested for (one undo step, however many
  photos); its ✕ dismisses it for those photos for good. A tag suggested for only some of the selection is marked `*`.
- **Smart Collections**: the *Contents* criterion (its own group in the field menu): *shows* a text (`red car`; the
  model's search cut-off unless the rule gives `{text, min}`), *is tagged* / *isn't tagged* with a tag (`animal|bird`).
  Without an index (no model, nothing indexed yet) these rules match nothing, and the editor says so.

### Accepting a tag

An accepted tag becomes an ordinary keyword: `Contents|<its name>` (`Contents|heron`), so accepted tags stay together in
the Keyword List and can be removed in one go. The parent is a setting (Settings ▸ Search ▸ *Accepted tags go under the
keyword*; empty: the tag's name alone). Like any keyword it is written to XMP and exported. A photo that already has a
keyword of that name (`heron`, or `Birds|heron`) is not suggested the tag again.

### How tags are worked out

- The vocabulary's phrases are embedded by the model's text tower once per model, in the background (an activity row,
  *Preparing auto-tags*, cancellable), each in the sentence "this is a photo of {}." A tag with synonyms
  (`animal|mammal|dog = dog, puppy`) gets the mean of their embeddings. The embeddings are cached in
  `content-vocab-<model id>.bin` in the content models folder: they belong to the model and the vocabulary, not to a
  library, so every library and every later start reuses them, and an edited vocabulary only embeds the tags that
  changed. A cache made by another model or version, or a damaged one, is rebuilt.
- Every indexed photo is scored against every tag (a matrix product over the index; 80,000 photos × 1,500 tags take
  about 0.35 s on a busy 16-core desktop processor, a 500-photo library about 10 ms), on a thread of its own, after
  each batch the scan adds and when the vocabulary or the model changes. Each photo keeps its 16 best scores in memory;
  nothing is stored. Once the vocabulary is cached, no model needs to be loaded for this.
- A photo shows the tags whose score reaches the cut-off, at most 8. The cut-off is the model's (0.01 for SigLIP 2
  Base and CLIP, 0.007 for So400m, chosen below) or yours (Settings ▸ Search ▸ Advanced ▸ *Tag cut-off*: lower shows more tags and more wrong ones).
  Moving it applies at once. Specific tags carry their own, higher cut-off in the vocabulary (bird species, dog breeds,
  people's roles: `@0.1`, `@0.05`); one that reaches the model's cut-off but not its own counts for the tag above it, so
  a photo the model thinks shows a spaniel is still tagged *dog*.
- Dismissed suggestions are remembered per library in `content-dismissed.bin` beside the catalog (photo and tag); a
  damaged file is ignored and written again with the next dismissal.

### The vocabulary

Settings ▸ Search ▸ *Vocabulary* edits it as text, one tag per line (`content.setVocabulary` does the same):

```text
# a comment
animal|bird|owl                  described by its last level ("owl")
animal|dog = dog, puppy          synonyms: one tag, their embeddings averaged; typing any of them finds it
scene|beach = beach @0.03        its own cut-off
```

Levels above a tag need no line of their own. A vocabulary has at most 5,000 tags of at most 6 levels and 120
characters, 8 phrases per tag; lines that can't be read (too long, repeated, empty) are skipped and reported. Your
vocabulary is kept in `vocabulary.txt` in the content models folder (for every library); *Restore Defaults* goes back to
the built-in one. Non-English terms work with SigLIP 2's multilingual text tower, not with CLIP (English only).

### Cut-offs, measured

**Auto-tags.** On 343 labelled photos (319 real photos of three scratch libraries, labelled by hand, and the 24
procedural demo scenes), the built-in vocabulary, at most 8 tags per photo. SigLIP 2 Base (256 px):

| Cut-off | Precision | Recall | Scenes P / R | Animals P / R | Objects P / R | People P / R | Vehicles P / R |
|---|---|---|---|---|---|---|---|
| 0.005 | 0.69 | 0.78 | 0.73 / 0.74 | 0.70 / 0.95 | 0.53 / 0.74 | 0.44 / 0.50 | 0.52 / 0.83 |
| **0.01** | **0.74** | **0.69** | 0.77 / 0.64 | 0.74 / 0.90 | 0.61 / 0.68 | 0.70 / 0.50 | 0.60 / 0.83 |
| 0.02 | 0.79 | 0.58 | 0.82 / 0.53 | 0.78 / 0.84 | 0.66 / 0.45 | 0.71 / 0.50 | 0.75 / 0.33 |
| 0.05 | 0.83 | 0.40 | 0.86 / 0.33 | 0.85 / 0.78 | 0.69 / 0.26 | 0.50 / 0.17 | — |

Precision: the share of shown tags that are right (a level above a right tag counts); recall: the share of the photos'
main subjects found (a tag below one counts). Scenes include nature, time of day, weather and light; objects include
buildings, rooms and things. People and vehicles had only 6 labelled subjects each, so their numbers are rough. Most
wrong tags are near misses (a cottage for a house seen from above, the wrong small bird), which is why species carry
their own cut-off. Chosen: 0.01 (suggestions are confirmed by the user, so recall matters).

The same photos and method for the other models (the chosen cut-off in bold):

| Model | Cut-off | Precision | Recall | Scenes P / R | Animals P / R | Objects P / R | People P / R | Vehicles P / R |
|---|---|---|---|---|---|---|---|---|
| SigLIP 2 So400m | 0.005 | 0.66 | 0.70 | 0.69 / 0.67 | 0.66 / 0.94 | 0.50 / 0.43 | 0.64 / 0.50 | 0.48 / 0.83 |
| SigLIP 2 So400m | **0.007** | **0.68** | **0.67** | 0.71 / 0.65 | 0.68 / 0.92 | 0.53 / 0.36 | 0.67 / 0.50 | 0.46 / 0.83 |
| SigLIP 2 So400m | 0.01 | 0.71 | 0.63 | 0.74 / 0.61 | 0.68 / 0.85 | 0.60 / 0.32 | 0.78 / 0.50 | 0.37 / 0.83 |
| CLIP ViT-B/32 (offset −30) | 0.01 | 0.52 | 0.64 | 0.63 / 0.56 | 0.49 / 0.94 | 0.32 / 0.68 | 0.14 / 0.00 | 0.25 / 1.00 |
| CLIP ViT-B/32 (offset −31) | **0.01** | **0.56** | **0.52** | 0.65 / 0.43 | 0.58 / 0.87 | 0.36 / 0.60 | 0.12 / 0.00 | 0.30 / 0.67 |
| CLIP ViT-B/32 (offset −32) | 0.01 | 0.64 | 0.37 | 0.70 / 0.28 | 0.72 / 0.68 | 0.41 / 0.45 | 0.14 / 0.00 | 0.40 / 0.50 |

So400m's scores run lower than Base's, so its tag cut-off is lower too; with this vocabulary (whose own cut-offs were
set on Base) it tags no better than Base, while it finds far more in searches (below). CLIP's offset was chosen where
its precision and recall meet, and put at the score SigLIP 2 Base's cut-off has (0.01),
so the vocabulary's own cut-offs mean as much more for both. CLIP tags clearly worse than SigLIP 2: more near misses
(objects and scenes), no person found among the few labelled.

**Searches.** 30 searches typed as a person would (*a bird*, *the night sky*, *a construction site*, *a dog*, *a
kitchen*, *mountains*, *a camper van*…) over the 319 labelled real photos; a shown photo is right when its labels
include what was searched for (or a near synonym), recall counts the photos labelled with it:

| Model | Cut-off | Precision | Recall | Photos shown |
|---|---|---|---|---|
| SigLIP 2 Base | 0.01 | 0.93 | 0.58 | 317 |
| SigLIP 2 Base | **0.02** | **0.94** | **0.44** | 230 |
| SigLIP 2 So400m | 0.002 | 0.93 | 0.76 | 453 |
| SigLIP 2 So400m | **0.003** | **0.94** | **0.70** | 405 |
| SigLIP 2 So400m | 0.005 | 0.94 | 0.60 | 339 |
| CLIP ViT-B/32 | 0.002 | 0.86 | 0.55 | 355 |
| CLIP ViT-B/32 | **0.005** | **0.90** | **0.38** | 223 |
| CLIP ViT-B/32 | 0.01 | 0.92 | 0.25 | 143 |

A model installed before its cut-offs changed picks the new ones up: a known model's cut-offs, offset and hint come
from the version of LightCraft in use, not from its installed manifest. SigLIP 2 Base at 384 px uses the 256 px
model's cut-offs (not measured: it was not downloaded for this).

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
| `content.similar` | `{id, limit?: 50, min?, show?}` | the photos whose contents are most like this one's; `show: true` ranks the view by them (Find Similar Contents) |
| `library.filter` | `{contents?: text \| null, text?: "… contents:…", wait?: true, …}` | search the view by contents (see *Searching*): `{count, contents}` |
| `content.prepare` | | start loading the text tower (the Contents mode does) → `{state: ready \| loading \| noModel \| error}` |
| `content.more` / `content.fewer` | | widen / narrow the view's search by contents one step → `{count, changed, contents}` |
| `content.clearQuery` | | end the view's search by contents (the Contents text, `contents:` words, Find Similar Contents) |
| `content.query` | | the view's search by contents as its chip shows it, or `null` |
| `content.reindex` | `{ids?}` | embed these photos (or all) again, failed ones included |
| `content.tags` | `{id? \| ids?, wait?, min?}` | the photos' auto-tags `[{tag, label, score, accepted}]` (`min`: every kept score instead, to choose a cut-off) |
| `content.tagCounts` | | every tag of the library with its photo count (levels included): the Contents column |
| `content.acceptTags` | `{ids?, tags, all?}` | make tags keywords (`Contents\|<name>`) of the photos they are suggested for (`all`: of every photo given), one undo step |
| `content.dismissTags` | `{ids?, tags}` | never suggest these tags for these photos again |
| `content.vocabulary` | `{text?}` | the vocabulary in use: `{custom, count, notes, state, embedded, terms, text?}` |
| `content.setVocabulary` | `{text? \| terms? \| path? \| reset?}` | replace your vocabulary (or restore the built-in one) |
| `content.tagOptions` | `{parent?, threshold?}` | the keyword parent of accepted tags and the tags' cut-off (`null`: the default) |
| `library.filter` | `{contentTag: tag \| null}` | the Contents filter: the photos tagged with it (levels below included) |
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

- The web build has no content search (the towers run natively only, on candle).
- The vocabulary is English only (other languages can be added as user vocabularies; with SigLIP 2 they work).
- No GPU on Linux and Windows (candle's CPU kernels; Metal on macOS): the large model is slow there.
- The cut-offs were measured on one person's 343 photos; people and vehicles were few. SigLIP 2 Base 384 px is not
  calibrated.
- CLIP's tokenizer skips Unicode NFC (accents typed decomposed tokenize differently).
