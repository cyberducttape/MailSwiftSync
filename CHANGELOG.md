# Changelog

All notable changes to MailSwiftSync are documented here. Detailed pre-alpha
development history is preserved in repository history and is not shipped in
operator distribution archives.

## [Unreleased]

- Startup recovery now bounds the number of active process identities it will
  materialize and fails closed on an oversized ledger instead of allocating an
  unbounded recovery vector.
- Durable batch creation and full mailbox/ID loads now fail closed above the
  100,000-row queue limit instead of accepting or materializing an arbitrarily
  large restored ledger.
- The doctor imapsync version probe now uses a five-second process timeout and
  bounded output capture, preventing a malformed executable from hanging or
  exhausting diagnostic memory.
- CI failure annotations no longer interpolate untrusted compiler or test log
  text into GitHub workflow commands; detailed logs remain available as
  artifacts and step summaries.
- Ledger validation now rejects projects whose durable mailbox queues exceed
  the runtime's 100,000-row admission and restore limit.
- CSV imports now validate and parse the same no-follow file handle, with a
  bounded reader that rejects files that grow past the input limit mid-read.
- Headless batch retry admission now fails closed when durable mailbox state is
  missing, rather than silently omitting those mailbox IDs from selection.
- Recovery schema validation now requires every runtime object to be an actual
  SQLite table, rejecting stamped ledgers that substitute views for tables.
- Durable batch reuse now streams queue comparison and loads only mailbox IDs,
  avoiding materialization of every persisted profile for large imported queues.
- Verification staging now keeps SQLite temporary sort and join data in memory,
  preventing sensitive mailbox metadata from spilling into a process-wide temp
  directory outside the private run lifecycle.
- Ledger validation now bounds both individual and aggregate persisted batch
  profile sizes, preventing oversized queues from causing unbounded startup
  allocations during durable-state restore; the checks use UTF-8 byte length.
- Terminal run details now use the durable event-size bound for both run and
  event records, preventing oversized provider errors from bloating the ledger.
- Batch parent runs now retain a bounded count-and-digest plan summary while
  child runs retain their individual plans, avoiding a duplicated giant parent
  snapshot for large queues.
- Batch project creation now rejects oversized persisted profile data before
  inserting any durable project or mailbox rows.
- Headless batch retry selection now uses chunked durable state reads instead
  of issuing per-mailbox state and attention queries for large queues.
- Live IMAP metadata verification now stages source and destination records in
  a private SQLite database and reconciles them in bounded batches, avoiding
  account-sized Rust message maps while preserving bounded mismatch evidence.
- Verification-stage databases now live inside private per-run directories, so
  normal cleanup and stale-run recovery remove mailbox metadata after a crash.
- XLSX import dimension checks now fail closed when a non-empty worksheet lacks
  an early bounded dimension declaration, preventing unchecked Calamine expansion.
- Removed the obsolete string-based IMAP FETCH parser so metadata tests and live
  behavior share the byte-preserving parser.
- Bound XOAUTH2 continuation reads, error acknowledgements, and tagged results
  to the live verification deadline and operator cancellation signal.
- Added an aggregate memory bound for retained IMAP LIST folder descriptors,
  preventing many individually bounded records from exhausting verifier memory.
- Released transient fetched-page reservations immediately after SQLite staging,
  so large-account admission scales with bounded page memory instead of
  accumulating the former account-sized allowance.
- Kept SQLite reconciliation intermediates inside the private per-run stage
  database instead of process-wide temporary tables, preserving metadata
  permissions and crash cleanup boundaries.
- Capped DNS address results before connection attempts so hostile or malformed
  resolver responses cannot create an unbounded socket-address vector.
- Bounded headless status and support projections of active process identities,
  with explicit truncation reporting; full recovery scans remain unbounded only
  where inspecting every recorded process is required for safety.
- Added an aggregate mailbox-row budget to detailed headless status so many
  projects cannot combine their per-project pages into an oversized response.
- Applied the same aggregate sample budget to support-bundle mailbox exports,
  preventing large multi-project ledgers from producing oversized artifacts.
- Recovery validation now rejects cross-owner links among runs, events, active
  processes, verification acceptances, and evidence records.
