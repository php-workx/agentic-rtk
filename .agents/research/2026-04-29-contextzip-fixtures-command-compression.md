---
id: research-2026-04-29-contextzip-fixtures-command-compression
type: research
date: 2026-04-29
---

# Research: ContextZip Fixtures and Command-Path Compression

**Backend:** inline
**Scope:** Evaluate two candidate follow-ups from ContextZip: benchmark/golden fixtures and conservative command-path compression.

## Summary

The live/session-hook compaction track is low value after testing. The high-value remaining work is pre-context command output compression plus better measurement. RTK already has the main ContextZip primitives; the missing pieces are broader regression fixtures and tighter routing/filtering for commands that still bypass or underperform.

## Key Files

| File | Purpose |
|------|---------|
| `scripts/benchmark/run.ts` | VM integration benchmark; token-savings phase currently checks only six cases. |
| `src/discover/README.md` | Rewrite/discover design; same classification powers hooks and missed-savings analysis. |
| `src/discover/rules.rs` | Rewrite rule registry for command-path coverage. |
| `src/core/runner.rs` | Shared command execution path where postprocessors run before tracking. |
| `src/core/toml_filter.rs` | TOML filter DSL and inline tests. |
| `.tmp-rtk-analysis/contextzip/docs/benchmark-results.md` | Published ContextZip 102-case benchmark report. Full fixture corpus is not present in the clone. |

## Current Baseline

- ContextZip reports 102 benchmark cases, weighted 61.1% savings, with strongest results in Docker builds, ANSI/spinners, stacktraces, build errors, pip installs, and noisy CLI output.
- The local ContextZip clone only contains a small fixture directory (`tests/fixtures/dotnet/*`), not the full 102 raw/golden corpus. We would need to reconstruct or author comparable fixtures.
- RTK already has stacktrace, package-install, build-group, web extraction, ANSI/progress cleanup, TOML filters, rewrite hooks, tracking, and discover.
- RTK benchmark phase 7 has only six token-savings checks, so it cannot catch regressions in the ContextZip categories.
- Local 7-day `rtk discover --all` found 3,628 commands that could have used existing RTK wrappers, with ~638,710 estimated tokens left on the table.
- Local tracking shows high-volume low-savings commands: `rtk read` at ~18.6% average savings over 247 tracked executions and `rtk grep` at ~13.1% over 215 executions.

## Option 1: Golden Benchmark Fixtures

**Value:** High for engineering confidence; medium for immediate token savings.

This does not directly compress more context. It prevents regressions and exposes weak filters. It also gives us an honest replacement for ContextZip marketing numbers. The likely direct product impact comes from the improvements it points to, not from the fixture work itself.

Recommended scope:
- Build a fixture suite around ContextZip categories: stacktraces, ANSI/spinners, build errors, package installs, Docker, web extraction, common CLI.
- Start with 30-40 fixtures, not all 102, because the source corpus is absent.
- Add per-fixture expected invariants and minimum savings thresholds.
- Integrate as fast unit tests for pure filters and a slower benchmark/quality gate for command-level tests.
- Emit a report similar to ContextZip's benchmark table.

Expected effort:
- MVP: 1.5-2.5 days for fixture structure, 30-40 cases, thresholds, and CI/just integration.
- Full 100-case corpus: 4-6 days if fixtures are hand-authored/reconstructed and reviewed.

Expected gains:
- Direct token savings: 0%.
- Indirect savings: likely 5-15 percentage-point improvement in weak/under-tested categories once the failures are acted on.
- Risk reduction: high. This would have caught over-aggressive or negative-savings cases such as small ESLint/Java/ls outputs.

## Option 2: Conservative Command-Path Compression

**Value:** High. This is the work that actually reduces live agent context.

RTK already routes commands through `rtk rewrite`, parses compound commands safely, preserves env prefixes and redirects, and runs filtering before the agent sees output. The remaining work is to improve adoption and a few underperforming high-volume filters.

Recommended scope:
- Use `rtk discover` as the prioritization loop.
- Improve rewrite/adoption for high-volume misses where semantics are safe:
  - `pnpm test` / `pnpm typecheck` should route to the right test/typecheck filters when script names are recognizable.
  - `uv run ... pytest|mypy|ruff|python -m ...` should unwrap to existing Python filters when safe.
  - `yarn install/build/test`, `just test/check/pre-commit`, and simple `go run tool` cases are likely candidates, but need conservative rules.
- Improve low-performing existing filters:
  - `rtk read` defaults are intentionally conservative; add better modes or hook-specific behavior only if we can preserve file semantics.
  - `rtk grep` should group/truncate more effectively without breaking search usefulness.
  - `terraform plan`, `helm template`, `gh pr checks`, and `pnpm install` have local low-savings examples worth fixture-driven tuning.
- Keep command-path only. Do not reintroduce Stop/session JSONL hooks for live savings.

Expected effort:
- Discovery-driven first pass: 2-3 days.
- Add 5-8 safe rewrite rules plus tests: 1-2 days.
- Tune `grep`, `read`, `pnpm`, `terraform`, `helm`, `gh` using fixtures: 3-5 days.
- Total useful tranche: 5-8 days.

Expected gains:
- From adoption alone, local 7-day discover estimates ~638k tokens of missed savings. With imperfect hook coverage and conservative rewrites, realistic recoverable savings are ~250k-450k tokens per week on this machine's current usage.
- From improving low-performing filters, local tracked inputs suggest meaningful upside:
  - `rtk read`: 1.28M input tokens with 18.6% avg savings; moving common cases to 40-50% would save another ~270k-400k tokens in this sample.
  - `rtk grep`: 138k input tokens with 13.1% avg savings; moving to 40-60% would save another ~35k-65k tokens.
  - Other low performers add smaller but useful wins.
- Combined realistic near-term gain on local usage: roughly +300k to +800k tokens saved per week, assuming hooks are installed and the commands recur.

## Recommendation

Do both, but sequence them as:

1. Add benchmark fixtures first, scoped to the categories we plan to tune. Keep it small enough to finish quickly.
2. Use that harness to tune command-path compression and rewrite coverage.

The fixture work is the guardrail. The command-path work is the value. Avoid automatic session compaction hooks unless the target runtime exposes an official before-context compression point.
