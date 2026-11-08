# Development plan

Working plan for taking MailSwiftSync from Technical Preview (`0.1.0-alpha.1`)
to a stable release. The release bar itself is defined in
[docs/release-readiness.md](docs/release-readiness.md) and the capability
inventory in [capabilities.toml](capabilities.toml); this file orders the work
and states how each milestone is judged done. Progress and evidence are logged
in [PROGRESS.md](PROGRESS.md).

## Where the release stands (2026-10-30)

- Code-side gates are green: `make check`, the full test suite, the
  cross-platform CI matrix, the IMAP integration lab, fuzzing, and the
  dependency audit.
- No open GitHub issues and no open code-scanning alerts. PRs #147, #148,
  #151 and #153 are merged.
- Most remaining 1.0 blockers are **external**: they need signing
  credentials, live provider tenants, or a person with a screen reader. They
  are listed under "Blocked on people" so they are not mistaken for code work.

## Milestones (highest priority first)

### M0. Baseline gates green: done

Done when:

- `make check` exits 0.
- `cargo test --locked --all-targets --all-features` reports 0 failures.

### M1. Clear the open CodeQL alert on PR #148: done

Alert #95 (`rust/weak-sensitive-data-hashing`) flagged SHA-256 over a
256-bit random API token in a test, not a password; passwords use Argon2id.
The repository owner dismissed it as a false positive.

### M2. Control-plane worker agent: done (live drill outstanding)

The control plane hands out fenced leases, but nothing executes them, so the
"MSP RBAC and tenant isolation" capability stays partial. Add
`mailswiftsync control worker`:

- `control worker plan <state.db>` prints a non-secret plan document for the
  local durable batch: the ledger project ID and a SHA-256 of each mailbox
  row's effective plan, keyed by row label. The operator uploads it as the
  project plan, so approval in the control plane covers exactly those plans.
- `control worker run <state.db> --server <url> --credential-file <file>`
  leases one assignment at a time, finds the local batch row whose label
  matches, refuses unless the leased plan names this ledger project and the
  row's recomputed digest equals the approved one, runs that row through the
  existing headless batch path, renews the lease in the background, cancels
  the run when the lease is lost, and reports a non-secret result.

Acceptance criteria:

1. An end-to-end test against an in-process control server enrolls a worker,
   leases an assignment, and completes it with the local outcome.
2. A plan digest mismatch, an unknown label, or a different ledger project
   is reported as a failed assignment and never starts a transfer.
3. Losing the lease (renewal refused) requests cancellation of the local
   run and does not submit a completion.
4. Credentials never leave the worker: the result passes the service's
   credential-field check, and the credential file must be owner-only.
5. `make check` and the full test suite pass; docs/control-plane.md and the
   `capabilities.toml` row describe the agent honestly.

Criteria 1–5 are met by tests (see PROGRESS.md). For criterion 3, the
agent-level stop is tested. Cancelling a real imapsync batch through the
headless stop flag still needs a live drill in the Docker IMAP lab.

### M3. Per-row verification capacity for batch imports: done

PR #147 checked verification capacity only in the single-plan account test.
Batch rows were not probed before admission. Done when batch preflight reports
the same blocked/unknown/passed capacity row per mailbox, with tests for
each outcome.

Met on `feat/batch-verification-capacity`:

- After a mailbox passes its batch dry run, both accounts are probed and the
  single-plan row is computed from the counts.
- The row is appended to the mailbox's dry-run result and recorded as a
  `verification_capacity` ledger event.
- It is counted in `status --summary` and in the metrics gauge.
- Tests cover passed, blocked, unknown (a failed probe, plain IMAP), Dovecot,
  secret redaction, cancellation, and ledger persistence.

### M4. Verification beyond 1,000,000 messages per account: done (3M)

Verification refused exact evidence above 1,000,000 messages per endpoint
and recorded `verification_limit_exceeded`.

The maintainer chose one pass with a higher durable limit over segments,
because reconciliation is account-wide: wrong-folder matches and
fingerprint uniqueness would change meaning across segments.

Met on `feat/durable-verification-above-1m`:

- Durable stages verify up to 3,000,000 messages per account; ephemeral
  stages keep 1,000,000.
