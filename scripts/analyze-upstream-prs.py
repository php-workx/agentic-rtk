#!/usr/bin/env python3
"""Upstream PR Analyzer for RTK fork.

Fetches open PRs from rtk-ai/rtk, categorizes them, checks for overlap
with the fork, and writes a markdown triage report to .agents/upstream-prs.md.

Usage:
    python3 scripts/analyze-upstream-prs.py
"""

import json
import os
import re
import subprocess
import sys
from datetime import datetime, timezone

UPSTREAM = "rtk-ai/rtk"
REPORT_FILE = ".agents/upstream-prs.md"


def run(cmd: str) -> str:
    result = subprocess.run(cmd, shell=True, capture_output=True, text=True)
    if result.returncode != 0:
        raise subprocess.CalledProcessError(
            result.returncode, cmd, output=result.stdout, stderr=result.stderr
        )
    return result.stdout.strip()


def escape_md_cell(value: str) -> str:
    return value.replace("|", "\|").replace("
", " ").replace("", " ")


def categorize(title: str) -> str:
    if re.match(r"^feat", title, re.I):
        return "feature"
    if re.match(r"^fix", title, re.I):
        return "bugfix"
    if re.match(r"^refact", title, re.I):
        return "refactor"
    if re.match(r"^docs", title, re.I):
        return "docs"
    if re.match(r"^chore", title, re.I):
        return "chore"
    if re.match(r"^ci\b", title, re.I):
        return "ci"
    if re.match(r"^test", title, re.I):
        return "test"
    if re.match(r"^perf", title, re.I):
        return "perf"
    if re.match(r"^build\b", title, re.I):
        return "deps"
    return "other"


def conflict_risk(files: int, title: str, body: str) -> str:
    risk = "low"
    if files > 10:
        risk = "high"
    elif files > 5:
        risk = "medium"
    combined = f"{title} {body}"
    if "src/core/" in combined or "src/main.rs" in combined:
        risk = "medium" if risk == "low" else risk
    if "src/hooks/" in combined and "init.rs" in combined:
        risk = "high"
    return risk


def already_in_fork(pr_num: int) -> bool:
    out = run(f'git log --all --oneline --grep="#{pr_num}\\b" 2>/dev/null | head -1')
    return bool(out)


def main() -> int:
    print(f"== Upstream PR Analyzer ==")
    print(f"Upstream: {UPSTREAM}")

    try:
        prs_json = run(
            f'gh pr list --repo {UPSTREAM} --state open --limit 50 '
            f'--json number,title,author,createdAt,changedFiles,headRefName,body'
        )
    except subprocess.CalledProcessError as e:
        print(f"Error fetching PRs: {e.stderr or e.output or str(e)}", file=sys.stderr)
        return 1
    if not prs_json:
        print("No PR data returned")
        return 0

    prs = json.loads(prs_json)
    if not prs:
        print("No open upstream PRs")
        return 0

    print(f"Found {len(prs)} open upstream PRs")

    lines = [
        "# Upstream PR Triage — " + datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC"),
        "",
        "| PR | Title | Author | Files | Category | Conflict Risk | Status | Notes |",
        "|----|-------|--------|-------|----------|---------------|--------|-------|",
    ]

    # Load existing statuses from current report so manual triage isn't overwritten
    existing_statuses = {}
    if os.path.exists(REPORT_FILE):
        with open(REPORT_FILE, "r") as f:
            for line in f:
                if line.startswith("| #"):
                    parts = [p.strip() for p in line.split("|")]
                    if len(parts) >= 8:
                        pr_num_str = parts[1].lstrip("#")
                        pr_status = parts[7]
                        if pr_status in {"approved", "rejected", "partial", "deferred", "merged", "pending"}:
                            existing_statuses[pr_num_str] = pr_status

    for pr in prs:
        num = pr["number"]
        title = pr["title"]
        author = pr["author"]["login"]
        files = pr["changedFiles"]
        body = pr.get("body", "") or ""

        cat = categorize(title)
        risk = conflict_risk(files, title, body)
        adopted = already_in_fork(num)

        num_str = str(num)
        if adopted:
            status = "merged"
            notes = "Already in fork"
        elif num_str in existing_statuses and existing_statuses[num_str] != "pending":
            status = existing_statuses[num_str]
            notes = ""
        else:
            status = "pending"
            notes = ""

        # First line of body as extra context
        body_summary = ""
        if body:
            first = body.splitlines()[0].strip()
            if first and not first.startswith("#"):
                body_summary = first[:60]

        title_short = (title[:52] + "...") if len(title) > 55 else title
        notes_full = (notes + ", " + body_summary) if notes and body_summary else (notes or body_summary)

        lines.append(
            f"| #{num} | {escape_md_cell(title_short)} | {escape_md_cell(author)} | {files} | {cat} | {risk} | {status} | {escape_md_cell(notes_full)} |"
        )

    lines.extend([
        "",
        "## How to use this report",
        "",
        "1. **Review each PR** — `gh pr view <num> --repo rtk-ai/rtk`",
        "2. **Check if already adopted** — `git log --all --oneline --grep=\"#<num>\"`",
        "3. **Assess conflict risk** — High = many files or touches core; will need manual merge.",
        "4. **Make a decision** — Update Status: `approved` | `rejected` | `partial` | `deferred`",
        "5. **After adopting** — Update this file and commit it.",
        "",
        "## Categories",
        "",
        "- **feature** — New commands, filters, or major functionality",
        "- **bugfix** — Fixes for existing behavior",
        "- **refactor** — Internal restructuring, no user-visible change",
        "- **docs** — Documentation, README, help text",
        "- **chore** — Build, CI, deps, linting",
        "- **test** — Test-only changes",
        "- **perf** — Performance improvements",
        "- **deps** — Dependabot bumps",
        "- **other** — Uncategorized",
        "",
        "## Quick commands",
        "",
        "```bash",
        "# View a specific PR",
        "gh pr view 1696 --repo rtk-ai/rtk",
        "",
        "# Download patch",
        "curl -sL https://patch-diff.githubusercontent.com/raw/rtk-ai/rtk/pull/1696.diff | head -50",
        "",
        "# Check if already in fork",
        "git log --all --oneline --grep=\"1696\"",
        "```",
    ])

    os.makedirs(os.path.dirname(REPORT_FILE), exist_ok=True)
    with open(REPORT_FILE, "w") as f:
        f.write("\n".join(lines) + "\n")

    print(f"\nReport written to: {REPORT_FILE}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
