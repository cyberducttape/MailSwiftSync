#!/usr/bin/env bash
set -euo pipefail

# Measures cached filtering, explicit selection, state refresh, a real
# virtualized egui first frame, the complete application shell at 100k rows
# (idle, with every row selected, and on a search keystroke), and a 100k-row
# CSV import (latency and peak memory growth). Run on the release host class
# and retain the output with release evidence.
if ! output=$(
  {
    cargo test --locked --release scale_ui_benchmark -- --ignored --nocapture --test-threads=1
    cargo test --locked --release full_shell_ui_benchmark -- --ignored --nocapture --test-threads=1
    cargo test --locked --release import_scale_benchmark -- --ignored --nocapture --test-threads=1
  } 2>&1
); then
  printf '%s\n' "$output"
  exit 1
fi
printf '%s\n' "$output"

metrics=$(printf '%s\n' "$output" | sed -n 's/.*scale-ui //p' | tail -n 1)
if [[ -z "$metrics" ]]; then
  echo "UI scale benchmark did not emit metrics" >&2
  exit 1
fi

check_budget() {
  local metric="$1"
  local budget="$2"
  local source="${3:-$metrics}"
  local unit="${4:-ms}"
  local value
  value=$(printf '%s\n' "$source" | sed -n "s/.*${metric}=\([0-9][0-9]*\).*/\1/p")
  if [[ -z "$value" ]]; then
    echo "UI scale benchmark omitted ${metric}" >&2
    exit 1
  fi
  if (( value > budget )); then
    echo "UI scale budget exceeded: ${metric}=${value}${unit} > ${budget}${unit}" >&2
    exit 1
  fi
}

check_budget filter_ms "${MAILSWIFTSYNC_UI_FILTER_BUDGET_MS:-100}"
check_budget selection_all_ms "${MAILSWIFTSYNC_UI_SELECTION_BUDGET_MS:-250}"
check_budget state_update_ms "${MAILSWIFTSYNC_UI_STATE_UPDATE_BUDGET_MS:-100}"
check_budget first_frame_ms "${MAILSWIFTSYNC_UI_FIRST_FRAME_BUDGET_MS:-100}"

full_shell_line=$(printf '%s\n' "$output" | awk '/scale-ui-full rows=100000 / { print; exit }')
if [[ -z "$full_shell_line" ]]; then
  echo "UI scale benchmark omitted full-shell metrics" >&2
  exit 1
fi
full_shell_elapsed=${full_shell_line##* first_frame_ms=}
full_shell_elapsed=${full_shell_elapsed%% *}
if [[ ! "$full_shell_elapsed" =~ ^[0-9]+$ ]]; then
  echo "UI scale benchmark emitted invalid full-shell first_frame_ms" >&2
  exit 1
fi
full_shell_budget=${MAILSWIFTSYNC_UI_FULL_SHELL_FIRST_FRAME_BUDGET_MS:-500}
if (( full_shell_elapsed > full_shell_budget )); then
  echo "UI scale budget exceeded: full_shell_first_frame_ms=${full_shell_elapsed}ms > ${full_shell_budget}ms" >&2
  exit 1
fi
printf 'PASS: full-shell first frame %sms <= %sms budget\n' "$full_shell_elapsed" "$full_shell_budget"
check_budget selected_frame_ms "${MAILSWIFTSYNC_UI_SELECTED_FRAME_BUDGET_MS:-100}" "$full_shell_line"
check_budget search_frame_ms "${MAILSWIFTSYNC_UI_SEARCH_FRAME_BUDGET_MS:-100}" "$full_shell_line"

import_line=$(printf '%s\n' "$output" | awk '/scale-import rows=/ { print; exit }')
if [[ -z "$import_line" ]]; then
  echo "Import scale benchmark omitted metrics" >&2
  exit 1
fi
check_budget import_ms "${MAILSWIFTSYNC_IMPORT_BUDGET_MS:-2000}" "$import_line"
check_budget import_rss_mib "${MAILSWIFTSYNC_IMPORT_RSS_BUDGET_MIB:-256}" "$import_line" MiB
printf 'PASS: 100k-row working frames and CSV import within budget\n'
