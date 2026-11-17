#!/usr/bin/env python3
"""List upstream pull requests for one intake round and validate the intake ledger.

The procedure is docs/upstream-pr-intake.md. This script only reads: it asks `gh` for the upstream
repository's pull requests, marks the files that touch fork-owned paths, and cross-references
docs/upstream-pr-reviews/ledger.md. It never decides, writes to the repository or pushes.

Usage:
  tools/upstream_intake.py enumerate [--since YYYY-MM-DD] [--states open,closed,merged]
                                     [--limit 300] [--max-files 20] [--out FILE] [--all]
  tools/upstream_intake.py check [--ledger FILE]

`enumerate` needs `gh`, signed in to a token that can read the upstream repository. `check` is offline.
"""

import argparse
import datetime as dt
import fnmatch
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LEDGER = ROOT / "docs" / "upstream-pr-reviews" / "ledger.md"
OWNED = ROOT / "upstream-owned.txt"

COLUMNS = ["item", "state", "title", "decision", "recheck", "reason", "upstream_ref", "reviewed", "fork"]
STATES = {"open", "closed", "merged", "commit"}
DECISIONS = {"ADOPT", "LEARN", "SKIP", "PENDING", "WATCH"}

# ---------------------------------------------------------------- helpers


def fail(message):
    print(f"upstream_intake: {message}", file=sys.stderr)
    return 2


def gh(args):
    """Run gh and return stdout, raising RuntimeError with its stderr on failure."""
    try:
        done = subprocess.run(["gh", *args], capture_output=True, text=True, encoding="utf-8", errors="replace")
    except FileNotFoundError as exc:
        raise RuntimeError("gh is not installed") from exc
    if done.returncode != 0:
        raise RuntimeError(done.stderr.strip() or f"gh exited {done.returncode}")
    return done.stdout


def upstream_repo():
    """owner/repo of the `upstream` remote."""
    try:
        url = subprocess.run(
            ["git", "-C", str(ROOT), "config", "--get", "remote.upstream.url"],
            capture_output=True,
            text=True,
        ).stdout.strip()
    except OSError:
        url = ""
    match = re.search(r"[:/]([^/:]+/[^/]+?)(?:\.git)?$", url)
    if not match:
        raise RuntimeError("no `upstream` git remote; add it (docs/upstream-merge.md)")
    return match.group(1)


# ---------------------------------------------------------------- upstream-owned.txt


def owned_patterns(path=OWNED):
    patterns, section = [], None
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        if line.startswith("[") and line.endswith("]"):
            section = line[1:-1]
            continue
        if section == "owned":
            patterns.append(line)
    return patterns


def _segments(text):
    return [part for part in text.split("/") if part]


def _match_segments(pattern, parts):
    if not pattern:
        return not parts
    if pattern[0] == "**":
        return any(_match_segments(pattern[1:], parts[skip:]) for skip in range(len(parts) + 1))
    if not parts:
        return False
    return fnmatch.fnmatchcase(parts[0], pattern[0]) and _match_segments(pattern[1:], parts[1:])


def is_owned(path, patterns):
    """Match a repository-relative path against the manifest, gitignore-style."""
    parts = _segments(path)
    for pattern in patterns:
        anchored = "/" in pattern
        is_dir = pattern.endswith("/")
        segs = _segments(pattern.strip("/"))
        if not segs:
            continue
        if not anchored:
            if parts and fnmatch.fnmatchcase(parts[-1], segs[0]):
                return True
        elif is_dir:
            if any(_match_segments(segs, parts[:n]) for n in range(1, len(parts))):
                return True
        elif _match_segments(segs, parts):
            return True
    return False


# ---------------------------------------------------------------- ledger


def split_row(line):
    """Split a markdown table row on unescaped pipes; unescape `\\|`."""
    text = line.strip()
    if text.startswith("|"):
        text = text[1:]
    if text.endswith("|"):
        text = text[:-1]
    cells, current, escaped = [], [], False
    for char in text:
        if escaped:
            current.append(char)
            escaped = False
        elif char == "\\":
            escaped = True
        elif char == "|":
            cells.append("".join(current).strip())
            current = []
        else:
            current.append(char)
    cells.append("".join(current).strip())
    return cells