- The opt-in scripted-server test verifies 1,000,100 messages per side with
  100% exact coverage.
- The benchmark meets its budgets at 1M, 2M and 3M. It misses the
  reconciliation-rate budget at 5M and 10M (see
  docs/verification-envelope.md).

Going past 3M needs faster reconciliation on large stages, or a decision
to relax the 20,000 msg/s budget for them.

### M5. Durable per-message transfer journal (review finding 3)

imapsync is external, so per-message progress has to be observed from its
output. Needs a scope decision from the maintainer before work starts:
journal granularity, retention, and whether resume may skip work based on it.

### R1. External architecture review (2026-10-29)

Each finding was checked against the code before any change.

| Finding | Verdict | Outcome |
|---|---|---|
| C1. Thread and subprocess model, not Tokio | Accurate description, not a defect | No change. Concurrency is bounded by the rate limiter and bounded channels. Distributed work goes through the control-plane lease protocol (M2), not an async rewrite. |
| C2. Retry schedule only in memory | Valid | Fixed on `fix/batch-claim-and-retry-durability`: retries are recorded with a wall-clock deadline, and later runs honour it. |
| C3. Panic containment without process supervision | Valid on Unix | Fixed on the same branch: a panicking supervisor ends the engine's process group and keeps its durable identity unless the group is gone. |
| S1. Claim accepts a running child | Valid | Fixed on the same branch, with a regression test that fails on the old code. |
| S2. Ownership needs fencing and isolation | Partly valid | Leases, fencing tokens, and agent self-stop already existed. Fixed here: an expired lease is never re-leased automatically; 60 seconds after expiry it goes to `needs_reconciliation`, and an operator requeues it with an audited reason (recovery policy C). Revocation no longer re-queues at once, fenced writes refuse revoked workers, and the agent times its lease from the request. |
| R1. Rate limits live in one controller's memory | Valid for a fleet | Not yet fixed. Two controllers cannot share a provider quota today. Proposed as M6 (below). |
| R2. Correlated retry jitter | Valid | Fixed in #151: OS-random full jitter above the required floor. The old jitter was zero at the 120-second cap. |
| R3. Retry state and admission limits | Mostly already true | Retries park without holding a worker; #151 persists their deadlines. The scheduler pages plans 64 at a time within a lookahead bounded by concurrency. |
| M1. JobSource holds every job ID | Accurate, minor | Three strings per mailbox, with batches capped at 100,000 (about 15 MB). Plans and credentials are already paged. Low priority. |
| M2. Process cleanup needs fleet supervision | Addressed | C3 (Unix group kill on panic), S2 (reassignment grace, revoked leases held), and agent self-stop. |
| M3. Blocking HTTP clients | Not a defect here | No Tokio runtime exists. Clients have timeouts. Revisit only with an async coordinator. |
| Observability | Partly valid | #151 adds the alert the review called most important: `oldest_running_heartbeat_age_seconds`. Other proposed series and trace correlation are part of M6. |
| Proposed PostgreSQL scheduler crate | Scope decision | This would replace the SQLite ledger's ownership layer for multi-controller fleets. Not started without a maintainer decision (M6). |
| S3. Exactly-once delivery cannot be assumed | Accurate; the design already assumes at-least-once | Uncertain runs go to operator review (`process_identity_unverified`, `interrupted`), never to automatic replay, and verification reports extra destination messages. A per-message journal with a scoped reconciliation key is M5, which still needs a scope decision. |

### R2. External review findings (2026-10-30)

