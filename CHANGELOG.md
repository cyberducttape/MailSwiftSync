# Changelog

All notable changes to Sourcecraft IMAP Sync are documented here.

## [Unreleased]

### Added

- Added explicit migration engine selection with Dovecot-native `doveadm`/`imapc` execution and an `imapsync` fallback.
- Added local and SSH-based Dovecot destination execution with non-interactive SSH and shell-quoted remote arguments.
- Added Dovecot post-run mailbox reconciliation using folder, message, and virtual-size status.
- Added durable redacted run output, lifecycle events, mailbox states, and verification evidence.
- Added `imapsync` summary parsing for automatic evidence capture.
- Added ephemeral mode-600 credential passfiles for live `imapsync` runs.
- Added regression coverage for command generation, credential handling, evidence parsing, and verification confidence.

### Changed

- Dovecot migrations default to additive `sync -1`; destination mirroring requires explicit destructive configuration.
- Automatic engine selection is conservative and uses `imapsync` unless Dovecot mode is explicitly selected.
- Updated operator, architecture, and security documentation to describe the native Dovecot workflow and its limitations.

### Security

- Passwords remain excluded from saved profiles and the SQLite ledger.
- Dovecot source credentials still use a destination-side `imapc_password` override and may be visible to process inspection; OS-keyring/OAuth delivery remains future work.
