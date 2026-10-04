#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -lt 4 || "$#" -gt 5 ]]; then
  echo "Usage: $0 <container> <wait-output-file> <watchdog-marker> <deadline-seconds> [kill-grace-seconds]" >&2
  exit 2
fi

container_name="$1"
wait_output_file="$2"
watchdog_marker="$3"
deadline_seconds="$4"
kill_grace_seconds="${5:-120}"
if ! [[ "$deadline_seconds" =~ ^[1-9][0-9]*$ && "$kill_grace_seconds" =~ ^[1-9][0-9]*$ ]]; then
  echo "Watchdog deadline and kill grace must be positive integers." >&2
  exit 2
fi

timeout_seconds=$((deadline_seconds + kill_grace_seconds + 30))
timeout --kill-after=15s "$timeout_seconds" docker wait "$container_name" \
  >"$wait_output_file" 2>&1 &
wait_pid=$!

(
  sleep "$deadline_seconds" &
  timer_pid=$!
  trap 'kill "$timer_pid" 2>/dev/null || true' TERM INT
  wait "$timer_pid" || exit 0

  if ! kill -0 "$wait_pid" 2>/dev/null; then
    exit 0
  fi
  echo "Scale container exceeded the ${deadline_seconds}-second host watchdog; stopping it to preserve diagnostics." >&2
  printf 'watchdog_triggered=true\n' > "$watchdog_marker"
  timeout --kill-after=5s 20 docker kill "$container_name" >/dev/null 2>&1 || true

  sleep "$kill_grace_seconds" &
  timer_pid=$!
  wait "$timer_pid" || exit 0
  if kill -0 "$wait_pid" 2>/dev/null; then
    # GNU timeout uses a dedicated process group unless --foreground is set.
    # Verify the group ID before signaling it, avoiding unrelated runner jobs.
    wait_pgid="$(ps -o pgid= -p "$wait_pid" | tr -d ' ')"
    if [[ "$wait_pgid" == "$wait_pid" ]]; then
      kill -KILL -- "-$wait_pgid" 2>/dev/null || true
    else
      if [[ -r "/proc/$wait_pid/task/$wait_pid/children" ]]; then
        read -r -a wait_children < "/proc/$wait_pid/task/$wait_pid/children" || true
        for child_pid in "${wait_children[@]:-}"; do
          kill -KILL "$child_pid" 2>/dev/null || true
        done
      fi
      kill -KILL "$wait_pid" 2>/dev/null || true
    fi
  fi
) &
watchdog_pid=$!

set +e
wait "$wait_pid"
wait_status=$?
set -e
kill "$watchdog_pid" 2>/dev/null || true
wait "$watchdog_pid" 2>/dev/null || true

if [[ -f "$watchdog_marker" ]]; then
  printf '124\n'
elif [[ "$wait_status" -ne 0 ]]; then
  if [[ "$wait_status" -eq 124 || "$wait_status" -eq 137 ]]; then
    printf '124\n'
  else
    echo "docker wait failed with status $wait_status; stopping the scale container." >&2
    timeout --kill-after=5s 20 docker kill "$container_name" >/dev/null 2>&1 || true
    printf '125\n'
  fi
else
  container_status="$(head -n 1 "$wait_output_file" 2>/dev/null || true)"
  if [[ "$container_status" =~ ^[0-9]+$ ]]; then
    printf '%s\n' "$container_status"
  else
    echo "docker wait returned no numeric container status." >&2
    printf '125\n'
  fi
fi
