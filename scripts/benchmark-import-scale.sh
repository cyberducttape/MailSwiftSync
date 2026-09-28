#!/usr/bin/env bash
set -euo pipefail

# This is intentionally opt-in: the 100k XLSX case is a real desktop-scale
# workload and should run on the release host class used for a readiness claim.
# The benchmark prints elapsed time, input bytes, imported rows, and Linux RSS
# before/after each case. Keep the output with the release evidence bundle.
# The default elapsed-time budgets are conservative release-host gates and can
# be overridden for a qualified host with MAILSWIFTSYNC_IMPORT_*_BUDGET_MS.
benchmark_output=$(cargo test --locked --release scale_import_benchmark -- \
  --ignored --nocapture --test-threads=1 2>&1)
printf '%s\n' "$benchmark_output"

declare -a benchmark_cases=(
  "csv:1000:MAILSWIFTSYNC_IMPORT_CSV_1000_BUDGET_MS:100"
  "csv:10000:MAILSWIFTSYNC_IMPORT_CSV_10000_BUDGET_MS:500"
  "csv:100000:MAILSWIFTSYNC_IMPORT_CSV_100000_BUDGET_MS:2000"
  "xlsx:10000:MAILSWIFTSYNC_IMPORT_XLSX_10000_BUDGET_MS:1000"
  "xlsx:100000:MAILSWIFTSYNC_IMPORT_XLSX_100000_BUDGET_MS:5000"
)
for benchmark_case in "${benchmark_cases[@]}"; do
  IFS=: read -r format rows budget_name default_budget <<<"$benchmark_case"
  budget=${!budget_name:-$default_budget}
  line=$(printf '%s\n' "$benchmark_output" | awk -v format="$format" -v rows="$rows" '$0 ~ "scale-import format=" format " rows=" rows " " { print; exit }')
  if [[ -z "$line" ]]; then
    printf 'ERROR: missing benchmark result for %s/%s\n' "$format" "$rows" >&2
    exit 1
  fi
  elapsed=${line##* elapsed_ms=}
  elapsed=${elapsed%% *}
  if [[ ! "$elapsed" =~ ^[0-9]+$ ]]; then
    printf 'ERROR: invalid elapsed_ms in benchmark result: %s\n' "$line" >&2
    exit 1
  fi
  if (( elapsed > budget )); then
    printf 'ERROR: %s/%s import took %sms; budget is %sms\n' "$format" "$rows" "$elapsed" "$budget" >&2
    exit 1
  fi
  printf 'PASS: %s/%s import %sms <= %sms budget\n' "$format" "$rows" "$elapsed" "$budget"
done
