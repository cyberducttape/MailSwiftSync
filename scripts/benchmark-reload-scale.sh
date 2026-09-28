#!/usr/bin/env bash
set -euo pipefail

# Measures restart and bounded workspace reads for the documented 100k-row
# durable project. This is intentionally opt-in because it creates a large
# temporary SQLite fixture and is not part of the normal test suite.
cargo test --locked --release workspace_reload_scale_benchmark -- --ignored --nocapture --test-threads=1
