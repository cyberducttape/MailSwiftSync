# Release readiness

MailSwiftSync should earn a stable 1.0 label through evidence, not feature count.

## Enterprise artifact-signing gate

Tagged releases are blocked unless the release environment supplies platform
signing material. The workflow signs Linux checksums with the configured GPG
key, signs Windows binaries with timestamped Authenticode, signs macOS
binaries with Developer ID, and submits macOS archives to Apple notarization.
Required CI inputs are `RELEASE_GPG_PRIVATE_KEY_BASE64`,
`RELEASE_GPG_KEY_ID`, `WINDOWS_SIGNING_CERT_BASE64`,
`WINDOWS_SIGNING_CERT_PASSWORD`, `WINDOWS_TIMESTAMP_URL`,
`MACOS_SIGNING_CERT_BASE64`, `MACOS_SIGNING_CERT_PASSWORD`,
`MACOS_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_APP_SPECIFIC_PASSWORD`, and
`APPLE_TEAM_ID`. They are deployment secrets, not repository defaults; missing
inputs intentionally fail the tag workflow instead of publishing unsigned
artifacts.

## Linux distribution gate

Portable archives and the pinned container image are sufficient for the alpha
preview. Before a stable Linux release, complete the
[Linux packaging roadmap](distribution/linux-packaging-roadmap.md): signed
Debian/Ubuntu and RHEL-family repositories, x86_64 and ARM64 artifacts, shell
completions, an installed man page, a dependency doctor, deterministic
upgrade/uninstall behavior, and automated enforcement of the qualified
imapsync `2.314` engine contract. Do not present the current package from a
Linux distribution as verification-qualified merely because a transfer can
run with it.

The container build uses immutable OCI digests for its Debian and Rust base
images. The imapsync package is independently SHA-256 verified before
installation. Base-image digest updates are deliberate supply-chain changes
and require the corresponding integration run.

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
  descendant tree. macOS now records and validates process start time, group,
  and session identity through `proc_pidinfo`; failures still use the
  conservative no-signal fallback.
- Windows private files and directories now receive protected owner/System
  DACLs rather than relying on inherited ACLs; the native Windows runtime test
  suite remains the release evidence for this implementation.
- CI and tagged releases generate a Rust CycloneDX 1.5 SBOM from the locked
  Cargo dependency graph and a final-image SPDX SBOM covering Debian packages,
  Dovecot, imapsync, and runtime libraries. Each is published with its own
  checksum; the Rust SBOM serial and timestamp are reproducible from
  `Cargo.lock` and `SOURCE_DATE_EPOCH`.
- The cross-platform CI matrix runs the full locked test suite on Linux,
  Windows, and macOS targets in addition to compiling release binaries. This
  is platform runtime coverage for shared behavior; the Windows native suite
  also verifies that closing the Job Object terminates the engine process.
  macOS retains a conservative no-signal fallback only when its native
  identity query cannot validate the recorded process.
- A compatibility-matrix release gate and verification script are checked into
  the repository. Branch validation checks structure; tagged release
  validation uses strict mode and rejects unresolved evidence markers in the
  dry-pilot, live-pilot, recovery, or evidence columns.
- Durable attention reasons now survive restart and are included in Markdown
  and proof-wrapped JSON reports; `mailswiftsync backup <state.db> <backup.db>`
  creates a locked, non-overwriting, SQLite-integrity-checked ledger backup.
- `mailswiftsync restore <backup.db> <state.db>` validates a current-schema,
  readable SQLite backup before installation and preserves an existing state
  database as a uniquely named rollback artifact.
- Authentication, transport, quota, policy, configuration, message-rejection,
  interruption, and process-identity failure categories are persisted for
  failed/cancelled and operator-review mailbox states; controller-generated
  failures carry a stable machine-readable attention reason while legacy
  records retain the compatibility text fallback.