def parse_ledger(path=LEDGER):
    rows = []
    if not path.is_file():
        raise RuntimeError(f"no ledger at {path}")
    header_seen = False
    for line in path.read_text(encoding="utf-8").splitlines():
        if header_seen and line.startswith("## "):
            break
        if not line.lstrip().startswith("|"):
            continue
        cells = split_row(line)
        if not header_seen:
            header_seen = [cell.lower().replace(" ", "_") for cell in cells] == COLUMNS
            continue
        if all(set(cell) <= {"-", ":"} for cell in cells if cell):
            continue
        if not cells or not cells[0]:
            continue
        values = cells[: len(COLUMNS)] + [""] * max(0, len(COLUMNS) - len(cells))
        rows.append(dict(zip(COLUMNS, values)))
    if not header_seen:
        raise RuntimeError("ledger header does not match the documented columns")
    return rows


def pr_number(item):
    match = re.search(r"/pull/(\d+)", item) or re.search(r"#(\d+)", item)
    return int(match.group(1)) if match else None


def check_ledger(rows):
    errors, warnings = [], []
    seen = {}
    for index, row in enumerate(rows, start=2):
        label = f"line {index}: {row['item'][:60]}"
        number = pr_number(row["item"])
        if row["state"] not in STATES:
            errors.append(f"{label}: state {row['state']!r} is not one of {sorted(STATES)}")
        if row["decision"] not in DECISIONS:
            errors.append(f"{label}: decision {row['decision']!r} is not one of {sorted(DECISIONS)}")
        if not row["reason"]:
            errors.append(f"{label}: empty reason")
        if row["decision"] == "ADOPT" and row["fork"] in ("", "-"):
            errors.append(f"{label}: ADOPT without a fork commit or PR")
        if row["decision"] in ("WATCH", "PENDING") and row["recheck"] in ("", "-"):
            errors.append(f"{label}: {row['decision']} without a recheck trigger")
        if row["state"] in ("merged", "commit") and len(row["upstream_ref"].strip("`")) < 7:
            errors.append(f"{label}: merged/commit row without a reviewed upstream SHA")
        if number is not None:
            if number in seen:
                errors.append(f"{label}: #{number} already on line {seen[number]}")
            seen[number] = index
        if row["decision"] == "ADOPT" and row["state"] == "open":
            warnings.append(f"{label}: ADOPT of an open PR; re-check its head before landing")
        match = re.match(r"(\d{4}-\d{2}-\d{2})", row["reviewed"])
        if row["decision"] in ("WATCH", "PENDING") and match:
            age = (dt.date.today() - dt.date.fromisoformat(match.group(1))).days
            if age > 14:
                warnings.append(f"{label}: {row['decision']} for {age} days")
    return errors, warnings


# ---------------------------------------------------------------- enumerate


def pr_files(repo, number, max_files):
    names = gh(["api", "--paginate", f"repos/{repo}/pulls/{number}/files", "--jq", ".[].filename"]).splitlines()
    names = [name for name in names if name]
    return names[:max_files] if max_files else names


def short(sha):
    return (sha or "")[:10]


def heading(pr, repo):
    number = pr["number"]
    title = pr["title"].replace("\n", " ")
    return f"### [#{number}](https://github.com/{repo}/pull/{number}) {title}"


def describe(pr, repo, max_files, ledger_row, patterns, lookup_budget):
    lines = [heading(pr, repo)]
    draft = "draft" if pr.get("isDraft") else "ready"
    review = pr.get("reviewDecision") or "-"
    merged = f" · merged {pr['mergedAt'][:10]}" if pr.get("mergedAt") else ""
    lines.append(
        f"- {pr['state'].lower()} · {draft} · review {review} · updated {pr['updatedAt'][:10]}"
        f" · head `{short(pr.get('headRefOid'))}`{merged}"
    )
    if ledger_row:
        lines.append(
            f"- ledger: **{ledger_row['decision']}** · recheck {ledger_row['recheck']} · "
            f"reviewed {ledger_row['reviewed']} · fork {ledger_row['fork']}"
        )
    files = []
    if max_files and lookup_budget[0] > 0:
        lookup_budget[0] -= 1
        try:
            files = pr_files(repo, pr["number"], max_files)
        except RuntimeError as exc:
            lines.append(f"- files: unavailable ({exc})")
    if files:
        owned = sum(1 for name in files if is_owned(name, patterns))
        lines.append(f"- files: {len(files)}, owned-only: {'yes' if owned == len(files) else 'no'}")
        lines.extend(f"  - `{name}`{' (ours)' if is_owned(name, patterns) else ''}" for name in files)
    return lines


