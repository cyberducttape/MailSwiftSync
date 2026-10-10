# Provider-Specific Setup Guide

MailSwiftSync uses IMAP for all migrations. This guide walks through the setup required for each major provider.

Provider authentication and capacity facts are maintained in the [canonical
provider facts](docs/provider-facts.md); update that source and the validation
test when provider policy changes.

## Folder-mapping boundary

For imapsync migrations, express provider folder differences as **Typed folder
mappings** (exact source-to-destination rules and source-folder exclusions)
under **Advanced migration settings** on the **Plan** page. Those rules are
immutable plan state, are passed to imapsync, and are reused by independent
verification. imapsync's `--automap` (**Map standard folders automatically**)
can help you explore a mapping in preflight, but live runs with automap are
rejected because the engine's resolved mapping cannot be verified
independently. MailSwiftSync does not provide a separately validated
Gmail/Microsoft 365/Fastmail namespace-translation engine. Differences such as
`Sent Items`/`Sent Mail`, `[Gmail]/Sent Mail`, `All Mail`/Archive, and
`Deleted Items`/Trash/Junk remain provider- and mailbox-specific. Provider
mapping is subject to live validation; review the preflight result before a
pilot and do not infer that matching names or labels imply identical semantics.

## Transport and TLS boundary

MailSwiftSync's native IMAP readiness probes use Rustls with certificate and
hostname verification and optional enterprise CA material. A leaf-certificate
pin is checked by the probe, but pinned plans are rejected before transfer
because the external engine cannot enforce the pin on every connection. The actual imapsync transfer is an
external process and performs TLS validation through its own Perl/SSL runtime.
MailSwiftSync passes the selected encrypted transport, certificate-verification
settings, and any configured CA file to imapsync, but the two stacks are not the
same implementation. A successful native probe is therefore readiness evidence,
not a guarantee that the external engine's runtime will accept every
certificate or negotiate the same protocol details.

## Gmail / Google Workspace

### Prerequisites
- Google Account with Gmail IMAP access permitted by the account or Workspace administrator
- OAuth 2.0 credentials and an IMAP-scoped token (preferred; see [OAUTH_SETUP.md](OAUTH_SETUP.md))
- Access to Google Account settings
- Destination mailbox ready to receive migration

### Step 1: Confirm Gmail IMAP access