- Opening an existing older on-disk schema now creates a unique,
  integrity-checked `state.db.pre-migrate-vN.<id>.db` backup before migration;
  new and in-memory databases are not copied.
- Dovecot checkpoint extraction accepts bounded standard-base64 state tokens
  (with or without padding), and
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
  text. Large projects retain exact mailbox totals and state counts while
  limiting detailed mailbox statuses to an explicitly marked 1,000-row sample.
- Headless single-mailbox preflight/live commands support an opt-in
  `--diagnostic-log <directory>` transcript. It is disabled by default; when
  enabled, engine output is secret-redacted, bounded to 20 owner-only files,
  names files with project/run identifiers, and warns that mailbox metadata may
  be present. Newly created directories are restricted, while existing
  directory permissions are preserved and must be selected appropriately by
  the operator. Batch commands reject this option until their per-child log
  lifecycle is explicitly defined.
- A headless `customer-proof <state.db> <output.json>` export uses the same
  redacted customer artifact as the GUI, so automation can produce a proof
  without depending on a file dialog. It requires durable project completion
  by default; `--allow-incomplete` is an explicit opt-in for a progress
  artifact whose JSON is labeled `completion_claim.status=incomplete`, and is
  never a completion certificate. The result remains unsigned until an
  approved Ed25519 signing key is applied with `sign`.
- A foreground `supervise <state.db>` controller can continuously watch and
  process automation-safe durable batch work while leaving operator-review
  rows untouched; external service-manager integration and scheduling policy
  are still deployment responsibilities. The service deployment guide now
  documents the credential hand-off and recovery checklist; supervision does
  not turn a credentialless restored queue into an unattended production
  approval.
- Run metadata records the version string returned by each engine when its
  executable supports `--version`; unavailable versions remain explicit in
  exports rather than being guessed.
- A reproducible Linux product integration lab starts two disposable,
  self-signed STARTTLS Dovecot servers, writes an isolated profile and
  owner-only secret files, drives the packaged MailSwiftSync binary through
  headless preflight, live and incremental migration, exports and verifies a
  customer proof, and checks the destination message IDs. The fixture selects
  Dovecot 2.3 or 2.4 configuration syntax to match the installed runtime and
  uses the same pinned Debian Bookworm Dovecot and imapsync packages used by
  the distributed container. Tagged release publication depends on this
  product-level gate. This lab does not fault-inject the transfer itself;
  see the engine storage-fault lab below for that coverage.
- An engine-side destination storage-fault lab
  (`scripts/engine-storage-fault-smoke.sh`) complements the ledger-level
  chaos lab below by fault-injecting the transfer itself rather than the
  ledger. It launches the disposable destination Dovecot's per-connection
  `imap` service under a small `RLIMIT_FSIZE` (substituted as
  `service imap { executable = <wrapper> }`, which confines the limit to
  the mail-writing worker rather than the master/auth/login services other
  connections depend on), then transfers one message under the limit and
  one over it. The oversized write is killed by `SIGXFSZ`, reproducing a
  full destination filesystem refusing to grow a file past its limit
  without root, a real full disk, or a loopback-mounted filesystem. The lab
  asserts the run is never reported as a clean `verified` success, that the
  ledger records an explicit non-clean terminal or attention state and
  stays readable afterward, and that the destination actually has the
  small message but not the oversized one, so a real per-message failure
  cannot be silently reported as a clean run. Tagged release publication
  depends on this gate the same way it depends on the other integration
  labs.
- A separate controller recovery lab uses a deterministic blocking engine to
  verify durable `running` state, simulate an ungraceful controller crash, and
  confirm `recover` clears process ownership and moves interrupted work to
  operator attention. This covers controller/ledger restart behavior without
  confusing it with successful engine transfer coverage.
