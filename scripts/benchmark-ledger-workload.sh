#!/usr/bin/env bash
set -euo pipefail

# Sustained controller write workload on a file-backed ledger while two
# read-only connections poll it (GUI snapshot worker, status/metrics). Gates
# write latency, lock contention, WAL growth, and durable bytes per mailbox.
# Defaults are release-host gates; override with MAILSWIFTSYNC_LEDGER_*_BUDGET
# for a qualified host class.
benchmark_output=$(cargo test --locked --release ledger_write_workload_benchmark -- --ignored --nocapture --test-threads=1 2>&1)
printf '%s\n' "$benchmark_output"
line=$(printf '%s\n' "$benchmark_output" | awk '/ledger-workload mailboxes=/ { print; exit }')
if [[ -z "$line" ]]; then
  printf 'ERROR: missing ledger workload benchmark result\n' >&2
  exit 1
fi

declare -a ledger_cases=(
  "write_p99_us:MAILSWIFTSYNC_LEDGER_WRITE_P99_BUDGET_US:20000"
  "write_max_us:MAILSWIFTSYNC_LEDGER_WRITE_MAX_BUDGET_US:1000000"
  "busy_errors:MAILSWIFTSYNC_LEDGER_BUSY_BUDGET:0"
  "wal_max_kib:MAILSWIFTSYNC_LEDGER_WAL_BUDGET_KIB:65536"
  "bytes_per_mailbox:MAILSWIFTSYNC_LEDGER_BYTES_PER_MAILBOX_BUDGET:16384"
)
for ledger_case in "${ledger_cases[@]}"; do
  IFS=: read -r metric budget_name default_budget <<<"$ledger_case"
  budget=${!budget_name:-$default_budget}
  value=${line##* "$metric"=}
  value=${value%% *}
  if [[ ! "$value" =~ ^[0-9]+$ ]]; then
    printf 'ERROR: invalid %s in benchmark result: %s\n' "$metric" "$line" >&2
    exit 1
  fi
  if (( value > budget )); then
    printf 'ERROR: ledger %s is %s; budget is %s\n' "$metric" "$value" "$budget" >&2
    exit 1
  fi
  printf 'PASS: ledger %s %s <= %s budget\n' "$metric" "$value" "$budget"
done
