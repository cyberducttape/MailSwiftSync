#!/usr/bin/env bash
# Evidence-based provider compatibility release gate.
# Requirements come from tests/provider-evidence/policy.json, never from
# wording in the Markdown compatibility matrix.
set -euo pipefail

EVIDENCE_DIR="${MAILSWIFTSYNC_EVIDENCE_DIR:-tests/provider-evidence}"
POLICY_FILE="$EVIDENCE_DIR/policy.json"
SCHEMA_FILE="$EVIDENCE_DIR/schema.json"
MODE="preview"
if [[ "${1:-}" == "--release" ]]; then
  MODE="release"
fi
export MAILSWIFTSYNC_GATE_MODE="$MODE"

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
    scenario_requirements = entry.get("scenario_requirements")
    values = [entry.get("pair"), entry.get("source_provider"), entry.get("destination_provider"), entry.get("engine"), entry.get("engine_version"), entry.get("minimum_mailboxes"), entry.get("minimum_messages"), entry.get("minimum_folders"), entry.get("minimum_bytes")]
    if not all(isinstance(value, str) and value for value in values[:5]) or not all(
        isinstance(value, int) and value > 0 for value in values[5:]
    ):
        raise SystemExit("each provider policy entry needs pair, source_provider, destination_provider, engine, and engine_version")
    if not isinstance(required, list) or not required or not all(isinstance(value, str) and value for value in required):
        raise SystemExit(f"{entry.get('pair', '<unknown>')}: required_phases must be non-empty strings")
    if not isinstance(scenario_requirements, dict):
        raise SystemExit(f"{entry.get('pair', '<unknown>')}: scenario_requirements must be an object")
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
        str(entry["minimum_mailboxes"]),
        str(entry["minimum_messages"]),
        str(entry["minimum_folders"]),
        str(entry["minimum_bytes"]),
        ",".join(entry["required_scenarios"]),
        json.dumps(scenario_requirements, separators=(",", ":")),
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
  IFS=$'\t' read -r pair source_provider destination_provider release_required expected_engine expected_version minimum_mailboxes minimum_messages minimum_folders minimum_bytes required_scenarios scenario_requirements_json required_phases <<< "$row"
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

