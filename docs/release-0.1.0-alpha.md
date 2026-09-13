# MailSwiftSync 0.1.0-alpha

This is a technical preview for administrators who want to test the local-first migration workflow with disposable or fully backed-up mailboxes. It is not a production-support commitment.

## Included

- Dovecot-native `doveadm`/`dsync` planning and execution when destination administration is available.
- `imapsync` fallback for arbitrary IMAP endpoints.
- Dry-run-first planning, certificate-verified authenticated IMAPS/STARTTLS readiness checks, bounded validation concurrency, cancellation, and transient validation retries.
- Durable SQLite project state, crash recovery to operator Attention, redacted event history, and Markdown/JSON verification reports.
- Portable Linux x86_64, Windows x86_64, and macOS arm64/x86_64 archives with SHA-256 checksums.

## Known limitations

- Native installers, code signing, provider OAuth/Modern Auth, and unattended secret brokering are not included. Optional OS-keyring password references are included for local operator-managed sessions.
- Remote Dovecot credential delivery is an explicit opt-in because the compatibility path may expose a password through destination-host process inspection.
- Live bulk execution is available only after matching dry validation and explicit operator confirmation; scheduler/API operation, maintenance windows, and unattended operation are not supported.
- Verification is aggregate evidence unless an engine supplies authoritative results; message-level reconciliation and UIDVALIDITY-aware evidence are not yet implemented. Dovecot live runs do persist and reuse engine-emitted `-s` state strings as a resumability optimization, atomically with each child result.

Use the [release-readiness criteria](release-readiness.md) and [security policy](../SECURITY.md) before evaluating a migration for customer production use. Report security issues privately as described in `SECURITY.md`.
