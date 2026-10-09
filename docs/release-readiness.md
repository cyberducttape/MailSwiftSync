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

Portable archives, deterministic Debian and RPM packages with signed
checksums, and the pinned container image are produced for x86_64 and ARM64
(`aarch64`). CI builds and verifies both packages on every push and installs,
runs, and removes the RPM on a digest-pinned Fedora image. Before a stable
Linux release, the remaining distribution work is hosting signed Debian/Ubuntu
and RHEL-family repositories (signed repository metadata and package
signatures from the release key). Shell completions ship in the `.deb`, and
`mailswiftsync doctor --strict` enforces the qualified imapsync `2.314` engine
contract with a nonzero exit status. The Debian package installs the
control plane and documentation only; do not present it as verification-
qualified merely because a transfer can run with it.

The container build uses immutable OCI digests for its Debian and Rust base
images. The imapsync package is independently SHA-256 verified before
installation. Base-image digest updates are deliberate supply-chain changes
and require the corresponding integration run.

## Desktop accessibility release gate

Do not infer desktop accessibility from egui defaults or unit tests alone.
Before a stable release, attach repeatable evidence for keyboard-only completion
of the primary workflows (including every confirmation dialog), predictable
focus order and visible focus, and inspection of the AccessKit accessibility
tree with a supported screen reader on each desktop platform. Exercise the
high-contrast theme, 200% interface scale, and state communication without
relying on red/green discrimination. A failure in a safety confirmation,
unreachable control, missing accessible name/role/state, or clipped content at
200% is a release blocker. Keep platform/screen-reader versions and the tested
scenes in the release evidence; automated egui tests complement, but do not
replace, those manual assistive-technology checks.
Current automated coverage, the 200%/High Contrast review, and the manual
screen-reader checklist are recorded in
[accessibility-evidence.md](accessibility-evidence.md).

## Completed in the current hardening pass

- Remote Dovecot password-in-argv execution is rejected rather than exposed by
  an operator acknowledgement.
- Endpoint parsing rejects malformed explicit ports instead of treating them as
  hostnames.
- Persistent profile and state directories fail closed when ownership or
  non-writable-by-other-users permissions cannot be established; sensitive
  files inside them remain owner-only.
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
- Headless preflight/live and batch commands support opt-in
  `--diagnostic-log <directory>` transcripts. Each batch child has a separate
  file; mailbox job IDs are hashed in filenames. Engine output is redacted for
  known secrets and bounded to 8 MiB per file / 128 MiB per directory, with
  retention capped at 20 completed files. These plaintext logs can still
  contain provider, mailbox, folder, and message metadata; transcript contents
  are not account-pseudonymized, encrypted, or compressed, and support bundles
  do not collect them. Operators must select and handle logs explicitly.
  Logging is disabled by default. Newly created directories are restricted;
  existing directory permissions are preserved and must be selected
  appropriately by the operator.
- A headless `customer-proof <state.db> <output.json>` export uses the same
  redacted customer artifact as the GUI, so automation can produce a proof
  without depending on a file dialog. It requires durable project completion
  by default; `--allow-incomplete` is an explicit opt-in for a progress
  artifact whose JSON is labeled `completion_claim.status=incomplete`, and is
  never a completion certificate. The result remains unsigned until an
  approved Ed25519 signing key is applied with `sign`.
- A headless `certificate <state.db> <output.json> <key>` command combines
  completed customer-proof export and Ed25519 signing. It refuses incomplete
  projects and publishes the signed output atomically, so an operator cannot
  accidentally deliver the unsigned intermediate as the final artifact. The
  signed evidence explicitly includes each mailbox's immutable run-plan
  SHA-256 and, when the plan snapshot resolved it, the engine-binary
  SHA-256. The certificate authenticates the durable ledger evidence and
  signer; it does not claim independent per-message attestation.
- An owner-only `organization-policy.toml` may require TLS, prohibit
  destination mutation, require metadata or bounded body verification, and cap
  batch concurrency. The same policy is shown during preflight and rechecked at
  batch admission; provider-tenant rate quotas and role-scoped policy
  administration remain outside this local policy file.
- `notify-webhook` includes a deterministic event ID, event type, and
  idempotency key. Production organization policy requires
  `MAILSWIFTSYNC_WEBHOOK_SIGNING_SECRET` or its owner-only file form, which
  adds an HMAC-SHA256 envelope signature; compatibility/lab policies may opt
  out only for explicitly approved legacy receivers.
