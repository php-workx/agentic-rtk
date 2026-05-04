#!/usr/bin/env bash
# Upstream PR Analyzer wrapper
# Delegates to the Python script for reliability.
set -euo pipefail

python3 "$(dirname "$0")/analyze-upstream-prs.py"
