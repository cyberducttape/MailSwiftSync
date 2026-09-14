# MailSwiftSync

> A local-first mailbox migration control plane: plan, execute, verify, and audit bulk migrations with the best available engine.

[![CI](https://github.com/itchyitchy123/MailSwiftSync/actions/workflows/ci.yml/badge.svg)](https://github.com/itchyitchy123/MailSwiftSync/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/built%20with-Rust-orange.svg)](https://www.rust-lang.org/)

![Migration plan](docs/wiki/assets/migration-plan.png)

> Documentation images are current workflow previews, not pixel-accurate screenshots of the egui interface. They illustrate the intended operator flow and emphasize explicit safety state, lifecycle visibility, redacted command review, and diagnostics.

MailSwiftSync is for hosting administrators, consultants, and MSPs moving multiple mailboxes between IMAP systems who need more than a command wrapper: a preflightable plan, controlled execution, restart-aware state, and evidence they can hand to a customer.

MailSwiftSync helps administrators and MSPs plan, execute, verify, and audit mailbox migrations. Select Dovecot native execution when the destination is Dovecot and administrative access is available; otherwise the conservative default uses a locally installed `imapsync` executable for arbitrary IMAP-to-IMAP migrations. Both paths provide a redacted plan, durable project state, phased execution, and an operator journal.

The product value is the control plane around the transfer engine: endpoint checks, pilot and cutover planning, mailbox scope, controlled execution, durable run records, and evidence-led verification. The central workflow is **Plan → Preflight → Execute → Verify → Audit**.

## Project status

MailSwiftSync is an early, usable 0.1 development release aimed at technical operators. The durable project ledger, dry-run safety gate, Dovecot/imapsync engine selection, streaming execution, and aggregate verification evidence are available today. Treat credential delivery, packaged installers, high-volume scheduling, message-level reconciliation, and unattended production operation as experimental or planned until the relevant release criteria are published.

Stable today:

- Dovecot-native `doveadm sync -1`/`backup` planning and execution, including remote `imapc` sources.
- `imapsync` fallback for arbitrary IMAP endpoints.
- CSV/XLS/XLSX batch queue with bounded operator-selected concurrency (1–16 workers), explicit worksheet selection for workbooks, preflight gates, live execution confirmation, cancellation, retries, and restart-visible child states.
- Explicit imapsync message and byte throttles for provider-friendly single-mailbox runs.
- Configurable per-process timeout (1–720 hours) so large mailboxes can run longer than the default while hung jobs remain bounded.
- Bounded transient retry policy for batch validation with cancellation-aware backoff.
- Actionable failure classification in worker output and durable run details: authentication, quota, transport, configuration, message, or unknown.
- Durable project phases, mailbox states, redacted events, run IDs, and verification evidence.
- Optional OS-keyring password references; keyring IDs are saved, while password material remains outside the profile and SQLite ledger.
- Dry-run default, explicit live confirmation, timeout, cancellation, and destructive-option warnings.
- Running jobs show elapsed time and can be stopped through an explicit confirmation; Advanced options include contextual guidance for per-process throttles.

Experimental or planned:

- Interactive provider consent/authorization and unattended secret brokering
  (the imapsync path accepts operator-supplied OAuth 2.0 access tokens
  through the OS keyring or session form, and can automatically refresh them
  from an operator-supplied refresh token; it still does not implement an
  authorization flow, so the operator obtains that refresh token through the
  provider's own tooling).
- Native installers, signed releases, and cross-platform binary distribution.
- A scheduler/API that can survive the desktop closing, and message-level verification for live batches. (`supervise` provides a foreground, maintenance-window-aware batch controller; see below.)
- UIDVALIDITY-aware delta checkpoints and message-level mismatch reports.
- Published large-scale migration case studies and compatibility matrix.

The current tested scope and explicit gaps are tracked in the
[compatibility matrix](docs/compatibility-matrix.md); an entry is not treated
as supported until its dry/live/recovery/evidence gates are complete.

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

- **Dovecot destination:** provide `doveadm` on the local controller host. Remote Dovecot execution is unavailable until a secret-safe broker is implemented; the destination administrator must permit the `imapc` source connection.
- **Arbitrary IMAP destination:** install `imapsync` locally.

- **Ubuntu/Debian:** download the current `.deb` from the official imapsync distribution, then install it with `sudo apt install ./imapsync-*.deb`.
- **macOS:** install imapsync using the vendor distribution or your approved package-management workflow.
- **Windows:** install the official Windows package and enter the full path to `imapsync.exe` in MailSwiftSync.

Verify an imapsync installation in a terminal before configuring accounts:

```bash
imapsync --version
```

Consult the [official imapsync installation documentation](https://imapsync.lamiral.info/#install) for current packages and prerequisites.

MailSwiftSync itself uses Rustls with bundled WebPKI certificate roots for its authenticated IMAP readiness probe. The probe validates certificates, authenticates, refreshes capabilities after authentication, and inspects namespace/folder listing; implicit IMAPS and STARTTLS use the same fresh encrypted live-authentication path. It does **not** require OpenSSL development headers or `pkg-config` to build. Dovecot-native execution requires `doveadm` on the destination host (or an operator-managed wrapper/remote shell); the desktop does not install or configure Dovecot for you. Source port and source TLS mode are explicit plan fields, and long-running commands have a configurable 1–720 hour safety timeout plus an operator cancellation control. Plain plans remain limited to the selected engine's preflight and require explicit cleartext acknowledgement.

### 2. Download or build MailSwiftSync

For released binaries, see [GitHub Releases](https://github.com/itchyitchy123/MailSwiftSync/releases). The release workflow produces portable Linux x86_64, Windows x86_64, and macOS arm64/x86_64 archives with SHA-256 checksums, a CycloneDX SBOM, and GitHub build-provenance attestations. Native installers and platform code-signing/notarization are not yet published; until then, verify the checksum and use the portable archive appropriate to your platform.

For contributors or users building from source:

```bash
cargo run --release
```

If `imapsync` is not on your PATH, enter its absolute path in **imapsync executable**. Begin with **Preflight** selected and a test destination mailbox. Tagged releases build Linux, Windows, and macOS artifacts in GitHub Actions; if no release artifact is available for your platform, Rust/Cargo remains the developer installation path. Release artifacts include SHA-256 checksums.

### 3. First migration

1. Choose **Dovecot native** when the destination is managed by Dovecot and administrative access is available; otherwise choose **imapsync fallback**.
2. Enter source details on the left and destination details on the right.
   For imapsync, destination transport is typed separately: implicit TLS defaults to port 993 and STARTTLS defaults to port 143; enter an explicit destination port when the provider uses a nonstandard endpoint.
3. Leave **Preflight** selected and click **Preview redacted command**.
4. Run validation and inspect the execution journal for successful access and folder mapping.
5. Only then select **Live migration** and launch it.

In imapsync mode, use the readiness action in the project workspace before a pilot to verify certificates and credentials, refresh post-auth capabilities such as QRESYNC, CONDSTORE, UIDPLUS, and SPECIAL-USE, and inspect namespace/folder listing. The visible capability-discovery dialog currently presents the IMAPS inventory probe; live admission also performs the same certificate-verified authentication flow for encrypted STARTTLS plans. Plain plans use the selected engine's preflight only after explicit cleartext acknowledgement. In Dovecot mode, preflight checks the remote `imapc` source and then runs destination-side `doveadm user` and mailbox-list checks; quota capacity still requires administrative review where it is not exposed by the configured Dovecot setup.

## Why MailSwiftSync exists

Bulk migration alone is not the differentiator: scripts and existing IMAP tools can already loop over accounts. MailSwiftSync is intended to answer the operational questions that matter during a migration window: which engine fits this destination, which accounts are ready, what failed, what needs a delta, and can the final result be demonstrated to another administrator?

## Security model

- **Local first.** The app does not relay mail data through a MailSwiftSync service; it invokes the selected local or destination-side engine only when you start a run.
- **No saved passwords.** Profiles retain only server, username, and selected options. Password fields begin empty on every launch.
- **Safe by default.** Dry mode adds `--dry`, which validates connectivity and proposed folder mapping without changing the destination. Runs have durable identities; on Unix, startup reconciliation terminates recorded interrupted process groups before exposing them for retry.
- **Single-owner state.** An exclusive application lock protects the project database. If another MailSwiftSync window is open, close that window rather than deleting the lock file; the second session cannot start a migration without durable ownership.
- **Descendant containment.** Linux uses owned sessions/process groups and Windows uses a kill-on-close Job Object for engine descendants. macOS deliberately fails closed when it cannot prove ownership and should use a Unix admin host for high-stakes windows.
- **Redacted preview.** Passwords and OAuth access tokens are hidden in the preview. imapsync live runs receive password credentials through short-lived owner-only `--passfile1/--passfile2` files and OAuth credentials through private `--oauthaccesstoken1/--oauthaccesstoken2` token files; local Dovecot runs use `MAILSWIFTSYNC_IMAPC_PASSWORD` through Dovecot's `$ENV:` expansion. Remote Dovecot execution is unavailable until a secret broker can deliver credentials without destination-host process exposure.
- **Owned engine logging.** imapsync is invoked with `--nolog` by default, so its unmanaged `LOG_imapsync/` files do not become a second uncontrolled record of mailbox metadata. Use MailSwiftSync’s redacted journal and exported reports as the operational record.
- **Explicit transport policy.** imapsync plans force encrypted source/destination transport (`--ssl1/--ssl2` for IMAPS or `--tls1` for STARTTLS), request certificate verification with `SSL_verify_mode=1`, and reject expert overrides of those settings instead of allowing automatic cleartext fallback. Plain source transport is an explicit insecure warning and requires operator acknowledgement before any authenticated operation, including dry preflight; it is never presented as a verified TLS plan.

### Current security boundary

The desktop runner does not persist passwords or OAuth access tokens. For imapsync, choose **OAuth 2.0 / XOAUTH2** per endpoint and enter a currently valid access token, or load it through an OS-keyring ID; live runs write it to a short-lived owner-only token file that imapsync reads without exposing it in argv. The readiness probe performs the same XOAUTH2 authentication before live admission. MailSwiftSync does not perform provider consent (there is no in-app "sign in with Google/Microsoft" flow), so the operator must still register their own OAuth application with the provider and obtain an initial refresh token through that provider's documented flow. Once that refresh token, the token endpoint, and the client ID/secret are stored under an OS-keyring ID (in the OAuth keyring dialog's "Automatic OAuth refresh" section, separate from the plain credential entry), MailSwiftSync exchanges it for a fresh access token before every live launch, including each mailbox in a batch queue, so a long unattended run does not stall on a token that expired while it waited. Refresh-token rotation is followed automatically. Without a configured refresh entry, behavior is unchanged: operators obtain and rotate tokens by hand. Dovecot native execution currently supports password authentication only. Remote Dovecot execution is unavailable because its current compatibility path uses `-o imapc_password=...`, which can expose the secret through process inspection on the destination host. Never put real passwords, tokens, refresh tokens, or client secrets in a committed CSV.

### Dovecot mode

 Dovecot mode configures the destination-side command in the form `doveadm ... sync -l 300 -s STATE -1Ru DESTINATION imapc:`. The last committed state is reused for subsequent live passes, while the first pass supplies an empty state; `-l 300` gives another dsync operation up to five minutes to release the mailbox lock. A newly emitted state is committed atomically with the child result, and dry preflight remains non-stateful. The Dovecot engine dialog supports local `doveadm`; legacy remote-SSH profiles are shown as unavailable and cannot be promoted until secret-safe brokering exists. After a live run, MailSwiftSync queries both sides with `doveadm mailbox status` and stores aggregate folder/message/virtual-size evidence. Enabling destination deletion selects `doveadm backup`, which makes the destination mirror the source and can remove destination-only mail. Dry preflight performs a non-mutating `imapc` mailbox listing against the source plus destination user and mailbox-list checks; it is a readiness check, not proof that the full migration will succeed.

## Verification

Verification is a primary product feature, not a process-exit decoration. After a live run, the project ledger records the available source/destination folder counts, message counts, virtual sizes, failures, warnings, and evidence level. A successful process with incomplete evidence remains pending review. Exact aggregate matches can be accepted as `Aggregate match`, but they are not message-level reconciliation and are intentionally not presented as 100% proof. Aggregate mismatches are surfaced for review rather than assigned a reassuring partial score. Export both human-readable Markdown and secret-free structured JSON project reports. Live execution is also bound to the exact secret-free plan captured by a successful dry preflight, so changing endpoints, users, engine, TLS, or controlled options requires preflight again.

The structured project report is a portable Migration Proof: it contains a deterministic `proof_digest` covering the report's semantic JSON content. Verify an archived or customer-shared report independently with:

```bash
mailswiftsync verify path/to/mailswiftsync-project-report.json
```

This command does not contact either mailbox or require the application database. It verifies report integrity only; it does not upgrade aggregate evidence into message-level reconciliation. For authenticity, sign the report with an owner-only Ed25519 PKCS#8 key and verify with the pinned public key:

```text
mailswiftsync sign path/to/mailswiftsync-project-report.json path/to/operator-key.pk8 migration-change-2026-09
mailswiftsync verify path/to/mailswiftsync-project-report.json <public-key-hex>
```

Unsigned reports remain supported as integrity-only artifacts. The public key is embedded for portability, but a trust pin is required to establish that the signer is the expected operator or organization.

Run manifests also retain the engine version reported by `imapsync` or
`doveadm` when available; older wrappers that do not support `--version` are
reported as unavailable rather than inferred.

While the GUI is closed, create a consistent, integrity-checked ledger backup:

```text
mailswiftsync backup /path/to/state.db /path/to/state-backup.db
```

The command takes the same instance lock as the GUI, refuses to overwrite an existing destination, and verifies SQLite integrity before reporting success. Backups contain durable project metadata and evidence, never mailbox passwords.

To restore a verified backup, stop every controller using the ledger and run:

```text
mailswiftsync restore /path/to/state-backup.db /path/to/state.db
```

The source is validated before installation. If the destination already exists,
it is preserved as a unique `*.pre-restore-<id>.db` rollback artifact.

For automation and incident response, the same durable ledger can be inspected
or recovered without opening the GUI:

```text
mailswiftsync status /path/to/state.db
mailswiftsync status /path/to/state.db <project-id>
mailswiftsync recover /path/to/state.db
mailswiftsync support-bundle /path/to/state.db /path/to/support-bundle.json
mailswiftsync customer-proof /path/to/state.db /path/to/customer-proof.json
mailswiftsync supervise /path/to/state.db [poll-seconds] [idle-polls] [maintenance-window]
mailswiftsync headless /path/to/state.db preflight
mailswiftsync headless /path/to/state.db live
mailswiftsync headless /path/to/state.db batch-preflight
mailswiftsync headless /path/to/state.db batch-live
```

Use `mailswiftsync --help` for the complete command contract and
`mailswiftsync --version` when collecting support or audit metadata. Headless
live commands return nonzero when work remains unresolved; a zero exit status
means the requested operation reached its documented terminal condition.

For Linux headless deployments, see the [container deployment guide](docs/container.md).
The image uses `/var/lib/mailswiftsync` for durable state and an isolated
`/run/user/10001` runtime volume for short-lived secrets.

`status` emits secret-free JSON containing project/mailbox states and recorded
process identities. For large ledgers, `status --summary` emits a bounded
projection with exact per-project mailbox state counts and no mailbox rows.
`recover` takes the application lock, verifies recorded
process ownership before signalling anything, preserves identities it cannot
prove, and applies the same conservative recovery transition as GUI startup.
`support-bundle` emits a private, sanitized JSON artifact for incident triage;
it excludes endpoints, credentials, plan snapshots, command paths, mailbox
content, and diagnostic text while retaining schema, platform, health, and
bounded run metadata. For large projects it includes exact mailbox totals and
state counts plus a clearly marked sample of at most 1,000 mailbox statuses.
`customer-proof` emits the same customer-safe proof artifact available from the
GUI and refuses to export until the selected project is durably complete with
verified mailbox evidence. For an explicitly labeled progress artifact only,
pass `--allow-incomplete`; that artifact is never a completion certificate.
It excludes internal topology and forensic detail; sign it separately with
`mailswiftsync sign` before treating it as an authenticated deliverable.
`supervise` is a foreground, GUI-independent batch controller. It processes
only automation-safe queued/retryable work, waits through GUI lock ownership,
and leaves Attention and verification-difference rows untouched. The optional
`idle-polls` value defaults to one quiet poll; set it to `0` for continuous
watching. An optional fourth argument, `maintenance-window`, confines new
batch passes to a `HH:MM-HH:MM` local time-of-day range (which may wrap past
midnight, for example `22:00-06:00`) and, with an `@Mon,Tue,...` suffix, to
specific days; a batch already admitted before the window closes still runs
to completion. Outside the window `supervise` only waits and re-checks the
clock, so a bounded (`idle-polls` != 0) invocation launched by an external
scheduler at the start of each window still exits at the end of it rather
than running through every subsequent one. It is a supervisor process, not a
remote API or a replacement for an external service manager.
For single-mailbox headless runs, credentials can be supplied through paired
owner-only secret files rather than a profile or command line:

```text
mailswiftsync headless /path/to/state.db preflight \
  --source-secret-file /run/private/source \
  --destination-secret-file /run/private/destination
```

The files must be regular files no larger than 64 KiB. On Unix they must be
owner-only; on Windows they must have the protected Owner Rights/System ACL
that MailSwiftSync validates on the opened file handle. They are read into the
zeroizing execution path and are never stored in the ledger. Batch headless modes intentionally require credentials
to be provided through the already admitted durable queue.

These commands are headless control-plane operations; they do not yet replace
the GUI with a continuously running scheduler. `headless live` is an explicit
one-shot operation for a single-mailbox project: it runs a fresh dry preflight
in the same process, then promotes only that exact plan and credential
fingerprint. It refuses restored batch queues rather than silently selecting
one row. `batch-preflight` and `batch-live` operate only on a complete durable
queue previously imported and validated through the GUI; they refuse an
ambiguous or partially restored queue.

### Real IMAP engine lab

On a Linux host with the packaged `mailswiftsync`, Dovecot, imapsync, and
OpenSSL installed, run the disposable product integration test:

```text
scripts/imap-integration-smoke.sh
```

It starts two local, temporary, self-signed STARTTLS Dovecot servers, writes an
isolated profile and secret files, then drives MailSwiftSync headless preflight,
live migration, incremental live migration, customer-proof export, and proof
verification. It also checks the destination Maildir. Set
`MAILSWIFTSYNC_KEEP_LAB=1` to retain the temporary logs for diagnosis. This is
the product integration gate; it does not replace controller crash/restart
chaos testing.

The container integration workflow also runs
`scripts/controller-recovery-smoke.sh`. That lab uses a deterministic blocking
engine to simulate an ungraceful controller crash and verifies that durable
running state is recovered into operator attention with no active process
ownership left behind.

A third lab, `scripts/controller-chaos-smoke.sh`, covers ledger-level storage
faults without needing root or a real full disk: a file-size limit hit while
the ledger is first written, and a corrupted or truncated ledger/backup file.
It verifies the controller stops instead of completing silently, that
`restore` rejects a bad source without touching the live ledger, that
`status`/`recover` refuse to operate on a corrupted ledger, and that
restoring an earlier verified backup returns the ledger to normal operation.
It does not cover the transfer engine itself running out of destination
storage.

![Mailboxes workspace](docs/wiki/assets/batch-queue.png)

The on-screen execution journal is intentionally capped at 10,000 lines for desktop stability; the structured durable event ledger remains the longer-lived audit record. Raw engine transcripts stay process-local and are not written to SQLite.

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

The **Extra imapsync options** field accepts only a small allowlist of non-connection tuning options (`nofoldersizes`, `skipcrossduplicates`, `maxlinelength`, timeout/retry controls, sleep controls, subscription, and debug flags). Numeric options are parsed and bounded before launch; endpoint, credential, TLS, preflight, destructive deletion, logging, execution, and unknown options are rejected. Test every change using Preflight first. The command preview shows the final arguments with passwords and OAuth tokens redacted. Bulk spreadsheets cannot provide this field.

## Packaging

Run `scripts/package.sh` from anywhere inside the checkout to create a host-native tarball and SHA-256 checksum in the repository's `dist/` directory. Signing and platform-native installers require your own release keys and distribution policy.
