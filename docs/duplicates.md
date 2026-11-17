# Duplicate Finder

> Names in angle brackets (`<app>`, `<cli>`, …) are the values set in [`brand.toml`](../brand.toml).

Library ▸ Find Duplicates… (issue #1107) finds the photos that are copies of one another across the
whole library, so the re-imported, resized or renamed copies can be cleaned out while the best copy
of each stays. What it finds, and what the window's buttons do:

- **Groups.** Each group is one picture in several copies, the best copy to keep first: the most
  pixels, then the largest file, then the record that knows its content hash, then the earliest
  import. Every other member starts marked (a click on a photo changes its mark).
- **Copies of the same file** (`exact`) are found at once, from the content hashes the catalog keeps
  for every imported file. Photos in Recently Deleted, virtual copies and photos only browsed in a
  Local folder never group.
- **Copies that differ** (`similar`) are found by **Find Similar Copies**, which reads every photo's
  picture in the background (a row in the activity stack, with Stop): a raw by its camera JPEG, an
  image file by itself, a clip by a poster frame. Each picture is hashed perceptually (a 32×32
  luminance grid, a DCT, the low-frequency bits; [`crates/engine/src/phash.rs`](../crates/engine/src/phash.rs))
  and pictures within 10 of 64 bits of each other group — that catches a copy that was resized,
  re-compressed or renamed, but not a different photo of the same scene. The hashes are kept on disk
  in the preview cache, so a second scan only reads what changed or is new.
- **Raw + JPEG pairs of the same shot** (`sameShot`) group as their own kind: something to stack, not
  to delete, so nothing in them starts marked.
- **Buttons.** Remove sends the marked photos to Recently Deleted (`photo.delete`, one undo step;
  purging stays in the Recently Deleted flow). Reject flags them (`photo.reject`). Stack Groups
  collapses each group into a stack (`stack.group`). Nothing is deleted from disk by the finder; the
  system trash stays where it already is (Delete from Disk, Recently Deleted).
- **Telling copies apart.** Each member is a card with a large thumbnail and, under it, nothing cut
  short: the whole file name, the whole folder path (wrapped, and the tooltip), the capture date,
  the size in pixels, the file size and the format. **Compare Group in Survey** (per group, the
  `library.compareDuplicateGroup` command, `{group?}`) selects the group and shows it in the
  library's Survey, large and side by side; the window steps aside with its marks kept, and Find
  Duplicates… brings it back as it was (photos gone meanwhile dropped).
- A photo whose record has no content hash (an import from before content hashes, a Lightroom
  catalog import) is read from its file during the similar scan; the record itself is not changed.

The same scan is the `library.findDuplicates` command (`{similar?: true}`), which returns the groups
with each photo's id, name, path, resolution, file size and import date for the control channel and
MCP; the CLI renders it through `<cli> mcp` or a control script like any other command. Not in the
web build: the similar scan reads the photos' files.

## What is deliberately not there

- **Rotation- or crop-aware matching.** A copy rotated 90° or cropped hard is a different picture to
  the hash. No ML similarity either: the hash is cheap, explainable and pure Rust.
- **A Duplicates view or smart album.** The window is a review, not a place; Missing Photos is a
  view because being missing is a lasting state a photo can be relinked from, while a duplicate is a
  fact about a pair that the review resolves.
- **Delete from Disk in the window.** Removing is recoverable; the destructive paths stay where they
  are (Recently Deleted, Delete Rejected, the delete dialog).

*Last updated: 2026-11-17.*
