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

The `core` module owns the durable project model. It persists projects, phases, mailbox jobs, audit events, per-run evidence history, verification evidence, and credential-free webhook delivery state. It intentionally never persists passwords, endpoint URLs, or mailbox content. The current SQLite schema (v30) establishes destination-identity policy v2, binds the one-row-per-mailbox current evidence projection to its run, records per-attempt transfer-pass provenance, stores narrow per-mailbox queue facts with a project-scoped FTS5 trigram search index, stores shared batch policies separately from mailbox identity/credential deltas, groups mailboxes into ordered, approvable migration waves, records per-run IMAP flag-verification results, distinguishes verification that stopped at a safety limit (`verification_limit_exceeded`) from other incomplete verification, stores project-scoped folder digests for mismatch drill-down without retaining folder names in mismatch rows, provides bounded webhook retry/dead-letter state with expiring atomic delivery leases, and transactionally turns selected ledger events into endpoint-unbound lifecycle deliveries with distinct completion, failure, preflight-failure, and proof-ready event types. Queue-fact triggers keep the search index current and upgrades rebuild it transactionally; terms shorter than three characters retain the literal substring fallback. Since v30 each queue fact also carries trigger-maintained counts of its mailbox's runs, queued runs, and verification acceptances, so the presented queue state (imported, queued, verification difference, or the durable state) is derived from one narrow covering index rather than by probing `runs` for every row. Counts, filters, selection summaries, and visible pages read that projection; admission and other execution paths still read `mailbox_jobs` directly. A ledger invariant requires the stored counts to equal the live rows. Project-scoped webhook workers bind and claim only their project's events. Normal opens trust the versioned identity invariant instead of reparsing every mailbox's serialized config; GUI/report reads use the current projection instead of repeatedly finding the latest historical record. Older ledgers rebuild these invariants transactionally during migration. Deep integrity checks remain separate from the ordinary startup path.

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

The batch queue is durable state: an import is written to the ledger as a new batch project in one transaction (on a worker thread with its own connection), and the project's `mailbox_jobs` rows are the queue. Each row also has a narrow `mailbox_queue_facts` row keyed by the mailbox rowid with its label, hosts, a case-folded search key, its destination-mutation policy, and a trigger-maintained copy of its state, so filtering and counting never decode or read past the per-row plans. Shared batch policies live once in `batch_plans`; mailbox rows retain only identity and credential deltas, while legacy inline configurations remain readable. The default unfiltered Mailboxes view obtains rowids from SQLite in pages and keeps only a bounded cache of on-screen rows; search and state filters currently retain their matching rowid list. Queue-health and selection counts are SQL aggregates rather than plan scans. Plaintext-import session passwords and preflight credential fingerprints stay in memory only, keyed by job ID. The one presentation-only state is a row waiting out a retry; queued, running, and terminal states are durable. Admission scans the queue from `mailbox_jobs` (never the derived state copy), then decodes, validates, and digests each selected plan one at a time without retaining plans or credentials; the scheduler reads admitted jobs back from the ledger in pages of 64 through its own read-only connection and loads each dry run's credentials as it reads the job. Batch validation and migration use a bounded OS-thread/process worker pool (1–32 workers; default 2). This is intentionally simple and safe for the current desktop control plane; future fleet-scale operation should move cheap probes and reconciliation toward asynchronous or pooled I/O rather than multiplying engine processes casually. Historical mailbox and verification pages use rowid keyset cursors, avoiding progressively expensive `OFFSET` scans. UI “select all visible” stores an all-matching marker plus excluded IDs instead of copying every selected mailbox ID, and action review hashes selected rows incrementally. Admission still holds the selected job IDs and per-child plan snapshots for the durable run transaction, so very large executions are bounded by the persisted-plan budget rather than constant-memory. Admission creates a parent wave run plus one mailbox-specific child run and snapshot per row in one transaction. Mailbox rows remain `queued` until a worker claims them; process identity and terminal results are then attributed to the child run. Worker output is redacted and retained only in bounded process-local UI memory; raw `run_output` transcripts are not persisted, preventing subjects, folder names, and message metadata from becoming durable ledger data. Structured lifecycle, run, phase, and evidence events remain durable.

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

