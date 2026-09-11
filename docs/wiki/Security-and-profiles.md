# Profiles, passwords, and security

## What Sourcecraft saves

Saved profiles contain only non-secret configuration: profile name, hosts, usernames, imapsync path, checkboxes, and extra options. The profile is stored in the standard operating-system configuration directory under `sourcecraft-imapsync/profile.toml`.

## What it does not save

Sourcecraft does not save passwords, sync output, or mailbox contents. Password fields are blank after a restart.

## Process visibility

imapsync receives account passwords for the active run. Use a private operating-system account and do not run untrusted local software during a migration. The command preview intentionally redacts passwords, but the preview is not a substitute for host-level process security.

## Recommended practice

- Use provider-issued app passwords where available.
- Start against a test destination mailbox.
- Keep the source mailbox unchanged during the first migration.
- Save the execution journal externally if you need an audit record.
- Restrict access to the computer while a live migration is running.
