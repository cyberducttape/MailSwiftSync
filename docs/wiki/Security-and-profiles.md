# Profiles, passwords, and security

## What MailSwiftSync saves

Saved profiles contain only non-secret configuration: profile name, hosts, usernames, OS-keyring IDs, imapsync path, checkboxes, and extra options. The profile is stored in the standard operating-system configuration directory under `mailswiftsync/profile.toml`. On Unix, profile and SQLite state files are restricted to owner read/write permissions (`0600`) where the platform permits it.

## What it does not save

MailSwiftSync does not save passwords, sync output, or mailbox contents. Password fields are blank after a restart.

## Credential delivery and process visibility

For imapsync runs, MailSwiftSync writes passwords to short-lived owner-only passfiles and removes their private runtime directory after the child exits. The runtime base uses `XDG_RUNTIME_DIR` when available and otherwise a dedicated system temporary directory; it is owner-only on Unix. After acquiring the application workspace lock and completing startup recovery, MailSwiftSync removes abandoned, namespaced runtime directories older than seven days. If a recorded process identity cannot be verified, cleanup is deferred rather than risking an active passfile. This reduces exposure but cannot guarantee secure deletion on every filesystem. Passwords may be loaded from the OS keyring for the active session; only the keyring ID is saved in the profile. For local Dovecot runs, the source password is passed through `MAILSWIFTSYNC_IMAPC_PASSWORD` and referenced by Dovecot's `$ENV:` expansion, so it is not placed in the local `doveadm` argument list. Remote Dovecot execution is currently unavailable: its compatibility path would use a destination-side `imapc_password` override that can expose the password through remote process inspection. Use local doveadm or imapsync until a secret broker is implemented.

The command preview intentionally redacts passwords, but the preview is not a substitute for host-level process security. OS-keyring references improve operator-managed sessions, but provider OAuth or an equivalent secret broker is still required before describing unattended credential delivery as enterprise-grade for both engines.

The application holds an exclusive lock beside the SQLite database while it is open. This prevents a second MailSwiftSync instance from treating a live first instance as an abandoned owner during startup recovery. If the lock message appears, close the existing window; do not delete the lock file casually. imapsync is also started with `--nolog` so diagnostic output remains under MailSwiftSync’s journal and report policy rather than creating an unmanaged `LOG_imapsync/` file.

## Recommended practice

- Use provider-issued app passwords where available; OAuth/Modern Auth is not yet implemented by MailSwiftSync.
- Plain source IMAP requires an explicit acknowledgement before any authenticated operation, including preflight, because the check still transmits credentials and mailbox protocol traffic without transport encryption.
- Start against a test destination mailbox.
- Keep the source mailbox unchanged during the first migration.
- Export the verification report and project-health summary for the durable audit record; the visible journal is bounded diagnostic context.
- Restrict access to the computer while a live migration is running.