- The notifier now persists a credential-free outbox record before attempting
  delivery, retries due records with bounded exponential backoff, and records
  dead-letter state after repeated failure. The outbox stores only an endpoint
  digest, never the endpoint URL or authentication material. Use
  `notify-webhook --watch` for continuously running operator-managed delivery;
  fleet-centralized management and service-manager deployment remain separate
  operational concerns.
- Ledger transitions now generate lifecycle events transactionally in the
  outbox (`migration.started`, `migration.completed`,
  `migration.cutover_ready`, `mailbox.completed`,
  `mailbox.preflight_failed`, `mailbox.verification_difference`,
  `migration.proof_ready`, and verification acceptance). Proof-ready events
  carry the SHA-256 of the successfully signed certificate. Events are
  endpoint-unbound until delivery configuration is supplied, so credentials
  and URLs remain outside durable state.
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
- The [durability and concurrency validation plan](durability-and-concurrency-validation.md)
  distinguishes process-loss testing from hypervisor power-loss testing and
  defines the real-engine 1/4/8/16-worker matrix. VM power-cut campaigns and
  simultaneous real-engine resource measurements remain release evidence to
  collect; the existing 100k single-engine result must not be generalized to
  that matrix.
- GA remains blocked until the release evidence package contains signed,
  reviewed real-provider qualification for the supported Gmail/Workspace and
  Microsoft 365 routes, a completed hypervisor power-cut campaign, and the
  real-engine 1/4/8/16-worker matrix. Synthetic reconciliation, generic IMAP
  fixtures, and controller-only scheduler stress are not substitutes for
  those gates. The verification envelope's 1,000,000-message metadata,
  100,000-body-proof, response-size, and shared-byte limits must be shown in
  capacity planning before admission where the observed inventory permits it;
  otherwise the run must remain explicitly subject to fail-closed verification
  limits rather than being described as scale-qualified.
- The [release upgrade and provider-failure matrix](release-upgrade-validation.md)
  defines the binary-produced 0.8→0.9→RC→1.0 persistence campaign and the
  real-provider throttling, quota, token-expiry, disconnect, folder-limit,
  large-message, and suspended-account cases. Current legacy-schema tests and
  generic fixtures do not satisfy those release or provider evidence gates.
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
- Pull-request CI independently collects line and branch LCOV with nightly
  LLVM instrumentation, and enforces per-module floors for safety-critical
  plan admission, batch attempt handling and
  lifecycle/scheduling,
  adaptive provider rate domains, provider response classification, durable
  state and schema-integrity proof, evidence projection, recovery, IMAP
  authentication, OAuth, process supervision, and verification code;
  the report is uploaded as an artifact for review rather than reduced to one
  repository-wide percentage. The branch floors are minimum regression
  guardrails, not a claim that coverage proves state-machine correctness.
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
  readiness probe. The initial refresh token comes from
  `mailswiftsync oauth-authorize` (authorization-code flow with PKCE, loopback
  redirect, state check, refresh-token required) or from the provider's own
  tooling. A
  rotated refresh token is persisted with bounded retries; if the keyring
  remains unavailable, live execution fails closed and retains the rotated
  configuration only in protected process memory for persistence recovery,
  without attempting another provider exchange.

## Available in 0.1

- Explicit Dovecot-native and imapsync engine paths.
- Dry-run default and explicit confirmation before destination changes.
- Durable projects, mailbox states, lifecycle events, run IDs, and redacted output.
- CSV/XLSX validation queue with bounded 1–256 worker concurrency and a conservative default of 2. Legacy XLS is rejected because Calamine materializes it before application resource checks.
- Bounded transient retries for validation and live work; the Activity view
  reports retry attempt/max-retry budget, and exhaustion is recorded as an
  explicit terminal reason. Authentication and configuration failures stop
  without retry loops. The current policy has a bounded retry-count budget and
  provider-aware exponential backoff/jitter; a configurable wall-clock retry
  budget and per-class failure counters remain future work.
- Live batch waves with mailbox-specific child runs, claim-before-launch, selective retry scopes, transactional plan checks, and per-mailbox evidence: independent message-level verification for encrypted imapsync children whose plan the verifier can reproduce, aggregate or engine evidence otherwise.
- Aggregate source/destination folder, message, and virtual-size evidence.
- The pre-migration risk report exposes an advisory 0–100 scale-readiness score
  derived only from supplied message, folder, and size facts. It never clears
  authentication, TLS, capacity, verification, policy, or provider-qualification
  gates; those remain explicit pass/warning/block outcomes in preflight.
