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
import math
import os
import re
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path


SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
REQUIRED_PHASES = {"dry_pilot", "live_pilot", "recovery_test"}
AB_SCENARIOS = {
    "large-mailbox-1gb", "large-mailbox-10gb", "large-mailbox-20gb",
    "large-mailbox-50gb", "large-mailbox-100gb",
}
AB_DIGEST_FIELDS = (
    "dataset_digest", "engine_binary_sha256", "engine_settings_sha256",
    "source_snapshot_sha256", "destination_baseline_sha256",
    "baseline_artifact_sha256", "managed_artifact_sha256",
)
AB_ARTIFACTS = (
    ("baseline_artifact_file", "baseline_artifact_sha256"),
    ("managed_artifact_file", "managed_artifact_sha256"),
)
AB_METRIC_FIELDS = (
    "wall_seconds", "messages", "bytes", "peak_rss_bytes", "cpu_seconds",
    "disk_growth_bytes", "retries", "throttled_seconds",
)
SUMMARY_COUNTERS = (
    "mailboxes_tested",
    "messages_total",
    "bytes_total",
    "destination_messages_total",
    "destination_bytes_total",
    "folders_total",
    "destination_folders_total",
)
RESULT_FIELDS = (
    "overall_result",
    "messages_covered_by_aggregate_evidence",
    "messages_verified",
    "verification_confidence",
    "mismatches",
    "exceptions_accepted",
)
MISMATCH_FIELDS = ("exact_match", "content_match", "missing", "extra", "changed")
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
    "source_version",
    "destination_version",
    "fixture_id",
)
MUST_BE_KNOWN_IDENTITY = (
    "mailswiftsync_version",
    "source_version",
    "destination_version",
    "fixture_id",
)
UNKNOWN_IDENTITY_VALUES = {"unknown", "unspecified", "n/a", "na", "not available"}


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
    if (
        not isinstance(phases, list)
        or not all(isinstance(phase, str) for phase in phases)
        or set(phases) != REQUIRED_PHASES
    ):
        fail(f"{pair}: qualification packs require exactly {sorted(REQUIRED_PHASES)}")
    return row


