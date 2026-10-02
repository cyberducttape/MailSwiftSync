#!/usr/bin/env python3
"""Reject current documentation claims that contradict capabilities.toml and code."""

from pathlib import Path
import re
import sys
import tomllib


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "capabilities.toml"
CORE_RS = ROOT / "src" / "core.rs"
CAPABILITY_MANIFEST_MD = ROOT / "CAPABILITY_MANIFEST.md"
README_MD = ROOT / "README.md"
EVIDENCE_RS = ROOT / "src" / "core" / "evidence.rs"
TABLE_BEGIN = "<!-- capabilities:begin -->"
TABLE_END = "<!-- capabilities:end -->"
LEVELS_BEGIN = "<!-- verification-levels:begin -->"
LEVELS_END = "<!-- verification-levels:end -->"
TABLE_FIELDS = (
    "code",
    "controller",
    "ui",
    "generic_lab",
    "gmail_live",
    "m365_live",
    "production_supported",
)
CODE_PRESENT = {"implemented", "available", "bounded_opt_in"}

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
    """Reject manifest-marked experimental terms duplicated in component status."""
    sections = re.split(r"(?m)^Experimental or planned:\s*$", content, maxsplit=1)
    status_heading = "Stable components (implementation status; not a production-support claim):"
    if len(sections) != 2 or status_heading not in sections[0]:
        return []
    stable = sections[0].split(status_heading, 1)[1].casefold()
    experimental = sections[1].casefold()
    violations = []
    for term in manifest.get("documentation", {}).get("experimental_only_terms", []):
        normalized = str(term).casefold()
        if normalized in stable and normalized in experimental:
            violations.append(
                "README status contradiction: manifest-marked experimental term "
                f"{term!r} appears in both stable components and Experimental or planned"
            )
    return violations


def render_status_table(manifest: dict) -> str:
    """Render every capability field verbatim as the manifest's status table."""
    def cell(value) -> str:
        if isinstance(value, bool):
            return "yes" if value else "no"
        return str(value)

    lines = [
        "| Capability | " + " | ".join(TABLE_FIELDS) + " |",
        "|" + "---|" * (len(TABLE_FIELDS) + 1),
    ]
    for name, capability in sorted(manifest.get("capabilities", {}).items()):
        cells = [cell(capability.get(field, "")) for field in TABLE_FIELDS]
        lines.append(f"| `{name}` | " + " | ".join(cells) + " |")
    return "\n".join(lines)


def replace_status_table(content: str, manifest: dict) -> str | None:
    """Return the manifest with a regenerated status block, or None if absent."""
    if content.count(TABLE_BEGIN) != 1 or content.count(TABLE_END) != 1:
        return None
    head, rest = content.split(TABLE_BEGIN, 1)
    _, tail = rest.split(TABLE_END, 1)
    return f"{head}{TABLE_BEGIN}\n{render_status_table(manifest)}\n{TABLE_END}{tail}"


def manifest_table_rows(content: str) -> dict[str, list[str]]:
    """Map each Markdown table row's capability name to its cells."""
    rows = {}
    for line in content.splitlines():
        if not line.startswith("|") or re.fullmatch(r"[|\s:-]+", line):
            continue
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        bold = re.match(r"\*\*(.+?)\*\*", cells[0])
        name = bold.group(1) if bold else strip_markup(cells[0])
        rows.setdefault(name, cells)
    return rows


def expected_wired(capability: dict) -> str:
    """Derive the human Wired column contract from controller/ui fields."""
    controller = capability.get("controller")
    ui = capability.get("ui")
    if controller == "wired" and ui in ("wired", "not_claimed"):
        return "yes"
    if controller in ("wired", "partial") and ui in ("wired", "partial", "not_claimed"):
        return "partial"
    return "no"


