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
mail systems. Use hypervisor power cuts, not process signals, at each boundary
below. Record the exact artifact digest, filesystem and mount options, SQLite
settings, cut point, and VM image for every trial.

| Cut point | Required post-reboot behavior |
| --- | --- |
| Before durable run admission; after admission but before process registration | No engine starts without durable ownership; an admitted but unstarted run is recoverable or explicitly requires review. |
| After process identity registration but before launcher release; immediately after release | No unowned engine is mistaken for a successful run; recovery either proves process identity and ownership or leaves the run for operator review. |
| During active transfer and while a Dovecot checkpoint is being written | No false terminal success; source/destination state and checkpoint identity are revalidated before resume. Uncertain in-flight messages are reconciled before completion. |
| During temporary runtime-secret creation, use, and cleanup | No secret is treated as valid after an ambiguous partial write; abandoned secret material is detected and handled by the recovery procedure. |
| During verification-page/cursor commit and terminal evidence transaction | SQLite integrity holds; page/cursor and evidence transactions are wholly committed or rolled back, with no stronger claim than recovered rows support. |
| During checkpoint synchronization, backup creation, report signing, and atomic export | The prior durable artifact remains intact or the new artifact is complete and verifiable; partial output is never presented as a valid certificate. |

After each reboot, open the ledger and run integrity checks before recovery;
run `mailswiftsync recover` and record every operator-attention or resumable
state; resume only when ownership and checkpoint provenance are established;
then compare the ledger, evidence, mismatch counts, certificate digest,
secret-file state, and destination inventory with a clean control run.

Acceptance: every interrupted job is safely repeatable, recoverable, or
explicitly marked for manual intervention. No trial may report success from
process exit alone, reuse an ambiguous checkpoint, or claim evidence stronger
than recovered observations support. Retain the VM image, ledger backup,
recovery JSON, and final comparison. Exercise every boundary repeatedly across
supported filesystem/runtime classes; process kills and SQLite unit tests are
not substitutes for this campaign.

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
| Descendant retains stdout/stderr pipes, repeated across captures | Reader drain returns within its deadline; error reports active readers, cumulative detached-reader count, and cumulative drain timeouts. Release test descendants and confirm their reader threads exit rather than accumulating. | Linux inherited-pipe integration test and repeated reader-counter unit test; Windows pipe/job behavior still requires native qualification |
| Ledger write limit/corruption | Operation fails closed; ledger remains recoverable or restoreable | `controller-chaos-smoke.sh` |
| PID reuse or identity mismatch | Existing process is never signalled as the old engine | Rust process-identity tests |
| Windows parent termination | Descendants terminate with the Job Object | Windows native CI qualification |
| Host power loss | Integrity, recovery, resume, and evidence are consistent | Required ephemeral-VM campaign above |

The first seven rows are automated or CI-qualified where noted. The final
row remains a release blocker and cannot be inferred from process-level tests.
Reader counts are process-local diagnostics: `active` is a live gauge,
`detached_total` and `drain_timeouts_total` are cumulative for the controller
lifetime, and each drain failure reports their values. They are not a durable
fleet metric or an automatic paging integration. Unix process groups and
Windows Job Objects bound supervised descendants differently; a pipe retained
by a process outside that ownership boundary may outlive either supervisor.

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
