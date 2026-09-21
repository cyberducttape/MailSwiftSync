# Gmail / Google Workspace IMAP Provider Test Setup

This guide describes how to set up disposable test accounts and run the MailSwiftSync provider integration test against Gmail or Google Workspace.

## Prerequisites

- MailSwiftSync binary installed and in PATH
- `imapsync 2.314` installed and in PATH
- Google Workspace domain with admin access (or personal Gmail accounts for testing)
- OAuth/XOAUTH2 credentials, or an eligible account for optional app-password testing

## Creating Test Accounts

### Option 1: Google Workspace (Organization Admin)

If you have admin access to a Google Workspace domain:

1. **Create two temporary user accounts:**
   ```bash
   # Using Google Admin Console or gcloud CLI
   gcloud identity users create \
     --email=mailswiftsync-source-test@your-workspace.example.com
   
   gcloud identity users create \
     --email=mailswiftsync-dest-test@your-workspace.example.com
   ```

2. **Prepare authentication:**
   - Prefer OAuth/XOAUTH2 for IMAP.
   - For password-based IMAP testing only, use app passwords when the account is eligible; store secrets in `/run/secrets/gmail-source` and `/run/secrets/gmail-dest` with 600 permissions.

3. **Enable IMAP access:**
   - In Google Admin Console → Security → API Controls → Domain-wide Delegation
   - Ensure IMAP is not restricted for these accounts

4. **Wait 5-10 minutes for account provisioning**

### Option 2: Personal Gmail Accounts

For testing without Workspace:

