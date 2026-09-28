#!/usr/bin/env bash
set -euo pipefail

# Measures cached filtering, explicit selection, state refresh, and a real
# virtualized egui first frame at 100k rows. Run on the release host class and
# retain the output with release evidence; this is not a substitute for a
# full application startup/render budget.
cargo test --locked --release scale_ui_benchmark -- \
  --ignored --nocapture --test-threads=1
