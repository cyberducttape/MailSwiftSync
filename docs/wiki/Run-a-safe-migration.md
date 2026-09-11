# Run a safe migration

## 1. Keep Safe Mode enabled

The **Dry run** checkbox adds `--dry` to imapsync. It validates both logins and displays the proposed folder mapping without copying mail to the destination.

Leave it checked for your first run. The imapsync tutorial recommends testing with a real source account and a test destination before a live run.

## 2. Choose sync rules

Sourcecraft exposes the most common options:

| Setting | What it does |
| --- | --- |
| Map standard folders automatically | Adds `--automap` to map common folders such as Sent and Trash. |
| Folders only | Adds `--justfolders`; useful for checking folder structure without messages. |
| Add Message-ID header when needed | Adds `--addheader`; this can help imapsync identify messages that lack a usable Message-ID. |
| Extra imapsync options | Adds advanced command-line options exactly as typed. Use only options you understand. |

Click **Advanced options** for guided controls for `--syncinternaldates`, `--useuid`, `--usecache`, `--fastio1`, `--fastio2`, and `--allowsizemismatch`. The `--delete2` setting is marked destructive and should only be considered for a deliberately exact backup after a successful dry run.

## 3. Preview the command

Click **Preview redacted command**. Confirm:

- `--host1` and `--user1` describe the source.
- `--host2` and `--user2` describe the destination.
- `--dry` appears for the first run.
- Passwords show as dots, never readable text.

## 4. Run validation

Click **Run dry validation**. The **Execution journal** streams output from imapsync. A successful run should show both logins succeeding and a sensible folder map.

## 5. Run the live migration

Only after validation succeeds:

1. Confirm the destination is the intended mailbox.
2. Uncheck **Dry run**. The header changes from **SAFE MODE** to **LIVE MODE**.
3. Click **Run synchronization**.
4. Keep Sourcecraft open until the journal reports completion.

Normal imapsync behavior is additive: it copies messages from the source and avoids already-synced duplicates. Do not add destructive options such as `--delete2` unless you have an independently verified backup and understand their effect.
