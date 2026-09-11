# Sourcecraft IMAP Sync

> A local-first Rust desktop console for safe, deliberate `imapsync` migrations.

Sourcecraft IMAP Sync is independently designed from scratch and does not reuse or inspect any other IMAP GUI. It configures a source and destination mailbox, produces a redacted command preview, saves non-secret profiles, runs dry validation by default, and streams `imapsync` output.

## Install

### 1. Install `imapsync`

Sourcecraft is a GUI for a locally installed `imapsync`; it does not download or operate a remote sync service for you.

- **Ubuntu/Debian:** download the current `.deb` from the official imapsync distribution, then install it with `sudo apt install ./imapsync-*.deb`.
- **macOS:** install imapsync using the vendor distribution or your approved package-management workflow.
- **Windows:** install the official Windows package and enter the full path to `imapsync.exe` in Sourcecraft.

Verify the installation in a terminal before configuring accounts:

```bash
imapsync --version
```

Consult the [official imapsync installation documentation](https://imapsync.lamiral.info/#install) for current packages and prerequisites.

Sourcecraft itself uses Rustls with bundled WebPKI certificate roots for its TLS preflight probe. It does **not** require OpenSSL development headers or `pkg-config` to build.

### 2. Build and run Sourcecraft

```bash
cargo run --release
```

If `imapsync` is not on your PATH, enter its absolute path in **imapsync executable**. Begin with **Dry run** enabled and a test destination mailbox.

### 3. First migration

1. Enter source details on the left and destination details on the right.
2. Leave **Dry run** selected and click **Preview redacted command**.
3. Run validation and inspect the execution journal for successful logins and folder mapping.
4. Only then disable Dry run and launch a live migration.

Use **Project cockpit → Discover server capabilities over verified TLS** before a pilot to see whether each endpoint advertises modern IMAP extensions such as QRESYNC, CONDSTORE, UIDPLUS, and SPECIAL-USE. This probe does not authenticate and does not send account passwords.

## Security model

- **Local first.** The app does not send mail data itself; it invokes your local `imapsync` executable only when you start a run.
- **No saved passwords.** Profiles retain only server, username, and selected options. Password fields begin empty on every launch.
- **Safe by default.** Dry mode adds `--dry`, which validates connectivity and proposed folder mapping without changing the destination.
- **Redacted preview.** Passwords are hidden in the preview. Be aware that `imapsync` itself receives passwords during the active process; run it under an account with appropriate process visibility controls.

### Current security boundary

The current desktop runner is a prototype bridge to the local `imapsync` executable. It does not persist passwords, but imapsync receives them during its active process. The production control-plane architecture is being introduced with a no-secret SQLite project ledger; OS-keyring/OAuth credential handling and a native IMAP worker are required before treating this as an enterprise release.

## Verification

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release
```

## Documentation

Begin with the [Sourcecraft IMAP Migrator Wiki](docs/wiki/Home.md) for illustrated, step-by-step setup and migration guidance.

For the durable project, phase, and verification model, see the [control-plane architecture](docs/architecture.md).

For batch work, see [Bulk migrations from CSV or Excel](docs/wiki/Bulk-migrations.md) and start from the included template. Never commit a populated spreadsheet containing passwords.

## Advanced options

Click **Advanced options** to add common imapsync flags with understandable descriptions: internal-date sync, UID matching, cache usage, fast I/O, and size-mismatch tolerance. The `--delete2` control is visually marked destructive because it can remove destination messages that do not exist on the source.

The **Extra imapsync options** field accepts any additional documented imapsync options. Test every change using Dry run first. The command preview shows the final arguments with passwords redacted.

## Packaging

Run `scripts/package.sh` from anywhere inside the checkout to create a host-native tarball and SHA-256 checksum in the repository's `dist/` directory. Signing and platform-native installers require your own release keys and distribution policy.
