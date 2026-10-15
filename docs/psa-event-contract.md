# PSA integration event contract

MailSwiftSync does not currently own PSA tickets or call ConnectWise,
Autotask, Halo, Syncro, or ServiceNow APIs. The stable integration boundary is
the signed, idempotent HTTPS webhook and its durable outbox. A receiver or
automation platform owns ticket creation, customer mapping, assignment, and
ticket updates.

## Envelope

Every delivery uses JSON and the event ID, event type, and idempotency headers.
When signing is configured, version 2 adds the signature headers shown here:

```text
X-MailSwiftSync-Event-Id: <stable event id>
X-MailSwiftSync-Event-Type: <event type>
X-MailSwiftSync-Signature-Version: 2
X-MailSwiftSync-Signature-Timestamp: <Unix seconds for this delivery attempt>
Idempotency-Key: <same stable event id>
X-MailSwiftSync-Signature: v2=sha256=<HMAC-SHA256>
```

The signature is required by the production organization policy. Version 2
HMAC-SHA256 covers a domain-separation string followed by the event ID, event
type, idempotency key, signature timestamp, and exact body bytes. Each field is
encoded as an unsigned 64-bit big-endian byte length followed by that many raw
bytes; text fields are UTF-8. The domain-separation bytes are
`mailswiftsync:webhook-signature:v2\0`. The signature is lowercase hex.

Receivers must verify the signature and require the idempotency key to equal
the event ID before parsing or acting on the payload. Check the signature
timestamp against a bounded clock-skew/replay window (five minutes is the
recommended default), then durably deduplicate the event ID before side effects.
Store the event ID with the ticket link. The timestamp is regenerated for each
delivery attempt so a delayed outbox retry can be authenticated as a fresh
delivery; it is distinct from the event's `occurred_at`. Delivery order is not
a contract; receivers use event time and current status to handle retries and
late delivery. The former body-only `sha256=` signature is no longer emitted;
receivers must upgrade to version 2 before enabling required signing.

The JSON envelope has the form:

```json
{
  "format": "mailswiftsync-webhook-event",
  "event_id": "ledger-event-123",
  "event_type": "mailbox.failed",
  "project_id": "opaque-project-id",
  "data": {},
  "occurred_at": "2026-10-08T12:34:56Z"
}
```

The exact payload fields are credential-free and may be reduced by the
configured status projection. Receivers must not assume customer names,
endpoints, or mailbox content is present.

## Event mapping

| Event | Receiver action | Ticket state suggestion |
|---|---|---|
| `migration.started` | Create or link the migration ticket | In progress |
| `mailbox.preflight_failed` | Add affected mailbox and remediation | Waiting on technician |
| `mailbox.failed` | Add classified failure and affected mailbox | Waiting on customer/technician |
| `migration.final_delta_started` | Begin the approved final-delta window; keep source delivery paused | In progress |
| `migration.cutover_verification_started` | Final Delta is complete and source quiescence has been reconfirmed; begin final independent verification | Verification in progress |
| `migration.cutover_ready` | Final Verification-stage evidence is complete; request external cutover approval/action | Pending customer approval |
| `migration.completed` | Update totals and link evidence | Ready for review |
| `migration.proof_ready` | Attach or link signed proof artifact | Customer signoff pending |
| `migration.status_snapshot` | Reconcile current aggregate projection | Set current status |

`migration.cutover_ready` is emitted only after every mailbox's final
Verification-stage evidence is durable, or its difference has an explicit
acceptance. Its detail includes separate `clean_verified` and
`accepted_exceptions` counts, the source-quiescence attestation expiry, and
separate `independent_cursor_revalidated` and `engine_or_aggregate_only`
mailbox counts. Metadata/content evidence is ineligible for readiness unless
the referenced run completed in the Verification phase and its transfer-pass
record matches the same verification method and outcome. That final-pass record
must contain complete source and destination folder observations and post-scan
cursor revalidation in the expected order. Engine
or aggregate evidence remains identified separately; that count is not an
independent mailbox-content or cursor-integrity claim. Missing, stale, or
phase-mismatched evidence suppresses the readiness event.
The event is published only while that attestation is fresh (15 minutes); if
verification finishes after expiry, refreshing the attestation during
Verification can publish readiness without rescanning. Receivers must treat
readiness as time-bounded and keep the source paused until external cutover
approval. If a previously published readiness window expires, a later fresh
attestation emits a new `migration.cutover_ready` event with a new event ID;
receivers should use the latest event and its expiry rather than treating an
older event as current. Cursor revalidation detects changes represented by
the observed IMAP folder inventory and UIDVALIDITY/UIDNEXT/EXISTS and, when
available, CONDSTORE HIGHESTMODSEQ. It narrows the observation window but is
not an atomic cross-server snapshot and cannot prove uninterrupted source
quiescence. Entering Final Delta emits
`migration.final_delta_started`. Transitioning from Final Delta to the
Verification stage clears the prior attestation; no verification worker can
be admitted until the operator records a new, fresh attestation. Only then is
`migration.cutover_verification_started` emitted. This is not a readiness
signal. The source-quiescence attestation remains an operator claim, not
machine-verified source stability.

Ticket correlation is intentionally receiver-owned. A future control plane may
add an opaque `external_ticket_ref` and project-scoped mapping, but must make
that mapping tenant-scoped, auditable, and idempotent. It must not put PSA
credentials into a worker ledger or webhook payload.

## Reliability and security obligations

The sender's outbox retries with bounded backoff, leases deliveries, and moves
permanent failures to a dead-letter state. Receivers must tolerate duplicate
events, retry non-2xx responses safely, and expose dead-letter/acknowledgement
health to operators. The current `notify-webhook` workflow is an operator- or
service-manager-managed integration, not a complete ticket lifecycle.

The webhook and OAuth refresh clients connect directly to addresses resolved
and checked by MailSwiftSync. They explicitly ignore ambient HTTP proxy
environment variables; proxy use is not currently supported by these
policy-controlled requests. Deployments requiring an outbound proxy need a
separately designed proxy policy that preserves destination authorization.