def enumerate_cmd(args):
    if not shutil.which("gh"):
        return fail("gh is not installed")
    try:
        repo = upstream_repo()
    except RuntimeError as exc:
        return fail(str(exc))
    since = args.since or (dt.date.today() - dt.timedelta(days=7)).isoformat()
    wanted = {state.strip() for state in args.states.split(",") if state.strip()}
    try:
        main_sha = gh(["api", f"repos/{repo}/commits/main", "--jq", ".sha"]).strip()
        fields = "number,title,state,isDraft,mergedAt,updatedAt,headRefOid,author,reviewDecision"
        found = []
        if "open" in wanted:
            # every open PR, not only the recently updated ones: an old, quiet open PR may never have been read
            found.extend(
                json.loads(
                    gh(
                        [
                            "pr",
                            "list",
                            "--repo",
                            repo,
                            "--state",
                            "open",
                            "--limit",
                            str(args.limit),
                            "--json",
                            fields,
                        ]
                    )
                )
            )
        found.extend(
            json.loads(
                gh(
                    [
                        "pr",
                        "list",
                        "--repo",
                        repo,
                        "--state",
                        "all",
                        "--limit",
                        str(args.limit),
                        "--search",
                        f"updated:>={since}",
                        "--json",
                        fields,
                    ]
                )
            )
        )
        unique = {}
        for pr in found:
            unique[pr["number"]] = pr
        found = list(unique.values())
    except RuntimeError as exc:
        return fail(str(exc))
    rows = {}
    try:
        rows = {pr_number(row["item"]): row for row in parse_ledger(Path(args.ledger)) if pr_number(row["item"])}
    except RuntimeError as exc:
        print(f"upstream_intake: warning: {exc}", file=sys.stderr)
    candidates = [pr for pr in found if pr["state"].lower() in wanted]
    candidates.sort(key=lambda pr: pr["number"])
    new = [pr for pr in candidates if pr["number"] not in rows]
    known = [pr for pr in candidates if pr["number"] in rows]
    patterns = owned_patterns()
    budget = [args.max_lookups]
    lines = [
        f"# Upstream intake candidates — {dt.date.today().isoformat()}",
        "",
        f"- upstream: `{repo}` at `{short(main_sha)}`",
        f"- since: {since} · states: {', '.join(sorted(wanted))} · ledger: {len(rows)} PR rows",
        f"- new: {len(new)} · already in the ledger: {len(known)}",
        "",
    ]
    if new:
        lines.append("## New (no ledger row)")
        lines.append("")
        for pr in new:
            lines.extend(describe(pr, repo, args.max_files, None, patterns, budget))
            lines.append("")
    if known and args.all:
        lines.append("## In the ledger")
        lines.append("")
        for pr in known:
            lines.extend(describe(pr, repo, args.max_files, rows[pr["number"]], patterns, budget))
            lines.append("")
    elif known:
        lines.append(f"## In the ledger ({len(known)} not shown; pass --all)")
        lines.append("")
        for pr in known:
            lines.append(
                f"- [#{pr['number']}](https://github.com/{repo}/pull/{pr['number']}) "
                f"{rows[pr['number']]['decision']}"
            )
        lines.append("")
    text = "\n".join(lines).rstrip() + "\n"
    if args.out:
        out = Path(args.out)
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(text, encoding="utf-8")
        print(f"{out}: {len(new)} new, {len(known)} in the ledger, upstream {short(main_sha)}")
    else:
        sys.stdout.write(text)
    return 0


def check_cmd(args):
    try:
        rows = parse_ledger(Path(args.ledger))
    except RuntimeError as exc:
        return fail(str(exc))
    errors, warnings = check_ledger(rows)
    for warning in warnings:
        print(f"warning: {warning}", file=sys.stderr)
    for error in errors:
        print(f"error: {error}", file=sys.stderr)
    print(f"{len(rows)} rows, {len(errors)} errors, {len(warnings)} warnings")
    return 1 if errors else 0


