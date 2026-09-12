# Profiles, passwords, and security

## What MailSwiftSync saves

Saved profiles contain only non-secret configuration: profile name, hosts, usernames, imapsync path, checkboxes, and extra options. The profile is stored in the standard operating-system configuration directory under `mailswiftsync/profile.toml`. On Unix, profile and SQLite state files are restricted to owner read/write permissions (`0600`) where the platform permits it.

## What it does not save

MailSwiftSync does not save passwords, sync output, or mailbox contents. Password fields are blank after a restart.

## Credential delivery and process visibility

For imapsync runs, MailSwiftSync writes passwords to short-lived owner-only passfiles and removes their private runtime directory after the child exits. Abandoned MailSwiftSync runtime directories older than seven days are removed before a new run, covering forced application termination. For local Dovecot runs, the source password is passed through `MAILSWIFTSYNC_IMAPC_PASSWORD` and referenced by Dovecot's `$ENV:` expansion, so it is not placed in the local `doveadm` argument list. Remote SSH runs still use a destination-side `imapc_password` override because SSH environment forwarding is deployment-dependent; remote process inspection can therefore expose it. Use SSH keys with `BatchMode=yes`, restrict access to the destination host, and do not run untrusted local software during a migration.

The command preview intentionally redacts passwords, but the preview is not a substitute for host-level process security. No credential mechanism should be described as enterprise-grade until keyring/OAuth or an equivalent secret broker is implemented for both engines.

## Recommended practice

- Use provider-issued app passwords where available.
- Start against a test destination mailbox.
- Keep the source mailbox unchanged during the first migration.
- Save the execution journal externally if you need an audit record.
- Restrict access to the computer while a live migration is running.
