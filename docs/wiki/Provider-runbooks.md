# Provider runbooks

[Run a safe migration](Run-a-safe-migration.md) covers the general MailSwiftSync
workflow; this page adds the specific endpoint, auth, and folder-mapping details
for pairings MSPs run most often. Provider IMAP requirements and endpoints
change over time — verify current values against the provider's own
documentation before a production migration, and always validate with
**Preflight** against a real account before trusting anything below.

Every runbook below assumes: engine = **imapsync fallback** (neither Gmail nor
Microsoft 365 exposes Dovecot-native administration), a test destination
mailbox first, and Preflight before Live migration every time.

## Gmail / Google Workspace → Microsoft 365

| | Source (Google) | Destination (Microsoft 365) |
| --- | --- | --- |
| Host | `imap.gmail.com` | `outlook.office365.com` |
| Port / TLS | 993, implicit IMAPS | 993, implicit IMAPS |
| Auth | OAuth 2.0 / XOAUTH2 preferred; eligible Google accounts may use an app password only for a controlled compatibility test | OAuth 2.0 / XOAUTH2 (Microsoft retired Basic Auth for IMAP) |

- **Authentication differs by provider and account type.** Use OAuth 2.0 /
  XOAUTH2 for Google Workspace and personal Gmail by default. A personal or
  other eligible Google account may use an app password for a deliberately
  scoped compatibility test when Google exposes that option; never use an
  ordinary account password. Microsoft 365 IMAP requires OAuth 2.0 /
  XOAUTH2. Register an app in each provider's console (Google Cloud Console
  for Gmail; Entra ID / Azure AD app registration for Microsoft 365) with IMAP
  scope, obtain an access token (and a refresh token if you want
  MailSwiftSync's automatic-refresh keyring entry, described in [Profiles,
  passwords, and security](Security-and-profiles.md)), and enter it or a
  keyring reference in the plan.
- **Folders.** Gmail exposes labels as IMAP folders under a namespace that
  includes `[Gmail]/All Mail`, `[Gmail]/Sent Mail`, `[Gmail]/Trash`, etc.
  `--automap` maps the common ones, but a message with multiple Gmail labels
  appears in each corresponding folder on the source side — decide up front
  whether you want `[Gmail]/All Mail` migrated (usually yes, as the
  authoritative complete set) and the individual label folders skipped with
  `--justfolders`-style planning to avoid duplicate delivery, or vice versa.
  Preview the folder mapping in Preflight before a live run and adjust.
- **Rate limits.** Google enforces per-account and per-project IMAP
  throttling; keep the per-mailbox worker count and message/byte throttles
  conservative for a first pass, and expect `capacity`-classified failures to
  retry with backoff rather than fail outright (see the failure taxonomy in
  [Bulk migrations](Bulk-migrations.md)).

## Microsoft 365 → Google Workspace

| | Source (Microsoft 365) | Destination (Google Workspace) |
| --- | --- | --- |
| Host | `outlook.office365.com` | `imap.gmail.com` |
| Port / TLS | 993, implicit IMAPS | 993, implicit IMAPS |
| Auth | OAuth 2.0 / XOAUTH2 (required) | OAuth 2.0 / XOAUTH2 preferred; eligible Google accounts may use an app password only for a controlled compatibility test |

- Microsoft 365 requires OAuth. For Google Workspace or personal Gmail,
  prefer OAuth; an app password is a conditional Google-account fallback only
  when the account is eligible and the test intentionally covers password IMAP.
  The same app-registration requirement applies to OAuth on each side.
- **Folders.** Microsoft 365's special folders (`Sent Items`, `Deleted Items`,
  `Junk Email`) do not share Gmail's names; confirm the automap result in
  Preflight rather than assuming a 1:1 match, and check that Gmail's IMAP
  access is enabled for the destination account (Google Workspace admins can
  disable IMAP per-user or org-wide).

## Hosted cPanel/Dovecot → Google Workspace or Microsoft 365

This is the pairing MailSwiftSync's own product integration lab exercises
directly against a real Dovecot server (see
[release readiness](../release-readiness.md)), so it is the best-covered path.

| | Source (cPanel/Dovecot) | Destination |
| --- | --- | --- |
| Host | the mail hostname the host gives you — often `mail.<domain>` or the server hostname, **not** always the domain itself | as above |
| Port / TLS | typically 993 implicit IMAPS, sometimes 143 + STARTTLS depending on the host's configuration | as above |
| Auth | mailbox password (or an app-specific password if 2FA is enabled at the panel level) | OAuth 2.0 / XOAUTH2 |

- **Get the real mail hostname from the panel**, not just the domain: cPanel
  frequently serves mail on a hostname distinct from the website
  (`mail.example.com`, or the underlying server's own hostname with a
  certificate that only matches that name). Using the wrong hostname is the
  most common first-preflight failure for this pairing — a certificate
  mismatch or connection refusal on port 993 usually means the wrong host was
  entered, not a broken server.
- **Certificate trust.** Many budget hosts still run older or non-public-CA
  certificates. If Preflight reports a certificate validation failure and you
  have independently confirmed the host and cert are legitimate, use
  **Enterprise certificate trust** to supply the specific CA bundle — never
  disable verification to work around it.
- **This is a real Dovecot server**, so if you administer the destination
  too, Dovecot-native execution may apply instead of imapsync; see
  [Run a safe migration](Run-a-safe-migration.md#1-choose-the-right-engine).
  For a hosted source you don't administer, imapsync fallback is the only
  option.

## Generic hosted IMAP → Microsoft 365 or Google Workspace

For any source not covered above (a smaller regional host, a legacy on-prem
IMAP server, another hosting panel):

1. Get the exact IMAP hostname, port, and TLS mode (implicit IMAPS vs.
   STARTTLS vs. — rare and not recommended — plaintext) from the source
   host's own documentation or support. Do not guess; a wrong TLS mode fails
   fast in Preflight, but a wrong port can hang until timeout.
2. Confirm the destination's OAuth requirement. Microsoft 365 requires OAuth;
   Google Workspace and personal Gmail should use OAuth by default, while an
   eligible Google account may be tested with an app password. Never assume a
   regular account password is accepted by either provider.
3. Run Preflight and read the folder mapping before assuming automap did the
   right thing — a source with a nonstandard folder naming scheme (anything
   outside `INBOX`/`Sent`/`Trash`/`Drafts`/`Junk`) is exactly what automap
   is least reliable for.
4. Treat source quota as informational for a read-only migration. A full
   source quota does not prevent MailSwiftSync from reading existing mail and
   MailSwiftSync never deletes source messages. Check the destination quota
   separately: an exhausted destination quota blocks live admission, while
   unknown or partial quota data requires an operator capacity check.

## After the runbook: what these pairings don't give you

None of the above changes MailSwiftSync's current evidence boundary:
aggregate/engine evidence, not independent per-message reconciliation (see
[release readiness](../release-readiness.md)). For a customer-facing
migration, still export and review the verification report before declaring
the project complete, regardless of which provider pairing you ran.
