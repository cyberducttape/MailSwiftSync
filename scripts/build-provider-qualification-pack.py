#!/usr/bin/env python3
"""Build one immutable provider-pair qualification pack from phase evidence.

The release gate remains authoritative for the full policy. This tool creates
the customer-facing, directed provider-pair artifact only after it sees exactly
one passing record for every required phase in the selected policy row. It
deliberately copies summaries and digests, never endpoints, mailbox names, or
credentials.

Usage:
  build-provider-qualification-pack.py <evidence-dir> <source> <destination>
      <output.json> [--policy <policy.json>]
"""

from __future__ import annotations

import hashlib
import json
import re
import sys
from datetime import datetime, timezone
from pathlib import Path


SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
REQUIRED_PHASES = {"dry_pilot", "live_pilot", "recovery_test"}
REQUIRED_IDENTITY = (
    "qualification_bundle_id",
    "mailswiftsync_version",
    "mailswiftsync_commit",
    "mailswiftsync_binary_sha256",
    "imapsync_binary_sha256",
    "engine",
    "engine_version",
    "source_provider",
    "destination_provider",
    "source_auth_method",
    "destination_auth_method",
)


def fail(message: str) -> "NoReturn":
    raise ValueError(message)


def canonical_json(value: object) -> bytes:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode(
        "utf-8"
    )


def sha256(value: object) -> str:
    return hashlib.sha256(canonical_json(value)).hexdigest()


def load_json(path: Path) -> dict:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"{path}: invalid JSON: {error}")
    if not isinstance(value, dict):
        fail(f"{path}: root must be an object")
    return value


def policy_row(policy_path: Path, pair: str) -> dict:
    policy = load_json(policy_path)
    rows = policy.get("providers")
    if not isinstance(rows, list):
        fail("provider policy must contain a providers array")
    matches = [row for row in rows if isinstance(row, dict) and row.get("pair") == pair]
    if len(matches) != 1:
        fail(f"provider policy must contain exactly one row for {pair}")
    row = matches[0]
    phases = row.get("required_phases")
    if set(phases or []) != REQUIRED_PHASES:
        fail(f"{pair}: qualification packs require exactly {sorted(REQUIRED_PHASES)}")
    return row


def validate_evidence(value: dict, path: Path, pair: str, policy: dict) -> None:
    expected_source, expected_destination = pair.split("->", 1)
    if value.get("source_provider") != expected_source or value.get("destination_provider") != expected_destination:
        fail(f"{path}: provider pair does not match {pair}")
    phase = value.get("testing_phase")
    if phase not in REQUIRED_PHASES:
        fail(f"{path}: unsupported or missing testing phase")
    for field in ("tested_at", "run_id", "selected_run_id", "proof_digest", "test_dataset_digest"):
        if not isinstance(value.get(field), str) or not value[field]:
            fail(f"{path}: missing phase field {field}")
    if value["run_id"] != value["selected_run_id"]:
        fail(f"{path}: run_id and selected_run_id must identify the same qualified run")
    if value.get("results", {}).get("overall_result") not in {"pass", "pass_with_exceptions"}:
        fail(f"{path}: overall result is not passing")
    if value.get("proof_verification") != "canonical_digest_verified":
        fail(f"{path}: proof digest was not canonically verified")
    for field in REQUIRED_IDENTITY:
        if not isinstance(value.get(field), str) or not value[field]:
            fail(f"{path}: missing identity field {field}")
    for field in ("mailswiftsync_binary_sha256", "imapsync_binary_sha256", "proof_digest", "test_dataset_digest"):
        if not SHA256_RE.fullmatch(value[field]):
            fail(f"{path}: {field} must be a lowercase SHA-256 digest")
    scenarios = value.get("scenario_ids")
    required_by_phase = policy.get("required_scenarios_by_phase", {}).get(phase, [])
    if not isinstance(scenarios, list) or not set(required_by_phase).issubset(scenarios):
        fail(f"{path}: required scenarios for {phase} are missing")
    summary = value.get("test_summary")
    if not isinstance(summary, dict) or not all(
        isinstance(summary.get(field), int) and summary[field] >= 0
        for field in (
            "mailboxes_tested",
            "messages_total",
            "bytes_total",
            "destination_messages_total",
            "destination_bytes_total",
            "folders_total",
            "destination_folders_total",
        )
    ):
        fail(f"{path}: test_summary counters are incomplete")


