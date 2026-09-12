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

The Project Cockpit also performs an authenticated IMAPS readiness probe over certificate-verified TLS. It consumes the server greeting, authenticates, re-issues `CAPABILITY` after authentication, then requests `NAMESPACE` and `LIST` before presenting the result as planning context. The probe is intentionally limited to IMAPS; plain and STARTTLS sources are handled by engine preflight rather than being mislabeled as TLS-verified. Credentials are held only by the probe thread and are never written to the project ledger or command-line arguments; control characters are rejected before they can enter the IMAP command stream. The probe does not claim that these extensions replace Dovecot's server-side dsync behavior. A failed certificate or authentication check blocks discovery rather than being silently ignored.

## Migration phases

`Discovery → Preflight → Pilot → Seed → Catch-up → Final delta → Verification → Complete`

`Attention` is a terminal operator-review state. A migration should never silently claim completion after an unresolved verification mismatch.

## Verification contract

The evidence schema can hold source/destination folder and message counts, byte counts, unmatched messages, and failed messages. A 100% confidence result requires exact count, folder, and byte agreement with no unmatched or failed messages. Until a verifier has populated those fields, the UI must report evidence as unavailable rather than infer success from process exit status.

The ledger records project lifecycle, redacted run output, and reconciliation evidence. The Dovecot adapter obtains aggregate folder/message/virtual-size status from both the remote `imapc` source and the destination after a live run. The imapsync adapter consumes its final summary. Verification exports include the durable run identifier and timestamps, so an audit artifact can be traced to one execution. On restart, running jobs become `attention`, their run records become `abandoned`, and a recovery event is written so the ledger cannot imply that an interrupted process completed. UIDVALIDITY-aware checkpoints and explainable message-level mismatch records remain future milestones.

Profile replacement is flushed before rename, and SQLite uses WAL mode, foreign keys, and a bounded busy timeout. Evidence history is transactionally written and must reference an existing run (except for the explicitly supported legacy import path).
