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
| 1 | Kill the MailSwiftSync controller while imapsync is copying a large message | `scripts/controller-recovery-smoke.sh` kills the controller during a running engine (deterministic blocking engine) and proves running state is recovered into Attention with no orphaned process ownership. Process identity checks (`process-supervision-chaos-smoke.sh`) prove the launcher never runs an engine before durable registration. | Partial: proven with a blocking engine, not mid-APPEND of a large message against a real server. |
| 2 | SIGKILL the engine while the destination is receiving data | Unit and runner tests classify an engine killed by a signal as a failed attempt; verification of a rerun detects missing or extra messages. | Open: no lab kills a real imapsync mid-transfer and then verifies the rerun. |
| 3 | Full destination filesystem during APPEND | `scripts/engine-storage-fault-smoke.sh` runs the destination IMAP worker under `RLIMIT_FSIZE`, so an oversized message fails partway while a small one succeeds; the run is surfaced as failed, never clean. `controller-chaos-smoke.sh` covers the ledger itself hitting a storage limit. | Proven in CI (integration workflow). |
| 4 | Restart the host immediately after a committed transfer checkpoint | Terminal results commit atomically with evidence and checkpoints; startup recovery reconciles recorded process identities. `controller-recovery-smoke.sh` covers restart after a crash. | Partial: no power-loss or host-reboot test; SQLite WAL durability is relied on. |
| 5 | Source-folder changes after transfer but before verification | Verification cursors record each folder's SELECT snapshot (UIDVALIDITY, UIDNEXT, EXISTS); a changed snapshot forces a rescan, and changes between scans appear as differences rather than as a clean result (`src/imap_probe_resume_tests.rs`: expunge-and-deliver, new arrivals, flag-only changes). | Proven at unit level against scripted IMAP servers; not yet against a live provider. |
| 6 | Rotate OAuth credentials while a migration is disconnected | Refresh before launch, during qualified imapsync reconnects, and before verification; rotated refresh tokens are persisted back to the keyring or the run fails closed (unit tests). | Open: requires live Google/Microsoft tenants. |
| 7 | Restore the ledger after a crash and resume a partially completed mailbox | `controller-chaos-smoke.sh` proves `restore` rejects a corrupt or truncated backup and that restoring a verified backup returns the ledger to normal operation; migration backups are taken before schema upgrades. | Partial: no lab resumes a partially transferred mailbox from a restored ledger and verifies the result. |

## Related protections

- A ledger invariant rejection of a terminal result no longer retries forever:
  the run is recorded as failed and the mailbox as Attention with the named
  reason, so an inconsistent result is reviewed rather than presented as
  verified.
- Credential directories are owned by a cleanup guard from creation, so a
  command-preparation error cannot leave credentials behind; startup removes
  abandoned runtime directories once process ownership is proven.

## To close before 1.0

1. Add real-engine interruption labs for scenarios 1 and 2: interrupt a
   transfer of a large message, rerun, and require a clean independent
   verification with no duplicates.
2. Add a restored-ledger resume lab for scenario 7.
3. Run scenario 6 during live provider qualification.
4. Record a host-reboot drill for scenario 4 on the release host class.
