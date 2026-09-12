# Architecture: the migration control plane

MailSwiftSync is a desktop migration control plane. It delegates transfer semantics to Dovecot's native dsync engine when selected and uses imapsync as the arbitrary-IMAP fallback; the application owns planning, safety gates, durable run state, and operator evidence.

## Core layers

```text
Desktop UI / future API
          │
   Project orchestrator
          │
 ┌────────┼─────────┐
Preflight Scheduler Verify
          │
      IMAP workers
          │
 Durable SQLite state + evidence ledger
```

The `core` module owns the durable project model. It persists projects, phases, mailbox jobs, audit events, per-run evidence history, and verification evidence. It intentionally never persists passwords or mailbox content.

The Project Cockpit also performs unauthenticated IMAPS capability discovery over certificate-verified TLS. It asks each endpoint for `CAPABILITY` and presents the result as planning context; it does not claim that these extensions replace Dovecot's server-side dsync behavior. A failed certificate check blocks discovery rather than being silently ignored.

## Migration phases

`Discovery → Preflight → Pilot → Seed → Catch-up → Final delta → Verification → Complete`

`Attention` is a terminal operator-review state. A migration should never silently claim completion after an unresolved verification mismatch.

## Verification contract

The evidence schema can hold source/destination folder and message counts, byte counts, unmatched messages, and failed messages. A 100% confidence result requires exact count, folder, and byte agreement with no unmatched or failed messages. Until a verifier has populated those fields, the UI must report evidence as unavailable rather than infer success from process exit status.

The ledger records project lifecycle, redacted run output, and reconciliation evidence. The Dovecot adapter obtains aggregate folder/message/virtual-size status from both the remote `imapc` source and the destination after a live run. The imapsync adapter consumes its final summary. UIDVALIDITY-aware checkpoints and explainable message-level mismatch records remain future milestones.