def find_manifest_drift(content: str, manifest: dict) -> list[str]:
    """Check the generated table and every manifest row linked to a capability."""
    violations = []
    regenerated = replace_status_table(content, manifest)
    if regenerated is None:
        violations.append(
            f"status table markers {TABLE_BEGIN} / {TABLE_END} must each appear exactly once"
        )
    elif regenerated != content:
        violations.append(
            "status table is stale; run python3 scripts/verify-capability-claims.py --write"
        )

    rows = manifest_table_rows(content)
    for name, capability in sorted(manifest.get("capabilities", {}).items()):
        for row_name in capability.get("manifest_rows", []):
            cells = rows.get(row_name)
            if cells is None or len(cells) < 5:
                violations.append(f"{name}: manifest row {row_name!r} not found")
                continue
            code, wired, live = cells[1].casefold(), cells[2].casefold(), cells[4].casefold()
            code_expected = "yes" if capability.get("code") in CODE_PRESENT else "no"
            if code != code_expected:
                violations.append(
                    f"{row_name}: Code is {cells[1]!r} but capabilities.toml "
                    f"{name}.code = {capability.get('code')!r} (expected {code_expected!r})"
                )
            wired_expected = expected_wired(capability)
            wired_ok = {
                "yes": wired.startswith("yes"),
                "partial": wired.startswith("yes") or wired.startswith("partial"),
                "no": not wired.startswith("yes") and not wired.startswith("partial"),
            }[wired_expected]
            if not wired_ok:
                violations.append(
                    f"{row_name}: Wired is {cells[2]!r} but capabilities.toml {name} has "
                    f"controller={capability.get('controller')!r}, ui={capability.get('ui')!r} "
                    f"(expected {wired_expected!r})"
                )
            if live.startswith("yes") and not (
                capability.get("gmail_live") or capability.get("m365_live")
            ):
                violations.append(
                    f"{row_name}: Live Provider is {cells[4]!r} but capabilities.toml "
                    f"{name} records no live provider validation"
                )
    return violations


# Evidence-tier vocabulary. Any numbered level in current prose, Rust labels,
# or locale strings must be one capabilities.toml defines.
LEVEL_NUMBER = re.compile(r"\b(?:Level|Stufe)\s+(\d+)", re.I)
LEVEL_NAMED = re.compile(r"\bLevel\s+(\d+)\s+[—–]\s+([^—–|\n]+)")
LEVEL_COUNT = re.compile(
    r"\b(one|two|three|four|five|six|seven|\d+)\s+(?:verification|evidence)\s+levels\b", re.I
)
COUNT_WORDS = {"one": 1, "two": 2, "three": 3, "four": 4, "five": 5, "six": 6, "seven": 7}
RUST_LEVEL_LITERAL = re.compile(r'"Level (\d+) — ([^"]*)"')
RUST_LEVEL_ARM = re.compile(
    r"((?:VerificationMethod::\w+\s*\|?\s*)*VerificationMethod::\w+)\s*=>\s*\{?\s*\"Level (\d+) — "
)
LOCALE_LEVEL_KEY = re.compile(r"\blevel-(\d+)\b")


def verification_levels(manifest: dict) -> dict[int, dict]:
    return {int(level["level"]): level for level in manifest.get("verification_levels", [])}


def render_level_table(manifest: dict) -> str:
    """Render the canonical evidence-tier table for README."""
    lines = ["| Level | Compares | Limitation |", "|---|---|---|"]
    for number, level in sorted(verification_levels(manifest).items()):
        lines.append(
            f"| **Level {number} — {level['name']}** | {level['compares']} | {level['limit']} |"
        )
    return "\n".join(lines)


def replace_block(content: str, begin: str, end: str, body: str) -> str | None:
    """Replace the generated body between one pair of markers, or None if absent."""
    if content.count(begin) != 1 or content.count(end) != 1:
        return None
    head, rest = content.split(begin, 1)
    _, tail = rest.split(end, 1)
    return f"{head}{begin}\n{body}\n{end}{tail}"


def level_names(level: dict) -> list[str]:
    return [level["name"], *level.get("aliases", [])]


