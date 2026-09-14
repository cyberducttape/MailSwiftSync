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
```

On macOS:

```bash
shasum -a 256 -c mailswiftsync-aarch64-apple-darwin.tar.gz.sha256
```

On Windows PowerShell, compare the displayed digest with the value in the
adjacent `.sha256` file:

```powershell
Get-FileHash .\mailswiftsync-x86_64-pc-windows-msvc.zip -Algorithm SHA256
Get-Content .\mailswiftsync-x86_64-pc-windows-msvc.zip.sha256
```

3. Extract the verified archive. The release archive contains the
   MailSwiftSync binary, README, license, and operator installation guides.

## Configure the migration engine

4. Install the engine required by the migration:
   - arbitrary IMAP-to-IMAP: install `imapsync` from its official distribution;
   - Dovecot destination: install `doveadm` on the destination host.

5. Verify the engine in a terminal (`imapsync --version` or
   `doveadm --version`). MailSwiftSync does not download, update, or configure
   either engine for you.

6. Run the extracted `mailswiftsync` binary. On Windows, run
   `mailswiftsync-x86_64-pc-windows-msvc.exe`.

7. If the engine is not on `PATH`, enter its absolute path in the migration
   plan. Keep **Preflight** selected, test with a representative destination
   mailbox, and only then approve a live pilot.

## Operational suitability

The current release is intended for attended technical-operator pilots on
known endpoints. Keep the dry preflight, live confirmation, and evidence
review gates in place; a service-manager deployment is not equivalent to
unattended production approval. Provider OAuth consent/refresh, secret-safe
remote Dovecot execution, independent message-level reconciliation, and
signed native installers are not included yet.

On macOS, process identity checks are deliberately conservative. If the
native process query cannot prove that a recorded child is the expected
process, MailSwiftSync refuses to signal it and leaves the work for operator
review. Use a Linux/Unix administration host for high-stakes migration
windows when stronger process supervision is required.

Release artifacts also carry GitHub build provenance and are published with a
release manifest. Native installers and platform code-signing/notarization are
not published yet; the checksum and provenance checks above are the release
verification path for portable archives.

For Linux headless deployments, see the [container deployment guide](../container.md)
and the [service-manager deployment guide](SERVICE.md).
