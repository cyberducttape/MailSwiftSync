# Profiles, passwords, and security

## What Sourcecraft saves

Saved profiles contain only non-secret configuration: profile name, hosts, usernames, imapsync path, checkboxes, and extra options. The profile is stored in the standard operating-system configuration directory under `sourcecraft-imapsync/profile.toml`.

## What it does not save

Sourcecraft does not save passwords, sync output, or mailbox contents. Password fields are blank after a restart.

## Credential delivery and process visibility

For imapsync runs, Sourcecraft writes each password to a short-lived mode-600 passfile and passes only the filename to the child process. The files are removed when the child exits, including startup failures. For Dovecot runs, the source password is currently supplied through the destination-side `imapc_password` override; remote process inspection can therefore expose it. Use SSH keys with `BatchMode=yes`, restrict access to the destination host, and do not run untrusted local software during a migration.

The command preview intentionally redacts passwords, but the preview is not a substitute for host-level process security. No credential mechanism should be described as enterprise-grade until keyring/OAuth or an equivalent secret broker is implemented for both engines.

## Recommended practice

- Use provider-issued app passwords where available.
- Start against a test destination mailbox.
- Keep the source mailbox unchanged during the first migration.
- Save the execution journal externally if you need an audit record.
- Restrict access to the computer while a live migration is running.
