# Bulk migrations from CSV or Excel

Use the **Transient retries** control for short-lived transport failures. MailSwiftSync labels failures as `authentication`, `quota`, `transport`, `configuration`, `message`, or `unknown`; only transport/throttling failures are eligible for retry. Authentication and configuration errors stop clearly, and backoff is cancellation-aware.

MailSwiftSync can import a migration list from CSV, XLS, or XLSX and process rows with a bounded worker pool. Choose 1–16 concurrent workers to balance migration-window speed against provider throttling. Every mailbox must first complete a matching dry validation; after that, the same durable queue can be explicitly promoted to a live batch run.

![Batch migration queue interface](assets/batch-queue.png)

> **Documentation illustration:** this image shows the intended batch-review workflow and is not a pixel-accurate capture of the current egui application. Passwords are never shown in the queue.

## Create the file

Use the header row below. Column names are case-insensitive.

```csv
name,source_host,source_user,source_password,destination_host,destination_user,destination_password,extra_options
Finance archive,imap.old.example,finance@example.com,APP_PASSWORD,imap.new.example,finance@example.com,APP_PASSWORD,--automap
```

Required columns are:

- `source_host`
- `source_user`
- `destination_host`
- `destination_user`

Optional columns are `source_password`, `destination_password`, `name`, and `extra_options`. If password columns are omitted, enter credentials in the masked per-row fields after import. This avoids putting passwords in the spreadsheet. Start from the [CSV template](../bulk-migrations-template.csv) when a protected credential-bearing import is genuinely required.

## Import and review

1. Click **Batch queue** in the MailSwiftSync header.
2. Click **Import CSV / XLSX…** and select the file.
3. Review each source and destination in the queue table and enter any missing credentials in the masked fields.
4. Correct the spreadsheet and import it again if any account is wrong.
5. Choose a conservative concurrency value and click **Run N dry validations**.
6. Review the resulting `Ready` states and exact plan fingerprints. Switch off **Simulation mode**, then click **Run N live migrations** and confirm the destructive-action dialog.

## Run safely

Dry validation and live execution are separate deliberate phases. The batch project and all imported mailbox jobs are committed atomically, and each batch has a durable run record. A live batch is allowed only when every mailbox has a matching successful dry-validation fingerprint and an operator confirms the queue. Secret-free row configuration is retained so a queue can be restored for review after restart; credentials are intentionally not persisted for replay and must be entered again. Batch startup validates every row before launching the first process, so a restored queue cannot accidentally run with blank credentials. The queue streams indexed output from concurrent workers, supports cancellation and bounded transient retries, and leaves completed/failed/attention child states visible for another delta or retry pass. Live batch verification remains aggregate/engine-dependent; review the exported evidence before declaring the project complete.

Do not commit a spreadsheet containing real passwords to Git, and treat the file as sensitive after import. Imported data exists only in MailSwiftSync memory for the current queue and is not written to the saved profile. For operator-managed work, use keyring IDs in the base profile where practical; provider OAuth and unattended batch secret brokering are not yet implemented.
