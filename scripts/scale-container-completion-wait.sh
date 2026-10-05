#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 3 ]]; then
  echo "Usage: $0 <progress-file> <watchdog-marker> <deadline-seconds>" >&2
  exit 2
fi

progress_file="$1"
watchdog_marker="$2"
deadline_seconds="$3"
if ! [[ "$deadline_seconds" =~ ^[1-9][0-9]*$ ]]; then
  echo "Completion deadline must be a positive integer." >&2
  exit 2
fi

progress_directory="$(dirname -- "$progress_file")"
if [[ -e "$progress_directory" && ! ( -r "$progress_directory" && -x "$progress_directory" ) ]]; then
  # A marker in an unreadable directory can never be observed; fail now
  # instead of waiting out the whole deadline.
  echo "Cannot read the scale progress directory $progress_directory; run this watcher with access to it." >&2
  printf 'completion_watchdog_unreadable_progress=true\n' > "$watchdog_marker"
  printf '125\n'
  exit 0
fi

deadline=$((SECONDS + deadline_seconds))
while (( SECONDS < deadline )); do
  if [[ -f "$progress_file" ]]; then
    completion_line="$(grep -E '^harness_cleanup_complete=true exit_status=[0-9]+$' "$progress_file" | tail -n 1 || true)"
    if [[ -n "$completion_line" ]]; then
      harness_status="${completion_line##*=}"
      # Do not synchronously contact Docker after the harness has completed.
      # Dovecot can leave the disposable container uninterruptible while the
      # hosted runner's Docker daemon waits for it. The runner VM is ephemeral;
      # its teardown owns disposal of this isolated fixture container.
      printf 'harness_exit_status=%s\ncontainer_teardown=delegated_to_ephemeral_runner\n' \
        "$harness_status" >&2
      printf '%s\n' "$harness_status"
      exit 0
    fi
  fi
  sleep 1
done

echo "Scale harness did not write its completion marker before the ${deadline_seconds}-second deadline." >&2
printf 'completion_watchdog_triggered=true\n' > "$watchdog_marker"
printf '124\n'
