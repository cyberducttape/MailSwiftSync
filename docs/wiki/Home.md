# Sourcecraft IMAP Migrator Wiki

Sourcecraft is a local migration control plane. It uses Dovecot's native `doveadm`/dsync workflow when the destination is Dovecot, and `imapsync` for arbitrary IMAP-to-IMAP work. It helps you plan a migration, test it safely, keep a durable project record, and verify the result.

![Migration plan screen](assets/migration-plan.png)

> The screenshot is an interface illustration. Field values in documentation are examples only.

## Start here

1. Install `doveadm` on or use an operator-managed wrapper for the Dovecot destination; install `imapsync` for the fallback path.
2. Open Sourcecraft and choose the migration engine.
3. Leave **Dry run** turned on.
4. Enter the source and destination account details.
5. Preview the redacted command and check the servers and usernames.
6. Run validation with a test destination mailbox.
7. Read the execution journal. Only after it succeeds should you disable Dry run and start a live migration.

## Guides

- [Install and first launch](Install-and-first-launch.md)
- [Run a safe migration](Run-a-safe-migration.md)
- [Bulk migrations from CSV or Excel](Bulk-migrations.md)
- [Profiles, passwords, and security](Security-and-profiles.md)
- [Troubleshooting](Troubleshooting.md)
