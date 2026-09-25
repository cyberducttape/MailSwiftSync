# Canonical Provider Facts

This is the maintenance source for provider facts repeated in operator docs,
UI presets, and runbooks. Update this page first, then update copied wording
and the documentation validation test in the same change.

## Google Workspace / Gmail

- OAuth 2.0 / XOAUTH2 is the preferred and default authentication path.
- Google Workspace third-party mail-client connections must use OAuth.
- App passwords are only a conditional fallback when Google offers them and
  the account policy permits password IMAP access.
- A regular Google account password must not be documented as an IMAP fallback.

## Microsoft 365

- Exchange Online IMAP uses OAuth; Basic Authentication and app-password
  workarounds are not supported.
- Mailbox capacity is plan-, license-, mailbox-, archive-, and tenant-dependent.
- Destination capacity must exceed source usage plus documented operational
  headroom; no universal 50 GB or 100 GB threshold should be prescribed.

## Verification wording

- Metadata reconciliation does not compare message bodies.
- Human-facing labels must say: “Metadata reconciled — message bodies not compared”.