The Project Cockpit performs an authenticated endpoint readiness probe in imapsync mode over certificate-verified TLS. Implicit IMAPS begins inside TLS; STARTTLS first verifies the server's advertised upgrade capability, negotiates TLS, then follows the same greeting/authentication/`CAPABILITY`/`NAMESPACE`/`LIST` sequence. The probe requires at least one untagged `* LIST` record and reports the discovered folder count and observed SPECIAL-USE annotations; a tagged `LIST` completion alone is not accepted as an inventory. Operators may add a PEM enterprise CA bundle; it is included in the plan identity and rechecked before live admission. Leaf certificate pins are validated by the Rust probe, but pinned plans are rejected before transfer because the qualified imapsync/IO::Socket::SSL backend is not currently qualified to enforce a strict pin on every connection and reconnect. This prevents a successful preflight pin check from being mistaken for transfer-level pin enforcement. Live admission uses this transport-specific probe for every encrypted imapsync endpoint; plain sources remain engine-preflight-only after explicit acknowledgement. Dovecot dry preflight checks the destination-side userdb with `doveadm user` and lists source mailboxes through remote `imapc`; it deliberately does not enumerate the target mailbox store before its first sync because Dovecot warns that early target access can cause GUID/UIDVALIDITY conflicts or synchronization failure. Dovecot certificate pins are also rejected because the native engine does not enforce them. Quota capacity remains an operator responsibility where it cannot be queried reliably. Credentials are held only by the probe thread and are never written to the project ledger or command-line arguments; control characters are rejected before they can enter the IMAP command stream. The probe does not claim that these extensions replace Dovecot's server-side dsync behavior. A failed certificate, authentication, or folder-inventory check blocks discovery rather than being silently ignored.

The Mailboxes workspace presents the same lifecycle as the Overview cockpit: import, resolve blockers, preflight, pilot/seed, catch-up, final delta, verification, and proof delivery. It surfaces the recommended next action for the active phase and keeps queue-policy controls under an explicit Operator tools disclosure, so expert controls remain available without competing with the normal migration path.

Overview's Migration confidence panel composes current plan completeness,
plan-bound endpoint observations, preflight outcomes, available quota
observations, verification scope, provider qualification, and mailbox
attention into named readiness states rather than a numeric score. It treats
missing qualification or capacity evidence as unknown, not success; provider
probe success is not provider qualification, and a non-exhausted quota is not a
capacity guarantee. Each finding exposes its consequence and routes to an
existing Plan, Mailboxes, or Verification remediation surface. A plan-affecting
remediation flags when another preflight is required.

Recovery groups are actionable projections of durable attention reasons: each group states the consequence of leaving it unresolved and routes to the safest existing follow-up (plan/account repair, queue policy, evidence review, or explicit mailbox selection). A remediation route never silently starts a migration; it returns the operator to the review/preflight path, and the group remains durable until the required evidence is recorded.

External-engine transfer progress is checkpointed as bounded, content-free JSON events at a coarse interval and at attempt completion. These checkpoints record observed aggregate message/byte counters and the attempt number; they do not claim individual-message transfer proof. MailSwiftSync deliberately does not turn imapsync's human-readable `msg ... copied to ...` lines into a resume manifest: those lines do not carry a complete, stable UIDVALIDITY-bound identity contract across reconnects, folder renames, deletions, duplicate messages, or destination-side changes. A future manifest may optimize discovery only after the qualified engine exposes a versioned per-message acknowledgment contract and the verifier revalidates source/destination UIDVALIDITY and folder snapshots before every reuse. Until then, imapsync retries still fail closed to revalidation rather than pretending to resume at a message boundary. Dovecot resume tokens and the independent imapsync verification cursors remain the stronger recovery/evidence mechanisms. Independent imapsync message verification is limited to identity-preserving plans and explicit typed folder mappings whose expected destination is immutable; automap, folder-only scope, header mutation, disabled INTERNALDATE synchronization, and permitted size mismatches are rejected from exact certification until qualified transformation models exist.

