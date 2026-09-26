#!/usr/bin/env python3
"""Reject current documentation claims that contradict capabilities.toml and code."""

from pathlib import Path
import re
import sys
import tomllib


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "capabilities.toml"
CORE_RS = ROOT / "src" / "core.rs"

# These patterns identify affirmative readiness claims, not references to the
# release gate or statements that production readiness is still outstanding.
AFFIRMATIVE_CLAIMS = (
    re.compile(r"^\s*#{1,6}\s*(?:✅\s*)?PRODUCTION[- ]READY\b", re.I),
    re.compile(r"\bstatus\s*:\s*(?:✅\s*)?production[- ]ready\b", re.I),
    re.compile(r"\ball major work complete\b", re.I),
    re.compile(r"\baccounts? with\s+1\s*(?:m|million)\+?\s+messages?\s+(?:are\s+)?supported\b", re.I),
)

# Schema version checks
SCHEMA_VERSION_PATTERN = re.compile(r"(?:SQLite\s+)?schema\s+v?(\d+)", re.I)

# UI capability pairing checks: if TOML says ui = "not_wired", docs shouldn't claim GUI support
UI_CAPABILITY_CLAIMS = {
    "provider_runbooks": r"(?:provider[- ]specific\s+)?runbook\s+(?:in\s+)?(?:GUI|dashboard|interface)",
    "recovery_guidance": r"(?:recovery|maintenance)\s+(?:window\s+)?(?:guidance|dashboard)\s+(?:in\s+)?(?:GUI|dashboard|interface)",
}


def get_schema_version() -> int:
    """Extract CURRENT_SCHEMA_VERSION from src/core.rs."""
    if not CORE_RS.exists():
        return 0
    content = CORE_RS.read_text(encoding="utf-8")
    match = re.search(r"pub\s+const\s+CURRENT_SCHEMA_VERSION\s*:\s*i64\s*=\s*(\d+)", content)
    return int(match.group(1)) if match else 0


def main() -> int:
    with MANIFEST.open("rb") as stream:
        manifest = tomllib.load(stream)

    schema_version = get_schema_version()
    unsupported = [
        name
        for name, capability in manifest.get("capabilities", {}).items()
        if capability.get("production_supported") is False
    ]

    violations = []

    for path in sorted(ROOT.rglob("*.md")):
        relative = path.relative_to(ROOT)
        if relative.parts[:2] == ("docs", "history"):
            continue

        try:
            content = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue

        for line_number, line in enumerate(content.splitlines(), 1):
            # Check for production-readiness claims on unsupported capabilities
            if unsupported and any(pattern.search(line) for pattern in AFFIRMATIVE_CLAIMS):
                violations.append(f"{relative}:{line_number}: PRODUCTION-READINESS CLAIM: {line.strip()}")

            # Check for schema version mismatches
            schema_match = SCHEMA_VERSION_PATTERN.search(line)
            if schema_match:
                claimed_version = int(schema_match.group(1))
                if claimed_version != schema_version:
                    violations.append(
                        f"{relative}:{line_number}: SCHEMA VERSION MISMATCH: "
                        f"docs claim v{claimed_version} but code has v{schema_version}: {line.strip()}"
                    )

            # Check for UI capability claims that contradict capabilities.toml
            for capability_name, pattern in UI_CAPABILITY_CLAIMS.items():
                capability = manifest.get("capabilities", {}).get(capability_name, {})
                if capability.get("ui") == "not_wired":
                    if re.search(pattern, line, re.I):
                        violations.append(
                            f"{relative}:{line_number}: UI CAPABILITY MISMATCH: "
                            f"{capability_name} UI is 'not_wired' in capabilities.toml: {line.strip()}"
                        )

    if violations:
        if unsupported:
            print("Current documentation makes production-readiness claims while these capabilities remain unsupported:")
            print("  " + ", ".join(unsupported))
            print()
        print("Documentation drift detected. Fix these issues:")
        for v in violations:
            print(v)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
