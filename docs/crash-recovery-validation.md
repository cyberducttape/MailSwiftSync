# Crash-recovery validation matrix

MailSwiftSync does not keep its own per-message transfer journal. Recovery of
an interrupted transfer relies on the engine's idempotent resynchronization
(imapsync reruns and skips messages already present; native Dovecot resumes
from a UIDVALIDITY-bound state token), followed by independent verification.
This document records which interruption scenarios are proven, by what
evidence, and which are still open. It is the release-evidence checklist for
the "Checkpoint persistence per message" gap in the
[capability manifest](../CAPABILITY_MANIFEST.md).

The acceptance requirement for every scenario: **resume must not silently skip
mail, create unexplained duplicates, or mark incomplete reconciliation as
successful.** For each executed scenario, retain the source and destination
inventories, the ledger state before and after the interruption, the process
state, and the final verification result.

Building a second, product-owned transfer journal is deliberately deferred:
the existing engine resumption and verification path must first be proven safe
and repeatable, and a new journal would add its own failure states.

## Matrix

| # | Scenario | Current evidence | Status |
|---|---|---|---|
| 1 | Kill the MailSwiftSync controller while imapsync is copying a large message | `scripts/controller-recovery-smoke.sh` kills the controller during a running engine (deterministic blocking engine) and proves running state is recovered into Attention with no orphaned process ownership. Process identity checks (`process-supervision-chaos-smoke.sh`) prove the launcher never runs an engine before durable registration. The packaged IMAP lab waits for the 64 MiB message's Maildir `tmp` file to exceed 1 MiB while remaining incomplete before SIGKILL; the configured 1 MiB/s value is imapsync's average run-level target, not per-APPEND socket shaping. On Ubuntu 24.04 with imapsync 2.314 and Dovecot 2.3.19.1, CI at `e86971b1b3e2838fd0fb39fbbfa800a9a823818e` killed the controller after observing 3,145,263 of 67,109,132 bytes. The engine stopped with its controller, and a fresh pass resumed to exactly one independently verified message. This rerun follows the reproduced failure at `6f714fb`, where 8,388,143 bytes were observed before the server accepted the full body and imapsync outlived the 20-second deadline; Linux crash-only parent-death handling now uses SIGKILL and checks the fork/pre-exec race. | **Proven in CI for this packaged fixture.** Provider-specific behavior, larger-message scenarios, and host power-loss behavior remain unqualified. |
| 2 | SIGKILL the engine while the destination is receiving data | Unit and runner tests classify an engine killed by a signal as a failed attempt. The interruption drill then runs `recover` and a fresh live pass: with the default header-matching plan the destination ends with exactly the 60 source messages, verified; with expert `--useuid --usecache` the in-flight message is appended twice (61) and verification reports the difference instead of a clean result. | Proven in CI (integration workflow) for both plans. The UID-cache duplicate is documented on the plan option. |
| 3 | Destination write failure during APPEND (`RLIMIT_FSIZE` simulation) | `scripts/engine-storage-fault-smoke.sh` runs only the destination IMAP worker under `RLIMIT_FSIZE`; this simulates a bounded file-write failure, not an actually full filesystem. A small message transfers, an oversized message is rejected, the ledger remains readable, and the run is surfaced as failed rather than clean. In the packaged integration workflow on PR head `16fa5132fb6622543eba6b6aa15fbf01898ffe4e` (merged as `202f7c949f4b98331bea770d8cd88152b3b92415`), run `38515348245` used imapsync 2.314 and Dovecot 2.3.19.1; the fixture confirmed all four conditions and no false clean success. `controller-chaos-smoke.sh` separately covers a storage limit during ledger writes and corrupt/truncated restores. | Proven for the stated RLIMIT_FSIZE fixture in CI; a real filesystem-full campaign remains unqualified. |
| 4 | Restart the host immediately after a committed transfer checkpoint | Terminal results commit atomically with evidence and checkpoints; startup recovery reconciles recorded process identities. `controller-recovery-smoke.sh` covers restart after a crash. | Partial: no power-loss or host-reboot test; SQLite WAL durability is relied on. |
| 5 | Source-folder changes after transfer but before verification | Verification cursors record each folder's SELECT snapshot (UIDVALIDITY, UIDNEXT, EXISTS); a changed snapshot forces a rescan, and changes between scans appear as differences rather than as a clean result (`src/imap_probe_resume_tests.rs`: expunge-and-deliver, new arrivals, flag-only changes). | Proven at unit level against scripted IMAP servers; not yet against a live provider. |
| 6 | Rotate OAuth credentials while a migration is disconnected | Unit tests cover refresh-command construction, token-file refresh on native-verifier reconnect, serialized concurrent refreshes, keyring/config rotation recovery, and refusal to overwrite a newer operator value. The packaged imapsync lab forces an IMAP socket drop after OAuth login and verifies both callbacks run before reauthentication and imapsync rereads the callback-written token files. The live-provider smoke still uses static initial access-token files; it does not exercise provider token expiry during transfer or refresh-token rotation. Recovery records are not encrypted and rely on owner-only filesystem protection. | **Open:** requires a short-lived-token expiry test during a real transfer against disposable Google/Microsoft tenants, including provider reconnect and rotated refresh-token persistence. |
| 7 | Restore the ledger after a crash and resume a partially completed mailbox | `controller-chaos-smoke.sh` proves `restore` rejects a corrupt or truncated backup. The IMAP lab's default-plan interruption drill kills the controller mid-transfer, recovers the run, backs up and restores the ledger to a new path, resumes the mailbox, and requires exactly the source message count plus verified state. | Proven in CI (integration workflow) for the default header-matching plan; this does not establish per-message checkpoints or host power-loss durability. |
| 8 | Verification staging reaches SQLite's page limit (`SQLITE_FULL`) | `core::message_staging::tests::sqlite_full_rolls_back_page_and_retains_durable_stage_for_recovery` caps the stage's SQLite page count, forces a staging write to fail with `SQLITE_FULL`, verifies the page transaction rolled back, reopens the retained stage, restores the page limit, and stages a later page. This is deterministic SQLite-level fault injection, not an actually full filesystem or an end-to-end controller/evidence-commit failure. | Proven by Rust unit test; real filesystem exhaustion and full-run recovery remain unqualified. |

