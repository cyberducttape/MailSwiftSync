# Development plan

Working plan for taking MailSwiftSync from Technical Preview (`0.1.0-alpha.1`)
to a stable release. The release bar itself is defined in
[docs/release-readiness.md](docs/release-readiness.md) and the capability
inventory in [capabilities.toml](capabilities.toml); this file orders the work
and states how each milestone is judged done. Progress and evidence are logged
in [PROGRESS.md](PROGRESS.md).

## Where the release stands (2026-10-29)

- Code-side gates are green: `make check`, the full test suite, the
  cross-platform CI matrix, the IMAP integration lab, fuzzing, and the
  dependency audit.
- No open GitHub issues. Two open PRs: #147 (pre-run verification capacity)
  and #148 (control plane identity, stacked with workers and OIDC).
- Most remaining 1.0 blockers are **external**: they need signing
  credentials, live provider tenants, or a person with a screen reader. They
  are listed under "Blocked on people" so they are not mistaken for code work.

## Milestones (highest priority first)

### M0. Baseline gates green: done

Done when:

- `make check` exits 0.
- `cargo test --locked --all-targets --all-features` reports 0 failures.

### M1. Clear the open CodeQL alert on PR #148: blocked

Alert #95 (`rust/weak-sensitive-data-hashing`) flags SHA-256 over the return
value of `create_api_key` in a test. The value is a 256-bit random bearer
token, not a password; passwords use Argon2id. Done when the alert is
dismissed as a false positive with that justification. Dismissing it needs
the repository owner (auto mode refused the API call).

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

### M3. Per-row verification capacity for batch imports

PR #147 checks verification capacity only in the single-plan account test.
Batch rows are not probed before admission. Done when batch preflight reports
the same blocked/unknown/passed capacity row per mailbox, with tests for
each outcome. Depends on PR #147 merging.

### M4. Segmented verification beyond 1,000,000 messages per account

Verification refuses exact evidence above the per-endpoint ceiling and
records `verification_limit_exceeded`. Done when an account above the
ceiling is verified in durable segments with an exact coverage percentage,
proven by a synthetic scripted-server test above the ceiling and the 100k
benchmark budgets staying within their limits.

### M5. Durable per-message transfer journal (review finding 3)

imapsync is external, so per-message progress has to be observed from its
output. Needs a scope decision from the maintainer before work starts:
journal granularity, retention, and whether resume may skip work based on it.

## Blocked on people (not code work)

| Item | Needs |
|---|---|
| Signed release artifacts | GPG, Authenticode, Apple Developer ID secrets in CI |
| Live provider qualification (Gmail, Microsoft 365, Fastmail) | Disposable tenants and accounts |
| OAuth token expiry during a real transfer | Disposable Google/Microsoft tenants |
| Screen-reader passes (Narrator, NVDA, VoiceOver, Orca) | A person on each platform |
| Signed APT/YUM repositories | Hosting and the release key |
| Published representative-scale migration evidence | Representative datasets and a release host |
| Merging PRs #147 and #148 | Maintainer review |
