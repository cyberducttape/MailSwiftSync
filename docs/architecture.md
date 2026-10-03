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

The `core` module owns the durable project model. It persists projects, phases, mailbox jobs, audit events, per-run evidence history, and verification evidence. It intentionally never persists passwords or mailbox content. The current SQLite schema (v17) establishes destination-identity policy v2, binds the one-row-per-mailbox current evidence projection to its run, records per-attempt transfer-pass provenance, stores narrow per-mailbox queue facts, and stores shared batch policies separately from mailbox identity/credential deltas. Normal opens trust the versioned identity invariant instead of reparsing every mailbox's serialized config; GUI/report reads use the current projection instead of repeatedly finding the latest historical record. Older ledgers rebuild these invariants transactionally during migration. Deep integrity checks remain separate from the ordinary startup path.

The implementation keeps the controller boundary explicit even while the
desktop shell continues to evolve. `migration_plan` owns profile validation,
engine argument construction, credential-file preparation, and immutable run
plan snapshots. `controller` owns worker admission, process/run orchestration,
failure classification, retry policy, and recovery-facing state transitions.
`output` owns the shared line/byte-bounded journal and process-tail buffer.
`ui` contains presentation helpers and semantic status/theme primitives; it
does not define migration policy. Headless and GUI entry points should call
these shared controller/plan boundaries rather than reimplementing them.

Failure classification recognizes RFC 5530 IMAP response codes from any server (`[UNAVAILABLE]`, `[INUSE]`, `[AUTHENTICATIONFAILED]`, `[EXPIRED]`, `[OVERQUOTA]`, `[NOPERM]`, `[LIMIT]`, and others) and a table of documented provider responses: Exchange Online throttling (`Request is throttled. Suggested Backoff Time: N milliseconds`, whose delay is honored as a floor, capped at 30 minutes) and `User is authenticated but not connected` (IMAP disabled or missing `IMAP.AccessAsUser.All` consent; not retried), Gmail's simultaneous-connection and bandwidth limits and its web-login and app-password prompts, and Dovecot's per-user/IP connection limit. A recognized signal is recorded in the classified failure detail as `[signal=source:name]` with a next step when a person must act. These entries come from provider documentation and support articles; they are not live-qualified and do not count as provider qualification evidence.

The controller converts the untrusted diagnostic edge into a typed
`MigrationError` before applying retry, cooldown, lifecycle, or durable
attention policy. Redacted provider/engine text remains attached as bounded
detail for operators, but later policy code consumes the typed class and
server-requested backoff rather than reparsing presentation prose. The
remaining migration APIs are being moved toward this boundary incrementally;
legacy `Result<_, String>` values are still accepted at subsystem edges.

At the execution boundary, each source and destination endpoint is assigned a
conservative provider identity (`gmail`, `microsoft365`, `dovecot`, or
`generic`). That identity is carried alongside the endpoint, tenant,
credential, and mailbox rate-domain scopes. Provider-aware classification and
backoff therefore affect the correct side's adaptive limiter; unknown hosts
remain generic and receive no assumed provider quota.

