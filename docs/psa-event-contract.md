# PSA integration event contract

MailSwiftSync does not currently own PSA tickets or call ConnectWise,
Autotask, Halo, Syncro, or ServiceNow APIs. The stable integration boundary is
the signed, idempotent HTTPS webhook and its durable outbox. A receiver or
automation platform owns ticket creation, customer mapping, assignment, and
ticket updates.

## Envelope

Every delivery uses JSON and these headers:

```text
X-MailSwiftSync-Event-Id: <stable event id>
X-MailSwiftSync-Event-Type: <event type>
X-MailSwiftSync-Timestamp: <Unix timestamp in seconds>
Idempotency-Key: <same stable event id>
```

When HMAC signing is configured, the request also includes
`X-MailSwiftSync-Signature-Version: 2` and
`X-MailSwiftSync-Signature: hmac-sha256-v2=<lowercase hex HMAC>`. Production
organization policy requires signing; unsigned delivery is limited to explicit
compatibility/lab policy.

The v2 signature is HMAC-SHA256 over a canonical byte sequence. Begin with
`mailswiftsync:webhook-signature:v2\0`; then, in order, append each of the
following fields as an unsigned 64-bit big-endian byte length followed by the
field's exact UTF-8/body bytes: event ID, event type, idempotency key, timestamp,
and exact HTTP body bytes. The signature therefore authenticates the event
identity headers as well as the payload. The signature is required by the
production organization policy. Receivers must verify the signature before
parsing the body, reject timestamps outside their configured replay window
(five minutes is a reasonable starting policy), and durably deduplicate event
IDs before performing ticket side effects. Keep deduplication records at least
as long as the sender can retry or an operator can replay the outbox. Delivery
order is not a contract; receivers use the timestamp and current status to
handle retries and late delivery. v2 is not wire-compatible with the earlier
body-only `sha256=` signature; update receiver verification before enabling
this sender version. When the JSON body contains `event_id` or `event_type`,
receivers should also require those values to equal the signed headers.

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
| `migration.cutover_ready` | Request external approval/action | Pending customer approval |
| `migration.completed` | Update totals and link evidence | Ready for review |
| `migration.proof_ready` | Attach or link signed proof artifact | Customer signoff pending |
| `migration.status_snapshot` | Reconcile current aggregate projection | Set current status |

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
