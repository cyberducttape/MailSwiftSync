#!/usr/bin/env bash
set -euo pipefail

strict=0
preview_release=0
if [[ "${1:-}" == "--release" ]]; then
  strict=1
  shift
elif [[ "${1:-}" == "--release-preview" ]]; then
  preview_release=1
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
awk -v strict="$strict" -v preview_release="$preview_release" '
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
      # including work that is still pending. Stable release publication must
      # only proceed when every evidence gate is resolved. Preview releases
      # require the local disposable lab row to be resolved, while unresolved
      # hosted-provider rows must explicitly say that they are unsupported or
      # not run. Columns 7–10 are the dry pilot, live pilot, recovery, and
      # evidence results; column 11 is the qualification note.
      if (strict && column >= 7 && column <= 10 && tolower(trim(columns[column + 1])) ~ /(pending|not tested|not run|no live evidence|outstanding|tbd|n\/a)/) {
        printf "FAIL: release matrix row has unresolved evidence in column %d: %s\n", column, $0 > "/dev/stderr"
        failed = 1
      }
      if (preview_release && column == 7) {
        local_lab = tolower(trim(columns[2])) ~ /disposable local dovecot/ && tolower(trim(columns[3])) ~ /disposable local dovecot/
        if (local_lab) {
          local_rows++
          local_row_unresolved = 0
          for (evidence_column = 7; evidence_column <= 10; evidence_column++) {
            if (tolower(trim(columns[evidence_column + 1])) ~ /(pending|not tested|not run|no live evidence|outstanding|tbd|n\/a)/) {
              local_row_unresolved = 1
            }
          }
          if (local_row_unresolved) {
            printf "FAIL: preview matrix local lab row has unresolved evidence: %s\n", $0 > "/dev/stderr"
            failed = 1
          }
        }
      }
      if (preview_release && column == 11 && !local_lab) {
        notes = tolower(trim(columns[12]))
        unresolved = 0
        for (evidence_column = 7; evidence_column <= 10; evidence_column++) {
          if (tolower(trim(columns[evidence_column + 1])) ~ /(pending|not tested|not run|no live evidence|outstanding|tbd|n\/a)/) {
            unresolved = 1
          }
        }
        if (unresolved && notes !~ /(not run|no live evidence|unsupported|not generally supported|code\/scenario coverage only)/) {
          printf "FAIL: preview matrix unresolved hosted-provider row lacks explicit unsupported/not-run labeling: %s\n", $0 > "/dev/stderr"
          failed = 1
        }
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
    if (preview_release && local_rows == 0) {
      print "FAIL: preview compatibility matrix has no disposable local Dovecot lab row" > "/dev/stderr"
      exit 1
    }
    if (failed) {
      exit 1
    }
    printf "Compatibility matrix structure verified: %d data row(s)\n", rows
  }
' "$matrix_path"
