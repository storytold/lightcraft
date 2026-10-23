# Contributing to LightCraft

Thank you for helping. Humans and agents follow the same rules: [`AGENTS.md`](AGENTS.md) has them (clean-room,
no Adobe assets, pure Rust, never crash, everything is a command). This page is the workflow around them: how to
pick something, claim it and get it merged. Questions, ideas, or a bug you'd like to talk through first: bring them
to [Discord](https://discord.gg/artcraft).

## Pick something

- **Gaps:** [`docs/gaps.md`](docs/gaps.md) lists every known shortfall against Lightroom, ranked by user impact
  (`B` marks a beta blocker). [`docs/parity-checklist.md`](docs/parity-checklist.md) has one row per feature, menu
  item and shortcut.
- **Issues:** [GitHub issues](https://github.com/storytold/lightcraft/issues), bugs and feature requests alike.
- **Check nobody has it yet.** Read the issue's comments for a claim and search the open pull requests for its number
  and its checklist id. Claims live in comments; the Assignees field isn't used.

## Claim it

Comment on the issue to say you're taking it, then go ahead. If there is no issue yet, open one first (what's wrong
or missing, and how you know), so there is something to claim and for the pull request to close. If you stop, say so
in the issue so someone else can pick it up.

## Open a pull request

- **From a fork** (contributors don't push to this repository), branched from `main`.
- **Title:** start with the task or checklist id, as the commits do: `LR-IMP-FORMATS: folder import finds Sony SR2
  and SRF files`, or `Issue #1080: …` when there is no checklist row.
- **Description:** put `Fixes #123` in it, so merging closes the issue. A pull request that does only part of an
  issue says `Part of #123` instead. Say what you changed and how you checked it (tests, the app, screenshots for UI
  changes). If it builds on another open pull request, say so at the top.
- **Green first:** `cargo xtask ci` passes before every commit (fmt, clippy with the never-crash lints, tests,
  layering, parity, assets, WASM). New features come with a test that fails without them; crash fixes with a small
  regression test that panicked before the fix.
- **One task id per commit**, and the docs it moves in the same commit: the feature's rows in
  `docs/parity-checklist.md`, its entry in `docs/gaps.md`, and the numbers in `ROADMAP.md` /
  `docs/target-app-parity.md` when a listed gap closes.
- **Assets:** every image, icon or font has an entry in [`assets/ATTRIBUTION.md`](assets/ATTRIBUTION.md), added in
  the same commit. Fonts go to [storytold/craft-fonts](https://github.com/storytold/craft-fonts), not here.

## Credits

The About window credits everyone by GitHub username. If you'd like a real or display name shown as well, add your
own entry to `contributors/people.toml` in your pull request ([`docs/contributors.md`](docs/contributors.md)). Only
ever add or change your own entry.

## Working with an agent

An agent working for you follows [`AGENTS.md`](AGENTS.md) and this page, and you are the author of what it submits:
read its pull request before you open it, and answer review comments yourself or through it.
