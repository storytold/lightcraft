# Lightroom Classic reference (our own words)

The behaviour we aim to match, described from public knowledge of how Lightroom Classic works and from the rows of
`docs/parity.md`. Nothing here is copied from Adobe: no help text, screenshots, icons, preset files or menu
transcripts. Write every description yourself; when in doubt, describe the *behaviour*, not the wording.

| File | What it holds |
|---|---|
| [feature-catalog.md](feature-catalog.md) | every tracked id (`LR-`, `LRC-`, `MENU-`, `KEY-`, `KEYC-`, `IMM-`) with feature name and tier, generated from `docs/parity.md` |
| [modules.md](modules.md) | per-module behaviour notes: what each module is for, its panels and how they interact |
| [menus.md](menus.md) | the menu structure per module, as a map of where commands live |
| [shortcuts.md](shortcuts.md) | the Classic keyboard map, grouped, and how it differs from ours today |

Private observations (notes taken while running the real application, screenshots for personal reference) go in
`plan/observations/`, which is git-ignored and never published.

**Updating the catalog.** `feature-catalog.md` is regenerated from the tracker when rows are added: take each row id,
its feature name and its tier, grouped by the tracker's `##` sections. The tracker is the source of truth for status.
