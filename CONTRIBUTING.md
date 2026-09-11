# Contributing

## Development

Use stable Rust and validate changes before opening a pull request:

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
