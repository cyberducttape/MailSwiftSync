# Changelog

All notable changes to MailSwiftSync are documented here. Detailed pre-alpha
development history is preserved in repository history and is not shipped in
operator distribution archives.

## [Unreleased]

- Encrypted imapsync verification now persists fetched-page cursors and opt-in
  body fingerprints in a private SQLite stage, resumes staged pages after a
  controller restart, and binds retained data to a SHA-256 plan identity.
  Successful verification or a changed plan clears the stage; provider-scale
  interruption qualification remains outstanding.
- Verification failures now surface their bounded diagnostic reason in
  operator attention state, making incomplete evidence actionable.
- The doctor command now reports engine-specific qualification requirements:
  it checks the trusted imapsync version for imapsync runs without incorrectly
  requiring imapsync for native Dovecot runs.
- German completion and safety statuses now use the translation catalog, and
  the localization guard covers additional visible UI text patterns.
- The disposable destination storage-fault lab now retries temporary-directory
  cleanup after Dovecot shutdown, preventing a socket-removal race from
  turning successful fault assertions into a CI failure.
- Added a persisted English/German language choice for the desktop interface,
  with German navigation, settings, account setup, overview, activity, and
  verification labels. CLI output and generated reports remain English.
- Support bundles no longer include customer-controlled project names; their
  redaction metadata explicitly records that project names are excluded.
- Added deterministic property-style parser tests for arbitrary IMAP response,
  endpoint, and shell-option inputs, plus UTF-8 quoted LIST-name round trips.
- Split metadata FETCH response parsing into its own IMAP parser module without
  changing the live verifier's call surface.
- Staged message pages are now inserted in canonical mailbox/UID order, making
  duplicate-Message-ID reconciliation independent of randomized HashMap order.
- Added generated differential reconciliation coverage comparing every
  semantic source/destination mismatch field across 48 deterministic cases.
- Aligned the in-memory reconciliation reference with SQLite's deterministic
  wrong-folder candidate selection and expanded staged parity checks to compare
  full source/destination folder and UID evidence.
- Updated the capability manifest to reflect that SQLite-backed streaming
  reconciliation is wired into live verification; large-account qualification
  and externally fault-injected per-message resume qualification remain
  outstanding. Durable verification-page checkpoints are now implemented.
- Microsoft 365 Basic-auth admission now identifies canonical hostnames when
  endpoints include ports or trailing dots, and no longer misclassifies hosts
  that merely contain a Microsoft domain as a substring.
- IMAP LIST quoted mailbox names now preserve UTF-8 bytes while unescaping
  quoted backslashes and quotes, preventing non-ASCII folder names from being
  corrupted during enumeration and verification.
- Documented the native Dovecot initial-backup limitation observed with
  Maildir targets that refuse INBOX replacement; the automated native-engine
  fixture covers mdbox, not Maildir. The native integration lab now also runs
  `sync -1` against an already-populated destination, covering the preservation
  command path that previously lacked product-level regression coverage.
- The native Dovecot integration lab now uses mdbox for its destination
  mailbox store because initial `doveadm backup` may need to replace INBOX,
  which Maildir cannot delete and recreate. Both fixture servers explicitly
  share a hierarchy separator, and messages are verified through
  `doveadm fetch` instead of Maildir filenames.
- Native Dovecot preflight now validates the destination userdb without
  enumerating the target mailbox store before its first sync, avoiding early
  target access that can trigger GUID/UIDVALIDITY conflicts.
- The disposable IMAP integration lab now captures bounded, redacted engine
  diagnostic logs on failure, including output produced before a headless
  command stalls or times out.
- Native Dovecot verification failures now retain the failing command's
  bounded diagnostic tail or report the parsed source/destination inventory
  counts, making incomplete status output actionable instead of generic.
- Native Dovecot source enumeration and verification now select the correct
  mail-location override for Dovecot 2.3 versus 2.4; headless failure output
  also includes the bounded verification reason instead of hiding it behind
  a generic durability message. Status fields use Dovecot's space-delimited
  field-list syntax, so 2.3 accepts the aggregate query.
- Headless durability failures now include the latest bounded persistence
  diagnostic, so integration failures identify the failed ledger operation.
- Late or foreign process-output events are discarded without marking the
  durable migration result as failed; those events are presentation-only.
- A completed single-mailbox project can be reopened for an incremental
  headless pass only with an explicit `--reopen-reason`; reopening verifies
  the prior queue is fully verified and has no active run, and records the
  operator-provided reason in the ledger.
- The Dovecot product-lab artifact now includes its bounded fetched Message-ID
  sample and fixture counts when destination assertions fail.
- Sparse-UID lab fixtures now use zero-padded Maildir filenames so Dovecot's
  lexical scan order matches numeric fixture order before expunging UIDs; the
  retained-message assertion checks UID-associated fixtures 91 and 100.
- The distributed Linux runtime now includes `procps`, which packaged
  imapsync invokes for process inspection; the product integration lab fails
  early if `ps` is missing.
- The packaged-imapsync integration fixture now enables bounded diagnostic
  output and a 30-second engine protocol timeout to locate the dry-run shutdown
  stall before the outer lab watchdog expires; a bounded live thread/process-
  group snapshot is captured after 60 seconds if a product command is still running.
- Integration-only process supervision diagnostics now distinguish a child
  wait stall, blocked stdout/stderr drain, terminal-event delivery, and
  repeated terminal persistence failure without routine product output when
  the opt-in environment switch is unset.
- Successful single and batch preflights now atomically persist engine
  observations with their immutable plan digest while keeping the mailbox
  `ready`; preflight statistics cannot be mistaken for verification.
