# Compatibility matrix

This is the release-gate template for validating a provider/engine pair. A
row is not considered supported until it has a successful dry pilot, live
pilot, interruption/recovery test, and evidence export using disposable or
fully backed-up mailboxes.

| Source | Destination | Engine | TLS/auth | Folder namespace | Dovecot/provider version | Dry pilot | Live pilot | Recovery | Evidence | Notes |
|---|---|---|---|---|---|---|---|---|---|---|
| TBD | TBD | imapsync | TBD | TBD | TBD | TBD | TBD | TBD | TBD | Add provider-specific limits and known issues. |

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
