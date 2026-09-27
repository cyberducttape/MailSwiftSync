#!/usr/bin/env bash
set -euo pipefail

# This is intentionally opt-in: the 100k XLSX case is a real desktop-scale
# workload and should run on the release host class used for a readiness claim.
# The benchmark prints elapsed time, input bytes, imported rows, and Linux RSS
# before/after each case. Keep the output with the release evidence bundle.
cargo test --locked --release scale_import_benchmark -- \
  --ignored --nocapture --test-threads=1
