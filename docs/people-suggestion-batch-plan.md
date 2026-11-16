# Plan: group unknown faces in People (issue #259)

Status: **plan; the remaining part (grouping) is not built.** Refreshed 2026-11-14 from draft PR #492 (2026-10-25).

## Done on main

- **Batch assign / reject on a person's page (#478)**, landed in #655: Ctrl/⌘-click, Shift-click range or the corner
  tick selects "More" suggestions; the batch bar offers Confirm, **Assign to…** (name field, existing or new person),
  **Reject** and Clear; the context menu has "Assign to Someone Else…". Assign reuses `faces.nameFaces`.
- **Ignore (first half of #259):** `faces.ignore {faces, ignored?}` (one undo step, `ignored: false` restores) and
  `faces.ignored` (paged list). Ignored faces are stored as `RegionKind::IgnoredFace` (catalog format 14), so rescans
  do not find them again, and they are left out of People, person pages, Unnamed faces and matching. The People header
  offers **Restore N ignored faces**. See `docs/faces.md` ▸ Ignored faces.
- **Reject = Ignore** was chosen (old option A): one stored state, one command. A per-person "not this person" store
  was not built; revisit only if users ask for it.
- Ignore state lives in the catalog only (old option A); nothing is written to XMP.

## Remaining: group unknown faces into stacks

### Today

Unnamed faces puts look-alikes *next to each other* by comparing every pair for up to the first 1,500 embedded faces
(`faces.unnamed {limit?}`). That is ordering, not stacks: no group boundaries, nothing stored, nothing past the cap
grouped.

### Algorithm and threshold (to decide)

- **A. Threshold graph + connected components (single linkage)** on cosine similarity at the model's suggestion bar
  (SFace 0.55, AuraFace 0.40). Simple, deterministic; chaining (A~B~C) can merge two people.
- **B. Agglomerative clustering, average or complete linkage**, cut at the threshold. Far less chaining; O(n²) like
  today's pass, so it keeps a cap or blocking.
- **C. Density clustering (DBSCAN / HDBSCAN-style, own implementation).** Leaves odd faces as singletons; more
  parameters, harder to make incremental.
- **Threshold:** the suggestion bar, a stricter per-model grouping bar, or a "tighter / looser" slider; `faces.evaluate`
  data can pick the default per model.

Each option needs a "Sources:" line (paper, own design, or a ported implementation under the licence policy in
`AGENTS.md`; share-alike sources are usable with provenance).

### On demand or stored

- **On demand** (when Unnamed faces opens, cached in memory): never stale, no schema; cost grows with the library,
  needs the cap or an approximate nearest-neighbour index; stacks can reshuffle between sessions.
- **Stored** (cluster id per face, updated as faces are embedded): scales, stable; needs a migration and invalidation
  on model change, naming, ignoring or threshold change.

Long work holds a `TaskGuard` on the activity stack and is cancellable.

### Command shape

- **A. New `faces.clusters {threshold?, limit?, minSize?}`** returning stacks (id, faces, representative face, best
  suggested name). Separately testable; MCP agents get groups directly.
- **B. `faces.unnamed {group: true}`** or a `cluster` field per face. One listing; the reply has two modes.

Hostile input (bad ids, huge limits, NaN thresholds) gives errors, never a panic; list sizes are capped.

### UI: stacks in Unnamed faces

- One tile per stack: representative face, count badge, best suggested name; singletons after the stacks.
- Click expands the stack to review; "Not in this group" pulls a face out.
- Right-click a stack or selection: **Name…** (the name field from #655, via `faces.nameFaces`), **Ignore**
  (`faces.ignore`), **Expand**. Selection and Select all count a stack as all its faces.
- Order: largest stacks first (suits bulk naming) unless review prefers most-alike first.

### Tests

Grouping is deterministic for a fixed embedding set; two clearly separate synthetic identities never share a stack and
near-duplicates do (behaviour, not exact counts); Name / Ignore on a stack undo in one step; hostile inputs to the new
command; one MCP round trip in `crates/mcp/tests/it/e2e.rs`; headless snapshots of stacks and the menu.

### Docs

`docs/faces.md` (Unnamed faces), `docs/mcp.md` if a command is added, manual pages for People, parity-checklist and
gaps rows, `docs/whats-new/` and `docs/changelog/unreleased/` entries.
