# Upstream Sync Strategy

## Goal

Keep our fork (`php-workx/agentic-rtk`) merge-compatible with upstream (`rtk-ai/rtk`) so that pulling new upstream releases is low-friction and agents can do it reliably.

## Principles

| # | Principle | Rule |
|---|-----------|------|
| 1 | **Isolate** | Put fork-only features in new files/modules. Never modify upstream files unless the feature MUST live there. |
| 2 | **Extend** | Use traits, hooks, enums, or config to add behavior. Avoid rewriting upstream functions. |
| 3 | **Annotate** | Every upstream file we modify gets a `// FORK:` comment with issue/PR reference. |
| 4 | **Preserve structure** | Keep upstream function signatures, module boundaries, and file names. |
| 5 | **Separate commits** | Pure upstream syncs go in their own commit. Fork features go in separate commits. |
| 6 | **Feature flags** | Major fork features should have a compile-time flag (e.g., `--features fork-extras`). |

---

## File/Module Ownership Map

### ✅ "Fork-only" — Safe to modify freely

These were created by us. Upstream has no knowledge of them.

| File/Module | Description |
|-------------|-------------|
| `src/core/postprocess/` | Context Zip compressors — our feature |
| `src/session/mod.rs` | Session compaction — our feature |
| `src/cmds/cloud/web_cmd.rs` | `rtk web` command — our feature |
| `src/core/noise.rs` | Noise reduction helpers — our feature |
| `src/core/utils.rs` | Fork utility functions — our feature |
| `src/analytics/gain.rs` | Gain analytics — our feature |
| `.agents/` | Agent documentation, triage, research |
| `.github/workflows/analyze-upstream.yml` | Upstream PR tracker |
| `scripts/analyze-upstream-prs.py` | Upstream PR analyzer |
| `Formula/rtk.rb` | Homebrew formula |
| `install.sh` | Fork installer |
| `justfile` | Fork task runner |
| `.pre-commit-config.yaml` | Fork pre-commit hooks |

### ⚠️ "Shared" — Modify with care, annotate every change

Upstream owns these files. We add to them. Keep diffs minimal.

| File | What we added | Merge risk |
|------|---------------|------------|
| `src/core/tracking.rs` | `session_compactions` table, `cleanup_old()`, `project_filter_params()` | Medium — schema changes can conflict |
| `src/hooks/init.rs` | Codex mode, `session_compaction`, `uninstall_codex_at()` hooks.json cleanup | High — upstream may refactor init |
| `src/discover/registry.rs` | `rewrite_cat_*` helpers, `is_whitespace_safe_source_file()` | Medium — upstream adds new rewrites |
| `src/main.rs` | `Commands::Web`, `is_operational_command()`, tests | Medium — upstream adds commands |
| `src/core/runner.rs` | `RunOptions::postprocess()` integration | Low — we added a field |
| `src/core/filter.rs` | Postprocessor fallback integration | Low — we added a branch |
| `src/core/stream.rs` | Minor fork-specific tweaks | Low |

### ❌ "Upstream" — Touch only for bugfixes, never for features

| File | Why hands-off |
|------|---------------|
| `src/parser.rs` | Core parsing — upstream changes frequently |
| `src/shell/` | Shell integration — upstream refactors often |
| `src/hooks/constants.rs` | We added constants, but upstream may add more — merge carefully |
| `Cargo.toml` | Only add deps for fork features; keep version in sync |
| `src/cmds/git/git.rs` | High churn, complex merge |
| `src/cmds/js/pnpm_cmd.rs` | High churn |
| `src/cmds/python/pip_cmd.rs` | High churn |
| `src/cmds/rust/cargo_cmd.rs` | High churn |
| `src/cmds/cloud/container.rs` | High churn |
| `src/learn/` | ML-heavy, upstream changes frequently |
| `src/discover/lexer.rs` | Core lexer — minimal changes |

---

## Adding a New Fork Feature (Agent Checklist)