### Current-head local process and ledger fault drills

On 2026-10-17, the repository's `process-supervision-chaos-smoke.sh`,
`controller-recovery-smoke.sh`, and `controller-chaos-smoke.sh` all passed
against the normal debug binary built from commit
`43f40ca87b77e2f579f5566cf81c4c413fa79938` (SHA-256
`d1398608817ba712f38fb41f2e750f1739f914d0520463173d176c015e95f6a5`). The
host was Ubuntu 26.04.1 x86_64, Linux 7.0.0-38-generic, with Rust 1.92.0.
The process-supervision drill proved that missing and invalid durable-release
tokens do not start the fake engine, while `GO\n` does. The recovery drill
SIGKILLed the controller and separately the engine process group; restart
classified interrupted work as Attention/failed and cleared active process
ownership. The ledger drill injected `RLIMIT_FSIZE` during initial ledger
creation, confirmed the controller failed rather than reporting success, then
reopened a valid ledger after the limit was lifted; it also rejected truncated
and corrupt restores without replacing the live ledger and restored a verified
backup over a deliberately corrupted copy.

These were local disposable-fixture runs using a fake shell engine, not
imapsync or Dovecot integration. `RLIMIT_FSIZE` is a bounded write-failure
simulation, not a physically full filesystem. They do not simulate OS restart,
power loss, kernel panic, or storage-controller cache loss, and do not replace
the packaged CI and release-host qualification required above.

### Latest packaged integration evidence

On 2026-10-16, packaged product integration jobs passed for current PR head
[`7766f2c6`](https://github.com/cyberducttape/MailSwiftSync/commit/7766f2c6e6332f87fc0ebfb9fb00e69e65923a60)
in [CI run 38906210211](https://github.com/cyberducttape/MailSwiftSync/actions/runs/38906210211).
Both jobs used Dovecot 2.3.19.1. The packaged imapsync path used imapsync
2.314 (SHA-256
`dda1a7380cdab8c8bfc981724ab84ef1f5b18d630221df8d0517fed708271875`):
the exported Level 2 metadata evidence reconciled 16 messages after the
initial pass and 17 after the final delta, with 100% flag comparison and zero
missing, extra, modified, failed, or unmatched messages. Its OAuth callback,
controller crash/recovery, controller storage-fault/corruption, and engine-side
destination storage-fault steps also passed. The native Dovecot path reconciled
16 and then 17 messages by aggregate counts only; it produced no per-message
metadata or body evidence, and its engine version was recorded as unknown.
The customer-proof digests for all four initial/final exports were accepted by
the application's `verify` command. Both paths correctly exported
`cutover_readiness: not_established`.

The earlier [CI run 38903674326](https://github.com/cyberducttape/MailSwiftSync/actions/runs/38903674326)
also passed these integration jobs for commit `bce333cb`; it is retained as
the initial recorded evidence set.

These artifacts demonstrate the stated disposable Dovecot fixture only. They
are not real-provider qualification, do not prove content equality, do not
establish a synchronized cross-server snapshot or cutover readiness, and do
not cover host power loss or a physically full filesystem. The downloadable
`imap-integration-evidence` and `native-dovecot-integration-evidence`
artifacts in the linked run contain the raw customer-proof exports and
environment summaries.

## Related protections

- A ledger invariant rejection of a terminal result no longer retries forever:
  the run is recorded as failed and the mailbox as Attention with the named
  reason, so an inconsistent result is reviewed rather than presented as
  verified.
- Credential directories are owned by a cleanup guard from creation, so a
  command-preparation error cannot leave credentials behind; startup removes
  abandoned runtime directories once process ownership is proven.

## To close before 1.0

1. Qualify larger message sizes and supported release-host storage profiles.
2. Run scenario 6 during live provider qualification.
3. Record a host-reboot and power-loss drill for scenario 4 on the release host
   class.
