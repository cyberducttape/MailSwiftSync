# Compatibility matrix

This is the release-gate template for validating a provider/engine pair. A
row is not considered supported until it has a successful dry pilot, live
pilot, interruption/recovery test, and evidence export using disposable or
fully backed-up mailboxes.

| Source | Destination | Engine | TLS/auth | Folder namespace | Dovecot/provider version | Dry pilot | Live pilot | Recovery | Evidence | Notes |
|---|---|---|---|---|---|---|---|---|---|---|
| Disposable local Dovecot | Disposable local Dovecot | imapsync | STARTTLS with fixture CA bundles and password files | Maildir default / automap | Pinned Debian Bookworm Dovecot 2.3.x, imapsync 2.314 | Automated product lab | Automated product lab, including incremental pass | Controller crash/recovery lab is separate; engine interruption remains pending | Durable state, destination Message-ID, customer-proof export and verifier checks | Reproducible CI fixture; evidence for the generic IMAP path, not a hosted-provider compatibility claim. |

## Verified engine fixture

The release integration job builds the distributed runtime image, which uses
Debian Bookworm's Dovecot package and pinned imapsync `2.314`, and runs the
disposable transfer fixture inside that image. The fixture prints the actual
engine versions and fails rather than silently skipping when the engines are
unavailable. It drives the packaged MailSwiftSync binary through its profile,
secret-file, preflight, live, incremental, durable-evidence, customer-proof,
and verification paths. A separate recovery fixture covers controller crash
and restart ownership. This remains evidence for a reproducible generic-IMAP
lab, not a hosted-provider support claim, and engine interruption,
storage-fault, and provider-specific tests remain separate gates.

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
