# Production migration runbook

This runbook is for attended migrations on a dedicated Unix admin workstation or jump host. MailSwiftSync is restart-aware, not unattended automation: keep an operator present for live work and review every `Attention` or `Failed` mailbox.

For supervised automation, the binary also exposes a headless one-shot path.
See the [service-manager deployment guide](../distribution/SERVICE.md) for
systemd and Windows wrapper examples:

```text
mailswiftsync headless /path/to/state.db preflight
mailswiftsync headless /path/to/state.db live
mailswiftsync headless /path/to/state.db batch-preflight
mailswiftsync headless /path/to/state.db batch-live
mailswiftsync customer-proof /path/to/state.db /path/to/customer-proof.json
```

The live form always runs a fresh preflight in the same process before
promotion and retains the normal durable plan/credential gates. It is suitable
for a service wrapper or scheduled job with an external timeout and log
capture. Batch commands require a complete queue already imported and
validated in the GUI. These commands are not yet a persistent scheduler or
independent supervisor.

## Before the window

- Use one operator session on a locked-down host. Do not run a second MailSwiftSync instance against the same workspace; if the instance-lock message appears, close the existing owner and do not delete the lock file.
- Confirm `imapsync` and local `doveadm` are installed at the expected versions and paths. Remote Dovecot execution is unavailable until a secret-safe broker exists.
- Confirm outbound access only to the source, destination, and any approved SSH target. Check NTP, disk space, and the configured process timeout for the actual migration window.
- Keep plain source transport disabled. Remote Dovecot execution is unavailable; use local doveadm or imapsync.

Before a high-value wave, create a verified ledger backup while no other
controller is running:

```text
mailswiftsync backup /path/to/state.db /path/to/state-20260913.db
```

The command takes the instance lock, refuses to overwrite an existing file,
and runs SQLite integrity verification. Preserve the backup with the matching
exported report. To recover, stop the controller, preserve the current
database, and run `mailswiftsync restore /path/to/backup.db /path/to/state.db`.
The restore validates the copy before installation and preserves an existing
destination under a rollback filename; never edit a live SQLite file or delete
the lock to bypass ownership. Opening an older
schema also creates a unique `state.db.pre-migrate-vN.<id>.db` backup before
the transactional migration; preserve that artifact until the upgrade is
validated.

Read-only inspection is deliberately different: `status`, report verification,
and support-bundle reads do not migrate the source ledger. When they encounter
an older supported schema, MailSwiftSync copies it into a private in-memory
database and migrates only that copy. A future schema is rejected until the
binary is upgraded, rather than being interpreted with missing columns.

## Safe execution sequence

1. Create or select the project and verify source/destination hosts, ports, users, TLS modes, engine, and destination policy.
2. Leave **Preflight** selected. If the source uses plain IMAP, acknowledge the cleartext warning before any authenticated preflight; preflight still transmits credentials and protocol traffic.
3. Preview the redacted command and confirm the endpoints and mailbox identities. For imapsync, MailSwiftSync owns the journal and passes `--nolog`; do not look for an unmanaged engine log as the audit record.
4. Run one representative **dry pilot** against a test destination. Resolve authentication, TLS, quota, folder, and configuration failures before increasing scope.
5. Run the **live pilot** only after the matching preflight succeeds. Confirm the destination again, select Live migration, and accept the live confirmation. If a static session password, manually supplied OAuth access token, or ordinary password keyring value changes, MailSwiftSync requires a new preflight before live promotion. Automatic OAuth refresh is different: its stable account/auth/refresh-reference binding must match, while the short-lived access token may rotate; live admission still performs fresh authenticated readiness checks after refresh.
6. For a batch, run **bulk preflight checks** first, review every row, then promote only the unchanged queue to live. Keep concurrency conservative (normally 1–2 until the provider pair is proven) and never ignore duplicate destinations or Attention items. On a retry, Verified rows are excluded by default; select **Include already verified mailboxes (explicit re-run)** only when you intentionally want to repeat them.
7. After each live phase, open Verification, review the evidence level and run identity, and export the Markdown/JSON verification report. Export the project-health JSON for the change ticket as well.
8. Treat `Verified` as evidence-backed completion. Aggregate evidence is not message-level proof. Use **Accept residual difference** in Verification only when the exception is approved; this creates `Verified with exceptions` with the operator, timestamp, related evidence run, and acceptance reason in the ledger.
9. Export **Customer proof JSON** for the change record and retain the separate project JSON/Markdown report for operator forensics. Customer proof omits internal endpoints, credential references, plan snapshots, executable paths, and diagnostic detail. Sign the customer proof with the approved Ed25519 key before distributing it.
10. For a GUI-independent maintenance window, run `mailswiftsync supervise <state.db> 30 0` under the host service manager. It retries only automation-safe work and leaves Attention/verification-difference rows for review; configure restart limits and logs in the service manager.

