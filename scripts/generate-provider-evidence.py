#!/usr/bin/env python3
"""
Convert customer-proof JSON into provider evidence records for release gate validation.

Usage:
  generate-provider-evidence.py <customer-proof.json> <source-provider> <destination-provider> <testing-phase> \
    --run-id <id> --engine-version <version> --mailswiftsync-version <version> \
    --mailswiftsync-commit <sha> --mailswiftsync-binary-sha256 <sha256> \
    --imapsync-binary-sha256 <sha256> [--engine <name>] [--output <evidence.json>]

The generated evidence records link back to the customer-proof via digest reference
and aggregate test results into the format expected by verify-evidence-gate.sh.
"""

import json
import sys
import hashlib
from datetime import datetime, timezone
from pathlib import Path

SUPPORTED_ENGINE_VERSIONS = {"imapsync": {"2.314"}}

def canonical_proof_digest(proof: dict) -> str:
    """Reproduce reports::integrity::with_proof_digest canonicalization."""
    unsigned = dict(proof)
    unsigned.pop("proof_digest", None)
    unsigned.pop("proof_signature", None)
    # serde_json's default Map representation is ordered, so mirror its
    # canonical object-key ordering rather than relying on input insertion
    # order from the proof file.
    canonical = json.dumps(unsigned, separators=(",", ":"), ensure_ascii=False, sort_keys=True)
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def generate_evidence(proof_path: str, source_provider: str, destination_provider: str,
                      phase: str, engine: str = "imapsync", engine_version: str | None = None,
                      run_id: str | None = None, mailswiftsync_version: str | None = None,
                      mailswiftsync_commit: str | None = None,
                      mailswiftsync_binary_sha256: str | None = None,
                      imapsync_binary_sha256: str | None = None) -> dict:
    """Convert customer-proof into provider evidence record."""

    try:
        with open(proof_path, 'r', encoding='utf-8') as f:
            proof = json.load(f)
    except (OSError, json.JSONDecodeError) as e:
        raise ValueError(f"Failed to read customer-proof: {e}")

    if not isinstance(proof, dict):
        raise ValueError("Customer-proof must be a JSON object")
    identity = {
        "engine_version": engine_version,
        "mailswiftsync_version": mailswiftsync_version,
        "mailswiftsync_commit": mailswiftsync_commit,
        "mailswiftsync_binary_sha256": mailswiftsync_binary_sha256,
        "imapsync_binary_sha256": imapsync_binary_sha256,
    }
    if any(not isinstance(value, str) or not value for value in identity.values()):
        raise ValueError("evidence requires identity captured from the executed binaries")
    if engine in SUPPORTED_ENGINE_VERSIONS and engine_version not in SUPPORTED_ENGINE_VERSIONS[engine]:
        raise ValueError(f"unsupported {engine} version for provider evidence: {engine_version}")

    # Verify proof structure
    if proof.get("format") != "mailswiftsync-customer-proof":
        raise ValueError("Invalid customer-proof format")

    expected_digest = proof.get("proof_digest")
    actual_digest = canonical_proof_digest(proof)
    if not isinstance(expected_digest, str) or expected_digest != actual_digest:
        raise ValueError("customer-proof canonical proof_digest is missing or invalid")
    claim = proof.get("completion_claim")
    if not isinstance(claim, dict):
        raise ValueError("customer-proof is missing completion_claim")

    runs = proof.get("runs", [])
    if not isinstance(runs, list) or not runs:
        raise ValueError("customer-proof must contain run records")
    selected_runs = [run for run in runs if isinstance(run, dict) and run.get("run_id") == run_id]
    if run_id is None or len(selected_runs) != 1:
        raise ValueError("evidence generation requires exactly one selected run_id")
    selected_run = selected_runs[0]
    project_id = proof.get("project", {}).get("project_id")
    if not isinstance(project_id, str) or not project_id:
        raise ValueError("customer-proof is missing project.project_id")
    provider_identity = proof.get("provider_identity")
    required_identity = (
        "source_provider", "destination_provider", "source_auth_method",
        "destination_auth_method", "fixture_id",
    )
    if not isinstance(provider_identity, dict) or any(
        not isinstance(provider_identity.get(field), str) or not provider_identity[field]
        for field in required_identity
    ):
        raise ValueError("customer-proof is missing signed provider identity")
    if provider_identity["source_provider"] != source_provider:
        raise ValueError("source provider does not match the customer-proof identity")
    if provider_identity["destination_provider"] != destination_provider:
        raise ValueError("destination provider does not match the customer-proof identity")
    if not isinstance(provider_identity.get("scenario_ids"), list) or not all(
        isinstance(value, str) and value for value in provider_identity["scenario_ids"]
    ):
        raise ValueError("customer-proof provider identity has invalid scenario IDs")
    if selected_run.get("project_id") != project_id:
        raise ValueError("selected run does not belong to the proof project")
    mailbox_ids = {
        mailbox.get("job_id") for mailbox in proof.get("mailboxes", [])
        if isinstance(mailbox, dict) and isinstance(mailbox.get("job_id"), str)
    }
    if selected_run.get("job_id") is not None and selected_run.get("job_id") not in mailbox_ids:
        raise ValueError("selected run does not belong to the proof mailbox set")
    if selected_run.get("status") != "completed" or not selected_run.get("started_at") or not selected_run.get("finished_at"):
        raise ValueError("selected evidence run is not a completed, timestamped run")
    if phase == "dry_pilot":
        if selected_run.get("phase_at_start") != "preflight" or claim.get("status") != "incomplete":
            raise ValueError("dry_pilot evidence must reference a completed preflight run")
    elif claim.get("status") != "durably_complete":
        raise ValueError("live/recovery evidence requires a durable completion certificate")

    # Aggregate the actual customer-proof schema. A qualification record may
    # only be generated from strict verified mailbox evidence and completed
    # runs; missing fields are errors, never defaults.
    total_messages = 0
    total_bytes = 0
    destination_messages = 0
    destination_bytes = 0
    total_folders = 0
    destination_folders = 0
    mailboxes_count = len(proof.get("mailboxes", []))
    messages_covered_by_aggregate_evidence = 0
    if mailboxes_count == 0:
        raise ValueError("customer-proof contains no mailboxes")
    for mailbox in proof["mailboxes"]:
        if phase == "dry_pilot":
            continue
        if not isinstance(mailbox, dict) or mailbox.get("state") != "verified":
            raise ValueError("customer-proof contains a mailbox that is not strictly verified")
        evidence = mailbox.get("evidence")
        required = ("scope", "evidence_level", "source_messages", "destination_messages",
                    "source_bytes", "destination_bytes", "source_folders", "destination_folders",
                    "unmatched_messages", "failed_messages")
        if not isinstance(evidence, dict) or any(field not in evidence for field in required):
            raise ValueError("customer-proof mailbox evidence is incomplete")
        if evidence["scope"] != "engine-confirmed" or evidence["evidence_level"] != "Engine-confirmed exact match":
            raise ValueError("customer-proof mailbox evidence is not engine-confirmed exact")
        if any(evidence[field] != 0 for field in ("unmatched_messages", "failed_messages")):
            raise ValueError("customer-proof contains unresolved mailbox evidence")
        if evidence["source_messages"] <= 0 or evidence["source_bytes"] <= 0:
            raise ValueError("customer-proof qualification requires non-empty mailbox evidence")
        if evidence["source_messages"] != evidence["destination_messages"] or evidence["source_bytes"] != evidence["destination_bytes"]:
            raise ValueError("customer-proof aggregate evidence is not balanced")
        total_messages += evidence["source_messages"]
        destination_messages += evidence["destination_messages"]
        total_bytes += evidence["source_bytes"]
        destination_bytes += evidence["destination_bytes"]
        total_folders += evidence["source_folders"]
        destination_folders += evidence["destination_folders"]
        messages_covered_by_aggregate_evidence += evidence["source_messages"]

    run_statuses = {
        run.get("status") for run in runs if isinstance(run, dict)
    }
    if phase == "recovery_test":
        abandoned_predecessors = [
            run for run in runs
            if isinstance(run, dict)
            and run.get("status") == "abandoned"
            and run.get("project_id") == selected_run.get("project_id")
            and run.get("job_id") == selected_run.get("job_id")
            and run.get("started_at", "") < selected_run.get("started_at", "")
        ]
        if not selected_run.get("job_id") or not abandoned_predecessors:
            raise ValueError("recovery evidence requires an abandoned predecessor for the same project and job")
        if not run_statuses.issubset({"completed", "abandoned"}):
            raise ValueError("recovery evidence requires a completed run and a resolved abandoned ancestor")
    elif phase != "dry_pilot":
        terminal_statuses = {"completed", "failed", "abandoned", "cancelled", "verification_failed"}
        if any(
            not isinstance(run, dict) or run.get("status") not in terminal_statuses
            for run in runs
        ):
            raise ValueError("live customer-proof contains unresolved queued or running history")
    if phase == "dry_pilot":
        total_messages = destination_messages = 0
        total_bytes = destination_bytes = 0
        total_folders = destination_folders = 0
        messages_covered_by_aggregate_evidence = 0

    # Generate evidence record
    now = datetime.now(timezone.utc).isoformat()
    project = proof.get("project", {})

    evidence = {
        "provider": f"{source_provider}->{destination_provider}",
        "tested_at": now,
        "testing_phase": phase,
        "source_version": project.get("source_version", "unknown"),
        "destination_version": project.get("destination_version", "unknown"),
        "engine": engine,
        "engine_version": engine_version,
        "test_summary": {
            "mailboxes_tested": mailboxes_count,
            "messages_total": total_messages,
            "bytes_total": total_bytes,
            "destination_messages_total": destination_messages,
            "destination_bytes_total": destination_bytes,
            "folders_total": total_folders,
            "destination_folders_total": destination_folders,
        },
        "results": {
            "overall_result": "pass" if phase == "dry_pilot" else "pass_with_exceptions",
            "messages_covered_by_aggregate_evidence": messages_covered_by_aggregate_evidence,
            "verification_confidence": "not_applicable" if phase == "dry_pilot" else "aggregate_only",
        },
        "tester": {
            "name": "MailSwiftSync Provider Test",
            "email": "tests@mailswiftsync.local",
            "organization": "Test Harness",
        },
        "notes": (
            f"Generated from customer-proof: {Path(proof_path).name}; "
            "live/recovery records contain aggregate engine evidence only until "
            "independent message-level reconciliation is wired into the run."
        ),
        "source_provider": source_provider,
        "destination_provider": destination_provider,
        "source_auth_method": provider_identity["source_auth_method"],
        "destination_auth_method": provider_identity["destination_auth_method"],
        "mailswiftsync_version": mailswiftsync_version,
        "mailswiftsync_commit": mailswiftsync_commit,
        "mailswiftsync_binary_sha256": mailswiftsync_binary_sha256,
        "imapsync_binary_sha256": imapsync_binary_sha256,
        "run_id": selected_run["run_id"],
        "selected_run_id": selected_run["run_id"],
        "run_type": "preflight" if phase == "dry_pilot" else "recovery" if phase == "recovery_test" else "live",
        "run_started_at": selected_run["started_at"],
        "run_finished_at": selected_run["finished_at"],
        "proof_digest": expected_digest,
        "proof_verification": "canonical_digest_verified",
        "proof_file": Path(proof_path).name,
        "fixture_id": provider_identity["fixture_id"],
        "scenario_ids": provider_identity["scenario_ids"],
        "test_dataset_digest": project.get("dataset_digest", "unknown"),
    }

    return evidence


