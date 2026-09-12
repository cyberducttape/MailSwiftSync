# Changelog

All notable changes to MailSwiftSync are documented here.

## [Unreleased]

### Added

- Added the MIT license and clarified the product identity and control-plane positioning.
- Repositioned the main UI around engine-neutral planning and verification/audit outcomes.

- Added explicit migration engine selection with Dovecot-native `doveadm`/`imapc` execution and an `imapsync` fallback.
- Added local and SSH-based Dovecot destination execution with non-interactive SSH and shell-quoted remote arguments.
- Added Dovecot post-run mailbox reconciliation using folder, message, and virtual-size status.
- Added durable redacted run output, lifecycle events, mailbox states, and verification evidence.
- Added `imapsync` summary parsing for automatic evidence capture.
- Added child-process-only credential delivery for live `imapsync` runs (superseded by protected ephemeral passfiles below).
- Added exportable Markdown verification reports and a tagged-release workflow for Linux, Windows, and macOS artifacts.
- Added explicit `ready`/`Preflight` outcomes for successful dry runs; dry validation no longer masquerades as a completed migration.
- Added conservative evidence scope, live-run preflight/persistence gates, process-group termination on Unix, and transactional project/evidence writes.
- Added abandoned-run recovery and structured run lifecycle records; verification failures now enter operator Attention instead of masquerading as transfer failures.
- Added bounded, cancellable verification subprocesses with Unix process-group termination.
- Batch cancellation now stops subsequent jobs and records cancelled mailbox states.
- Hardened atomic profile writes and endpoint validation, including trimmed ports and malformed IPv6 rejection.
- Added regression coverage for command generation, credential handling, evidence parsing, and verification confidence.
- Added durable batch validation jobs, per-run evidence IDs, bounded execution, and operator cancellation.
- Bound live execution to the exact secret-free plan captured by a successful dry preflight, and require the mailbox job to be explicitly ready.
- Made project creation transactional in the cockpit, reject advanced options that override controlled endpoints or credentials, and redact worker-thread output before it reaches the event journal.
- Added cancellation status to structured run records and expanded exported reports with mailbox state and evidence scope.
- Made batch project and mailbox creation atomic, attached batch validation to a durable run record, and journaled batch activity under its project.
- Restored the selected mailbox identity from durable state and marked interrupted run records as abandoned with recovery events on restart.
- Corrected IMAP capability discovery to consume the server greeting first, honor configured ports, and clearly limit the probe to IMAPS.
- Bounded the in-memory visible execution journal to 10,000 lines while retaining the durable event ledger.
- Persisted secret-free batch row configuration and restored durable batch queues for review after restart; credentials remain session-only.
- Upgraded the GUI/spreadsheet dependency line to remove the reported high-severity `quick-xml` vulnerabilities and added `cargo audit` to CI.
- Batch startup now validates every row before launching any process, preventing restored queues with blank credentials from running.
- Tightened the live project-phase gate to recognized execution and verification phases only.
- Local Dovecot commands and verification now receive source credentials through a child environment variable and Dovecot `$ENV:` expansion; remote SSH exposure remains documented.
- imapsync credentials now use short-lived owner-only passfiles instead of environment variables, with cleanup after process completion.
- imapsync plans now explicitly force encrypted transport for IMAPS/STARTTLS and reject destructive expert flags; stale credential directories are cleaned up after forced termination.
- Credential files are now created atomically with owner-only permissions before any password bytes are written, then flushed before execution.
- Activity now exposes durable run history after restart and offers an explicit safe-retry path that returns failed or interrupted mailboxes to dry preflight.
- Restricted profile and SQLite state files to owner-only permissions on Unix systems.
- Added durable run metadata to exported verification reports so each report is traceable to a specific execution after restart.
- Strengthened crash durability with flushed atomic profile writes, SQLite writer backoff, and rejection of evidence records that reference unknown runs.
- Made direct run completion atomic across run status, mailbox state, and the terminal audit event; persistence failures are now surfaced in the operator journal instead of being silently ignored.
- Project-phase and batch-completion persistence errors are now surfaced to the operator rather than being discarded by the event poller.
- Verification evidence and its derived `verified`/`delta_required` mailbox state are now committed in one transaction; event-poller persistence failures are surfaced instead of silently ignored.
- A reported durability failure can no longer be overwritten by a later “completed successfully” status; the run is explicitly marked for durability review.
- Reworked the README for first-time operators with above-the-fold screenshots, CI/license trust signals, audience guidance, alternative-tool comparison, and honest release/install boundaries.
- Expanded `SECURITY.md` with supported-version policy, credential/transport/persistence boundaries, engine trust assumptions, and a safe vulnerability-reporting checklist.
- Live mailbox starts now atomically create the durable run record and move the job to `running`; persistence failures abort before the migration process is spawned.
- The IMAPS readiness probe now authenticates, refreshes post-auth capabilities, and requests `NAMESPACE` and `LIST` so capability results are not presented as sufficient migration readiness on their own.
- IMAPS readiness now requires tagged `OK` responses for capability, authentication, namespace, and folder-inventory commands; completed `NO`/`BAD` responses are reported as failures.
- Reject control characters in hosts, usernames, and passwords before validation, process execution, or authenticated IMAP probing.
- Batch startup now atomically records the parent run and marks every child job as running before worker execution, so restart recovery cannot lose queued children between UI events.
- Dovecot mode no longer offers the dual-IMAPS readiness probe when the destination has no IMAP credential; native `doveadm` dry preflight is now the explicit destination check.
- Remote Dovecot execution is now disabled by default until secret-broker delivery exists; operators must explicitly acknowledge the current destination process-argument exposure to opt in.
- Bulk imports no longer require password columns; missing credentials can be entered in masked per-row fields, reducing the need to keep plaintext passwords in migration spreadsheets.
- Separated structural bulk-import validation from credential-required execution validation, so passwordless identity imports are accepted but cannot be launched until credentials are supplied.
- Added project-level Markdown verification reports covering every durable mailbox job, evidence scope, confidence, unresolved states, and recent runs.
- Replaced sequential batch validation with a bounded 1–16 worker pool and an operator-selected concurrency setting; cancellation and indexed output remain shared across workers.
- Batched concurrent worker output into one SQLite transaction per UI cycle, retaining redacted durable output while avoiding one synchronous disk transaction per line.

### Changed

- Added Drop-based cleanup guards for prepared secret directories, retaining stale-directory cleanup for forced termination.
- Batch validation retries only classified transient transport failures up to three times with exponential backoff; authentication and configuration failures remain terminal.

- Verification exports are now written atomically, flushed before rename, and restricted to owner-only permissions on Unix.
- Added secret-free, versioned JSON project reports alongside Markdown exports for automation and ticketing workflows.

- Dovecot migrations default to additive `sync -1`; destination mirroring requires explicit destructive configuration.
- Automatic engine selection is conservative and uses `imapsync` unless Dovecot mode is explicitly selected.
- Updated operator, architecture, and security documentation to describe the native Dovecot workflow and its limitations.
- Added explicit source port/TLS controls, including correct Dovecot mapping for plaintext (`imapc_ssl=no`), and stricter CSV/option validation.
- Renamed the product and runtime namespace to MailSwiftSync, while retaining one-time loading of legacy profile files.

### Security

- Passwords remain excluded from saved profiles and the SQLite ledger.
- Dovecot source credentials still use a destination-side `imapc_password` override and may be visible to process inspection; OS-keyring/OAuth delivery remains future work.
