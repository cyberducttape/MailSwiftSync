# Sourcecraft IMAP Sync

> A local-first migration control plane: Dovecot-native execution when possible, `imapsync` when necessary.

Sourcecraft helps operators plan, execute, record, and verify mailbox migrations. When the destination is a Dovecot server, it can run Dovecot's native `doveadm`/dsync workflow with a remote IMAP source through `imapc`. For arbitrary IMAP-to-IMAP migrations it falls back to a locally installed `imapsync` executable. Both paths provide a redacted command plan, durable project state, phased execution, and an operator journal.

The important distinction is that Sourcecraft does not try to replace Dovecot's migration engine. It makes the surrounding migration work repeatable: endpoint checks, pilot and cutover planning, mailbox scope, durable run records, and evidence-led verification.

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

Sourcecraft itself uses Rustls with bundled WebPKI certificate roots for its TLS preflight probe. It does **not** require OpenSSL development headers or `pkg-config` to build. Dovecot-native execution requires `doveadm` on the destination host (or an operator-managed wrapper/remote shell); the desktop does not install or configure Dovecot for you.

### 2. Build and run Sourcecraft

```bash
cargo run --release
```

If `imapsync` is not on your PATH, enter its absolute path in **imapsync executable**. Begin with **Dry run** enabled and a test destination mailbox.

### 3. First migration

1. Choose **Dovecot native** when the destination is managed by Dovecot; otherwise choose **imapsync fallback**.
2. Enter source details on the left and destination details on the right.
3. Leave **Dry run** selected and click **Preview safe command**.
4. Run validation and inspect the execution journal for successful access and folder mapping.
5. Only then disable Dry run and launch a live migration.

Use **Project cockpit → Discover server capabilities over verified TLS** before a pilot to see whether each endpoint advertises modern IMAP extensions such as QRESYNC, CONDSTORE, UIDPLUS, and SPECIAL-USE. This probe does not authenticate and does not send account passwords.

## Security model

- **Local first.** The app does not send mail data itself; it invokes your local `imapsync` executable only when you start a run.
- **No saved passwords.** Profiles retain only server, username, and selected options. Password fields begin empty on every launch.
- **Safe by default.** Dry mode adds `--dry`, which validates connectivity and proposed folder mapping without changing the destination.
- **Redacted preview.** Passwords are hidden in the preview. imapsync live runs use ephemeral protected passfiles; Dovecot's remote `imapc_password` override is still visible to the destination-side process and should be treated accordingly.

### Current security boundary

The desktop runner does not persist passwords. imapsync credentials are written to short-lived mode-600 files and removed after the child exits; Dovecot credentials currently use `-o imapc_password=...`, which can expose the secret through process inspection on the destination host. Treat this as an operator workstation tool until OS-keyring/OAuth delivery or an equivalent secret broker is added. Never put real passwords in a committed CSV.

### Dovecot mode

Dovecot mode configures the destination-side command in the form `doveadm ... sync -1Ru DESTINATION imapc:`. This is an additive final-delta-safe default. After a live run, Sourcecraft queries both sides with `doveadm mailbox status` and stores aggregate folder/message/virtual-size evidence. Enabling destination deletion selects `doveadm backup`, which makes the destination mirror the source and can remove destination-only mail. The dry command performs a non-mutating `imapc` mailbox listing against the source; it is a connectivity/configuration check, not proof that the full migration will succeed.

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
