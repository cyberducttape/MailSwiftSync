# Changelog

All notable changes to MailSwiftSync are documented here. Detailed pre-alpha
development history is preserved in repository history and is not shipped in
operator distribution archives.

## [Unreleased]

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
