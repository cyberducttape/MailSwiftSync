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
   In the [Google Admin console](https://admin.google.com/), go to
   **Directory → Users → Add new user**. Use separate disposable accounts for
   source, destination, and recovery; bulk creation is also available from
   the Users page. The Cloud Identity command group manages groups and
   memberships, not Workspace user creation; use the Admin console for these
   accounts.

2. **Prepare authentication:**
   - Use OAuth/XOAUTH2 for Google Workspace IMAP qualification. Do not use a
     user's normal account password.
   - If the organization deliberately tests an eligible app-password path,
     follow the separate app-password section below; availability is governed
     by account and organizational policy.

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
   - Do not use the ordinary Google account password for IMAP.

### Optional app-password test (eligible Google accounts)

This is a separate compatibility test, not the recommended Workspace
authentication path. An app password may be available for a Google account
with 2-Step Verification, but Google may hide the option for organizational
accounts, security-key-only 2-Step Verification, or Advanced Protection.

1. Open [Google app passwords](https://myaccount.google.com/apppasswords).
2. Select `Mail` and an appropriate device label.
3. Save the generated 16-character app password in the secret file. Never use
   the account's ordinary password.

## Populating Test Data

Before migration, add test mailboxes to the source account:

```bash
#!/usr/bin/env bash
source_user="mailswiftsync-source-test@gmail.com"
source_secret="$(cat /run/secrets/gmail-source)"
dest_user="mailswiftsync-dest-test@gmail.com"
dest_secret="$(cat /run/secrets/gmail-dest)"

# Create a few test mailboxes using imapsync's list/create logic
# or use Gmail's web UI to create labels:
#   - Test-Small (5 messages)
#   - Test-Large (1000+ messages)
#   - Test-Unicode (测试, тест, etc.)
#   - Test-Attachments (messages with 2MB+ attachments)

# Verify IMAP can see them:
imapsync --host1 imap.gmail.com --user1 "$source_user" --password1 "$source_secret" \
  --host2 imap.gmail.com --user2 "$dest_user" --password2 "$dest_secret" \
  --listfolders --nosyncinternaldates --nosyncacls
```

## Setting Up Secret Files

Create owner-only secret files for credentials:

```bash
# Create /run/secrets directory (or use /tmp/mailswiftsync-secrets)
sudo mkdir -p /run/secrets
sudo chmod 700 /run/secrets

# Save a current OAuth access token or an eligible app password; never a
# regular Google/Workspace account password.
echo -n "oauth-access-token-or-app-password" | sudo tee /run/secrets/gmail-source >/dev/null
sudo chmod 600 /run/secrets/gmail-source

# Save the corresponding destination OAuth access token or app password.
echo -n "oauth-access-token-or-app-password" | sudo tee /run/secrets/gmail-dest >/dev/null
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
export MAILSWIFTSYNC_PROVIDER_SOURCE_AUTH="oauth2" # use password only for an eligible app-password test
export MAILSWIFTSYNC_PROVIDER_DEST_ENDPOINT="imap.gmail.com:993"
export MAILSWIFTSYNC_PROVIDER_DEST_USER="mailswiftsync-dest-test@gmail.com"
export MAILSWIFTSYNC_PROVIDER_DEST_SECRET="/run/secrets/gmail-dest"
export MAILSWIFTSYNC_PROVIDER_DEST_AUTH="oauth2" # use password only for an eligible app-password test
# Recovery must use a separate, empty destination account.
export MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_ENDPOINT="imap.gmail.com:993"
export MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_USER="mailswiftsync-recovery-test@gmail.com"
export MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_SECRET="/run/secrets/gmail-recovery"
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
Evidence: /tmp/gmail-provider-evidence/gmail-to-gmail-live_pilot.json
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
   - MailSwiftSync: 0.1.0-alpha.1 (packaged)
   - imapsync: 2.314
   
   **Test Results:**
   - Dry pilot: [record actual result]
   - Live pilot: [record actual result]
   - Recovery: [record actual result]
   - Evidence: [record actual result]

Scenario names are not evidence by themselves. Record only scenarios actually
executed by the fixture and retain the fixture manifest/dataset digest with the
phase proofs. The current harness does not complete hosted-provider release
qualification until structured observations and independent message evidence
are available.
   
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
# Google Workspace: remove disposable users in Admin console → Directory →
# Users. The Cloud Identity CLI is not the Workspace user-management path.

# Personal Gmail
- Manually delete accounts via https://myaccount.google.com/account
- Revoke app passwords from the Google Account security page if one was used

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
