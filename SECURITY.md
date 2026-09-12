# Security policy

## Reporting a vulnerability

Please do not open a public issue for a suspected vulnerability. Contact the repository owner privately with a concise description, affected version, reproduction steps, and any suggested mitigation.

If private contact is unavailable, use a GitHub private vulnerability report for this repository. Please allow reasonable time for acknowledgement before publishing details.

## Supported versions

Only the latest released version and the current `main` branch receive security fixes. The 0.1 development line is not a promise of unattended production safety; review the release-readiness criteria before using it for customer migrations.

MailSwiftSync executes the local `imapsync` binary or destination-side `doveadm` command selected in the interface. Treat the executable, its configuration, account endpoints, and any supplied extra options as trusted only when they come from a trusted source.

## Scope

MailSwiftSync does not collect telemetry or retain passwords in its profile file. Passwords are held only while the application is running and passed to the active migration process. Always use dry-run mode before a live migration and use an operating system account with appropriate process visibility controls.

## Security boundaries

- **Credential lifetime:** imapsync passwords are written to short-lived owner-only passfiles and removed after the child exits. Local Dovecot runs use a child environment variable. Remote Dovecot execution remains an explicit opt-in because the current compatibility path can expose the source password through process inspection on the destination host.
- **Transport:** imapsync plans explicitly select IMAPS or STARTTLS according to the plan. A plain source is an explicit warning, not a verified secure plan. The Rustls readiness probe validates certificates only for the IMAPS probe path; it does not magically enforce TLS behavior in an externally supplied engine or wrapper.
- **Persistence:** profiles, SQLite state, event output, reports, and diagnostics must remain secret-free. Output is redacted before it reaches the visible journal or durable event ledger, but operators must still treat the host, engine executable, imported spreadsheets, and crash/debug tooling as trusted infrastructure.
- **Destructive actions:** destination deletion and expert options are controlled and require deliberate review. Do not run MailSwiftSync or an engine wrapper from an untrusted account, and do not grant it broader destination access than the migration requires.
- **Engine trust:** MailSwiftSync orchestrates `imapsync`, `doveadm`, SSH, and their configuration; it does not audit or sandbox those programs. Verify engine provenance and versions independently.

## Reporting checklist

Include the affected MailSwiftSync version or commit, operating system, selected engine, whether execution was local or remote, sanitized configuration details, reproduction steps, and whether any credential or mailbox data may have been exposed. Never attach real passwords, mailbox content, or unredacted logs.
