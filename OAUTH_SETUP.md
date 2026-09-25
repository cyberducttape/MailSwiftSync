# OAuth Token Configuration Guide

MailSwiftSync supports OAuth2 token refresh for unattended migrations. This enables long-running migrations to automatically refresh access tokens without operator intervention.

## Authentication model

MailSwiftSync consumes a provider-issued access token and can refresh it when
the operator supplies a refresh-token configuration. It does not perform
provider consent or choose an OAuth flow on the operator's behalf.

There are two distinct OAuth models:

- **Delegated mailbox OAuth:** a user signs in through authorization code or
  device flow. Request the provider's IMAP delegated permission and
  `offline_access` when unattended refresh is required. This produces a
  user-context access token and, when approved, a refresh token.
- **Application/app-only OAuth:** a service principal uses client credentials.
  No user signs in and no refresh token is issued; the application requests
  new access tokens with its credentials. MailSwiftSync's current unattended
  refresh settings are designed for delegated refresh tokens, so app-only
  Exchange configuration is documented separately and is not implied to be a
  supported MailSwiftSync credential workflow.

App passwords are provider- and account-specific password authentication. They
are not a general OAuth substitute and do not work as an Exchange Online IMAP
workaround after Basic Authentication removal.

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

Use Google's OAuth library to obtain the raw refresh token required by
MailSwiftSync:

**Installed application flow (recommended)**

```python
from google_auth_oauthlib.flow import InstalledAppFlow
import json

# Load the client_secret.json from Step 1
flow = InstalledAppFlow.from_client_secrets_file(
    'client_secret.json',
    scopes=['https://mail.google.com/']
)

# This opens a browser for consent
creds = flow.run_local_server(port=0)

# Print the refresh token
print("Refresh token:", creds.refresh_token)
print("Client ID:", creds.client_id)
print("Client Secret:", creds.client_secret)
```

Application Default Credentials are not a supported MailSwiftSync handoff.
`gcloud auth application-default login` writes an ADC credential cache for
Google client libraries; MailSwiftSync neither reads that cache nor accepts it
in place of its raw refresh-token setting. Even when gcloud is supplied the
required `--client-id-file` and `--scopes=https://mail.google.com/` arguments,
the resulting ADC file is a different credential-storage model. Do not extract
or copy tokens from the ADC cache into MailSwiftSync.

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

MailSwiftSync requires the IMAP-specific OAuth scope:
- `https://mail.google.com/` — IMAP/POP/SMTP access (required for all email protocols)

**Important:** Do NOT use Gmail Graph API scopes like `gmail.readonly` — those work for Gmail API but NOT for IMAP protocol access. The correct scope for IMAP is `https://mail.google.com/`.

This scope grants IMAP access and allows MailSwiftSync to:
- Read messages via IMAP
- List folders via IMAP
- Access mailbox metadata via IMAP

**For Google Workspace domain-wide delegation (admin migrations):**
Use the `gmail.imap_admin` OAuth scope instead. See Google's domain-wide delegation documentation for setup.

---

## Microsoft 365 OAuth

**Note:** Microsoft permanently disabled Basic Authentication from Exchange Online beginning October 1, 2022. All tenants now have Basic Authentication disabled; no further extension periods are granted. OAuth (Modern Authentication) is the only supported method for IMAP access.

### Step 1: Register Azure Application

