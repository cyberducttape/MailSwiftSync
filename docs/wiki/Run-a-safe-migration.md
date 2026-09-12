# Run a safe migration

## 1. Choose the right engine

Choose **Dovecot native** when the destination is managed by Dovecot and administrative access is available. MailSwiftSync then prepares a destination-side `doveadm` command using `imapc` for the remote source. Choose **imapsync fallback** when the destination is another arbitrary IMAP server.

## 2. Keep Safe Mode enabled

The **Dry run** checkbox adds `--dry` to imapsync. It validates both logins and displays the proposed folder mapping without copying mail to the destination.

Leave it checked for your first run. The imapsync tutorial recommends testing with a real source account and a test destination before a live run.

## 3. Choose sync rules

MailSwiftSync exposes the most common options:

| Setting | What it does |
| --- | --- |
| Map standard folders automatically | Adds `--automap` to map common folders such as Sent and Trash. |
| Folders only | Adds `--justfolders`; useful for checking folder structure without messages. |
| Add Message-ID header when needed | Adds `--addheader`; this can help imapsync identify messages that lack a usable Message-ID. |
| Extra imapsync options | Adds non-connection advanced command-line options exactly as typed. Connection, credential, TLS, dry-run, and destructive deletion flags are controlled by the migration plan and rejected here. |
| Performance throttles | In Advanced options, optional message/byte-per-second limits are passed as explicit imapsync settings; `0` means unlimited. These controls do not affect Dovecot-native runs. |

Click **Advanced options** for guided controls for `--syncinternaldates`, `--useuid`, `--usecache`, `--fastio1`, `--fastio2`, and `--allowsizemismatch`. The `--delete2` setting is marked destructive and should only be considered for a deliberately exact backup after a successful dry run.

## 4. Preview the command

Click **Preview redacted command**. Confirm:

- In Dovecot mode, `imapc_host`/`imapc_user` describe the source and `-Ru` describes the destination user.
- In imapsync mode, `--host1`/`--user1` and `--host2`/`--user2` describe the endpoints.
- Dovecot dry mode uses a non-mutating `imapc` mailbox listing against the source; imapsync dry mode adds `--dry`.
- Passwords show as dots, never readable text. imapsync receives them through short-lived owner-only passfiles, not command-line values or environment variables.
- For imapsync, the generated plan explicitly forces `--ssl1`/`--ssl2` for IMAPS and `--tls1` for STARTTLS; it does not permit automatic cleartext fallback. A deliberately configured plain source is shown as a warning.

## 5. Run validation

Click **Run dry validation**. The **Execution journal** streams output from the selected engine. A successful Dovecot check should authenticate to the remote source and list its mailboxes; a successful imapsync check should show both logins succeeding and a sensible folder map.

## 6. Run the live migration

Only after validation succeeds:

1. Confirm the destination is the intended mailbox.
2. Uncheck **Dry run**. The header changes from **SAFE MODE** to **LIVE MODE**.
3. Click **Run synchronization**.
4. Keep MailSwiftSync open until the journal reports completion. You can request cancellation from the activity controls; a one-day execution limit also prevents an abandoned child process from running forever.

Dovecot mode uses `sync -1` by default so destination-side changes are preserved during the migration. After a live run, MailSwiftSync queries both source and destination mailbox status and stores aggregate folder/message/virtual-size evidence. Enabling destination deletion switches to `backup`; this can remove destination-only messages. In imapsync mode, the final summary is parsed when complete. Do not treat an incomplete summary or failed verification as success.
