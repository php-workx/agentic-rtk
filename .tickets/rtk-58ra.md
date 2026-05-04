---
id: rtk-58ra
status: open
deps: []
links: []
created: 2026-04-29T16:37:19Z
type: chore
priority: 2
assignee: Ronny Unger
tags: [rtk, analytics, follow-up]
---
# Verify RTK command-path compression effectiveness

Follow up after the conservative command-path compression changes have seen real usage. Use RTK analytics to verify whether the new whitespace read filter, grep duplicate grouping, package-manager rewrites, uv-run rewrites, and just rewrites are improving token savings without surprising fallbacks.

## Acceptance Criteria

Run rtk gain --project --by-feature and rtk gain --history from the RTK repo; inspect rtk read and rtk grep savings plus recent rewrite coverage; manually spot-check representative rewrites with rtk rewrite for cat source files, package-manager safe scripts, uv run pytest/ruff/mypy, and unsafe dev/watch scripts; record whether savings improved or whether more tuning/tests are needed.

