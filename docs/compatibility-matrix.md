# Compatibility matrix

This is the release-gate template for validating a provider/engine pair. A
row is not considered supported until it has a successful dry pilot, live
pilot, interruption/recovery test, and evidence export using disposable or
fully backed-up mailboxes.

| Source | Destination | Engine | TLS/auth | Folder namespace | Dovecot/provider version | Dry pilot | Live pilot | Recovery | Evidence | Notes |
|---|---|---|---|---|---|---|---|---|---|---|
| Disposable local Dovecot | Disposable local Dovecot | imapsync | IMAP cleartext fixture credentials | Maildir default / automap | Dovecot 2.3.x (Debian Bookworm package), imapsync 2.314 | Pending rerun on packaged versions | Pending rerun on packaged versions | Not tested | Message-ID assertions and invalid-auth rejection | Engine-only smoke evidence; not a supported provider row and does not satisfy the release gate. |

## Verified engine fixture

The release integration job builds the distributed runtime image, which uses
Debian Bookworm's Dovecot package and pinned imapsync `2.314`, and runs the
disposable transfer fixture inside that image. The fixture prints the actual
engine versions and fails rather than silently skipping when the engines are
unavailable. This is evidence for the external IMAP engine path only; it does
not mark the row above as generally supported and does not replace controller
crash/restart, storage-fault, or provider-specific tests.

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
