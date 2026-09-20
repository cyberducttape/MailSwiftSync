# Provider-Specific Setup Guide

MailSwiftSync uses IMAP for all migrations. This guide walks through the setup required for each major provider.

## Gmail / Google Workspace

### Prerequisites
- Gmail account with 2-Step Verification enabled (recommended)
- Access to Google Account settings
- Destination mailbox ready to receive migration

### Step 1: Enable IMAP in Gmail

1. Go to [Gmail Settings](https://mail.google.com/mail/u/0/#settings/general)
2. Scroll to "IMAP Access"
3. Select "Enable IMAP"
4. Click "Save Changes"

### Step 2: Generate App Password

If you have 2-Step Verification enabled:

1. Go to [Google Account Security](https://myaccount.google.com/security)
2. Click "App passwords" (under "Signing in to Google")
3. Select "Mail" and "Windows Computer" (or your OS)
4. Google generates a 16-character password
5. **Copy this password immediately** — you won't see it again
6. Use this password in MailSwiftSync instead of your account password

If 2-Step Verification is not enabled:
- You can use your regular Gmail password
- **Not recommended for security reasons**

### Step 3: Configure MailSwiftSync

In MailSwiftSync account settings:

```
Provider: Gmail (IMAP)
IMAP Host: imap.gmail.com
IMAP Port: 993
Security: TLS/SSL
Username: your.email@gmail.com
Password: [16-character app password from Step 2]
```

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

MailSwiftSync automatically limits Gmail migrations to **100 messages/second** to respect Gmail's API limits.

- Connection pool size: 4
- Batch size: 100 messages
- Recommended wait on rate limit: 60 seconds

For very large migrations (100k+ messages), consider:
- Running during off-peak hours
- Splitting into multiple smaller jobs by folder
- Enabling the recovery dashboard to monitor progress

---

## Microsoft 365 / Outlook

### Prerequisites
- Microsoft 365 account
- Admin access to Exchange Online (for mailbox IMAP setup)
- Destination mailbox ready to receive migration

### Step 1: Enable IMAP for Source Mailbox

As an admin in Exchange Online:

1. Go to [Exchange Admin Center](https://admin.exchange.microsoft.com)
2. Navigate to "Mailboxes"
3. Select the source mailbox
4. Under "General", click "Manage IMAP access"
5. Ensure IMAP is **enabled**
6. Click "Save"

### Step 2: Create App Password (if MFA enabled)

If Multi-Factor Authentication is enabled:

1. Go to [Microsoft Account Security](https://account.microsoft.com/security)
2. Click "Advanced security options"
3. Under "App passwords", create a new password for "Mail" and "Other (custom)"
4. **Copy the password immediately**

If MFA is not enabled:
- You can use your Microsoft 365 password
- **Not recommended** — enable MFA for security

### Step 3: Configure MailSwiftSync

In MailSwiftSync account settings:

```
Provider: Microsoft 365 (IMAP)
IMAP Host: imap.outlook.com
IMAP Port: 993
Security: TLS/SSL
Username: your.email@microsoft.com
Password: [App password from Step 2, or your password if no MFA]
```

### Step 4: Verify Connection

Run a preflight check in MailSwiftSync:
- MailSwiftSync will connect and verify IMAP access
- It will discover folders (Inbox, Sent Items, Deleted Items, etc.)
- No messages are modified during preflight

### Step 5: Check Destination Quota

Before migration, ensure destination has sufficient quota:

```powershell
# In Exchange Online PowerShell:
Get-Mailbox -Identity destination@tenant.onmicrosoft.com | Select UsageLocation, ProhibitSendQuota, ProhibitSendReceiveQuota
```

If quota is low (< 100GB), increase it:

```powershell
Set-Mailbox -Identity destination@tenant.onmicrosoft.com -ProhibitSendQuota 100GB -ProhibitSendReceiveQuota 100GB
```

### Known Issues & Workarounds

**Issue: "Soft throttling" — migration slows significantly**
- O365 enforces soft throttling when load is high
- Expected behavior: MailSwiftSync will automatically back off
- Workaround: Run migration during off-peak hours

**Issue: Special folders have different names than source**
- O365 uses different names (e.g., "Deleted Items" vs "Trash")
- MailSwiftSync handles mapping automatically
- Verify folder mapping in preflight step

**Issue: Shared mailbox not accessible**
- Verify the account has "Full Access" permission
- In Exchange Admin Center: select mailbox → Delegates → Full Access

### O365-Specific Throttling

MailSwiftSync automatically limits O365 migrations to **150 messages/second**.

- Connection pool size: 6
- Batch size: 200 messages
- Recommended wait on soft throttling: 10+ seconds before retry

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
- MailSwiftSync handles this transparently

### Fastmail-Specific Throttling

MailSwiftSync automatically limits Fastmail migrations to **50 messages/second** (conservative default).

- Connection pool size: 2
- Batch size: 50 messages
- No known rate limiting issues; this is a safety default

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

For providers without documented rate limits, MailSwiftSync uses:
- **20 messages/second** (very conservative)
- Connection pool size: 1
- Batch size: 25 messages

You can increase these in the provider settings if migration is too slow, but test cautiously with small migrations first.

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

1. **Always use app-specific passwords** if your provider supports them
2. **Enable 2-Step Verification** on source and destination accounts
3. **Use TLS/SSL** (port 993) instead of plain STARTTLS when possible
4. **Don't share credentials** — create dedicated app passwords for MailSwiftSync
5. **Secure your MailSwiftSync state directory** — it contains OAuth tokens
6. **Review audit logs** on provider accounts after migration completes

---

## Getting Help

If you encounter issues:

1. Run `mailswiftsync support-bundle [state.db]` to create a diagnostic bundle
2. The bundle includes sanitized logs (credentials removed)
3. Open an issue at https://github.com/itchyitchy123/MailSwiftSync/issues with the bundle

For provider-specific questions:
- Gmail: Check https://support.google.com/mail/answer/7190
- Microsoft 365: Check https://learn.microsoft.com/en-us/exchange/imap4-pop3/imap4-pop3
- Fastmail: Check https://www.fastmail.com/help/clients/imap.html
