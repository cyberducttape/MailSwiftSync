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

- Keep Git operations explicit and avoid background network activity.
- Do not add secret storage, analytics, or external services without documenting the change and obtaining maintainer approval.
- Place destructive Git actions behind a clear confirmation and describe their recovery path.
- Add tests for parsing and command-construction logic where practical.
