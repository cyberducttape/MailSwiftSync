#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 5 ]]; then
  echo "Usage: $0 <container> <wait-output-file> <watchdog-marker> <watchdog-seconds> <outer-deadline-seconds>" >&2
  exit 2
fi

container_name="$1"
wait_output_file="$2"
watchdog_marker="$3"
watchdog_seconds="$4"
outer_deadline_seconds="$5"
if ! [[ "$watchdog_seconds" =~ ^[1-9][0-9]*$ && "$outer_deadline_seconds" =~ ^[1-9][0-9]*$ ]]; then
  echo "Watchdog deadlines must be positive integers." >&2
  exit 2
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
set +e
wait_output="$(timeout --kill-after=10s "$outer_deadline_seconds" \
  bash "$script_dir/scale-docker-wait.sh" "$container_name" \
    "$wait_output_file" "$watchdog_marker" "$watchdog_seconds")"
wait_status=$?
set -e

if [[ "$wait_status" -eq 0 && "$wait_output" =~ ^[0-9]+$ ]]; then
  printf '%s\n' "$wait_output"
elif [[ "$wait_status" -eq 124 || "$wait_status" -eq 137 ]]; then
  echo "Outer scale wait deadline expired; force-stopping the container to preserve diagnostics." >&2
  printf 'outer_watchdog_triggered=true\n' > "$watchdog_marker"
  timeout --kill-after=5s 20 docker kill "$container_name" >/dev/null 2>&1 || true
  printf '124\n'
else
  echo "Scale wait helper failed with status $wait_status; force-stopping the container." >&2
  printf 'wait_helper_failed_status=%s\n' "$wait_status" > "$watchdog_marker"
  timeout --kill-after=5s 20 docker kill "$container_name" >/dev/null 2>&1 || true
  printf '125\n'
fi
