# Release readiness

MailSwiftSync should earn a stable 1.0 label through evidence, not feature count.

## Completed in the current hardening pass

- Remote Dovecot password-in-argv execution is rejected rather than exposed by
  an operator acknowledgement.
- Endpoint parsing rejects malformed explicit ports instead of treating them as
  hostnames.
- Persistent profile and state directories fail closed when owner-only
  permissions cannot be applied.
- Host-native packaging uses locked dependencies, deterministic tar metadata,
  checksum verification, and GitHub build-provenance attestations.
- Windows engine children are attached to a Job Object configured with
  kill-on-close semantics, so controller exit does not leave an unowned
  descendant tree. macOS still uses the conservative no-signal fallback.
- CI and tagged releases generate a CycloneDX 1.5 SBOM from the locked Cargo
  dependency graph and publish it with its own checksum. The SBOM serial and
  timestamp are reproducible from `Cargo.lock` and `SOURCE_DATE_EPOCH`.
- The cross-platform CI matrix runs the full locked test suite on Linux,
  Windows, and macOS targets in addition to compiling release binaries. This
  is platform runtime coverage for shared behavior; native process-supervision
  tests still need to be added for Windows Job Objects and the macOS fallback.
- A compatibility-matrix release gate and verification script are checked into
  the repository.
- Durable attention reasons now survive restart and are included in Markdown
  and proof-wrapped JSON reports; `mailswiftsync backup <state.db> <backup.db>`
  creates a locked, non-overwriting, SQLite-integrity-checked ledger backup.
- `mailswiftsync restore <backup.db> <state.db>` validates a current-schema,
  readable SQLite backup before installation and preserves an existing state
  database as a uniquely named rollback artifact.
- Authentication, transport, quota, policy, configuration, interruption, and
  process-identity failure categories are persisted for failed/cancelled and
  operator-review mailbox states; ordinary process-exit text is not mistaken
  for an ownership failure.
- Opening an existing older on-disk schema now creates a unique,
  integrity-checked `state.db.pre-migrate-vN.<id>.db` backup before migration;
  new and in-memory databases are not copied.
- Dovecot checkpoint extraction accepts unpadded printable state tokens, and
  semantic Dovecot verification continues when only the bounded diagnostic
  transcript is truncated.
- Authenticated IMAP readiness probes require an untagged folder inventory,
  count discovered folders, and surface observed SPECIAL-USE annotations;
  tagged `LIST` completion alone cannot pass discovery.
- Verification differences have a durable, audited `Verified with exceptions`
  workflow with operator, reason, timestamp, and evidence-run linkage.
- Machine-readable proof exports include the complete durable run manifest;
  activity views remain intentionally bounded.
- Proof artifacts support Ed25519 signatures with owner-only PKCS#8 key files
  and verifier trust pins; unsigned exports are explicitly labeled
  integrity-only.
- The Verification workspace exports a separate customer-safe JSON proof that
  omits endpoints, credential references, plan snapshots, executable paths,
  and diagnostic detail while retaining the complete run metadata and evidence
  digests. The operator project report remains the forensic artifact.
- A headless `support-bundle <state.db> <output.json>` export provides a
  sanitized incident artifact containing platform/schema/health facts without
  credentials, endpoints, plans, command paths, mailbox content, or diagnostic
  text.
- A headless `customer-proof <state.db> <output.json>` export uses the same
  redacted customer artifact as the GUI, so automation can produce a proof
  without depending on a file dialog. The result remains unsigned until an
  approved Ed25519 signing key is applied with `sign`.
- A foreground `supervise <state.db>` controller can continuously watch and
  process automation-safe durable batch work while leaving operator-review
  rows untouched; external service-manager integration and scheduling policy
  are still deployment responsibilities.
- Run metadata records the version string returned by each engine when its
  executable supports `--version`; unavailable versions remain explicit in
  exports rather than being guessed.
- A reproducible Linux engine lab starts two disposable Dovecot servers,
  performs a real imapsync transfer, and checks the destination fixture. This
  is engine-level coverage. It also asserts that invalid source credentials
  fail. The lab runs on pull requests as well as through the scheduled/manual
  CI workflow; runners without the pinned Dovecot 2.4.x fixture skip
  explicitly. Controller crash/restart
  and storage-fault chaos coverage is still required before unattended
  production use.
