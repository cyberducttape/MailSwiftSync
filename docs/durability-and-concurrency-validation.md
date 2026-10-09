# Durability and real-engine concurrency validation

The existing controller and storage-fault labs validate process loss, ledger
write failures, corrupted backups, and destination storage faults. The
`scripts/process-supervision-chaos-smoke.sh` lab additionally qualifies the
production gatekeeper handshake: an internal launcher refuses to execute an
engine without the exact durable-release token, rejects invalid tokens, and
executes only after release. The controller recovery lab covers parent loss,
and the native process tests cover PID identity checks and Windows Job Object
containment. These are process-level proofs; they do not simulate a host or
VM power cut. A process kill is useful evidence, but it does not exercise
filesystem write ordering, journal recovery, or boot-time recovery.

## Power-loss campaign

Run this campaign on an ephemeral VM with disposable source and destination
mail systems. Start a migration, wait for a durable running state and at least
one committed progress/evidence boundary, then cut power through the
hypervisor rather than sending a process signal. After reboot:

1. Open the ledger and run integrity checks before recovery.
2. Run `mailswiftsync recover` and record every resulting operator-attention
   or resumable state.
3. Resume the migration and verification using the normal workflow.
4. Compare the final ledger, evidence, mismatch counts, certificate digest,
   and destination inventory with a clean control run.

Vary the cut point across admission, process registration, engine transfer,
verification staging, evidence commit, backup creation, and atomic export.
Repeat hundreds of cuts per supported filesystem/runtime class, retain the VM
image, ledger backup, recovery JSON, and final comparison for every failure,
and fail the campaign on silent completion, unreadable state, unexpected data
loss, or evidence stronger than the observed coverage.

Until this campaign has run, the release claim is limited to controller
crash/process-loss and storage-fault recovery. It must not be described as
power-loss tested.

## Process-supervision qualification matrix

Run the process supervision lab with the packaged production binary before an
unattended release:

| Fault point | Required result | Coverage |
| --- | --- | --- |
| Launcher stdin closes before registration/release | Engine never starts | `process-supervision-chaos-smoke.sh` |
| Invalid release token | Engine never starts and launcher exits nonzero | `process-supervision-chaos-smoke.sh` |
| Valid release token | Engine starts only after release; controller recovery covers recorded ownership | `process-supervision-chaos-smoke.sh` plus controller recovery lab |
| Controller SIGKILL during a running transfer | Durable run remains for recovery; no false completion | `controller-recovery-smoke.sh` |
| Ledger write limit/corruption | Operation fails closed; ledger remains recoverable or restoreable | `controller-chaos-smoke.sh` |
| PID reuse or identity mismatch | Existing process is never signalled as the old engine | Rust process-identity tests |
| Windows parent termination | Descendants terminate with the Job Object | Windows native CI qualification |
| Host power loss | Integrity, recovery, resume, and evidence are consistent | Required ephemeral-VM campaign above |

The first seven rows are automated or CI-qualified where noted. The final
row remains a release blocker and cannot be inferred from process-level tests.

## Real-engine concurrency matrix

The single-mailbox 100k-message scale gate measures a meaningful envelope but
does not measure simultaneous imapsync memory, file descriptors, page cache,
or provider pressure. The next disposable-IMAP campaign should run at least:

| Workers | Mailboxes | Messages/mailbox |
| ---: | ---: | ---: |
| 1 | 1 | 100,000 |
| 4 | 20 | 10,000 |
| 8 | 50 | 10,000 |
| 16 | 100 | 5,000 |

Each row should record controller RSS, summed engine RSS, peak page cache,
open file descriptors, process count, SQLite transaction latency, queue depth,
CPU utilization, verification elapsed time, provider responses/throttles,
engine crashes, retries, and final evidence outcomes. Run uniform and
folder-heavy/large-attachment distributions, and repeat with verification
enabled. Controller and engine resource budgets must be reported separately;
a passing controller RSS number does not establish that the host can safely
run the selected number of real engines.

The current 100k-message workflow remains valid evidence for its tested single
engine envelope. It is not evidence for 100 simultaneous mailboxes or provider
qualification.
