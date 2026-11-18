# Writing manual pages

The manual is a short, picture-first feature guide: bullets and screenshots, almost no prose. Think feature flyer
with a deep contents tree. Cover every feature by **name** and where to find it; describe only what is not obvious.

## Keeping it current
This is a standing duty, not a one-off: every change that alters what a user sees or does updates the page that names it in
the same commit (AGENTS.md → "The manual moves with the product"). Before a release, compare `docs/changelog/<version>.md`
with the manual: each feature there needs a page or a line, and stale text is fixed.

## Page shape
- One `.mdx` (or `.md` when it has no components) per feature or small group, in the section folder
  (`library/`, `develop/`, `ai/`, `output/`, `sync/`, `automate/`, `platforms/`, `reference/`). The sidebar builds itself
  from the folders; use `sidebar: { order: N }` in the frontmatter to order pages.
- Frontmatter: `title` (2–4 words) and a one-line `description`.
- Bullets, not paragraphs. Bold the feature name, then a few words. Tables for lookups. Aside notes for Modern-layout menu
  paths. No marketing words.
- The display name may be written as "Chromaforge" in pages; never write binary names, the env prefix or legacy names
  (`cargo xtask brand check` fails on them).
- Link to other pages with relative links (`../colour/`), never `/…`: the site is served under a base path.
- Facts come from the code and docs in this repository (`docs/*.md`, `docs/parity-checklist.md`, `crates/…`), not from
  memory. Mark a ⬜ (not built) or 🟡 (partial) row honestly: say "partial" or leave it out. Never write about a feature
  that does not exist.

## Screenshots
- `<Shot name="x" alt="…" />` shows one picture as Classic / Modern / Compact tabs (the reader's choice is remembered).
  Import it: `import Shot from '../../../components/Shot.astro';` (the path depth follows the page's folder).
- A shot is an entry in `scripts/shots.d/<area>.json` (one array per area; names are unique across the site):
  `{ "name": "x", "steps": [ {control-protocol request}, … ] }`. The runner adds the layout switch, waits for the renders to
  settle and takes the screenshot. Steps are `engine.execute` commands, `ui.clickWidget {id}`, `ui.dragWidget`,
  `ui.pointer` (normalised image coordinates), `ui.set`, `ui.key`, `ui.menu.invoke` — see `docs/control-protocol.md`.
  Find widget ids with `ui.widgets {filter}` in a scratch script. Shots run in order in one session, so state carries on;
  start a shot from a known state (`module.library` / `module.develop`, `library.select`).
- Per shot: `"layouts": ["classic","compact"]` skips Modern (when the feature is menu-only there; also pass
  `menu="Window ▸ …"` to `<Shot>` so the Modern tab says where it is). Per step: `"only": ["modern"]`.
  Layout variables (`$masks`) are in `scripts/shots.json`.
- Render: `node scripts/shots.mjs --cli <path to app-cli> <shot-name> …`. Output is committed
  (`src/assets/shots/<layout>/<name>.jpg`). Look at every image you make; a wrong or empty shot is worse than none.
- Only the procedural demo library (`--demo`) is available: no real photos, no network, no GPU models, no camera, no
  native file dialogs. Where a picture cannot be made headless, use `<NeedsImage what="what it should show" />`
  (`import NeedsImage from '../../../components/NeedsImage.astro';`). Do not fake a screenshot.

## Modern layout
Modern shows only Library and Develop on screen; the rest is in the menu (☰). On every page whose feature is menu-only
or off-screen in Modern, add `<Aside type="note" title="Modern layout">` with the menu path. The table in
`start/layouts.mdx` lists what is where.

## Check
`pnpm build` in this folder; `cargo xtask brand check` and `cargo xtask assets` from the repository root.