def build_pack(evidence_dir: Path, source: str, destination: str, policy_path: Path) -> dict:
    pair = f"{source}->{destination}"
    policy = policy_row(policy_path, pair)
    records = []
    for path in sorted(evidence_dir.glob("*.json")):
        if path.name in {policy_path.name, "schema.json", "pack-schema.json"}:
            continue
        value = load_json(path)
        if value.get("source_provider") == source and value.get("destination_provider") == destination:
            validate_evidence(value, path, pair, policy)
            records.append((path, value))
    by_phase: dict[str, tuple[Path, dict]] = {}
    for path, value in records:
        phase = value["testing_phase"]
        if phase in by_phase:
            fail(f"{pair}: multiple evidence records for phase {phase}; select one immutable run")
        by_phase[phase] = (path, value)
    if set(by_phase) != REQUIRED_PHASES:
        fail(f"{pair}: missing qualification phases: {sorted(REQUIRED_PHASES - set(by_phase))}")

    ordered = [by_phase[phase] for phase in sorted(REQUIRED_PHASES)]
    _, first = ordered[0]
    identity_fields = (
        "qualification_bundle_id",
        "mailswiftsync_version",
        "mailswiftsync_commit",
        "mailswiftsync_binary_sha256",
        "imapsync_binary_sha256",
        "engine",
        "engine_version",
        "source_auth_method",
        "destination_auth_method",
    )
    for _, value in ordered[1:]:
        for field in identity_fields:
            if value.get(field) != first.get(field):
                fail(f"{pair}: phase identity mismatch in {field}")

    phases = []
    for path, value in ordered:
        phases.append(
            {
                "phase": value["testing_phase"],
                "evidence_file": path.name,
                "tested_at": value["tested_at"],
                "run_id": value["run_id"],
                "proof_digest": value["proof_digest"],
                "test_dataset_digest": value["test_dataset_digest"],
                "scenario_ids": sorted(value["scenario_ids"]),
                "test_summary": value["test_summary"],
                "results": value["results"],
            }
        )
    tested_at = sorted(phase["tested_at"] for phase in phases)
    return {
        "format": "mailswiftsync-provider-qualification-pack",
        "schema_version": 1,
        "provider_pair": pair,
        "status": "qualified",
        "qualification_bundle_id": first["qualification_bundle_id"],
        "tested": {
            "source_provider": source,
            "destination_provider": destination,
            "source_auth_method": first["source_auth_method"],
            "destination_auth_method": first["destination_auth_method"],
            "engine": first["engine"],
            "engine_version": first["engine_version"],
            "mailswiftsync_version": first["mailswiftsync_version"],
            "mailswiftsync_commit": first["mailswiftsync_commit"],
            "mailswiftsync_binary_sha256": first["mailswiftsync_binary_sha256"],
            "imapsync_binary_sha256": first["imapsync_binary_sha256"],
        },
        "qualification_window": {"started_at": tested_at[0], "finished_at": tested_at[-1]},
        "phases": phases,
        "known_limitations": [
            "Qualification is bound to the recorded provider pair, engine, binary, and release commit.",
            "The pack is evidence of the tested scenarios; it is not a universal provider guarantee.",
            "A release must still pass scripts/verify-evidence-gate.sh --release with the retained evidence bundle.",
        ],
        "evidence_digest": sha256(phases),
        "generated_at": datetime.now(timezone.utc).isoformat(),
    }


def main() -> int:
    if len(sys.argv) < 5:
        print(__doc__, file=sys.stderr)
        return 2
    evidence_dir = Path(sys.argv[1])
    source = sys.argv[2]
    destination = sys.argv[3]
    output = Path(sys.argv[4])
    policy = Path("tests/provider-evidence/policy.json")
    index = 5
    while index < len(sys.argv):
        if sys.argv[index] == "--policy" and index + 1 < len(sys.argv):
            policy = Path(sys.argv[index + 1])
            index += 2
        else:
            print(f"unknown argument: {sys.argv[index]}", file=sys.stderr)
            return 2
    try:
        pack = build_pack(evidence_dir, source, destination, policy)
        output.write_text(json.dumps(pack, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    except (OSError, ValueError) as error:
        print(f"Qualification pack refused: {error}", file=sys.stderr)
        return 1
    print(f"Qualification pack generated: {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
