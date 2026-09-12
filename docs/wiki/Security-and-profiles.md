# Profiles, passwords, and security

## What MailSwiftSync saves

Saved profiles contain only non-secret configuration: profile name, hosts, usernames, OS-keyring IDs, imapsync path, checkboxes, and extra options. The profile is stored in the standard operating-system configuration directory under `mailswiftsync/profile.toml`. On Unix, profile and SQLite state files are restricted to owner read/write permissions (`0600`) where the platform permits it.

## What it does not save

MailSwiftSync does not save passwords, sync output, or mailbox contents. Password fields are blank after a restart.

## Credential delivery and process visibility

For imapsync runs, MailSwiftSync writes passwords to short-lived owner-only passfiles and removes their private runtime directory after the child exits. The runtime base uses `XDG_RUNTIME_DIR` when available and otherwise a dedicated system temporary directory; it is owner-only on Unix. Abandoned MailSwiftSync runtime directories older than seven days are removed at application startup and before a new run, covering forced application termination. This reduces exposure but cannot guarantee secure deletion on every filesystem. Passwords may be loaded from the OS keyring for the active session; only the keyring ID is saved in the profile. For local Dovecot runs, the source password is passed through `MAILSWIFTSYNC_IMAPC_PASSWORD` and referenced by Dovecot's `$ENV:` expansion, so it is not placed in the local `doveadm` argument list. Remote Dovecot runs are disabled by default: their compatibility path uses a destination-side `imapc_password` override and remote process inspection can expose it. An explicit acknowledgement is required to opt in on a trusted destination. Use SSH keys with `BatchMode=yes`, restrict access to the destination host, and do not run untrusted local software during a migration.

The command preview intentionally redacts passwords, but the preview is not a substitute for host-level process security. OS-keyring references improve operator-managed sessions, but provider OAuth or an equivalent secret broker is still required before describing unattended credential delivery as enterprise-grade for both engines.

The application holds an exclusive lock beside the SQLite database while it is open. This prevents a second MailSwiftSync instance from treating a live first instance as an abandoned owner during startup recovery. If the lock message appears, close the existing window; do not delete the lock file casually. imapsync is also started with `--nolog` so diagnostic output remains under MailSwiftSync’s journal and report policy rather than creating an unmanaged `LOG_imapsync/` file.

## Recommended practice

- Use provider-issued app passwords where available; OAuth/Modern Auth is not yet implemented by MailSwiftSync.
- Start against a test destination mailbox.
- Keep the source mailbox unchanged during the first migration.
- Save the execution journal externally if you need an audit record.
- Restrict access to the computer while a live migration is running.
