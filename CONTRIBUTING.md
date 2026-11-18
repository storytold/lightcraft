# Contributing to Chromaforge

Humans and agents follow the same rules: [`AGENTS.md`](AGENTS.md) has them; this page is the workflow.
Questions/ideas: [the Discord](https://discord.gg/c3dfYATzsQ), [project channel](https://discord.com/channels/1359579021108842617/1559361045468942346).

## Pick something

- [`docs/gaps.md`](docs/gaps.md) (impact-ranked; `B` = beta blocker), [`docs/parity-checklist.md`](docs/parity-checklist.md)
  (one row per feature/menu item/shortcut), or a GitHub issue.
- **Check nobody has it:** claims live in issue comments; search open PRs for the issue number and checklist id.
- **Check upstream:** upstream is no longer merged — each round is reviewed and ported by hand
  ([`docs/upstream-pr-intake.md`](docs/upstream-pr-intake.md)), landing as one fork PR. Search the log and open PRs
  for the upstream issue/PR number before starting; the fix may be ported already or due to be.

## Claim it

Comment on the issue; if none exists, open one (what's wrong/missing, how you know). If you stop, say so in the
issue. A checklist row with no issue is named in the PR title instead.

## Open a pull request

- **From a fork**, branched from latest `main`. GitHub merges only up-to-date branches: when `main` moves, merge it
  in, re-run the checks, push.
- **Template** ([`.github/PULL_REQUEST_TEMPLATE.md`](.github/PULL_REQUEST_TEMPLATE.md)); one change per PR.
- **Title:** task/checklist id first, as commits do (`LR-IMP-FORMATS: …`), or `Issue #123: …`.
- **Description:** `Fixes #123` (or `Part of #123`); what you changed and how you checked it (tests, app,
  screenshots for UI).
- **Green first** ([`docs/ci.md`](docs/ci.md)): *before pushing*, every commit passes `cargo xtask ci --fast`,
  `cargo test -p <crate>` for touched crates, and `cargo xtask sil`. The full suite runs in cloud CI on the PR.
  New features ship with a test that fails without them; crash fixes with a hostile-input no-panic check.
- **One task id per commit**, with the docs it moves (parity-checklist rows, gaps entry, ROADMAP/target-app-parity
  numbers) in the same commit.
- **Manual:** a user-visible change (feature, menu item, setting, shortcut, default, platform, screenshot) updates the
  matching page in `docs/manual-site/src/content/docs/` and/or `docs/manual/` in the same commit; see
  [`docs/manual-site/AUTHORING.md`](docs/manual-site/AUTHORING.md). The PR says which pages, or "none".
- **Release notes:** user-visible changes add `docs/whats-new/<date>-<slug>.md` (+ `docs/changelog/unreleased/…`);
  the READMEs there say how.
- **Sources:** an algorithm PR names what it was built from on a `Sources:` line (paper, prose spec, "own design").
  What may be read: [`AGENTS.md`](AGENTS.md) → third-party policy.
- **Assets:** every image/icon/font gets an [`assets/ATTRIBUTION.md`](assets/ATTRIBUTION.md) entry in the same
  commit. New fonts go to [craft-fonts](https://github.com/storytold/craft-fonts), not here.
- **Licence:** contributions are offered under the licence of the code you change — `MIT OR GPL-3.0-or-later`
  (default; GPL-3.0-or-later share-alike welcome, [`LICENSE-GPL-3.0.txt`](LICENSE-GPL-3.0.txt)); `crates/segment` +
  `crates/fetch` also offer Apache-2.0.
- CI is maintainer-run; a fork PR's result can come back hours after push.

## Credits

The About window credits everyone by GitHub username; add a real/display name via **your own** entry in
`contributors/people.toml` ([`docs/contributors.md`](docs/contributors.md)). Only ever change your own entry.

## Working with an agent

Your agent follows [`AGENTS.md`](AGENTS.md) and this page; you are the author of what it submits — read its PR
before opening it and answer review comments yourself.
