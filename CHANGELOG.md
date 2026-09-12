# Changelog

All notable changes to MailSwiftSync are documented here.

## [Unreleased]

### Added

- Project phase recovery from `Attention` to `Preflight` or `Verification` now works as intended, and phase changes plus their audit events commit atomically.
- Updated user-facing troubleshooting and architecture language to consistently call imapsync output engine-confirmed rather than authoritative, and to distinguish a successful process exit from verified completion.
- Live batch promotion now validates every child plan fingerprint inside the same SQLite transaction that marks the batch running, preventing a partially edited or stale queue from starting.
- Verification exports now use explicit evidence levels and identify engine-confirmed versus aggregate evidence; compatibility percentages are labelled as internal comparison metrics rather than probabilities. Durable project queries also have indexes for mailbox state, run history, events, evidence history, and active process lookup.
- Secret runtime fallback now uses the system temporary directory rather than persistent application data, enforces owner-only Unix permissions, and performs stale-run cleanup at application startup as well as before a new run.
- Persistent state now creates and enforces an owner-only database directory on Unix, containing SQLite sidecars as well as the primary database file.
- Process recovery now records Linux start-time, session, and process-group identity and refuses to signal a recorded PID when those values cannot be proven to match.
- Batch run completion now updates the terminal run status and clears active process identities in one SQLite transaction.
- Dovecot dry validation now checks both sides: the remote `imapc` source plus destination user and mailbox-list readiness, for single and batch validation runs.
- Added explicit Dovecot execution-location choices for local or SSH invocation; automatic hostname inference remains available only for legacy compatibility.
- Added durable active-process records and startup reconciliation so recorded Unix migration process groups are terminated before interrupted runs become retryable operator-review jobs.
- imapsync execution now requests certificate verification for both encrypted endpoints (`SSL_verify_mode=1`); expert options cannot override the transport policy.
- Added bounded redacted stdout/stderr tails to process failures so failure classification and transient retry decisions receive the engine's actual diagnostic output.
- Added five-second graceful shutdown before process-group escalation, Linux parent-death signalling, and retry-safe handling of timeout/cancellation cleanup.
- Aggregate evidence mismatches now report zero compatibility confidence instead of the misleading 85% fallback; exact aggregate matches remain explicitly labelled as aggregate rather than message-level proof.
- IMAP readiness now tries every resolved address before reporting connectivity failure, and bulk imports preserve password whitespace exactly.
- Removed the superseded non-terminal evidence persistence API after atomic terminal completion became the only supported path.
- Promoted the durable batch queue from validation-only to gated live execution after matching dry-validation fingerprints and explicit operator confirmation.
- Added elapsed-time running feedback, Escape-key cancellation, dark/light theme switching, and contextual help for performance controls.
- Added explainable evidence levels to complement the internal confidence percentage in the verification UI and architecture model.
- IMAP readiness now treats missing NAMESPACE support as compatible while parsing capabilities strictly from the post-auth response.
- Clarified that Automatic engine selection is a conservative imapsync default, not server-environment autodetection.
- Added an atomic evidence-backed terminal completion path so successful live verification can move `running` to `verified` without violating ordinary mailbox state transitions.
- Added a persistent lifecycle stepper to the main workspace, password show/hide controls, and inline account-field validation for common input errors.
- Clarified in the README and wiki that current UI images are workflow illustrations rather than pixel-accurate application screenshots.
- Batch workers now claim immutable jobs through an atomic index instead of contending on a mutex-protected iterator.
- Added a configurable 1–720 hour per-process timeout for large mailbox migrations.
- Added explicit imapsync message/byte-per-second throttles for provider-friendly runs.
- Pinned CI and release builds to Rust 1.92.0 so linting and published artifacts use a reproducible toolchain.
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
- Added package metadata and a deliberately marked technical-preview release note covering artifact coverage and known production limitations.
- Added optional OS-keyring credential references with session-only loading, deletion controls, and documentation that distinguishes keyring password storage from OAuth and unattended secret brokering.
- Added stable failure taxonomy for operator action and retry policy; only transport/throttling failures are classified as transient, while run details retain the failure class.
- Bound the exact preflight fingerprint to non-secret OS-keyring credential references so changing the credential source requires a fresh preflight.
- Added cross-platform release-target compilation to pull-request CI, covering Linux, Windows, and both macOS architectures instead of waiting for a tag build to catch platform regressions.
- Closed the IMAP probe validation bypass: capability discovery now validates form input and its quoting helper rejects control characters defensively; redundant secret-directory cleanup was removed from worker execution.
- Clarified that the imapsync text-summary `unmatched_messages` value is a proof-pending sentinel, not a literal unresolved-message count.
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
- Dovecot source credentials still use a destination-side `imapc_password` override for remote execution and may be visible to process inspection; provider OAuth and unattended secret brokering remain future work.
