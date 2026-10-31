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

### M6. Multi-controller fleet (needs a scope decision)

Today one controller owns its ledger. The control plane (M2) coordinates
workers, but each worker runs its own rate limiter. A fleet that shares
provider quotas needs:

- a central quota authority that grants bounded capacity and pauses work
  when it is unreachable;
- trace correlation from tenant to worker;
- possibly the PostgreSQL scheduler the review proposes.

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
