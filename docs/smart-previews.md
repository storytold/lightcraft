# Smart previews

Smart previews are reduced sources for editing and export while originals are offline. Online originals still take precedence. A proxy does not retain the original's full resolution.

## Choose a format

Settings > Performance > Smart previews has a per-library format preference. Compact (JPEG) remains the default. Lossless (32-bit TIFF) preserves every finite float sample of the decoded proxy source. It uses substantially more disk space, about 40 to 50 MB on the two public Sony samples measured for issue #709, compared with 0.5 to 1 MB for Compact at its former JPEG quality 92 (quality 97 is about twice that).

File > Previews > Build Smart Previews offers the library format or a format for that build. A build override does not change the library preference. Settings also has Build for All Photos. Before a Lossless build starts, a dialog shows the photo count, estimated total size and free space on the destination volume. The estimate uses about 39.5 MB per photo from the measured A7CR proxy; actual sizes vary. If the estimate exceeds free space, the dialog warns and offers Build Anyway or Cancel. Builds run in the background, report progress, and can be stopped.

Build Smart Previews builds several photos at once: one lane per four cores, up to four, as many as an export batch (issue #1492). The opening look refresh uses one lane, so it does not slow the first renders. Copies of a photo share one proxy, which is written once. On a 16-core Apple M4 Max (four lanes), 24 corpus raws took 2.6 s instead of 5.8 s in Compact and 3.1 s instead of 8.3 s in Lossless. Peak memory rose from about 1.0 GB to about 2.0 GB (Compact) and 1.1 GB to 1.9 GB (Lossless). Two lanes took 3.7 s and 4.7 s.

Changing the preference does not rewrite files. A subsequent build converts existing previews from available originals. If an original is offline or unreadable, its existing preview stays unchanged. Reconnect the drive and build again to convert it. Converting a JPEG proxy cannot restore precision, so builds never use an old proxy as their source.

The displayed count and total size use the actual files in both the old flat layout and the current sharded layout. Mixed-format libraries are supported.

## Stored pixels and metadata

Both formats store the decoded, oriented, reduced scene-linear working RGB source (Rec.2020), before develop controls and the camera tone curve. The camera colour fit is already in these pixels. Tone and chroma curves remain in the JSON header and apply at render time.

Compact keeps the existing `LCSP1` container at JPEG quality 97, and its header records the encode version (`enc: 3`: version 2 raised the quality, PR #850; version 3 added the decoder-facts tail, issue #1534). A Compact proxy from an older encoding is rebuilt like one from an older camera look, for every photo, not only the raws that take the camera look. Its stored scale covers the brightest 99.95% of samples, so extreme highlights and negative values can be lost, and the higher quality retains the 8-bit limit.

Both formats also keep the decoder facts an offline edit interprets the pixels against (the colour model, white balance facts, embedded lens data, the gain table map and semantic mattes; issue #1534) in a tail after the pixels: after the JPEG for Compact, after the completeness marker for Lossless. The header's `tail` records its byte count from the end of the file, so a header rewrite keeps pointing at it. A malformed tail is ignored like a missing one, and a file cut inside its tail is incomplete. A Lossless proxy written before the tail existed keeps working without the facts; build it again to add them.

Lossless uses `LCSP2`, a JSON header, a Deflate-compressed RGB float TIFF from the existing pure-Rust codec, and a completeness marker. It stores the source values directly, including negative values and values above 1.0. No scaling, transfer curve, integer quantization, or render process version is added.

The header has `format: "tiff32"`, format-local `enc: 1` (its own version, not Compact's encode version, so a JPEG encoding change never marks a Lossless proxy stale), dimensions, `look_version`, payload length, a 128-bit checksum, and optional `tone`. Unknown top-level and tone fields survive rewrites. This includes a newer build's `white_stat`. When integrating the light-calibration branch, its header backfill must recognize both container magics and preserve the pixel payload and completeness marker.

The reader bounds the JSON header to 4096 bytes, the file to 256 MiB, dimensions to 8192 on either axis, and the pixel count to 16 Mi pixels. The decoder must agree with the header dimensions. Lossless payload length and checksum are verified before decoding. The cheap is_complete check verifies length and the trailing marker for staleness hints. The separate is_usable check also requires a complete file, then verifies the checksum and decodes the pixels. A Compact JPEG has no checksum, and one cut short still decodes with part of the picture made up, so a Compact payload that does not end with the JPEG end marker is refused: it is not counted, rendered or moved over a complete copy, and Build Smart Previews repairs it (issue #106). Build Smart Previews uses is_usable before skipping a rebuild. Folder migration verifies the destination before deleting the source. A malformed file returns an error.

Builds write atomically. Format conversions, repairs, discards, and automatic look refreshes invalidate rendered caches. Decoded sources follow the same cache generation, and a pending older render cannot refill them. Automatic look refreshes preserve each proxy's format. Their candidate scan checks for an older look version or, for a Compact proxy, an older encoding. It does not decode current-look proxies to find corruption. Use Build Smart Previews to verify and repair those files.

A build, discard or look refresh clears the thumbnail cache once, after its last photo. The clear moves the cache generation on at once and removes the files afterwards, without holding the generation lock the app reads every frame; until the files are gone no thumbnail is read from disk, so none from before the clear is shown (removing 100,000 files took 6.7 s, during which drawing used to wait). Before it writes or removes its first proxy, it creates and syncs an empty marker file in the library's `thumbs` folder, and it removes the marker after that clear. If the app quits or crashes in between, every proxy is either its old or its new file, and the marker remains. The next opening then saves a new thumbnail cache generation before it shows any thumbnail, so thumbnails are rendered again from the current proxies. This also holds when a retry cannot change anything because the originals are offline. Every change records the marker, including a discard: an interrupted discard leaves thumbnails of the removed proxies, which still show the photo until a first build replaces that picture. Each leftover marker moves the generation on by one, so when an opening cannot save its generation, a later opening still does not reuse it after another interrupted build.

A full drive can refuse even an empty file. The run then clears the thumbnail cache at once, which removes every thumbnail of the proxies it is about to change and frees space, and tries the marker again. If the drive is still full, the run continues without the marker and clears the thumbnail cache again after each proxy it replaces (a format conversion or a newer camera look). A build into a smart preview folder on another drive with room therefore still works, and a thumbnail rendered from a proxy before its replacement can survive an exit only if the exit falls between writing that proxy and the clear after it. A first build or a repair does not clear after each proxy, because no thumbnail shows a picture it replaces, and each clear empties every thumbnail and loaded picture on screen. Other failures, such as a read-only `thumbs` folder, stop a build with an error before it changes any proxy, without clearing anything. A discard is never stopped: without a marker it still removes the proxies, so a full drive never blocks the discard that frees its space.

Invalidation follows the cache currently attached to the same library, including reopening it while a conversion or look refresh is running, however the library's path is spelled (a symlink, `..` or a relative path). Switching to another library does not invalidate that other library's cache.

## Move the preview folder

Settings runs folder changes on a worker and shows progress and cancellation in the activity stack. Builds and look refreshes must finish before a folder change starts. New builds wait until the folder change finishes.

For Move, the worker copies and verifies every destination before the library saves and uses the new location. This also applies on the same volume. Renaming each source early would leave the old folder incomplete after cancellation or a crash, so there is no rename fast path. The destination needs enough free space for the copies while the old folder is retained.

Cancel during copying keeps the entire old folder in use. Verified copies already written to the new folder may remain; retrying reuses them. A copy, verification or preference-save failure also retains all sources and the old location. A source that does not decode, such as a proxy cut short, cannot be verified at the new place: it stays in the old folder, the rest of the folder moves, and the result names it. Build Smart Previews rebuilds it in the new folder from its original. Reopening a library during copying cancels the folder change without waiting for the worker's I/O.

After the new location is saved, cleanup removes redundant sources on the worker and cannot be cancelled. Each destination is verified again before its source is removed. A crash before the location is saved reopens the complete old folder; retry the move. A crash during cleanup reopens the complete new folder and can leave redundant old files. Those leftovers can be removed after checking that the new folder is available. Neither crash point leaves the active folder split. Leave retains the old files. Delete removes them only after the new location is saved.

The folder preference and a new thumbnail cache generation are saved in one atomic write. A process exit immediately after that write reopens with the new generation, so it cannot serve thumbnails made from a different proxy in the old folder. The running session also replaces its memory cache before using the new folder. This does not depend on the cleanup worker starting. Cache namespace selection does not scan or delete files; old thumbnail files count toward the same cache budget and normal pruning removes them.

## Opening the library in an older version

Versions without Lossless smart previews, up to and including 0.7.1, can open a library this version has used, with these limits:

- They cannot read Lossless (`LCSP2`) proxies. A photo whose original is offline has no usable smart preview there.
- They do not know the format preference or the thumbnail cache generation. Saving preferences in them drops both, so the library format returns to Compact.
- They read the thumbnails of generation 0 and ignore the interrupted-build marker. After a folder change or an interrupted build, they can show thumbnails made from the previous proxies.
- After an older version has saved preferences, this version starts again at generation 0 and can show those same thumbnails.

File > Previews > Discard Preview Cache removes every cached thumbnail, in all generations, and they are rendered again from the current proxies.

## Codec comparison

The existing codecs support PNG16, TIFF16 with Deflate and a horizontal predictor, native lossless JXL16, and TIFF32 with Deflate. Float TIFF was selected because it preserves source samples exactly without adding a dependency. TIFF16 used roughly half the space, with small quantization error. PNG16 was larger than TIFF16. The original JXL comparison mixed sRGB-tagged integer samples with decoded linear Rec.2020 pixels. Converting the decoded pixels back to the integer sample space before restoring the range corrects the comparison. Maximum source error on the public A7CR sample is 1.47e-5, and the direct 16-bit integer round trip is exact. There is no JXL codec bug. Float TIFF keeps its exact-float advantage over integer quantization. A linear DNG would add raw-container metadata and decoding rules without improving preservation of an already decoded source.

Reproduce the comparison with `cargo run -p lightcraft-engine --example smart_formats -- sample.ARW`. It reports sizes, encoding and decoding times, maximum source error, and rendered CIE76 delta E at neutral settings, Exposure -2 with Highlights -100, and Exposure +2. Render comparisons use the same proxy grid and metadata to isolate storage error. They do not measure the difference between a full-resolution original render and a reduced source render.
