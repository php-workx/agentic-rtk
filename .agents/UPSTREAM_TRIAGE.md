# Upstream PR Triage System

## What This Is

A repeatable, agent-friendly process for tracking open PRs on the upstream `rtk-ai/rtk` repository and deciding which ones to adopt into our fork (`php-workx/agentic-rtk`).

The upstream maintainers are slow to merge. We stay ahead by continuously evaluating their open PRs, cherry-picking the ones we want, and rejecting the ones we don't.

---

## System Components

| Component | Purpose |
|-----------|---------|
| `.agents/upstream-prs.md` | The living decision log — a markdown table of all open upstream PRs with status |
| `scripts/analyze-upstream-prs.py` | Fetches upstream PRs, categorizes them, checks overlap with fork, writes the report |
| `scripts/analyze-upstream-prs.sh` | Thin wrapper that delegates to the Python script |
| `.github/workflows/analyze-upstream.yml` | GitHub Action that refreshes the report every Monday at 06:00 UTC |

---

## Decision Workflow

```text
┌─────────────────────────────────────────────────────────────────────┐
│  STEP 1: Read the current report                                    │
│  .agents/upstream-prs.md                                            │
│                                                                     │
│  Look for PRs with Status = "pending" and pick one to evaluate.     │
└──────────────────────────────┬──────────────────────────────────────┘
                               │
                               ▼
┌─────────────────────────────────────────────────────────────────────┐
│  STEP 2: Analyze the PR                                             │
│                                                                     │
│  Run these commands to understand what it does:                     │
│                                                                     │
│  gh pr view <NUM> --repo rtk-ai/rtk                                 │
│  gh pr diff <NUM> --repo rtk-ai/rtk                                 │
│                                                                     │
│  Check: Is it already in our fork?                                │
│  git log --all --oneline --grep="#<NUM>"                            │
│                                                                     │
│  Check: Will it conflict heavily?                                   │
│  Look at the "Conflict Risk" column in the report.                  │
│  High = touches core or many files; manual merge likely needed.   │
└──────────────────────────────┬──────────────────────────────────────┘
                               │
                               ▼
┌─────────────────────────────────────────────────────────────────────┐
│  STEP 3: Make a decision                                            │
│                                                                     │
│  Update the Status column in .agents/upstream-prs.md:             │
│                                                                     │
│  - `approved`  → Good to adopt. Cherry-pick or manually port.       │
│  - `rejected`  → Skip. Add reason in Notes column.                │
│  - `partial`   → Only adopt subset. Document what's kept.         │
│  - `deferred`  → Good idea, but wait for upstream to stabilize.     │
│  - `merged`    → Already in the fork.                             │
│  - `pending`   → Default. Hasn't been evaluated yet.                │
└──────────────────────────────┬──────────────────────────────────────┘
                               │
                               ▼
┌─────────────────────────────────────────────────────────────────────┐
│  STEP 4: If approved — integrate it                                 │
│                                                                     │
│  See "Integration Process" below for the exact steps.               │
└──────────────────────────────┬──────────────────────────────────────┘
                               │
                               ▼
┌─────────────────────────────────────────────────────────────────────┐
│  STEP 5: Commit the updated report                                  │
│                                                                     │
│  git add .agents/upstream-prs.md                                    │
│  git commit -m "triage: mark PR #<NUM> as <STATUS>"                  │
│  git push                                                           │
└─────────────────────────────────────────────────────────────────────┘
```

---

## How to Evaluate a PR

### Quick analysis (5 minutes)

```bash
# 1. Read the PR description
gh pr view <NUM> --repo rtk-ai/rtk

# 2. See the diff (first 50 lines)
gh pr diff <NUM> --repo rtk-ai/rtk | head -50

# 3. Check if we already have it
git log --all --oneline --grep="#<NUM>"

# 4. Check if the same files are already modified in our fork
gh pr diff <NUM> --repo rtk-ai/rtk --name-only | while read f; do
  git diff origin/main HEAD -- "$f" >/dev/null 2>&1 && echo "CONFLICT: $f"
done
```

### Deep analysis (when conflict risk is high)

```bash
# Download the full patch for offline review
curl -sL "https://patch-diff.githubusercontent.com/raw/rtk-ai/rtk/pull/<NUM>.diff" > /tmp/pr-<NUM>.diff

# See what files it touches
cat /tmp/pr-<NUM>.diff | grep "^diff --git" | head -20

# Compare against our fork's current state for each file
for f in $(cat /tmp/pr-<NUM>.diff | grep "^diff --git" | awk '{print $3}' | sed 's/^a\///'); do
  if [ -f "$f" ]; then
    echo "=== $f ==="
    git diff origin/main HEAD -- "$f" | head -20
    echo ""
  fi
done
```

### Decision criteria

| Factor | Approve if... | Reject if... |
|--------|--------------|--------------|
| **Value** | Fixes a bug we also have, adds a command we want, or improves a filter we use | Niche feature we don't need, or already solved differently in our fork |
| **Conflict Risk** | Low (1-3 files, no core/ overlap) | High (>10 files, or touches src/core/, src/hooks/init.rs, src/main.rs) |
| **Stability** | PR is focused, well-tested, single concern | PR is a draft, has failing CI, or bundles unrelated changes |
| **Our divergence** | The code area hasn't diverged much from upstream | We've heavily rewritten the same module (e.g., tracking.rs, runner.rs) |

---

## Integration Process

### For low-conflict PRs (recommended approach: cherry-pick)

