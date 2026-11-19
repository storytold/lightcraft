# Pulling upstream work: the intake runbook

> **Last updated:** 2026-11-17 · **Change:** `drift` reports waiting rows whose upstream PR moved. Previously: 2026-11-08, an intake PR merges when it is ready, in any order with other PRs, as
> every round so far has; it is no longer held for a release. Previously: 2026-11-02, created (per-PR and per-commit
> intake, one PR per round, release-gated; the merge routine is retired).

This page is the whole procedure for bringing work down from upstream. It is written so an agent can run one intake
round end to end without asking questions. Read [`AGENTS.md`](../AGENTS.md) first: its clean-room, brand, never-crash,
layering and testing rules apply to everything here.

## Why there is no merge any more

The last merge (PR #486: 281 commits, 88 conflicted files) showed that merging has stopped working: the fork's
UI, catalog and shell replaced upstream's, so every merge conflicts there, and resolving those conflicts silently
brought in upstream behaviour and tests that had to be undone afterwards. That is where unintentional degradation came
from. **Never merge upstream, and never use `cargo xtask upstream-merge`.** The only path is one upstream PR or commit
at a time, read in full, ported deliberately, and proved by the fork's own checks. This is slower per change and much
safer per release.

## Scope: what a round pulls

| Input | Where it lives | How it is taken |
|---|---|---|
| **Open PR** | `refs/pull/<n>/head` | Read the head at the commit reviewed; fetch the PR head, or port by hand. A draft or red PR is `PENDING`, never `ADOPT`. |
| **Closed, never merged PR** | `refs/pull/<n>/head` (GitHub keeps it) | Still eligible: abandoned, superseded or closed-for-cleanup work often holds a fix we want. Confirm it never landed by another route first. |
| **Merged PR** | Commits now on `upstream/main` | **Use the commits, never the merge commit.** Upstream lands community work in integration batches (#3415: 17 PRs, #3430: 178 PRs), so a merge commit is a container with nothing of its own to port. |
| **Commit on `main` with no PR** | `upstream/main` | The commit lane in [`docs/upstream-merge.md`](upstream-merge.md). Review it like a one-commit PR. |

A PR that is merged is not automatically in: check `git log upstream/main --grep='#<n>'` and read the commits
themselves. A PR that is closed is not automatically out: read the closing comment.

## The rules that make this safe

1. **Never blindly grab work.** A PR is not adopted because it exists, is popular, or is already merged upstream. It is
   adopted because the fork needs it and the diff is safe here. Every `ADOPT` carries a reason, the checks that
   passed, and a fork commit.
2. **Read everything.** The full diff, every human review, every CodeRabbit thread, the PR body, the linked issue, the
   CI result, and the commit messages. The merged or closed state is not a substitute for reading.
3. **Path classification comes first.** A PR that touches only [`upstream-owned.txt`](../upstream-owned.txt) `[owned]`
   paths (brand, `crates/catalog`, `crates/ui-egui`, `crates/notes`, Immich, …) is almost always `SKIP`: those are the
   paths we rewrote. Shared crates are in scope; an owned-file change that must come along is ported by hand, not by
   cherry-pick.
4. **Prove no regression, not "it compiles".** A change to rendering, process versions, catalog format, settings
   defaults, XMP/sidecars, or generated files needs its numbers or an explicit "unmeasured, kept out" note. When in
   doubt, leave it `PENDING`; a later round is cheap, a regressed release is not.
5. **One round, one PR.** Every adopted PR and commit from a round goes into a single fork branch and a single PR. No
   stray pushes, no per-PR PRs.
6. **An intake PR merges when it is ready.** Once reviewed and green it merges like any other PR, before or after
   other work; a round takes long enough that other PRs always land meanwhile. It isn't held for a release, and a
   release doesn't wait for one ([Landing](#landing)).
7. **The ledger is the memory.** Every decision, including the ones not taken, is recorded in
   [`docs/upstream-pr-reviews/ledger.md`](upstream-pr-reviews/ledger.md). An unrecorded decision did not happen.

## The ledger

[`ledger.md`](upstream-pr-reviews/ledger.md) is the single source of truth. The number-banded files beside it
(`open-below-2850.md`, `open-2850-2929.md`, `open-2930-up.md`, `closed-unmerged.md`, `upstream-main-commits.md`) are the
**frozen archive** of the 2026-10 reviews: do not edit them, and do not add rows to them. When an archived decision is
revisited, the new row goes in the ledger with a pointer to the old file.

One row per upstream PR (or per commit in the commit lane):

| Column | Meaning |
|---|---|
| `Item` | `#1234` for a PR, `commit <sha>` for the commit lane. |
| `State` | `open`, `closed`, `merged`, `commit` — as observed at the last review. |
| `Title` | The upstream title, shortened if needed. A pipe inside a cell is escaped with a backslash. |
| `Decision` | `ADOPT` (ported), `LEARN` (idea taken in our own form, or a named follow-up), `SKIP` (not for us), `PENDING` (not decidable yet), `WATCH` (good, waiting for upstream's verdict). |
| `Recheck` | `-` for a final decision; otherwise what has to happen (`upstream approval`, `CI green`, `#1234 lands`, `each round`, `next release`). |
| `Reason` | One line: the user-visible value, or the concrete reason not to. For `PENDING`/`WATCH`, what is still unknown. |
| `Upstream ref` | The PR head SHA (`headRefOid`) or commit SHA that was reviewed — 10+ characters. This is what makes "the PR changed after review" detectable. |
| `Reviewed` | `YYYY-MM-DD @ <upstream-main sha10>`. |
| `Fork` | The fork commit or PR that carried it; `-` when not adopted. |

`PENDING` and `WATCH` are the re-review queue. Every round re-reads those rows' PRs — all comments and code — and either
decides them or updates `Upstream ref` and the reason. A `WATCH` row whose PR receives an approving review or is merged
is taken without a fresh full review, but still through every gate below.

## One round, step by step

Work on a branch off the latest `main`: `git switch -c upstream-intake-<YYYY-MM-DD>`. Keep the run's scratch output in
`target/upstream/` (gitignored).

### 0. Preflight

```sh
UP=$(git remote get-url upstream | sed -E 's#(git@|https://)github.com[:/]##; s#\.git$##')
echo "$UP"                     # <owner>/<repo>, the upstream remote (see docs/upstream-merge.md)
git fetch origin && git fetch upstream
git status                     # clean, or know what is there
git rev-parse --short upstream/main
python3 tools/upstream_intake.py check     # ledger is valid before you touch it
```

Record the upstream/main SHA in the ledger's run log. If the repo has uncommitted work, stop and report it.

### 1. Enumerate the candidates

```sh
python3 tools/upstream_intake.py enumerate --since <last run date> \
    --states open,closed,merged --out target/upstream/intake-<YYYY-MM-DD>.md
```

The report lists every open upstream PR plus every PR updated since that date, marks the ones already in the ledger
(with their decision), flags PRs that touch only owned paths, and shows the touched files. Candidates are:

- every open PR whose number is above the last reviewed boundary, plus every open PR updated since the last run;
- every closed PR updated since the last run (a closed PR can be reopened, re-split or replaced);
- every merged PR updated since the last run;
- every `PENDING` and `WATCH` row, whatever its state.

`gh` (with `UP` set as above) is the source of truth for state; `git fetch upstream` is the source of truth for code.

### 2. Triage before reading code

Sort the candidates in this order and stop early where the answer is already clear:

1. **Owned-only** → `SKIP` with "upstream-owned paths" unless a shared fix is trapped inside it.
2. **Already in the fork** → search the fork's log and code (`git log --all --grep='#<n>'`, `git log -S'<symbol>'`,
   `git grep`). Say which fork commit fixed it. `SKIP` as duplicate.
3. **Superseded upstream** → the PR body or the closing comment names a later PR; follow it and review that one
   instead. `SKIP` the old number with the pointer.
4. **Draft, red CI, or an unresolved change request** → `PENDING`; name what is missing.

### 3. Read the PR in full

```sh
gh pr view <n> --repo "$UP" \
  --json number,title,state,isDraft,mergedAt,headRefOid,baseRefName,body,author,reviewDecision,statusCheckRollup
gh pr diff <n> --repo "$UP"
gh pr view <n> --repo "$UP" --comments
gh api --paginate "repos/$UP/pulls/<n>/comments"     # inline review threads
gh api --paginate "repos/$UP/issues/<n>/comments"    # conversation
```

Read the linked issue too. A PR that fixes an issue the fork does not have is not automatically useful, and one that
closes an issue the fork already fixed is a duplicate.

Then inspect the fork: the files the PR touches, the fork's version of those files, and the surrounding callers. Decide
whether the change applies as-is, needs adaptation, or solves a problem we do not have.

### 4. Classify

- `ADOPT` — useful here, applies to our structure, passes the gates in §5.
- `LEARN` — the idea is useful but the patch is not portable (fork-owned UI, diverged catalog, a different process
  version). Write the named follow-up in `Reason`; if it is small enough, do it in this round as our own commit.
- `SKIP` — not useful, already fixed, upstream-only (CI, packaging, release notes, dependency bumps, upstream docs), or
  a UI rework of something we redesigned.
- `PENDING` — cannot decide yet: incomplete, head moved mid-review, unresolved thread, needs a measurement, needs an
  upstream decision.
- `WATCH` — we want it once upstream has vetted it (approving review or merge).

The default is not `SKIP`. "A fork user would not notice" is a reason to skip; "I did not finish reading" is a reason
for `PENDING`.

### 5. Verify an ADOPT — the gate

Every item below must pass, or the row becomes `PENDING`/`SKIP`:

- **Clean-room and licence.** Third-party code, weights and data follow `AGENTS.md`'s licence policy (2026-11-05):
  allowed when the licence permits derivatives, sharing and commercial use (GPL, CC BY, public domain, MIT-style),
  credited in `NOTICE` / `LICENSE` in the same commit. No Adobe material, no vendor binary.
- **Brand.** `cargo xtask brand check`; no legacy name, no upstream product strings; text goes through `tr()`.
- **Never crash.** No new `unwrap`/`expect`/`panic!`/indexing on input-derived values in non-test code; no new `unsafe`
  outside the crates `AGENTS.md` names. Hostile-input tests where the change parses anything.
- **Layering.** `cargo xtask layers`; nothing below L5 gains a UI dependency.
- **No silent behaviour change.** Rendering, process version, catalog format, defaults, sidecars and file writes are
  the risky ones: state the version numbers taken (see the PR template's *Shared numbers taken*) and do not reuse a
  number another PR is taking. A render change needs before/after numbers or stays out.
- **Tests.** The PR's tests come along, adapted; where upstream has none, the fork-only test is added. A crash fix
  needs the hostile-input check.
- **Build and checks.** On the intake branch: `cargo xtask ci --fast`, `cargo xtask sil` where commands/UI are touched,
  and `cargo test -p <crates touched>`. User-visible changes are run in the app and looked at.
- **Dependencies.** Pure Rust, workspace-managed, licence allowed by `deny.toml`; a new dependency is called out in the
  PR body.

### 6. Port

```sh
git fetch upstream pull/<n>/head:refs/remotes/upstream/pr-<n>   # open or closed PR
git log --oneline upstream/main..upstream/pr-<n>               # the commits to port
```

- Cherry-pick when the patch still applies: `git cherry-pick -x <sha>` (`-x` records the upstream commit).
- Port by hand when the fork diverged: read `git show <sha>`, write the change in the fork's structure, bring the
  tests. A hand-port says "adapted from upstream #<n>" in the message.
- Map names with `cargo xtask rename-crates --upstream --since HEAD~1`, then `cargo xtask brand check`.
- **One commit per adopted PR or upstream commit.** Subject names the upstream PR: `<task id>: <what> (upstream #<n>)`
  or, when there is no task id, `upstream #<n>: <what>`. Keep the upstream author's `Co-authored-by` where the code is
  theirs.
- Never port a merge commit. Never port a whole integration batch as one commit.
- Bring the behaviour the PR intended, not just the diff: if the fork needs a follow-up to make it work, that follow-up
  is part of the same commit.

### 7. One PR for the round

Open a single PR into `main` from `upstream-intake-<YYYY-MM-DD>`:

- Title: `Upstream intake <YYYY-MM-DD>: <n> PRs, <m> commits`.
- Body: the round's table — every item, decision, one-line reason, and for `ADOPT` the fork commit and what was checked.
  Link the upstream PRs. Call out shared version numbers taken and anything unmeasured.
- It is a normal PR by every other rule ([`CONTRIBUTING.md`](../CONTRIBUTING.md)): branched from the latest `main`,
  green `cargo xtask ci --fast`, docs and release notes in the same commits, no bundling of unrelated fork work.
- It merges when it is reviewed and green, like any other PR ([Landing](#landing)).

### 8. Close the round

Update the ledger (rows, `Upstream ref`, `Reviewed`, `Fork`), the run log, and the boundaries. Release-note files
(`docs/whats-new/`, `docs/changelog/unreleased/`) are added with the ported commits and named in the PR. Then:

```sh
python3 tools/upstream_intake.py check
cargo xtask ci --fast
```

The round is done when the ledger is valid, every candidate has a row, and the PR links every `ADOPT`.

## Landing

An intake PR merges when it is reviewed and its checks are green, in any order with other PRs. (This page first held it
for a release, as the last merge before the changelog is gathered; in practice every round merged within hours, and
a round takes long enough that other work always lands meanwhile.) The full CI run before a release covers whatever
rounds have landed since the last one, and the release notes name their upstream PRs
([`docs/releasing.md`](releasing.md), step 2). Each merge into `main` leaves an open intake PR behind (branches must be
up to date): merge `origin/main` into it when it is next to merge and re-run the fast checks; never rebase a shared
branch.

Security and data-loss fixes are the one place where the maintainer may choose to land a port sooner. Mark the ledger
row `URGENT` in `Reason` and ask; do not decide it alone, and do not let an urgent fix bypass the gates.

## Automation

`tools/upstream_intake.py` (standard library only, `gh` for the API) does the mechanical parts:

```sh
python3 tools/upstream_intake.py enumerate --since 2026-10-29 --out target/upstream/intake-2026-11-02.md
python3 tools/upstream_intake.py check          # ledger schema, duplicates, missing refs/links
python3 tools/upstream_intake.py drift --out target/upstream/drift.md   # waiting rows whose PR moved
```

It never decides anything and never writes to the repository: it lists candidates, marks owned paths, and
cross-references the ledger. Suggested next steps, in value order:

1. **`cargo xtask upstream-prs`** — the same report as a workspace command so CI and agents share one implementation.
2. **Head-drift detection** — done (2026-11-17): `drift` lists the `PENDING`/`WATCH` rows whose PR merged, closed, was
   approved, left draft or moved its head since review (`--all` adds `LEARN`/`ADOPT`). A daily local monitor runs it
   and re-reads the movers; its reports land in `target/upstream/monitor/`.
3. **Comment digest** — a per-row summary of unresolved human/CodeRabbit threads, so `PENDING` rows re-open with their
   open questions first.
4. **A ledger merge helper** — one row per PR keyed by number, with the banded archives folded in, so a PR cannot appear
   twice with different decisions.

Anything that automates the *decision* is out of scope: classification stays a read-the-diff job, by a person or an
agent held to the gates above.

## Failure modes to avoid

- **Cherry-picking a merge commit** or an integration batch: it drags the batch's conflicts and unrelated changes in.
- **Trusting the merge state**: a PR merged upstream can still be wrong here, and a closed one can still be right.
- **Adopting a stale review**: if `headRefOid` moved, the review is void; re-read.
- **Two rounds at once**: one intake PR at a time; two branches will take the same shared version number or the same
  lines.
- **Silent render changes**: "it builds" is not evidence. Measure, or leave it out.
- **Owner-path creep**: porting an upstream UI change by hand into `crates/ui-egui` usually means redoing it in the
  fork's design; that is `LEARN`, not `ADOPT`.
- **Forgetting the ledger**: an unrecorded port will be re-reviewed next round and either duplicated or reverted.
