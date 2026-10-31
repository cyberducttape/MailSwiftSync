# Progress

Work log for [DEVELOPMENT_PLAN.md](DEVELOPMENT_PLAN.md). Newest first.

## 2026-10-30

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

1. Run a live lease-loss drill for M2 in the IMAP integration lab (needs
   Docker).
2. Push the branch and open a PR stacked on #148 (needs approval).
3. M3 (per-row batch verification capacity) once PR #147 is merged.
4. M4 (segmented verification beyond 1M messages per account).
