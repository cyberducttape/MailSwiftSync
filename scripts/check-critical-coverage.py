#!/usr/bin/env python3
"""Enforce line and branch coverage floors for safety-critical modules.

The project deliberately does not use a vanity whole-repository percentage.
This gate protects the modules where an untested branch can lose destination
state, misreport evidence, or bypass durable recovery invariants.
"""

from pathlib import Path
import argparse
import sys


ROOT = Path(__file__).resolve().parents[1]
CRITICAL_FILES = {
    "src/controller/batch_admission.rs": 85.0,
    "src/controller/batch_work_item.rs": 65.0,
    "src/controller/batch_scheduler.rs": 85.0,
    "src/controller/batch_start.rs": 80.0,
    "src/controller/events.rs": 75.0,
    "src/controller/failure.rs": 85.0,
    "src/controller/poll.rs": 75.0,
    "src/controller/queue.rs": 85.0,
    "src/controller/rate_domains.rs": 90.0,
    "src/core/database.rs": 80.0,
    "src/core/database/schema.rs": 85.0,
    "src/core/evidence.rs": 90.0,
    "src/core/message_verification.rs": 85.0,
    "src/core/provider_intelligence.rs": 85.0,
    "src/core/recovery.rs": 80.0,
    "src/core/recovery_queue.rs": 90.0,
    "src/core/state.rs": 95.0,
    "src/extra_options.rs": 80.0,
    "src/imap_probe/auth.rs": 85.0,
    "src/migrate_audit.rs": 80.0,
    "src/migration_plan.rs": 75.0,
    "src/oauth_authorize.rs": 85.0,
    "src/oauth_refresh.rs": 70.0,
    "src/oauth_redirect.rs": 85.0,
    "src/process.rs": 85.0,
    "src/verification.rs": 95.0,
}

# Branch floors focus on safety-sensitive decision logic. They are deliberately
# per-module and conservative; they are guardrails for regression, not proofs of
# correctness or a project-wide percentage target.
CRITICAL_BRANCHES = {
    "src/controller/batch_admission.rs": 60.0,
    "src/controller/batch_scheduler.rs": 60.0,
    "src/controller/batch_start.rs": 45.0,
    "src/controller/batch_work_item.rs": 40.0,
    "src/controller/failure.rs": 75.0,
    "src/controller/poll.rs": 45.0,
    "src/controller/rate_domains.rs": 70.0,
    "src/core/database/schema.rs": 70.0,
    "src/core/evidence.rs": 70.0,
    "src/core/message_verification.rs": 65.0,
    "src/core/provider_intelligence.rs": 65.0,
    "src/core/recovery.rs": 60.0,
    "src/extra_options.rs": 70.0,
    "src/imap_probe/auth.rs": 65.0,
    "src/migrate_audit.rs": 75.0,
    "src/migration_plan.rs": 60.0,
    "src/oauth_authorize.rs": 60.0,
    "src/oauth_refresh.rs": 45.0,
    "src/oauth_redirect.rs": 65.0,
    "src/process.rs": 60.0,
    "src/verification.rs": 70.0,
}


def parse_lcov(path: Path) -> dict[str, tuple[int, int, int, int]]:
    coverage = {}
    current = None
    lf = lh = None
    brf = brh = None
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        if raw_line.startswith("SF:"):
            current = raw_line[3:]
            lf = lh = None
            brf = brh = None
        elif raw_line.startswith("LF:"):
            lf = int(raw_line[3:])
        elif raw_line.startswith("LH:"):
            lh = int(raw_line[3:])
        elif raw_line.startswith("BRF:"):
            brf = int(raw_line[4:])
        elif raw_line.startswith("BRH:"):
            brh = int(raw_line[4:])
        elif raw_line == "end_of_record" and current is not None:
            if lf is not None and lh is not None:
                normalized = current.replace("\\", "/")
                marker = "src/"
                if marker in normalized:
                    normalized = normalized[normalized.index(marker) :]
                coverage[normalized] = (lh, lf, brh or 0, brf or 0)
            current = None
    return coverage


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("lcov", type=Path, help="stable-toolchain line coverage LCOV")
    parser.add_argument(
        "--branch-lcov",
        type=Path,
        required=True,
        help="nightly LLVM branch coverage LCOV",
    )
    args = parser.parse_args()
    coverage = parse_lcov(args.lcov)
    branch_coverage = parse_lcov(args.branch_lcov)
    failed = False
    for filename, minimum in CRITICAL_FILES.items():
        if filename not in coverage:
            print(f"FAIL {filename}: no LCOV record", file=sys.stderr)
            failed = True
            continue
        hit, lines, _, _ = coverage[filename]
        percent = 100.0 if lines == 0 else 100.0 * hit / lines
        status = "PASS" if percent >= minimum else "FAIL"
        print(f"{status} {filename}: {percent:.2f}% (required {minimum:.2f}%)")
        failed |= percent < minimum
    for filename, minimum in CRITICAL_BRANCHES.items():
        if filename not in branch_coverage:
            print(f"FAIL {filename}: no LCOV record for branch coverage", file=sys.stderr)
            failed = True
            continue
        _, _, hit, branches = branch_coverage[filename]
        if branches == 0:
            print(f"FAIL {filename}: no branch data in LCOV report", file=sys.stderr)
            failed = True
            continue
        percent = 100.0 * hit / branches
        status = "PASS" if percent >= minimum else "FAIL"
        print(f"{status} {filename} branches: {percent:.2f}% (required {minimum:.2f}%)")
        failed |= percent < minimum
    if failed:
        print(
            "Critical-module coverage is below its safety gate; add or repair "
            "focused tests before merging.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