- The [adoption qualification plan](adoption-qualification-plan.md) defines
  the required mailbox shapes, interruptions, mutations, exact engine versions,
  direct-imapsync baselines, and performance measurements. The
  [automation contract](automation-contract.md) documents the current JSON
  and durable-state rules, but a stable major-version API promise is not yet
  claimed.
- The [production qualification environment](production-qualification-environment.md)
  separates deterministic Dovecot evidence from disposable provider evidence
  and defines the required fault, recovery, quota, OAuth, UIDVALIDITY, resource,
  and verification-limit scenarios before promotion.
- Timeout, cancellation, child-process-only imapsync passfile credentials, fresh dual-IMAPS authentication before live imapsync launches, and documented Dovecot process-visibility limits.

## Required before calling it production-ready

- **100k-row desktop scale gate:** the importer accepts up to 100,000 rows.
  Imported rows now retain only mailbox identity/credential deltas and share
  immutable batch defaults through an `Arc`; a full `Form` is hydrated only at
  admission or worker execution. Since 2026-10-02 the queue itself is
  SQLite-backed: an import is written to the ledger as a batch project, the
  durable ledger stores one normalized `batch_plans` record per distinct batch
  policy and compact mailbox identity/credential deltas on `mailbox_jobs`.
  Import interns shared plans before constructing rows so equivalent
  mailbox records hold shared serialized-plan allocations rather than
  repeatedly serializing full profiles; legacy full-row configs remain readable
  for compatibility. The
  Mailboxes view renders virtual rows from SQLite rowid pages in the default
  unfiltered and unselected filtered states, plus a bounded row cache; search
  and state filters run as SQL over a narrow facts table and fetch matching
  rowid pages on demand. Explicit selection materializes the filtered index so
  selection membership remains exact. Admission and the scheduler read rows
  from the ledger instead of an in-memory job list. Selection summaries and first-selection lookups use
  SQL aggregates and `LIMIT 1`; full queue scans remain only for operations
  whose identity deliberately includes every selected mailbox, such as live
  confirmation fingerprints. The importer, UI, and reload gates are reproducible
  with the three benchmark scripts below and must be repeated on each
  supported release host class.