policy_result=$(python3 - "$SCHEMA_FILE" "$pair" "$source_provider" "$destination_provider" "$expected_engine" "$expected_version" "$minimum_mailboxes" "$minimum_messages" "$minimum_folders" "$minimum_bytes" "$required_scenarios" "$scenario_requirements_json" "$required_phases" "${provider_files[@]}" <<'PY'
import json
import hashlib
import os
import re
import sys
from pathlib import Path

try:
    import jsonschema
    has_jsonschema = True
except ImportError:
    raise SystemExit("jsonschema is required; install requirements-provider-evidence.txt")

schema_file, pair, source_provider, destination_provider, expected_engine, expected_version, minimum_mailboxes, minimum_messages, minimum_folders, minimum_bytes, scenarios_csv, scenario_requirements_json, phases_csv, *files = sys.argv[1:]
required_scenarios = set(scenarios_csv.split(","))
scenario_requirements = json.loads(scenario_requirements_json)
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
        "source_auth_method", "destination_auth_method",
        "proof_digest", "proof_verification", "proof_file", "run_id", "selected_run_id", "run_type",
        "run_started_at", "run_finished_at", "fixture_id",
        "mailswiftsync_version", "mailswiftsync_commit", "mailswiftsync_binary_sha256",
        "imapsync_binary_sha256",
        "scenario_ids",
        "scenario_observations", "test_dataset_digest", "qualification_bundle_id",
    }
    missing_root = sorted(required_root - evidence.keys())
    if missing_root:
        errors.append(f"{filename}: missing required fields: {', '.join(missing_root)}")
        continue
    if evidence.get("source_provider") != source_provider or evidence.get("destination_provider") != destination_provider:
        errors.append(f"{filename}: source/destination provider pair does not match policy pair {pair}")
    if not isinstance(evidence.get("scenario_ids"), list) or not required_scenarios.issubset(set(evidence.get("scenario_ids", []))):
        errors.append(f"{filename}: required qualification scenarios are missing")
    summary = evidence.get("test_summary")
    if not isinstance(summary, dict) or not all(
        isinstance(summary.get(field), int) and summary.get(field) >= minimum
        for field, minimum in (("mailboxes_tested", 1), ("messages_total", 0), ("bytes_total", 0), ("destination_messages_total", 0), ("destination_bytes_total", 0), ("folders_total", 0), ("destination_folders_total", 0))
    ):
        errors.append(f"{filename}: test_summary has invalid required counters")
    observations = evidence.get("scenario_observations")
    if not isinstance(observations, dict):
        errors.append(f"{filename}: scenario_observations are missing")
    else:
        for scenario, requirement in scenario_requirements.items():
            if scenario not in evidence.get("scenario_ids", []):
                continue
            if scenario == "large-mailbox-10k" and (observations.get("large_mailbox_10k", {}).get("messages", 0) < requirement.get("minimum_messages", 0) or summary.get("messages_total", 0) < requirement.get("minimum_messages", 0)):
                errors.append(f"{filename}: large-mailbox-10k does not contain 10,000 messages")
            if scenario == "large-messages" and (observations.get("large_messages", {}).get("maximum_message_bytes", 0) < requirement.get("minimum_message_bytes", 0) or summary.get("maximum_message_bytes", 0) < requirement.get("minimum_message_bytes", 0)):
                errors.append(f"{filename}: large-messages does not contain a message at least 10 MiB")
            if scenario == "unicode-folders" and (observations.get("unicode_folders", {}).get("observed") is not True or summary.get("unicode_folders_observed") is not True):
                errors.append(f"{filename}: Unicode folder scenario was not observed")
            if scenario == "special-use-folders" and (observations.get("special_use_folders", {}).get("observed") is not True or summary.get("special_use_folders_observed") is not True):
                errors.append(f"{filename}: SPECIAL-USE folder scenario was not observed")
            if scenario == "mismatch-detection":
                mismatch = observations.get("mismatch_detection", {})
                if mismatch.get("planted") is not True or mismatch.get("detected") is not True or summary.get("mismatch_planted") is not True or summary.get("mismatch_detected") is not True:
                    errors.append(f"{filename}: planted mismatch was not demonstrably detected")
    if evidence.get("testing_phase") not in {"dry_pilot", "live_pilot", "recovery_test", "edge_case"}:
        errors.append(f"{filename}: testing_phase is not a supported evidence phase")
    if evidence.get("engine") != expected_engine:
        errors.append(f"{filename}: engine must be {expected_engine}")
    if evidence.get("engine_version") != expected_version:
        errors.append(f"{filename}: engine_version must be {expected_version}")
    release_commit = os.environ.get("MAILSWIFTSYNC_RELEASE_COMMIT")
    if os.environ.get("MAILSWIFTSYNC_GATE_MODE") == "release" and release_commit and evidence.get("mailswiftsync_commit") != release_commit:
        errors.append(f"{filename}: evidence commit does not match the release commit")
    for field in ("mailswiftsync_version", "mailswiftsync_commit", "mailswiftsync_binary_sha256", "imapsync_binary_sha256"):
        if not isinstance(evidence.get(field), str) or not evidence.get(field):
            errors.append(f"{filename}: {field} is missing")
    if evidence.get("mailswiftsync_commit") == "unknown":
        errors.append(f"{filename}: packaged MailSwiftSync binary has no embedded Git commit")
    for field in ("mailswiftsync_binary_sha256", "imapsync_binary_sha256"):
        value = evidence.get(field, "")
        if not isinstance(value, str) or len(value) != 64 or any(character not in "0123456789abcdefABCDEF" for character in value):
            errors.append(f"{filename}: {field} is not a SHA-256 digest")
    results = evidence.get("results")
    if not isinstance(results, dict):
        errors.append(f"{filename}: results must be an object")
        continue
    if results.get("overall_result") not in {"pass", "pass_with_exceptions", "fail"}:
        errors.append(f"{filename}: results.overall_result is invalid")
    if not isinstance(results.get("messages_covered_by_aggregate_evidence"), int) or results.get("messages_covered_by_aggregate_evidence") < 0:
        errors.append(f"{filename}: results.messages_covered_by_aggregate_evidence is invalid")
    if "messages_verified" in results and (not isinstance(results.get("messages_verified"), int) or results.get("messages_verified") < 0):
        errors.append(f"{filename}: results.messages_verified is invalid")
    if results.get("verification_confidence") not in {"high", "medium", "low", "aggregate_only", "not_applicable"}:
        errors.append(f"{filename}: results.verification_confidence is invalid")
    if evidence.get("proof_verification") != "canonical_digest_verified":
        errors.append(f"{filename}: customer-proof canonical digest was not verified")
    proof_digest = evidence.get("proof_digest", "")
    if not isinstance(proof_digest, str) or not re.fullmatch(r"[0-9a-f]{64}", proof_digest):
        errors.append(f"{filename}: proof_digest is not a canonical SHA-256 digest")
    proof_file = evidence.get("proof_file")
    proof_path = Path(filename).parent / proof_file if isinstance(proof_file, str) else None
    proof = None
    if proof_path is None or proof_path.name != proof_file:
        errors.append(f"{filename}: proof_file must be a basename in the evidence directory")
    elif not proof_path.is_file():
        errors.append(f"{filename}: referenced customer proof is missing: {proof_file}")
    else:
        try:
            with proof_path.open(encoding="utf-8") as handle:
                proof = json.load(handle)
            unsigned = dict(proof)
            expected_proof_digest = unsigned.pop("proof_digest", None)
            unsigned.pop("proof_signature", None)
            canonical = json.dumps(unsigned, separators=(",", ":"), ensure_ascii=False, sort_keys=True)
            calculated = hashlib.sha256(canonical.encode("utf-8")).hexdigest()
            if expected_proof_digest != calculated or expected_proof_digest != proof_digest:
                errors.append(f"{filename}: referenced customer proof digest does not match evidence")
        except (OSError, json.JSONDecodeError, TypeError) as error:
            errors.append(f"{filename}: referenced customer proof is invalid ({error})")
    if isinstance(proof, dict):
        identity = proof.get("provider_identity")
        if not isinstance(identity, dict):
            errors.append(f"{filename}: customer proof lacks provider identity")
        else:
            for field in ("source_provider", "destination_provider", "source_auth_method", "destination_auth_method", "fixture_id"):
                if evidence.get(field) != identity.get(field):
                    errors.append(f"{filename}: {field} does not match customer proof")
        project_id = proof.get("project", {}).get("project_id") if isinstance(proof.get("project"), dict) else None
        project_dataset_digest = proof.get("project", {}).get("dataset_digest") if isinstance(proof.get("project"), dict) else None
        if project_dataset_digest != evidence.get("test_dataset_digest"):
            errors.append(f"{filename}: dataset digest does not match the referenced customer proof")
        runs = proof.get("runs")
        selected = [run for run in runs or [] if isinstance(run, dict) and run.get("run_id") == evidence.get("selected_run_id")]
        if not isinstance(runs, list) or len(selected) != 1:
            errors.append(f"{filename}: selected run is not uniquely present in customer proof")
        else:
            run = selected[0]
            if run.get("project_id") != project_id or run.get("status") != "completed":
                errors.append(f"{filename}: selected proof run is not a completed run for its project")
            if run.get("engine") != evidence.get("engine"):
                errors.append(f"{filename}: evidence engine does not match the selected proof run")
            if run.get("engine_version") != evidence.get("engine_version"):
                errors.append(f"{filename}: evidence engine version does not match the selected proof run")
            if run.get("started_at") != evidence.get("run_started_at") or run.get("finished_at") != evidence.get("run_finished_at"):
                errors.append(f"{filename}: selected run timestamps do not match customer proof")
            expected_type = {"dry_pilot": "preflight", "live_pilot": "live", "recovery_test": "recovery"}.get(evidence.get("testing_phase"))
            if evidence.get("run_type") != expected_type:
                errors.append(f"{filename}: evidence run_type does not match testing phase")
            if evidence.get("testing_phase") == "recovery_test":
                predecessors = [ancestor for ancestor in runs if isinstance(ancestor, dict) and ancestor.get("status") == "abandoned" and ancestor.get("project_id") == run.get("project_id") and ancestor.get("job_id") == run.get("job_id") and ancestor.get("started_at", "") < run.get("started_at", "")]
                if not predecessors:
                    errors.append(f"{filename}: recovery proof lacks an abandoned predecessor for the selected job")
    if summary.get("mailboxes_tested", 0) < int(minimum_mailboxes):
        errors.append(f"{filename}: too few mailboxes for qualification")
    phase = evidence.get("testing_phase")
    expected_run_type = {"dry_pilot": "preflight", "live_pilot": "live", "recovery_test": "recovery"}.get(phase)
    if expected_run_type is None:
        errors.append(f"{filename}: unsupported phase cannot qualify")
    elif evidence.get("run_type") != expected_run_type:
        errors.append(f"{filename}: run_type must be {expected_run_type} for {phase}")
    if evidence.get("run_id") != evidence.get("selected_run_id"):
        errors.append(f"{filename}: run_id must identify the selected phase run")
    if not isinstance(evidence.get("run_started_at"), str) or not isinstance(evidence.get("run_finished_at"), str):
        errors.append(f"{filename}: phase run timestamps are required")
    if phase == "dry_pilot":
        if results.get("verification_confidence") != "not_applicable":
            errors.append(f"{filename}: dry_pilot confidence must be not_applicable")
        if any(summary.get(field) != 0 for field in ("messages_total", "bytes_total", "destination_messages_total", "destination_bytes_total")):
            errors.append(f"{filename}: dry_pilot must not report transfer totals")
    else:
        if summary.get("messages_total", 0) < int(minimum_messages):
            errors.append(f"{filename}: test dataset is too small for qualification")
        if summary.get("folders_total", 0) < int(minimum_folders):
            errors.append(f"{filename}: too few source folders for qualification")
        if summary.get("bytes_total", 0) < int(minimum_bytes):
            errors.append(f"{filename}: test dataset has too few bytes for qualification")
        if summary.get("messages_total") != summary.get("destination_messages_total"):
            errors.append(f"{filename}: source/destination message totals differ")
        if summary.get("bytes_total") != summary.get("destination_bytes_total"):
            errors.append(f"{filename}: source/destination byte totals differ")
        if summary.get("folders_total") != summary.get("destination_folders_total"):
            errors.append(f"{filename}: source/destination folder totals differ")
        if results.get("messages_verified", 0) < int(minimum_messages):
            errors.append(f"{filename}: too few verified messages for qualification")
        if results.get("verification_confidence") != "high":
            errors.append(f"{filename}: qualification requires high verification confidence")
    if not isinstance(evidence.get("proof_digest"), str) or not evidence.get("proof_digest"):
        errors.append(f"{filename}: proof_digest is missing")
    if phase in required_phases:
        seen_phases.add(phase)
        result = evidence.get("results", {}).get("overall_result")
        if result != "pass":
            errors.append(f"{filename}: {phase} overall_result must be pass, got {result!r}")
missing = sorted(required_phases - seen_phases)
if missing:
        errors.append("missing required phases: " + ", ".join(missing))

# A phase collection is one qualification bundle, not three independent
# assertions. Every required phase must identify the same build, engine,
# fixture, dataset, provider pair, authentication modes, and bundle.
phase_metadata = {}
for filename in files:
    try:
        with open(filename, encoding="utf-8") as handle:
            evidence = json.load(handle)
    except (OSError, json.JSONDecodeError):
        continue
    phase = evidence.get("testing_phase")
    if phase not in required_phases:
        continue
    metadata = tuple(evidence.get(field) for field in (
        "mailswiftsync_version", "mailswiftsync_commit", "mailswiftsync_binary_sha256",
        "engine", "engine_version", "fixture_id", "test_dataset_digest",
        "source_provider", "destination_provider", "source_auth_method",
        "destination_auth_method", "qualification_bundle_id",
    ))
    previous = next(iter(phase_metadata.values()), None)
    if previous is not None and metadata != previous:
        errors.append(f"{filename}: qualification phase metadata does not match the bundle")
    phase_metadata[phase] = metadata
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
