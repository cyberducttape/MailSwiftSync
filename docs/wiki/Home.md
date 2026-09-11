# Sourcecraft IMAP Migrator Wiki

Sourcecraft is a local desktop front end for the `imapsync` command. It helps you plan a mailbox migration, test it safely, and then start the real transfer.

![Migration plan screen](assets/migration-plan.png)

> The screenshot is an interface illustration. Field values in documentation are examples only.

## Start here

1. Install `imapsync` on the computer where Sourcecraft runs.
2. Open Sourcecraft and leave **Dry run** turned on.
3. Enter the source and destination account details.
4. Click **Preview redacted command** and check the servers and usernames.
5. Run **Dry validation** with a test destination mailbox.
6. Read the execution journal. Only after it succeeds should you disable Dry run and start a live migration.

## Guides

- [Install and first launch](Install-and-first-launch.md)
- [Run a safe migration](Run-a-safe-migration.md)
- [Bulk migrations from CSV or Excel](Bulk-migrations.md)
- [Profiles, passwords, and security](Security-and-profiles.md)
- [Troubleshooting](Troubleshooting.md)
