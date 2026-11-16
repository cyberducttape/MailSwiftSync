#!/usr/bin/env python3
"""Measure migration-audit SQLite staging on synthetic JSON snapshots.

Linux-only engineering benchmark. It reports observed peak process RSS and
sampled temporary-stage bytes; it does not establish a supported capacity
budget or persistent-storage durability guarantee.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import tempfile
import time


def snapshot(path: Path, count: int, payload_bytes: int) -> None:
    prefix = "000000000-"
    padding = "x" * (payload_bytes - len(prefix))
    assert len(prefix + padding) == payload_bytes
    with path.open("w", encoding="utf-8", newline="\n") as output:
        output.write('{"resources":[')
        for index in range(count):
            if index:
                output.write(",")
            record = {
                "id": f"resource-{index:09}",
                "payload": f"{index:09}-" + padding,
            }
            json.dump(record, output, separators=(",", ":"), ensure_ascii=True)
        output.write("]}\n")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def tree_bytes(path: Path) -> int:
    total = 0
    for root, _, names in os.walk(path):
        for name in names:
            try:
                total += (Path(root) / name).stat().st_size
            except FileNotFoundError:
                continue
    return total


def peak_rss_kib(pid: int) -> int:
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("VmHWM:"):
                return int(line.split()[1])
    except (FileNotFoundError, ProcessLookupError, ValueError):
        pass
    return 0


def host_metadata() -> list[str]:
    metadata = [
        f"os={platform.platform()}",
        f"machine={platform.machine()}",
        f"logical_cpus={os.cpu_count() or 'unknown'}",
        f"temp_directory={tempfile.gettempdir()}",
    ]
    try:
        cpuinfo = Path("/proc/cpuinfo").read_text(errors="replace")
        model = next(
            (line.split(":", 1)[1].strip() for line in cpuinfo.splitlines() if line.startswith("model name")),
            "unknown",
        )
        metadata.append(f"cpu_model={model}")
        memory = Path("/proc/meminfo").read_text()
        total_kib = next(
            int(line.split()[1]) for line in memory.splitlines() if line.startswith("MemTotal:")
        )
        metadata.append(f"host_memory_mib={total_kib // 1024}")
    except (OSError, StopIteration, ValueError):
        metadata.append("cpu_model_or_host_memory=unavailable")
    filesystem = subprocess.run(
        ["df", "-PT", tempfile.gettempdir()],
        check=False,
        capture_output=True,
        text=True,
    )
    if filesystem.returncode == 0:
        lines = filesystem.stdout.splitlines()
        if len(lines) > 1:
            metadata.append(f"temp_filesystem={lines[-1]}")
    try:
        compiler = subprocess.run(
            ["rustc", "--version"], check=False, capture_output=True, text=True
        )
        metadata.append(f"rustc={compiler.stdout.strip() or 'unavailable'}")
    except OSError:
        metadata.append("rustc=unavailable")
    return metadata


def run_case(binary: Path, count: int, payload_bytes: int) -> dict[str, object]:
    with tempfile.TemporaryDirectory(prefix="mss-migrate-audit-bench-") as temporary:
        root = Path(temporary)
        runtime = root / "runtime"
        runtime.mkdir(mode=0o700)
        source = root / "source.json"
        destination = root / "destination.json"
        report = root / "report.json"
        snapshot(source, count, payload_bytes)
        snapshot(destination, count, payload_bytes)
        stage = runtime / "mailswiftsync"
        environment = os.environ.copy()
        environment["XDG_RUNTIME_DIR"] = str(runtime)
        command = [str(binary), "migrateaudit", str(source), str(destination), str(report)]
        started = time.monotonic()
        process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=environment)
        stage_peak = 0
        rss_peak = 0
        while process.poll() is None:
            stage_peak = max(stage_peak, tree_bytes(stage) if stage.exists() else 0)
            rss_peak = max(rss_peak, peak_rss_kib(process.pid))
            time.sleep(0.01)
        stdout, stderr = process.communicate()
        elapsed = time.monotonic() - started
        stage_peak = max(stage_peak, tree_bytes(stage) if stage.exists() else 0)
        if process.returncode != 0:
            raise RuntimeError(
                f"migrateaudit failed for {count} records: "
                f"{stdout.decode(errors='replace')}{stderr.decode(errors='replace')}"
            )
        return {
            "records_per_side": count,
            "payload_bytes_per_record": payload_bytes,
            "source_bytes": source.stat().st_size,
            "destination_bytes": destination.stat().st_size,
            "source_sha256": sha256_file(source),
            "destination_sha256": sha256_file(destination),
            "elapsed_seconds": round(elapsed, 3),
            "peak_stage_bytes_sampled": stage_peak,
            "peak_rss_kib_sampled": rss_peak,
            "report_verdict": json.loads(report.read_text())["verdict"],
        }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--binary",
        type=Path,
        default=Path("target/release/mailswiftsync"),
        help="built MailSwiftSync executable (default: target/release/mailswiftsync)",
    )
    parser.add_argument("--sizes", type=int, nargs="+", default=[1_000, 10_000, 100_000])
    parser.add_argument("--payload-bytes", type=int, default=512)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    if not os.access(binary, os.X_OK):
        parser.error(f"binary is not executable: {binary}")
    if any(size < 1 for size in args.sizes) or args.payload_bytes < len("000000000-"):
        parser.error("sizes must be positive and payload bytes must be at least 10")

    print(f"binary={binary}")
    print(f"binary_sha256={sha256_file(binary)}")
    for item in host_metadata():
        print(item)
    print(f"workload=unique identities; equal source/destination; payload_bytes={args.payload_bytes}")
    print("stage_sampling_interval_ms=10")
    for size in args.sizes:
        print(json.dumps(run_case(binary, size, args.payload_bytes), sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