When implementing a feature that does NOT exist upstream, follow this flow:

### Step 1: Can it be a new file?

**YES →** Create `src/{area}/fork_{feature}.rs` or `src/fork/{feature}.rs`

```rust
// src/core/postprocess/my_feature.rs
// FORK: Issue #42 — My new feature
pub fn my_feature(input: &str) -> String { ... }
```

Register it in the area's `mod.rs`:
```rust
// src/core/postprocess/mod.rs
// FORK: Added my_feature module
mod my_feature;
pub use my_feature::*;
```

**NO →** Go to Step 2.

### Step 2: Can it be a trait extension?

**YES →** Define a trait in a fork file, implement it for upstream types:

```rust
// src/core/fork_extensions.rs
trait ForkPostprocess {
    fn postprocess(&self, output: &str) -> String;
}

impl ForkPostprocess for runner::RunOptions {
    fn postprocess(&self, output: &str) -> String {
        // Our logic here
    }
}
```

**NO →** Go to Step 3.

### Step 3: Must modify an upstream file?

**YES →** Follow the "Upstream File Modification Protocol":

1. **Keep the diff minimal.** Add 5 lines, don't rewrite 50.
2. **Use `// FORK:` comments** on every added block:
   ```rust
   // FORK: php-workx/agentic-rtk#42 — Add Context Zip postprocessing
   if self.postprocess {
       output = postprocess::apply(&output);
   }
   // END FORK
   ```
3. **Preserve existing structure.** Don't rename upstream functions. Don't change signatures unless absolutely necessary.
4. **Add a test in a separate fork test file.** Don't add tests to upstream test modules if possible.

---

## Upstream File Modification Protocol

### Comment markers

Every modification to an upstream-owned file MUST be wrapped:

```rust
// FORK: <repo>#<issue-or-pr-number> — <one-line description>
<our code>
// END FORK
```

For small inline changes (e.g., adding a variant to a `match`):

```rust
// FORK: php-workx/agentic-rtk#11 — Add Web to operational commands
| Commands::Web { .. }
// END FORK
```

### Forbidden patterns

❌ **Don't do this:**
```rust
// Complete rewrite of upstream function
fn upstream_function() {
    // Our entirely different implementation
}
```

✅ **Do this instead:**
```rust
fn upstream_function() {
    // ... upstream code ...
    
    // FORK: php-workx/agentic-rtk#11 — Hook into upstream function
    fork_callback();
    // END FORK
}
```

---

## Syncing with Upstream (Agent Playbook)

### When to sync

- Upstream releases a new version (watch `rtk-ai/rtk` releases)
- The weekly triage report flags a high-value upstream PR as "approved"
- Agent is instructed to sync

### Step-by-step sync process

#### 1. Prepare

```bash
# Ensure main is clean
git checkout main
git pull origin main

# Create sync branch
git checkout -b sync/upstream-v0.39.0
```

#### 2. Add upstream remote (first time only)

```bash
git remote add upstream https://github.com/rtk-ai/rtk.git
git fetch upstream --tags
```

#### 3. Merge upstream release tag

```bash
# Option A: Merge the tag (preserves history, shows conflicts)
git merge upstream/v0.39.0 --no-edit

# Option B: Cherry-pick specific upstream commits (surgical)
git cherry-pick abc1234
git cherry-pick def5678
```

**Prefer Option A** for full releases (minor/major bumps).  
**Prefer Option B** for single upstream PRs we want to adopt.

#### 4. Resolve conflicts

Conflict resolution priority:

