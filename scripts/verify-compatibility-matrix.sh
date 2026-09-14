#!/usr/bin/env bash
set -euo pipefail

strict=0
if [[ "${1:-}" == "--release" ]]; then
  strict=1
  shift
fi
matrix_path="${1:-docs/compatibility-matrix.md}"
if [[ ! -f "$matrix_path" ]]; then
  echo "FAIL: compatibility matrix not found: $matrix_path" >&2
  exit 1
fi

# Keep this gate deliberately structural. It verifies that the release has a
# real, reviewable data row and that every gate column is populated, while the
# matrix itself remains the authority for whether a row is supported.
awk -v strict="$strict" '
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
      # The normal branch gate accepts an explicitly documented lab row,
      # including work that is still pending. Release publication must only
      # proceed when every evidence gate is resolved. Columns 7–10 are the
      # dry pilot, live pilot, recovery, and evidence results.
      if (strict && column >= 7 && column <= 10 && tolower(trim(columns[column + 1])) ~ /(pending|not tested|outstanding|tbd|n\/a)/) {
        printf "FAIL: release matrix row has unresolved evidence in column %d: %s\n", column, $0 > "/dev/stderr"
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
