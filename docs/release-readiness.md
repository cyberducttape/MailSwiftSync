# Release readiness

MailSwiftSync should earn a stable 1.0 label through evidence, not feature count.

## Available in 0.1

- Explicit Dovecot-native and imapsync engine paths.
- Dry-run default and explicit confirmation before destination changes.
- Durable projects, mailbox states, lifecycle events, run IDs, and redacted output.
- CSV/XLS/XLSX validation queue with bounded 1–16 worker concurrency.
- Bounded transient retries for validation; authentication and configuration failures stop without retry loops.
- Aggregate source/destination folder, message, and virtual-size evidence.
- Timeout, cancellation, child-process-only imapsync passfile credentials, and documented Dovecot process-visibility limits.

## Required before calling it production-ready

- Provider-specific OAuth/Modern Auth and equivalent unattended secret-broker delivery; no password command-line exposure in the Dovecot path. Local OS-keyring password references are available, but remote Dovecot execution is currently opt-in because its compatibility path can expose the password through destination-host process inspection.
- Signed installers for Linux, Windows, and macOS, with checksums and reproducible release instructions.
- A compatibility matrix covering Dovecot versions, common hosted IMAP providers, TLS modes, folder namespaces, and authentication methods.
- Preflight checks for DNS, TCP/TLS, authentication, quotas, source size, folder inventory, special-use folders, and destination readiness.
- Explicit retry/resume/delta semantics with UIDVALIDITY-aware checkpoints and idempotent recovery after interruption.
- Bounded concurrency, throttling, maintenance windows, and a scheduler/API that can survive the desktop closing.
- Message-level mismatch reporting and exportable per-mailbox and batch reports in Markdown and JSON.
- Published migration evidence from representative datasets, including failures and recovery results.
- Integration tests using disposable IMAP/Dovecot environments in CI or a documented reproducible harness.
- A clean `cargo audit` result for vulnerabilities; unmaintained transitive dependencies must be tracked and reviewed before each release.

## Proposed eight-week sequence

1. Week 1: finalize identity, licensing, release metadata, and a compatibility/test matrix.
2. Week 2: implement keyring-backed credential providers (available for operator-managed sessions); next, add provider OAuth and remove Dovecot process-argument secrets.
3. Week 3: split preflight into DNS, transport, TLS, auth, quota, inventory, and readiness checks.
4. Week 4: add resumable checkpoints, retries, delta status, and explicit mailbox state transitions.
5. Week 5: add bounded concurrency, throttling, scheduling, and durable cancellation/recovery.
6. Week 6: build exportable verification reports and message-level mismatch diagnostics.
7. Week 7: publish installers, signed artifacts, checksums, and upgrade/rollback guidance.
8. Week 8: run documented large-scale migrations, publish results, and decide whether the 1.0 criteria are met.

## Current dependency audit

CI runs `cargo audit`. The current dependency graph has no reported vulnerabilities; RustSec reports only the unmaintained transitive crates `paste` and `ttf-parser`, which come from the desktop GUI stack and remain tracked for upstream replacement.
