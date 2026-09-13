# MailSwiftSync installation

MailSwiftSync is the migration control plane. It does not bundle an IMAP
engine or configure a mail server.

1. Install the engine required by the migration:
   - arbitrary IMAP-to-IMAP: install `imapsync` from its official distribution;
   - Dovecot destination: install `doveadm` on the destination host.
2. Verify the engine in a terminal (`imapsync --version` or `doveadm --version`).
3. Extract this archive and run the `mailswiftsync` binary. On Windows, run
   `mailswiftsync-x86_64-pc-windows-msvc.exe`.
4. If the engine is not on `PATH`, enter its absolute path in the migration
   plan. Start with a test destination and **Preflight** selected.

The archive includes the project README and the first-launch guide. Verify the
adjacent `.sha256` file before use. Release artifacts also carry GitHub build
provenance and are published with the release manifest.