def drift_cmd(args):
    """Waiting rows (PENDING/WATCH) whose upstream PR moved since review: merged, closed, approved, head changed,
    or out of draft. Read-only: it reports, it never decides (docs/upstream-pr-intake.md, Automation)."""
    if not shutil.which("gh"):
        return fail("gh is not installed")
    try:
        repo = upstream_repo()
        rows = parse_ledger(Path(args.ledger))
    except RuntimeError as exc:
        return fail(str(exc))
    waiting = {"PENDING", "WATCH"} | ({"LEARN", "ADOPT"} if args.all else set())
    watched = [r for r in rows if r["decision"] in waiting and pr_number(r["item"]) is not None]
    try:
        fields = "number,title,state,isDraft,headRefOid,reviewDecision,mergedAt,updatedAt"
        live = json.loads(gh(["pr", "list", "--repo", repo, "--state", "all", "--limit", str(args.limit), "--json", fields]))
    except (RuntimeError, ValueError) as exc:
        return fail(f"gh pr list: {exc}")
    by_number = {pr["number"]: pr for pr in live}
    groups = {"merged": [], "closed": [], "approved": [], "out of draft": [], "head moved": [], "not found": []}
    quiet = 0
    for row in watched:
        number = pr_number(row["item"])
        pr = by_number.get(number)
        if pr is None:
            groups["not found"].append((row, None))
            continue
        ref = row["upstream_ref"].strip("`").strip()
        head = pr.get("headRefOid") or ""
        moved = bool(ref) and not head.startswith(ref[:10])
        state = (pr.get("state") or "").lower()
        if state == "merged" and row["state"] != "merged":
            groups["merged"].append((row, pr))
        elif state == "closed" and row["state"] != "closed":
            groups["closed"].append((row, pr))
        elif pr.get("reviewDecision") == "APPROVED" and "approv" not in row["reason"].lower():
            groups["approved"].append((row, pr))
        elif moved:
            groups["head moved"].append((row, pr))
        elif not pr.get("isDraft") and "draft" in row["reason"].lower():
            groups["out of draft"].append((row, pr))
        else:
            quiet += 1
    lines = [f"# Upstream drift: {len(watched)} waiting rows", "", f"- upstream: `{repo}` · unchanged: {quiet}", ""]
    for name, items in groups.items():
        if not items:
            continue
        lines += [f"## {name.capitalize()} ({len(items)})", ""]
        for row, pr in items:
            head = short(pr.get("headRefOid") or "") if pr else "-"
            title = (pr or {}).get("title") or row["title"]
            lines.append(
                f"- [#{pr_number(row['item'])}](https://github.com/{repo}/pull/{pr_number(row['item'])}) {title} — "
                f"{row['decision']}, reviewed `{row['upstream_ref'].strip('`')}` → now `{head}`; recheck: {row['recheck']}"
            )
        lines.append("")
    text = "\n".join(lines)
    if args.out:
        Path(args.out).parent.mkdir(parents=True, exist_ok=True)
        Path(args.out).write_text(text, encoding="utf-8")
        print(f"{args.out}: {len(watched) - quiet} of {len(watched)} waiting rows moved")
    else:
        print(text)
    return 0


def main(argv):
    parser = argparse.ArgumentParser(description="Upstream PR intake helper (docs/upstream-pr-intake.md)")
    sub = parser.add_subparsers(dest="command", required=True)

    enum = sub.add_parser("enumerate", help="list candidate upstream PRs (all open, plus recently updated)")
    enum.add_argument("--since", help="inclusive update date (YYYY-MM-DD); default 7 days ago")
    enum.add_argument("--states", default="open,closed,merged", help="comma-separated states")
    enum.add_argument("--limit", type=int, default=300, help="PRs to ask gh for (default 300)")
    enum.add_argument("--max-files", type=int, default=20, help="files per PR, 0 disables file lookups")
    enum.add_argument("--max-lookups", type=int, default=60, help="PRs whose files are fetched (default 60)")
    enum.add_argument("--ledger", default=str(LEDGER))
    enum.add_argument("--out", help="write the report here instead of stdout")
    enum.add_argument("--all", action="store_true", help="also list PRs already in the ledger")

    checker = sub.add_parser("check", help="validate the ledger")
    checker.add_argument("--ledger", default=str(LEDGER))

    drift = sub.add_parser("drift", help="waiting rows whose upstream PR moved since review (head, state, approval)")
    drift.add_argument("--ledger", default=str(LEDGER))
    drift.add_argument("--limit", type=int, default=6000, help="PRs to ask gh for (default 6000)")
    drift.add_argument("--all", action="store_true", help="also LEARN and ADOPT rows")
    drift.add_argument("--out", help="write the report here instead of stdout")

    args = parser.parse_args(argv)
    commands = {"check": check_cmd, "enumerate": enumerate_cmd, "drift": drift_cmd}
    try:
        return commands[args.command](args)
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
