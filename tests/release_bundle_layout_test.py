#!/usr/bin/env python3
"""Verify that every release archive format has one consistent bundle root."""

from pathlib import Path
import io
import stat
import subprocess
import tarfile
import tempfile
import zipfile


DOCUMENTS = (
    "README.md",
    "LICENSE",
    "PROVIDER_SETUP.md",
    "OAUTH_SETUP.md",
    "CAPABILITY_MANIFEST.md",
    "PRODUCTION_STATUS.md",
    "SECURITY.md",
    "CHANGELOG.md",
    "capabilities.toml",
)


def make_bundle(root: Path) -> None:
    for document in DOCUMENTS:
        path = root / document
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("# fixture\n", encoding="utf-8")
    (root / "mailswiftsync-test").write_text("binary fixture\n", encoding="utf-8")
    (root / "CAPABILITY_MANIFEST.md").write_text(
        "[capabilities](capabilities.toml)\n", encoding="utf-8"
    )
    (root / "README.md").write_text(
        "[release docs](docs/release-readiness.md)\n", encoding="utf-8"
    )
    docs = root / "docs"
    docs.mkdir()
    (docs / "release-readiness.md").write_text("# fixture\n", encoding="utf-8")


def run_verifier(archive: Path, expected: bool) -> None:
    result = subprocess.run(
        ["scripts/verify-release-bundle.sh", str(archive)],
        text=True,
        capture_output=True,
    )
    if (result.returncode == 0) != expected:
        raise AssertionError(
            f"unexpected verifier result for {archive}: {result.stdout}{result.stderr}"
        )


def run_link_checker(root: Path, expected: bool) -> None:
    result = subprocess.run(
        ["python3", "scripts/verify-markdown-links.py", str(root)],
        text=True,
        capture_output=True,
    )
    if (result.returncode == 0) != expected:
        raise AssertionError(
            f"unexpected link-checker result for {root}: {result.stdout}{result.stderr}"
        )


with tempfile.TemporaryDirectory(prefix="mailswiftsync-release-layout-") as temporary:
    workspace = Path(temporary)

    links = workspace / "links"
    (links / "docs").mkdir(parents=True)
    (links / "README.md").write_text(
        "[guide](docs/guide.md) [external](https://example.test/missing.md)\n",
        encoding="utf-8",
    )
    (links / "docs/guide.md").write_text("# guide\n", encoding="utf-8")
    run_link_checker(links, True)
    (links / "README.md").write_text("[missing](docs/missing.md)\n", encoding="utf-8")
    run_link_checker(links, False)

    bundle = workspace / "bundle"
    bundle.mkdir()
    make_bundle(bundle)

    linux_archive = workspace / "linux.tar.gz"
    with tarfile.open(linux_archive, "w:gz") as archive:
        for entry in bundle.iterdir():
            archive.add(entry, arcname=entry.name)
    run_verifier(linux_archive, True)

    for name in ("macos.zip", "windows.zip"):
        archive_path = workspace / name
        with zipfile.ZipFile(archive_path, "w") as archive:
            for entry in bundle.rglob("*"):
                if entry.is_file():
                    archive.write(entry, entry.relative_to(bundle).as_posix())
        run_verifier(archive_path, True)

    nested_archive = workspace / "nested.zip"
    with zipfile.ZipFile(nested_archive, "w") as archive:
        for entry in bundle.rglob("*"):
            if entry.is_file():
                archive.write(entry, (Path("bundle") / entry.relative_to(bundle)).as_posix())
    run_verifier(nested_archive, False)

    traversal_zip = workspace / "traversal.zip"
    with zipfile.ZipFile(traversal_zip, "w") as archive:
        archive.writestr("../release-verifier-escape.txt", "must not extract\n")
    run_verifier(traversal_zip, False)

    traversal_tar = workspace / "traversal.tar.gz"
    with tarfile.open(traversal_tar, "w:gz") as archive:
        entry = tarfile.TarInfo("../release-verifier-escape.txt")
        payload = b"must not extract\n"
        entry.size = len(payload)
        archive.addfile(entry, fileobj=io.BytesIO(payload))
    run_verifier(traversal_tar, False)

    symlink_zip = workspace / "symlink.zip"
    with zipfile.ZipFile(symlink_zip, "w") as archive:
        info = zipfile.ZipInfo("README.md")
        info.create_system = 3
        info.external_attr = (stat.S_IFLNK | 0o777) << 16
        archive.writestr(info, "/etc/passwd")
    run_verifier(symlink_zip, False)

    symlink_tar = workspace / "symlink.tar.gz"
    with tarfile.open(symlink_tar, "w:gz") as archive:
        entry = tarfile.TarInfo("README.md")
        entry.type = tarfile.SYMTYPE
        entry.linkname = "/etc/passwd"
        archive.addfile(entry)
    run_verifier(symlink_tar, False)

print("PASS: release archive layouts are consistent across tar.gz and ZIP formats")
