#!/usr/bin/env python3
"""
Convert customer-proof JSON into provider evidence records for release gate validation.

Usage:
  generate-provider-evidence.py <customer-proof.json> <source-provider> <destination-provider> <testing-phase> \
    [--engine <name>] [--engine-version <version>] [--output <evidence.json>]

The generated evidence records link back to the customer-proof via digest reference
and aggregate test results into the format expected by verify-evidence-gate.sh.
"""

import json
import sys
import subprocess
import hashlib
from datetime import datetime, timezone
from pathlib import Path


def get_git_commit() -> str:
    """Get current git commit hash if available."""
    try:
        result = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            timeout=2
        )
        if result.returncode == 0:
            return result.stdout.strip()
    except (subprocess.TimeoutExpired, FileNotFoundError):
        pass
    return "unknown"


def compute_file_digest(file_path: str) -> str:
    """Compute SHA256 digest of a file."""
    try:
        sha256 = hashlib.sha256()
        with open(file_path, 'rb') as f:
            for chunk in iter(lambda: f.read(4096), b''):
                sha256.update(chunk)
        return sha256.hexdigest()
    except (OSError, IOError):
        return "unknown"


def generate_evidence(proof_path: str, source_provider: str, destination_provider: str,
                      phase: str, engine: str = "imapsync", engine_version: str = "2.314") -> dict:
    """Convert customer-proof into provider evidence record."""

    try:
        with open(proof_path, 'r', encoding='utf-8') as f:
            proof = json.load(f)
    except (OSError, json.JSONDecodeError) as e:
        raise ValueError(f"Failed to read customer-proof: {e}")

    if not isinstance(proof, dict):
        raise ValueError("Customer-proof must be a JSON object")

    # Verify proof structure
    if proof.get("format") != "mailswiftsync-customer-proof":
        raise ValueError("Invalid customer-proof format")

    # Extract version information
    app_version = proof.get("application_version", "unknown")

    # Aggregate results from mailboxes/runs
    total_messages = 0
    total_bytes = 0
    mailboxes_count = 0
    messages_verified = 0
    overall_result = "pass"

    mailboxes = proof.get("mailboxes", [])
    if mailboxes:
        mailboxes_count = len(mailboxes)

        for mailbox in mailboxes:
            if not isinstance(mailbox, dict):
                continue

            # Count total messages and bytes from mailbox metadata
            metadata = mailbox.get("metadata", {})
            if isinstance(metadata, dict):
                total_messages += metadata.get("total_messages_source", 0)
                total_bytes += metadata.get("total_bytes_source", 0)

            # Extract results if present
            evidence = mailbox.get("evidence")
            if evidence and isinstance(evidence, dict):
                messages_verified += evidence.get("messages_verified", 0)

    # Check runs for overall result
    runs = proof.get("runs", [])
    if runs:
        for run in runs:
            if not isinstance(run, dict):
                continue

            # Check if any run failed
            result = run.get("result")
            if result == "fail":
                overall_result = "fail"
                break
            elif result == "pass_with_exceptions":
                if overall_result == "pass":
                    overall_result = "pass_with_exceptions"

    # Generate evidence record
    now = datetime.now(timezone.utc).isoformat()
    proof_digest = compute_file_digest(proof_path)
    git_commit = get_git_commit()

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
        },
        "results": {
            "overall_result": overall_result,
            "messages_verified": messages_verified,
            "verification_confidence": "high" if messages_verified > 0 else "medium" if total_messages > 0 else "low",
        },
        "tester": {
            "name": "MailSwiftSync Provider Test",
            "email": "tests@mailswiftsync.local",
            "organization": "Test Harness",
        },
        "notes": f"Generated from customer-proof: {Path(proof_path).name}",
        "source_provider": source_provider,
        "destination_provider": destination_provider,
        "source_auth_method": project.get("source_auth_method", "password"),
        "destination_auth_method": project.get("destination_auth_method", "password"),
        "mailswiftsync_commit": git_commit,
        "run_id": proof.get("runs", [{}])[0].get("run_id", "unknown") if proof.get("runs") else "unknown",
        "proof_digest": proof_digest,
        "fixture_id": project.get("fixture_id", "provider-test"),
        "test_dataset_digest": project.get("dataset_digest", "unknown"),
    }

    return evidence


def main():
    if len(sys.argv) < 5:
        print(
            "Usage: generate-provider-evidence.py <customer-proof.json> <source-provider> <destination-provider> <testing-phase> "
            "[--engine <name>] [--engine-version <version>] [--output <evidence.json>]",
            file=sys.stderr,
        )
        sys.exit(2)

    proof_path = sys.argv[1]
    source_provider = sys.argv[2]
    destination_provider = sys.argv[3]
    phase = sys.argv[4]
    output_path = None
    engine = "imapsync"
    engine_version = "2.314"

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
        else:
            i += 1

    try:
        evidence = generate_evidence(
            proof_path, source_provider, destination_provider, phase, engine, engine_version
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
