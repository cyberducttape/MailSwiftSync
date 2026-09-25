# Provider-Specific Setup Guide

MailSwiftSync uses IMAP for all migrations. This guide walks through the setup required for each major provider.

Provider authentication and capacity facts are maintained in the [canonical
provider facts](docs/provider-facts.md); update that source and the validation
test when provider policy changes.

## Folder-mapping boundary

For imapsync migrations, MailSwiftSync currently relies on imapsync's standard
folder behavior and the optional `--automap` setting. It displays the proposed
mapping during preflight, but it does not provide a separately validated
Gmail/Microsoft 365/Fastmail namespace-translation engine. Differences such as
`Sent Items`/`Sent Mail`, `[Gmail]/Sent Mail`, `All Mail`/Archive, and
`Deleted Items`/Trash/Junk remain provider- and mailbox-specific. Provider
mapping is subject to live validation; review the preflight result before a
pilot and do not infer that matching names or labels imply identical semantics.

## Transport and TLS boundary

MailSwiftSync's native IMAP readiness probes use Rustls with certificate and
hostname verification, optional enterprise CA material, and optional
application-level leaf-certificate pinning. The actual imapsync transfer is an
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

In MailSwiftSync account settings:

```
Provider: Gmail (IMAP)
IMAP Host: imap.gmail.com
IMAP Port: 993
Security: TLS/SSL
Username: your.email@gmail.com
Password: [app password from Step 2, only when using the fallback]
```

For OAuth, configure the account with the access-token workflow described in
`OAUTH_SETUP.md` instead of entering a password.

### Step 4: Verify Connection

Run a preflight check in MailSwiftSync:
- MailSwiftSync will connect to Gmail and verify IMAP access
- It will discover your folders and labels
- No messages are read or modified during preflight

### Known Issues & Workarounds

**Issue: "All Mail" folder appears as duplicate**
- Gmail's "All Mail" folder contains all labeled messages
- Expected behavior: not a migration error
- Workaround: Exclude "All Mail" if you don't want duplicates

**Issue: MailSwiftSync reports "Too many connections"**
- Gmail limits concurrent IMAP connections per account
- Workaround: In MailSwiftSync settings, reduce batch size and increase delay
- Example: `--maxbatchsize=50 --sleepdelay=1`

**Issue: Starred messages or custom labels not migrating**
- Gmail labels are IMAP folders
- Verify in Gmail settings that "Labels" section shows expected folders
- MailSwiftSync will migrate all IMAP folders automatically

### Gmail-Specific Throttling

MailSwiftSync does not claim a Gmail-specific IMAP throughput limit. Configure
the profile's imapsync message/byte limits conservatively and adjust them from
observed IMAP responses; Gmail API quota figures do not define this IMAP path.

- Provider-specific connection pools and message rates are not automatically applied.
- A rate-limit or connection-capacity response should be retried after backoff.

For very large migrations (100k+ messages), consider:
- Running during off-peak hours
- Splitting into multiple smaller jobs by folder
- Monitoring recovery with `mailswiftsync status <state.db>` and explicitly running `mailswiftsync recover <state.db>` when the ledger requires review; a dedicated recovery dashboard is not currently exposed

---

## Microsoft 365 / Outlook

**IMPORTANT: Microsoft removed Basic Authentication from Exchange Online IMAP in September 2023. App passwords no longer work for IMAP access. OAuth (Modern Authentication) is required.**

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

In MailSwiftSync account settings:

```
Provider: Microsoft 365 (OAuth)
Token Endpoint: https://login.microsoftonline.com/common/oauth2/v2.0/token
Client ID: [from Azure app registration]
Client Secret: [from Azure app registration]
Refresh Token: [from OAUTH_SETUP.md Step 4]
```

MailSwiftSync will automatically refresh OAuth tokens before each migration run.

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
- Treat slower responses, connection limits, server-busy responses, and
  timeouts as operational signals; MailSwiftSync does not currently apply a
  provider-specific adaptive throttle
- Workaround: use conservative profile limits and run migration during
  off-peak hours

**Issue: Special folders have different names than source**
- O365 uses different names (e.g., "Deleted Items" vs "Trash")
- MailSwiftSync relies on imapsync's standard mapping and optional `--automap`;
  it does not independently translate every provider namespace
- Verify the proposed folder mapping in preflight and validate it with a pilot

**Issue: Shared mailbox not accessible**
- Verify the account has "Full Access" permission
- In Exchange Admin Center: select mailbox → Delegates → Full Access

### O365-Specific Throttling

MailSwiftSync does not claim an Exchange Online-specific IMAP throughput limit.
Use the profile's imapsync message/byte limits and tune them from observed
server responses. Provider API quota documentation is not an IMAP rate limit.

- Provider-specific connection pools and message rates are not automatically applied.
- Treat connection limits, server-busy responses, and timeouts as signals to back off.

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

In MailSwiftSync account settings:

```
Provider: Fastmail (IMAP)
IMAP Host: imap.fastmail.com
IMAP Port: 993
Security: TLS/SSL
Username: your.email@fastmail.com
Password: [App password from Step 1]
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
- MailSwiftSync relies on imapsync's standard folder handling; review the
  preflight mapping and validate Fastmail-specific behavior with a pilot

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

3. **Configure in MailSwiftSync:**
   - IMAP Host: [determined above]
   - IMAP Port: [determined above]
   - Security: TLS/SSL or STARTTLS
   - Username: [your email/account]
   - Password: [your password or app password]

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
3. **Use TLS/SSL** (port 993) instead of plain STARTTLS when possible
4. **Don't share credentials** — create dedicated app passwords for MailSwiftSync
5. **Secure your MailSwiftSync state directory** — it contains migration state and evidence. OAuth refresh configuration and active access tokens are handled through the OS keyring and short-lived private runtime files, not the SQLite state database.
6. **Review audit logs** on provider accounts after migration completes

---

## Getting Help

If you encounter issues:

1. Run `mailswiftsync support-bundle [state.db]` to create a diagnostic bundle
2. The bundle includes sanitized logs (credentials removed)
3. Open an issue at https://github.com/cyberducttape/MailSwiftSync/issues with the bundle

For provider-specific questions:
- Gmail: Check https://support.google.com/mail/answer/7190
- Microsoft 365: Check https://learn.microsoft.com/en-us/exchange/imap4-pop3/imap4-pop3
- Fastmail: Check https://www.fastmail.com/help/clients/imap.html