def find_level_prose_violations(text: str, manifest: dict, check_names: bool = True) -> list[str]:
    """Reject undefined level numbers, renamed levels, and a wrong level count."""
    levels = verification_levels(manifest)
    normalized = strip_markup(text)
    violations = []
    for match in LEVEL_NUMBER.finditer(normalized):
        number = int(match.group(1))
        if number not in levels:
            violations.append(
                f"undefined verification level {match.group(0)!r} "
                f"(capabilities.toml defines {sorted(levels)})"
            )
    if check_names:
        for match in LEVEL_NAMED.finditer(normalized):
            level = levels.get(int(match.group(1)))
            if level is None:
                continue
            label = match.group(2).strip().casefold()
            if not any(label.startswith(name.casefold()) for name in level_names(level)):
                violations.append(
                    f"Level {match.group(1)} is named {match.group(2).strip()!r}; "
                    f"capabilities.toml names it {level['name']!r}"
                )
    for match in LEVEL_COUNT.finditer(normalized):
        word = match.group(1).casefold()
        count = COUNT_WORDS.get(word) or int(word)
        if count != len(levels):
            violations.append(
                f"{match.group(0)!r} but capabilities.toml defines {len(levels)} levels"
            )
    return violations


def find_rust_level_violations(source: str, manifest: dict) -> list[str]:
    """Rust level labels must use the canonical name and limitation verbatim."""
    levels = verification_levels(manifest)
    violations = []
    for match in RUST_LEVEL_LITERAL.finditer(source):
        number, parts = int(match.group(1)), match.group(2).split(" — ")
        level = levels.get(number)
        if level is None:
            violations.append(f"undefined verification level in label {match.group(0)}")
            continue
        if parts[0] != level["name"]:
            violations.append(f"{match.group(0)}: expected name {level['name']!r}")
        if len(parts) > 1 and " — ".join(parts[1:]) != level["limit"]:
            violations.append(f"{match.group(0)}: expected limitation {level['limit']!r}")
    return violations


def find_level_method_violations(source: str, manifest: dict) -> list[str]:
    """verification_level() must map exactly the declared methods to each level."""
    declared = {number: set(level.get("methods", [])) for number, level in verification_levels(manifest).items()}
    mapped: dict[int, set[str]] = {}
    for match in RUST_LEVEL_ARM.finditer(source):
        methods = set(re.findall(r"VerificationMethod::(\w+)", match.group(1)))
        mapped.setdefault(int(match.group(2)), set()).update(methods)
    if mapped != declared:
        return [f"VerificationMethod level mapping {mapped} differs from capabilities.toml {declared}"]
    return []


def find_locale_level_violations(path: Path, manifest: dict) -> list[str]:
    """Locale keys and values must agree on a defined level number."""
    levels = verification_levels(manifest)
    english = path.stem == "en"
    with path.open("rb") as stream:
        strings = tomllib.load(stream).get("messages", {})
    violations = []
    for key, value in strings.items():
        if not isinstance(value, str):
            continue
        key_level = LOCALE_LEVEL_KEY.search(key)
        value_levels = {int(number) for number in LEVEL_NUMBER.findall(value)}
        if key_level and int(key_level.group(1)) not in levels:
            violations.append(f"{key}: undefined verification level")
        if key_level and value_levels and value_levels != {int(key_level.group(1))}:
            violations.append(f"{key}: value names level {sorted(value_levels)}: {value!r}")
        violations.extend(
            f"{key}: {violation}"
            for violation in find_level_prose_violations(
                value, manifest, check_names=english and not key.endswith("-description")
            )
        )
    return violations


def contradicting_claims(manifest: dict) -> list[tuple[str, re.Pattern]]:
    """Patterns that deny an implemented capability, keyed by capability."""
    return [
        (name, re.compile(pattern, re.I))
        for name, capability in sorted(manifest.get("capabilities", {}).items())
        if capability.get("code") in CODE_PRESENT
        for pattern in capability.get("contradicted_by", [])
    ]


def find_contradicted_capability_claims(line: str, claims: list[tuple[str, re.Pattern]]) -> list[str]:
    normalized = strip_markup(line)
    return [
        f"denies implemented capability {name!r}: {pattern.search(normalized).group(0)!r}"
        for name, pattern in claims
        if pattern.search(normalized)
    ]


def markdown_paragraphs(content: str) -> list[tuple[int, str]]:
    """Yield (first line number, joined text) for blank-line separated blocks.

    Hard-wrapped prose splits sentences across lines, so claim checks that
    span a sentence must see the whole paragraph.
    """
    paragraphs, start, lines = [], None, []
    for line_number, line in enumerate(content.splitlines(), 1):
        if line.strip():
            start = start or line_number
            lines.append(line.strip())
        elif lines:
            paragraphs.append((start, " ".join(lines)))
            start, lines = None, []
    if lines:
        paragraphs.append((start, " ".join(lines)))
    return paragraphs