1. Go to [Azure Portal](https://portal.azure.com)
2. Navigate to "App registrations"
3. Click "New registration"
4. Name: "MailSwiftSync Migration"
5. Supported account types: "Accounts in this organizational directory only"
6. Redirect URI: `http://localhost` (required for desktop auth flow)
7. Click "Register"
8. Save the **Application (client) ID** from the overview page

### Step 2: Grant API Permissions

1. In your app registration, go to "API permissions"
2. Click "Add a permission"
3. Select "APIs my organization uses"
4. Search for "Office 365 Exchange Online"
5. Select "Delegated permissions"
6. Find and add: `IMAP.AccessAsUser.All`
7. Click "Grant admin consent"

**Important:** Do NOT use Microsoft Graph scope `Mail.Read` — that's for Graph API. IMAP authentication requires the Exchange Online delegated scope `https://outlook.office.com/IMAP.AccessAsUser.All` in the authorization request.

### Step 3: Choose the delegated client type

For device authorization or another public-client flow, enable public client
flows in the app registration and do not create a client secret. A secret is
needed only when using a confidential authorization-code client. If using that
model:

1. Go to "Certificates & secrets"
2. Click "New client secret"
3. Description: "MailSwiftSync"
4. Set the shortest practical expiry
5. Click "Add"
6. **Copy the secret value immediately** — you won't see it again

Do not mix a public-client device flow with the client-credentials flow. The
latter is the separate app-only model described below and does not issue a
refresh token.

### Delegated mailbox OAuth

Use authorization code or device authorization flow when a specific mailbox
user is signing in. Microsoft documents the IMAP delegated scope as:

```
https://outlook.office.com/IMAP.AccessAsUser.All
```

Request `offline_access` as well when MailSwiftSync must refresh access tokens
for unattended work. Do not substitute Microsoft Graph `Mail.Read`, and do not
use `https://outlook.office365.com/.default` for this delegated IMAP request.
See [Microsoft's IMAP OAuth documentation](https://learn.microsoft.com/en-us/exchange/client-developer/legacy-protocols/how-to-authenticate-an-imap-pop-smtp-application-by-using-oauth)
for the provider's current flow and scope details.

### Step 4: Bootstrap a raw refresh token

MailSwiftSync currently consumes a raw OAuth refresh token. It does not consume
an MSAL token cache or broker session. Do not use an MSAL device-flow sample to
bootstrap this setting: MSAL Python reserves and adds `offline_access` itself,
stores refresh tokens in its cache, and does not define a dependable workflow
for printing a raw refresh token from the authentication result.

Use a delegated authorization-code client whose registered redirect URI you
control. Send the administrator to the tenant-specific authorization endpoint:

```text
https://login.microsoftonline.com/TENANT_ID/oauth2/v2.0/authorize
  ?client_id=CLIENT_ID
  &response_type=code
  &redirect_uri=REGISTERED_REDIRECT_URI
  &response_mode=query
  &scope=https%3A%2F%2Foutlook.office.com%2FIMAP.AccessAsUser.All%20offline_access
  &state=UNPREDICTABLE_CSRF_VALUE
```

After validating that the returned `state` is identical to the value stored
before authorization, exchange the one-time code directly at:

```text
POST https://login.microsoftonline.com/TENANT_ID/oauth2/v2.0/token
Content-Type: application/x-www-form-urlencoded

client_id=CLIENT_ID
&client_secret=CLIENT_SECRET
&grant_type=authorization_code
&code=RETURNED_AUTHORIZATION_CODE
&redirect_uri=REGISTERED_REDIRECT_URI
&scope=https%3A%2F%2Foutlook.office.com%2FIMAP.AccessAsUser.All%20offline_access
```

The successful OAuth response contains the raw `refresh_token` required by the
current MailSwiftSync account configuration. Keep the authorization code,
client secret, access token, and refresh token out of shell history, process
arguments, logs, and screenshots. Public authorization-code clients must use
PKCE and omit `client_secret`; use a maintained OAuth bootstrap tool that can
return the raw token rather than adapting the confidential-client request
above without PKCE.

If an organization requires MSAL cache or broker-backed credential handling,
that integration is not currently supported. Keep refresh handling in MSAL and
do not attempt to extract its cached refresh-token records manually.

### Application access (app-only; separate workflow)

Client credentials are not delegated mailbox OAuth. They act as the
application, issue no refresh token, and require Exchange Online application
permissions and service-principal mailbox authorization. Microsoft's IMAP
app-only flow requires the `IMAP.AccessAsApp` application permission, tenant
admin consent, Exchange service-principal registration, and mailbox permission
assignment. Token requests use the Exchange app-only resource scope:

```
https://outlook.office365.com/.default
```

This is intentionally separate from the delegated scope above. MailSwiftSync
does not currently configure Exchange service principals or app-only mailbox
permissions; do not enter an app-only access token into the delegated refresh
configuration. See [Microsoft's app-only IMAP guidance](https://learn.microsoft.com/en-us/exchange/client-developer/legacy-protocols/how-to-authenticate-an-imap-pop-smtp-application-by-using-oauth#use-client-credentials-grant-flow-to-authenticate-smtp-imap-and-pop-connections)
if evaluating that deployment model.

### Step 5: Store in MailSwiftSync

In MailSwiftSync account settings, configure OAuth:

```
Provider: Microsoft 365 (OAuth)
Token Endpoint: https://login.microsoftonline.com/YOUR_TENANT_ID/oauth2/v2.0/token
Client ID: [from Step 1]
Client Secret: [from Step 3]
Refresh Token: [the refresh_token value returned by Step 4]
```

MailSwiftSync will automatically:
- Refresh the access token before each migration run
- Handle token rotation if Microsoft issues new refresh tokens
- Fail with a clear error if the refresh token expires

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
2. **Use appropriate scopes** — MailSwiftSync requires `https://mail.google.com/` (full mail access for IMAP) for Gmail and equivalent full-access scopes for other providers
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

## Getting Help

For OAuth-specific issues:

1. Check provider's OAuth documentation
2. Verify token in provider's security settings (not revoked)
3. Check MailSwiftSync logs in the support bundle
4. Open an issue with sanitized logs at https://github.com/cyberducttape/MailSwiftSync/issues

For provider-specific OAuth help:
- Gmail: https://developers.google.com/identity/protocols/oauth2
- Microsoft 365: https://learn.microsoft.com/en-us/azure/active-directory/develop/v2-oauth2-auth-code-flow
- Fastmail: https://www.fastmail.com/help/developers/oauth.html
