# Security policy

## Reporting a vulnerability

Please do not open a public issue for a suspected vulnerability. Contact the repository owner privately with a concise description, affected version, reproduction steps, and any suggested mitigation.

MailSwiftSync executes the local `imapsync` binary or destination-side `doveadm` command selected in the interface. Treat the executable, its configuration, account endpoints, and any supplied extra options as trusted only when they come from a trusted source.

## Scope

MailSwiftSync does not collect telemetry or retain passwords in its profile file. Passwords are held only while the application is running and passed to the active migration process. Always use dry-run mode before a live migration and use an operating system account with appropriate process visibility controls.