- Maildir integration assertions now inspect only `cur/` and `new/` messages,
  excluding Dovecot index/cache files from fixture message counts.
- Provider qualification now normalizes durable operator-facing engine labels
  (such as `imapsync fallback`) to canonical evidence engine identifiers.
- The small generic IMAP integration lab now uploads its verified product
  proof without mislabeling it as provider-qualification evidence.
- The disposable destination storage-fault fixture now runs with the
  privileges required to prepare Dovecot-owned test mail storage and locates
  the Dovecot worker without aborting on absent library directories. Controller
  state and runtime files now live in an owner-matched private subdirectory;
  the file-size limit allows server metadata writes while rejecting the target
  oversized-message fixture, and the expected absent-message search is safe
  under strict shell error handling.
- Dovecot native transfers now use a private runtime config that includes the
  destination config and reads the source credential from an owner-only file;
  the password stays out of process arguments and the profile.
- Fixed Dovecot source mailbox verification command ordering so TLS and
  `-o` overrides are passed before the `mailbox status` subcommand.
- Fixed Dovecot live command ordering: dsync-specific `-l`, `-s`, and `-1`
  options now follow `sync`/`backup`, avoiding a Dovecot 2.3 global-option
  parse failure before migration starts.
- The Dovecot integration fixture now selects `initial_mirror` explicitly, so
  its initial transfer exercises `doveadm backup` rather than inheriting the
  final-preservation `sync -1` default.
- Strengthened the archive-layout regression test to inspect the actual
  macOS/Linux/Windows release packaging commands before validating extracted
  archive roots.
- The Dovecot integration fixture now asserts sparse-mailbox ownership and
  owner-write permission before transfer, failing immediately with the actual
  UID and mode instead of timing out later in the migration.
- Expanded `make check` to include documentation-claim fixtures, local Markdown
  link validation, release-channel and archive-layout tests, and the same
  preview compatibility gate used by CI.
- Restored Dovecot service-account ownership of the sparse Maildir after
  `doveadm expunge`, which can create or rewrite root-owned mailbox metadata.
- Staged verifier fallback lookups now index each message by its reconciled
  mailbox, normalized date, and size, avoiding repeated cross-folder scans
  when many folders share common metadata.
- Diagnostic transcripts now request owner-only permissions atomically when
  each file is created on Unix, avoiding a brief umask-dependent exposure
  before permissions are tightened.
- Fixed the sparse-UID Dovecot fixture ownership after creating its Maildir
  tree, so the Dovecot service user can assign UIDs and expunge the fixture
  messages during integration setup.
- Schema validation now parses actual SQLite `CHECK` expressions instead of
  accepting matching text embedded in defaults or comments; writable repair
  and read-only validation use the same strict constraint signature.
- Closed SQLite handles before removing temporary database fixtures, fixing
  Windows test failures caused by platform-specific file locking.
- Fixed integration artifact-directory setup so the host runner applies
  private permissions before handing ownership to the container user.
- The disposable Dovecot integration lab now explicitly runs its fixture
  provisioning as root and creates a matching private runtime directory;
  product state, configuration, secrets, and proof outputs live under that
  private directory, and the distributed image still defaults to its
  unprivileged service account.
- Native Dovecot integration failures now upload the same bounded, redacted
  environment and server/controller diagnostics as the packaged IMAP lab.
- Recovery schema validation now checks the `WHERE` predicates of partial
  unique run indexes, rejecting same-named indexes that do not enforce active
  run ownership; writable recovery backs up and rebuilds malformed indexes.
- Webhook notifications now default to credential-free operational status with
  customer names, endpoint hosts, and active-process details excluded; sending
  customer metadata requires the explicit `--include-customer-metadata` option.
- Removed the verifier-wide dead-code exemption so unused production verifier
  paths are reported by normal compiler and lint checks.
- Batch verification failures now persist an explicit verification-incomplete
  reason, and bounded status summaries/webhooks include aggregate counts by
  durable attention reason in deterministic key order.
- Moved the message-verification test suite into its own module file so the
  production reconciliation implementation is easier to review independently.
- Moved the large core persistence test suite out of `core.rs`; the root now
  serves as a compact module index over database, state, evidence, report, and
  policy responsibilities.
- The disposable IMAP lab now records bounded runtime/dependency diagnostics
  before early exits and uploads environment and server/controller diagnostics
  when a product transfer fails.
- New profiles disable imapsync automapping by default so independent message
  verification can certify live migrations; headless live rejects plans that
  cannot provide required verification evidence.
- Batch migrations now persist verification-incomplete detail with the
  durable terminal result instead of reducing the reason to an output line.
- XLSX early-dimension inspection now uses a bounded streaming XML parser and
  resolves namespace-qualified OOXML element and attribute names by local name.
- The disposable IMAP integration artifact now retains MailSwiftSync command
  output when a smoke run fails, enabling diagnosis without retaining fixture
  credentials or private server keys.
- Startup now immediately removes leftover credential run directories when
  durable process recovery proves there are no surviving or unverified owners;
  ambiguous ownership retains the seven-day cleanup fallback.
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
- XLSX imports now validate and parse through one bounded no-follow file
  handle, preventing path replacement between archive checks and parsing.
- XLSX import now validates actual cell coordinates and dense range area before
  Calamine allocates its cell matrix. Legacy XLS import is disabled because its
  parser materializes the matrix before those resource bounds can be enforced.
- Release compatibility and evidence gates now share explicit prerelease
  classification, including numbered alpha tags such as `v0.1.0-alpha.1`.
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
