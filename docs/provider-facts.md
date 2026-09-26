# Canonical Provider Facts

This is the maintenance source for provider facts repeated in operator docs,
UI presets, and runbooks. Update this page first, then update copied wording
and the documentation validation test in the same change.

## Google Workspace

- OAuth 2.0 / XOAUTH2 is the preferred and default authentication path.
- Google Workspace third-party mail-client connections must use OAuth.
- A normal Workspace account password must not be documented as an IMAP
  fallback. App-password availability is policy-dependent and is not the
  normal qualification path.

## Personal Gmail

- OAuth 2.0 / XOAUTH2 is the preferred authentication path.
- An app password is a conditional fallback for an eligible Google account
  with 2-Step Verification when Google exposes the option; it is suitable for
  a deliberately scoped compatibility test, not a general password promise.
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
