#!/usr/bin/env python3
"""Fail closed when UI translation keys or obvious raw UI copy drift."""

from pathlib import Path
import re
import sys


ROOT = Path(__file__).resolve().parents[1]
UI_DIR = ROOT / "src" / "ui"
LANGUAGE = (UI_DIR / "language.rs").read_text(encoding="utf-8")

# Product/protocol names and visual separators are intentionally not translated.
RAW_LITERAL_ALLOWLIST = {
    "MailSwiftSync",
    "doveadm",
    "→",
    "✓",
    "⏺",
    "!",
    "in progress",
}


def translation_keys() -> set[str]:
    keys: set[str] = set()
    for path in UI_DIR.glob("*.rs"):
        if path.name == "language.rs":
            continue
        source = path.read_text(encoding="utf-8")
        keys.update(
            re.findall(
                r"(?:self\.)?language\.text\(\s*\"((?:\\.|[^\"\\])*)\"",
                source,
            )
        )
        keys.update(
            re.findall(
                r"(?<![A-Za-z_])language\.text\(\s*\"((?:\\.|[^\"\\])*)\"",
                source,
            )
        )
    return keys


def status_message_keys() -> set[str]:
    """Fixed messages passed to `set_status`; the header translates them
    with `UiLanguage::lookup`, so each needs a catalog entry."""
    keys: set[str] = set()
    for path in (ROOT / "src").rglob("*.rs"):
        source = path.read_text(encoding="utf-8")
        keys.update(re.findall(r'set_status\(\s*"((?:\\.|[^"\\])*)"', source))
    return keys


def raw_ui_literals() -> list[str]:
    violations: list[str] = []
    patterns = (
        re.compile(
            r"\bui\.(?:label|heading|button|small|strong|monospace)\(\s*\"([^\"]+)\""
        ),
        re.compile(r"egui::(?:Window|Modal)::new\(\s*\"([^\"]+)\""),
        re.compile(r"egui::Button::new\(\s*\"([^\"]+)\""),
        re.compile(r"RichText::new\(\s*\"([^\"]+)\""),
    )
    for path in UI_DIR.glob("*.rs"):
        if path.name in {"language.rs", "theme.rs"}:
            continue
        for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            for pattern in patterns:
                for literal in pattern.findall(line):
                    if literal not in RAW_LITERAL_ALLOWLIST:
                        violations.append(f"{path.relative_to(ROOT)}:{line_number}: {literal!r}")
    return violations


def main() -> int:
    missing = sorted(
        key
        for key in translation_keys() | status_message_keys()
        if f'"{key}" =>' not in LANGUAGE
    )
    raw = raw_ui_literals()
    if missing or raw:
        for key in missing:
            print(f"missing German translation key: {key!r}", file=sys.stderr)
        for violation in raw:
            print(f"raw user-facing UI literal: {violation}", file=sys.stderr)
        return 1
    print("PASS: UI translation keys and raw-literal guard")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
