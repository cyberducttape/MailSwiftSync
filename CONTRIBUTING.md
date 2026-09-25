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

## Expectations

- Keep synchronization runs explicit and avoid background network activity.
- Do not add secret storage, analytics, or external services without documenting the change and obtaining maintainer approval.
- Keep dry-run mode the default and clearly identify any option that can modify a destination mailbox.
- Add tests for profile validation and command-construction logic where practical.
