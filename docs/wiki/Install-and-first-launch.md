# Install and first launch

## What you need

- A computer running MailSwiftSync.
- A working `imapsync` installation.
- Login details for both IMAP accounts.
- A test mailbox at the destination, strongly recommended for the first run.

## Start MailSwiftSync

Build and start from the project folder:

```bash
cargo run --release
```

MailSwiftSync looks for `imapsync` on your PATH. If it is installed elsewhere, enter the full path in **imapsync executable** near the bottom of the window.

## Fill in the two account panels

The left panel is **01 SOURCE**. This is the mailbox you are copying from.

The right panel is **02 DESTINATION**. This is the mailbox that receives copied messages.

For each panel enter:

- **Server** — the IMAP server host name, such as `imap.example.com`.
- **User** — usually the complete email address.
- **Password** — the account password or an app password, if your mail provider requires one.

Do not reverse the panels. MailSwiftSync never treats the destination as a source unless you put it in the left panel.

## Save a profile

Click **Save non-secret profile** to remember the profile name, server names, usernames, selected rules, and executable path. Passwords are deliberately excluded. You must re-enter them after restarting MailSwiftSync.
