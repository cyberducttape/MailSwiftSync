# Progress

Work log for [DEVELOPMENT_PLAN.md](DEVELOPMENT_PLAN.md). Newest first.

## 2026-11-18: Preserve SQLite staging errors through IMAP fetch (M7, partial)

The durable verification sink now returns typed IMAP errors. SQLite failures
retain their `MessageStageError`/`rusqlite::Error` source chain, are classified
as local storage failures, and do not enter provider/session retry handling.
Non-SQLite stage validation errors remain unclassified. The verification
runner still converts its final result to a string, so this does not complete
typed propagation through the controller/report boundary.

Evidence: focused SQLite-related tests and the full
`cargo +1.92.0 test --locked --all-targets --all-features` suite passed
(1,530 passed, 14 ignored); formatting and Clippy with `-D warnings` passed.

## 2026-11-16: Keep unclassified OAuth failures out of text classification (M7)

The `Unclassified` OAuth error path previously handed its diagnostic string to
the legacy provider classifier. A persistence diagnostic containing words such
as `invalid_grant` could therefore be mistaken for an authentication failure.
It now remains `Unknown`, carries no provider signal or retry delay, and is
terminal for the current attempt. Regression test covers the misleading-text
case.

Evidence: focused regression and full `cargo +1.92.0 test --locked
--all-targets --all-features` passed (1,524 passed, 14 ignored);
`cargo +1.92.0 clippy --locked --all-targets --all-features -- -D warnings`
and formatting check passed.

## 2026-11-16: Typed OAuth refresh failures (M7, partial)

Patch was carried from branch `feat/typed-oauth-refresh-errors` onto current
`main` as `feat/typed-oauth-refresh-errors-main`; the original worktree remains
untouched. The OAuth producer now has typed authentication, capacity,
transport, configuration, provider and invalid-response failures. Live batch
admission maps those to the endpoint side and controller failure class without
parsing presentation text. `Retry-After` accepts delta-seconds and HTTP-date
values and is capped at 30 minutes. Authorization-code exchange converts the
typed error at its CLI/API boundary. Keyring and rotation-persistence errors
remain unclassified and now bypass legacy text classification.

## Existing progress entries
## 2026-11-16: Preserve SQLite errors in verification page writes (M7 step 2)

Branch `fix/typed-verification-storage-errors`:

- Added `MessageStageError`, which preserves `rusqlite::Error` as its source
  for staged page writes while keeping non-SQLite validation errors distinct.
- Updated the disk-full regression to assert the typed `SQLITE_FULL` result and
  verify the page transaction rolls back while the durable stage survives.
- The IMAP message sink still converts stage errors to strings. Extending the
  typed error through the fetch adapter and controller classification remains
  outstanding; this is not completion of M7.

Evidence: focused SQLite-full and page-rollback tests passed; full
`cargo test --locked --all-targets --all-features` passed (1,511 passed, 14
ignored, all integration/documentation tests passed); `cargo clippy --locked
--all-targets --all-features -- -D warnings` and `cargo fmt --check` passed.

## 2026-11-15: Explicit per-mailbox assurance steps

Branch `fix/ipv6-cooldown-and-stale-batch-summary`:

- The Mailboxes review card now shows transfer completion, metadata
  reconciliation, bounded content verification, and cutover approval separately
  without requiring the operator to open the full Verification view.
- Customer proof format version 7 exports the same four statuses for every
  mailbox. Cutover acceptance stays `not_established` and explains that it is a
  separate project-level approval and source-quiescence decision.
- Added regression assertions for aggregate-only evidence and updated provider
  evidence validation to accept proof version 7 while rejecting unknown future
  versions.
- Traced a failing hosted container integration build to the Docker builder
  omitting the compile-time `capabilities.toml`; added the missing input and a
  repository regression assertion.

Evidence: focused customer-proof Rust test, provider evidence Python tests
(15 passed, 1 skipped), `cargo fmt`, localization validation, and `git diff
--check` passed. `cargo clippy --locked --all-targets --all-features -- -D
warnings` passed. `cargo test --locked --all-targets --all-features` passed
(1,511 passed, 14 ignored, plus all integration and documentation targets).

## 2026-11-15: Immutable Rust toolchain action references

