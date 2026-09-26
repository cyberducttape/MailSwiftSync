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
    re.compile(r"^\s*(?:✅\s*)?PRODUCTION[- ]READY\b", re.I),
    re.compile(r"\bstatus\b\s*(?::|\s)\s*(?:✅\s*)?production[- ]ready\b", re.I),
    re.compile(r"\b(?:is|are|was|were|becomes?|now)\s+(?:fully\s+)?production[- ]ready\b", re.I),
    re.compile(r"\bfully\s+production[- ]ready\b", re.I),
    re.compile(r"\bready\s+for\s+production\b", re.I),
    re.compile(r"\bGA[- ]ready\b", re.I),
    re.compile(r"\bproduction\s+supported\b", re.I),
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


def strip_markup(text: str) -> str:
    """Remove presentation markup while preserving prose for claim matching."""
    # Keep link/image labels, but discard their destinations.
    text = re.sub(r"!?(?:\[([^\]]+)\])\([^)]*\)", r"\1", text)
    # HTML tags can split a claim (for example, <strong>production-ready</strong>).
    text = re.sub(r"<[^>]*>", " ", text)
    # Markdown headings, blockquotes, table separators, and emphasis/code marks
    # do not change the semantic wording being checked.
    text = re.sub(r"^\s{0,3}#{1,6}\s*", "", text)
    text = re.sub(r"^\s{0,3}>\s?", "", text)
    text = text.replace("|", " ")
    text = re.sub(r"[*_`~]", "", text)
    return re.sub(r"\s+", " ", text).strip()


def contains_affirmative_claim(line: str) -> bool:
    """Return whether a rendered Markdown line makes an unsupported claim."""
    normalized = strip_markup(line)
    return any(pattern.search(normalized) for pattern in AFFIRMATIVE_CLAIMS)


def find_status_section_violations(content: str, manifest: dict) -> list[str]:
    """Reject manifest-marked experimental terms duplicated in stable status."""
    sections = re.split(r"(?m)^Experimental or planned:\s*$", content, maxsplit=1)
    if len(sections) != 2 or "Stable today:" not in sections[0]:
        return []
    stable = sections[0].split("Stable today:", 1)[1].casefold()
    experimental = sections[1].casefold()
    violations = []
    for term in manifest.get("documentation", {}).get("experimental_only_terms", []):
        normalized = str(term).casefold()
        if normalized in stable and normalized in experimental:
            violations.append(
                "README status contradiction: manifest-marked experimental term "
                f"{term!r} appears in both Stable today and Experimental or planned"
            )
    return violations


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

    readme = ROOT / "README.md"
    if readme.exists():
        violations.extend(
            f"README.md: {violation}"
            for violation in find_status_section_violations(
                readme.read_text(encoding="utf-8"), manifest
            )
        )

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
            if unsupported and contains_affirmative_claim(line):
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
