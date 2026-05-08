# FORK.md — Agentic RTK Fork Registry

This document is the single source of truth for "what did the fork change?"
Every edit to an upstream file is registered here. New contributors read this
file, not the raw diff against upstream.

## Sentinel Marker

All fork edits to upstream files use the marker:

```
AGENTIC-RTK-FORK
```

Grep for it to enumerate the entire fork delta:

```bash
grep -rn "AGENTIC-RTK-FORK" src/ .github/ scripts/
```

## Fork-Owned Files (No Sentinel Needed)

These files were created by the fork and do not exist in upstream:

| File | Description |
|------|-------------|
| `src/core/postprocess/mod.rs` | Context Zip postprocessor orchestration |
| `src/core/postprocess/stacktrace.rs` | Stacktrace compressor |
| `src/core/postprocess/package_install.rs` | Package-install log compressor |
| `src/core/postprocess/build_group.rs` | Build-group compiler error compressor |
| `src/core/postprocess/web_extract.rs` | Web-extract HTML chrome stripper |
| `src/core/noise.rs` | Noise reduction helpers |
| `src/session/mod.rs` | Session compaction (Claude/Codex transcripts) |
| `src/cmds/cloud/web_cmd.rs` | `rtk web` command |
| `src/fork/` | Fork-owned gate and feature modules |
| `.agents/` | Agent documentation, triage, research |
| `scripts/analyze-upstream-prs.py` | Upstream PR analyzer |
| `.github/workflows/analyze-upstream.yml` | Weekly upstream PR triage CI |
| `Formula/rtk.rb` | Homebrew formula |
| `install.sh` | One-line installer |
| `justfile` | Task runner |
| `.pre-commit-config.yaml` | Pre-commit hooks |

## Registry — Upstream File Edits

### Core Engine

| File | Reason | Gate |
|------|--------|------|
| `src/core/mod.rs` | Add `noise` and `postprocess` module declarations | `n/a` (structural) |
| `src/core/runner.rs` | Add `postprocessors` and `failure_fallback` to `RunOptions`; integrate Context Zip postprocessing into command output pipeline | `rtk_fork::postprocess_enabled()` |
| `src/core/filter.rs` | Add `Whitespace` filter level for Context Zip whitespace-safe mode | `rtk_fork::postprocess_enabled()` |
| `src/core/stream.rs` | Fix off-by-one in raw capture buffer cap logic; add `#[allow(dead_code)]` for fork-only surfaces | `n/a` (bugfix + structural) |
| `src/core/tracking.rs` | Add `session_compactions` table, `cleanup_old()` integration, `project_filter_params()` canonicalization | `rtk_fork::session_compaction_enabled()` |
| `src/core/utils.rs` | Add cross-platform warning suppression for `resolve_binary()` | `n/a` (cross-platform fix) |

### Command Dispatch

| File | Reason | Gate |
|------|--------|------|
| `src/main.rs` | Add `Commands::Web` variant; wire `web_cmd::run`; add `is_operational_command` test | `rtk_fork::web_command_enabled()` |
| `src/cmds/cloud/container.rs` | Minor fork-specific container command tweaks | `n/a` |
| `src/cmds/js/npm_cmd.rs` | Minor npm command enhancements | `n/a` |
| `src/cmds/js/pnpm_cmd.rs` | Minor pnpm command enhancements | `n/a` |
| `src/cmds/python/pip_cmd.rs` | Minor pip command enhancements | `n/a` |
| `src/cmds/rust/cargo_cmd.rs` | Minor cargo command enhancements | `n/a` |
| `src/cmds/rust/runner.rs` | Minor cargo runner enhancements | `n/a` |
| `src/cmds/system/grep_cmd.rs` | Minor grep command enhancements | `n/a` |

### Hook System