Branch `fix/ipv6-cooldown-and-stale-batch-summary`:

- Replaced `dtolnay/rust-toolchain@master` with its locked 40-character commit
  in CI, release, CodeQL, dependency-audit, and authenticated DAST workflows.
- Strengthened the workflow regression test to require the exact immutable ref
  and matching action-lock entry. The lockfile verifier reports all workflows
  valid.

Evidence: `cargo fmt --check`, the focused Rust workflow-lock regression test,
`gh actions-lock --verify` (all 10 workflows valid), and `git diff --check`
passed. `actionlint` is not installed in this work environment; CI's pinned
actionlint workflow gate remains the YAML syntax check.

## 2026-11-15: Data-driven provider qualification and explicit queue lookup errors

Branch `fix/ipv6-cooldown-and-stale-batch-summary`:

- Connected qualification display and `doctor` JSON to a shared model loaded
  from `capabilities.toml`; provider identifiers, date, engine, digest,
  limitations, and invalidation conditions are now sourced from one registry.
  Unknown custom-host routes fail closed, and a `qualified` entry is displayed
  green only when its date, engine, and 64-character evidence digest exist.
- `MailboxQueue::row_by_id` now returns `rusqlite::Result<Option<QueueRow>>`;
  the operations view distinguishes a missing mailbox from ledger read errors.
- The provider qualification table in release docs is regenerated with evidence
  digest and invalidation columns. Registry validation tests ensure unique route
  identifiers, explicit invalidation conditions, and provenance for any future
  qualified status.

Evidence: `cargo fmt --check`, `cargo check --locked --all-targets
--all-features`, `cargo clippy --locked --all-targets --all-features -- -D
warnings`, `cargo test --locked --all-targets --all-features` (1,511 passed,
14 ignored plus all integration/doc test targets), capability-claim generation
and validation, and UI localization validation passed.

## 2026-11-15: Background mailbox queue reads and visible provider qualification

Branch `fix/ipv6-cooldown-and-stale-batch-summary`:

- Moved mailbox filtering, paged row IDs, visible-row reads, queue totals, and
  selection summaries to a dedicated read-only SQLite worker for the desktop
  GUI. Requests carry project/filter/generation identity and stale responses
  are discarded. Selection IDs are independent of the currently loaded page.
- Queue read failures preserve prior selection state, display an unavailable or
  loading state rather than an empty result, and retry on a bounded interval.
- Added a persistent provider-pair qualification warning next to execution
  controls and in individual and batch live-migration confirmation reviews.

Evidence: `cargo fmt --check`, `cargo check --locked --all-targets
--all-features`, `cargo clippy --locked --all-targets --all-features -- -D
warnings`, `cargo test --locked --all-targets --all-features` (1,507 passed,
14 ignored plus all integration/doc test targets), and
`python3 scripts/verify-ui-localization.py` passed.

## 2026-11-15: IPv6 cooldown matching and stale batch selection recovery

Branch `fix/ipv6-cooldown-and-stale-batch-summary`:

- Command-center cooldown matching now uses the shared endpoint parser and
  covers IPv4, DNS, bare IPv6, and bracketed IPv6 endpoints with and without
  explicit ports.
- A failed SQLite selection-summary read retains the last successful view,
  exposes a stale-data warning, and retries after 500 ms. Filtering failures
  no longer erase the cached summary either.
- Completed typed IMAP LIST error propagation through folder discovery; the
  verification path no longer tags inventory limits by parsing error text.

Evidence: `cargo fmt --check`, `cargo check --locked --all-targets
--all-features`, `cargo clippy --locked --all-targets --all-features -- -D
warnings`, and `cargo test --locked --all-targets --all-features` passed
(1,611 passed, 14 ignored).
## 2026-11-14: Lease-loss drill for a crashed worker (M2)

Branch `test/lease-loss-drill` (from `main`). New test
`cli::control::agent::tests::a_killed_worker_leaves_its_lease_for_reconciliation`:

- The test runs the real control service over HTTP. The real agent loop
  runs in a child process (the test binary re-executed) with a stand-in
  engine, a `sleep` process.
