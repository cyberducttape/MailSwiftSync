# Sourcecraft IMAP Sync

> A local-first Rust desktop console for safe, deliberate `imapsync` migrations.

Sourcecraft IMAP Sync is independently designed from scratch and does not reuse or inspect any other IMAP GUI. It configures a source and destination mailbox, produces a redacted command preview, saves non-secret profiles, runs dry validation by default, and streams `imapsync` output.

## Run

```bash
cargo run --release
```

Install `imapsync` separately and ensure it is on your PATH, or enter its absolute path in the application. Begin with **Dry run** enabled and a test destination mailbox.

## Security model

- **Local first.** The app does not send mail data itself; it invokes your local `imapsync` executable only when you start a run.
- **No saved passwords.** Profiles retain only server, username, and selected options. Password fields begin empty on every launch.
- **Safe by default.** Dry mode adds `--dry`, which validates connectivity and proposed folder mapping without changing the destination.
- **Redacted preview.** Passwords are hidden in the preview. Be aware that `imapsync` itself receives passwords during the active process; run it under an account with appropriate process visibility controls.

## Verification

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release
```

## Documentation

Begin with the [Sourcecraft IMAP Migrator Wiki](docs/wiki/Home.md) for illustrated, step-by-step setup and migration guidance.

## Administration

- Settings live in the operating system's standard configuration directory under `forgepad/settings.conf`. You can pin Forgepad to a managed Git binary.
- Forgepad uses existing Git authentication (SSH agent, Git credential helpers, proxy, and certificate settings). It never stores credentials.
- Network actions only occur when you explicitly use a Git network operation such as **Push**. Forgepad has no telemetry.
- The optional local audit log is stored beside the settings. It records command outcomes and repository paths; commit messages are redacted.
- Run `scripts/package.sh` on each target platform to create a host-native tarball and SHA-256 checksum. Signing and OS-native installers require your own release keys and distribution policy.