Once every mailbox is evidence-backed, MailSwiftSync may mark the project
**Complete**. Complete projects are intentionally read-only: adding a mailbox,
starting a run, or changing mailbox state is rejected by the durable core. If a
post-cutover correction is required, open the readiness/project action, enter the
change reason, and use **Reopen project**. This records a `project_reopened`
audit event and returns the project to Attention before further work is
allowed.

The Dovecot path uses the durable `checkpoint` field as a conservative
stateful-resume token. Live `sync`/`backup` passes provide the last committed
value to `doveadm -s`; the initial pass uses an empty state, and a newly
emitted state is committed atomically with the child result. Dry preflight and
failed or cancelled runs do not advance it. This is an engine resume
optimization, not UIDVALIDITY-aware message evidence: after recovery, review
the resulting evidence and run another delta or verification pass when the
destination is not yet reconciled.

## During execution

- Watch the Activity journal, but remember that the durable SQLite ledger and exported reports are the source of truth; the visible journal is bounded and redacted.
- A transient batch retry keeps the same mailbox child run and durable claim
  across attempts. The worker may back off without making the mailbox appear
  unowned; a restart during that interval is therefore recovered as active
  interrupted work rather than as a new, unrelated run.
- Keep concurrency and provider throttling below the tenant’s tested limits. Per-process throttles are not a tenant-wide rate limit.
- Use Cancel when the migration window must stop. Do not kill the application as a normal cancellation method.
- Do not edit plan or batch identity fields, import a new queue, or clear the queue while execution is active.

## Crash or restart

1. Restart MailSwiftSync and wait for startup recovery to finish. A matching recorded Unix process group is reaped before the interrupted run is made retryable.
2. Expect interrupted jobs to move to **Attention** and runs to become abandoned. This is conservative recovery, not proof that a transfer failed or succeeded.
3. Review each Attention item, its classified failure/output, and the destination before retrying. Use the batch queue’s default unresolved-only retry behavior; never enable the explicit Verified re-run option without documenting why.
4. Confirm no migration engine remains active outside the application. Linux uses process-group identity checks and Windows uses Job Object ownership with kill-on-close. macOS remains a weaker platform path; prefer a Unix admin host for production windows there.
5. Re-enter credentials as required, rerun preflight when the plan or credentials changed, and export the resulting evidence after the retry.

If the application reports **Migration result requires durability review** or a
`[durability]` error, do not retry the mailbox and do not advance the project
manually. The external process has ended, but the terminal run commit was not
confirmed. Preserve the workspace, restore disk space or SQLite availability,
then restart MailSwiftSync and let startup recovery reconcile the still-active
durable run. If the database remains unavailable, stop and copy the diagnostic
information through the normal incident process; never delete the database or
lock file to make a retry possible.

If startup cannot verify a recorded process identity, MailSwiftSync fails closed and keeps that identity in the ledger across restarts. Check the host process list and use **I confirmed no unverified migration process remains** only after confirming that no MailSwiftSync engine is still active; do not delete the state or lock files to bypass this review.

## Closeout

- Export the project verification report and project-health summary and attach both to the change record.
- Confirm every intended mailbox has an appropriate terminal state; do not call a batch complete merely because transfer processes exited successfully.
- Confirm no child engine processes remain, then rotate or remove temporary/keyring credentials according to local policy.
- Preserve the SQLite workspace and exported artifacts together if the migration may need an audit or post-incident review.

The durable ledger retains lifecycle and evidence history. Verbose subprocess output is diagnostic context held only in bounded process-local memory; raw engine transcripts are not written to the ledger. Export the relevant report and health summary during the migration window if detailed engine output may be needed later.

## Policy exceptions

Plain IMAP requires explicit approval because credentials and mailbox traffic cross the network without transport encryption. Remote Dovecot is not an available policy exception until secret-broker delivery is implemented.
