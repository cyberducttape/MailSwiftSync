# Bulk migrations from CSV or Excel

MailSwiftSync can import a migration list from CSV, XLS, or XLSX and validate each row sequentially. The queue is deliberately sequential so one migration's output stays readable and one problematic mailbox does not overload a server. Bulk validation uses the currently selected engine; live migrations remain individually confirmed until scheduler and concurrency work is complete.

![Batch migration queue interface](assets/batch-queue.png)

> This is an interface illustration showing what the batch-review workflow looks like; passwords are never shown in the queue.

## Create the file

Use the header row below. Column names are case-insensitive.

```csv
name,source_host,source_user,source_password,destination_host,destination_user,destination_password,extra_options
Finance archive,imap.old.example,finance@example.com,APP_PASSWORD,imap.new.example,finance@example.com,APP_PASSWORD,--automap
```

Required columns are:

- `source_host`
- `source_user`
- `source_password`
- `destination_host`
- `destination_user`
- `destination_password`

Optional columns are `name` and `extra_options`. Start from the [CSV template](../bulk-migrations-template.csv), replace the example values, and keep the file in a protected location.

## Import and review

1. Click **Batch queue** in the MailSwiftSync header.
2. Click **Import CSV / XLSX…** and select the file.
3. Review each source and destination in the queue table. The passwords are never shown in the table.
4. Correct the spreadsheet and import it again if any account is wrong.
5. Click **Run N dry validations**.

## Run safely

Batch runs are deliberately validation-only and require Dry run to be enabled. The batch project and all imported mailbox jobs are committed atomically, and the batch has a durable run record. Secret-free row configuration is retained so a queue can be restored for review after restart; credentials are intentionally not persisted for replay and must be entered again. The queue streams output sequentially so individual failures remain visible. Review the execution journal for every job before considering a live migration. Use the main single-mailbox workflow for a confirmed live migration.

Do not commit a spreadsheet containing real passwords to Git, and treat the file as sensitive after import. Imported data exists only in MailSwiftSync memory for the current queue and is not written to the saved profile. A future keyring-backed credential source should be preferred for production batch work.
