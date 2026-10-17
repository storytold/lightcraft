# User docs: rules for agents and contributors

Everything in this folder (except this file, CLAUDE.md and README.md) is written to be published as a docs site
(upstream publishes its copy on its project website, under `/docs/<upstream>/`). The file path is the URL:
`troubleshooting/raw-photos-look-dull.md` → `<docs root>/troubleshooting/raw-photos-look-dull`.
Renaming a file breaks links, so add the old path to `redirect_from` when you rename.

## Who reads this

Artists and photographers using the app, community moderators answering them, and AI assistants looking
things up for either. Write for a photographer, not a developer.

## Page rules

- One question or problem per page. Lead with the answer or the fix in the first two lines.
- Plain words, short sentences, numbered steps for anything the reader does.
- Start the body at `##`. The page title comes from frontmatter.
- Link to other pages instead of repeating them. Use relative links to the `.md` file:
  `[Install {{app}}](../getting-started/install.md)`.
- No marketing copy, no "simply", no claims you have not checked in the code or a release.
- Call the product by its display name, as the app and its downloads do. Pages that name it are `X.md.in` templates
  that write `{{app}}` (also `{{binary}}`, `{{repository}}`, `{{library_default}}`, `{{settings_dir}}`); edit the
  template and run `cargo xtask docs` to render the `.md` next to it.
- Name UI labels exactly as the app shows them, in **bold**.
- Troubleshooting pages: `## Symptoms`, `## Fix`, `## Why this happens` (optional),
  `## Still stuck?` (link the issue tracker).

## Frontmatter

    ---
    title: Raw photos look dull
    description: One sentence for search results, link previews and AI summaries.
    order: 10                 # optional, sorts within the folder (lower first)
    verified: 0.6.0           # optional, app version you last checked this page against
    redirect_from: [old/path] # optional, old URLs (relative to the docs root)
    ---

Only `key: value` and `key: [a, b]` lines work. Quote values that start with a quote or contain ` #`.

Set `verified` to the latest release (the newest release tag) after checking the page against that tag with `git show <tag>:<path>`. Pages describe the latest release users download, not unreleased code on main.

## Folders

- `getting-started/` install and first run
- `guides/` how to do one thing
- `troubleshooting/` symptom → fix
- `faq/` short answers
- `reference/` lists and settings

A folder's `index.md` names the folder in the sidebar. Files or folders starting with `_` are
drafts and are not published.

## Check before you push

    cargo xtask docs --check

Upstream also runs its website's docs checker over this folder (it lives in upstream's website repository).

## Support PRs

Pages written to answer a user question come in as PRs titled `docs(support): <page title>`.
Review them like code: check every claim against the source.
