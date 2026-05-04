set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

default:
  @just --list

# Ensure a required command is available.
require tool:
  @command -v {{tool}} >/dev/null 2>&1 || { echo "missing required tool: {{tool}}" >&2; exit 127; }

# Format Rust code in-place.
format:
  cargo fmt --all

# Check Rust formatting without writing changes.
format-check:
  cargo fmt --all -- --check

# Build the debug binary.
build:
  cargo build

# Build the release binary.
build-release:
  cargo build --release

# Install this checkout's RTK into Cargo's bin directory.
install:
  cargo install --path . --force

# Install the debug binary as the active rtk on PATH.
install-debug:
  cargo build
  mkdir -p "${CARGO_HOME:-$HOME/.cargo}/bin"
  cp target/debug/rtk "${CARGO_HOME:-$HOME/.cargo}/bin/rtk"

# Run the full test suite.
test:
  cargo test

# Run focused tests for session compaction, hook init, and tracking.
test-focused:
  cargo test session::tests -- --nocapture
  cargo test hooks::init::tests -- --nocapture
  cargo test tracking -- --nocapture

# Run clippy with warnings treated as errors.
lint:
  cargo clippy --all-targets -- -D warnings

# Check new/modified command modules have tests.
test-presence base='origin/main':
  bash scripts/check-test-presence.sh {{base}}

# Scan Cargo dependencies for advisories.
audit: (require "cargo-audit")
  cargo audit

# Scan for secrets with Betterleaks.
secrets: (require "betterleaks")
  betterleaks dir --no-banner --redact .

# Run Semgrep's automatic ruleset.
semgrep: (require "semgrep")
  semgrep scan --config auto .

# Run security-oriented local gates.
security: audit secrets semgrep

# Run the offline session cache benchmark.
bench-cache:
  cargo run -- session cache-bench --provider openai --mode offline

# Run the optional live OpenAI cache benchmark. Requires OPENAI_API_KEY.
bench-cache-live:
  cargo run -- session cache-bench --provider openai --mode live

# Smoke-test Claude Stop hook compaction with a temp transcript.
smoke-claude-hook: (require "jq")
  #!/usr/bin/env bash
  set -euo pipefail
  tmp="$(mktemp /tmp/rtk-claude-hook.XXXXXX)"
  text="$(awk 'BEGIN { for (i = 0; i < 120; i++) print "repeat line" }')"
  jq -cn '{type:"assistant",message:{content:[{type:"tool_use",id:"bash1",name:"Bash",input:{command:"printf"}}]}}' > "$tmp"
  jq -cn --arg text "$text" '{type:"user",message:{content:[{type:"tool_result",tool_use_id:"bash1",content:[{type:"text",text:$text}]}]}}' >> "$tmp"
  before="$(wc -c < "$tmp" | tr -d '[:space:]')"
  printf '{"hook_event_name":"Stop","transcript_path":"%s"}\n' "$tmp" \
    | cargo run --quiet -- session hook --agent claude --event stop
  after="$(wc -c < "$tmp" | tr -d '[:space:]')"
  compressed="$(grep -c rtk_compressed "$tmp" || true)"
  rm -f "$tmp"
  if [ "$compressed" -lt 1 ] || [ "$after" -ge "$before" ]; then
    echo "Claude hook smoke failed: before=$before after=$after rtk_compressed=$compressed" >&2
    exit 1
  fi
  echo "Claude hook smoke passed: before=$before after=$after rtk_compressed=$compressed"

# Smoke-test Codex JSONL compaction without mutating a real session.
smoke-codex: (require "jq")
  #!/usr/bin/env bash
  set -euo pipefail
  tmp="$(mktemp /tmp/rtk-codex.XXXXXX)"
  text="$(awk 'BEGIN { for (i = 0; i < 120; i++) print "same output" }')"
  jq -cn '{timestamp:"2026-04-29T00:00:00Z",type:"session_meta",payload:{id:"codex-smoke",base_instructions:{text:"keep base instructions"}}}' > "$tmp"
  jq -cn '{timestamp:"2026-04-29T00:00:01Z",type:"response_item",payload:{type:"function_call",name:"exec_command",arguments:"{\"cmd\":\"cargo test\"}",call_id:"call_1"}}' >> "$tmp"
  jq -cn --arg text "$text" '{timestamp:"2026-04-29T00:00:02Z",type:"response_item",payload:{type:"function_call_output",call_id:"call_1",output:$text}}' >> "$tmp"
  output="$(cargo run --quiet -- session compact "$tmp" --dry-run --explain-cache --agent codex)"
  rm -f "$tmp"
  echo "$output"
  echo "$output" | grep -q 'newly_compacted_blocks=1'
  echo "$output" | grep -q 'BashHistoryCompact=1'

# Install a Codex Stop hook that invokes this checkout's debug binary.
install-codex-hook-debug: (require "jq")
  #!/usr/bin/env bash
  set -euo pipefail
  cargo build
  rtk_bin="$(pwd)/target/debug/rtk"
  codex_home="${CODEX_HOME:-$HOME/.codex}"
  hooks="$codex_home/hooks.json"
  mkdir -p "$codex_home"
  if [ -f "$hooks" ]; then
    cp "$hooks" "$hooks.bak"
  else
    printf '{"hooks":{}}\n' > "$hooks"
  fi
  tmp="$(mktemp)"
  jq --arg cmd "$rtk_bin session hook --agent codex --event stop" '
    .hooks = (.hooks // {}) |
    .hooks.Stop = (.hooks.Stop // []) |
    if ([.hooks.Stop[]?.hooks[]?.command] | index($cmd)) then .
    else .hooks.Stop += [{"hooks":[{"type":"command","command":$cmd,"timeout":10}]}] end
  ' "$hooks" > "$tmp"
  mv "$tmp" "$hooks"
  echo "Codex Stop hook installed: $rtk_bin session hook --agent codex --event stop"

# Install pre-commit hooks for this repository.
install-hooks: (require "pre-commit")
  pre-commit install

# Run configured pre-commit hooks across the full repository.
pre-commit-all: (require "pre-commit")
  pre-commit run --all-files

# Fast pre-commit gate for local development.
pre-commit: format-check lint test-focused test-presence

# Full local pre-push gate.
pre-push: pre-commit test bench-cache security

# Alias for the full local gate.
check: pre-push