System name resolution for authenticated IMAP probes runs in a short-lived internal helper process because the platform `getaddrinfo`/`ToSocketAddrs` call cannot be cancelled safely in-process. The parent caps concurrent resolver helpers at 16, applies the existing per-probe DNS deadline, and kills/reaps the helper on timeout or cancellation; a stuck system resolver therefore cannot permanently occupy a reusable worker thread. The helper starts with a resolver-specific allowlist (including `LOCALDOMAIN` and `RES_OPTIONS` where configured, but excluding unrelated credentials) and returns at most 64 socket addresses. On Linux the helper also receives a parent-death signal, so a controller crash mid-lookup does not orphan it. Unit-test builds resolve directly, but the production parent path (argument contract, output parsing, helper failures, deadline, cancellation, the 16-helper bound, slot release, and crash cleanup) is shared code that resolver tests drive with stub helpers, and `tests/dns_resolver.rs` runs the packaged binary's `--internal-dns-resolve` contract for literal, named, unresolvable, malformed, and parallel lookups. Connection attempts then use staggered IPv4/IPv6 racing under a separate overall deadline. This bounds MailSwiftSync-owned resolver resources, but cannot guarantee that every platform resolver implementation will return within the deadline absent process termination support from the OS.

## Ledger invariants

The SQLite ledger is validated at two levels.
`validate_schema_layout`/`validate_schema_constraints` prove its shape: tables,
columns, indexes, triggers, CHECK constraints, and `foreign_key_check`.
`validate_internal_invariants` (`src/core/database/invariants.rs`) adds
`integrity_check` and the data-level invariants that shape cannot express:

- the FTS5 search index has exactly one row per queue fact, with the same key;
- each queue fact projects its job's rowid, id, project, and state;
- every batch plan is used by a mailbox in its own project, and no mailbox
  references another project's plan (plans are deduplicated by content digest);
- every run's mailbox and parent run belong to the run's project;
- every wave has members, all from the wave's project (a mailbox belongs to
  at most one wave);
- webhook leases exist exactly while a delivery is `delivering`.

Restore refuses a ledger that violates any of these. Tests inject an aborting
trigger at each write boundary of batch import, phase changes, outbox-producing
events, cutover planning, and webhook outcomes, then require the failed
operation to leave no partial rows and every invariant intact. Optimizations
that temporarily drop a trigger (the set-based search-index rebuild during
import) must do so inside the same transaction so a failure restores it.
When adding a derived table, trigger, or cross-table reference, add its
invariant to `DATA_INVARIANTS` and a failure-injection case.

## Migration phases

`Discovery → Preflight → Pilot → Seed → Catch-up → Final delta → Verification → Complete`

`Attention` is a terminal operator-review state. A migration should never silently claim completion after an unresolved verification mismatch.

## Verification contract

