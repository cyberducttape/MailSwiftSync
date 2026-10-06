# Durability and real-engine concurrency validation

The existing controller and storage-fault labs validate process loss, ledger
write failures, corrupted backups, and destination storage faults. They do not
simulate a host or VM power cut. A process kill is useful evidence, but it
does not exercise filesystem write ordering, journal recovery, or boot-time
recovery.

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
