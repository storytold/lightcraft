# Search by content

LightCraft is learning to find photos by what they show: type `beach at sunset` or `red car` and get the photos that
match, with no keywords needed. It runs a content model (an image and text embedding model) on this computer; no photo,
embedding or search ever leaves it.

**Where it stands (phase 2 of 6):** the models and their download are done: Settings ▸ Search lists them, downloads
one behind its licence, checks it and tests it. Indexing photos and searching them come next; the **Index photo
contents** switch is stored but nothing indexes yet.

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
- `settings.json`: `{"enabled": false, "model": "siglip2-b16-256"}`.
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

## Privacy

Everything runs on this computer. The model is downloaded once from Hugging Face (or your mirror); after that nothing
is sent anywhere: not photos, not embeddings, not what you search for.

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
| `content.enable` | `{on}` | the Index photo contents setting (needs a model in use) |
| `content.status` | | the model in use, whether indexing is on; `indexed` / `total` / `stale` / `indexing` come with the index |

```text
lightcraft-cli run --demo content.models.install path=~/.config/lightcraft/models/content/siglip2-b16-256 acknowledged=true \
  content.models.test id=siglip2-b16-256 wait=true content.status
```

## Not yet

Indexing the library (phase 3), the Contents search mode and `contents:` field (phase 4), auto-tags (phase 5), and a
second model family, CLIP (phase 6). The web build has no content search (the towers run natively only).
