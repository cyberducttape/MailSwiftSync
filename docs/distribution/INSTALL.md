# MailSwiftSync installation

MailSwiftSync is the migration control plane. It does not bundle an IMAP
engine or configure a mail server.

## Install a release archive

1. Download the archive and its adjacent `.sha256` file from the same GitHub
   release. Keep both files in the same directory and use the archive matching
   the host (`x86_64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`,
   `aarch64-apple-darwin`, or `x86_64-apple-darwin`).
2. Verify the archive before extracting it.

On Linux:

```bash
sha256sum -c mailswiftsync-x86_64-unknown-linux-gnu.tar.gz.sha256

# After obtaining the organization's GPG public key and checking its
# fingerprint through a separate trusted channel:
gpg --verify mailswiftsync-x86_64-unknown-linux-gnu.tar.gz.sha256.asc \
  mailswiftsync-x86_64-unknown-linux-gnu.tar.gz.sha256
```

On macOS:

```bash
shasum -a 256 -c mailswiftsync-aarch64-apple-darwin.zip.sha256
```

On Windows PowerShell, compare the displayed digest with the value in the
adjacent `.sha256` file:

```powershell
Get-FileHash .\mailswiftsync-x86_64-pc-windows-msvc.zip -Algorithm SHA256
Get-Content .\mailswiftsync-x86_64-pc-windows-msvc.zip.sha256
```

3. For a stronger release check, download the release manifest and its
   checksum, then verify both before extracting:

```bash
sha256sum -c mailswiftsync-release-manifest.txt.sha256
sha256sum -c mailswiftsync-rust-sbom.cdx.json.sha256
```

Releases also include `mailswiftsync-image-sbom.spdx.json`, which inventories
the Debian packages in the final headless runtime image. The Rust SBOM covers
Cargo dependencies; the image SBOM covers operating-system packages and their
installed versions. Neither artifact claims source/file provenance for Debian
packages beyond what the package database exposes.

The manifest contains the digest of every release artifact. If the GitHub CLI
is installed, also verify the build provenance attached to the manifest:

```bash
gh attestation verify mailswiftsync-release-manifest.txt \
  --repo itchyitchy123/MailSwiftSync
```

Run the attestation command from the directory containing the downloaded
manifest. It verifies provenance; it does not replace the adjacent checksum
checks.

4. Extract the verified archive. The release archive contains the
  MailSwiftSync binary, README, license, and operator installation guides.

## Configure the migration engine

5. Install the engine required by the migration:
   - arbitrary IMAP-to-IMAP: install `imapsync` from its official distribution;
   - Dovecot destination: install `doveadm` on the destination host.

6. Verify the engine in a terminal (`imapsync --version` or
   `doveadm --version`). MailSwiftSync does not download, update, or configure
   either engine for you.

7. Run the extracted `mailswiftsync` binary. On Windows, run
   `mailswiftsync-x86_64-pc-windows-msvc.exe`.

8. If the engine is not on `PATH`, enter its absolute path in the migration
   plan. Keep **Preflight** selected, test with a representative destination
   mailbox, and only then approve a live pilot.

## Operational suitability

The current release is intended for attended technical-operator pilots on
known endpoints. Keep the dry preflight, live confirmation, and evidence
review gates in place; a service-manager deployment is not equivalent to
unattended production approval. Provider OAuth consent, secret-safe remote
Dovecot execution, and independent message-level reconciliation are not
included yet. Portable release archives are signed when the release signing
environment is configured; native OS-specific installer packages are not
currently published.

On macOS, process identity checks are deliberately conservative. If the
native process query cannot prove that a recorded child is the expected
process, MailSwiftSync refuses to signal it and leaves the work for operator
review. Use a Linux/Unix administration host for high-stakes migration
windows when stronger process supervision is required.

Release artifacts also carry GitHub build provenance and are published with a
release manifest. Windows archives contain Authenticode-signed binaries and
macOS archives contain Developer ID-signed binaries submitted to Apple
notarization; Linux archives have detached GPG signatures for their checksums.
The release signing key fingerprint must be obtained from the organization's
separate trusted channel before `gpg --verify` is meaningful. If `gh
attestation` is unavailable, retain the manifest, signatures, and checksum
files with the installed archive for later independent review.

For Linux headless deployments, see the [container deployment guide](../container.md)
and the [service-manager deployment guide](SERVICE.md).