def validate_evidence(value: dict, path: Path, pair: str, policy: dict) -> None:
    expected_source, expected_destination = pair.split("->", 1)
    if value.get("source_provider") != expected_source or value.get("destination_provider") != expected_destination:
        fail(f"{path}: provider pair does not match {pair}")
    for field in (
        "engine",
        "engine_version",
        "source_auth_method",
        "destination_auth_method",
    ):
        if value.get(field) != policy.get(field):
            fail(f"{path}: {field} does not match provider policy")
    phase = value.get("testing_phase")
    if phase not in REQUIRED_PHASES:
        fail(f"{path}: unsupported or missing testing phase")
    for field in ("tested_at", "run_id", "selected_run_id", "proof_digest", "test_dataset_digest"):
        if not isinstance(value.get(field), str) or not value[field]:
            fail(f"{path}: missing phase field {field}")
    if value["run_id"] != value["selected_run_id"]:
        fail(f"{path}: run_id and selected_run_id must identify the same qualified run")
    results = value.get("results")
    if not isinstance(results, dict) or results.get("overall_result") != "pass":
        fail(f"{path}: overall result must be an unqualified pass")
    if value.get("proof_verification") != "canonical_digest_verified":
        fail(f"{path}: proof digest was not canonically verified")
    for field in REQUIRED_IDENTITY:
        if not isinstance(value.get(field), str) or not value[field]:
            fail(f"{path}: missing identity field {field}")
    for field in MUST_BE_KNOWN_IDENTITY:
        if value[field].strip().casefold() in UNKNOWN_IDENTITY_VALUES:
            fail(f"{path}: {field} must identify the tested version or fixture")
    if value["mailswiftsync_commit"].strip().casefold() in UNKNOWN_IDENTITY_VALUES:
        fail(f"{path}: MailSwiftSync commit identity is unknown")
    for field in ("mailswiftsync_binary_sha256", "imapsync_binary_sha256", "proof_digest", "test_dataset_digest"):
        if not SHA256_RE.fullmatch(value[field]):
            fail(f"{path}: {field} must be a lowercase SHA-256 digest")
    scenarios = value.get("scenario_ids")
    required_by_phase = policy.get("required_scenarios_by_phase", {}).get(phase, [])
    if (
        not isinstance(scenarios, list)
        or not all(isinstance(scenario, str) and scenario for scenario in scenarios)
        or len(scenarios) != len(set(scenarios))
        or not isinstance(required_by_phase, list)
        or not all(isinstance(scenario, str) for scenario in required_by_phase)
        or not set(required_by_phase).issubset(scenarios)
    ):
        fail(f"{path}: required scenarios for {phase} are missing")
    if phase == "live_pilot":
        observations = value.get("scenario_observations")
        if not isinstance(observations, dict):
            fail(f"{path}: direct-imapsync comparisons require scenario observations")
        for scenario in sorted(AB_SCENARIOS & set(required_by_phase)):
            key = scenario.replace("-", "_")
            scenario_observation = observations.get(key)
            comparison = (
                scenario_observation.get("direct_imapsync_comparison")
                if isinstance(scenario_observation, dict)
                else None
            )
            if not isinstance(comparison, dict):
                fail(f"{path}: {scenario} lacks a direct-imapsync A/B comparison")
            for field in AB_DIGEST_FIELDS:
                if not isinstance(comparison.get(field), str) or not SHA256_RE.fullmatch(comparison[field]):
                    fail(f"{path}: {scenario} A/B {field} must be a lowercase SHA-256 digest")
            for filename_field, digest_field in AB_ARTIFACTS:
                artifact_name = comparison.get(filename_field)
                if not isinstance(artifact_name, str) or not artifact_name or Path(artifact_name).name != artifact_name:
                    fail(f"{path}: {scenario} A/B {filename_field} must be a basename")
                artifact_path = path.parent / artifact_name
                if artifact_path.is_symlink() or not artifact_path.is_file():
                    fail(f"{path}: {scenario} A/B artifact {artifact_name!r} is missing or unsafe")
                try:
                    artifact_digest = hashlib.sha256(artifact_path.read_bytes()).hexdigest()
                except OSError as error:
                    fail(f"{path}: cannot read {scenario} A/B artifact {artifact_name!r}: {error}")
                if artifact_digest != comparison.get(digest_field):
                    fail(f"{path}: {scenario} A/B artifact digest mismatch for {artifact_name!r}")
            if comparison["dataset_digest"] != value["test_dataset_digest"]:
                fail(f"{path}: {scenario} A/B dataset digest differs from the qualified dataset")
            if comparison["engine_binary_sha256"] != value["imapsync_binary_sha256"]:
                fail(f"{path}: {scenario} A/B baseline does not use the qualified imapsync binary")
            if not isinstance(comparison.get("baseline_run_id"), str) or not comparison["baseline_run_id"] or comparison["baseline_run_id"] == value["selected_run_id"]:
                fail(f"{path}: {scenario} A/B baseline must identify a separate direct-engine run")
            baseline = comparison.get("baseline_metrics")
            managed = comparison.get("managed_metrics")
            for label, metrics in (("baseline", baseline), ("managed", managed)):
                if not isinstance(metrics, dict) or any(
                    isinstance(metrics.get(field), bool)
                    or not isinstance(metrics.get(field), (int, float))
                    or not math.isfinite(metrics[field])
                    or metrics[field] < 0
                    or (field == "wall_seconds" and metrics[field] == 0)
                    for field in AB_METRIC_FIELDS
                ):
                    fail(f"{path}: {scenario} A/B {label} metrics are incomplete or invalid")
            if baseline["messages"] != managed["messages"] or baseline["bytes"] != managed["bytes"]:
                fail(f"{path}: {scenario} A/B runs do not cover the same message and byte totals")
            if managed["bytes"] != scenario_observation.get("bytes"):
                fail(f"{path}: {scenario} managed A/B bytes do not match the qualified mailbox observation")
    summary = value.get("test_summary")
    if not isinstance(summary, dict) or not all(
        type(summary.get(field)) is int and summary[field] >= 0
        for field in SUMMARY_COUNTERS
    ):
        fail(f"{path}: test_summary counters are incomplete")
    confidence = results.get("verification_confidence")
    accepted = results.get("exceptions_accepted", 0)
    if type(accepted) is not int or accepted < 0 or accepted != 0:
        fail(f"{path}: passing evidence must not accept exceptions")
    mismatches = results.get("mismatches", {})
    if not isinstance(mismatches, dict):
        fail(f"{path}: mismatch summary must be an object")
    for field, count in mismatches.items():
        if field not in MISMATCH_FIELDS or type(count) is not int or count < 0:
            fail(f"{path}: mismatch summary contains an invalid counter")
        if field in {"missing", "extra", "changed"} and count != 0:
            fail(f"{path}: passing evidence must not contain unresolved mismatches")
    minimum_mailboxes = policy.get("minimum_mailboxes", 1)
    if (
        type(minimum_mailboxes) is not int
        or minimum_mailboxes < 0
        or summary["mailboxes_tested"] < minimum_mailboxes
    ):
        fail(f"{path}: mailboxes_tested does not meet the provider policy minimum")
    if phase == "dry_pilot":
        if confidence != "not_applicable":
            fail(f"{path}: dry_pilot confidence must be not_applicable")
        if any(summary[field] != 0 for field in (
            "messages_total", "bytes_total", "destination_messages_total", "destination_bytes_total"
        )):
            fail(f"{path}: dry_pilot must not claim transferred messages or bytes")
    else:
        minimums = {
            "mailboxes_tested": policy.get("minimum_mailboxes", 1),
            "messages_total": policy.get("minimum_messages", 1),
            "folders_total": policy.get("minimum_folders", 1),
            "bytes_total": policy.get("minimum_bytes", 1),
        }
        for field, minimum in minimums.items():
            if type(minimum) is not int or minimum < 0 or summary[field] < minimum:
                fail(f"{path}: {field} does not meet the provider policy minimum")
        for source_field, destination_field in (
            ("messages_total", "destination_messages_total"),
            ("bytes_total", "destination_bytes_total"),
            ("folders_total", "destination_folders_total"),
        ):
            if summary[source_field] != summary[destination_field]:
                fail(f"{path}: source and destination totals do not match")
        if confidence != "high":
            fail(f"{path}: live and recovery phases require high verification confidence")
        verified = results.get("messages_verified")
        if type(verified) is not int or verified < minimums["messages_total"]:
            fail(f"{path}: too few individually verified messages for provider policy")


