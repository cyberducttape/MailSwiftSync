# Troubleshooting

## “Could not start imapsync”

MailSwiftSync could not find or execute the configured program. Install imapsync, then either add it to PATH or enter its complete file path in **imapsync executable**.

## Login failure

Check the server name, username, and password for the affected side. Many providers require an app password or a separate IMAP enablement setting. Keep Dry run enabled while resolving login problems.

## Folder mapping is wrong

Run Dry validation and examine the journal. Enable **Map standard folders automatically** first. For unusual folder names, add an imapsync `--f1f2` mapping in **Extra imapsync options** after verifying its syntax in the imapsync documentation.

## The destination has unexpected mail

Stop using live mode and preserve the journal. Do not add deletion options as a quick fix. Check that the right-side account is the intended destination, then test again with a separate mailbox.

## The journal stops changing

Large mailboxes can take time. Check network connectivity and leave the application open. If imapsync exits, MailSwiftSync reports either **Completed successfully** or a failure status with the exit condition.
