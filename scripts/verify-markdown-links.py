#!/usr/bin/env python3
"""Verify that Markdown's local inline links resolve within a document tree."""

from __future__ import annotations

import re
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit


INLINE_LINK = re.compile(r"!?\[[^\]]*\]\(\s*(?:<([^>]+)>|([^\s)]+))")
REFERENCE_DEFINITION = re.compile(
    r"^\s{0,3}\[[^\]]+\]:\s*(?:<([^>]+)>|(\S+))"
)
IGNORED_DIRECTORIES = {".git", "target", "node_modules"}


def without_code(text: str) -> str:
    lines: list[str] = []
    fenced = False
    for line in text.splitlines(keepends=True):
        if re.match(r"^\s*(```|~~~)", line):
            fenced = not fenced
            lines.append("\n" if line.endswith("\n") else "")
        elif fenced:
            lines.append("\n" if line.endswith("\n") else "")
        else:
            lines.append(re.sub(r"`[^`\n]*`", "", line))
    return "".join(lines)


def local_target(target: str) -> str | None:
    parsed = urlsplit(target)
    if parsed.scheme or parsed.netloc or not parsed.path:
        return None
    return unquote(parsed.path)


def links_in(document: Path) -> list[tuple[int, str]]:
    text = without_code(document.read_text(encoding="utf-8"))
    links: list[tuple[int, str]] = []
    for line_number, line in enumerate(text.splitlines(), start=1):
        for match in INLINE_LINK.finditer(line):
            target = match.group(1) or match.group(2)
            if target is not None:
                links.append((line_number, target))
        for match in (REFERENCE_DEFINITION.fullmatch(line),):
            if match:
                target = match.group(1) or match.group(2)
                if target is not None:
                    links.append((line_number, target))
    return links


def check(root: Path) -> list[str]:
    root = root.resolve()
    errors: list[str] = []
    documents = (
        document
        for document in root.rglob("*.md")
        if not any(part in IGNORED_DIRECTORIES for part in document.relative_to(root).parts)
    )
    for document in sorted(documents):
        for line_number, raw_target in links_in(document):
            target = local_target(raw_target)
            if target is None:
                continue
            resolved = (document.parent / target).resolve()
            try:
                resolved.relative_to(root)
            except ValueError:
                errors.append(
                    f"{document.relative_to(root)}:{line_number}: link escapes document root: {raw_target}"
                )
                continue
            if not resolved.exists():
                errors.append(
                    f"{document.relative_to(root)}:{line_number}: missing local link: {raw_target}"
                )
    return errors


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {Path(sys.argv[0]).name} DOCUMENT_ROOT", file=sys.stderr)
        return 2
    root = Path(sys.argv[1])
    if not root.is_dir():
        print(f"FAIL: document root does not exist: {root}", file=sys.stderr)
        return 1
    errors = check(root)
    if errors:
        print("FAIL: unresolved local Markdown links:", file=sys.stderr)
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"PASS: all local Markdown links resolve under {root}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
