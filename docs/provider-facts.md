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
- The optional forensic `body_hash` mode is separately labeled, bounded by per-message and total-byte limits, and is not implied by metadata reconciliation.

## Server hints from the account address

While an account's Server field is empty, the desktop app offers one button
derived from the address typed as the user (`src/provider.rs`,
`suggest_endpoint_for_address`):

- `gmail.com` and `googlemail.com` suggest the Google Workspace preset;
  `outlook.com`, `hotmail.com`, `live.com` and `msn.com` the Microsoft 365
  preset; `fastmail.com` and `fastmail.fm` the Fastmail preset; `zoho.com` and
  `zohomail.com` the Zoho Mail preset.
- Any other domain suggests `imap.<domain>`. Custom domains hosted by these
  providers cannot be recognized offline and get this guess too.
- The hint is computed offline from the address alone. Nothing is looked up
  over the network, nothing changes until the operator clicks, and preflight
  still verifies the endpoint.
- For an `imap.<domain>` guess, an optional **Look up settings for
  <domain>** button asks only that domain's own servers, over HTTPS, for
  their published Mozilla-format autoconfig
  (`https://autoconfig.<domain>/mail/config-v1.1.xml`, then
  `https://<domain>/.well-known/autoconfig/mail/config-v1.1.xml`;
  `src/mail_autoconfig.rs`). No third-party database is contacted and the
  address is not sent. Only implicit-TLS or STARTTLS IMAP servers are
  accepted, and the result fills an empty Server field only.