def get_schema_version() -> int:
    """Extract CURRENT_SCHEMA_VERSION from src/core.rs."""
    if not CORE_RS.exists():
        return 0
    content = CORE_RS.read_text(encoding="utf-8")
    match = re.search(r"pub\s+const\s+CURRENT_SCHEMA_VERSION\s*:\s*i64\s*=\s*(\d+)", content)
    return int(match.group(1)) if match else 0


def main(argv: list[str]) -> int:
    with MANIFEST.open("rb") as stream:
        manifest = tomllib.load(stream)

    manifest_content = CAPABILITY_MANIFEST_MD.read_text(encoding="utf-8")
    if "--write" in argv:
        regenerated = replace_status_table(manifest_content, manifest)
        if regenerated is None:
            print(f"{CAPABILITY_MANIFEST_MD.name}: missing {TABLE_BEGIN} / {TABLE_END} markers")
            return 1
        CAPABILITY_MANIFEST_MD.write_text(regenerated, encoding="utf-8")
        manifest_content = regenerated
        readme_content = README_MD.read_text(encoding="utf-8")
        regenerated = replace_block(
            readme_content, LEVELS_BEGIN, LEVELS_END, render_level_table(manifest)
        )
        if regenerated is None:
            print(f"{README_MD.name}: missing {LEVELS_BEGIN} / {LEVELS_END} markers")
            return 1
        README_MD.write_text(regenerated, encoding="utf-8")

    schema_version = get_schema_version()
    unsupported = [
        name
        for name, capability in manifest.get("capabilities", {}).items()
        if capability.get("production_supported") is False
    ]

    violations = []

    violations.extend(
        f"{CAPABILITY_MANIFEST_MD.name}: {violation}"
        for violation in find_manifest_drift(manifest_content, manifest)
    )

    if not verification_levels(manifest):
        violations.append("capabilities.toml: no [[verification_levels]] defined")
    readme_content = README_MD.read_text(encoding="utf-8")
    violations.extend(
        f"README.md: {violation}"
        for violation in find_status_section_violations(readme_content, manifest)
    )
    regenerated = replace_block(
        readme_content, LEVELS_BEGIN, LEVELS_END, render_level_table(manifest)
    )
    if regenerated is None:
        violations.append(f"README.md: {LEVELS_BEGIN} / {LEVELS_END} must each appear exactly once")
    elif regenerated != readme_content:
        violations.append(
            "README.md: verification level table is stale; "
            "run python3 scripts/verify-capability-claims.py --write"
        )

    evidence_source = EVIDENCE_RS.read_text(encoding="utf-8")
    violations.extend(
        f"{EVIDENCE_RS.relative_to(ROOT)}: {violation}"
        for violation in find_level_method_violations(evidence_source, manifest)
    )
    for path in sorted((ROOT / "src").rglob("*.rs")):
        violations.extend(
            f"{path.relative_to(ROOT)}: {violation}"
            for violation in find_rust_level_violations(path.read_text(encoding="utf-8"), manifest)
        )
    for path in sorted((ROOT / "locales").glob("*.toml")):
        violations.extend(
            f"{path.relative_to(ROOT)}: {violation}"
            for violation in find_locale_level_violations(path, manifest)
        )

    claims = contradicting_claims(manifest)
    for path in sorted(ROOT.rglob("*.md")):
        relative = path.relative_to(ROOT)
        if relative.parts[:2] == ("docs", "history"):
            continue
        # Build output and vendored dependency sources are not our documentation.
        if relative.parts[0] == "target" or any(part.startswith(".") for part in relative.parts):
            continue

        try:
            content = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue

        for line_number, paragraph in markdown_paragraphs(content):
            violations.extend(
                f"{relative}:{line_number}: CAPABILITY CONTRADICTION: {violation}"
                for violation in find_contradicted_capability_claims(paragraph, claims)
            )

        for line_number, line in enumerate(content.splitlines(), 1):
            violations.extend(
                f"{relative}:{line_number}: VERIFICATION LEVEL VOCABULARY: {violation}"
                for violation in find_level_prose_violations(line, manifest)
            )

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
    sys.exit(main(sys.argv[1:]))
