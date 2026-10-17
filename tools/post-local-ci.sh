#!/usr/bin/env bash
# Run `cargo xtask ci` on the current commit and, when it passes, post the required
# "Linux (cargo xtask ci)" check to GitHub as a commit status, so a PR can merge without
# waiting for the runner. Refuses with uncommitted changes (the status must match the code
# that was tested). See docs/ci.md.
set -euo pipefail
repo="${REPO:-$(git remote get-url origin | sed -E 's#.*github.com[:/]##; s#\.git$##')}"
context="Linux (cargo xtask ci)"
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  echo "uncommitted changes: commit first, the status is posted for HEAD" >&2
  exit 1
fi
sha="$(git rev-parse HEAD)"
if ! git branch -r --contains "$sha" | grep -q .; then
  echo "HEAD $sha is not pushed yet: push the branch first" >&2
  exit 1
fi
start=$(date +%s)
if cargo xtask ci; then
  state=success; desc="local cargo xtask ci passed in $(( ($(date +%s) - start) / 60 )) min on $(hostname)"
else
  state=failure; desc="local cargo xtask ci failed on $(hostname)"
fi
gh api -X POST "repos/$repo/statuses/$sha" -f state="$state" -f context="$context" -f description="$desc" >/dev/null
echo "$state posted for $sha: $desc"
[ "$state" = success ]