def build_pack(evidence_dir: Path, source: str, destination: str, policy_path: Path) -> dict:
    pair = f"{source}->{destination}"
    policy = policy_row(policy_path, pair)
    records = []
    for path in sorted(evidence_dir.glob("*.json")):
        if path.name in {policy_path.name, "schema.json", "pack-schema.json"}:
            continue
        if path.is_symlink():
            fail(f"{path}: evidence records must not be symlinks")
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
        "fixture_id",
        "test_dataset_digest",
        "source_version",
        "destination_version",
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
                "fixture_id": value["fixture_id"],
                "source_version": value["source_version"],
                "destination_version": value["destination_version"],
                "scenario_ids": sorted(value["scenario_ids"]),
                "direct_imapsync_comparisons": [
                    {
                        "scenario_id": scenario,
                        "comparison": value["scenario_observations"][scenario.replace("-", "_")]["direct_imapsync_comparison"],
                    }
                    for scenario in sorted(AB_SCENARIOS & set(value["scenario_ids"]))
                ],
                "test_summary": {
                    key: item
                    for key, item in value["test_summary"].items()
                    if key in {
                        *SUMMARY_COUNTERS,
                        "maximum_message_bytes",
                        "large_messages_tested",
                        "unicode_folders_observed",
                        "unicode_folders_tested",
                        "special_use_folders_observed",
                        "special_folders_tested",
                        "mismatch_planted",
                        "mismatch_detected",
                    }
                },
                # Free-text known issues/workarounds can contain mailbox or
                # endpoint identifiers; only publish typed outcome counters.
                "results": {
                    **{
                        key: value["results"][key]
                        for key in RESULT_FIELDS
                        if key in value["results"] and key != "mismatches"
                    },
                    **(
                        {"mismatches": {
                            key: count
                            for key, count in value["results"].get("mismatches", {}).items()
                            if key in MISMATCH_FIELDS
                        }}
                        if "mismatches" in value["results"]
                        else {}
                    ),
                },
            }
        )
    tested_at = sorted(phase["tested_at"] for phase in phases)
    pack = {
        "format": "mailswiftsync-provider-qualification-pack",
        "schema_version": 2,
        "provider_pair": pair,
        # This summary is not itself qualification evidence. Promotion remains
        # gated by signed evidence and the authoritative release validator.
        "status": "qualification_candidate",
        "release_gate_required": True,
        "qualification_bundle_id": first["qualification_bundle_id"],
        "tested": {
            "source_provider": source,
            "destination_provider": destination,
            "source_version": first["source_version"],
            "destination_version": first["destination_version"],
            "fixture_id": first["fixture_id"],
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
        "generated_at": datetime.now(timezone.utc).isoformat(),
    }
    # Bind every exported claim, including the duplicated top-level summary,
    # to one canonical digest. Excluding only the digest avoids a recursive
    # definition while making edits to status, provenance, limitations, or
    # the generation timestamp detectable by downstream tooling.
    pack["evidence_digest"] = sha256(pack)
    return pack


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
        payload = (json.dumps(pack, indent=2, ensure_ascii=False) + "\n").encode("utf-8")
        if not output.parent.is_dir():
            fail(f"output directory does not exist: {output.parent}")
        descriptor, temporary_name = tempfile.mkstemp(
            prefix=f".{output.name}.", suffix=".tmp", dir=output.parent
        )
        temporary = Path(temporary_name)
        try:
            with os.fdopen(descriptor, "wb") as handle:
                handle.write(payload)
                handle.flush()
                os.fsync(handle.fileno())
            # Publish atomically without replacing an existing artifact or
            # following a pre-existing output symlink.
            os.link(temporary, output, follow_symlinks=False)
            directory_fd = os.open(output.parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)
        finally:
            temporary.unlink(missing_ok=True)
    except (OSError, ValueError) as error:
        print(f"Qualification pack refused: {error}", file=sys.stderr)
        return 1
    print(f"Qualification pack generated: {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