- The agent leases the mailbox, starts the engine, and renews. The test
  then SIGKILLs the agent; the engine keeps running, as it can after a
  host crash.
- Checked: nothing renews the lease after the kill; a second worker gets
  nothing while it is live or after it expires; after the grace it is
  `needs_reconciliation`; the dead worker's late `complete` and `renew`
  are refused with `409`; after the operator stops the engine and
  requeues, the second worker leases it under a higher fencing token and
  completes it, and the dead worker's token stays refused.
- The 30-second lease and 60-second grace are fast-forwarded in the
  database. The test takes about 8 seconds and passed five consecutive
  runs with no processes left behind.

Not covered: a lost lease cancelling a real imapsync run through the
headless stop flag (see M2).

## 2026-11-14: Typed IMAP client errors (M7 step 1)

Branch `feat/typed-imap-errors` (from `main`):

- New `imap_probe::ImapError { kind, detail }`. The detail is the text the
  client reported before, so durable details and transcripts are unchanged.
- Typed at the source: protocol reads and writes, the TLS connect and
  handshake (rustls certificate errors are told apart from other TLS and
  I/O failures), DNS and connect racing, authentication, readiness, the
  fetch budget, and verification limits.
- `SidedProbeError` carries the typed error, and the batch worker classifies
  it with `ClassifiedFailure::imap`.

Behaviour changes, each covered by a test:

