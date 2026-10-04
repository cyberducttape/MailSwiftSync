#!/usr/bin/env python3
"""Create bounded, deterministic RFC822 payloads for end-to-end scale runs."""

import argparse
from pathlib import Path
import sys


MAX_MESSAGES = 100_000
MAX_TOTAL_BODY_BYTES = 2 * 1024**3
WRITE_CHUNK_BYTES = 64 * 1024


def generate_maildir(root: Path, message_count: int, total_body_bytes: int) -> int:
    if not 1 <= message_count <= MAX_MESSAGES:
        raise ValueError(f"message count must be between 1 and {MAX_MESSAGES}")
    if not 0 <= total_body_bytes <= MAX_TOTAL_BODY_BYTES:
        raise ValueError("total body bytes must be between 0 and 2 GiB")
    if not root.is_dir():
        raise ValueError("Maildir new directory must already exist")

    quotient, remainder = divmod(total_body_bytes, message_count)
    payload = b"x" * WRITE_CHUNK_BYTES
    for index in range(1, message_count + 1):
        path = root / f"scale-{index:06d}.eml"
        if path.exists():
            raise ValueError(f"refusing to overwrite existing fixture: {path.name}")
        headers = (
            "From: scale-lab@example.test\r\n"
            "To: lab@example.test\r\n"
            f"Subject: Scale fixture {index}\r\n"
            f"Message-ID: <mailswiftsync-scale-{index:06d}@example.test>\r\n"
            "Date: Tue, 01 Jan 2030 02:00:00 +0000\r\n"
            "Content-Type: text/plain; charset=utf-8\r\n\r\n"
        ).encode("ascii")
        body_size = quotient + int(index <= remainder)
        with path.open("xb") as message:
            message.write(headers)
            remaining = body_size
            while remaining:
                chunk_size = min(remaining, len(payload))
                message.write(payload[:chunk_size])
                remaining -= chunk_size
            message.write(b"\r\n")
        if index % 10_000 == 0 or index == message_count:
            print(
                f"Generated {index}/{message_count} scale messages "
                f"({total_body_bytes} body bytes total)",
                file=sys.stderr,
                flush=True,
            )
    return total_body_bytes


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("maildir_new", type=Path)
    parser.add_argument("message_count", type=int)
    parser.add_argument("total_body_bytes", type=int)
    args = parser.parse_args()
    try:
        generate_maildir(args.maildir_new, args.message_count, args.total_body_bytes)
    except (OSError, ValueError) as error:
        print(f"scale fixture generation failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
