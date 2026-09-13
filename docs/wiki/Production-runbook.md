# Production migration runbook

This runbook is for attended migrations on a dedicated Unix admin workstation or jump host. MailSwiftSync is restart-aware, not unattended automation: keep an operator present for live work and review every `Attention` or `Failed` mailbox.

## Before the window

- Use one operator session on a locked-down host. Do not run a second MailSwiftSync instance against the same workspace; if the instance-lock message appears, close the existing owner and do not delete the lock file.
- Confirm `imapsync`, `doveadm`, and `ssh` are installed at the expected versions and paths. Prefer local Dovecot execution or imapsync over remote Dovecot when destination administration is unavailable.
- Confirm outbound access only to the source, destination, and any approved SSH target. Check NTP, disk space, and the configured process timeout for the actual migration window.
- Keep plain source transport and remote-Dovecot password-in-argv disabled unless the change ticket explicitly approves them. Never treat either option as a routine convenience.

## Safe execution sequence

1. Create or select the project and verify source/destination hosts, ports, users, TLS modes, engine, and destination policy.
2. Leave **Dry run** enabled. If the source uses plain IMAP, acknowledge the cleartext warning before any authenticated preflight; a dry run still transmits credentials and protocol traffic.
3. Preview the redacted command and confirm the endpoints and mailbox identities. For imapsync, MailSwiftSync owns the journal and passes `--nolog`; do not look for an unmanaged engine log as the audit record.
4. Run one representative **dry pilot** against a test destination. Resolve authentication, TLS, quota, folder, and configuration failures before increasing scope.
5. Run the **live pilot** only after the matching dry validation succeeds. Confirm the destination again, disable Dry run, and accept the live confirmation. If a session password changes or a referenced keyring credential is reloaded, MailSwiftSync requires a new dry preflight before live promotion.
6. For a batch, run **bulk dry validation** first, review every row, then promote only the unchanged queue to live. Keep concurrency conservative (normally 1–2 until the provider pair is proven) and never ignore duplicate destinations or Attention items. On a retry, Verified rows are excluded by default; select **Include already verified mailboxes (explicit re-run)** only when you intentionally want to repeat them.
7. After each live phase, open Verification, review the evidence level and run identity, and export the Markdown/JSON verification report. Export the project-health JSON for the change ticket as well.
8. Treat `Verified` as evidence-backed completion. Aggregate evidence is not message-level proof; if residual differences are accepted, record the decision and risk in the change ticket until the application provides a durable acceptance action.

## During execution

- Watch the Activity journal, but remember that the durable SQLite ledger and exported reports are the source of truth; the visible journal is bounded and redacted.
- Keep concurrency and provider throttling below the tenant’s tested limits. Per-process throttles are not a tenant-wide rate limit.
- Use Cancel when the migration window must stop. Do not kill the application as a normal cancellation method.
- Do not edit plan or batch identity fields, import a new queue, or clear the queue while execution is active.

## Crash or restart

1. Restart MailSwiftSync and wait for startup recovery to finish. A matching recorded Unix process group is reaped before the interrupted run is made retryable.
2. Expect interrupted jobs to move to **Attention** and runs to become abandoned. This is conservative recovery, not proof that a transfer failed or succeeded.
3. Review each Attention item, its classified failure/output, and the destination before retrying. Use the batch queue’s default unresolved-only retry behavior; never enable the explicit Verified re-run option without documenting why.
4. Confirm no migration engine remains active outside the application. On Windows and macOS, process-recovery guarantees are weaker than the Linux path; prefer a Unix admin host for production windows.
5. Re-enter credentials as required, rerun dry validation when the plan or credentials changed, and export the resulting evidence after the retry.

If startup cannot verify a recorded process identity, MailSwiftSync fails closed and keeps that identity in the ledger across restarts. Check the host process list and use **I confirmed no unverified migration process remains** only after confirming that no MailSwiftSync engine is still active; do not delete the state or lock files to bypass this review.

## Closeout

- Export the project verification report and project-health summary and attach both to the change record.
- Confirm every intended mailbox has an appropriate terminal state; do not call a batch complete merely because transfer processes exited successfully.
- Confirm no child engine processes remain, then rotate or remove temporary/keyring credentials according to local policy.
- Preserve the SQLite workspace and exported artifacts together if the migration may need an audit or post-incident review.

The durable ledger retains lifecycle and evidence history. Verbose subprocess output is diagnostic context and is automatically retained as a bounded per-project tail, so export the relevant report and health summary during the migration window if detailed engine output may be needed later.

## Policy exceptions

Remote Dovecot password-in-argv exposes the credential to process inspection on the destination. If it is approved, use a trusted destination, a dedicated migration principal, a time-boxed change, and no untrusted users/processes on that host. Plain IMAP similarly requires explicit approval because credentials and mailbox traffic cross the network without transport encryption.
