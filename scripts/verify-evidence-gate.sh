#!/usr/bin/env bash
# Evidence-based provider compatibility release gate.
# Requirements come from tests/provider-evidence/policy.json, never from
# wording in the Markdown compatibility matrix.
set -euo pipefail

EVIDENCE_DIR="tests/provider-evidence"
POLICY_FILE="$EVIDENCE_DIR/policy.json"
SCHEMA_FILE="$EVIDENCE_DIR/schema.json"
MODE="preview"
if [[ "${1:-}" == "--release" ]]; then
  MODE="release"
fi

if [[ ! -f "$POLICY_FILE" ]]; then
  echo "FAIL: provider evidence policy not found: $POLICY_FILE" >&2
  exit 1
fi
if [[ ! -f "$SCHEMA_FILE" ]]; then
  echo "FAIL: provider evidence schema not found: $SCHEMA_FILE" >&2
  exit 1
fi

echo "Provider Compatibility Evidence Gate (Mode: $MODE)"
echo "=================================================="

# Use Python only for JSON parsing so the gate does not depend on jq being
# installed on CI runners.
mapfile -t POLICY_ROWS < <(python3 - "$POLICY_FILE" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    policy = json.load(handle)

providers = policy.get("providers")
if not isinstance(providers, list) or not providers:
    raise SystemExit("provider evidence policy must contain a non-empty providers array")

seen = set()
for entry in providers:
    required = entry.get("required_phases")
    values = [entry.get("pair"), entry.get("source_provider"), entry.get("destination_provider"), entry.get("engine"), entry.get("engine_version")]
    if not all(isinstance(value, str) and value for value in values):
        raise SystemExit("each provider policy entry needs pair, source_provider, destination_provider, engine, and engine_version")
    if not isinstance(required, list) or not required or not all(isinstance(value, str) and value for value in required):
        raise SystemExit(f"{entry.get('pair', '<unknown>')}: required_phases must be non-empty strings")
    if entry["pair"] in seen:
        raise SystemExit(f"duplicate provider-pair policy entry: {entry['pair']}")
    seen.add(entry["pair"])
    print("\t".join([
        entry["pair"],
        entry["source_provider"],
        entry["destination_provider"],
        "true" if entry.get("release_required") is True else "false",
        entry["engine"],
        entry["engine_version"],
        ",".join(required),
    ]))
PY
)

