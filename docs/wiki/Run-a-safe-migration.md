# Run a safe migration

## 1. Choose the right engine

Choose **Dovecot native** when the destination is managed by Dovecot and administrative access is available. MailSwiftSync then prepares a destination-side `doveadm` command using `imapc` for the remote source. Choose **imapsync fallback** when the destination is another arbitrary IMAP server.

## 2. Keep Preflight selected

**Preflight** validates both logins and displays the proposed folder mapping without intentionally changing the destination.

Leave it checked for your first run. The imapsync tutorial recommends testing with a real source account and a test destination before a live run.

## 3. Choose sync rules

MailSwiftSync exposes the most common options:

| Setting | What it does |
| --- | --- |
| Map standard folders automatically | Adds `--automap` to map common folders such as Sent and Trash. |
| Folders only | Adds `--justfolders`; useful for checking folder structure without messages. |
| Add Message-ID header when needed | Adds `--addheader`; this can help imapsync identify messages that lack a usable Message-ID. |
| Extra imapsync options | Accepts only the application’s safe allowlist of non-connection tuning options and the general debug flag. Protocol-level `debugimap1`/`debugimap2` output is rejected because transformed authentication data cannot be covered by literal secret redaction. Connection, credential, TLS, dry-run, destructive deletion, logging, and unknown flags are rejected; bulk spreadsheets cannot provide this field. |
| Performance throttles | In Advanced options, optional message/byte-per-second targets are passed to imapsync; for batches MailSwiftSync divides them across workers and globally paces process starts. A finite batch target must be at least the worker count; `0` means unlimited. These controls do not affect Dovecot-native runs. |
| Process timeout | Bounds one migration process from 1 to 720 hours. Increase it for very large or slow mailboxes; cancellation remains available at any time. |

Click **Advanced options** for guided controls for `--syncinternaldates`, `--useuid`, `--usecache`, `--fastio1`, `--fastio2`, and `--allowsizemismatch`. The `--delete2` setting is marked destructive and should only be considered for a deliberately exact backup after a successful preflight.

## 4. Preview the command

Click **Preview redacted command**. Confirm:

- In Dovecot mode, `imapc_host`/`imapc_user` describe the source and `-Ru` describes the destination user.
- In imapsync mode, `--host1`/`--user1` and `--host2`/`--user2` describe the endpoints.
- Dovecot dry mode uses a non-mutating `imapc` mailbox listing against the source; imapsync dry mode adds `--dry`.
- Passwords show as dots, never readable text. imapsync receives them through short-lived owner-only passfiles, not command-line values or environment variables.
- For imapsync, the generated plan explicitly forces `--ssl1`/`--ssl2` for IMAPS and `--tls1` for STARTTLS; it does not permit automatic cleartext fallback. A deliberately configured plain source is shown as an insecure-transport warning, defaults to port 143 when no port is supplied, and requires an explicit acknowledgement before any authenticated operation, including dry preflight. MailSwiftSync also passes `--nolog` so imapsync does not create an unmanaged persistent log outside the application journal.

For private enterprise PKI, expand **Enterprise certificate trust** and provide a
PEM CA bundle for either endpoint. In imapsync mode, the bundle is added to the TLS readiness
probe and to imapsync's `SSL_ca_file` setting; public roots remain enabled. An
optional 64-character SHA-256 leaf-certificate pin is checked after the TLS
handshake and blocks both readiness and live re-authentication on mismatch.
Trust settings are part of the plan fingerprint, so changing them requires a
new preflight. Never disable certificate verification to work around an
untrusted private CA. Dovecot-native mode accepts a source CA bundle for its
`imapc` source, but certificate pins are not enforced by the native engine and
are rejected during validation.

## 5. Run validation

Click **Run preflight**. The **Execution journal** streams output from the selected engine. A successful Dovecot check should authenticate to the remote source and list its mailboxes; a successful imapsync check should show both logins succeeding and a sensible folder map.

## 6. Run the live migration

Only after validation succeeds and the durable project still matches the displayed source/destination identities:

1. Confirm the destination is the intended mailbox.
2. Select **Live migration**. The header changes to **LIVE MIGRATION**.
3. Click **Run synchronization**.
4. Keep MailSwiftSync open until the journal reports completion. You can request cancellation from the activity controls; the configured 1–720 hour execution limit also bounds an abandoned child process.

For dual-IMAPS imapsync plans, live admission performs a fresh certificate-validated authentication probe against both endpoints immediately before mutation. If the plan is edited while that probe is running, the probe and live confirmation no longer apply to the edited plan.

Native Dovecot mode exposes four migration strategies: **Initial mirror** uses
`doveadm backup`; **Incremental mirror** repeats `backup` with the durable
checkpoint; **Final preservation pass** uses `doveadm sync -1` for the cutover;
and **Destination already active** is an advanced preservation mode using
`sync -1`. These are migration strategies, not a simple delete-extras switch:
review how each treats destination-side changes. Native Dovecot has no
MailSwiftSync throttle and may place high load on the source. Dovecot exit code
2 means synchronization completed but was not perfect; MailSwiftSync marks the
run as delta-required and the final pass should be repeated until exit code 0.
After a live run, MailSwiftSync queries both source and destination mailbox
status and stores aggregate folder/message/virtual-size evidence. In imapsync
mode, the final summary is parsed against the supported output contract for
the packaged imapsync `2.314` profile: all six aggregate records, the exact
completion marker, and an explicit error-count record are required. The
executable version is resolved before launch and selects the streaming parser
profile. If the version is unavailable or not explicitly supported, the
execution journal says: **Engine [version] is not qualified for MailSwiftSync
verification. Transfer may work, but MailSwiftSync cannot provide trusted
migration evidence from this version. Qualified version: 2.314.** The transfer
may run, but it is transfer-only and must not be represented to a customer as
verified. Unknown wording, extra prose, or missing fields likewise produces
incomplete evidence rather than success. Do not treat an incomplete summary or
failed verification as success.

For a production window, follow the [production migration runbook](Production-runbook.md), including the pilot sequence, crash/restart recovery, Attention review, and report export checklist.
