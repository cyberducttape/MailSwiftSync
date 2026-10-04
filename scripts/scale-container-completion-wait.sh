#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 5 ]]; then
  echo "Usage: $0 <container> <progress-file> <wait-output-file> <watchdog-marker> <deadline-seconds>" >&2
  exit 2
fi

container_name="$1"
progress_file="$2"
wait_output_file="$3"
watchdog_marker="$4"
deadline_seconds="$5"
if ! [[ "$deadline_seconds" =~ ^[1-9][0-9]*$ ]]; then
  echo "Completion deadline must be a positive integer." >&2
  exit 2
fi

deadline=$((SECONDS + deadline_seconds))
while (( SECONDS < deadline )); do
  if [[ -f "$progress_file" ]]; then
    completion_line="$(grep -E '^harness_cleanup_complete=true exit_status=[0-9]+$' "$progress_file" | tail -n 1 || true)"
    if [[ -n "$completion_line" ]]; then
      harness_status="${completion_line##*=}"
      # Disposable scale fixtures can retain Dovecot children in blocked I/O
      # after all test assertions and cleanup have completed. The marker is
      # emitted only by the harness EXIT trap, so container kill is now teardown.
      timeout --kill-after=5s 20 docker kill "$container_name" >/dev/null
      set +e
      timeout --kill-after=5s 30 docker wait "$container_name" >"$wait_output_file" 2>&1
      wait_status=$?
      set -e
      if [[ "$wait_status" -ne 0 ]]; then
        echo "Docker wait after completed harness failed with status $wait_status." >&2
        exit 125
      fi
      container_status="$(head -n 1 "$wait_output_file" 2>/dev/null || true)"
      if ! [[ "$container_status" =~ ^[0-9]+$ ]]; then
        echo "Docker wait returned no numeric container status." >&2
        exit 125
      fi
      printf 'harness_exit_status=%s\ncontainer_exit_status=%s\n' \
        "$harness_status" "$container_status" >&2
      printf '%s\n' "$harness_status"
      exit 0
    fi
  fi
  sleep 1
done

echo "Scale harness did not write its completion marker before the ${deadline_seconds}-second deadline." >&2
printf 'completion_watchdog_triggered=true\n' > "$watchdog_marker"
timeout --kill-after=5s 20 docker kill "$container_name" >/dev/null 2>&1 || true
printf '124\n'