1. **Create two disposable Gmail accounts** (via https://accounts.google.com/signup)
   - Source: `mailswiftsync-source-test+XXXXXX@gmail.com`
   - Destination: `mailswiftsync-dest-test+XXXXXX@gmail.com`

2. **Choose authentication:**
   - Prefer OAuth/XOAUTH2 using the `https://mail.google.com/` scope.
   - For password-based IMAP testing, generate an app password at https://myaccount.google.com/apppasswords
   - Select `Mail` and `Windows Computer`
   - Google will generate a 16-character password
   - Save these in `/run/secrets/gmail-source` and `/run/secrets/gmail-dest`

## Populating Test Data

Before migration, add test mailboxes to the source account:

```bash
#!/usr/bin/env bash
source_user="mailswiftsync-source-test@gmail.com"
source_password="$(cat /run/secrets/gmail-source)"
dest_user="mailswiftsync-dest-test@gmail.com"
dest_password="$(cat /run/secrets/gmail-dest)"

# Create a few test mailboxes using imapsync's list/create logic
# or use Gmail's web UI to create labels:
#   - Test-Small (5 messages)
#   - Test-Large (1000+ messages)
#   - Test-Unicode (测试, тест, etc.)
#   - Test-Attachments (messages with 2MB+ attachments)

# Verify IMAP can see them:
imapsync --host1 imap.gmail.com --user1 "$source_user" --password1 "$source_password" \
  --host2 imap.gmail.com --user2 "$dest_user" --password2 "$dest_password" \
  --listfolders --nosyncinternaldates --nosyncacls
```

## Setting Up Secret Files

Create owner-only secret files for credentials:

```bash
# Create /run/secrets directory (or use /tmp/mailswiftsync-secrets)
sudo mkdir -p /run/secrets
sudo chmod 700 /run/secrets

# Save source account password (from app password or workspace password)
echo -n "16-character-app-password" | sudo tee /run/secrets/gmail-source >/dev/null
sudo chmod 600 /run/secrets/gmail-source

# Save destination account password
echo -n "16-character-app-password" | sudo tee /run/secrets/gmail-dest >/dev/null
sudo chmod 600 /run/secrets/gmail-dest

# Verify permissions
ls -la /run/secrets/gmail-*
# Should show: -rw------- 1 root root 16 Sep 17 20:15 gmail-dest
#              -rw------- 1 root root 16 Sep 17 20:15 gmail-source
```

## Running the Provider Integration Test

### Step 1: Prepare environment

```bash
export MAILSWIFTSYNC_PROVIDER_BINARY="/usr/local/bin/mailswiftsync"
export MAILSWIFTSYNC_PROVIDER="gmail"
export MAILSWIFTSYNC_PROVIDER_SOURCE_ENDPOINT="imap.gmail.com:993"
export MAILSWIFTSYNC_PROVIDER_SOURCE_USER="mailswiftsync-source-test@gmail.com"
export MAILSWIFTSYNC_PROVIDER_SOURCE_SECRET="/run/secrets/gmail-source"
export MAILSWIFTSYNC_PROVIDER_DEST_ENDPOINT="imap.gmail.com:993"
export MAILSWIFTSYNC_PROVIDER_DEST_USER="mailswiftsync-dest-test@gmail.com"
export MAILSWIFTSYNC_PROVIDER_DEST_SECRET="/run/secrets/gmail-dest"
export MAILSWIFTSYNC_EVIDENCE_OUTPUT="/tmp/gmail-provider-evidence"
```

### Step 2: Run the test

```bash
bash scripts/provider-integration-test.sh gmail
```

### Expected Output

```
=== Starting dry pilot for gmail ===
✓ Dry pilot preflight succeeded
=== Starting live migration for gmail ===
✓ Live migration succeeded
=== Exporting evidence for gmail ===
✓ Customer proof exported
=== Verifying proof integrity ===
✓ Proof integrity verified
=== Test Summary for gmail ===
[durable state summary]
✓ All tests passed for gmail
Evidence: /tmp/gmail-provider-evidence/gmail-live_pilot.json
```

## Recording Evidence

After a successful test run, document the results:

1. **Test metadata template** — create this file only after an actual pilot; the
   example below is a template and is not evidence:
   ```markdown
   # Gmail Provider Test — YYYY-MM-DD
   
   **Accounts:**
   - Source: mailswiftsync-source-test@gmail.com (disposable)
   - Dest: mailswiftsync-dest-test@gmail.com (disposable)
   
   **Engine Versions:**
   - MailSwiftSync: 0.1.0 (packaged)
   - imapsync: 2.314
   
   **Test Results:**
   - Dry pilot: [record actual result]
   - Live pilot: [record actual result]
   - Recovery: [record actual result]
   - Evidence: [record actual result]
   
   **Discovered Quirks:**
   - Gmail's [Gmail]/All Mail folder contains all messages (including sent); not migrated by default
   - Sent label messages appear in destination; aggregate count matches source
   - SPECIAL-USE folders discovered: \All, \Sent, \Drafts, \Trash
   
   **Cleanup:**
   - Accounts deleted via Google Admin Console
   - No residual data
   ```

2. **Customer proof** — save anonymized copy (no credentials or endpoints):
   ```bash
   cat /tmp/gmail-provider-evidence/customer-proof.json | jq '.' \
     > docs/provider-tests/gmail-proof-YYYY-MM-DD.json
   # Evidence records are in the same directory, one per required phase.
   ls -1 /tmp/gmail-provider-evidence/*-to-*-*.json
   ```

3. **Update compatibility matrix row:**
   Edit `docs/compatibility-matrix.md` and update the Gmail row:
   ```markdown
   | Gmail/Workspace | Gmail/Workspace | imapsync | [TLS/auth] | Gmail labels → [Gmail]/ folders | [versions] | [actual dry-pilot evidence] | [actual live-pilot evidence] | [actual recovery evidence] | [actual evidence result] | Test metadata: docs/provider-tests/gmail-test-YYYY-MM-DD.md |
   ```

## Cleanup

After testing, **delete the disposable accounts:**

```bash
# Google Workspace (via gcloud)
gcloud identity users delete mailswiftsync-source-test@your-workspace.example.com
gcloud identity users delete mailswiftsync-dest-test@your-workspace.example.com

# Personal Gmail
- Manually delete accounts via https://myaccount.google.com/account
- Or use gcloud to clear app passwords

# Delete secret files
shred -u /run/secrets/gmail-source /run/secrets/gmail-dest
```

## Troubleshooting

### "IMAP access is disabled for your account"
- **Workspace:** Ensure IMAP is not restricted via Security → API Controls and that the OAuth client has the required scope
- **Gmail:** Prefer OAuth/XOAUTH2; use an app password only if the account is eligible for password-based IMAP

### "Invalid credentials"
- Verify the OAuth scope/token, or verify the app password if using password-based IMAP
- Check that secret files contain only the password (no extra newlines)
- Test with `imapsync` directly first to isolate MailSwiftSync issues

### "Folder list incomplete"
- Gmail may throttle LIST if run repeatedly; wait 10+ seconds between tests
- Verify `[Gmail]/ prefix is present (indicates IMAP namespace discovery worked)

### "Message count mismatch"
- Gmail's [Gmail]/All Mail folder is read-only and not migrated by default
- Sent messages may appear on both source and destination (Gmail behavior)
- Drafts folder behavior differs between Gmail and standard IMAP

## Next Steps

Once this test is documented and recorded:
1. Update the compatibility matrix row with evidence markers
2. Create a CI/CD job to run this test on a schedule (monthly or per-release)
3. Add similar tests for Microsoft 365 and Fastmail following the same pattern