The batch queue is durable state: an import is written to the ledger as a new batch project in one transaction (on a worker thread with its own connection), and the project's `mailbox_jobs` rows are the queue. Each row also has a narrow `mailbox_queue_facts` row keyed by the mailbox rowid with its label, hosts, a case-folded search key, its destination-mutation policy, and a trigger-maintained copy of its state, so filtering and counting never decode or read past the per-row plans. Shared batch policies live once in `batch_plans`; mailbox rows retain only identity and credential deltas, while legacy inline configurations remain readable. The Mailboxes view keeps only the row IDs matching the current search and state filter (its virtual-row index) and a bounded cache of rows that have been on screen; queue health and selection counts come from one scan of the facts table. Plaintext-import session passwords and preflight credential fingerprints stay in memory only, keyed by job ID. The one presentation-only state is a row waiting out a retry; queued, running, and terminal states are durable. Admission scans the queue from `mailbox_jobs` (never the derived state copy), then decodes, validates, and digests each selected plan one at a time without retaining plans or credentials; the scheduler reads admitted jobs back from the ledger in pages of 64 through its own read-only connection and loads each dry run's credentials as it reads the job. Batch validation and migration use a bounded OS-thread/process worker pool (1–256 workers; default 2). This is intentionally simple and safe for the current desktop control plane; future fleet-scale operation should move cheap probes and reconciliation toward asynchronous or pooled I/O rather than multiplying engine processes casually. Historical mailbox and verification pages use rowid keyset cursors, avoiding progressively expensive `OFFSET` scans. UI “select all visible” stores an all-matching marker plus excluded IDs instead of copying every selected mailbox ID, and action review hashes selected rows incrementally. Admission still holds the selected job IDs and per-child plan snapshots for the durable run transaction, so very large executions are bounded by the persisted-plan budget rather than constant-memory. Admission creates a parent wave run plus one mailbox-specific child run and snapshot per row in one transaction. Mailbox rows remain `queued` until a worker claims them; process identity and terminal results are then attributed to the child run. Worker output is redacted and retained only in bounded process-local UI memory; raw `run_output` transcripts are not persisted, preventing subjects, folder names, and message metadata from becoming durable ledger data. Structured lifecycle, run, phase, and evidence events remain durable.

Batch destination collision checks use a canonical endpoint/port/mailbox identity
when a restored row contains secret-free profile configuration. Gmail and
Exchange Online's recognized IMAP endpoints apply case-insensitive account
identity; generic endpoints preserve exact account spelling. A live batch with
case-folded collisions on an unknown endpoint is visibly warned and requires
operator acknowledgement and concurrency 1 before admission. Durable run
admission also uses a conservative case-folded destination lock across active
projects, so two differently cased names cannot write simultaneously. This is
an ambiguity warning, not provider qualification: operators must verify those
accounts, and provider-specific case policies beyond the recognized endpoints
remain unqualified.
Legacy rows without endpoint identity remain conservative and are checked by
normalized mailbox name.

The Project Cockpit performs an authenticated endpoint readiness probe in imapsync mode over certificate-verified TLS. Implicit IMAPS begins inside TLS; STARTTLS first verifies the server's advertised upgrade capability, negotiates TLS, then follows the same greeting/authentication/`CAPABILITY`/`NAMESPACE`/`LIST` sequence. The probe requires at least one untagged `* LIST` record and reports the discovered folder count and observed SPECIAL-USE annotations; a tagged `LIST` completion alone is not accepted as an inventory. Operators may add a PEM enterprise CA bundle and/or a SHA-256 leaf-certificate pin; these settings are included in the plan identity and are rechecked before live admission. imapsync receives the additional CA file through its typed TLS arguments, while certificate pinning remains an application-level fail-closed check. Live admission uses this transport-specific probe for every encrypted imapsync endpoint; plain sources remain engine-preflight-only after explicit acknowledgement. Dovecot dry preflight checks the destination-side userdb with `doveadm user` and lists source mailboxes through remote `imapc`; it deliberately does not enumerate the target mailbox store before its first sync because Dovecot warns that early target access can cause GUID/UIDVALIDITY conflicts or synchronization failure. A source CA bundle can configure that TLS connection, but Dovecot certificate pins are rejected because the native engine does not enforce application-level leaf pins. Quota capacity remains an operator responsibility where it cannot be queried reliably. Credentials are held only by the probe thread and are never written to the project ledger or command-line arguments; control characters are rejected before they can enter the IMAP command stream. The probe does not claim that these extensions replace Dovecot's server-side dsync behavior. A failed certificate, authentication, or folder-inventory check blocks discovery rather than being silently ignored.

