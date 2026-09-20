# OAuth Token Configuration Guide

MailSwiftSync supports OAuth2 token refresh for unattended migrations. This enables long-running migrations to automatically refresh access tokens without operator intervention.

## When to Use OAuth vs App Passwords

| Scenario | OAuth Tokens | App Passwords |
|----------|--------------|---------------|
| Interactive migration | ❌ Not needed | ✅ Recommended |
| Unattended/scheduled | ✅ Recommended | ❌ Not practical |
| Token expiry < 1 hour | ✅ Automatic refresh | ❌ Will fail mid-migration |
| Multi-hour migrations | ✅ Safe | ❌ High failure risk |

## OAuth Limitations in MailSwiftSync

**MailSwiftSync does NOT perform OAuth consent.** Instead, you must:

1. Register an OAuth application with the provider (once)
2. Obtain an initial refresh token using the provider's tools
3. Store the refresh token and client credentials in MailSwiftSync
4. MailSwiftSync will automatically refresh the token before each migration run

## Gmail / Google Workspace OAuth

### Step 1: Register OAuth Application

1. Go to [Google Cloud Console](https://console.cloud.google.com)
2. Create a new project or select existing one
3. Enable the "Gmail API"
4. Go to "Credentials" → "Create Credentials" → "OAuth 2.0 Client ID"
5. Application type: "Desktop application"
6. Download the JSON credentials file
7. Note the `client_id` and `client_secret` from the file

### Step 2: Obtain Initial Refresh Token

You must use Google's official tools or library to get the initial refresh token:

**Option A: Using Google CLI (recommended)**

```bash
# Install Google Cloud CLI if not already installed
# See: https://cloud.google.com/sdk/docs/install

gcloud auth application-default login
# Follow the browser flow to grant consent
# This creates local OAuth token
```

**Option B: Using a Python script**

```python
from google_auth_oauthlib.flow import InstalledAppFlow
import json

# Load the client_secret.json from Step 1
flow = InstalledAppFlow.from_client_secrets_file(
    'client_secret.json',
    scopes=['https://www.googleapis.com/auth/gmail.readonly']
)

# This opens a browser for consent
creds = flow.run_local_server(port=0)

# Print the refresh token
print("Refresh token:", creds.refresh_token)
print("Client ID:", creds.client_id)
print("Client Secret:", creds.client_secret)
```

### Step 3: Store in MailSwiftSync

In MailSwiftSync account settings, configure OAuth:

```
Provider: Gmail (OAuth)
Token Endpoint: https://oauth2.googleapis.com/token
Client ID: [from Step 1]
Client Secret: [from Step 1]
Refresh Token: [from Step 2]
```

MailSwiftSync will:
- Securely store credentials in the OS keyring
- Refresh the access token before each migration run
- Automatically obtain a new access token if the refresh token rotates

### Step 4: Verify Configuration

Run a preflight check in MailSwiftSync:
- MailSwiftSync will attempt to refresh the token
- If successful, it will show "OAuth token refreshed"
- IMAP connection will proceed with the fresh access token

### Gmail OAuth Scopes

MailSwiftSync uses the minimal required OAuth scope:
- `https://www.googleapis.com/auth/gmail.readonly` — Read-only access

This allows IMAP access but prevents MailSwiftSync from:
- Modifying messages
- Deleting folders
- Changing account settings

---

## Microsoft 365 OAuth

### Step 1: Register Azure Application

1. Go to [Azure Portal](https://portal.azure.com)
2. Navigate to "App registrations"
3. Click "New registration"
4. Name: "MailSwiftSync Migration"
5. Supported account types: "Accounts in this organizational directory only"
6. Redirect URI: (leave empty for now; not used by MailSwiftSync)
7. Click "Register"

### Step 2: Grant API Permissions

1. In your app registration, go to "API permissions"
2. Click "Add a permission"
3. Select "Microsoft Graph"
4. Select "Delegated permissions"
5. Search for and add: `IMAP.AccessAsUser.All`
6. Click "Grant admin consent"

### Step 3: Create Client Secret

1. Go to "Certificates & secrets"
2. Click "New client secret"
3. Description: "MailSwiftSync"
4. Expiry: 24 months (or your preferred duration)
5. Click "Add"
6. **Copy the secret value immediately** — you won't see it again
7. Note the Application (client) ID from the overview page

### Step 4: Obtain Initial Refresh Token

You must use Microsoft's OAuth flow to get the initial refresh token:

```bash
# Using Python
pip install msal

python3 << 'EOF'
import msal
import json

client_id = "YOUR_CLIENT_ID"
client_secret = "YOUR_CLIENT_SECRET"
authority = "https://login.microsoftonline.com/common"
scope = ["Mail.Read"]

app = msal.PublicClientApplication(
    client_id=client_id,
    authority=authority
)

# Get device code flow (for headless systems)
flow = app.initiate_device_flow(scopes=scope)
print(flow['message'])
input('Press Enter after authorizing...')

# Or use browser flow (simpler)
# result = app.acquire_token_interactive(scopes=scope)

# Get refresh token
result = app.acquire_token_by_device_flow(flow)
print("Refresh token:", result.get('refresh_token'))
print("Access token expires:", result.get('expires_in'), "seconds")
EOF
```

### Step 5: Store in MailSwiftSync

In MailSwiftSync account settings, configure OAuth:

```
Provider: Microsoft 365 (OAuth)
Token Endpoint: https://login.microsoftonline.com/common/oauth2/v2.0/token
Client ID: [from Step 3, Application ID]
Client Secret: [from Step 3, secret value]
Refresh Token: [from Step 4]
```

MailSwiftSync will automatically refresh tokens for unattended migrations.

### Step 6: Verify Configuration

Run a preflight check:
- MailSwiftSync will refresh the token
- IMAP connection will proceed with fresh credentials

---

## Fastmail OAuth

Fastmail supports OAuth but has different requirements than Gmail/O365. As of 2024, consult [Fastmail OAuth documentation](https://www.fastmail.com/help/developers/oauth.html) for current setup.

The process is similar:
1. Register an app at Fastmail
2. Obtain OAuth credentials
3. Get initial refresh token via Fastmail's auth endpoint
4. Store in MailSwiftSync

---

## OAuth Token Lifecycle

When you configure OAuth in MailSwiftSync:

1. **Storage:** Credentials stored securely in OS keyring (not in migration state files)
2. **Refresh:** Before each migration run, MailSwiftSync refreshes the access token
3. **Rotation:** If provider issues new refresh token, MailSwiftSync stores it automatically
4. **Expiry:** If refresh token expires, migration fails with clear error message

### Refresh Token Expiry

- **Gmail:** Refresh tokens expire after 6 months of inactivity
- **Microsoft 365:** Configurable, often 1-2 years
- **Fastmail:** Check provider documentation

**Pro tip:** Periodically run a preflight check to keep your refresh token active.

---

## Troubleshooting OAuth

### "OAuth token refresh failed"
1. Check network connectivity to provider's token endpoint
2. Verify client secret hasn't expired
3. Ensure refresh token hasn't been revoked (check provider security settings)
4. Try refreshing the token manually via a test request

### "Access token is invalid"
- This should not happen; MailSwiftSync refreshes before each run
- If it does occur, delete the stored OAuth config and reconfigure

### "Refresh token expired"
1. Return to the provider's OAuth setup
2. Re-authorize and get a new refresh token
3. Update in MailSwiftSync with the new token

### "Client authentication failed"
- Verify client ID and secret match provider records
- Check that client secret hasn't expired
- Ensure app hasn't been deleted from provider portal

---

## Security Best Practices for OAuth

1. **Keep client secrets secure** — store in environment variables, not in version control
2. **Use minimal scopes** — MailSwiftSync uses read-only scopes
3. **Monitor token usage** — check provider audit logs periodically
4. **Rotate tokens** — regenerate client secrets annually
5. **Review permissions** — audit OAuth app permissions in provider account
6. **Revoke when done** — remove the OAuth app after migrations complete if it's temporary

---

## Differences Between OAuth and App Passwords

| Feature | OAuth | App Password |
|---------|-------|-------------|
| Setup complexity | More complex | Simple |
| Token refresh | Automatic | Not supported |
| Multi-hour migrations | ✅ Safe | ❌ Risky |
| Revocation | Per-app in provider settings | All app passwords revoked at once |
| Audit trail | Better (scoped per app) | Mixed with account activity |

---

## When OAuth Fails Over to IMAP Password

If OAuth token refresh fails during a migration:

1. MailSwiftSync will attempt retry with exponential backoff
2. If retries exhaust, migration stops with an error
3. Check the support bundle to diagnose the issue
4. You can configure a fallback IMAP password in MailSwiftSync
5. If fallback exists, migration will retry using password-based auth

---

## Getting Help

For OAuth-specific issues:

1. Check provider's OAuth documentation
2. Verify token in provider's security settings (not revoked)
3. Check MailSwiftSync logs in the support bundle
4. Open an issue with sanitized logs at https://github.com/itchyitchy123/MailSwiftSync/issues

For provider-specific OAuth help:
- Gmail: https://developers.google.com/identity/protocols/oauth2
- Microsoft 365: https://learn.microsoft.com/en-us/azure/active-directory/develop/v2-oauth2-auth-code-flow
- Fastmail: https://www.fastmail.com/help/developers/oauth.html
