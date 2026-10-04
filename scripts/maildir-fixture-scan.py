#!/usr/bin/env python3
"""Stream Maildir headers once and retain only MailSwiftSync fixture IDs."""

import os
from pathlib import Path
import re
import sys


MESSAGE_ID = re.compile(rb"(?i)^message-id:\s*<([^>]+)>")
ASSERTED_FIXTURE_IDS = {
    b"mailswiftsync-integration-fixture@example.test",
    b"mailswiftsync-incremental-fixture@example.test",
    b"mailswiftsync-literal-framing@example.test",
    b"mailswiftsync-non-utf8@example.test",
    b"mailswiftsync-renamed-special-use@example.test",
    b"mailswiftsync-duplicate@example.test",
} | {
    f"mailswiftsync-sparse-{index}@example.test".encode("ascii")
    for index in range(91, 101)
}
MAX_HEADER_BYTES = 256 * 1024
MAX_HEADER_LINE_BYTES = 8192


def scan_maildir(root: Path) -> tuple[dict[bytes, int], int]:
    found: dict[bytes, int] = {}
    files_scanned = 0
    def walk_error(error: OSError) -> None:
        raise RuntimeError(f"could not traverse destination Maildir: {error}") from error

    for directory, _, filenames in os.walk(root, followlinks=False, onerror=walk_error):
        if Path(directory).name not in {"cur", "new"}:
            continue
        for filename in filenames:
            path = Path(directory, filename)
            if path.is_symlink():
                continue
            files_scanned += 1
            try:
                with path.open("rb") as message:
                    remaining = MAX_HEADER_BYTES
                    while remaining > 0:
                        line = message.readline(min(MAX_HEADER_LINE_BYTES, remaining))
                        if not line or line in {b"\n", b"\r\n", b"\r"}:
                            break
                        remaining -= len(line)
                        match = MESSAGE_ID.match(line)
                        if match:
                            fixture_id = match.group(1)
                            normalized_id = fixture_id.lower()
                            if normalized_id in ASSERTED_FIXTURE_IDS:
                                found[normalized_id] = found.get(normalized_id, 0) + 1
            except OSError as error:
                raise RuntimeError(f"could not read a destination Maildir message: {error}") from error
    return found, files_scanned


def main() -> int:
    if len(sys.argv) != 2:
        print(f"Usage: {sys.argv[0]} <maildir-root>", file=sys.stderr)
        return 2
    root = Path(sys.argv[1])
    if not root.is_dir():
        print("destination Maildir is not a directory", file=sys.stderr)
        return 2
    try:
        fixture_ids, files_scanned = scan_maildir(root)
    except RuntimeError as error:
        print(error, file=sys.stderr)
        return 1
    for fixture_id, count in sorted(fixture_ids.items()):
        for _ in range(count):
            print(f"Message-ID: <{fixture_id.decode('ascii', errors='replace')}>")
    print(
        f"Maildir fixture scan examined {files_scanned} message files and found "
        f"{sum(fixture_ids.values())} fixture Message-ID headers.",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
