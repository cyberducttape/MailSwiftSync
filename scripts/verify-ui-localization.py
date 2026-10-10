#!/usr/bin/env python3
"""Fail closed when UI translation keys or obvious raw UI copy drift."""

from pathlib import Path
import re
import sys
import json
import tomllib


ROOT = Path(__file__).resolve().parents[1]
UI_DIR = ROOT / "src" / "ui"
LOCALES_DIR = ROOT / "locales"
ENGLISH_FILE = tomllib.loads((LOCALES_DIR / "en.toml").read_text(encoding="utf-8"))
ENGLISH = ENGLISH_FILE["messages"]
# Every translated catalog must cover exactly the English stable keys.
TRANSLATIONS = {
    code: (name, tomllib.loads((LOCALES_DIR / f"{code}.toml").read_text(encoding="utf-8")))
    for code, name in (("de", "German"), ("id", "Indonesian"))
}

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


def decode_rust_string(value: str) -> str:
    """Decode a simple Rust string literal captured without its quotes."""
    return json.loads('"' + value + '"')


def translation_keys() -> set[str]:
    keys: set[str] = set()
    for path in UI_DIR.glob("*.rs"):
        if path.name == "language.rs":
            continue
        source = path.read_text(encoding="utf-8")
        keys.update(
            decode_rust_string(value)
            for value in re.findall(
                r"(?:self\.)?language\.text\(\s*\"((?:\\.|[^\"\\])*)\"",
                source,
            )
        )
        keys.update(
            decode_rust_string(value)
            for value in re.findall(
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
        keys.update(
            decode_rust_string(value)
            for value in re.findall(r'set_status\(\s*"((?:\\.|[^"\\])*)"', source)
        )
    return keys


def stable_message_keys() -> set[str]:
    keys: set[str] = set()
    for path in (ROOT / "src").rglob("*.rs"):
        source = path.read_text(encoding="utf-8")
        keys.update(
            decode_rust_string(value)
            for value in re.findall(
                r"(?:self\.)?language\.message\(\s*\"((?:\\.|[^\"\\])*)\"",
                source,
            )
        )
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
    literal_texts = translation_keys()
    source_copy = {text for text in literal_texts if not text.startswith("ui.")} | status_message_keys()
    stable_keys = stable_message_keys() | {
        text for text in literal_texts if text.startswith("ui.")
    }
    english_sources = set(ENGLISH.values())
    missing = sorted(source_copy - english_sources)
    # Duplicate English labels are safe only when they resolve to the same
    # localized text. Stable keys can intentionally name the same UI concept
    # from different surfaces (for example Source/Destination); differing
    # translations for one English source would make source-based lookup
    # ambiguous and nondeterministic.
    translation_errors: list[str] = []
    for code, (name, locale_file) in TRANSLATIONS.items():
        messages = locale_file["messages"]
        if locale_file.get("meta", {}).get("language") != code:
            translation_errors.append(f"{name} locale metadata language identifier is invalid")
        for key in sorted(key for key in ENGLISH if key not in messages):
            translation_errors.append(f"missing {name} locale message: {key!r}")
        for key in sorted(key for key in messages if key not in ENGLISH):
            translation_errors.append(f"{name} locale has unknown message: {key!r}")
        for text in sorted(
            text
            for text in english_sources
            if list(ENGLISH.values()).count(text) > 1
            and len({messages[key] for key, value in ENGLISH.items() if value == text and key in messages}) > 1
        ):
            translation_errors.append(f"English locale source copy is ambiguous in {name}: {text!r}")
        for key in sorted(
            key
            for key in ENGLISH.keys() & messages.keys()
            if set(re.findall(r"\{[^{}]+\}", ENGLISH[key]))
            != set(re.findall(r"\{[^{}]+\}", messages[key]))
        ):
            translation_errors.append(f"English/{name} format placeholders differ: {key!r}")
    invalid_ids = sorted(
        key
        for key in ENGLISH
        if not re.fullmatch(r"(?:ui|mailbox|migration|verification)\.[a-z0-9]+(?:-[a-z0-9]+)*", key)
    )
    missing_stable = sorted(key for key in stable_keys if key not in ENGLISH)
    raw = raw_ui_literals()
    metadata_ok = ENGLISH_FILE.get("meta", {}).get("language") == "en"
    if (
        missing
        or translation_errors
        or invalid_ids
        or missing_stable
        or not metadata_ok
        or raw
    ):
        for key in missing:
            print(f"missing English locale source copy: {key!r}", file=sys.stderr)
        for error in translation_errors:
            print(error, file=sys.stderr)
        for key in invalid_ids:
            print(f"locale key is not a stable ui.* identifier: {key!r}", file=sys.stderr)
        for key in missing_stable:
            print(f"stable UI message key is absent from locale catalogs: {key!r}", file=sys.stderr)
        if not metadata_ok:
            print("locale metadata language identifiers are invalid", file=sys.stderr)
        for violation in raw:
            print(f"raw user-facing UI literal: {violation}", file=sys.stderr)
        return 1
    print("PASS: locale catalog coverage, source-copy mapping, and raw-literal guard")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
