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
| `migration.cutover_verification_started` | Begin final independent verification; keep source delivery paused | Verification in progress |
| `migration.cutover_ready` | Final Verification-stage evidence is complete; request external cutover approval/action | Pending customer approval |
| `migration.completed` | Update totals and link evidence | Ready for review |
| `migration.proof_ready` | Attach or link signed proof artifact | Customer signoff pending |
| `migration.status_snapshot` | Reconcile current aggregate projection | Set current status |

`migration.cutover_ready` is emitted only after every mailbox's final
Verification-stage evidence is durable, or its difference has an explicit
acceptance. Its detail includes separate `clean_verified` and
`accepted_exceptions` counts. Entering Final Delta emits
`migration.final_delta_started`; entering the subsequent Verification phase
emits `migration.cutover_verification_started`, not a readiness signal. The
source-quiescence attestation remains an operator claim, so receivers should
keep the source paused until external cutover approval.

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