System name resolution for authenticated IMAP probes runs in a short-lived internal helper process because the platform `getaddrinfo`/`ToSocketAddrs` call cannot be cancelled safely in-process. The parent caps concurrent resolver helpers at 16, applies the existing per-probe DNS deadline, and kills/reaps the helper on timeout or cancellation; a stuck system resolver therefore cannot permanently occupy a reusable worker thread. The helper starts with a resolver-specific allowlist (including `LOCALDOMAIN` and `RES_OPTIONS` where configured, but excluding unrelated credentials) and returns at most 64 socket addresses. Connection attempts then use staggered IPv4/IPv6 racing under a separate overall deadline. This bounds MailSwiftSync-owned resolver resources, but cannot guarantee that every platform resolver implementation will return within the deadline absent process termination support from the OS.

## Migration phases

`Discovery → Preflight → Pilot → Seed → Catch-up → Final delta → Verification → Complete`

`Attention` is a terminal operator-review state. A migration should never silently claim completion after an unresolved verification mismatch.

## Verification contract

The evidence schema can hold source/destination folder and message counts, byte counts, an optional unresolved-message count, and failed messages. Operator-facing verification uses three evidence levels: Level 1 compares aggregate folder/message/byte totals and engine counters but does not compare individual messages; Level 2 reconciles per-message identity and metadata such as Message-ID, INTERNALDATE, and RFC822.SIZE without comparing bodies; Level 3 performs opt-in, bounded RFC822 body-content fingerprints. Level 3 is not a complete byte-for-byte mailbox proof and remains provider-unqualified. Incomplete evidence has no level, and process completion alone is never verification. Encrypted imapsync live runs perform bounded Level 2 reconciliation by default using independently fetched metadata; an explicit forensic profile can perform Level 3 SHA-256 body-content comparison within per-message and total-byte bounds. Native Dovecot runs currently provide Level 1 aggregate evidence. UID enumeration uses bounded `UID SEARCH` windows derived from `UIDNEXT`, so a mailbox-wide `UID SEARCH ALL` response is never materialized. Live metadata is staged in a private SQLite database and reconciled in bounded batches, avoiding simultaneous account-wide Rust maps; the live path remains capped at one million records and a separate estimated 64 MiB mismatch-detail budget. SQLite staging reduces verifier peak residency but is not a process-wide memory guarantee. Dovecot resume checkpoints are bound to a complete source/destination UIDVALIDITY digest. Message verification is durably restartable: fetched metadata pages, optional body fingerprints, and per-folder UID cursors survive a controller interruption, and each cursor records the folder's SELECT snapshot (UIDVALIDITY, UIDNEXT, EXISTS). A resumed scan reuses staged pages only when the server still reports that exact snapshot; otherwise the folder is rescanned from zero, and a folder completes only when its staged rows equal the snapshot's EXISTS. Each live engine attempt also has durable transfer-pass provenance (since ledger version 15, `transfer_passes` and `transfer_pass_folders`). A child run is one pass of a mailbox and its retries are attempts of that pass; the ledger numbers passes per mailbox within a project. At start, in the same transaction as the attempt marker, it records the mailbox-pair digest, pass kind (imapsync sync or the Dovecot strategy), engine, executable identity, the launched argument vector with credentials, private runtime file paths, and the Dovecot resume-state token replaced by placeholders, that command's SHA-256, the requested folder scope, and the source range (always the whole mailbox; a Dovecot resume point is recorded by the same digest the run's plan snapshot uses). At finish it records the classified outcome, whether a delta pass is required, the engine's own completion counters, and the digest of any newly emitted resume state. After verification it records the method and outcome and, per verified folder and side, the observed UIDVALIDITY/UIDNEXT/EXISTS snapshot, the UID the verifier reached, the staged row count, and whether the folder completed. Folder names are stored only as project-scoped SHA-256 digests. An attempt lacking a finish marker is reported as interrupted/unknown. This lets an operator reconstruct exactly what MailSwiftSync asked the engine to do in every pass and what was observed, but it is still not a per-message transfer checkpoint. The transfer itself still has no per-message checkpoint: an interrupted imapsync pass is rerun and skips messages already present, and Dovecot resumes through its UIDVALIDITY-bound state. The compatibility percentage is not a probability or an evidence level; the GUI identifies the evidence method and outcome separately and never treats an aggregate score as body equivalence. When the imapsync completion proof is absent, `unmatched_messages` is `null` rather than a fake count; the UI and exports must report the count as unknown rather than infer success from process exit status.