1. **Accept upstream's version** for "Upstream" files (see ownership map above)
2. **Accept ours** for "Fork-only" files (upstream doesn't have them)
3. **Manual merge** for "Shared" files — preserve both changes, re-apply our `// FORK:` blocks

```bash
# After resolving, verify no markers left
grep -rn "<<<<<<< HEAD\|=======\|>>>>>>>" src/ .github/
```

#### 5. Verify the merge

```bash
# Check for stale markers
grep -rn "FORK:" src/ | grep -v "// FORK:" | head -20

# Build and test
cargo test

# Check for orphaned files (upstream deleted something we depend on)
git diff --name-status upstream/v0.39.0 | grep "^D" | grep -E "src/(core|hooks|discover|main)"
```

#### 6. Update version

```bash
# Sync Cargo.toml version with upstream
# Edit Cargo.toml: version = "0.39.0"
# The fork counter resets to 1 on upstream version bumps
```

#### 7. Commit and PR

```bash
git add -A
git commit -m "sync: upstream v0.39.0

- Merged rtk-ai/rtk@v0.39.0
- Preserved fork features (Context Zip, session compaction, web extract)
- Counter reset: next fork tag will be v0.39.0-workx.1"
git push origin sync/upstream-v0.39.0
```

Open a PR to `main` with the `sync/` branch. **Do NOT use `develop` for syncs.**

#### 8. Post-merge release

After the sync PR merges to `main`:

```bash
git checkout main
git pull origin main
git tag v0.39.0-workx.1
git push origin v0.39.0-workx.1
```

---

## The Upstream Triage → Sync Pipeline

```
┌─────────────────┐     ┌──────────────┐     ┌─────────────┐     ┌────────────┐
│ 1. Triage       │────→│ 2. Approve   │────→│ 3. Cherry   │────→│ 4. Test   │
│    (weekly)     │     │    PRs       │     │    pick     │     │    + PR   │
└─────────────────┘     └──────────────┘     └─────────────┘     └─────┬──────┘
                                                                      │
┌─────────────────┐     ┌──────────────┐     ┌────────────────────────┘
│ 6. Full release │←────│ 5. Merge     │←────│
│    vX.Y.Z-workx │     │    to main   │
└─────────────────┘     └──────────────┘
```

**Two modes of adoption:**

| Mode | When | How |
|------|------|-----|
| **Cherry-pick** | Single approved upstream PR | `git cherry-pick <commit>` onto `main` |
| **Full sync** | Upstream releases new version | `git merge upstream/vX.Y.Z` onto `main` |

---

## Directory Layout for Fork Isolation

```
src/
├── core/
│   ├── mod.rs              # Shared — add fork module declarations
│   ├── postprocess/        # ✅ Fork-only — Context Zip
│   ├── tracking.rs         # ⚠️ Shared — added session_compactions
│   ├── runner.rs           # ⚠️ Shared — added RunOptions::postprocess
│   ├── filter.rs           # ⚠️ Shared — added fallback postprocessor
│   └── noise.rs            # ✅ Fork-only
├── session/
│   └── mod.rs              # ✅ Fork-only
├── cmds/
│   └── cloud/
│       └── web_cmd.rs       # ✅ Fork-only
├── hooks/
│   ├── init.rs             # ⚠️ Shared — added Codex mode
│   └── constants.rs          # ⚠️ Shared — added CODEX_* constants
├── discover/
│   └── registry.rs         # ⚠️ Shared — added cat/head/tail rewrites
├── main.rs                 # ⚠️ Shared — added Commands::Web
└── fork/                   # 🆕 Future: isolated fork feature directory
    └── mod.rs
```

---

## Agent Instructions Summary

When working on this repo, you MUST:

1. **Read this file first** before modifying any `src/` file outside of `src/core/postprocess/`, `src/session/`, `src/cmds/cloud/web_cmd.rs`, or `.agents/`.
2. **Check the ownership map** to know if a file is "Fork-only", "Shared", or "Upstream".
3. **Never rewrite upstream functions.** Extend, wrap, or add new functions instead.
4. **Use `// FORK:` markers** on every change to a "Shared" file.
5. **Prefer new files** over modifying existing ones.
6. **Run `cargo test`** after any change to a "Shared" file to catch regressions.
7. **Before syncing with upstream**, read `.agents/UPSTREAM_TRIAGE.md` for the current approved PR list.

When in doubt, **create a new file** rather than editing an upstream one.
