# Compatibility matrix

This matrix documents provider support and test coverage. 

**Status levels:**
- **Code path verified** — Logic implemented and tested locally; this is not provider evidence
- **Documented test** — Step-by-step procedure maintained in the repository's qualification documentation
- **Scenario tests** — Automated model/scenario coverage for match detection,
  recovery, and edge cases. These are not provider integration tests; current
  runnable counts and pass/fail results come from the CI test summary artifact.
- **Message verification** — Aggregate verification is wired for all engines; encrypted imapsync runs additionally perform bounded metadata-level reconciliation by default and an explicit bounded body-hash mode when selected. Neither path is provider qualification evidence.

A row is not considered generally supported (1.0 release) until it has a successful live pilot using disposable or fully backed-up mailboxes. Current status shows code-level support and is suitable for technical previews and early adoption.

Release enforcement is defined by the machine-readable
`tests/provider-evidence/policy.json`, not by wording in this document. The
release workflow requires a separate passing evidence record for every policy
phase (dry pilot, live pilot, and recovery test), with the expected imapsync
engine and version. Evidence is generated or supplied as an external release
bundle after checkout so its commit field can name the exact immutable release
SHA; the qualification hand-off is repository/CI material rather than a normal
operator-bundle document. This Markdown matrix remains the human-readable report.

| Source | Destination | Engine | TLS/auth | Folder namespace | Dovecot/provider version | Dry pilot | Live pilot | Recovery | Evidence | Notes |
|---|---|---|---|---|---|---|---|---|---|---|
| Disposable local Dovecot | Disposable local Dovecot | imapsync | STARTTLS with fixture CA bundles and password files | Maildir default; automap disabled so independent metadata verification is available | Pinned Debian Bookworm Dovecot 2.3.x, imapsync 2.314 | Automated product lab | Automated product lab, including incremental pass | Packaged controller recovery lab covers controller crash and engine interruption with durable review state | Durable state, destination Message-ID, customer-proof export and verifier checks | Reproducible CI fixture; evidence for the generic IMAP path, not a hosted-provider compatibility claim. |
| Gmail / Workspace | Microsoft 365 | imapsync | OAuth/XOAUTH2 → OAuth2 | Gmail labels → IMAP folders; [Gmail]/ prefix | Google Workspace and Microsoft 365, imapsync 2.314 | Not run | No live evidence | No live evidence | No live evidence | Primary MSP migration route; hosted-provider pilot not run. See PROVIDER_SETUP.md and OAUTH_SETUP.md. Destination UIDs are local metadata, not cross-mailbox identity. |
| Microsoft 365 | Gmail / Workspace | imapsync | OAuth2 → OAuth/XOAUTH2 | Flat source folders → Gmail labels / IMAP folders | Microsoft 365 and Google Workspace, imapsync 2.314 | Not run | No live evidence | No live evidence | No live evidence | Hosted-provider pilot not run. Microsoft 365 Basic Authentication/app passwords are unsupported; OAuth is mandatory. Destination UIDs are local metadata, not cross-mailbox identity. |
| Fastmail | Microsoft 365 | imapsync | App password → OAuth2 | `/` separators and SPECIAL-USE → Microsoft folders | Fastmail and Microsoft 365, imapsync 2.314 | Not run | No live evidence | No live evidence | No live evidence | Hosted-provider pilot not run. See PROVIDER_SETUP.md and OAUTH_SETUP.md. Destination UIDs are local metadata, not cross-mailbox identity. |

## Verified engine fixture

The release integration job builds the distributed runtime image, which uses
Debian Bookworm's Dovecot package and pinned imapsync `2.314`, and runs the
disposable transfer fixture inside that image. The fixture prints the actual
engine versions and fails rather than silently skipping when the engines are
unavailable. It drives the packaged MailSwiftSync binary through its profile,
secret-file, preflight, live, incremental, durable-evidence, customer-proof,
and verification paths. A separate recovery fixture covers controller crash,
engine interruption, and restart ownership. This remains evidence for a
native Dovecot path that uses mdbox for the target, including both the initial
`backup` and a `sync -1` preservation pass against the populated destination;
it is not Maildir destination qualification. The imapsync fixture remains
evidence for the generic IMAP path, not a hosted-provider compatibility claim.
Storage-fault and provider-specific tests remain separate gates.

Minimum test cases for every row:

- DNS, certificate validation, authentication, capability, namespace, and
  mailbox-list checks.
- Empty, small, and large mailboxes; special-use folders; Unicode folder
  names; renamed special-use folders; sparse/high UID mailboxes after expunge;
  literal-framed and non-UTF-8 messages; duplicate and missing Message-IDs;
  and message-size mismatch behavior.
- Cancellation, timeout, process crash, application restart, retry, and final
  delta behavior.
- Aggregate evidence limits and the exact report produced for mismatches.

Until this matrix contains representative tested rows, the product should be
described as a technical preview rather than a generally supported migration
service.