The ledger records project lifecycle, structured run events, reconciliation evidence, and bounded per-message mismatch rows; verbose engine transcripts remain bounded process-local diagnostics and are not persisted. The Dovecot adapter obtains aggregate folder/message/virtual-size status plus UIDVALIDITY for every reported mailbox from both the remote `imapc` source and the destination after a live run. Native Dovecot strategy selection is durable in the immutable run snapshot: backup is used for initial/incremental mirrors and `sync -1` for preservation passes; exit code 2 remains delta-required until a later pass returns 0. The imapsync adapter consumes its final summary and, for encrypted live runs, independently fetches bounded Message-ID, INTERNALDATE, and RFC822.SIZE metadata from every selectable folder into the private SQLite stage before reconciling and committing verification evidence and mismatch details atomically; an explicit forensic mode additionally fetches bounded RFC822 bodies and emits body-hash evidence only after complete coverage. Mismatch detail remains capped at 64 MiB; exceeding it fails closed for operator review. Verification exports include the durable run identifier and timestamps, so an audit artifact can be traced to one execution. Unix migrations run in their own process group; cancellation allows graceful shutdown before escalation, and Linux children receive a parent-death signal as an additional crash-safety measure. The runner records each started process together with platform process start-time, process-group, and session identity. Startup terminates a recorded Unix group only when those identity values still match; otherwise it refuses to signal the PID and leaves the job for operator review. Windows children are attached to a kill-on-close Job Object; macOS validates identity through `proc_pidinfo` and retains the conservative no-signal fallback when that native query fails. Production engines start behind MailSwiftSync's internal launcher, which holds the engine until the controller has durably registered the process identity and acknowledged it; if registration fails or the event channel disconnects, the child is cancelled before the engine runs. On Unix the launcher then replaces itself with the engine, so the recorded PID, process group, and session identify the migration engine itself. A saved Dovecot checkpoint is validated against a fresh source/destination UIDVALIDITY digest before resume; absent or changed context blocks the run.

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
opaque state string with `-s` (or an empty state for the initial pass). The
runner captures the state emitted on stdout, verifies source and destination
mailbox UIDVALIDITY values, and stores an envelope containing the state and a
deterministic 256-bit context digest in the same terminal transaction as the
child result and mailbox state. Dry preflight and failed or cancelled runs do
not replace the last committed checkpoint. A legacy raw state or a context
that changes before resume is rejected; a report without complete UIDVALIDITY
coverage does not publish a resumable checkpoint.

Profile replacement is flushed before rename, and SQLite uses WAL mode, foreign keys, a bounded busy timeout, and an explicit schema-version guard. Databases stamped by a newer application are refused rather than partially opened or downgraded. The persistent database directory is owner-owned and non-writable by group/other users on Unix; sensitive files are owner-only, so WAL/SHM sidecars cannot be modified by another local user even when SQLite creates them after startup. Evidence history is transactionally written and must reference an existing run (except for the explicitly supported legacy import path).

Terminal persistence is the control-plane commit boundary: an external engine
result is not allowed to advance the project phase until the run and mailbox
terminal state have been durably committed. If that commit fails, the
controller reports a durability review and leaves the durable run available for
startup recovery; it does not emit a terminal completion event or manufacture
an Attention/Complete transition in memory.
