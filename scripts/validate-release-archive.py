#!/usr/bin/env python3
"""Validate release archive metadata before any member is extracted."""

import os
import re
import stat
import struct
import sys
import tarfile
import zipfile
from pathlib import PurePosixPath


MAX_ARCHIVE_BYTES = 512 * 1024 * 1024
MAX_UNPACKED_BYTES = 512 * 1024 * 1024
MAX_MEMBERS = 60_000


def safe_member_name(name: str) -> str:
    if not name or "\0" in name or "\\" in name or name.startswith("/"):
        raise ValueError(f"unsafe archive member path: {name!r}")
    if re.match(r"^[A-Za-z]:", name):
        raise ValueError(f"drive-qualified archive member path: {name!r}")
    path = PurePosixPath(name)
    if any(part in ("", ".", "..") for part in path.parts):
        raise ValueError(f"unsafe archive member path: {name!r}")
    normalized = path.as_posix().rstrip("/")
    if not normalized:
        raise ValueError(f"empty archive member path: {name!r}")
    return normalized


def validate_members(names_and_sizes):
    seen = set()
    count = 0
    unpacked_bytes = 0
    for name, size in names_and_sizes:
        count += 1
        if count > MAX_MEMBERS:
            raise ValueError(f"archive contains more than {MAX_MEMBERS} members")
        normalized = safe_member_name(name)
        if normalized in seen:
            raise ValueError(f"duplicate archive member path: {normalized!r}")
        seen.add(normalized)
        if size < 0:
            raise ValueError(f"negative archive member size: {normalized!r}")
        unpacked_bytes += size
        if unpacked_bytes > MAX_UNPACKED_BYTES:
            raise ValueError(
                f"archive expands beyond the {MAX_UNPACKED_BYTES}-byte release limit"
            )


def validate_tar(path: str) -> None:
    with tarfile.open(path, "r|*") as archive:
        def records():
            for member in archive:
                if not (member.isfile() or member.isdir()):
                    raise ValueError(
                        f"unsupported tar member type for {member.name!r}; links and special files are forbidden"
                    )
                yield member.name, member.size if member.isfile() else 0

        validate_members(records())


def validate_zip_directory_size(path: str) -> None:
    # zipfile loads every central-directory entry into memory when it opens an
    # archive. Check the EOCD count first so a small archive with millions of
    # empty entries cannot allocate an unbounded list before our member cap.
    with open(path, "rb") as archive:
        size = os.fstat(archive.fileno()).st_size
        archive.seek(max(0, size - (65_535 + 22)))
        tail = archive.read()
    signature = b"PK\x05\x06"
    position = tail.rfind(signature)
    while position >= 0:
        if position + 22 <= len(tail):
            (
                _signature,
                disk_number,
                directory_disk,
                disk_entries,
                total_entries,
                directory_size,
                directory_offset,
                comment_length,
            ) = struct.unpack_from("<4s4H2LH", tail, position)
            if position + 22 + comment_length == len(tail):
                if disk_number != 0 or directory_disk != 0 or disk_entries != total_entries:
                    raise ValueError("multi-disk ZIP archives are not supported")
                if 0xFFFF in (disk_entries, total_entries) or 0xFFFFFFFF in (
                    directory_size,
                    directory_offset,
                ):
                    raise ValueError("ZIP64 archives are not supported by the bounded verifier")
                if total_entries > MAX_MEMBERS:
                    raise ValueError(f"archive contains more than {MAX_MEMBERS} members")
                if directory_size > size or directory_offset > size:
                    raise ValueError("ZIP central-directory bounds exceed archive size")
                return
        position = tail.rfind(signature, 0, position)
    raise ValueError("ZIP end-of-central-directory record is missing or malformed")


def validate_zip(path: str) -> None:
    validate_zip_directory_size(path)
    with zipfile.ZipFile(path) as archive:
        members = archive.infolist()
        for member in members:
            if member.flag_bits & 0x1:
                raise ValueError(f"encrypted ZIP member is not allowed: {member.filename!r}")
            mode = member.external_attr >> 16
            kind = stat.S_IFMT(mode)
            if kind not in (0, stat.S_IFREG, stat.S_IFDIR):
                raise ValueError(
                    f"unsupported ZIP member type for {member.filename!r}; links and special files are forbidden"
                )
        validate_members((member.filename, member.file_size) for member in members)


def bounded_snapshot(
    source: str, destination: str, max_bytes: int = MAX_ARCHIVE_BYTES
) -> None:
    if max_bytes < 0:
        raise ValueError("archive snapshot byte limit must not be negative")
    copied = 0
    created = False
    try:
        with open(source, "rb") as input_file, open(destination, "xb") as output_file:
            created = True
            while True:
                chunk = input_file.read(min(1024 * 1024, max_bytes + 1 - copied))
                if not chunk:
                    return
                copied += len(chunk)
                if copied > max_bytes:
                    raise ValueError(f"compressed archive exceeds {max_bytes}-byte limit")
                output_file.write(chunk)
    except BaseException:
        if created:
            try:
                os.unlink(destination)
            except FileNotFoundError:
                pass
        raise


def main() -> int:
    if len(sys.argv) == 4 and sys.argv[1] == "--snapshot":
        source, snapshot = sys.argv[2:]
        path = snapshot
        create_snapshot = True
    elif len(sys.argv) == 2:
        path = sys.argv[1]
        create_snapshot = False
    else:
        print(
            "usage: validate-release-archive.py ARCHIVE | --snapshot SOURCE DESTINATION",
            file=sys.stderr,
        )
        return 2
    try:
        if create_snapshot:
            bounded_snapshot(source, snapshot)
        archive_size = os.path.getsize(path)
        if archive_size > MAX_ARCHIVE_BYTES:
            raise ValueError(f"compressed archive exceeds {MAX_ARCHIVE_BYTES}-byte limit")
        if zipfile.is_zipfile(path):
            validate_zip(path)
        else:
            validate_tar(path)
    except (EOFError, OSError, tarfile.TarError, zipfile.BadZipFile, ValueError) as error:
        print(f"FAIL: release archive metadata rejected: {error}", file=sys.stderr)
        return 1
    print("PASS: release archive metadata is safe to extract")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
