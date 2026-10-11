#!/usr/bin/env bash
set -euo pipefail

# Measures metadata-only verification staging and reconciliation on a durable
# (FULL-synchronous) stage at increasing per-side message counts. Each size runs
# in its own process so peak RSS (VmHWM) is per size. This is opt-in: the 1M
# case writes roughly 1 GB of temporary SQLite data.
# The benchmark line also includes phase-separated /proc/self/io read/write
# bytes and syscall counts; cache-backed stages may legitimately report zero
# block bytes. It does not claim allocator-level allocation counts.
#
# Gates (override for a qualified host class):
#   MAILSWIFTSYNC_VERIFY_PEAK_RSS_BUDGET_MIB  controller peak RSS, any size (64)
#   MAILSWIFTSYNC_VERIFY_MIN_RECONCILE_RATE   reconciled messages/sec, any size (20000)
#   MAILSWIFTSYNC_VERIFY_SIZES                space-separated per-side counts
#   MAILSWIFTSYNC_VERIFY_SHAPES               space-separated fixture shapes
sizes=${MAILSWIFTSYNC_VERIFY_SIZES:-"1000 10000 100000 500000 1000000"}
shapes=${MAILSWIFTSYNC_VERIFY_SHAPES:-typical}
rss_budget=${MAILSWIFTSYNC_VERIFY_PEAK_RSS_BUDGET_MIB:-64}
min_rate=${MAILSWIFTSYNC_VERIFY_MIN_RECONCILE_RATE:-20000}

cargo test --locked --release --no-run --bin mailswiftsync >/dev/null 2>&1

metric() {
  local line="$1" name="$2" value
  value=${line##* "$name"=}
  value=${value%% *}
  if [[ ! "$value" =~ ^[0-9]+$ ]]; then
    printf 'ERROR: invalid %s in benchmark result: %s\n' "$name" "$line" >&2
    exit 1
  fi
  printf '%s' "$value"
}

for shape in $shapes; do
  for size in $sizes; do
    output=$(MAILSWIFTSYNC_RECONCILIATION_BENCH_MESSAGES="$size" \
      MAILSWIFTSYNC_RECONCILIATION_BENCH_SHAPE="$shape" \
      cargo test --locked --release --bin mailswiftsync \
        durable_stage_reconciliation_benchmark -- --ignored --nocapture --test-threads=1 2>&1)
    # Match all keyed dimensions rather than assuming positional output, so
    # additional benchmark dimensions cannot make a different result pass.
    line=$(printf '%s\n' "$output" | awk -v size="$size" -v shape="$shape" \
      '$0 ~ /verification-scale/ && index($0, "shape=" shape " ") && index($0, "messages_per_side=" size " ") { print; exit }')
    if [[ -z "$line" ]]; then
      printf '%s\n' "$output" >&2
      printf 'ERROR: missing %s-shape, %s-message verification benchmark result\n' "$shape" "$size" >&2
      exit 1
    fi
    printf '%s\n' "$line"
    peak_rss=$(metric "$line" peak_rss_mib)
    rate=$(metric "$line" reconcile_messages_per_sec)
    if (( peak_rss > rss_budget )); then
      printf 'ERROR: %s shape at %s messages/side peaked at %s MiB RSS; budget is %s MiB\n' "$shape" "$size" "$peak_rss" "$rss_budget" >&2
      exit 1
    fi
    if (( rate < min_rate )); then
      printf 'ERROR: %s shape at %s messages/side reconciled at %s msg/s; minimum is %s\n' "$shape" "$size" "$rate" "$min_rate" >&2
      exit 1
    fi
    printf 'PASS: %s shape, %s messages/side, peak RSS %s MiB <= %s MiB, reconcile %s msg/s >= %s\n' \
      "$shape" "$size" "$peak_rss" "$rss_budget" "$rate" "$min_rate"
  done
done
