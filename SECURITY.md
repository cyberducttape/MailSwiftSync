# Security policy

## Reporting a vulnerability

Please do not open a public issue for a suspected vulnerability. Contact the repository owner privately with a concise description, affected version, reproduction steps, and any suggested mitigation.

If private contact is unavailable, use a GitHub private vulnerability report for this repository. Please allow reasonable time for acknowledgement before publishing details.

## Supported versions

Only the latest released version and the current `main` branch receive security fixes. The 0.1 development line is not a promise of unattended production safety; review the release-readiness criteria before using it for customer migrations.

MailSwiftSync executes the local `imapsync` binary or destination-side `doveadm` command selected in the interface. Treat the executable, its configuration, account endpoints, and any supplied extra options as trusted only when they come from a trusted source.

## Scope

MailSwiftSync does not collect telemetry or retain passwords in its profile file. Secret values are held in zeroizing memory wrappers where possible. imapsync credentials are also written to owner-only temporary passfiles for the child process; normal cleanup removes them after the child exits. A hard kill, operating-system crash, or power loss can leave those files on disk after MailSwiftSync exits. On the next startup, while holding the exclusive state lock, MailSwiftSync reconciles the durable process ledger: when every recorded process is confirmed gone, leftover run directories are removed immediately. If any process identity is ambiguous, immediate cleanup is skipped and only directories older than seven days are eligible for age-based cleanup. If the durable ledger cannot be read, startup cleanup is blocked. Always use dry-run mode before a live migration and use an operating system account with appropriate process visibility controls.

## Recent fixes

- **State directory permissions (9e37e20):** Fixed a directory permission mutation vulnerability where arbitrary state paths could cause MailSwiftSync to chmod pre-existing system directories. Now only restricts permissions on directories MailSwiftSync creates; pre-existing directories are verified writable but never modified.

## Security boundaries

- **Credential lifetime:** imapsync passwords are written to owner-only run directories. Normal child exit removes them; crash residue is reconciled at startup as described above. The seven-day fallback applies only when process ownership is ambiguous; it is not a claim that disk residue cannot survive an application crash. Local Dovecot runs use a child environment variable. Remote Dovecot execution is unavailable because the current compatibility path could expose the source password through process inspection on the destination host; it must not be enabled through an expert flag or wrapper.
- **State directory ownership:** Explicit state paths undergo permission validation only if the parent directory doesn't already exist. Pre-existing parent directories are verified writable but their permissions are not modified, preventing accidental chmod of system directories.
- **Transport:** imapsync plans explicitly select IMAPS or STARTTLS according to the plan. A plain source is an explicit warning, not a verified secure plan. The Rustls readiness probe validates certificates on both the IMAPS and STARTTLS paths; it does not magically enforce TLS behavior in an externally supplied engine or wrapper.
- **OAuth refresh:** an optional automatic-refresh configuration (token endpoint, client ID/secret, refresh token) is stored as one JSON blob under its own OS-keyring service, distinct from the plain password/access-token entries. Refreshing only ever opens a certificate-validated `https://` connection to the operator-configured token endpoint, built with the same Rustls/WebPKI stack as the IMAP readiness probe; a non-`https://` endpoint is rejected outright. MailSwiftSync does not implement an OAuth consent/authorization flow; the operator must obtain the initial refresh token through the provider's own tooling and is responsible for that application's registered scope and redirect configuration.
- **Persistence:** profiles, SQLite state, event output, reports, and diagnostics must remain secret-free. Output is redacted before it reaches the visible journal or durable event ledger, but operators must still treat the host, engine executable, imported spreadsheets, and crash/debug tooling as trusted infrastructure.
- **Destructive actions:** destination deletion and expert options are controlled and require deliberate review. Do not run MailSwiftSync or an engine wrapper from an untrusted account, and do not grant it broader destination access than the migration requires.
- **Engine trust:** MailSwiftSync orchestrates `imapsync`, `doveadm`, SSH, and their configuration; it does not audit or sandbox those programs. Verify engine provenance and versions independently.

## Reporting checklist

Include the affected MailSwiftSync version or commit, operating system, selected engine, whether execution was local or remote, sanitized configuration details, reproduction steps, and whether any credential or mailbox data may have been exposed. Never attach real passwords, mailbox content, or unredacted logs.