if [[ ${#POLICY_ROWS[@]} -eq 0 ]]; then
  echo "FAIL: provider evidence policy contains no providers" >&2
  exit 1
fi

mapfile -t EVIDENCE_FILES < <(find "$EVIDENCE_DIR" -maxdepth 1 -type f -name '*.json' ! -name 'schema.json' ! -name 'policy.json' | sort)
declare -a FAILURES=()

for row in "${POLICY_ROWS[@]}"; do
  IFS=$'\t' read -r pair source_provider destination_provider release_required expected_engine expected_version required_phases <<< "$row"
  if [[ "$MODE" == "release" && "$release_required" != "true" ]]; then
    echo "Skipped non-release provider pair: $pair"
    continue
  fi
  provider_files=()
  for evidence_file in "${EVIDENCE_FILES[@]}"; do
    [[ -n "$evidence_file" ]] || continue
    file_provider=$(python3 - "$evidence_file" <<'PY'
import json
import sys
try:
    with open(sys.argv[1], encoding="utf-8") as handle:
        evidence = json.load(handle)
        value = f"{evidence.get('source_provider', '')}->{evidence.get('destination_provider', '')}"
except (OSError, json.JSONDecodeError):
        value = ""
print(value)
PY
)
    if [[ "$file_provider" == "$pair" || "$file_provider" == "$source_provider->$destination_provider" ]]; then
      provider_files+=("$evidence_file")
    fi
  done

  if [[ ${#provider_files[@]} -eq 0 ]]; then
    FAILURES+=("$pair: no evidence files")
    continue
  fi

policy_result=$(python3 - "$SCHEMA_FILE" "$pair" "$source_provider" "$destination_provider" "$expected_engine" "$expected_version" "$required_phases" "${provider_files[@]}" <<'PY'
import json
import sys

try:
    import jsonschema
    has_jsonschema = True
except ImportError:
    has_jsonschema = False

schema_file, pair, source_provider, destination_provider, expected_engine, expected_version, phases_csv, *files = sys.argv[1:]
required_phases = set(phases_csv.split(","))
seen_phases = set()
errors = []

# Load schema for validation
schema = None
if has_jsonschema:
    try:
        with open(schema_file, encoding="utf-8") as handle:
            schema = json.load(handle)
    except (OSError, json.JSONDecodeError) as error:
        errors.append(f"Warning: Could not load schema for validation: {error}")

for filename in files:
    try:
        with open(filename, encoding="utf-8") as handle:
            evidence = json.load(handle)
    except (OSError, json.JSONDecodeError) as error:
        errors.append(f"{filename}: invalid JSON ({error})")
        continue

    # Validate against schema if available
    if schema and has_jsonschema:
        try:
            jsonschema.validate(evidence, schema)
        except jsonschema.ValidationError as error:
            errors.append(f"{filename}: schema validation failed: {error.message}")
            continue

    required_root = {
        "provider", "source_provider", "destination_provider", "tested_at", "testing_phase", "source_version",
        "destination_version", "engine", "engine_version", "test_summary", "results",
    }
    missing_root = sorted(required_root - evidence.keys())
    if missing_root:
        errors.append(f"{filename}: missing required fields: {', '.join(missing_root)}")
        continue
    if evidence.get("source_provider") != source_provider or evidence.get("destination_provider") != destination_provider:
        errors.append(f"{filename}: source/destination provider pair does not match policy pair {pair}")
    if evidence.get("testing_phase") not in {"dry_pilot", "live_pilot", "recovery_test", "edge_case"}:
        errors.append(f"{filename}: testing_phase is not a supported evidence phase")
    if evidence.get("engine") != expected_engine:
        errors.append(f"{filename}: engine must be {expected_engine}")
    if evidence.get("engine_version") != expected_version:
        errors.append(f"{filename}: engine_version must be {expected_version}")
    summary = evidence.get("test_summary")
    if not isinstance(summary, dict) or not all(
        isinstance(summary.get(field), int) and summary.get(field) >= minimum
        for field, minimum in (("mailboxes_tested", 1), ("messages_total", 0), ("bytes_total", 0))
    ):
        errors.append(f"{filename}: test_summary has invalid required counters")
    results = evidence.get("results")
    if not isinstance(results, dict):
        errors.append(f"{filename}: results must be an object")
        continue
    if results.get("overall_result") not in {"pass", "pass_with_exceptions", "fail"}:
        errors.append(f"{filename}: results.overall_result is invalid")
    if not isinstance(results.get("messages_verified"), int) or results.get("messages_verified") < 0:
        errors.append(f"{filename}: results.messages_verified is invalid")
    if results.get("verification_confidence") not in {"high", "medium", "low"}:
        errors.append(f"{filename}: results.verification_confidence is invalid")
    phase = evidence.get("testing_phase")
    if phase in required_phases:
        seen_phases.add(phase)
        result = evidence.get("results", {}).get("overall_result")
        if result != "pass":
            errors.append(f"{filename}: {phase} overall_result must be pass, got {result!r}")
missing = sorted(required_phases - seen_phases)
if missing:
    errors.append("missing required phases: " + ", ".join(missing))
for error in errors:
    print(error)
PY
  )
  while IFS= read -r error; do
    [[ -n "$error" ]] && FAILURES+=("$pair: $error")
  done <<< "$policy_result"

  if [[ "$release_required" == "true" ]]; then
    echo "Checked release-required provider pair: $pair"
  else
    echo "Checked non-release provider pair: $pair"
  fi
done

if [[ ${#FAILURES[@]} -gt 0 ]]; then
  echo ""
  printf '%s\n' "${FAILURES[@]}" | sed 's/^/FAIL: /'
  if [[ "$MODE" == "release" ]]; then
    echo "FAIL: release blocked by provider evidence policy" >&2
    exit 1
  fi
  echo "Preview mode: evidence policy failures are non-blocking"
else
  echo "PASS: all provider evidence policy checks passed"
fi