- A controller chaos lab (`scripts/controller-chaos-smoke.sh`) covers two
  ledger-level fault classes without needing a real full disk: an `RLIMIT_FSIZE`
  limit hit while the ledger is first written (proving the controller stops
  rather than completing silently, and that the ledger opens cleanly once the
  limit is lifted, with no partial write visible), and a corrupted or
  truncated ledger/backup file (proving `restore` rejects a truncated or
  non-database restore source without touching the live ledger, that `status`
  and `recover` both refuse to operate on a corrupted ledger rather than
  silently continuing, and that restoring an earlier verified backup returns
  the ledger to normal operation). This closes the ledger-level half of the
  disk-full/corruption chaos gap; the engine-side half (the transfer itself
  running out of destination space) is covered by
  `scripts/engine-storage-fault-smoke.sh`, described above.
- Headless `status` and `recover` commands expose secret-free durable state and
  reuse the GUI's fail-closed process recovery path. They are control-plane
  primitives, and `headless preflight|live` now drives the existing controller
  without a window. `headless live` always performs a fresh preflight before
  promotion and refuses restored batch queues. `headless batch-preflight|batch-live`
  drives a complete durable queue without silently narrowing its scope.
  `supervise` now accepts an optional `HH:MM-HH:MM[@Mon,Tue,...]` maintenance
  window (may wrap past midnight) that confines new batch passes to that
  local time-of-day/day-of-week range without abandoning a batch already
  admitted before the window closes; being outside the window counts toward
  the same idle-poll limit as having no actionable work, so a
  scheduler-launched, bounded invocation still exits instead of running
  through every subsequent window. This is a foreground process-level
  building block, not a persistent service; an external service
  manager/scheduler (systemd timer, cron, Task Scheduler) is still required
  to relaunch it, and a long-lived remote API remains outstanding.
- A Linux headless `Dockerfile` provides explicit durable-state and per-user
  runtime volumes, non-root execution, pinned packaged
  `imapsync`/`dovecot`/`doveadm` dependencies, and the real IMAP transfer
  fixture. CI builds and smoke-tests the image; tagged release publication
  also depends on the packaged integration run. Official registry
  publication and image signing remain release gates.
- Tagged binary publication now depends on a dedicated release quality gate
  covering formatting, shell syntax, strict Clippy, the locked test suite, and
  RustSec auditing in addition to cross-platform compilation.
- The publish job emits a deterministic SHA-256 manifest covering every
  release file, verifies its checksum, and attaches GitHub build provenance to
  that manifest; native code-signing and notarization remain separate gates.
- The default ledger path is resolved only from `MAILSWIFTSYNC_STATE_PATH` or
  the OS data directory. If neither is available, the controller enters a
  clearly blocked non-durable mode instead of silently placing the ledger in
  a temporary directory.
- Saved migration profiles use the OS configuration directory and fail
  explicitly when it cannot be resolved; endpoint configuration is never
  silently persisted under a temporary path.
- The imapsync OAuth 2.0 path now supports automatic access-token refresh.
  An operator who has registered their own OAuth application with the
  provider and obtained a refresh token can store the token endpoint,
  client ID/secret, and refresh token under an OS-keyring ID; MailSwiftSync
  exchanges it for a fresh access token immediately before each live launch
  (single mailbox or each mailbox in a batch queue), follows refresh-token
  rotation, and only performs `https://` token-endpoint requests over a
  certificate-validated TLS connection built the same way as the IMAP
  readiness probe. This remains provider-consent-free: MailSwiftSync still
  does not implement an OAuth authorization flow, so the operator must
  obtain the initial refresh token through the provider's own tooling. A
  rotated refresh token is persisted with bounded retries; if the keyring
  remains unavailable, live execution fails closed and retains the rotated
  configuration only in protected process memory for persistence recovery,
  without attempting another provider exchange.

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

