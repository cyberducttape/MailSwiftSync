# Troubleshooting

## “Could not start imapsync”

MailSwiftSync could not find or execute the configured program. Install imapsync, then either add it to PATH or enter its complete file path in **imapsync executable**.

## Login failure

Check the server name, username, and password for the affected side. Many providers require an app password or a separate IMAP enablement setting. Keep Preflight selected while resolving login problems.

## Folder mapping is wrong

Run **Preflight** and examine the journal. Enable **Map standard folders automatically** first. The Extra imapsync options field is intentionally limited to MailSwiftSync’s documented tuning options; unsupported mapping flags must be handled through a typed product control or a reviewed engine-specific workflow.

## The destination has unexpected mail

Stop using live mode and preserve the journal. Do not add deletion options as a quick fix. Check that the right-side account is the intended destination, then test again with a separate mailbox.

## The journal stops changing

Large mailboxes can take time. Check network connectivity and leave the application open. When imapsync exits, MailSwiftSync reports whether the transfer was engine-confirmed, needs a final delta, or requires verification review; a zero exit status alone is not treated as proof of completion.
