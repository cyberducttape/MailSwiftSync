# Architecture: the migration control plane

Sourcecraft is moving from a desktop command launcher toward a durable migration control plane.

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

The `core` module owns the durable project model. It persists projects, phases, mailbox jobs, audit events, and verification evidence. It intentionally never persists passwords or mailbox content.

## Migration phases

`Discovery → Preflight → Pilot → Seed → Catch-up → Final delta → Verification → Complete`

`Attention` is a terminal operator-review state. A migration should never silently claim completion after an unresolved verification mismatch.

## Verification contract

Every mailbox receives evidence containing source/destination message counts, byte counts, unmatched messages, and failed messages. A 100% confidence result requires exact count and byte agreement with no unmatched or failed messages.

This is the start of an evidence ledger; future engine work will add folder-level identities, UIDVALIDITY-aware checkpoints, and explainable mismatch records.
