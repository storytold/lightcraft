<!--
Title: start with the checklist id or the issue, as the commits do:
`LR-IMP-FORMATS: folder import finds Sony SR2 and SRF files`, or `Issue #123: …` when there is no checklist row.

One change per pull request. Unrelated fixes and clean-ups (clippy, formatting, a second feature) go in their own:
small ones land fast, bundles get sent back to be split. The rules are in AGENTS.md.

Delete the sections and lines that don't apply, and these comments. A translation or a one-line fix needs only the
first three sections.
-->

Fixes #

<!-- `Part of #123` when this does only part of the issue. Built on another open pull request? Say so here
(`Stacked on #123`) and name the commits that are this one's own. -->

## Problem

<!-- What is wrong or missing, as a person using the app meets it. For a bug: the cause. -->

## Change

<!-- What you changed and where. -->

**Not in this PR:** <!-- what you left out on purpose, and known limits -->

## How it was checked

- [ ] `cargo xtask ci` passes on this branch, rebased on current `main`
- [ ] A test that fails without this change (for a crash fix: a regression test that panicked before it)
- [ ] User-visible change: ran the app and looked at the result

<!-- Numbers, when it changes rendering, decoding or speed: before and after, on more than one photo or camera
(ΔE against the reference, `cargo xtask bench`, `/usr/bin/time`, memory). Name the files or corpus entries used. -->

## Screenshots

<!-- For a visible change: before and after, labelled. Never a screenshot of an Adobe app. -->

## Merging

<!-- Pull requests land in batches, merged together. This is what the person merging can't see in the diff. -->

- **Overlap:** <!-- open pull requests that fix the same issue or change the same functions, by number, and how
  this one differs. Search the open pull requests for the issue number and checklist id first. "none found" is fine. -->
- **Existing edits and libraries:** <!-- does any photo already edited render differently, or any saved setting,
  preset, sort order or default change? Then it goes behind a new process version (docs/process-versions.md) or is a
  setting that is off by default, with a line in docs/whats-new.md. "unchanged" is fine. -->
- **Shared numbers taken:** <!-- catalog format `VERSION` (crates/catalog/src/journal.rs), `RENDER_CACHE_VERSION`,
  `LOOK_VERSION`, process version: the number this takes. If another pull request lands with it first, take one
  past main's when you rebase. -->
- **Where it came from:** <!-- for a decoder, camera or lens data, or fitted constants: the public spec, your own
  measurements, or observed output, and the files measured. Never GPL / LGPL code (dcraw, LibRaw, rawspeed, darktable,
  RawTherapee, lensfun …), text derived from it, or Adobe profiles. -->
- **New dependencies, `unsafe`, build scripts, network access, spawned processes, deleted files, CI or `xtask` gate
  changes:** <!-- each with its reason, or "none". Dependencies are pure Rust and use `workspace = true`; actions are
  pinned by SHA; downloads are checked against a sha256; nothing leaves the machine unless the person asked. -->

## Checklist

- [ ] No new panic path in non-test code: no `unwrap` / `expect` / `panic!`, no indexing or unchecked arithmetic on a
      number from a file, a person or an agent (`get`, `checked_mul`, NaN and char boundaries handled)
- [ ] New text on screen goes through `tr()` and has its key in `crates/ui-egui/locales/*.json`; a new shortcut is
      free in `docs/library-shortcuts.md`
- [ ] The feature's rows in `docs/parity-checklist.md` and its entry in `docs/gaps.md` are updated, honestly (🟡 when
      it is partial), in the same commit
- [ ] New code has a caller: no command, module or setting that nothing reaches yet
- [ ] Every new image, icon or other asset has its entry in `assets/ATTRIBUTION.md`; no font or media files added
- [ ] Ready to merge (otherwise open it as a draft)
