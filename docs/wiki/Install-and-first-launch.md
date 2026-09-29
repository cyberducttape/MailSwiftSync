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

MailSwiftSync looks for `imapsync` on your PATH. If it is installed elsewhere, enter the full path in **imapsync executable** in the **imapsync options** card on the **Plan** page.

## Follow the safe first migration

1. Configure source and destination on **Plan** and click **Save profile**.
2. Leave **Dry run / preflight** checked, click **Run authenticated readiness probe**, then **Run preflight**.
3. Review the plan, capability results, and any Attention items.
4. Use one representative mailbox as a pilot before selecting a larger batch.
5. Approve live execution only after the pilot is understood, then verify and
   export the customer-safe proof.

## Fill in the two account cards

![Migration plan page](assets/migration-plan.png)

The left card is **Source account**. This is the mailbox you are copying from.

The right card is **Destination account**. This is the mailbox that receives copied messages. With the Dovecot engine it becomes **Local Dovecot destination**: no destination password, port, or TLS setting applies, because Dovecot writes to local storage.

For each card choose a provider preset if one fits, then enter:

- **Server** — the IMAP server host name, such as `imap.example.com`.
- **User** — usually the complete email address.
- **Authentication** — **Password**, or **OAuth 2.0 / XOAUTH2** for imapsync.
- **Password** or **Access token** — the account password, an app password where the provider requires one, or a current OAuth access token.

**Connection details** below each card hold the port, an optional OS-keyring **Credential ID**, an enterprise **CA bundle**, an optional **Certificate pin (SHA-256)** (imapsync only), and **TLS**.

Do not reverse the cards. MailSwiftSync never treats the destination as a source unless you put it in the left card.

## Save a profile

Click **Save profile** to remember the plan name, server names, usernames, selected options, and executable path. Passwords are deliberately excluded. You must re-enter them after restarting MailSwiftSync, or store them in the OS keyring with **OS keyring credentials…** in the **Tools** row.
