# Contributor credits

> Names in angle brackets (`<app>`, `<binary>`, `<cli>`, `<PREFIX>`, `<settings_dir>`, …) are the values set in
> [`brand.toml`](../brand.toml); see the README.

The contributor records credit everyone who contributed to the app, and the AI models named in
`Co-Authored-By` trailers. This page is the decision.

## Decision

- Two files in [`contributors/`](../contributors/):
  - [`contributors/contributors.json`](../contributors/contributors.json): commit stats per GitHub
    username. **Generated** by `contributors/fork-contributors.py`; never hand-edit.
  - [`contributors/people.toml`](../contributors/people.toml): the names people chose for
    themselves. **Each contributor adds only their own entry**, in their own PR.
- Each contributor gets one record: GitHub username (always), display name (opt-in, or their synced
  GitHub profile name), real name (only if they told us), merged PRs, commits, lines added, lines
  deleted, binary assets added, binary assets removed, first and last commit date.

## In the app

- Since 2026-11-15 the app no longer shows these lists: Help ▸ About <app> shows only the app's
  name, version, licences and links. The files are kept up to date for the repository's own
  credits.

## Credit yourself

Add your entry to `contributors/people.toml` in a PR you commit yourself (every field optional):

```toml
[people.your-github-username]
real_name = "Your Name"
display_name = "Nickname"
sync_github_name = true   # use the name on your public GitHub profile as display name
```

Names show after the next refresh: the script checks that each entry was committed by its own
user (or by a maintainer with a `consent_source`). Delete your entry to remove your names.

## Refreshing (maintainers)

```sh
python3 contributors/fork-contributors.py   # regenerate contributors.json; needs `gh` signed in and the `upstream` remote fetched
```

Commit the updated `contributors/contributors.json`. Git author names and emails are never recorded.
