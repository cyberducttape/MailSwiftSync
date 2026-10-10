# Contributing

## Development

Use stable Rust and validate changes before opening a pull request. The
canonical local gate is:

```bash
make check
make test
```

For the disposable product lab and the release-quality subset, use:

```bash
make integration
make release-check
```

The integration target requires the packaged IMAP lab prerequisites. The
release target also requires `cargo-audit`; signing, SBOM publication, and
cross-platform builds remain CI/release-environment responsibilities.

The underlying commands are:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release
```

## Gates that `make check` does not run

CI enforces a few checks outside the local gate; run them when your change
touches the affected area:

- **Critical-module coverage.** Line and branch floors for safety-critical
  modules are listed in `scripts/check-critical-coverage.py`. Reproduce with
  `cargo +1.92.0 llvm-cov --locked --all-targets --all-features --lcov --output-path cov.lcov`,
  the same command with the pinned nightly and `--branch` into
  `branches.lcov`, then
  `python3 scripts/check-critical-coverage.py cov.lcov --branch-lcov branches.lcov`.
  Do not edit sources while llvm-cov is building; mixed objects report
  misleading coverage. Raise coverage with focused tests, never by lowering a
  floor.
- **Scale budgets.** After touching the Mailboxes page, the queue read model,
  or queue SQL, run `scripts/benchmark-ui-scale.sh` (release build, 100k rows
  with a child run per mailbox, plus sustained updates) and
  `scripts/benchmark-import-scale.sh`.
- **Integration lab.** `scripts/imap-integration-smoke.sh` drives the packaged
  binary against disposable Dovecot servers in the release container image;
  CI runs it on every push to `main`.

## Schema, locale, and contract changes

- A ledger schema change bumps `CURRENT_SCHEMA_VERSION` and must update the
  migration in `src/core/database.rs`, `validate_schema_layout` (tables,
  columns, indexes, triggers, CHECK constraints) in
  `src/core/database/schema.rs`, any derived-data invariant in
  `src/core/database/invariants.rs`, and documents that name the schema
  version (`python3 scripts/verify-capability-claims.py --write`).
- UI copy lives in `locales/en.toml`, `locales/de.toml`, and `locales/id.toml`
  under stable keys; add every new key to all three catalogs.
  `scripts/verify-ui-localization.py` checks key, placeholder, and
  source-copy parity.
- `status`, `status --summary`, and `fleet-status` JSON is a versioned
  contract. Additive fields go into the fixtures under
  `tests/fixtures/automation/`; removing, renaming, or retyping a field needs
  a new `format_version` (see [docs/automation-contract.md](docs/automation-contract.md)).
- Capability claims come from `capabilities.toml`; regenerate the generated
  tables rather than editing them by hand.

## Expectations

- Keep synchronization runs explicit and avoid background network activity.
- Do not add secret storage, analytics, or external services without documenting the change and obtaining maintainer approval.
- Keep dry-run mode the default and clearly identify any option that can modify a destination mailbox.
- Add tests for profile validation and command-construction logic where practical.