def main():
    if len(sys.argv) < 5:
        print(
            "Usage: generate-provider-evidence.py <customer-proof.json> <source-provider> <destination-provider> <testing-phase> "
            "--run-id <id> --engine-version <version> --mailswiftsync-version <version> "
            "--mailswiftsync-commit <sha> --mailswiftsync-binary-sha256 <sha256> "
            "--imapsync-binary-sha256 <sha256> [--engine <name>] [--output <evidence.json>]",
            file=sys.stderr,
        )
        sys.exit(2)

    proof_path = sys.argv[1]
    source_provider = sys.argv[2]
    destination_provider = sys.argv[3]
    phase = sys.argv[4]
    output_path = None
    engine = "imapsync"
    engine_version = None
    run_id = None
    mailswiftsync_version = None
    mailswiftsync_commit = None
    mailswiftsync_binary_sha256 = None
    imapsync_binary_sha256 = None

    # Parse optional arguments
    i = 5
    while i < len(sys.argv):
        if sys.argv[i] == "--output" and i + 1 < len(sys.argv):
            output_path = sys.argv[i + 1]
            i += 2
        elif sys.argv[i] == "--engine" and i + 1 < len(sys.argv):
            engine = sys.argv[i + 1]
            i += 2
        elif sys.argv[i] == "--engine-version" and i + 1 < len(sys.argv):
            engine_version = sys.argv[i + 1]
            i += 2
        elif sys.argv[i] == "--run-id" and i + 1 < len(sys.argv):
            run_id = sys.argv[i + 1]
            i += 2
        elif sys.argv[i] == "--mailswiftsync-version" and i + 1 < len(sys.argv):
            mailswiftsync_version = sys.argv[i + 1]
            i += 2
        elif sys.argv[i] == "--mailswiftsync-commit" and i + 1 < len(sys.argv):
            mailswiftsync_commit = sys.argv[i + 1]
            i += 2
        elif sys.argv[i] == "--mailswiftsync-binary-sha256" and i + 1 < len(sys.argv):
            mailswiftsync_binary_sha256 = sys.argv[i + 1]
            i += 2
        elif sys.argv[i] == "--imapsync-binary-sha256" and i + 1 < len(sys.argv):
            imapsync_binary_sha256 = sys.argv[i + 1]
            i += 2
        else:
            i += 1

    try:
        evidence = generate_evidence(
            proof_path, source_provider, destination_provider, phase, engine, engine_version,
            run_id, mailswiftsync_version, mailswiftsync_commit,
            mailswiftsync_binary_sha256, imapsync_binary_sha256
        )
    except ValueError as e:
        print(f"Error: {e}", file=sys.stderr)
        sys.exit(1)

    # Output result
    output_json = json.dumps(evidence, indent=2)

    if output_path:
        try:
            with open(output_path, 'w', encoding='utf-8') as f:
                f.write(output_json)
            print(f"Evidence generated: {output_path}")
        except OSError as e:
            print(f"Error writing output: {e}", file=sys.stderr)
            sys.exit(1)
    else:
        print(output_json)


if __name__ == "__main__":
    main()
