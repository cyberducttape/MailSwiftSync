# Bulk migrations from CSV or Excel

Use the **Transient retries** control for short-lived transport failures. MailSwiftSync labels failures as `authentication`, `quota`, `capacity`, `transport`, `configuration`, `message`, or `unknown`; transport and provider-capacity signals such as rate limits, HTTP 429 responses, server-busy responses, and connection ceilings are eligible for retry. Capacity retries use a longer bounded backoff than ordinary transport retries, while exhausted quota, authentication, and configuration errors stop clearly. Backoff is cancellation-aware. For imapsync, message/byte throttle targets are divided across concurrent workers and process starts are globally paced; a finite target must be at least the worker count. These are application-level safeguards, not a substitute for provider-specific tenant limits.

MailSwiftSync can import a migration list from CSV or XLSX and process rows with a bounded worker pool. Legacy XLS is currently rejected: its parser materializes ranges before resource limits can be checked. Convert legacy workbooks to XLSX or CSV before importing. Choose 1–16 concurrent workers under **Queue settings** to balance migration-window speed against provider throttling. Every mailbox must first complete a matching preflight; after that, the same durable queue can be explicitly promoted to a live batch run. Batch actions apply only to explicitly selected rows, never to an empty selection. **Select unresolved** selects rows that need follow-up after a run (failed, cancelled, attention, delta required, or verification difference); **Select attention** selects only the rows that need operator review; the state filter plus **Select visible** builds any other focused set. Selecting a verified row re-runs it deliberately, so inspect the durable reason first and re-run an evidence-backed migration only on purpose.

XLSX files are limited to 100,000 data rows, 64 columns, and 1,000,000 occupied or expanded cells per selected worksheet. Sheets whose declared or actual cell range exceeds those limits are rejected before Calamine creates its dense worksheet matrix.

![Mailboxes page with a loaded batch queue](assets/batch-queue.png)

*Mailboxes page of the current release with placeholder `.example` data: queue health, collapsed queue settings, selection controls, and the review drawer for two selected rows. Passwords are never shown in the queue.*

## Create the file

Use the header row below. Column names are case-insensitive. `project_name` is
optional and should normally be repeated on each row when the file contains a
single customer migration; MailSwiftSync uses it for the durable project name.
The per-row `name` remains the mailbox label shown in the queue.

```csv
project_name,name,source_host,source_user,source_credential_id,destination_host,destination_user,destination_credential_id
Finance archive,finance mailbox,imap.old.example,finance@example.com,finance-source,imap.new.example,finance@example.com,finance-destination
```

Required columns are:

- `source_host`
- `source_user`
- `destination_host`
- `destination_user`

Optional columns are `project_name`, `source_credential_id`, `destination_credential_id`, and `name`. Password columns are intentionally rejected by default, even when blank; use keyring IDs instead, either as columns or with **Queue settings → Passwordless queue credentials** after import. The normal protected workflow starts from the [CSV template](../bulk-migrations-template.csv). Plaintext password imports are a separate, explicitly opt-in administrative exception and require `MAILSWIFTSYNC_ALLOW_PLAINTEXT_SECRETS=1`; never commit a populated password spreadsheet. Engine options are trusted application settings and cannot be imported from a spreadsheet.

## Import and review

1. Open **Mailboxes** and click **Import CSV / XLSX…**, then select the file. Review the import confirmation.
2. If the file is XLSX, choose the worksheet containing the migration headers; MailSwiftSync does not assume the first worksheet.
3. Review each source and destination in the queue table. Hover a truncated host or user to see the full value.
4. Expand **Queue settings**. Choose a conservative **Concurrent workers** value (normally 1–2 until the provider pair is proven) and the number of **Transient retries**. For rows without a credential reference, enter an existing OS-keyring ID and click **Apply to empty source rows** or **Apply to empty destination rows**; rows that already have a password or credential ID are left unchanged.
5. Correct the spreadsheet and import it again if any account is wrong. **Clear queue** discards the current queue after confirmation.
6. With the state filter on **All states**, click **Select visible** (or narrow the list first with search or the state filter), check the **Review selected** drawer, and click **Run preflight (N)**. On later passes, **Select unresolved** picks the rows that need follow-up.
7. Review the resulting `Ready` states and exact plan fingerprints. Click **Run live migration (N)** and confirm the destructive-action dialog. After cutover, use **Run final delta (N)** for rows marked `Delta required`.

To hand a selection to another reviewer, click **Export selected set…**. It writes the selected rows as JSON without credentials or engine options. Messages about imports, blocked starts, and queue tools appear in a banner at the top of the page until you dismiss them.

## Run safely

Preflight and live execution are separate deliberate phases. The batch project and all imported mailbox jobs are committed atomically, with a durable parent wave run and one mailbox-specific child run per row. Child runs remain queued until their worker claims the mailbox, then transition to running with the mailbox in one durable operation. A live batch is allowed only when every selected mailbox has a matching successful preflight fingerprint and an operator confirms the queue. Secret-free row configuration is retained so a queue can be restored for review after restart; credentials are intentionally not persisted for replay and must be entered again. Batch startup validates only the selected rows before launching the first process, so credentialless rows that are intentionally excluded do not block a retry. The queue streams indexed output from concurrent workers, supports cancellation and bounded transient retries, and leaves completed/failed/attention child states visible for another delta or retry pass. Each child run retains its engine, launch snapshot, process identity, attempt and terminal result. Live batch children that use encrypted imapsync run the same independent message-level verification as single-mailbox runs: metadata reconciliation by default, or bounded body-hash comparison when the forensic profile enables it. Plans whose semantics the verifier cannot independently reproduce (automap, justfolders, addheader, disabled internal-date sync, allowed size mismatches) and native Dovecot children are limited to aggregate or engine evidence and are marked for review. Review the exported evidence before declaring the project complete.

Do not commit a spreadsheet containing real passwords or OAuth tokens to Git, and treat the file as sensitive after import. Imported data exists only in MailSwiftSync memory for the current queue and is not written to the saved profile. For operator-managed work, use keyring IDs in the base profile where practical. imapsync OAuth 2.0/XOAUTH2 tokens are supported when configured on the base profile; an operator-supplied refresh configuration (for example from `mailswiftsync oauth-authorize`) can renew access tokens before each live launch, but unattended credential brokering beyond the OS keyring remains outside MailSwiftSync.
