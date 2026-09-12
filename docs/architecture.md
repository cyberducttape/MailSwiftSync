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

Batch validation uses a bounded standard-thread worker pool (1–16 workers) over an immutable in-memory job list. Each child is marked `running` in the same transaction as the parent run before workers start, allowing forced-restart recovery to surface every unresolved child. Worker output is redacted in the UI and committed to the event ledger in batches per UI cycle rather than issuing one disk transaction per output line.

The Project Cockpit also performs an authenticated dual-endpoint IMAPS readiness probe in imapsync mode over certificate-verified TLS. It consumes the server greeting, authenticates, re-issues `CAPABILITY` after authentication, then requests `NAMESPACE` and `LIST` before presenting the result as planning context. The probe is intentionally limited to dual-IMAPS plans; plain and STARTTLS sources are handled by engine preflight rather than being mislabeled as TLS-verified. Dovecot mode uses the destination's native dry preflight instead, since administrative `doveadm` access does not imply a destination IMAP password. Credentials are held only by the probe thread and are never written to the project ledger or command-line arguments; control characters are rejected before they can enter the IMAP command stream. The probe does not claim that these extensions replace Dovecot's server-side dsync behavior. A failed certificate or authentication check blocks discovery rather than being silently ignored.

## Migration phases

`Discovery → Preflight → Pilot → Seed → Catch-up → Final delta → Verification → Complete`

`Attention` is a terminal operator-review state. A migration should never silently claim completion after an unresolved verification mismatch.

## Verification contract

The evidence schema can hold source/destination folder and message counts, byte counts, unmatched messages, and failed messages. Reports expose an explainable evidence level (`Engine-confirmed exact match`, `Aggregate match`, `Aggregate mismatch`, or `Incomplete evidence`). The compatibility percentage is not a probability: exact engine-confirmed evidence may reach 100%, exact aggregate evidence is capped at 85%, and aggregate mismatches receive 0 rather than a reassuring partial score. The current imapsync text adapter uses `unmatched_messages=1` as a proof-pending sentinel when its authoritative success line is absent; it cannot claim that this is a literal message count. Until a verifier has populated reliable fields, the UI must report evidence as unavailable rather than infer success from process exit status.

The ledger records project lifecycle, redacted run output, and reconciliation evidence. The Dovecot adapter obtains aggregate folder/message/virtual-size status from both the remote `imapc` source and the destination after a live run. The imapsync adapter consumes its final summary. Verification exports include the durable run identifier and timestamps, so an audit artifact can be traced to one execution. Unix migrations run in their own process group; cancellation allows graceful shutdown before escalation, and Linux children receive a parent-death signal as an additional crash-safety measure. The runner records each started process, and startup terminates recorded Unix process groups before marking interrupted runs `abandoned`, preventing a known orphan from being retried concurrently. The process record is best-effort during the short interval before the UI receives the child-start event; Windows Job Object supervision remains a future milestone. UIDVALIDITY-aware checkpoints and explainable message-level mismatch records remain future milestones.

Profile replacement is flushed before rename, and SQLite uses WAL mode, foreign keys, and a bounded busy timeout. Evidence history is transactionally written and must reference an existing run (except for the explicitly supported legacy import path).