- Added SQLite nonnegative constraints for process identity fields and durable
  mismatch numeric metadata, with transactional repair of older ledger tables.
- Classified IMAP folder failures as control, session-fatal, or folder-local
  outcomes so cancellation and unusable connections stop account enumeration
  promptly while recoverable folder errors retain bounded diagnostics.
- Hardened release-bundle verification against absolute, traversal, and
  backslash-containing archive member paths, with tar and ZIP regression cases.
- Release-bundle verification now rejects symlinks and special filesystem
  entries instead of following or validating them as ordinary files.
- External provider-evidence extraction now rejects unsafe or special tar
  members and refuses to overwrite existing evidence files.
- Fixed headless batch completion validation to use chunked durable mailbox
  states instead of a capped full-mailbox status read, avoiding false failures
  for batches larger than the status-page limit.
- Bounded complete project report snapshots so customer-proof and operator
  exports fail closed instead of materializing an unbounded mailbox population;
  larger projects remain available through paged status/report views.
- Removed an unnecessary full clone of the pending single-run mismatch vector
  before terminal persistence, reducing peak memory during large verifications
  while preserving retry-on-durability-failure behavior.
- Added a separate 64 MiB estimated budget for accumulated mismatch detail;
  pathological mismatch-heavy verification now fails closed for operator review
  instead of growing mismatch evidence without a process-level bound.
- Shared the immutable job/run identifiers across in-memory mismatch records,
  removing two repeated heap allocations per detail row during reconciliation
  and bounded mismatch report reads.
- Enforced the mismatch-detail budget inside each reconciliation pass, preventing
  an individual pass from exceeding the bound before its results are merged.
- Expanded the verifier admission estimate to include reconciliation indexes and
  classification sets, so expensive in-memory structures are budgeted before
  they are constructed.
- Transferred pass-level mismatch vectors into the final result with allocation
  reuse, reducing transient peak memory during reconciliation merges.
- Interned live IMAP mailbox names across metadata-page keys, reducing repeated
  folder-string allocations while preserving mailbox-local message identity.
- Removed the duplicate owned UID from live metadata records; coverage now
  validates the canonical mailbox key directly.
- Read-only and writable ledger validation now rejects orphaned foreign-key
  rows, including records imported while SQLite foreign-key enforcement was off.
- Ledger validation now rejects semantically contradictory exact-verification
  outcomes instead of trusting a forged status label over its counters.
- The terminal evidence write boundary now applies the same exact-outcome
  invariant before committing a verification-difference record.
- Explicit exact aggregate-engine evidence now also requires the
  engine-authoritative marker.
- Schema validation now verifies foreign-key actions and match modes, not
  only parent and child column names.
- IMAP folder-failure reporting now retains only a bounded cause sample while
  preserving the total failure count.
- Evidence history now enforces a foreign key from each historical result to
  its recorded run, rejecting orphaned audit claims during recovery.
- Recovery validation now rejects evidence and mismatch rows whose valid run
  belongs to a different mailbox.
- Live source and destination metadata scans now share one fetched-state
  admission budget, preventing each account from independently consuming the
  full allowance before reconciliation begins.
- Reconciliation indexes now borrow interned mailbox names instead of cloning
  a folder string for every message candidate.
- Routed budgeted IMAP authentication and metadata commands through
  cancellation-aware, timeout-retrying writes with short socket I/O slices.

### Documentation and release packaging

- Fixed the broken bulk-migration template link and added a real local
  Markdown-link validator for source documentation and release archives.
- Release staging now includes linked capability metadata and the provider
  operator documentation allowlist, so packaged documentation does not point
  outside the archive.
- Archived scheduler and message-verification design documents, maintainer
  qualification material, CI setup, and historical reports from distribution
  archives.
- Removed the duplicate Native Dovecot entry from the README's stable section
  and added manifest-backed validation for experimental-only status terms.
- Corrected Gmail qualification guidance to use the Admin console for Workspace
  user creation and distinguish Workspace OAuth, personal Gmail OAuth, and
  conditional app-password testing.
- Replaced fixed Google and Microsoft refresh-token lifetime claims with
  provider-linked lifecycle guidance and explicit reauthorization behavior.
