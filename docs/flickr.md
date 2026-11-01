# Sharing to Flickr

LightCraft shares chosen photos to a Flickr account (File ▸ Share ▸ Flickr…, or the share button in the top bar),
rendered with their edits. It is a one-way upload for
showcasing work, not a publish service: LightCraft doesn't remember what it uploaded, so an edited photo isn't
republished and nothing on Flickr is changed or deleted afterwards (see `LRC-LIB-PUBLISH` in
[parity-checklist.md](parity-checklist.md)).

## For photographers

1. **An API key.** Flickr gives every app that talks to it a key and a secret. Official builds can carry LightCraft's
   own; otherwise **Settings ▸ Accounts** walks you through it: *Open Flickr's API Key Page* opens
   <https://www.flickr.com/services/apps/create/noncommercial/> (any app name will do), then paste the key and its
   secret and click **Continue**.
2. **Sign in.** Continue goes straight on: Flickr's approval page opens in your browser. Approve LightCraft (it asks
   to upload, nothing more), then paste the code Flickr shows (like `123-456-789`) into the Code field; it is sent as
   soon as it is complete. LightCraft never sees your Flickr password. While an upload runs, the sign-in can't be changed.
3. **Upload.** Select photos, then **File ▸ Share ▸ Flickr…**:
   - **Size**: 1024, 2048 (default) or 4096 px on the long edge, or full size; never enlarged.
   - **Quality**: JPEG quality, 90 by default.
   - **Visibility**: public, friends, family, friends & family, or only you.
   - **Keywords as tags**, **caption as description** (both on by default). Tags are the keywords the exported file
     carries: a keyword whose *Include on Export* is off is never sent, and a nested keyword goes by its own name
     (with its containing keywords and synonyms when their options say so). The title is the photo's title, or its
     file name without the extension.
   - **Remove location info** (on by default): GPS is left out of the file, so the photo isn't placed on Flickr's map.
     Turn it off to share where it was taken.
   - **Hide from public searches**.
   - **Album**: one of your Flickr albums to put the photos in, **New Album…** to make one (type its name), or None
     (the default every time the dialog opens). The list is read from Flickr when the dialog opens. Flickr makes an
     album around one of your photos, so a new album is made right after the first photo goes up, with that photo as
     its cover, and the others join it. A photo that uploads but can't be added to the album still counts as
     uploaded; the toast says how many weren't added, or why the new album couldn't be made.

   The upload runs in the background with a row in the activity stack (✕ stops it after the current photo). A toast
   says how many went up. Photos are rendered like an export (sRGB JPEG, sharpened for screen), so they look as
   they do in LightCraft.

The sign-in is kept on this computer, in `flickr.json` in LightCraft's settings folder (Windows
`%APPDATA%\LightCraft`, macOS `~/Library/Application Support/LightCraft`, Linux `~/.config/lightcraft`), not in a
library. On Unix the file is readable by you only. Sign Out forgets it; Forget Key removes your API key. A
`--memory` session keeps a sign-in only until it quits. If Flickr stops accepting the sign-in (you revoked
LightCraft's access on Flickr, or the key changed), the upload stops and asks you to sign in again.

Free Flickr accounts hold a limited number of photos; Flickr refuses uploads past it (error 6).

## For agents and scripts

The same commands run in the app (through the control channel, in the background), in `lightcraft-cli run` and
over MCP (`lightcraft-cli mcp`, synchronously):

| Command | Params | Result |
|---|---|---|
| `flickr.status` | — | `{hasKey, ownKey, builtinKey, signedIn, username, fullname, userNsid, pending, saved, problem?, lastUpload?}` |
| `flickr.setApiKey` | `{key, secret}` (both empty: the build's key) | status; a different key signs out |
| `flickr.signIn` | — | `{authorizeUrl}`: open it, approve, then… |
| `flickr.completeSignIn` | `{code}` | status, signed in and saved |
| `flickr.cancelSignIn` / `flickr.signOut` | — | status |
| `flickr.albums` | — | `{albums: [{id, title, photos}]}`, as arranged on Flickr |
| `flickr.upload` | `{ids?, longEdge?, quality?, privacy?, hidden?, tags?, description?, removeLocation?, album?, newAlbum?}` (`album` or `newAlbum`, not both) | `{total, uploaded, failed, albumFailed, cancelled, signInNeeded, album, createdAlbum?, albumCreateError?, photos: [{photo, file, photoId, url, albumError?} \| {photo, file, error}]}` |

At most 500 photos per upload. In the app, `flickr.upload` returns `{background: true, total}` at once; poll
`ui.inspect` → `flickr` (or `activity.list`, kind `upload`). `flickr.albums` likewise returns `{started: true}` and
reads the list on a worker thread; it appears in `ui.inspect` → `flickr.albums` (`albumsLoading`, `albumsError`).

## For maintainers

- `crates/flickr` (`lightcraft-flickr`, L1): OAuth 1.0a request signing (HMAC-SHA1, RustCrypto `hmac` + `sha1`),
  Flickr's out-of-band sign-in, signed `multipart/form-data` uploads to `up.flickr.com` (every field but the photo
  signed), retries only when Flickr certainly didn't take the request (no connection, HTTP 429 / 503, Flickr 105 / 106; a lost
  answer after the photo was sent is reported, never retried, so a retry can't upload a photo twice), the account file. It
  speaks HTTPS through `lightcraft-fetch` (pure Rust rustls), so there is no C. Tests run against a local stand-in
  that checks each signature; the signer is also checked against the OAuth specification's worked example.
- `crates/engine/src/flickr.rs`: the session's account, `prepare_upload` (catalog → export jobs + title, caption,
  keywords) and `UploadJob::run` (render, upload; runs without the session). Commands in `cmd/flickr.rs`.
- `crates/ui-egui/src/flickr.rs`: the app runs the network steps on worker threads (`tasks::spawn` for sign-in,
  an upload task with an activity row); `panels/flickr.rs` is the Settings tab.
- **A built-in API key** is compiled in when `LIGHTCRAFT_FLICKR_API_KEY` and `LIGHTCRAFT_FLICKR_API_SECRET` are set
  at build time (e.g. as release-workflow secrets). Without them, users enter their own. A key's secret in a
  desktop binary can be extracted; Flickr's terms for non-commercial keys apply to whoever registers it.
- Albums are Flickr photosets: `flickr.photosets.getList` (signed `GET` to `api.flickr.com/services/rest/`, 500 per
  page), `flickr.photosets.addPhoto` (signed form `POST`) after each upload ("already in the album" is fine) and
  `flickr.photosets.create`, which needs a primary photo the account owns: it is called once, after the first upload.
- Not done: publish collections (remember what was uploaded, republish edited photos, delete on Flickr), other
  services, a localhost callback instead of copying the code.
