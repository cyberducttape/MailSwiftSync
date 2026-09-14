# Install and first launch

## What you need

- A computer running a released MailSwiftSync archive or a development
  checkout.
- A working `imapsync` installation for arbitrary IMAP migrations, or local
  `doveadm` access for a Dovecot destination.
- Login details for both IMAP accounts.
- A test mailbox at the destination, strongly recommended for the first run.

## Start MailSwiftSync

For a release, verify the adjacent checksum, extract the archive, and run the
platform binary. The exact verification commands are in the
[distribution installation guide](../distribution/INSTALL.md).

For a source checkout only, build and start with:

```bash
cargo run --release
```

MailSwiftSync looks for `imapsync` on your PATH. If it is installed elsewhere, enter the full path in **imapsync executable** near the bottom of the window.

## Follow the safe first migration

1. Configure source and destination and save the non-secret profile.
2. Leave **Preflight** selected and run the readiness checks.
3. Review the plan, capability results, and any Attention items.
4. Use one representative mailbox as a pilot before selecting a larger batch.
5. Approve live execution only after the pilot is understood, then verify and
   export the customer-safe proof.

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