- Headless `status` and `recover` commands expose secret-free durable state and
  reuse the GUI's fail-closed process recovery path. They are control-plane
  primitives, and `headless preflight|live` now drives the existing controller
  without a window. `headless live` always performs a fresh preflight before
  promotion and refuses restored batch queues. `headless batch-preflight|batch-live`
  drives a complete durable queue without silently narrowing its scope; a
  long-lived scheduler/supervisor is still required for overnight unattended
  operation.
- A Linux headless `Dockerfile` provides explicit durable-state and per-user
  runtime volumes, non-root execution, and packaged `imapsync`/`doveadm`
  dependencies. CI builds the image and smoke-tests its version command;
  official registry publication and image signing remain release gates.

## Available in 0.1

- Explicit Dovecot-native and imapsync engine paths.
- Dry-run default and explicit confirmation before destination changes.
- Durable projects, mailbox states, lifecycle events, run IDs, and redacted output.
- CSV/XLS/XLSX validation queue with bounded 1–16 worker concurrency.
- Bounded transient retries for validation; authentication and configuration failures stop without retry loops.
- Live batch waves with mailbox-specific child runs, claim-before-launch, selective retry scopes, transactional plan checks, and per-mailbox aggregate/engine-dependent evidence.
- Aggregate source/destination folder, message, and virtual-size evidence.
- Timeout, cancellation, child-process-only imapsync passfile credentials, fresh dual-IMAPS authentication before live imapsync launches, and documented Dovecot process-visibility limits.

## Required before calling it production-ready

- Provider-specific OAuth/Modern Auth and equivalent unattended secret-broker delivery. Remote Dovecot execution is deliberately disabled until the application has a delivery mechanism that cannot expose the password through destination-host process inspection.
- Signed installers for Linux, Windows, and macOS, with checksums and reproducible release instructions. Artifact provenance exists, but native installer signing/notarization is still outstanding.
- A compatibility matrix covering Dovecot versions, common hosted IMAP providers, TLS modes, folder namespaces, and authentication methods.
- Preflight checks for DNS, TCP/TLS, authentication, folder inventory, and
  observed special-use folders are implemented for the authenticated IMAP
  probe. Quota capacity, source-size forecasting, and provider-specific
  destination readiness remain explicit unknowns where the server cannot
  provide a reliable query.
- Explicit retry/resume/delta semantics with idempotent recovery after interruption. Dovecot checkpoints remain engine resume tokens, not UIDVALIDITY-aware message proof.
- Bounded concurrency, throttling, maintenance windows, and a scheduler/API that can survive the desktop closing.
- Independent message-level mismatch reporting and reconciliation; current reports are aggregate/engine evidence plus durable exception acceptance.
- Published migration evidence from representative datasets, including failures and recovery results.
- Controller-level integration and chaos tests using disposable IMAP/Dovecot environments, including process kill, GUI restart, storage failure, retry, and evidence recovery.
- A clean `cargo audit` result for vulnerabilities; unmaintained transitive dependencies must be tracked and reviewed before each release.
- CI and release workflows should keep third-party GitHub Actions pinned to reviewed commit SHAs; update pins deliberately with the corresponding release version documented in a comment.

The test-gate template is [compatibility-matrix.md](compatibility-matrix.md).
An empty matrix is an explicit release blocker, not evidence of compatibility.

## Remaining roadmap (not a release gate by itself)

1. Add provider OAuth/Modern Auth and a secret-safe remote execution path.
2. Expand the compatibility matrix with provider-specific dry/live pilots,
   interruption recovery, and evidence exports.
3. Add independent message-level reconciliation and UIDVALIDITY-aware evidence.
4. Add controller crash/restart, storage-fault, and cross-platform supervision
   tests against disposable servers.
5. Publish signed native installers, upgrade/rollback guidance, and results
   from representative large-scale migrations.

## Current dependency audit

CI runs `cargo audit`. The current dependency graph has no reported vulnerabilities; RustSec reports only the unmaintained transitive crates `paste` and `ttf-parser`, which come from the desktop GUI stack and remain tracked for upstream replacement.
