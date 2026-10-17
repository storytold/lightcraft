## Development

- **Release notes, one file per change:** What's New entries are files in `docs/whats-new/` and changelog entries
  files in `docs/changelog/unreleased/`, so pull requests no longer conflict over them. The app's build joins What's
  New with upstream's `docs/whats-new.md`, `cargo xtask notes --release <version>` gathers the changelog, and
  `cargo xtask notes --check` (part of `ci`) checks both. Thanks @clydecbrown. (no PR)
