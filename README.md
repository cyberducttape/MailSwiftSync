# MailSwiftSync

> A local-first mailbox migration control plane: plan, execute, verify, and audit bulk migrations with the best available engine.

[![CI](https://github.com/itchyitchy123/MailSwiftSync/actions/workflows/ci.yml/badge.svg)](https://github.com/itchyitchy123/MailSwiftSync/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/built%20with-Rust-orange.svg)](https://www.rust-lang.org/)

![Migration plan](docs/wiki/assets/migration-plan.png)

MailSwiftSync is for hosting administrators, consultants, and MSPs moving multiple mailboxes between IMAP systems who need more than a command wrapper: a preflightable plan, controlled execution, restart-aware state, and evidence they can hand to a customer.

MailSwiftSync helps administrators and MSPs plan, execute, verify, and audit mailbox migrations. It selects the migration engine based on the environment: Dovecot destinations can use native `doveadm`/dsync with a remote IMAP source through `imapc`; arbitrary IMAP-to-IMAP migrations can use a locally installed `imapsync` executable. Both paths provide a redacted plan, durable project state, phased execution, and an operator journal.

The product value is the control plane around the transfer engine: endpoint checks, pilot and cutover planning, mailbox scope, controlled execution, durable run records, and evidence-led verification. The central workflow is **Plan → Preflight → Execute → Verify → Audit**.

## Project status

MailSwiftSync is an early, usable 0.1 development release aimed at technical operators. The durable project ledger, dry-run safety gate, Dovecot/imapsync engine selection, streaming execution, and aggregate verification evidence are available today. Treat credential delivery, packaged installers, high-volume scheduling, message-level reconciliation, and unattended production operation as experimental or planned until the relevant release criteria are published.

Stable today:

- Dovecot-native `doveadm sync -1`/`backup` planning and execution, including remote `imapc` sources.
- `imapsync` fallback for arbitrary IMAP endpoints.
- CSV/XLS/XLSX validation-only batch queue with bounded operator-selected concurrency (1–16 workers).
- Bounded transient retry policy for batch validation with cancellation-aware backoff.
- Durable project phases, mailbox states, redacted events, run IDs, and verification evidence.
- Optional OS-keyring password references; keyring IDs are saved, while password material remains outside the profile and SQLite ledger.
- Dry-run default, explicit live confirmation, timeout, cancellation, and destructive-option warnings.

Experimental or planned:

- Provider-specific OAuth/Modern Auth and unattended secret brokering.
- Native installers, signed releases, and cross-platform binary distribution.
- Live migration concurrency, retry/resume checkpoints, maintenance windows, throttling, and scheduler/API operation.
- UIDVALIDITY-aware delta checkpoints and message-level mismatch reports.
- Published large-scale migration case studies and compatibility matrix.

## Why use this instead of the alternatives?

| Approach | Good at | What MailSwiftSync adds or avoids |
| --- | --- | --- |
| Raw `imapsync` or shell scripts | Flexible one-off transfers | Durable project state, safety gates, bounded queues, and exportable evidence |
| Native Dovecot `dsync` | Dovecot-to-Dovecot semantics and incremental sync | A guided plan, operator workflow, and verification around the native engine |
| Hosted migration SaaS | Broad provider coverage and managed execution | Local data flow, no per-mailbox SaaS fee, and inspectable local records |
| MailSwiftSync | Local IMAP migration operations | Uses proven engines while owning planning, preflight, recovery, and audit output |

Choose native Dovecot tooling directly when you already have a well-tested server-side workflow and do not need a desktop control plane. Choose MailSwiftSync when you need to coordinate and document a heterogeneous or multi-mailbox migration locally. It is deliberately not a hosted service and does not replace the engines' own mailbox semantics.

## Install

### 1. Install the selected migration engine

MailSwiftSync does not bundle or operate a remote sync service. Install the engine appropriate to the destination:

- **Dovecot destination:** provide `doveadm` on the destination host, either locally or through the configured SSH wrapper. The destination administrator must permit the `imapc` source connection.
- **Arbitrary IMAP destination:** install `imapsync` locally.

- **Ubuntu/Debian:** download the current `.deb` from the official imapsync distribution, then install it with `sudo apt install ./imapsync-*.deb`.
- **macOS:** install imapsync using the vendor distribution or your approved package-management workflow.
- **Windows:** install the official Windows package and enter the full path to `imapsync.exe` in MailSwiftSync.

Verify an imapsync installation in a terminal before configuring accounts:

```bash
imapsync --version
```

Consult the [official imapsync installation documentation](https://imapsync.lamiral.info/#install) for current packages and prerequisites.

MailSwiftSync itself uses Rustls with bundled WebPKI certificate roots for its authenticated IMAPS readiness probe. The probe validates the certificate, authenticates, refreshes capabilities after authentication, and inspects namespace/folder listing; it does **not** require OpenSSL development headers or `pkg-config` to build. Dovecot-native execution requires `doveadm` on the destination host (or an operator-managed wrapper/remote shell); the desktop does not install or configure Dovecot for you. Source port and source TLS mode are explicit plan fields, and long-running commands have a one-day safety timeout plus an operator cancellation control. Plain and STARTTLS plans still use the selected engine's dry preflight for authentication validation.

### 2. Download or build MailSwiftSync

For released binaries, see [GitHub Releases](https://github.com/itchyitchy123/MailSwiftSync/releases). The release workflow produces portable Linux x86_64, Windows x86_64, and macOS arm64/x86_64 archives with SHA-256 checksums. Native installers and signed artifacts are not yet published; until then, verify the checksum and use the portable archive appropriate to your platform.

For contributors or users building from source:

```bash
cargo run --release
```

If `imapsync` is not on your PATH, enter its absolute path in **imapsync executable**. Begin with **Dry run** enabled and a test destination mailbox. Tagged releases build Linux, Windows, and macOS artifacts in GitHub Actions; until a tagged release is published, Rust/Cargo is developer installation UX. Release artifacts include SHA-256 checksums.

### 3. First migration

1. Choose **Dovecot native** when the destination is managed by Dovecot and administrative access is available; otherwise choose **imapsync fallback**.
2. Enter source details on the left and destination details on the right.
3. Leave **Dry run** selected and click **Preview safe command**.
4. Run validation and inspect the execution journal for successful access and folder mapping.
5. Only then disable Dry run and launch a live migration.

In imapsync mode, use **Project cockpit → Run authenticated IMAPS readiness probe** before a pilot to verify certificates and credentials, refresh post-auth capabilities such as QRESYNC, CONDSTORE, UIDPLUS, and SPECIAL-USE, and inspect namespace/folder listing. The probe is limited to dual-IMAPS plans; plain and STARTTLS plans use the selected engine's dry preflight for authentication validation. In Dovecot mode, the destination is checked through the native `doveadm` dry preflight and does not require a destination IMAP password.

## Why MailSwiftSync exists

Bulk migration alone is not the differentiator: scripts and existing IMAP tools can already loop over accounts. MailSwiftSync is intended to answer the operational questions that matter during a migration window: which engine fits this destination, which accounts are ready, what failed, what needs a delta, and can the final result be demonstrated to another administrator?

## Security model

- **Local first.** The app does not relay mail data through a MailSwiftSync service; it invokes the selected local or destination-side engine only when you start a run.
- **No saved passwords.** Profiles retain only server, username, and selected options. Password fields begin empty on every launch.
- **Safe by default.** Dry mode adds `--dry`, which validates connectivity and proposed folder mapping without changing the destination.
- **Redacted preview.** Passwords are hidden in the preview. imapsync live runs receive credentials through short-lived owner-only `--passfile1/--passfile2` files; local Dovecot runs use `MAILSWIFTSYNC_IMAPC_PASSWORD` through Dovecot's `$ENV:` expansion, while remote Dovecot runs still use a destination-side `imapc_password` override and should be treated accordingly.
- **Explicit transport policy.** imapsync plans force encrypted source/destination transport (`--ssl1/--ssl2` for IMAPS or `--tls1` for STARTTLS) instead of allowing automatic cleartext fallback. Plain source transport is an explicit warning and is never presented as a verified TLS plan.

### Current security boundary

The desktop runner does not persist passwords. You may enter a password for the current session or load it through an OS-keyring ID. imapsync credentials are written to short-lived owner-only passfiles and removed after the child exits. Local Dovecot credentials use a child environment variable and Dovecot config expansion. Remote Dovecot execution is disabled by default because its current compatibility path uses `-o imapc_password=...`, which can expose the secret through process inspection on the destination host; an explicit acknowledgement is required to opt in. Provider-specific OAuth/Modern Auth and unattended secret brokering are not implemented yet. Never put real passwords in a committed CSV.

### Dovecot mode

Dovecot mode configures the destination-side command in the form `doveadm ... sync -1Ru DESTINATION imapc:`. This is an additive final-delta-safe default. After a live run, MailSwiftSync queries both sides with `doveadm mailbox status` and stores aggregate folder/message/virtual-size evidence. Enabling destination deletion selects `doveadm backup`, which makes the destination mirror the source and can remove destination-only mail. The dry command performs a non-mutating `imapc` mailbox listing against the source; it is a connectivity/configuration check, not proof that the full migration will succeed.

## Verification

Verification is a primary product feature, not a process-exit decoration. After a live run, the project ledger records the available source/destination folder counts, message counts, virtual sizes, failures, warnings, and confidence result. A successful process with incomplete evidence remains pending review. Aggregate evidence is not a substitute for message-level reconciliation; that distinction is explicit in the architecture and release criteria. Export both human-readable Markdown and secret-free structured JSON project reports. Live execution is also bound to the exact secret-free plan captured by a successful dry preflight, so changing endpoints, users, engine, TLS, or controlled options requires preflight again.

![Batch migration review](docs/wiki/assets/batch-queue.png)

The on-screen execution journal is intentionally capped at 10,000 lines for desktop stability; the redacted durable event ledger remains the longer-lived audit record.

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release
```

## Documentation

MailSwiftSync is distributed under the [MIT License](LICENSE).

Begin with the [MailSwiftSync Wiki](docs/wiki/Home.md) for illustrated, step-by-step setup and migration guidance.

For the durable project, phase, and verification model, see the [control-plane architecture](docs/architecture.md).

For batch work, see [Bulk migrations from CSV or Excel](docs/wiki/Bulk-migrations.md) and start from the included template. Never commit a populated spreadsheet containing passwords.

See the [release-readiness criteria](docs/release-readiness.md) for the boundary between the current operator-focused 0.1 release and production-ready 1.0 work.

## Advanced options

Click **Advanced options** to add common imapsync flags with understandable descriptions: internal-date sync, UID matching, cache usage, fast I/O, and size-mismatch tolerance. The `--delete2` control is visually marked destructive because it can remove destination messages that do not exist on the source.

The **Extra imapsync options** field accepts additional non-connection imapsync options. Endpoint, credential, TLS, dry-run, and destructive deletion flags are controlled by the plan and rejected from this field. Test every change using Dry run first. The command preview shows the final arguments with passwords redacted.

## Packaging

Run `scripts/package.sh` from anywhere inside the checkout to create a host-native tarball and SHA-256 checksum in the repository's `dist/` directory. Signing and platform-native installers require your own release keys and distribution policy.
