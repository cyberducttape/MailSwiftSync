# Progress

Work log for [DEVELOPMENT_PLAN.md](DEVELOPMENT_PLAN.md). Newest first.

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