Confirm that IMAP access is permitted for the account. For Google Workspace,
the administrator may control this centrally; for accounts that expose a Gmail
IMAP setting, review it in [Gmail Settings](https://mail.google.com/mail/u/0/#settings/general).
Follow Google's current account or Workspace instructions if the setting is not
shown.

### Step 2: Choose Gmail authentication

OAuth 2.0 / XOAUTH2 is the preferred and default authentication path for Gmail,
especially for Google Workspace. Workspace accounts must use OAuth for
third-party mail-client connections; do not enter the user's normal Google
account password. Follow [OAUTH_SETUP.md](OAUTH_SETUP.md) to obtain the
refresh token and configure automatic refresh where supported.

As a fallback, an app password may be used only when Google makes app
passwords available for that account and the account's policy permits password
IMAP access. App passwords require 2-Step Verification:

1. Go to [Google Account Security](https://myaccount.google.com/security)
2. Click "App passwords" (under "Signing in to Google")
3. Select "Mail" and "Windows Computer" (or your OS)
4. Google generates a 16-character password
5. **Copy this password immediately** — you won't see it again
6. Use this password in MailSwiftSync instead of your account password

If the "App passwords" option is unavailable, use OAuth. Do not substitute a
regular Gmail or Google Workspace password.

### Step 3: Configure MailSwiftSync

In the account card on the **Plan** page:

```
Provider: Google Workspace   (fills imap.gmail.com, port 993, implicit TLS, OAuth)
User:     your.email@example.com
Sign-in:  Connect Google Workspace account   (browser OAuth with PKCE)
```

**Connect Google Workspace account** uses your own registered OAuth client;
the built-in guide shows the console steps and the exact scope and redirect
values, and the refresh configuration is stored in the OS keyring (see
[OAUTH_SETUP.md](OAUTH_SETUP.md)). For the app-password fallback, choose
**Password** as the authentication method under **Advanced connection
settings** and enter the app password from Step 2.

### Step 4: Verify Connection

Run a preflight check in MailSwiftSync:
- MailSwiftSync will connect to Gmail and verify IMAP access
- It will discover your folders and labels
- No messages are read or modified during preflight

### Known Issues & Workarounds

**Issue: "All Mail" folder appears as duplicate**
- Gmail's "All Mail" folder contains all labeled messages
- Expected behavior: not a migration error
- Workaround: exclude `[Gmail]/All Mail` with **Exclude source folder** under
  **Typed folder mappings** if you don't want duplicates

**Issue: MailSwiftSync reports "Too many connections"**
- Gmail limits concurrent IMAP connections per account
- MailSwiftSync recognizes Gmail's documented simultaneous-connection response,
  backs off, and halves concurrency for the affected mailbox and credential
- Workaround: lower **Concurrent workers** under **Queue settings** and set
  message/byte targets in the **Advanced** dialog; do not add raw imapsync
  flags (the Extra imapsync options field accepts only its documented
  allowlist)

**Issue: Starred messages or custom labels not migrating**
- Gmail labels are IMAP folders
- Verify in Gmail settings that "Labels" section shows expected folders
- Every selectable IMAP folder is migrated unless a typed mapping excludes it;
  the Gmail "Starred" state is the `\Flagged` flag, which encrypted imapsync
  runs verify per message

### Gmail-Specific Throttling

MailSwiftSync does not claim a Gmail-specific IMAP throughput limit. Configure
the profile's imapsync message/byte limits conservatively and adjust them from
observed IMAP responses; Gmail API quota figures do not define this IMAP path.

- Provider-specific connection pools and message rates are not automatically applied.
- A rate-limit or connection-capacity response should be retried after backoff.

For very large migrations (100k+ messages), consider:
- Running during off-peak hours
- Splitting into multiple smaller jobs by folder
- Monitoring progress on **Activity** and interrupted or failed mailboxes on
  **Recovery**; headless operators can use `mailswiftsync status <state.db>`
  and run `mailswiftsync recover <state.db>` when the ledger requires review

---

## Microsoft 365 / Outlook

**IMPORTANT: Microsoft began permanently disabling Basic Authentication for Exchange Online IMAP on October 1, 2022, and no tenant can re-enable it. App passwords no longer work for IMAP access. OAuth (Modern Authentication) is required.**

### Prerequisites
- Microsoft 365 account
- Admin access to Azure/Exchange Online
- OAuth application registered (see OAUTH_SETUP.md)
- Initial refresh token obtained (see OAUTH_SETUP.md)
- Destination mailbox ready to receive migration

### Step 1: Register OAuth Application

See **OAUTH_SETUP.md** — Microsoft 365 OAuth Setup section. You must:
1. Register an app in Azure Portal
2. Grant `IMAP.AccessAsUser.All` permission
3. Use a delegated authorization-code or device flow; create a client secret
   only for a confidential client
4. Request `offline_access` and obtain the initial refresh token using the
   delegated flow

### Step 2: Enable IMAP for Source Mailbox

As an admin in Exchange Online:

1. Go to [Exchange Admin Center](https://admin.exchange.microsoft.com)
2. Navigate to "Mailboxes"
3. Select the source mailbox
4. Under "General", click "Manage IMAP access"
5. Ensure IMAP is **enabled**
6. Click "Save"

### Step 3: Configure MailSwiftSync with OAuth

In the account card on the **Plan** page:

```
Provider: Microsoft 365   (fills outlook.office365.com, port 993, implicit TLS, OAuth)
User:     user@tenant.example
Sign-in:  Connect Microsoft 365 account   (browser OAuth with PKCE)
```

Enter the Application (client) ID and the Directory (tenant) ID or a verified
domain when prompted. MailSwiftSync stores the refresh configuration in the OS
keyring, tests a token refresh, and verifies IMAP authentication. It then
refreshes access tokens before each live launch, during qualified imapsync
reconnects, and before independent verification. A refresh token obtained
through other tooling can be stored instead under **OS keyring credentials →
Automatic OAuth refresh**.

### Step 4: Verify Connection

Run a preflight check in MailSwiftSync:
- MailSwiftSync will connect and verify IMAP access
- It will discover folders (Inbox, Sent Items, Deleted Items, etc.)
- No messages are modified during preflight

### Step 5: Check Destination Quota

Before migration, compare the source usage with the destination's discovered
quota and leave operational headroom for indexing, new mail, and provider
rounding. Exchange Online capacity is plan-, license-, mailbox-, archive-, and
tenant-dependent; there is no universal 50 GB or 100 GB threshold. A 4 GB
source does not need a 100 GB destination, while a source larger than the
destination's licensed capacity must be remediated before migration.

```powershell
# In Exchange Online PowerShell:
Get-Mailbox -Identity destination@tenant.onmicrosoft.com |
  Select DisplayName,RecipientTypeDetails,ProhibitSendQuota,ProhibitSendReceiveQuota,RecoverableItemsQuota
Get-MailboxStatistics -Identity destination@tenant.onmicrosoft.com |
  Select TotalItemSize,TotalDeletedItemSize
```

Use the reported source usage and destination limits to calculate a
documented headroom target. Do not apply a fixed `Set-Mailbox` value: mailbox
quota changes may be unavailable or inappropriate for the assigned plan and
tenant policy. If capacity is insufficient, work with the tenant administrator
to assign the required license/mailbox configuration or reduce the migration
scope before starting.


### Known Issues & Workarounds

**Issue: "Soft throttling" — migration slows significantly**
- O365 enforces soft throttling when load is high
- MailSwiftSync honors Exchange Online's `Suggested Backoff Time` as a minimum
  delay (capped at 30 minutes) and its adaptive rate domains halve concurrency
  for the throttled mailbox, credential, or tenant; no Microsoft quota values
  are encoded, and the behavior is not live-qualified
- Workaround: use conservative worker counts and message/byte limits, and run
  migration during off-peak hours

**Issue: Special folders have different names than source**
- O365 uses different names (e.g., "Deleted Items" vs "Trash")
- Map them explicitly with **Typed folder mappings** (for example
  `Trash` → `Deleted Items`); MailSwiftSync does not translate every provider
  namespace automatically
- Verify the mapping in preflight and validate it with a pilot

**Issue: Shared mailbox not accessible**
- Verify the account has "Full Access" permission
- In Exchange Admin Center: select mailbox → Delegates → Full Access

### O365-Specific Throttling

MailSwiftSync does not claim an Exchange Online-specific IMAP throughput limit.
Use the profile's imapsync message/byte limits and tune them from observed
server responses. Provider API quota documentation is not an IMAP rate limit.

- Provider-specific connection pools and message rates are not automatically applied.
- Recognized throttling and connection-limit responses trigger backoff and
  reduced concurrency automatically; tune the configured limits from what you
  observe.

---

## Fastmail

### Prerequisites
- Fastmail account
- Destination mailbox ready to receive migration

### Step 1: Create App Password

1. Go to [Fastmail Settings](https://www.fastmail.com/settings/security)
2. Under "Password & Sign In", click "App passwords"
3. Enter password and click "Generate new app password"
4. Give it a name like "MailSwiftSync Migration"
5. **Copy the password immediately**

### Step 2: Configure MailSwiftSync

In the account card on the **Plan** page:

```
Provider: Fastmail   (fills imap.fastmail.com, port 993, implicit TLS)
User:     your.email@fastmail.com
Password: [app password from Step 1]
```

### Step 3: Verify Connection

Run a preflight check in MailSwiftSync:
- MailSwiftSync will connect and verify IMAP access
- All folders should be discoverable
- No modifications made during preflight

### Known Issues & Workarounds

**Issue: JMAP vs IMAP folder differences**
- Fastmail primarily uses JMAP internally
- IMAP interface is fully functional but namespace may differ
- Express any folder differences as **Typed folder mappings**; review the
  preflight result and validate Fastmail-specific behavior with a pilot

### Fastmail-Specific Throttling

MailSwiftSync does not claim a Fastmail-specific IMAP throughput limit. Use the
profile's imapsync message/byte limits and tune them from observed server
responses rather than relying on an invented provider constant.

---

## Generic IMAP Provider

If your provider is not listed above:

### Setup Steps

1. **Determine IMAP details:**
   - IMAP host (typically imap.yourdomain.com)
   - IMAP port (usually 993 for TLS, 143 for STARTTLS)
   - TLS/STARTTLS availability
   - Authentication method (password, OAuth2, etc.)

2. **Test manually first:**
   ```bash
   # Test IMAP connection
   openssl s_client -connect imap.yourdomain.com:993
   # If STARTTLS:
   openssl s_client -connect imap.yourdomain.com:143 -starttls imap
   ```

3. **Configure in MailSwiftSync** (account card on the **Plan** page):
   - **Provider**: **Other IMAP server**
   - **Server**: [IMAP host determined above]
   - **User**: [your email/account]
   - **Password**: [your password or app password]
   - Under **Advanced connection settings**: **Port** and **TLS** (implicit
     TLS or STARTTLS) as determined above

4. **Run preflight:**
   - MailSwiftSync will attempt connection and folder discovery
   - If it fails, check:
     - Firewall/network blocking port
     - Credentials are correct
     - Provider's IMAP service is enabled

### Conservative Throttling for Unknown Providers

MailSwiftSync does not silently select a provider rate for unknown IMAP
servers. Set the profile's imapsync message/byte limits explicitly and begin
conservatively, then adjust them from observed server responses. Test changes
with small migrations first.

---

## Troubleshooting Connection Issues

### "TLS certificate validation failed"
- Provider's SSL certificate may not be trusted
- Solution: Update your OS's certificate store
- Or: Verify the certificate manually using OpenSSL

### "Authentication failed"
- Check username and password (especially app passwords)
- Verify IMAP is enabled in provider account settings
- If using 2FA/MFA, ensure you're using app password, not regular password

### "Connection timeout"
- Check firewall allows outbound IMAP connections
- If on corporate network, check for IMAP proxy requirements
- Try different IMAP port (143 STARTTLS vs 993 TLS)

### "IMAP not enabled for this account"
- Return to Step 1 above for your provider
- Some providers disable IMAP by default
- May require admin approval on corporate accounts

---

## Security Best Practices

1. **Use OAuth where the provider requires or recommends it**
2. **Use app-specific passwords only** where the provider offers them and the account policy permits them
3. **Prefer implicit TLS** (port 993); STARTTLS on port 143 is acceptable, and plain IMAP requires an explicit insecure-transport acknowledgement
4. **Don't share credentials** — create dedicated app passwords for MailSwiftSync
5. **Secure your MailSwiftSync state directory** — it contains migration state and evidence. OAuth refresh configuration and active access tokens are handled through the OS keyring and short-lived private runtime files, not the SQLite state database.
6. **Review audit logs** on provider accounts after migration completes

---

## Getting Help

If you encounter issues:

1. Run `mailswiftsync support-bundle <state.db> <support-bundle.json>` to create a diagnostic bundle
2. The bundle contains durable state classifications, platform details, and
   bounded run metadata. It excludes credentials, endpoints, project names,
   plan snapshots, mail content, and engine logs; attach a diagnostic log only
   if you deliberately captured one with `--diagnostic-log` and reviewed it
3. Review the bundle, then open an issue at https://github.com/cyberducttape/MailSwiftSync/issues with it

For provider-specific questions:
- Gmail: Check https://support.google.com/mail/answer/7190
- Microsoft 365: Check https://learn.microsoft.com/en-us/exchange/imap4-pop3/imap4-pop3
- Fastmail: Check https://www.fastmail.com/help/clients/imap.html
