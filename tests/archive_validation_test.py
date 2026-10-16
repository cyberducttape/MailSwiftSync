"""Unit tests for pre-extraction release archive safeguards."""

import importlib.util
import stat
import zipfile
from pathlib import Path

import pytest


MODULE_PATH = Path(__file__).resolve().parents[1] / "scripts" / "validate-release-archive.py"
SPEC = importlib.util.spec_from_file_location("validate_release_archive", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
validator = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(validator)


def test_bounded_snapshot_stops_at_limit_and_removes_partial_file(tmp_path):
    source = tmp_path / "oversized.bin"
    snapshot = tmp_path / "snapshot.bin"
    source.write_bytes(b"x" * 65)

    with pytest.raises(ValueError, match="exceeds 64-byte limit"):
        validator.bounded_snapshot(source, snapshot, max_bytes=64)

    assert not snapshot.exists()


def test_bounded_snapshot_does_not_delete_an_existing_destination(tmp_path):
    source = tmp_path / "source.bin"
    snapshot = tmp_path / "snapshot.bin"
    source.write_bytes(b"ok")
    snapshot.write_bytes(b"preserve")

    with pytest.raises(FileExistsError):
        validator.bounded_snapshot(source, snapshot, max_bytes=64)

    assert snapshot.read_bytes() == b"preserve"


def test_zip_member_limit_is_checked_before_zipfile_materializes_members(
    tmp_path, monkeypatch
):
    archive_path = tmp_path / "many-members.zip"
    with zipfile.ZipFile(archive_path, "w") as archive:
        archive.writestr("one", "1")
        archive.writestr("two", "2")

    monkeypatch.setattr(validator, "MAX_MEMBERS", 1)
    with pytest.raises(ValueError, match="more than 1 members"):
        validator.validate_zip_directory_size(archive_path)


def test_zip_symlink_metadata_is_rejected(tmp_path):
    archive_path = tmp_path / "symlink.zip"
    with zipfile.ZipFile(archive_path, "w") as archive:
        link = zipfile.ZipInfo("link")
        link.create_system = 3
        link.external_attr = (stat.S_IFLNK | 0o777) << 16
        archive.writestr(link, "target")

    with pytest.raises(ValueError, match="links and special files are forbidden"):
        validator.validate_zip(archive_path)
