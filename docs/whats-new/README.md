# What's New entries

Help ▸ What's New lists what changed, newest first. Each change of ours is one file here, so pull requests that
each add an entry never edit the same file and never conflict over it.

- **Name:** `<YYYY-MM-DD>-<slug>.md`: the day you write it, then a few words in lowercase letters, digits and
  hyphens, e.g. `2026-10-16-faster-grid.md`. The date puts the entry under that day's heading, and entries of one day are listed by name.
- **Content:** one `### Title` and its bullets, nothing else (the date heading comes from the name):

  ```markdown
  ### Faster grid
  - Scrolling a large library no longer stutters (issue #123).
  ```

- Write for people using the app, and say "the app" or `{app}` rather than its name. Each line is translated as it
  stands (`docs/localization.md`), so a later rewording shows in English until the catalogs catch up.
- `docs/whats-new.md` is upstream's What's New: only upstream merges change it. `upstream.sha256` records it as the
  last merge left it, and `cargo xtask notes --check` (part of `cargo xtask ci`) fails if anything else edits it.
- The app's build joins upstream's file and these entries (`crates/ui-egui/build.rs`, `lcc-notes`); on a day both
  have, ours come first. The joined text isn't committed.

A release's changelog has its own entries: `docs/changelog/unreleased/README.md`.