- **100k-message end-to-end scale gate:** `.github/workflows/scale-qualification.yml`
  provides a monthly/manual run that creates 100,000 distinct RFC822 messages
  in one disposable Dovecot mailbox, then exercises the packaged imapsync
  transfer, durable evidence, and metadata verifier. The first successful run
  on 2026-10-03 used imapsync 2.314 and Dovecot 2.3.19.1: the live pass took
  14m10s, and the complete workflow step took 16m57s. Metadata reconciliation
  matched 100,016 messages / 28,681,943 bytes across three folders with zero
  missing, extra, failed, or unresolved messages; the subsequent delta left
  100,017 messages on the destination. See the [retained Actions run](https://github.com/cyberducttape/MailSwiftSync/actions/runs/37139081988).
  A later instrumented run also completed and retained an exact 100,016-message
  metadata-reconciled customer proof, but its separate incremental pass did not
  finish before the 90-minute job limit. See the [instrumented run and proof
  artifact](https://github.com/cyberducttape/MailSwiftSync/actions/runs/37154259149).
  This confirms the base transfer again, not successful incremental recovery
  or a completed instrumented qualification run.
  The next instrumented [hosted run](https://github.com/cyberducttape/MailSwiftSync/actions/runs/37165164757)
  retained an exact-metadata proof for 100,016 messages / 28,681,943 bytes
  with zero failed or unmatched messages. Its phase markers show the initial
  live pass, the later incremental live pass, and the one-pass Maildir fixture
  scan all completed. However, the proof was exported before the incremental
  pass, so it does not verify the post-delta result. A subsequent
  [instrumented run](https://github.com/cyberducttape/MailSwiftSync/actions/runs/37169979731)
  again retained an exact-metadata proof for 100,016 messages with zero
  unresolved differences; its incremental transfer, fixture scan, and both
  Dovecot shutdowns completed. The final markers stop immediately before
  recursive workspace deletion, and the job hit its 90-minute limit without a
  resource summary. Scale mode now leaves that 100k-file workspace inside the
  disposable container for host teardown, avoiding per-file deletion after
  verification. Run 37174639410 confirms that the incremental transfer, fixture
  scan, both Dovecot shutdowns, and harness cleanup completed, but the container
  did not exit and GitHub cancelled the job at 90 minutes before host metrics
  were recorded. The disposable container now uses Docker's init shim to reap
  orphaned fixture children and exit with the harness; Dovecot shutdown remains
  bounded to 10 seconds per server, and the host wait has its own deadline and
  fallback marker. Earlier hosted runs that appeared to stall after cleanup were a
  host-watcher defect: the runner user could not read the container-private
  progress directory, so a harness that had already written its completion
  marker was reported as a 3,800-second watchdog timeout. With the watcher
  given access to that directory, both packaged scenarios completed on hosted
  `ubuntu-24.04` runners on 2026-10-04 (commit `b058158`):

  | Scenario | Messages | Body payload | Wall clock | Sampled peak container memory | Result | Run |
  |---|---|---|---|---|---|---|
  | `100k-small` | 100,016 | 28,681,943 bytes total | 1,045 s | 1,254,130,450 bytes (≈1.17 GiB) | exact metadata match, 0 missing/extra/modified | [37246504219](https://github.com/cyberducttape/MailSwiftSync/actions/runs/37246504219) |
  | `10k-1g` | 10,016 | 1,075,944,871 bytes total | 90 s | 366,162,739 bytes (≈349 MiB) | exact metadata match, 0 missing/extra/modified | [37246501700](https://github.com/cyberducttape/MailSwiftSync/actions/runs/37246501700) |

  Each run completed seed, incremental delta, fixture Message-ID scan, and
  harness cleanup, and retained its customer proof, progress markers, and
  resource summary as a workflow artifact. Wall clock covers the whole
  containerized harness (fixture generation, both passes, verification, and
  proof export). These runs exported their proof before the incremental pass;
  the harness now also exports a post-delta proof. Memory is the aggregate
  container figure (controller, imapsync, and both Dovecot fixture servers),
  sampled once per second. These results qualify the packaged generic-IMAP
  path at these sizes only; they do not qualify 20 GiB mailboxes, Gmail,
  Microsoft 365, or other hosted providers.
  The container memory figure is aggregate cgroup usage: the controller,
  imapsync, both Dovecot fixture servers, and active page cache from writing
  the Maildir fixtures. Scale runs now also retain
  `process-memory-peaks.txt`, which records the peak summed RSS per process
  name and the peak cgroup anonymous versus file (page-cache) memory, so the
  controller's own share is reported separately. Hosted runs on 2026-10-05
  (commit `f5eb85b`) attribute it as follows:

  | Scenario | Container (docker stats) | Process memory (cgroup anon) | Page cache (cgroup file) | imapsync (`perl`) | Dovecot `imap` | MailSwiftSync |
  |---|---|---|---|---|---|---|
  | `100k-small` ([run](https://github.com/cyberducttape/MailSwiftSync/actions/runs/37254315384)) | 1,265 MB | 531 MB | 1,145 MB | 451 MiB | 78 MiB | 29 MiB |
  | `10k-1g` ([run](https://github.com/cyberducttape/MailSwiftSync/actions/runs/37254317846)) | 370 MB | 219 MB | 2,240 MB | 195 MiB | 18 MiB | 25 MiB |

  The controller is about 2% of the container figure and matches the local
  verification benchmark below. Process memory is dominated by imapsync,
  which held about 4.7 KB per message for one 100k-message folder; the rest of
  the container figure is reclaimable kernel page cache from fixture and
  Maildir writes. Size migration nodes by engine memory per concurrent job
  (imapsync grows with the largest folder's message count), not by controller
  memory.

  The controller's metadata-only verification path is measured independently
  by `scripts/benchmark-verification-scale.sh` (durable FULL-synchronous stage,
  rows staged in 5,000-message pages as the live IMAP adapter does, each size
  in its own process). Local release build, Linux x86_64, 2026-10-04:

  | Messages per side | Peak controller RSS | Stage after staging | Peak stage during reconcile | Peak rollback journal | Staging rows/s | Reconcile msg/s | CPU s |
  |---|---|---|---|---|---|---|---|
  | 10,000 | 15 MiB | 5.8 MB | — | — | 186,915 | 81,967 | 0.2 |
  | 100,000 | 17 MiB | 61 MB | 102 MB | 9.7 MB | 174,064 | 76,804 | 2.4 |
  | 250,000 | 18 MiB | 156 MB | — | — | 156,838 | 73,014 | 6.8 |
  | 500,000 | 21 MiB | 315 MB | — | — | 165,562 | 73,152 | 12.9 |
  | 1,000,000 | 28 MiB | 633 MB | 1,055 MB | 101 MB | 111,856 | 72,061 | 26.6 |

  Controller memory is effectively flat in message count; the scaling cost is
  private stage disk (about 475 bytes per message per side, plus about 50%
  transient during reconciliation). The release workflow gates every size at
  64 MiB peak RSS and 20,000 reconciled messages/s.

  Each benchmark line also reports phase-separated `/proc/self/io` read/write
  bytes and read/write syscall counts. `read_bytes` and `write_bytes` can be
  zero on a cache-backed or tmpfs stage; those values are not physical-disk
  evidence, so qualification runs must place the stage on the target storage
  class. Allocator-level allocation counts are not inferred from RSS; collect
  them separately with the deployment's allocator/profiling tool when making
  an allocator decision.

  Reconciliation deliberately runs as one transaction on the private,
  per-verification stage. Measured at 1M messages per side, its rollback
  journal peaks near 100 MB (about 10% of the stage), an interruption loses at
  most about 18 seconds of recomputable work, and no other connection shares
  the stage, so lock duration has no contender. Most transient growth is the
  intermediate ranking tables, which chunked commits would not reduce; a
  chunked/generation model is therefore not adopted unless a larger
  qualification shows the journal or lost work becoming material.
  The earlier repeated Maildir scans have been replaced by one pass, which
  took 1.78 seconds on a synthetic 100,004-file Maildir.
  Scale mode disables per-message debug output and allows up to 40 minutes per
  CLI command within a 90-minute workflow limit.
- Batch scheduling keeps conservative defaults but exposes explicit global
  worker and process-start ceilings for qualified deployments. The adaptive
  global/provider/tenant/credential/mailbox rate-domain limiter remains the
  effective safety boundary; changing these ceilings is recorded in the run
  snapshot and invalidates an open live confirmation.

  The controller scheduler test also settles 1,024 synthetic mailbox jobs
  across 32 tenant domains at 16 workers and checks per-tenant completion and
  observed parallelism. This is scheduler/rate-domain component evidence only:
  it does not launch IMAP engines, model provider latency/throttling, or replace
  multi-migration load qualification against disposable real servers.

  Provider capacity events honor provider-suggested retry delays when
  available, reduce the affected hierarchical domain, and expose the observed
  domain, adaptive ceiling, configured ceiling, and escalation count in run
  telemetry. Retry budgets and global/provider/tenant/credential/mailbox
  fairness remain bounded by the scheduler. Cooldown state is intentionally
  process-local today; a controller restart re-qualifies capacity from fresh
  provider signals rather than restoring a stale cooldown. This remains a
  qualification limitation, not evidence of a provider-wide quota guarantee.

  The latest local release-mode baseline on 2026-09-30 (Linux x86_64, the
  cases run sequentially in one process) was:

  | Import | Rows | Elapsed | RSS after case |
  | --- | ---: | ---: | ---: |
  | CSV | 1,000 | 2 ms | 8.7 MiB |
  | CSV | 10,000 | 22 ms | 14.5 MiB |
  | CSV | 100,000 | 220 ms | 71.6 MiB |
  | XLSX | 10,000 | 55 ms | 22.2 MiB |
  | XLSX | 100,000 | 558 ms | 125.9 MiB |

  RSS is the process resident set reported by Linux and is cumulative because
  the cases run in one process; it is evidence for a baseline, not a portable
  memory budget. Re-run the script on each supported release host and retain
  the raw output with the release evidence. The import benchmark enforces
  default elapsed-time budgets of 100/500/2,000 ms for 1k/10k/100k CSV and
  1,000/5,000 ms for 10k/100k XLSX. Qualified host classes may override these
  with the documented `MAILSWIFTSYNC_IMPORT_*_BUDGET_MS` variables; an absent
  or malformed result fails the script.

  The tagged-release quality job runs all three scale scripts on its Linux
  runner, so the documented budgets are release gates rather than an
  unexecuted local benchmark recipe.

  The queue read path has a companion command,
  `scripts/benchmark-ui-scale.sh`. Against a file-backed 100,000-row durable
  queue it measures a SQL filter keystroke, select-all with its selection and
  queue-health accounting, a 1,000-row durable state change, the first
  Mailboxes frame through the real App, and the off-thread write of a parsed
  import to the ledger. The latest local release-mode result on 2026-10-04 was
  7 ms for filtering, 54 ms for select-all, 23 ms for the state change, 100 ms
  for the Mailboxes first frame, and 2.623 s to write the import; the complete
  shell's first frame was 48 ms, selected-frame update 1 ms, and search-frame
  update 8 ms. The same 100k-row CSV import benchmark measured 295 ms and 48
  MiB RSS growth. All scripted budgets passed; the Mailboxes first frame is
  at the 100 ms limit. These are
  slower than the earlier in-memory
  baselines (single-digit milliseconds) in exchange for bounded memory and a
  queue that survives restarts unchanged. These are release-mode host
  baselines. Filter results now use SQL counts and viewport paging when no
  explicit selection is active, avoiding a Rust-side vector of every matching
  row ID; terms of three or more characters use an FTS5 trigram index scoped
  by the queue-facts project predicate, while shorter or control-character
  terms retain a literal substring fallback. The script enforces default budgets of 100 ms for filtering, 250
  ms for selection-all, 100 ms for state refresh, 100 ms for the virtualized
  first frame, 5,000 ms for writing the import, and 500 ms for the full shell; qualified host classes may
  override these with the documented `MAILSWIFTSYNC_UI_*_BUDGET_MS`
  environment variables. This is a first-frame render gate; native window
  startup and GPU initialization remain separate platform-release measurements.

  Durable workspace restart/read behavior has a separate opt-in command,
  `scripts/benchmark-reload-scale.sh`. It seeds and closes a file-backed
  100,000-row project, reopens it, then reads bounded mailbox, status, and
  verification pages plus aggregate counts. The latest local release baseline
  was 557 ms to seed, 286 ms to reopen, and 84 ms for those bounded reads. This
  is evidence for the SQLite read path, not an egui first-frame measurement or
  a release budget; capture both on each supported release host. The script
  enforces default budgets of 5,000 ms for seeding, 2,000 ms for reopen, and
  1,000 ms for bounded reads. Qualified hosts may override these with
  `MAILSWIFTSYNC_RELOAD_*_BUDGET_MS`; missing or malformed metrics fail the
  script.
- Provider consent is implemented as a CLI authorization-code flow with PKCE
  (`oauth-authorize`) for the operator's registered application; it still
  needs a recorded live pilot against Google and Microsoft tenants, and
  unattended secret-broker delivery beyond the OS keyring remains
  outstanding. The imapsync path now supports operator-supplied OAuth 2.0 access tokens with
  XOAUTH2, including keyring references, private token-file delivery,
  redacted previews, fresh pre-live authentication, and (once a refresh
  token is stored) automatic access-token refresh before each live launch,
  during qualified imapsync reconnects, and before independent verification.
  Rotated refresh tokens are persisted to the configured keyring or the run
  fails closed.
  Remote Dovecot execution is deliberately disabled until the application has
  a delivery mechanism that cannot expose credentials through
  destination-host process inspection.
- Signed portable release archives for Linux, Windows, and macOS, plus a
  deterministic signed Debian package for Linux, with checksums,
  platform signatures/notarization, and reproducible release instructions.
  The RPM package is part of the current Linux release format; native
  Windows/macOS installer packages remain outside it.
- A compatibility matrix covering Dovecot versions, common hosted IMAP providers, TLS modes, folder namespaces, and authentication methods.
- Preflight checks for DNS, TCP/TLS, authentication, folder inventory, and
  observed special-use folders are implemented for the authenticated IMAP
  probe. Quota capacity, source-size forecasting, and provider-specific
  destination readiness remain explicit unknowns where the server cannot
  provide a reliable query.
- Explicit retry/resume/delta semantics with idempotent recovery after interruption. Dovecot checkpoints retain the opaque engine state but are now envelope-bound to a complete source/destination mailbox UIDVALIDITY digest; legacy or incomplete-context checkpoints fail closed.
- A complete per-message imapsync transfer manifest is not currently implemented. Aggregate progress checkpoints remain operational telemetry; imapsync retries may rediscover already-present messages. Do not use human-readable copy lines as a skip index until a qualified engine acknowledgment contract supplies stable message identity and UIDVALIDITY-bound invalidation.
- Bounded concurrency and throttling are implemented; `supervise` now accepts an optional maintenance window (see above), but a scheduler/API that can survive the desktop closing without an external process manager remains outstanding.
- Independent metadata-level message mismatch reporting and reconciliation is wired for encrypted imapsync runs, with mismatch rows committed atomically alongside terminal evidence and exposed in operator verification reports. Verification staging is private and restartable: fetched pages, body fingerprints, and per-mailbox UID cursors survive a controller interruption and are cleared after a successful verification result or plan-identity invalidation. Each cursor records the folder's SELECT snapshot (UIDVALIDITY, UIDNEXT, EXISTS); a resumed scan reuses staged pages only when the server still reports that exact snapshot, otherwise the folder is rescanned from zero, and a folder completes only when its staged rows equal the snapshot's EXISTS. An explicit forensic mode now fetches bounded RFC822 bodies, retains SHA-256 fingerprints for both sides, and emits `body_hash` evidence only when coverage and total-byte limits hold. The default path remains metadata-only; representative live body-hash evidence and live-provider/large-mailbox qualification remain outstanding.
- Published migration evidence from representative datasets, including failures and recovery results.
- Controller-level integration and chaos tests using disposable IMAP/Dovecot environments, including process kill, GUI restart, retry, and evidence recovery. Ledger-level storage failure (a storage limit hit mid-write, and a corrupted or truncated ledger/backup) is covered by `scripts/controller-chaos-smoke.sh`; engine-side storage failure (the transfer itself exhausting destination space) is covered by `scripts/engine-storage-fault-smoke.sh`.
- Production gatekeeper handshake qualification is covered by `scripts/process-supervision-chaos-smoke.sh`: a launcher with no durable release token or an invalid token never starts the engine, while the exact `GO\n` release token does. This complements controller crash recovery, PID/start-time identity checks, and Windows Job Object CI coverage; it does not substitute for the required hypervisor power-cut campaign.
- **Dovecot 2.4.x probe stall safeguard (resolved in the controller):** the LIST/discovery reader now retries transient socket `TimedOut`/`WouldBlock` reads only within its 60-second total deadline, instead of checking the deadline only after a successful read. A regression test covers a timeout during LIST followed by a valid response. Dovecot 2.4.x remains a compatibility-matrix qualification target until a disposable live pilot is recorded; the controller will now fail with bounded discovery evidence rather than hang for the external 300-second watchdog.
- A clean `cargo audit` result for vulnerabilities; unmaintained transitive dependencies must be tracked and reviewed before each release.
- CI and release workflows should keep third-party GitHub Actions pinned to reviewed commit SHAs; update pins deliberately with the corresponding release version documented in a comment.

The test-gate template is [compatibility-matrix.md](compatibility-matrix.md).
An empty matrix is an explicit release blocker, not evidence of compatibility.

## Remaining roadmap (not a release gate by itself)

1. Qualify the implemented provider OAuth/Modern Auth flow against live Google
   and Microsoft tenants; keep remote Dovecot execution disabled until a
   secret-safe credential-delivery mechanism is available.
2. Expand the compatibility matrix with provider-specific dry/live pilots,
   interruption recovery, and evidence exports.
3. Extend restartable verification staging to provider-specific recovery drills and qualify the UIDVALIDITY-bound Dovecot resume path and opt-in body-hash path against representative providers and large mailboxes.
4. Add controller crash/restart, storage-fault, and cross-platform supervision
   tests against disposable servers.
5. Publish signed native installers, upgrade/rollback guidance, and results
   from representative large-scale migrations.
6. Qualify a UIDVALIDITY-bound per-message transfer manifest for imapsync
   reconnect optimization, including folder rename, deletion, duplicate, and
   destination-change fault cases; until then retain full engine rediscovery.

## Current dependency audit

Pull-request CI, release verification, and the scheduled dependency workflow run
both `cargo audit` and `cargo deny check advisories bans licenses sources`.
The current dependency graph has no reported vulnerabilities; RustSec reports
only the unmaintained transitive crate `ttf-parser`, which comes from the
desktop GUI stack and remains tracked for upstream replacement. This
maintenance finding is not suppressed by the policy.
