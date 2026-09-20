# Compatibility matrix

This matrix documents provider support and test coverage. 

**Status levels:**
- **Code path verified** — Logic implemented and tested locally; this is not provider evidence
- **Documented test** — Step-by-step procedure available in provider-testing-guide.md
- **Scenario tests** — Automated model/scenario coverage for match detection,
  recovery, and edge cases. These are not provider integration tests; current
  runnable counts and pass/fail results come from the CI test summary artifact.
- **Message verification** — Aggregate verification is wired; message-level reconciliation is a prototype and is not part of live migrations

A row is not considered generally supported (1.0 release) until it has a successful live pilot using disposable or fully backed-up mailboxes. Current status shows code-level support and is suitable for technical previews and early adoption.

Release enforcement is defined by the machine-readable
`tests/provider-evidence/policy.json`, not by wording in this document. The
release workflow requires a separate passing evidence record for every policy
phase (dry pilot, live pilot, and recovery test), with the expected imapsync
engine and version. This Markdown matrix remains the human-readable report.

| Source | Destination | Engine | TLS/auth | Folder namespace | Dovecot/provider version | Dry pilot | Live pilot | Recovery | Evidence | Notes |
|---|---|---|---|---|---|---|---|---|---|---|
| Disposable local Dovecot | Disposable local Dovecot | imapsync | STARTTLS with fixture CA bundles and password files | Maildir default / automap | Pinned Debian Bookworm Dovecot 2.3.x, imapsync 2.314 | Automated product lab | Automated product lab, including incremental pass | Packaged controller recovery lab covers controller crash and engine interruption with durable review state | Durable state, destination Message-ID, customer-proof export and verifier checks | Reproducible CI fixture; evidence for the generic IMAP path, not a hosted-provider compatibility claim. |
| Generic IMAP (Gmail/Workspace) | Generic IMAP (Gmail/Workspace) | imapsync | IMAPS with app passwords / XOAUTH2 | Gmail labels → IMAP folders; [Gmail]/ prefix | Google Workspace 2024+, imapsync 2.314 | Not run | No live evidence | No live evidence | No live evidence | Code/scenario coverage only; hosted-provider pilot not run. See PROVIDER_SETUP.md and OAUTH_SETUP.md. Destination UIDs are local metadata, not cross-mailbox identity. |
| Generic IMAP (Microsoft 365) | Generic IMAP (Microsoft 365) | imapsync | IMAPS with OAuth2 only (Basic Auth disabled Oct 1, 2022) | Flat; special folders prefixed (Deleted Items, Sent Items, etc.) | Microsoft 365 2024+, imapsync 2.314 | Not run | No live evidence | No live evidence | No live evidence | Code/scenario coverage only; hosted-provider pilot not run. See PROVIDER_SETUP.md and OAUTH_SETUP.md. Basic Authentication (app passwords) no longer works. OAuth is mandatory. Destination UIDs are local metadata, not cross-mailbox identity. |
| Generic IMAP (Fastmail) | Generic IMAP (Fastmail) | imapsync | IMAPS with app passwords | JMAP-first IMAP; `/` separators; full SPECIAL-USE | Fastmail 2024+, imapsync 2.314 | Not run | No live evidence | No live evidence | No live evidence | Code/scenario coverage only; hosted-provider pilot not run. See PROVIDER_SETUP.md. Destination UIDs are local metadata, not cross-mailbox identity. |

## Verified engine fixture

The release integration job builds the distributed runtime image, which uses
Debian Bookworm's Dovecot package and pinned imapsync `2.314`, and runs the
disposable transfer fixture inside that image. The fixture prints the actual
engine versions and fails rather than silently skipping when the engines are
unavailable. It drives the packaged MailSwiftSync binary through its profile,
secret-file, preflight, live, incremental, durable-evidence, customer-proof,
and verification paths. A separate recovery fixture covers controller crash,
engine interruption, and restart ownership. This remains evidence for a
reproducible generic-IMAP lab, not a hosted-provider support claim, and
storage-fault and provider-specific tests remain separate gates.

Minimum test cases for every row:

- DNS, certificate validation, authentication, capability, namespace, and
  mailbox-list checks.
- Empty, small, and large mailboxes; special-use folders; Unicode folder
  names; and message-size mismatch behavior.
- Cancellation, timeout, process crash, application restart, retry, and final
  delta behavior.
- Aggregate evidence limits and the exact report produced for mismatches.

Until this matrix contains representative tested rows, the product should be
described as a technical preview rather than a generally supported migration
service.
