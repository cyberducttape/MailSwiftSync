# MailSwiftSync Wiki

MailSwiftSync is a local mailbox migration control plane. It uses Dovecot's native `doveadm`/dsync workflow when the destination is Dovecot, and `imapsync` for arbitrary IMAP-to-IMAP work. It helps you plan a migration, test it safely, keep a durable project record, and verify the result.

![Illustrative migration plan screen](assets/migration-plan.png)

> **Documentation illustration:** this image is a polished workflow mockup, not a pixel-accurate capture of the current egui application. Field values are examples only. The shipped interface is intentionally utilitarian and its current controls are documented below.

## Start here

1. Install `doveadm` on or use an operator-managed wrapper for the Dovecot destination; install `imapsync` for the fallback path.
2. Open MailSwiftSync and choose the migration engine.
3. Leave **Preflight** selected.
4. Enter the source and destination account details.
5. Preview the redacted command and check the servers and usernames.
6. Run validation with a test destination mailbox.
7. Confirm the mailbox is **Ready** and the preflight plan still matches the current fields. Only then should you select Live migration; changing endpoints, users, engine, TLS, ports, or controlled options requires another preflight.

## Choose the right engine

- **Dovecot native** is the recommended path when the destination is Dovecot and you have destination-side administrative access. MailSwiftSync plans `doveadm`/dsync with an `imapc` source.
- **imapsync fallback** is for arbitrary IMAP-to-IMAP migrations where destination-native tooling is unavailable.

MailSwiftSync is the planning, execution, verification, and audit layer around those engines. It does not replace their server-side semantics or bundle a hosted migration service.

## Recovery and evidence

Projects, mailbox states, run IDs, redacted events, and verification evidence are stored in a local SQLite ledger. Interrupted jobs reopen in **Attention** for review rather than being treated as successful. Imported batch queues restore their secret-free configuration after restart, but passwords are never persisted and must be entered again.

Verification currently distinguishes engine-confirmed imapsync summaries from aggregate Dovecot folder/message/size totals. Aggregate totals are useful reconciliation evidence but are not message-level proof; unresolved or incomplete evidence remains pending review.

## Guides

- [Install and first launch](Install-and-first-launch.md)
- [Run a safe migration](Run-a-safe-migration.md)
- [Production migration runbook](Production-runbook.md)
- [Bulk migrations from CSV or Excel](Bulk-migrations.md)
- [Profiles, passwords, and security](Security-and-profiles.md)
- [Troubleshooting](Troubleshooting.md)
