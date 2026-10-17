# Changelog entries for the next release

Each change of ours that the release's changelog should mention is one file here, so pull requests never edit the
same changelog file. At release, `cargo xtask notes --release <version>` gathers them into
`docs/changelog/<version>.md` and deletes them.

- **Name:** `<YYYY-MM-DD>-<slug>.md`: the day you write it, then a few words in lowercase letters, digits and
  hyphens, e.g. `2026-10-16-faster-grid.md`. Within a section, entries are listed oldest first.
- **Content:** one `## Section` heading and its bullets, written as in the released changelogs
  (`docs/changelog/0.9.0.md`): a bold lead, what changed, credits, the fork issue and the matching upstream PR:

  ```markdown
  ## Library

  - **Faster grid:** scrolling a large library no longer stutters. Thanks @someone. Fork #123. (no PR)
  ```

- **Sections**, in the order a release lists them: Highlights, Develop and masking, Develop, Library, Import,
  export and formats, Import, Display, Interface, Performance, Updater and security, Release builds and CI, From
  upstream, Upstream merge, Fixes, Development. Another name is fine; it goes after these.
- `cargo xtask notes --check` (part of `cargo xtask ci`) checks every entry's name and shape.
- **At release:** `cargo xtask notes --release <version> --dry-run` shows the result. Without `--dry-run`, it
  writes the file, adding to the matching sections of one already started by hand (its introduction and other
  text are kept). Then edit it as usual.
