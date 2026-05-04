# Release Schema for php-workx/agentic-rtk

## Overview

This fork uses a **dual-versioning system** to stay in sync with upstream `rtk-ai/rtk` while tracking our own releases independently.

## Tag Format

```
v{upstream_version}-workx.{counter}
```

| Component | Meaning | Example |
|-----------|---------|---------|
| `v` | Prefix | Always `v` |
| `{upstream_version}` | Current upstream RTK version from `Cargo.toml` | `0.38.0` |
| `-workx` | Fork suffix (was `-fork` historically) | Always `-workx` |
| `{counter}` | Incremental release counter for this base version | `1`, `2`, `3`... |

### Examples

- `v0.38.0-workx.1` — First release on upstream 0.38.0
- `v0.38.0-workx.2` — Second release on upstream 0.38.0 (no upstream bump)
- `v0.39.0-workx.1` — First release after upstream bumped to 0.39.0 (counter resets)
- `v0.39.1-workx.2` — Second release on upstream 0.39.1

## Rules

1. **Base version comes from `Cargo.toml`**: `package.version` tracks upstream RTK.
2. **Only bump `Cargo.toml` when upstream bumps**: If upstream releases 0.39.0, update `Cargo.toml` to match.
3. **Counter only increments on our releases**: Even if we make 10 releases while upstream stays at 0.38.0, we go `workx.1` → `workx.2` → ... → `workx.10`.
4. **Counter resets on upstream bump**: When `Cargo.toml` changes to a new upstream version, the counter resets to `1`.
5. **No pre-release identifiers in `Cargo.toml`**: `version = "0.38.0"` is valid Cargo; `0.38.0-workx.1` is NOT valid Cargo. The suffix lives only in Git tags.

## How to Determine the Next Tag

### Step 1: Read the base version
```bash
grep '^version' Cargo.toml
# → version = "0.38.0"
```

### Step 2: Find the latest tag for this base version
```bash
git tag --sort=-v:refname | grep "v0.38.0-workx" | head -1
# → v0.38.0-workx.1 (or empty if none yet)
```

### Step 3: Compute next counter
- If no tag exists for this base version → counter = `1`
- If latest is `v0.38.0-workx.1` → counter = `2`

### Step 4: Construct tag
```
v0.38.0-workx.2
```

## Special Cases

### Upstream version bumped
If upstream releases `0.39.0`:
1. Update `Cargo.toml`: `version = "0.39.0"`
2. Reset counter: first tag is `v0.39.0-workx.1`
3. Add a `### Upstream Sync` section to `CHANGELOG.md`

### No upstream change, only fork fixes
If upstream is still at `0.38.0` and we have new commits:
1. Keep `Cargo.toml` at `0.38.0`
2. Increment counter: `v0.38.0-workx.2`

## CI/CD Integration

The `.github/workflows/cd.yml` workflow:
- Triggers on tag pushes matching `v*`
- Validates tag format: `^v[0-9]+\.[0-9]+\.[0-9]+(-workx\.[0-9]+)?$`
- Builds release artifacts and updates Homebrew formula automatically

## Changelog

Keep `CHANGELOG.md` in the upstream format (Keep a Changelog) but add a fork-specific header for each `workx` release so users can distinguish our changes from upstream.

## For Release Agents / Skills

When this file is present, prefer this schema over standard SemVer bumping:
- Do NOT bump the version in `Cargo.toml` unless explicitly syncing with upstream.
- Do bump the `workx.N` counter in the Git tag.
- The commit message should be: `chore: release v{upstream_version}-workx.{counter}`.
