#!/usr/bin/env python3
"""Reject current documentation claims that contradict capabilities.toml."""

from pathlib import Path
import re
import sys
import tomllib


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "capabilities.toml"

# These patterns identify affirmative readiness claims, not references to the
# release gate or statements that production readiness is still outstanding.
AFFIRMATIVE_CLAIMS = (
    re.compile(r"^\s*#{1,6}\s*(?:✅\s*)?PRODUCTION[- ]READY\b", re.I),
    re.compile(r"\bstatus\s*:\s*(?:✅\s*)?production[- ]ready\b", re.I),
    re.compile(r"\ball major work complete\b", re.I),
    re.compile(r"\baccounts? with\s+1\s*(?:m|million)\+?\s+messages?\s+(?:are\s+)?supported\b", re.I),
)


def main() -> int:
    with MANIFEST.open("rb") as stream:
        manifest = tomllib.load(stream)
    unsupported = [
        name
        for name, capability in manifest.get("capabilities", {}).items()
        if capability.get("production_supported") is False
    ]
    if not unsupported:
        return 0

    violations = []
    for path in sorted(ROOT.rglob("*.md")):
        relative = path.relative_to(ROOT)
        if relative.parts[:2] == ("docs", "history"):
            continue
        for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            if any(pattern.search(line) for pattern in AFFIRMATIVE_CLAIMS):
                violations.append(f"{relative}:{line_number}: {line.strip()}")

    if violations:
        print("Current documentation makes production-readiness claims while these capabilities remain unsupported:")
        print("  " + ", ".join(unsupported))
        print("Remove the claim or update capabilities.toml only when the capability is actually qualified.")
        print("\n".join(violations))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