The evidence schema can hold source/destination folder and message counts, byte counts, an optional unresolved-message count, and failed messages. Operator-facing verification uses three evidence levels: Level 1 compares aggregate folder/message/byte totals and engine counters but does not compare individual messages; Level 2 reconciles per-message identity and metadata such as Message-ID, INTERNALDATE, and RFC822.SIZE without comparing bodies; Level 3 performs opt-in, bounded RFC822 body-content fingerprints. Level 3 is not a complete byte-for-byte mailbox proof and remains provider-unqualified. Independently of the level, encrypted imapsync runs compare IMAP FLAGS (system flags and custom keywords) for every message pair whose identity is unambiguous (exactly one source and one destination message share the Message-ID, expected folder, INTERNALDATE, and RFC822.SIZE); groups of identical duplicates (the same identity on both sides, the same number of times) are compared as multisets of flag sets, since no observer can tell the copies apart but a lost or altered flag still changes the multiset; probable pairings and unequal groups are not compared, and the compared count is reported as flag coverage. Exact messages with partial or excepted flag coverage are stored with their exact message label and a `verification_difference` state that an operator can accept; actual flag mismatches downgrade the label to `flags_changed`. A difference consisting only of flags the destination folder's PERMANENTFLAGS cannot store is a recorded provider exception; any other difference is the `flags_changed` outcome and blocks `verified`. Flags are compared at verification time, so a user changing flags between the source and destination scans, or after cutover, appears as a difference. Incomplete evidence has no level, and process completion alone is never verification. Encrypted imapsync live runs perform bounded Level 2 reconciliation by default using independently fetched metadata; an explicit forensic profile can perform Level 3 SHA-256 body-content comparison within per-message and total-byte bounds. Native Dovecot runs currently provide Level 1 aggregate evidence. UID enumeration uses sequence-number `FETCH` pages bounded by the selected mailbox's `EXISTS`, so command count follows extant messages rather than historical `UIDNEXT` gaps and a mailbox-wide `UID SEARCH ALL` response is never materialized. Live metadata is staged in a private SQLite database and reconciled in bounded batches, avoiding simultaneous account-wide Rust maps; the live path remains capped at one million records and a separate estimated 64 MiB mismatch-detail budget. SQLite staging reduces verifier peak residency but is not a process-wide memory guarantee. Dovecot resume checkpoints are bound to a complete source/destination UIDVALIDITY digest. Message verification is durably restartable: fetched metadata pages, optional body fingerprints, and per-folder UID cursors survive a controller interruption, and each cursor records the folder's SELECT snapshot (UIDVALIDITY, UIDNEXT, EXISTS). A resumed scan reuses staged pages only when the server still reports that exact snapshot; otherwise the folder is rescanned from zero, and a folder completes only when its staged rows equal the snapshot's EXISTS. Each live engine attempt also has durable transfer-pass provenance (since ledger version 15, `transfer_passes` and `transfer_pass_folders`). A child run is one pass of a mailbox and its retries are attempts of that pass; the ledger numbers passes per mailbox within a project. At start, in the same transaction as the attempt marker, it records the mailbox-pair digest, pass kind (imapsync sync or the Dovecot strategy), engine, executable identity, the launched argument vector with credentials, private runtime file paths, and the Dovecot resume-state token replaced by placeholders, that command's SHA-256, the requested folder scope, and the source range (always the whole mailbox; a Dovecot resume point is recorded by the same digest the run's plan snapshot uses). At finish it records the classified outcome, whether a delta pass is required, the engine's own completion counters, and the digest of any newly emitted resume state. After verification it records the method and outcome and, per verified folder and side, the observed UIDVALIDITY/UIDNEXT/EXISTS snapshot, the UID the verifier reached, the staged row count, and whether the folder completed. Folder names are stored only as project-scoped SHA-256 digests. An attempt lacking a finish marker is reported as interrupted/unknown. This lets an operator reconstruct exactly what MailSwiftSync asked the engine to do in every pass and what was observed, but it is still not a per-message transfer checkpoint. The transfer itself still has no per-message checkpoint: an interrupted imapsync pass is rerun and skips messages already present, and Dovecot resumes through its UIDVALIDITY-bound state. The compatibility percentage is not a probability or an evidence level; the GUI identifies the evidence method and outcome separately and never treats an aggregate score as body equivalence. When the imapsync completion proof is absent, `unmatched_messages` is `null` rather than a fake count; the UI and exports must report the count as unknown rather than infer success from process exit status.

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
terminal state have been durably committed. If that commit fails because of
storage (a busy, full, or unavailable database), the controller keeps the
result, retries the identical commit, reports a durability review, and leaves
the durable run available for startup recovery; it does not emit a terminal
completion event or manufacture an Attention/Complete transition in memory. If
the store instead rejects the result because it would violate a ledger
invariant (for example, an exact verification label over contradictory
counters), retrying can never succeed: the controller records the run as
failed and the mailbox as Attention, with the named invariant in the run
detail, so the result is reviewed rather than presented as verified.
