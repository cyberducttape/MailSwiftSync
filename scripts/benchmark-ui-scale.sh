#!/usr/bin/env bash
set -euo pipefail

# Measures the pure cached-filter path and explicit selection-all operation at
# 100k rows. Run on the release host class and retain the output with release
# evidence; this is not a substitute for a rendered first-frame/UI snapshot.
cargo test --locked --release scale_ui_benchmark -- \
  --ignored --nocapture --test-threads=1
