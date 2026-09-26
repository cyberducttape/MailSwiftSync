# MailSwiftSync 0.1.0-alpha

This is a technical preview for administrators who want to test the local-first migration workflow with disposable or fully backed-up mailboxes. It is not a production-support commitment.

## Included

- Dovecot-native `doveadm`/`dsync` planning and execution when destination administration is available.
- `imapsync` fallback for arbitrary IMAP endpoints.
- Dry-run-first planning, certificate-verified authenticated IMAPS/STARTTLS readiness checks, bounded validation concurrency, cancellation, and transient validation retries.
- Durable SQLite project state, crash recovery to operator Attention, redacted event history, and Markdown/JSON verification reports.
- Portable Linux x86_64, Windows x86_64, and macOS arm64/x86_64 archives with SHA-256 checksums; configured release workflows also provide platform signing/notarization and provenance artifacts.
- A deterministic host-native package path with checksum verification via
  `scripts/package.sh` and `scripts/verify-release.sh`.

## Known limitations

- Native installers are not included. Portable release signing/notarization is performed when release credentials are configured. imapsync supports operator-supplied OAuth 2.0/XOAUTH2 access tokens through the session form or OS keyring; automatic refresh from an operator-supplied refresh token is supported, while provider consent flows and unattended secret brokering are not included.
- Remote Dovecot execution is disabled because the compatibility path may expose a password through destination-host process inspection.
- Live bulk execution is available only after matching dry validation and explicit operator confirmation; a foreground supervisor supports bounded retries and optional maintenance windows, but scheduler/API operation and unattended operation are not supported.
- Native Dovecot runs provide aggregate evidence, while encrypted imapsync runs also perform bounded metadata-level message reconciliation (folder, Message-ID, INTERNALDATE, RFC822.SIZE, and UIDVALIDITY-aware mailbox identity). This is not body-content proof. Dovecot live runs persist and reuse engine-emitted `-s` state strings as a resumability optimization, atomically with each child result.

Use the [release-readiness criteria](release-readiness.md) and [security policy](../SECURITY.md) before evaluating a migration for customer production use. Report security issues privately as described in `SECURITY.md`.