| Finding | Verdict | What changed |
|---|---|---|
| H-01 Non-injective stage file names | Confirmed; fixed | Stage files are named by a domain-separated SHA-256 of the full job ID. A legacy stage is adopted, sidecars first, only when its old name was unambiguous; an ambiguous one is left in place. |
| H-02 Cleartext source IMAP after acknowledgement | Partly confirmed; fixed | The production profile already refused plain IMAP on every path. Two gaps fixed: the exception had no host scope or lifetime (now `plain_imap_hosts` and `plain_imap_expires`), and a single dry preflight skipped the transport rule while still logging in. Workers enforce their local policy, and control-plane plans carry only digests. |
| H-03 Unbounded GUI probe threads | Confirmed; fixed | A process-wide cap of four running account tests. Requests beyond it are refused at once, with no thread started. Results of a superseded request were already discarded by request ID. |
| H-04 Metadata is not content proof | Already met | Evidence levels are explicit (aggregate, metadata, content). The content label says "not full byte-for-byte proof". Reports name each mailbox's level, and the customer proof states the per-folder snapshots and that later changes are not covered. Policy can require a minimum level. No change. |
| M-01 Durability of artifact publication | Partly confirmed; fixed | The writer already guaranteed a whole old or new file. No ledger state depends on an artifact being published, so a record-based publication protocol would only add a second source of truth. Added: the documented boundary, crash-injection tests at all three steps, and removal of stale temporaries a crashed writer left. |
| M-02 Cross-system completion after a crash | Already met; risk stated | A process exit is never treated as proof of no destination change. An interrupted or failed attempt is `interrupted`/`attention`, never `completed`, and process completion is never `verified`. A retry reruns imapsync, which inspects the destination by message headers and skips what is already there. Independent verification then reconciles by Message-ID, INTERNALDATE and size, never by UIDs, before `verified`. Production refuses `--useuid` with `--usecache`, which can duplicate an in-flight APPEND on resume. Residual risk: messages with no usable header identity rely on imapsync's header fingerprint; that is the per-message journal decision in M5. |
| M-03 Resource limits for blocking work | Already met (probes fixed in H-03) | Bounded today: transfers (batch concurrency, organization `max_concurrency`, default 8), account tests (4, H-03), DNS lookups (16 resolver subprocesses, killed at their deadline), verification (its own worker pool), run events (bounded `MAX_PENDING_EVENTS` / `MAX_BATCH_PENDING_EVENTS` channels, lifecycle events acknowledged), the diagnostic log (its own bounded queue). SQLite is written by the single controller thread. Engine output readers are one pair per engine process, so they follow the transfer limit. No Tokio conversion is planned. |
| M-04 Live-provider evidence | Agreed; external | Qualification cannot be inherited: the release gate accepts only evidence whose `mailswiftsync_commit` is the release commit, and binds the MailSwiftSync and imapsync binary SHA-256s and a supported engine version. The live matrix (provider pairs, sizes, OAuth rotation, throttling, disconnects, interrupted final deltas) needs real tenants; it is listed under "Blocked on people". |
| L-01 Typed errors | Partly met; staged as M7 | Retry, backoff, cooldown and terminal state already key off a typed `FailureClass`, classified once at the boundary (`ClassifiedFailure`, PR #128). Verification limits carry stable `[verification_limit=<code>]` codes. About 474 producers still return `Result<_, String>`; see M7. |
| L-02 Correctness metrics | Partly met; gaps on `feat/integrity-metrics` | Already exported: running jobs, retries and outcomes by failure class, oldest heartbeat age, unresolved message findings, verification capacity. Added: attention mailboxes by durable reason, plus compared-source and exact-match counts for coverage. Commit latency cannot come from the after-the-fact `metrics` command. Lease losses belong to the control-plane service. Labels are closed sets; no mailbox, address or subject is a dimension. |
| §6 Attack-surface follow-ups | No new defect | Plaintext exceptions: H-02. Probe budget: H-03. Still to run: disk-full, corruption and concurrent-restart drills; hostile cross-tenant authorization tests against the control server; fuzzing engine argument construction; Maildir durability through doveadm on real filesystems. |
| §7 Evidence-based terminal transitions | Already met | A batch child's evidence and its terminal mailbox state commit in one ledger transaction, after its diagnostics. The child-run claim updates only from the expected state and checks row counts (R1 S1). Control-plane completion is fenced by lease token and worker. Interruptions become `interrupted`/`attention`, never `verified`. Maildir rules depend on doveadm and need engine-version and filesystem qualification. |

### R3. Control-plane review findings (2026-11-07)

| Finding | Verdict | What changed |
|---|---|---|
| P1 HTTP body limit below the plan limit | Confirmed; fixed | The server refused bodies over 64 KiB while the store accepts 256 KiB plans. The body limit is now twice the plan limit, enough for the largest plan pretty-printed; a test sends exactly that through the request reader. |
| P1 Serial agents; agents sharing a ledger contend for its lock | Confirmed; contention fixed, concurrency staged in M6 | A run takes the ledger's exclusive controller lock, so a second agent (or the desktop app) on the same ledger leased work it could not run and reported it `failed`. The agent now probes the lock before leasing and, if it is held, leases nothing and waits, so the work stays queued for other workers. One agent per ledger is documented; parallel mailboxes come from separate ledgers. Running several leases inside one agent, through the batch worker pool under one lock, is an M6 design item. |
| P1 OIDC pending-login quota without a per-client throttle | Confirmed; fixed | Pending sign-ins record a client key (IPv4 address or IPv6 /64; control-plane database version 5), at most 20 per client within the global 10,000. Both quotas answer 429 with `Retry-After` (the global one used to be a 500). Loopback is exempt because a TLS-terminating proxy makes every user loopback; the docs require that proxy to rate-limit `start`. |
| P1 Reads served when their audit write fails | Confirmed; fixed | The read audit result was discarded. Reads now propagate it and return 500 with no data; a test blocks audit inserts and checks every audited read. Mutations already audited inside their own transaction. |
| P2 Plan secrets detected by field name | Confirmed; fixed | A plan must be the `mailswiftsync.worker-plan` v1 document, with only its known fields and typed values (64-hex digests, bounded names). Results are limited to `job_id`, `plan_revision`, and `detail`. Unknown keys are refused whatever they are called. |
| P2 Lease renewal stops before completion is acknowledged | Confirmed; fixed | Reporting now runs inside the renewal scope, so the lease is renewed through completion retries. A test shows renewals during refused reports; with the old ordering it saw none. |

**§8 enterprise-readiness follow-ups (2026-11-07):**

| Finding | Verdict | What changed |
|---|---|---|
| Five bad passwords lock a known account | Confirmed; fixed | Failures count per (account, client), keyed like the OIDC quota, and lock only that client; the account itself locks after 50 failures from all clients. `password_sign_in_for_linked_accounts = false` (IdP-first) refuses passwords for linked accounts without counting them, so they cannot be locked out by guessing. |
| `/healthz` has no readiness check | Confirmed; fixed | `/readyz` takes the write lock in a rolled-back transaction, checks the schema version, and reads the audit chain head; `503` otherwise. `/healthz` stays liveness. |
| One SQLite connection behind a mutex | Confirmed; measured | An opt-in benchmark drives audited reads over loopback HTTP: about 1,400–1,500 requests/s on NVMe with p99 74 ms at 32 clients, bounded by serialized commits. Documented with guidance; a pool or another database is part of M6. |
| OIDC fetches under the metadata mutex | Confirmed; fixed | Fetches run outside the cache lock, under a refresh lock only refreshers take; a test shows a stalled refresh no longer blocks sign-ins served from the cache. |
| No external audit anchor | Confirmed; fixed | `control audit-checkpoint` writes a signed chain head to publish outside the service; `audit-verify --checkpoint` catches a chain rebuilt after an edit, which the chain alone cannot. |

### R4. Scale and usability review (2026-11-07)

| Finding | Verdict | What changed or is planned |
|---|---|---|
| Control HTTP server: thread per connection, fixed 32 | Confirmed | Throughput is bounded by serialized SQLite commits (about 1,500 audited requests/s, #168), not by the thread count, so more threads alone would not raise it. A `--max-connections` option waits for #168 to merge (both touch the CLI option list). An async server or pool belongs with M6. |
| Central SQLite store behind one mutex | Confirmed; measured in #168 | Every API request commits an audit entry, so a read pool would not remove the bottleneck. A pool or another database is an M6 item. |
| Agent runs one assignment per loop | Confirmed; staged in M6 | Recorded with R3. |
| Plan export is one JSON object listing every mailbox | Confirmed; design decision needed | The 256 KiB plan limit holds about 2,000 mailboxes. Options in M8. |
| Body verification buffers whole FETCH pages, 64 MiB ceiling | Confirmed; detection fixed, streaming planned | Preflight now counts messages larger than the per-message bound (`EXAMINE` + `UID SEARCH LARGER`) and blocks before the transfer. Streaming or partial-fetch hashing and resumable segments are M8. |
| Desktop queue materializes IDs for explicit selections | Confirmed; within budget | Paged browsing holds only the viewport; explicit selection keeps one integer row ID per match, inside the enforced 100,000-row UI budgets. Revisit if larger single-project queues are supported. |
| Supervisor linear search inside `retain()` | Confirmed; fixed | Pruning uses a set. |
| `docs/control-plane.md` says the worker agent is still to come | Confirmed; fixed | The agent shipped in #152; the intro now describes it. |
| Installer: no native Windows/macOS installers or signed repositories | Agreed; blocked on people | Needs signing identities and repository hosting; see "Blocked on people". |
| A `doctor` that reports readiness with remediation | Mostly met | `mailswiftsync doctor [--strict]` already reports the OS, engine version and qualification (with an install hint), TLS/auth modes, keyring references, database state, owner-only directories, and free space, with exit codes for automation. Missing: OAuth token readiness, live TLS reachability, and a remediation line for every check. Staged in M8. |

### M8. Verification at any message size, and planning it up front

Goal: an operator who asks for a proof level learns before the transfer
whether it can be produced, and the verifier is not limited by one
response buffer.

1. **Partial-fetch body hashing: done.** Each metadata page fetches the
   first 256 KiB of every body; a body that fills it continues in 4 MiB
   `BODY.PEEK[]<offset.length>` ranges into the same SHA-256, so memory is
   one range regardless of message size. The per-message bound rose from
   64 MiB to 2 GiB (still within the total budget). Not yet run against a
   live provider.
2. **Resumable verification segments.** Persist per-folder hashing
   progress in the durable stage, as metadata pages already are, so an
   interrupted body pass resumes instead of refetching.
3. **Time and disk estimate in preflight.** From the census (messages,
   bytes, oversized count) and measured rates, show the expected stage
   disk and verification time beside the capacity check.
4. **`doctor` gaps.** Optional `--connect` checks for OAuth refresh and
   TLS reachability of configured accounts, and a remediation line for
   every failing check.
5. **Plan export at MSP scale: paged plan upload (decided 2026-11-07).**
   Keep the approval document format; store plan mailboxes as rows and
   upload large plans in pages that commit as one new revision. Leases
   carry only the leased mailbox's entry.

### M7. Typed errors through the producers (review L-01)

Retry decisions are already typed at the boundary. The producers still
return `Result<_, String>`, so the boundary parses tagged text.

Stage it as follows:

1. IMAP and verification producers return a typed error carrying class,
   side, and `retry_after`.
2. Message staging and SQLite errors keep their `rusqlite::Error` source.
3. Strings are produced only at the UI, report and automation boundaries.

Done when no retry or attention decision parses formatted text.

### M6. Multi-controller fleet (needs a scope decision)

Today one controller owns its ledger. The control plane (M2) coordinates
workers, but each worker runs its own rate limiter. A fleet that shares
provider quotas needs:

- a central quota authority that grants bounded capacity and pauses work
  when it is unreachable;
- trace correlation from tenant to worker;
- possibly the PostgreSQL scheduler the review proposes;
- a connection pool or another database for the control plane, once load
  approaches the measured plateau of about 1,500 audited requests/s;
- several leases running inside one worker agent, through the batch
  worker pool under one ledger lock (review R3 P1). Today an agent runs one
  assignment at a time and parallelism comes from one agent per ledger.

This is an architectural change, so it waits for a maintainer decision.

## Blocked on people (not code work)

| Item | Needs |
|---|---|
| Signed release artifacts | GPG, Authenticode, Apple Developer ID secrets in CI |
| Live provider qualification (Gmail, Microsoft 365, Fastmail) | Disposable tenants and accounts |
| OAuth token expiry during a real transfer | Disposable Google/Microsoft tenants |
| Screen-reader passes (Narrator, NVDA, VoiceOver, Orca) | A person on each platform |
| Signed APT/YUM repositories | Hosting and the release key |
| Published representative-scale migration evidence | Representative datasets and a release host |