- Added SQLite nonnegative constraints for durable counters and made signed
  integer-to-unsigned reads fail closed instead of turning negative values into
  huge evidence counts.
- Migrated existing current-version evidence tables that lacked those SQLite
  constraints, with a pre-repair backup and constraint validation on open,
  read-only, backup, and snapshot paths.
- Extended the current-schema signature to require the evidence-history
  foreign-key relationship used by recovery and reporting.
- Made SQLite snapshots copy the validated, migrated connection so restoring a
  dirty current-version ledger cannot reinstall the unrepaired source file.
- Strengthened the v12 schema signature to verify runtime index columns,
  uniqueness, and partial-index properties rather than names alone.
- Made durable mismatch readers reject negative SQLite values instead of
  silently interpreting malformed unsigned fields as absent.
- Replaced string-matched mailbox stability retries with a typed mutation
  outcome so transport and protocol failures are not retried as mailbox churn.
- Hardened XLSX dimension preflight to inspect every worksheet XML part,
  avoiding assumptions about worksheet-part numbering before Calamine parsing.
- Schema index validation now also verifies each required index is attached to
  its intended runtime table.
- Hardened XLSX dimension parsing against overflowing column references before
  applying worksheet size limits.
- Preserved duplicate IMAP SEARCH UIDs until coverage validation so malformed
  server responses cannot be silently deduplicated.
- Made IMAP LIST read retries observe operator cancellation while waiting on
  transient socket timeouts or nonblocking reads.
- Applied an absolute eight-second connection budget to IMAP greeting reads,
  including slow-drip peers during STARTTLS setup.
- Added an absolute per-command deadline to non-verification tagged IMAP
  responses so preflight/authentication reads cannot be extended by slow drips.
- Applied the same absolute deadline to XOAUTH2 continuation and result reads.
- Made restore permission/flush failures roll back the installed ledger and
  recover the preserved SQLite sidecars when possible.
- Bounded secret-file reads after opening the file, so concurrent growth cannot
  bypass the configured 64 KiB credential-file limit.
- Applied the same bounded signing-key read to the Windows signing path as to
  Unix, preventing platform-specific unbounded key allocations.
- Made Windows signing-key reads open the final path component without
  following reparse points, matching the secret-file safety boundary.
- Switched executable and trust-bundle identity hashing to fixed-size chunks,
  avoiding whole-file allocations during plan validation.
- Bounded current and legacy saved-profile reads to 1 MiB before TOML parsing.
- Bounded migration-proof reads to 32 MiB before signing or verification JSON
  parsing.
- Bounded migration-assurance snapshot inputs to 256 MiB per file before
  in-memory comparison.
- Streamed migration-assurance snapshot hashing instead of allocating a
  second full serialized snapshot-sized buffer.
- Made customer-proof aggregate counters saturating so malformed extreme
  evidence values cannot wrap into misleading totals.
- Bounded diagnostic-log filename components so oversized durable IDs cannot
  create oversized headers or evade per-file rotation limits.
- Bounded trusted extra-option input to 64 KiB and 128 parsed tokens before
  allowlist validation and command generation.
- Bounded branding and appearance preference reads before TOML parsing, so
  oversized local configuration files cannot consume unbounded memory.
- Capped project-browser query results so an all-projects refresh cannot turn
  SQLite's signed LIMIT conversion into an accidental unbounded ledger load.
- Stopped secret-free mailbox summary reads from materializing discarded
  mailbox configuration blobs in support bundles.
- Kept full mailbox configuration out of report snapshots, where report
  rendering never consumes it.
- Bounded report snapshot history reads to the latest acceptance/evidence per
  mailbox and the 20 runs rendered by operator reports.
- Kept headless status and completion checks off the full mailbox configuration
  read path; those consumers only need mailbox identity and state.
- Bounded persisted batch-profile TOML parsing and identity extraction to 1 MiB
  before deserialization.
- Bounded durable report plan-snapshot TOML parsing to 1 MiB before
  deserialization.
- Bounded OAuth refresh-configuration JSON read from the keyring to 64 KiB
  before deserialization.