| File | Reason | Gate |
|------|--------|------|
| `src/hooks/constants.rs` | Add `STOP_KEY`, `SESSION_END_KEY`, `CLAUDE_SESSION_HOOK_COMMAND`, `CLAUDE_STOP_HOOK_COMMAND`, `CODEX_SESSION_HOOK_COMMAND` | `n/a` (constants) |
| `src/hooks/init.rs` | Add Codex mode, session-compaction hook installation, `uninstall_codex_at()` hooks.json cleanup, granular `remove_session_hook_entries()` | `rtk_fork::session_compaction_enabled()` |

### Command Discovery / Rewrite

| File | Reason | Gate |
|------|--------|------|
| `src/discover/registry.rs` | Add `head`/`tail` line-range rewrite, `cat`→`rtk read` rewrite, whitespace-safe source file extensions, shared `parse_cat_command` helper | `rtk_fork::command_rewrite_enabled()` |
| `src/discover/lexer.rs` | Add `#[allow(dead_code)]` for `strip_quotes()` used by fork features | `n/a` (structural) |
| `src/discover/README.md` | Minor documentation updates | `n/a` (docs) |

### Analytics

| File | Reason | Gate |
|------|--------|------|
| `src/analytics/gain.rs` | Minor analytics enhancements | `n/a` |

## Audit

Run the fork audit script before every merge:

```bash
./scripts/fork-audit
```

It cross-checks sentinels against this registry and fails on drift.

## Upstream Update Workflow

When a new upstream version is released (e.g., v0.39.0), follow this process:

### 1. Fetch and assess

```bash
git fetch upstream --tags
gh release view vX.Y.Z --repo rtk-ai/rtk    # get release notes
git log --oneline v<current>..v<target>          # count commits
git diff --stat v<current>..v<target>           # scope of changes
```

### 2. Merge (not rebase)

Use `--no-commit` to inspect and resolve conflicts before committing:

```bash
git checkout -b adopt/upstream-X.Y.Z
git merge vX.Y.Z --no-commit
```

Rebase rewrites history and makes sentinels harder to track. Merge preserves
a clear dual-parent commit showing exactly what came from upstream.

### 3. Resolve conflicts

Strategy for each conflict:

- **Accept upstream** for new features/bugfixes (upstream owns that code now)
- **Preserve fork** additions marked with `// FORK:` or `AGENTIC-RTK-FORK`
- **Keep both** when upstream and fork changes serve different purposes
- **Remove fork code** only when upstream supersedes it entirely (same feature,
  upstream version is the replacement)

Mark every resolution with a brief `// FORK:` note if it's non-obvious why
fork code was kept or removed.

### 4. Overlap audit (mandatory)

After merge, check whether upstream accepted PRs the fork previously adopted.
Duplicate code is a merge artifact that compiles but wastes tokens and confuses
future contributors.

```text
1. Extract upstream PR numbers from release notes
2. Cross-reference against fork CHANGELOG "Upstream Adopts" entries
3. For each overlap:
   - If code is identical → remove fork's duplicate, upstream owns it now
   - If code diverged → verify fork variant still adds value; if not, adopt upstream's
   - If code is additive → keep both (different features)
4. Remove stale `// FORK:` markers that no longer mark fork-owned code
5. Remove dead code from merge artifacts (e.g., duplicate passthrough blocks)
```

### 5. Verify

```bash
cargo fmt --all && cargo clippy --all-targets && cargo test --workspace
```

All three must pass before committing. Zero clippy warnings, zero test failures.

### 6. Commit and update CHANGELOG

```bash
git add -A
git commit    # message: "adopt: merge upstream vX.Y.Z into fork"
```

The CHANGELOG already contains the upstream version section from the merge.
Add a brief fork note if any fork-specific resolution was non-trivial.

### 7. Verify fork audit

```bash
./scripts/fork-audit
```

Ensures sentinels and this registry are still in sync after the merge.

### 8. Verify both modes

```bash
RTK_POSTPROCESS=0 cargo test  # upstream-like mode
RTK_POSTPROCESS=1 cargo test  # fork mode
```

### 9. Push and PR

```bash
git push -u origin adopt/upstream-X.Y.Z
gh pr create --title "adopt: merge upstream vX.Y.Z" --base develop
```
