# Merging upstream (LightCraft)

This repository is a fork of LightCraft. The fork point is tagged `fork-base`, so

```sh
git log --oneline fork-base..upstream/main
```

always lists what upstream has done since. Merge it in **weekly** at first (more often while the code bases are still
close, since small merges are cheap and big ones are not).

## Remotes (one-time)

```sh
git remote -v                    # upstream = the LightCraft repository, origin = this fork
git remote add upstream ../lightcraft   # only if it is missing
```

## One-time setup

```sh
git config rerere.enabled true   # git records conflict resolutions and replays them in later merges
```

## The commands

Owned paths (where we diverge on purpose, `plan/upstream.md` → *What we own*) are listed in
[`upstream-owned.txt`](../upstream-owned.txt), together with the regenerated files (`Cargo.lock`, the
`docs/parity.md` summary, `*.md` rendered from `*.md.in`) and the paths exempt from the shared-code check.

- **`cargo xtask upstream-merge [--ref upstream/main] [--no-ci]`** automates steps 1–3 below and checks step 4:
  refuses on a dirty tree, fetches, creates `merge/upstream-YYYYMMDD` from the current HEAD, runs
  `git merge --no-ff --no-commit`, then puts back our version of every owned path (conflicted ones too; files
  upstream added under an owned path are dropped). Git's `merge=ours` attribute is not enough: it only applies when
  both sides changed a file. Upstream commits that touched owned paths go into `target/upstream/review-YYYYMMDD.md`
  (hash, subject, files with line counts): they were set aside, review them for anything worth porting by hand.
  It then runs `rename-crates --upstream --since HEAD`, prints `brand check` findings, lists remaining conflicts
  (exit non-zero), and otherwise leaves the merge staged but uncommitted and runs `cargo xtask ci`. Continue at
  step 4.
- **`cargo xtask upstream-pr <branch> <commit>...`** creates `<branch>` off `upstream/main` and replays the commits
  (`git am --3way`) with our crate names mapped back to upstream's (`dac-x` → upstream's prefix, `-p dac-app` →
  upstream's app package), in the patches and the touched files. It never pushes: it prints the push/PR steps. Our
  copy of the commit then carries `UPSTREAM-PR: <link>` in its message.
- **`cargo xtask shared-check [--strict]`** (a warning step in `cargo xtask ci`) lists commits since the last
  upstream merge (else the `fork-base` tag) that change shared paths without an `UPSTREAM-PR:` line. It warns and
  exits 0 unless `--strict`; without the `upstream` remote or the tag it warns and skips.

## The routine by hand (what `upstream-merge` automates)

1. **Fetch.**
   ```sh
   git fetch upstream
   git log --oneline fork-base..upstream/main | wc -l   # how much is new
   ```
2. **Merge on a branch** named for the day:
   ```sh
   git switch -c merge/upstream-$(date +%Y%m%d) main
   git merge --no-ff upstream/main
   ```
   Git's rename detection maps upstream's `apps/lightcraft`, `apps/lightcraft-cli` and `apps/lightcraft-web` onto
   `apps/app`, `apps/cli` and `apps/web`. Resolve content conflicts as usual, but **never by hand-editing names**
   (step 3 does that).
3. **Map upstream crate names to ours.** Upstream calls its crates `lightcraft-*` (paths `lightcraft_*`); ours use
   the internal prefix (`dac-*`). Rewrite every incoming reference in the files the merge touched:
   ```sh
   cargo xtask rename-crates --upstream            # files changed since fork-base, plus conflicted files
   cargo xtask rename-crates --upstream --since HEAD~1   # narrower: only what this merge brought
   ```
   Only names of crates this workspace has are rewritten (`lightcraft-raw` → `dac-raw`, `lightcraft_engine::` →
   `dac_engine::`, `-p lightcraft` → `-p dac-app`). A crate upstream **added** is not known yet: rename its package
   to `dac-<name>` in its `Cargo.toml`, register it in `xtask/src/layers.rs`, and run the command again.
4. **Remove upstream's product name** everywhere else. Upstream writes "LightCraft", `lightcraft`, `LIGHTCRAFT_*`
   and its app id into code, docs and packaging; here every name comes from `brand.toml`:
   ```sh
   cargo xtask brand check
   ```
   lists each `file:line`. Fix them with the brand constants (`dac_brand::DISPLAY_NAME`, `dac_brand::env("LOG")`,
   `dac_brand::fill("… {app} …")`), `{{app}}`/`{{binary}}` placeholders in `*.md.in` docs and `packaging/**/*.in`
   templates, or neutral wording ("the app"). New legacy handling (code that must name an old identity to migrate
   from it) goes in `LEGACY_FILES` in `xtask/src/brand.rs`, and the old values in `[legacy]` of `brand.toml`.
5. **Regenerate and verify.**
   ```sh
   cargo xtask docs          # re-render *.md.in after upstream doc changes
   cargo xtask ci            # fmt, brand check, docs --check, clippy, tests, parity, layers, assets, deny, wasm
   cargo xtask brand test    # optional locally; CI runs it: the throw-away brand shows nowhere else
   ```
6. **Commit, review, merge.** Commit the merge (one commit for the merge itself, separate commits for the
   rename and brand fixes keep the history readable), open a PR from `merge/upstream-YYYYMMDD`, and merge it into
   `main` once CI is green.

## Notes

- Upstream renames are resolved by the brand tooling (`rename-crates --upstream`, `brand check`), never by
  hand-editing names; if a pattern keeps coming back, teach the tooling instead.
- Licence: upstream code arrives under MIT OR Apache-2.0; we use it under MIT and keep upstream's copyright lines in
  `LICENSE`/`NOTICE`. `crates/segment` and `crates/fetch` stay Apache-2.0. `cargo deny check licenses` (part of
  `cargo xtask ci`) catches a dependency upstream adds under a licence `deny.toml` does not allow.
- To change our own crate prefix (an internal codename users never see), run `cargo xtask rename-crates <prefix>` in
  a commit of its own, then re-check that `--upstream` still maps cleanly.
