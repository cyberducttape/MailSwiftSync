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

The implementation keeps the controller boundary explicit even while the
desktop shell continues to evolve. `migration_plan` owns profile validation,
engine argument construction, credential-file preparation, and immutable run
plan snapshots. `controller` owns worker admission, process/run orchestration,
failure classification, retry policy, and recovery-facing state transitions.
`output` owns the shared line/byte-bounded journal and process-tail buffer.
`ui` contains presentation helpers and semantic status/theme primitives; it
does not define migration policy. Headless and GUI entry points should call
these shared controller/plan boundaries rather than reimplementing them.

Batch validation and migration use a bounded worker pool (1–16 workers) over an immutable in-memory job list. Admission creates a parent wave run plus one mailbox-specific child run and snapshot per row in one transaction. Mailbox rows remain `queued` until a worker claims them; process identity and terminal results are then attributed to the child run. Worker output is redacted in the UI and committed to the event ledger in batches per UI cycle rather than issuing one disk transaction per output line. Verbose `run_output` events retain a bounded per-project tail; lifecycle, run, phase, and evidence events are not pruned.

Batch destination collision checks use a canonical endpoint/port/mailbox identity
when a restored row contains secret-free profile configuration. This permits the
same mailbox name on separate destination hosts while still rejecting concurrent
writes to one actual endpoint. Legacy rows without that identity remain
conservative and are checked by normalized mailbox name.

The Project Cockpit performs an authenticated endpoint readiness probe in imapsync mode over certificate-verified TLS. Implicit IMAPS begins inside TLS; STARTTLS first verifies the server's advertised upgrade capability, negotiates TLS, then follows the same greeting/authentication/`CAPABILITY`/`NAMESPACE`/`LIST` sequence. The probe requires at least one untagged `* LIST` record and reports the discovered folder count and observed SPECIAL-USE annotations; a tagged `LIST` completion alone is not accepted as an inventory. Operators may add a PEM enterprise CA bundle and/or a SHA-256 leaf-certificate pin; these settings are included in the plan identity and are rechecked before live admission. imapsync receives the additional CA file through its typed TLS arguments, while certificate pinning remains an application-level fail-closed check. Live admission uses this transport-specific probe for every encrypted endpoint; plain sources remain engine-preflight-only after explicit acknowledgement. Dovecot dry preflight checks the destination-side user and mailbox list in addition to the remote `imapc` source listing; quota capacity remains an operator responsibility where it cannot be queried reliably. Credentials are held only by the probe thread and are never written to the project ledger or command-line arguments; control characters are rejected before they can enter the IMAP command stream. The probe does not claim that these extensions replace Dovecot's server-side dsync behavior. A failed certificate, authentication, or folder-inventory check blocks discovery rather than being silently ignored.

## Migration phases

`Discovery → Preflight → Pilot → Seed → Catch-up → Final delta → Verification → Complete`

`Attention` is a terminal operator-review state. A migration should never silently claim completion after an unresolved verification mismatch.

## Verification contract

The evidence schema can hold source/destination folder and message counts, byte counts, unmatched messages, and failed messages. Reports expose an explainable evidence level (`Engine-confirmed exact match`, `Aggregate match`, `Aggregate mismatch`, or `Incomplete evidence`). The compatibility percentage is not a probability: exact engine-confirmed evidence may reach 100%, exact aggregate evidence is capped at 85%, and aggregate mismatches receive 0 rather than a reassuring partial score. The current imapsync text adapter uses `unmatched_messages=1` as a proof-pending sentinel when its engine-confirmed success line is absent; it cannot claim that this is a literal message count. Until a verifier has populated reliable fields, the UI must report evidence as unavailable rather than infer success from process exit status.

The ledger records project lifecycle, redacted run output, and reconciliation evidence. The Dovecot adapter obtains aggregate folder/message/virtual-size status from both the remote `imapc` source and the destination after a live run. The imapsync adapter consumes its final summary. Verification exports include the durable run identifier and timestamps, so an audit artifact can be traced to one execution. Unix migrations run in their own process group; cancellation allows graceful shutdown before escalation, and Linux children receive a parent-death signal as an additional crash-safety measure. The runner records each started process together with platform process start-time, process-group, and session identity. Startup terminates a recorded Unix group only when those identity values still match; otherwise it refuses to signal the PID and leaves the job for operator review. Windows children are attached to a kill-on-close Job Object; macOS validates identity through `proc_pidinfo` and retains the conservative no-signal fallback when that native query fails. The process record is best-effort during the short interval before the UI receives the child-start event. The current Dovecot checkpoint is an engine resume token, not UIDVALIDITY-aware verification evidence; explainable message-level mismatch records remain future milestones.

Report generation is isolated under `src/reports/`. Customer proof, operator
health, and signing/verification builders receive explicit durable inputs and
paths; they do not construct `App`, restore a GUI profile, perform startup
recovery, or own egui state. The GUI and headless CLI therefore share the same
artifact code while keeping controller and presentation concerns replaceable.

Each run also stores the project phase observed at admission. This is immutable
provenance for reports and incident review, not a claim that the current engine
has distinct implementation semantics for every lifecycle phase.

Dovecot live syncs use the `mailbox_jobs.checkpoint` column as a conservative
stateful-sync resume point. Each live `sync`/`backup` passes the last committed
state string with `-s` (or an empty state for the initial pass). The runner
captures the state string emitted on stdout and commits it in the same terminal
transaction as the child result and mailbox state. Dry preflight and failed or
cancelled runs do not replace the last committed checkpoint. The checkpoint is
an engine resume optimization, not UIDVALIDITY-aware message evidence; that
and explainable message-level mismatch records remain future milestones.

Profile replacement is flushed before rename, and SQLite uses WAL mode, foreign keys, a bounded busy timeout, and an explicit schema-version guard. Databases stamped by a newer application are refused rather than partially opened or downgraded. The persistent database directory is owner-only on Unix so WAL/SHM sidecars remain contained even when SQLite creates them after startup. Evidence history is transactionally written and must reference an existing run (except for the explicitly supported legacy import path).

Terminal persistence is the control-plane commit boundary: an external engine
result is not allowed to advance the project phase until the run and mailbox
terminal state have been durably committed. If that commit fails, the
controller reports a durability review and leaves the durable run available for
startup recovery; it does not emit a terminal completion event or manufacture
an Attention/Complete transition in memory.
