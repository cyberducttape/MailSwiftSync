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