- Client-side timeouts (read, connect, TLS handshake, a stalled response,
  the probe's total deadline) were `unknown` and never retried, because
  their text did not say "timed out". They are now transport failures
  and are retried.
- "Server greeting was missing or invalid" was classified as configuration
  because of the word "missing". It is now a protocol failure, not retried.
- During verification, a stalled response or one cut off at its size bound
  was treated as a folder failure, and the next folder reused the stream
  with that response still unread. A stall now ends the session (reconnect),
  and a size-limit cut ends the account check.

Evidence: `make check` and `make test` pass locally.

## 2026-11-09: Production-readiness audit (R5)

Audited `main` at `9bd15639`. Findings and verdicts are in DEVELOPMENT_PLAN.md
under R5. M8.3 and M8.4 are done in the same change; M8.2 is not started.

Evidence: see the gate results on the pull request. New regression tests
cover the TLS and plain deadlines, slot release after a panic, 503 refusals,
duplicate assignments, assignment paging, upload ownership, backup and its
refusal of a tampered chain, the v6 to v7 migration, and credential pruning.

Not testable here: a live IdP or multi-host control-plane drill,
`doctor --connect` against real provider servers, and the live-provider
qualification listed under "Blocked on people".

## 2026-11-08

### M8.1 Partial-fetch body hashing

Branch `feat/partial-fetch-body-hash` (from `main`):

- Pages request `BODY.PEEK[]<0.262144>`; bodies that fill the prefix
  continue in 4 MiB ranges, hashed incrementally (`BodyProgress`).
- Range responses must name the requested UID and origin; `""`/`NIL` past
  the end is an empty range; a whole `BODY[]` in reply to a range is
  refused, but a whole body in reply to the page prefix is accepted.
- The per-message bound rose to 2 GiB. Tests cover bodies at and around
  both boundaries, `NIL` endings, servers that ignore ranges, the bound
  tripping mid-body, and the parser edge cases; the fuzz target gained the
  prefix-plus-range digest invariant.

## 2026-11-07

### Paged plan upload (M8 item 5, decided: paged upload)

Branch `feat/paged-plan-upload` (from `main`):

- Plan mailboxes are rows (`plan_mailboxes`); version 6 migrates existing
  plans in SQL with `json_each`, keeping their approval.
- Paged upload: start against a revision, idempotent numbered pages of up
  to 2,000 entries, and a commit that checks count and revision and swaps
  the plan in one transaction. Limit: 1,000,000 mailboxes.
- `control plan upload` drives it, retrying failed pages; tested over HTTP
  with 4,500 mailboxes.
- Leases carry only the leased mailbox's entry; queuing an unlisted job ID
  is refused.

### R4. Scale and usability review

Branch `fix/scale-review` (from `main`):

- Fixed the stale "worker agent still to come" text in
  `docs/control-plane.md`.
- The supervisor prunes backoff with a set instead of `Vec::contains`.
- With body verification requested, the account test counts messages
  larger than the per-message bound, and preflight blocks before the
  transfer.

Verdicts for every finding, and the M8 plan for streaming verification,
are in [DEVELOPMENT_PLAN.md](DEVELOPMENT_PLAN.md#r4-scale-and-usability-review-2026-11-07).

### R3. Control-plane review findings

Branch `fix/control-plane-review` (from `main`). One commit per finding:

- Plans and results are allowlisted documents; unknown keys are refused.
- Audited reads return 500 and no data if the audit write fails.
- The HTTP body limit is twice the 256 KiB plan limit.
- The agent keeps renewing its lease until the completion is
  acknowledged.
- OIDC sign-in initiation is limited to 20 pending sign-ins per client
  (control-plane database version 5); quota refusals are 429.
- The agent leases nothing while another controller owns its ledger.

Then the review's §8 follow-ups, on the same branch:

- Login failures lock out the guessing client, not the account (50 across
  all clients lock the account); optional IdP-first sign-in.
- `/readyz` readiness check beside `/healthz`.
- OIDC fetches no longer hold the metadata cache lock.
- Signed audit checkpoints to publish outside the service, checked by
  `audit-verify --checkpoint`.
- A concurrency benchmark: about 1,400–1,500 audited requests/s on NVMe.

Verdicts are in [DEVELOPMENT_PLAN.md](DEVELOPMENT_PLAN.md#r3-control-plane-review-findings-2026-11-07).

### CI: yanked async-executor

PR #166 updates `async-executor` 1.14.0 → 1.14.1. The yanked version
failed `cargo deny` in CI and in the scheduled audit.

## 2026-10-30

### Review L-02: correctness metrics

Branch `feat/integrity-metrics` (from `main`).

- Added `mailswiftsync_attention_mailboxes{reason}`, with labels limited to
  the durable attention reasons and anything else exported as `other`.
- Added `mailswiftsync_verification_compared_source_messages` and
  `mailswiftsync_verification_exact_message_matches`, for exact coverage
  from metadata or body evidence only. Native Dovecot and aggregate
  evidence are excluded and tested as excluded.
- Status summaries carry the two counts, declared in both automation
  fixtures.

### M4. Verification beyond 1,000,000 messages: done at 3M

Branch `feat/durable-verification-above-1m`, stacked on
`feat/batch-verification-capacity` (PR #154).

- Opt-in proof (release build):
  `a_durable_stage_verifies_an_account_above_the_ephemeral_limit` passed.
  It ran 1,000,100 messages per side, staged in 39.6 s, with 100% exact
  coverage; the ephemeral stage refused at 1,000,000.
- `scripts/benchmark-verification-scale.sh`, typical shape, on an idle
  workstation:
  - 100k, 1M, 2M and 3M met their budgets.
  - 5M (18,738 msg/s) and 10M (16,863 msg/s) missed the 20,000 msg/s rate
    budget, with peak RSS of 17 MiB and exact results.
  - An earlier run was invalid: another project's build pushed the load
    average to 28, and the stage sat on a 7 GB tmpfs. A reboot also
    interrupted one 10M run.
- Not measured: real-provider fetch time at this scale.

### M3. Per-row verification capacity for batch imports: done

Branch `feat/batch-verification-capacity` (from `main`).

- Gate: `make check` passed; `make test` reported 1,552 passed, 0 failed,
  12 ignored; the Windows clippy cross-check passed.
- New tests:
  - `each_batch_mailbox_gets_the_single_plan_capacity_row` covers passed,
    blocked, and unknown, including a failed probe.
  - `batch_capacity_is_not_probed_where_it_cannot_apply` covers plain IMAP
    and Dovecot.
  - `a_dry_run_mailbox_records_its_capacity_row_without_secrets` covers
    redaction and cancellation.
  - `a_batch_dry_run_records_each_mailboxs_verification_capacity` covers
    ledger persistence.
  - `the_latest_verification_capacity_row_per_mailbox_is_counted` covers
    the status counts.
  - The metrics test checks the new gauge.
- Not tested here: a batch dry run against real IMAP servers. The IMAP
  integration lab in CI runs batch dry runs, but it does not assert the
  capacity row.

### M1. CodeQL alert #95: done

The owner dismissed the alert as a false positive. GitHub shows no open
code-scanning alerts.

## 2026-10-29

### R1. External architecture review

See the table in DEVELOPMENT_PLAN.md for each verdict.

- `fix/batch-claim-and-retry-durability` (from `main`), commit `d577e8e1`
  (S1, C2, C3).
  - Gate: `make check` passed; `make test` reported 1,483 passed, 0
    failed, 12 ignored; the Windows clippy cross-check passed.
  - In one earlier full run, `imap_probe::resolver::tests::hung_helper_is_killed_at_the_deadline_or_on_cancellation`
    failed with "helper never started". The test has a 10-second startup
    wait, and that code was not changed. It then passed 5 of 5 isolated
    runs and the full rerun.
- This branch (S2):
  - Store tests cover each case: the 60-second reassignment grace, a
    revoked worker's lease held until expiry plus grace, and a revoked
    worker refused at renew and complete.
  - The HTTP test now expects a revoked worker's lease to stay `leased`.
- Not testable here: the real duration of an engine stop under load, which
  the 60-second grace is meant to cover. It needs the Docker IMAP lab.

### M2. Control-plane worker agent: done (live drill outstanding)

Branch `feat/control-plane-worker-agent`, stacked on
`feat/control-plane-identity` (PR #148). Not pushed.

What changed:
- `src/cli/control/agent.rs`: `control worker plan`, `control worker
  enroll`, and `control worker run`. Plan digests bind central approval to
  each local batch row, renewals run in the background, and a lost lease
  cancels the local run without reporting.
- `src/headless.rs`: `headless_worker_execute` runs one durable batch row
  for a given ledger project. An optional stop flag cancels the run through
  the controller's existing cancel flag, then drains it, so no engine
  outlives the call and the result is never success.
- docs/control-plane.md ("Running a worker agent"), `capabilities.toml`,
  regenerated PRODUCTION_STATUS.md, and CHANGELOG.

Test evidence:
- 9 new tests in `cli::control::agent::tests`:
  - argument parsing (plain HTTP refused off loopback)
  - digest framing
  - plan export against a real ledger file, plus binding refusals:
    unknown row, other project, wrong format, a local plan edited after
    approval
  - completed and failed reporting
  - a mismatch never runs the executor
  - a refused renewal (409 or 401) stops the run and reports nothing
  - an unreachable service stops the run before the lease expires
  - a transient reporting failure is retried, and a persistent one is
    surfaced
  - an end-to-end lease, run and completion over real HTTP against the
    in-process server
- Final gate, chained with `&&`:
  - `make check` passed.
  - `make test` reported 1,527 passed, 0 failed, 12 ignored (main suite:
    1,422 passed).
  - The Windows clippy cross-check (`x86_64-pc-windows-gnu`, `-D warnings`)
    passed.
- Real-binary smoke on loopback:
  - `control init`/`serve`, login, tenant, and enrollment work.
  - `worker enroll` writes a `0600` credential; reusing the token is
    refused.
  - `worker run --once` exits 0 with no queued work.
  - `audit-verify` reports the chain intact.

Not testable here:
- A live batch run cancelled by lease loss. It needs the Docker IMAP
  integration lab and a GUI-imported batch, and neither is available in this
  environment. The stop path reuses the cancel flag the GUI Cancel button
  sets.

### M1. CodeQL alert #95 on PR #148: blocked

The alert is a false positive: SHA-256 over a random API bearer token, not
a password. Dismissing it needs the repository owner, because auto mode
refused the API call.

### M0. Baseline: done

On `feat/control-plane-identity` at `a93ad737`:
- `make check` passed.
- `make test` reported 1,518 passed, 0 failed, 12 ignored (main suite:
  1,413 passed).

## Next steps

1. Drill a lost lease cancelling a real imapsync run in the IMAP
   integration lab. The crashed-worker drill is done; this one needs a
   durable batch in the lab, and batch import is GUI-only.
2. Push the branch and open a PR stacked on #148 (needs approval).
3. M3 (per-row batch verification capacity) once PR #147 is merged.
4. M4 (segmented verification beyond 1M messages per account).
