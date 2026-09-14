#!/usr/bin/env bash
set -euo pipefail

matrix_path="${1:-docs/compatibility-matrix.md}"
if [[ ! -f "$matrix_path" ]]; then
  echo "FAIL: compatibility matrix not found: $matrix_path" >&2
  exit 1
fi

# Keep this gate deliberately structural. It verifies that the release has a
# real, reviewable data row and that every gate column is populated, while the
# matrix itself remains the authority for whether a row is supported.
awk '
  function trim(value) {
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
    return value
  }
  /^\|[[:space:]]*Source[[:space:]]*\|/ {
    header = 1
    next
  }
  header && /^\|[[:space:]]*:?-+[[:space:]]*\|/ {
    next
  }
  header && /^\|/ {
    fields = split($0, columns, "|")
    if (fields - 2 != 11) {
      printf "FAIL: compatibility matrix row has %d columns; expected 11: %s\n", fields - 2, $0 > "/dev/stderr"
      failed = 1
      next
    }
    for (column = 1; column <= 11; column++) {
      if (trim(columns[column + 1]) == "") {
        printf "FAIL: compatibility matrix row has an empty column %d: %s\n", column, $0 > "/dev/stderr"
        failed = 1
      }
    }
    rows++
  }
  END {
    if (!header) {
      print "FAIL: compatibility matrix table header is missing" > "/dev/stderr"
      exit 1
    }
    if (rows == 0) {
      print "FAIL: compatibility matrix has no data rows" > "/dev/stderr"
      exit 1
    }
    if (failed) {
      exit 1
    }
    printf "Compatibility matrix structure verified: %d data row(s)\n", rows
  }
' "$matrix_path"
