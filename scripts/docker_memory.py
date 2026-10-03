#!/usr/bin/env python3
"""Parse sampled Docker CLI memory-usage strings into byte counts."""

import re
import sys
from pathlib import Path


MEMORY_VALUE = re.compile(r"^\s*(\d+(?:\.\d+)?)\s*([A-Za-z]*)\s*$")
UNIT_MULTIPLIERS = {
    "": 1,
    "B": 1,
    "kB": 1000,
    "MB": 1000**2,
    "GB": 1000**3,
    "TB": 1000**4,
    "KiB": 1024,
    "MiB": 1024**2,
    "GiB": 1024**3,
    "TiB": 1024**4,
}


def parse_memory_bytes(value: str) -> int:
    """Parse one Docker stats value or its ``used / limit`` memory column."""
    used = value.split("/", 1)[0].strip()
    match = MEMORY_VALUE.fullmatch(used)
    if match is None or match.group(2) not in UNIT_MULTIPLIERS:
        raise ValueError(f"invalid Docker memory value: {value!r}")
    return int(float(match.group(1)) * UNIT_MULTIPLIERS[match.group(2)])


def peak_memory_bytes(path: Path) -> tuple[int, int]:
    samples = [
        parse_memory_bytes(line)
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    if not samples:
        raise ValueError("Docker stats produced no memory samples")
    return max(samples), len(samples)


def main() -> int:
    if len(sys.argv) != 2:
        print(f"Usage: {Path(sys.argv[0]).name} <docker-stats-memory-file>", file=sys.stderr)
        return 2
    try:
        peak, count = peak_memory_bytes(Path(sys.argv[1]))
    except (OSError, UnicodeError, ValueError) as error:
        print(f"Cannot summarize Docker memory samples: {error}", file=sys.stderr)
        return 1
    print(f"sampled_peak_container_memory_bytes={peak}")
    print(f"memory_samples={count}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