- Provider consent flows and equivalent unattended secret-broker delivery
  remain outstanding: MailSwiftSync still does not implement an OAuth
  authorization flow, so an operator must register their own application and
  obtain the initial refresh token through the provider's own tooling. The
  imapsync path now supports operator-supplied OAuth 2.0 access tokens with
  XOAUTH2, including keyring references, private token-file delivery,
  redacted previews, fresh pre-live authentication, and (once a refresh
  token is stored) automatic access-token refresh before each live launch.
  Remote Dovecot execution is deliberately disabled until the application has
  a delivery mechanism that cannot expose credentials through
  destination-host process inspection.
- Signed portable release archives for Linux, Windows, and macOS, with checksums, platform signatures/notarization, and reproducible release instructions. Native OS-specific installer packages remain outside the current archive format.
- A compatibility matrix covering Dovecot versions, common hosted IMAP providers, TLS modes, folder namespaces, and authentication methods.
- Preflight checks for DNS, TCP/TLS, authentication, folder inventory, and
  observed special-use folders are implemented for the authenticated IMAP
  probe. Quota capacity, source-size forecasting, and provider-specific
  destination readiness remain explicit unknowns where the server cannot
  provide a reliable query.
- Explicit retry/resume/delta semantics with idempotent recovery after interruption. Dovecot checkpoints remain engine resume tokens, not UIDVALIDITY-aware message proof.
- Bounded concurrency and throttling are implemented; `supervise` now accepts an optional maintenance window (see above), but a scheduler/API that can survive the desktop closing without an external process manager remains outstanding.
- Independent metadata-level message mismatch reporting and reconciliation is now wired for encrypted imapsync runs, with mismatch rows committed atomically alongside terminal evidence and exposed in operator verification reports. The current verifier has a conservative estimated in-memory state budget and fails closed when it is exceeded; SQLite-backed per-message extraction staging, body-content hashing, and live-provider qualification remain outstanding.
- Published migration evidence from representative datasets, including failures and recovery results.
- Controller-level integration and chaos tests using disposable IMAP/Dovecot environments, including process kill, GUI restart, retry, and evidence recovery. Ledger-level storage failure (a storage limit hit mid-write, and a corrupted or truncated ledger/backup) is covered by `scripts/controller-chaos-smoke.sh`; engine-side storage failure (the transfer itself exhausting destination space) is covered by `scripts/engine-storage-fault-smoke.sh`.
- **Dovecot 2.4.x probe stall safeguard (resolved in the controller):** the LIST/discovery reader now retries transient socket `TimedOut`/`WouldBlock` reads only within its 60-second total deadline, instead of checking the deadline only after a successful read. A regression test covers a timeout during LIST followed by a valid response. Dovecot 2.4.x remains a compatibility-matrix qualification target until a disposable live pilot is recorded; the controller will now fail with bounded discovery evidence rather than hang for the external 300-second watchdog.
- A clean `cargo audit` result for vulnerabilities; unmaintained transitive dependencies must be tracked and reviewed before each release.
- CI and release workflows should keep third-party GitHub Actions pinned to reviewed commit SHAs; update pins deliberately with the corresponding release version documented in a comment.

The test-gate template is [compatibility-matrix.md](compatibility-matrix.md).
An empty matrix is an explicit release blocker, not evidence of compatibility.

## Remaining roadmap (not a release gate by itself)

1. Add provider OAuth/Modern Auth and a secret-safe remote execution path.
2. Expand the compatibility matrix with provider-specific dry/live pilots,
   interruption recovery, and evidence exports.
3. Add body-content hashing, UIDVALIDITY-aware evidence, and per-message checkpoints.
4. Add controller crash/restart, storage-fault, and cross-platform supervision
   tests against disposable servers.
5. Publish signed native installers, upgrade/rollback guidance, and results
   from representative large-scale migrations.

## Current dependency audit

CI runs `cargo audit`. The current dependency graph has no reported vulnerabilities; RustSec reports only the unmaintained transitive crates `paste` and `ttf-parser`, which come from the desktop GUI stack and remain tracked for upstream replacement.
