#!/usr/bin/env python3
"""Attribute container memory to process groups during a scale run.

Samples /proc once per interval and records, per process name, the peak of the
summed resident set size across that name's processes (Dovecot runs several
workers). It also records the peak cgroup anonymous memory and page-cache
(file) usage, which separates process memory from filesystem cache that
`docker stats` reports as container memory. The summary file is rewritten
atomically whenever a peak changes, so it survives the sampler being killed.

Usage: process-memory-sampler.py <summary-file> [interval-seconds]
"""

import os
import sys
import time
from pathlib import Path

CGROUP_MEMORY_STAT = Path("/sys/fs/cgroup/memory.stat")


def process_rss_by_name(proc: Path = Path("/proc")) -> dict[str, int]:
    """Current summed VmRSS in bytes, keyed by process name."""
    totals: dict[str, int] = {}
    for entry in proc.iterdir():
        if not entry.name.isdigit():
            continue
        try:
            status = (entry / "status").read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue  # The process exited between listing and reading.
        name = None
        rss_kib = None
        for line in status.splitlines():
            if line.startswith("Name:"):
                name = line.split(":", 1)[1].strip()
            elif line.startswith("VmRSS:"):
                parts = line.split()
                if len(parts) >= 2 and parts[1].isdigit():
                    rss_kib = int(parts[1])
        if name and rss_kib is not None:
            totals[name] = totals.get(name, 0) + rss_kib * 1024
    return totals


def cgroup_memory(path: Path = CGROUP_MEMORY_STAT) -> dict[str, int]:
    """Anonymous and file-backed cgroup memory in bytes, when available."""
    values: dict[str, int] = {}
    try:
        for line in path.read_text(encoding="utf-8").splitlines():
            key, _, value = line.partition(" ")
            if key in ("anon", "file") and value.strip().isdigit():
                values[key] = int(value)
    except OSError:
        pass
    return values


def render(peaks: dict[str, int], cgroup_peaks: dict[str, int], samples: int) -> str:
    lines = [f"memory_samples={samples}"]
    for key in ("anon", "file"):
        if key in cgroup_peaks:
            lines.append(f"peak_cgroup_{key}_bytes={cgroup_peaks[key]}")
    for name, value in sorted(peaks.items(), key=lambda item: (-item[1], item[0])):
        safe = "".join(ch if ch.isalnum() or ch in "-_." else "_" for ch in name)
        lines.append(f"peak_rss_bytes[{safe}]={value}")
    return "\n".join(lines) + "\n"


def write_atomically(path: Path, text: str) -> None:
    temporary = path.with_name(f".{path.name}.tmp")
    temporary.write_text(text, encoding="utf-8")
    os.replace(temporary, path)


def main() -> int:
    if len(sys.argv) not in (2, 3):
        print(__doc__.strip().splitlines()[-1], file=sys.stderr)
        return 2
    summary = Path(sys.argv[1])
    interval = float(sys.argv[2]) if len(sys.argv) == 3 else 1.0
    peaks: dict[str, int] = {}
    cgroup_peaks: dict[str, int] = {}
    samples = 0
    while True:
        samples += 1
        changed = samples == 1
        for name, value in process_rss_by_name().items():
            if value > peaks.get(name, 0):
                peaks[name] = value
                changed = True
        for key, value in cgroup_memory().items():
            if value > cgroup_peaks.get(key, 0):
                cgroup_peaks[key] = value
                changed = True
        if changed or samples % 60 == 0:
            write_atomically(summary, render(peaks, cgroup_peaks, samples))
        time.sleep(interval)


if __name__ == "__main__":
    raise SystemExit(main())