```bash
# Add upstream as a remote if not already present
git remote add upstream https://github.com/rtk-ai/rtk.git 2>/dev/null || true
git fetch upstream

# Create a feature branch for the cherry-pick
git checkout -b feat/adopt-upstream-<NUM>

# Cherry-pick the PR's commits (use the merge commit if available)
# If the PR hasn't been merged yet, cherry-pick from the PR branch:
gh pr checkout <NUM> --repo rtk-ai/rtk --branch tmp-upstream-<NUM>
# Then cherry-pick:
git cherry-pick <COMMIT_HASH>

# Or, if the PR has a single commit and you want the patch directly:
git am < <(curl -sL "https://patch-diff.githubusercontent.com/raw/rtk-ai/rtk/pull/<NUM>.patch")

# Resolve any conflicts, then test
cargo test
just test-presence

# Commit the merge resolution if needed
git add -A && git commit

# Push
git push origin feat/adopt-upstream-<NUM>
```

### For high-conflict PRs (recommended approach: manual port)

```bash
# 1. Read the PR diff thoroughly
git diff upstream/master...upstream/<PR_BRANCH> -- <FILE> > /tmp/pr-change.patch

# 2. Apply the conceptual change to our fork's version of the file
# Don't blindly apply the patch — our fork may have diverged.
# Instead, read the patch, understand the fix, then re-implement it
# in our codebase using our patterns and variable names.

# 3. Test
cargo test
just pre-commit

# 4. Commit with attribution
git commit -m "feat: adopt upstream PR #<NUM> — <TITLE>

Original: https://github.com/rtk-ai/rtk/pull/<NUM>
Author: <AUTHOR>

<Brief description of what was adopted and what was adapted>"
```

### After integration

```bash
# Update the report
git checkout feat/context-zip  # or wherever the report lives
# Edit .agents/upstream-prs.md: change Status to `merged`
git add .agents/upstream-prs.md
git commit -m "triage: mark PR #<NUM> as merged"
git push
```

---

## Report Format

The `.agents/upstream-prs.md` table has these columns:

| Column | Meaning |
|--------|---------|
| **PR** | GitHub PR number (linkable) |
| **Title** | PR title (truncated for display) |
| **Author** | Upstream contributor |
| **Files** | Number of changed files |
| **Category** | `feature`, `bugfix`, `refactor`, `docs`, `chore`, `ci`, `test`, `perf`, `deps`, `other` |
| **Conflict Risk** | `low` (1-3 files, no core), `medium` (4-10 files or touches core), `high` (>10 files or heavy core overlap) |
| **Status** | `pending`, `approved`, `rejected`, `partial`, `deferred`, `merged` |
| **Notes** | Free-form context — body summary, your reasoning, or links |

### Status meanings

| Status | When to use |
|--------|-------------|
| `pending` | Default. PR hasn't been evaluated yet. |
| `approved` | We want this. Ready to cherry-pick or manually port. |
| `rejected` | We don't want this. Add reason in Notes. |
| `partial` | Only adopting a subset. Document what's kept in Notes. |
| `deferred` | Good idea, but wait for upstream to stabilize or for us to have bandwidth. |
| `merged` | Already integrated into our fork. |

---

## Automation

### Weekly refresh

The GitHub Action `.github/workflows/analyze-upstream.yml` runs every Monday at 06:00 UTC and:
1. Fetches the latest open PRs from upstream
2. Regenerates `.agents/upstream-prs.md`
3. Commits and pushes if there are new PRs or status changes

### Manual refresh

```bash
# Refresh the report now
python3 scripts/analyze-upstream-prs.py

# Or use the shell wrapper
bash scripts/analyze-upstream-prs.sh
```

The script is idempotent — running it multiple times produces the same output unless upstream has new PRs.

---

## Agent Handoff Checklist

When you stop working and another agent might resume, ensure:

- [ ] `.agents/upstream-prs.md` is committed and pushed
- [ ] Any in-progress PR integrations are on a feature branch (not `feat/context-zip`)
- [ ] The status of any partially-evaluated PRs is updated in the report
- [ ] Notes column explains what was left unfinished

Example note format:

```markdown
| #1696 | fix(tee): keep head + tail... | iliaal | 1 | bugfix | low | **approved** | Ready to cherry-pick. Agent stopped before creating branch. |
```

---

## Current Priorities (as of last report)

These are the kinds of PRs we prioritize:

1. **Bugfixes that affect us** — especially in `src/core/`, `src/cmds/`, `src/hooks/`
2. **New commands we actively use** — e.g., new language support (PHP, C++, etc.)
3. **Performance improvements** — anything that reduces overhead or improves filtering
4. **CI/build improvements** — reproducible builds, faster tests, better linting

We deprioritize:

1. **Upstream-only features** — things tied to `rtk-ai.app` website, telemetry, or Discord
2. **Features we've already solved differently** — e.g., our Context Zip postprocessors vs upstream's approach
3. **Draft PRs or WIP** — wait until they're stable

---

## Quick Reference

```bash
# View upstream PR
gh pr view <NUM> --repo rtk-ai/rtk

# Diff upstream PR
git fetch upstream
git diff upstream/master...upstream/<BRANCH>

# Check if in fork
git log --all --oneline --grep="#<NUM>"

# Download patch
curl -sL "https://patch-diff.githubusercontent.com/raw/rtk-ai/rtk/pull/<NUM>.diff" | head -50

# Cherry-pick from upstream PR branch
git fetch upstream pull/<NUM>/head:pr-<NUM>
git cherry-pick pr-<NUM>

# Refresh report
python3 scripts/analyze-upstream-prs.py

# Check for conflicts between PR and our fork
gh pr diff <NUM> --repo rtk-ai/rtk --name-only | while read f; do
  git diff origin/main HEAD -- "$f" >/dev/null 2>&1 && echo "CONFLICT: $f"
done
```

---

## Last Updated

This file was generated on: **2026-05-04**

Next scheduled refresh: **Monday 06:00 UTC** (via GitHub Actions)
