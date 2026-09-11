# Security policy

## Reporting a vulnerability

Please do not open a public issue for a suspected vulnerability. Contact the repository owner privately with a concise description, affected version, reproduction steps, and any suggested mitigation.

Forgepad executes the local Git binary selected in its Settings. Treat repository paths, Git configuration, hooks, and remotes as trusted only when they come from a trusted source.

## Scope

Forgepad does not collect telemetry or retain Git credentials. It delegates authentication to the operating system and the installed Git client. Audit logs are local and redact commit messages.
