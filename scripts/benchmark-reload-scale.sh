#!/usr/bin/env bash
set -euo pipefail

# Measures restart and bounded workspace reads for the documented 100k-row
# durable project. This is intentionally opt-in because it creates a large
# temporary SQLite fixture and is not part of the normal test suite.
# Defaults are release-host gates and can be overridden with
# MAILSWIFTSYNC_RELOAD_*_BUDGET_MS for a qualified host class.
benchmark_output=$(cargo test --locked --release workspace_reload_scale_benchmark -- --ignored --nocapture --test-threads=1 2>&1)
printf '%s\n' "$benchmark_output"
line=$(printf '%s\n' "$benchmark_output" | awk '/workspace reload benchmark: rows=100000 / { print; exit }')
if [[ -z "$line" ]]; then
  printf 'ERROR: missing 100000-row reload benchmark result\n' >&2
  exit 1
fi

declare -a reload_cases=(
  "seed_ms:MAILSWIFTSYNC_RELOAD_SEED_BUDGET_MS:5000"
  "reopen_ms:MAILSWIFTSYNC_RELOAD_REOPEN_BUDGET_MS:2000"
  "bounded_reads_ms:MAILSWIFTSYNC_RELOAD_READ_BUDGET_MS:1000"
)
for reload_case in "${reload_cases[@]}"; do
  IFS=: read -r metric budget_name default_budget <<<"$reload_case"
  budget=${!budget_name:-$default_budget}
  value=${line##* "$metric"=}
  value=${value%% *}
  if [[ ! "$value" =~ ^[0-9]+$ ]]; then
    printf 'ERROR: invalid %s in benchmark result: %s\n' "$metric" "$line" >&2
    exit 1
  fi
  if (( value > budget )); then
    printf 'ERROR: reload %s took %sms; budget is %sms\n' "$metric" "$value" "$budget" >&2
    exit 1
  fi
  printf 'PASS: reload %s %sms <= %sms budget\n' "$metric" "$value" "$budget"
done
