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