- Bounded ordinary keyring credentials and OAuth access/refresh tokens to
  64 KiB before retaining them in process memory.
- Aligned report evidence selection with the ledger's captured-time ordering,
  including the deterministic ID tie-breaker.
- Restore failures while flushing a temporary snapshot now remove the
  temporary ledger before returning an error.
- Corrected the active production-status document to identify the full
  `0.1.0-alpha.1` package version used by binaries and reports.
- Added deterministic 10k/100k synthetic verifier coverage at 0%, 10%, and
  100% mismatch rates to guard the indexed reconciliation path at scale.
- Bounded opt-in diagnostic transcripts with buffered checkpoint flushing,
  per-file rotation, and a total diagnostic-directory size cap.
- Applied the pinned cargo-deny advisories, bans, licenses, and sources policy
  to pull-request CI and release publication gates.
- Added the capability-claim validator to the local `make check` path and
  explicitly exclude repository metadata from host-native package archives.
- Updated dependency-audit documentation to reflect pull-request and release
  enforcement, not only the scheduled audit.
- Updated the cargo-deny pin to 0.20.2 so current RustSec CVSS 4.0 advisories
  are parsed before policy checks run.
- Indexed wrong-folder reconciliation by metadata and ordered folder ranges to
  avoid rescanning every folder for each duplicate Message-ID candidate.
- Indexed duplicate Message-ID content and metadata matching with per-group
  queues, avoiding repeated linear scans of large duplicate groups.
- Capped core mailbox pages at 1,000 rows, mailbox status exports at 100,000
  rows, and verification/run-list pages at 1,000 rows before passing limits
  to SQLite.
- Capped detailed headless status mailbox output at 10,000 rows per project
  and added an explicit truncation indicator; summary status remains exact.

## [0.1.0-alpha.1] - 2026-09-25

MailSwiftSync 0.1.0-alpha.1 is a technical preview for controlled operator
pilots. It is not a general-availability or unattended-production release.
See [the release note](docs/release-0.1.0-alpha.md),
[production status](PRODUCTION_STATUS.md), and the
[release-readiness criteria](docs/release-readiness.md) for scope and open
gates.

### Added

- Durable project, mailbox, run, recovery, evidence, and operator-review
  workflows with local SQLite state and secret-free reports.
- Dovecot-native and imapsync transfer engines, with local `doveadm`
  execution and bounded metadata verification for encrypted imapsync runs.
- Cross-platform portable release archives, checksums, SBOMs, and build
  provenance artifacts.
- Capability, compatibility, security, and release-readiness documentation
  backed by automated drift and validation checks.

### Changed

- Hardened v12 schema validation, backup, restore, and read-only recovery;
  malformed or non-MailSwiftSync SQLite files fail closed.
- Reworked message reconciliation to use indexed bookkeeping, prepared
  mismatch persistence, normalized IMAP dates, and bounded evidence detail.
- Added bounded IMAP DNS resolution, connection/read/write deadlines, LIST and
  cancellation budgets, incremental response framing, folded Message-ID
  parsing, and bounded folder failure details.
- Made plaintext-secret import require exactly
  `MAILSWIFTSYNC_ALLOW_PLAINTEXT_SECRETS=1` and tightened bulk-import limits.
- Removed dormant remote-Dovecot SSH configuration and command generation;
  native Dovecot execution is explicitly local-only until a secret broker
  exists. Legacy profiles requesting remote execution fail closed.
- Set the package version and release identity to `0.1.0-alpha.1`; release
  tags must exactly match the Cargo package version.

### Security and integrity

- Credentials remain session-only at the application layer and are delivered
  through private files or local child environments as appropriate.
- Ledger, evidence, process identity, enum, signed-integer, and filesystem
  validation fail closed on malformed persisted state.
- Release and capability documentation gates reject contradictory production,
  GA, and support claims.

### Known limitations

- Hosted-provider live pilots, very-large-mailbox scale evidence, and full
  body-content verification remain release-readiness gates.
- Verification currently materializes bounded metadata state in memory;
  SQLite-backed streaming reconciliation is still required for very large
  migrations.
- Native Dovecot verification remains aggregate-level, and remote Dovecot
  execution is intentionally unsupported.
