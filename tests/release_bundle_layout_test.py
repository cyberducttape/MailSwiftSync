#!/usr/bin/env python3
"""Verify that every release archive format has one consistent bundle root."""

from pathlib import Path
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
    "CONTRIBUTING.md",
)


def make_bundle(root: Path) -> None:
    for document in DOCUMENTS:
        (root / document).write_text("# fixture\n", encoding="utf-8")
    (root / "mailswiftsync-test").write_text("binary fixture\n", encoding="utf-8")
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


with tempfile.TemporaryDirectory(prefix="mailswiftsync-release-layout-") as temporary:
    workspace = Path(temporary)
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

print("PASS: release archive layouts are consistent across tar.gz and ZIP formats")
