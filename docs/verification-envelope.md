# Verification envelope

This document separates implementation admission limits from measured scale
and provider qualification. A configured ceiling is not a claim that every
provider, mailbox shape, or host has been qualified at that size.

| Mode | Enforced implementation envelope | Evidence currently available |
| --- | --- | --- |
| Metadata reconciliation | Up to 1,000,000 messages on each endpoint across all selectable folders; up to 100,000 selectable folders and 32 MiB of folder inventory per endpoint. Fetch-page transient state has an estimated 256 MiB source/destination-pair budget. Durable runs stage mismatch rows and stream them into the ledger; ephemeral runs retain an estimated 64 MiB in-memory mismatch cap. Before scanning, every staged page, materialized reconciliation phase, and every 256 persisted mismatch rows, the stage filesystem must report at least 512 MiB free. | Release-mode durable SQLite reconciliation was measured at 100,000 messages per endpoint (200,000 staged records total) in 1.286 s on the synthetic benchmark, including about 2% changed-ID cases. This measures reconciliation, not IMAP fetching, report persistence, or provider qualification. No maximum aggregate metadata GiB value has been established; the 512 MiB floor is a last-resort high-watermark, not a full-workload disk estimate or reservation, and concurrent writers can race it. Operators still need target-host capacity planning and independent filesystem monitoring. |
| Body fingerprint proof | Durable SQLite-backed verification supports the general 1,000,000-message-per-endpoint limit; ephemeral in-memory verification retains a 100,000-message-per-endpoint ceiling. Each body is bounded by 8 MiB by default (configurable up to 64 MiB); the entire tagged FETCH response is also capped at 64 MiB, including protocol overhead. The shared source-plus-destination hash-byte budget is 512 MiB by default and can be configured up to 8 GiB. Durable reconciliation uses staged indexes and bounded Rust batches, but its disk amplification and large-scale latency have not yet been qualified. | Bounded and fail-closed in code, but not production-scale or provider-qualified. The configured body-byte budget is independent of message count and does not establish a memory or disk-capacity guarantee. |

The free-space check runs before network scanning, each fetched-page commit,
major SQLite materialization phases, and every 256 persisted mismatch rows.
It runs between operations: an individual SQLite operation may consume space
before the next check. It does not reserve space or bound aggregate stage growth,
terminal evidence persistence, WAL/journal activity, or report export. Keep
monitoring the entire filesystem and leave capacity for those phases; the guard
reduces one failure mode but cannot guarantee that the disk will not fill.

The latest release-mode durable-stage benchmark on a local real-disk
workspace (100,000 synthetic messages per endpoint, typical distribution)
completed staging in 1.64 s and reconciliation in 1.87 s. It observed about
22 MiB peak RSS, a 102 MiB peak stage directory during reconciliation, a
9.3 MiB rollback-journal peak, and 14.4 MiB of reconciliation writes; the
restart reconciliation completed in 1.89 s. This is a repeatable engineering
measurement of SQLite reconciliation, not a provider qualification or a
10/50/100 GiB mailbox result. Hardware, filesystem, cache state, and message
distribution materially affect the numbers; run
`scripts/benchmark-verification-scale.sh` on the target storage before
setting a production mailbox envelope.

The scale script defaults to the typical distribution. To exercise adversarial
reconciliation shapes at a bounded size, set `MAILSWIFTSYNC_VERIFY_SIZES` and
`MAILSWIFTSYNC_VERIFY_SHAPES`, for example:

```sh
MAILSWIFTSYNC_VERIFY_SIZES="10000 100000" \
MAILSWIFTSYNC_VERIFY_SHAPES="duplicate_ids absent_ids mismatch_heavy" \
scripts/benchmark-verification-scale.sh
```

Each shape/size pair runs in a separate process and is reported and gated
independently. Available shapes are `typical`, `duplicate_ids`, `absent_ids`,
and `mismatch_heavy`. This broadens synthetic workload coverage; it does not
replace network/provider qualification or realistic end-to-end mailbox tests.

A second release-mode run on 2026-10-11 at commit
`061e8db9138edc7eac7957083cc7104f4d27dffd` used that script at 100,000 and
500,000 synthetic messages per endpoint. The host was Ubuntu 24.04 on an AMD
Ryzen 7 7735U (8 cores/16 threads), Rust 1.92.0, and a local ext4 filesystem on
NVMe. It was a shared interactive workstation, not an isolated benchmark
host, so these timings are illustrative rather than a hardware qualification.
At 100,000 per endpoint, staging took 1.707 s, reconciliation 1.874 s, peak
RSS was 22 MiB, and the stage directory peaked at 102,324,688 bytes. At
500,000 per endpoint, staging took 8.669 s, reconciliation 9.795 s (51,046
messages/s), peak RSS was 28 MiB, the stage directory peaked at 528,686,824
bytes, the rollback journal peaked at 50,224,872 bytes, and reconciliation
wrote 74,530,816 bytes. The 500,000 case compared flags for 490,000 messages
and recorded the fixture's intentional 10,000 changed-ID cases. This extends
the synthetic durable-reconciliation measurement to 500,000 records per side;
it still does not measure IMAP network fetching, a 50/100 GiB transfer,
provider behavior, or production-host performance. The current benchmark
does not estimate allocator-level allocation counts or establish a safe
aggregate stage-disk ceiling.

A follow-up run on the same host and commit exercised the configured
1,000,000-message-per-endpoint metadata ceiling. It staged in 20.145 s and
reconciled in 20.112 s (49,721 messages/s); flag comparison covered 980,000
unambiguous messages. Peak RSS was 34 MiB, the stage directory peaked at
1,061,770,424 bytes, the rollback journal peaked at 100,893,880 bytes, and
reconciliation wrote 157,138,944 bytes. The fixture recorded 20,000
intentional changed-ID cases. The test passed the script's 64 MiB RSS and
20,000 messages/s local thresholds. This demonstrates that this synthetic
workload reaches the software's 1M admission ceiling on the recorded host; it
does not establish capacity on other hosts, include network fetch or
report-commit costs, or justify relaxing any guard.

A fresh run of `scripts/benchmark-verification-scale.sh` at commit
`d15754a85b3c9e83dadcd1d165fe1e2b1a1d636e` on 2026-10-11 repeated the full
1,000 through 1,000,000 message-per-endpoint typical-shape matrix. All cases
passed the script's 64 MiB peak-RSS and 20,000 messages/s reconciliation
thresholds. On the 1M case, staging took 20.769 s; reconciliation took
22.120 s (45,207 messages/s); restart reconciliation took 22.328 s; and flag
comparison covered 980,000 unambiguous messages. Peak RSS was 35 MiB, peak
stage-directory size was 1,151,854,544 bytes, peak rollback journal was
100,894,672 bytes, and reconciliation wrote 157,106,176 bytes. The fixture
contained 20,000 intentional changed-ID cases. The shared host was an AMD
Ryzen 7 7735U workstation with Ubuntu 26.04.1 x86_64, Rust 1.92.0, and local
ext4 on NVMe. This remains synthetic durable SQLite reconciliation evidence:
it does not measure IMAP fetching, a complete migration, report-commit cost,
provider behavior, or a 10/50/100 GiB transfer, and does not establish a safe
aggregate disk ceiling or capacity on other hosts.

The benchmark implementation at that revision called the convenience
`detect_mismatches_from_stage` API, which materializes mismatch detail in
memory and has a separate 64 MiB evidence-detail cap. Live durable runs use
`reconcile_stage` and retain mismatch rows in SQLite. Therefore those recorded
timings/RSS are valid for the stated typical fixture (2% changed IDs), but are
not a measurement of the live durable mismatch-heavy path. The benchmark now
uses `reconcile_stage`; new results from that path must be used for capacity
claims about high-difference mailboxes.

At commit `bc72c8a93f9e27855b2a1a3d6533a6742023a45f`, the corrected benchmark
ran on the same shared Ryzen 7 7735U / Ubuntu 26.04.1 / Rust 1.92.0 / ext4-on-
NVMe workstation. Each case passed the harness's default 64 MiB peak-RSS and
20,000 messages/s reconciliation thresholds:

| Messages/endpoint | Shape | Reconcile rate | Peak RSS | Peak stage directory | Peak journal | Mismatch rows |
| ---: | --- | ---: | ---: | ---: | ---: | ---: |
| 100,000 | duplicate IDs | 25,667/s | 21 MiB | 111,356,464 B | 9,779,760 B | 2,000 |
| 100,000 | absent IDs | 53,590/s | 22 MiB | 83,800,896 B | 7,107,392 B | 10,000 |
| 100,000 | mismatch-heavy | 23,424/s | 21 MiB | 105,626,136 B | 9,722,392 B | 60,000 |
| 500,000 | mismatch-heavy | 32,566/s | 21 MiB | 545,489,752 B | 50,234,200 B | 300,000 |
| 1,000,000 | typical | 46,225/s | 21 MiB | 1,151,854,544 B | 100,894,672 B | 20,000 |
| 1,000,000 | mismatch-heavy | 31,965/s | 21 MiB | 1,094,514,640 B | 100,894,672 B | 600,000 |

The 1M typical fixture compared flags for 980,000 unambiguous messages. The 1M
mismatch-heavy fixture classified 500,000 messages as matched and recorded
100,000 missing, 100,000 extra, and 400,000 changed messages. All reported
`outcome=ok`; mismatch detail remained staged in SQLite rather than being
materialized in a Rust vector. These results qualify only synthetic durable
SQLite reconciliation on this host. They do not measure remote IMAP fetching,
engine transfer, the final ledger evidence commit, real mailbox distributions,
or provider behavior, and do not establish capacity on other storage or a
10/50/100 GiB transfer.

A current-head repeat of the 1M typical case on 2026-10-12 at commit
`927f04b1cf4baa1519d8dab3a4430ad3bcfa4c3b` used the same shared Ryzen 7 7735U
workstation (Ubuntu 26.04.1 x86_64, Linux 7.0.0-38-generic, Rust 1.92.0,
ext4 on NVMe), with its SQLite stage explicitly placed under the ext4-backed
repository rather than the host's tmpfs `/tmp`. Staging took 27.250 s;
reconciliation took 23.822 s (41,978 messages/s); FLAGS comparison took
6.484 s; restart reconciliation took 23.266 s. Peak RSS was 21 MiB. The stage
directory was 730,079,232 B after staging and peaked at 1,151,854,544 B during
reconciliation; the peak rollback journal was 100,894,672 B. Filesystem
accounting reported 948,056,064 B written during staging and 709,632,000 B
during reconciliation. The fixture recorded 20,000 changed-ID mismatches and
compared flags for 980,000 messages. This confirms the synthetic durable-stage
measurement on the current revision only; it remains neither an IMAP
integration nor a provider, large-transfer, or production-host qualification.

A duplicate-heavy current-head run at commit `6f714fb07ea2bb708cc61b69882aef1e2a8ae22a`
on 2026-10-12 used the same shared Ryzen 7 7735U workstation (Ubuntu 26.04.1
x86_64, Linux 7.0.0-38-generic, Rust 1.92.0, ext4 on NVMe). The synthetic
`duplicate_ids` fixture assigns every 100th row to a hot Message-ID (1% of
rows); rows whose indices are 0 or 1 modulo 10 use an ID keyed by their
50-row block, except those hot-ID rows. Thus the block-keyed groups contain
nine or ten messages and cover about 19% of rows. The fixture also changes
2% of destination sizes. At 1,000,000 messages per endpoint, staging took
24.084 s and reconciliation took 26.625 s (37,558 messages/s); restart
reconciliation took 26.738 s. FLAGS comparison took 9.845 s and covered
980,000 unambiguous messages. Peak RSS was 21 MiB; the stage directory
measured 732,151,808 B after staging and peaked at 1,153,976,072 B during
reconciliation, with a 101,443,336 B peak rollback journal. Reconciliation
wrote 679,972,864 B, and the fixture produced 20,000 changed-size mismatches.
The case passed the script's 64 MiB RSS and 20,000 messages/s thresholds.
This is synthetic SQLite reconciliation evidence for one host and fixture
only; it does not establish mailbox/provider behavior, end-to-end network
performance, report-commit cost, or a safe disk ceiling.

A targeted 100,000-message-per-endpoint repeat at commit
`0a4a5ea50bce9001d29961f046575150fd2fb175` on 2026-10-13 exercised typical,
mismatch-heavy, and duplicate-ID fixtures with the stage on the workspace's
ext4/NVMe filesystem. The host was the shared AMD Ryzen 7 7735U workstation
(Ubuntu 26.04.1 x86_64, Linux 7.0.0-38-generic, Rust 1.92.0). All cases passed
the script's 64 MiB peak-RSS and 20,000 messages/s reconciliation thresholds:

| Shape | Stage / reconcile time | Reconcile rate | Peak RSS | Stage after staging | Peak stage / journal | FLAGS compared | Mismatch rows |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Typical | 2.196 s / 2.115 s | 47,281/s | 21 MiB | 70,328,320 B | 111,049,240 B / 9,722,392 B | 98,000 | 2,000 changed |
| Mismatch-heavy | 2.207 s / 3.221 s | 31,046/s | 22 MiB | 70,131,712 B | 105,626,136 B / 9,722,392 B | 50,000 | 60,000 (10,000 missing, 10,000 extra, 40,000 changed) |
| Duplicate IDs | 2.950 s / 2.499 s | 40,016/s | 21 MiB | 70,590,464 B | 111,356,464 B / 9,779,760 B | 98,000 | 2,000 changed |

These are fresh synthetic durable-SQLite measurements, not IMAP fetching,
engine transfer, final ledger evidence commit, real mailbox distributions, or
provider qualification. The shared workstation and filesystem make them
illustrative rather than a production capacity guarantee; in particular, they
do not establish a universal per-message disk multiplier or safe aggregate
stage-disk ceiling.

At current commit `f9f4501c5ed4894412c25d684d3c698122b3321a`, the same
benchmark was rerun on 2026-10-12 with its stage explicitly under the
repository's ext4-on-NVMe filesystem (not the host's tmpfs `/tmp`). The host
was Ubuntu 26.04.1 x86_64 / Linux 7.0.0-38-generic, Rust 1.92.0, on an AMD
Ryzen 7 7735U workstation. Each run used 1,000,000 messages per endpoint,
durable SQLite staging, reconciliation, flag comparison, and a restart
reconciliation. All three shapes passed the script's 64 MiB peak-RSS and
20,000 messages/s reconciliation gates:

| Shape | Stage write | Reconcile time / rate | Reconcile write | Peak RSS | Peak stage / journal | Flags compared | Mismatch rows |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Typical | 948,060,160 B | 22.323 s / 44,796/s | 683,937,792 B | 20 MiB | 1,151,854,544 B / 100,894,672 B | 980,000 | 20,000 changed |
| Mismatch-heavy | 1,021,054,976 B | 31.758 s / 31,488/s | 882,282,496 B | 20 MiB | 1,094,510,640 B / 100,894,672 B | 500,000 | 600,000 (100,000 missing, 100,000 extra, 400,000 changed) |
| Duplicate IDs | 1,017,860,096 B | 24.481 s / 40,848/s | 679,940,096 B | 20 MiB | 1,153,976,072 B / 101,443,336 B | 980,000 | 20,000 changed |

The stage directory after initial staging was 730,079,232 B (typical),
727,031,808 B (mismatch-heavy), and 732,151,808 B (duplicate IDs). Restart
reconciliation took 22.690 s, 32.748 s, and 24.682 s respectively. The
duplicate-ID shape uses block-keyed groups of nine or ten and a hot ID for 1%
of rows; it is not a single million-message duplicate group. These are
repeatable synthetic durable-SQLite measurements on one shared workstation,
not IMAP fetches, transfers, customer mailbox data, provider qualification,
full-process peak RSS, or a production disk-capacity guarantee. The measured
peak stage and journal sizes should be included in target-host free-space
planning; the benchmark does not establish a safe aggregate disk ceiling.

A maximum-envelope stress repeat at current commit
`930439da071f2d2dcf3d38a83ae727ac5175f8d6` on 2026-10-13 exercised the
1,000,000-message-per-endpoint mismatch-heavy and duplicate-ID shapes on the
same shared Ubuntu 26.04.1 / Ryzen 7 7735U / ext4-on-NVMe host. Both passed the
64 MiB peak-RSS and 20,000 messages/s reconciliation thresholds:

| Shape | Stage / reconcile time | Reconcile rate | Peak RSS | Stage after staging | Peak stage / journal | FLAGS compared | Mismatch rows |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Mismatch-heavy | 23.823 s / 31.887 s | 31,360/s | 21 MiB | 727,031,808 B | 1,094,514,640 B / 100,894,672 B | 500,000 | 600,000 (100,000 missing, 100,000 extra, 400,000 changed) |
| Duplicate IDs | 24.283 s / 24.846 s | 40,247/s | 20 MiB | 732,151,808 B | 1,153,976,072 B / 101,443,336 B | 980,000 | 20,000 changed |

The mismatch-heavy case wrote 1,021,054,976 B while staging and
801,488,896 B during reconciliation; the duplicate-ID case wrote
1,017,864,192 B while staging and 713,072,640 B during reconciliation. These
remain synthetic durable-SQLite figures on one shared host: they do not include
IMAP reads, transfer-engine costs, final ledger evidence commits, or realistic
provider distributions, and do not establish a safe disk ceiling or GA
capacity. Re-run the benchmark on the target filesystem and workload before
admission policy relies on these measurements.

## Report-export memory boundary

Project JSON and customer proof exports now traverse mailbox rows in pages of
up to 100 rows, adaptively reduced to keep staged plan snapshots at or below
4 MiB per page (one row is the minimum), and run/pass records in four-run
pages. Both passes used to calculate the canonical digest and write the
artifact share one consistent SQLite read snapshot, so a concurrent ledger
update cannot make the digest describe different rows from the file. The
complete project model is no longer retained in memory. Exports fail closed
above 100,000 mailboxes, runs,
transfer attempts, or pass-folder records, or above 16 MiB of recorded
transfer-command JSON; the final artifact remains capped at 32 MiB.

This bounds application-level report data to page-sized models subject to
those admission limits, but is not yet a measured peak-RSS guarantee. A
single run can have many associated folder rows up to the project-wide cap,
and a long read snapshot can delay WAL checkpointing while hashing and writing
the output. The synthetic reconciliation measurements above do not exercise
report export memory, WAL growth under concurrent writes, or report latency;
those still need dedicated scale and fault-injection measurements.

## Evidence dimensions

Verification results expose independent dimensions rather than treating one
exact-message outcome as complete mailbox fidelity:

| Dimension | Metadata reconciliation | Body proof |
| --- | --- | --- |
| Message presence, folder mapping, INTERNALDATE | PASS when reconciliation is exact | PASS when reconciliation is exact |
| Body integrity | NOT VERIFIED | PASS within the bounded hash scope |
| System flags (`\Seen`, `\Answered`, `\Flagged`, `\Draft`, `\Deleted`) | PASS when every source message's flags were compared without difference; WARNING for differences, provider exceptions, or partial coverage | Same as metadata reconciliation |
| Custom keywords | Same as system flags | Same as system flags |
| Subscriptions, ACLs, quotas, SPECIAL-USE, Gmail labels | NOT VERIFIED | NOT VERIFIED |

Flag verification is a separate fidelity pass over the private stage after
message reconciliation; it never forms or changes message pairings. When the
selected folder returns a tracked CONDSTORE `HIGHESTMODSEQ`, the live FETCH
requests `FLAGS` (custom keywords are FLAGS atoms; IMAP has no portable
`KEYWORDS` item), and the production parser stores a canonical flag
set per message: system flags in canonical case, keywords lowercased because
IMAP flags are case-insensitive, and `\Recent` dropped because it is session
state a client cannot store. Without tracked MODSEQ, FLAGS are omitted from
evidence.

| Rule | Behavior |
| --- | --- |
| Compared messages | Singleton pairs are compared when exactly one source and one destination message share the Message-ID, expected destination folder, INTERNALDATE, and RFC822.SIZE. Equal-sized duplicate groups with FLAGS on every message are compared as multisets of canonical flag sets; mismatched group sizes, probable pairings, unmatched messages, and messages whose FETCH carried no FLAGS are not compared. Duplicate flag frequencies are aggregated in SQLite and consumed as ordered rows, avoiding per-message flag-string collections in Rust; SQLite temporary aggregation cost remains workload-dependent. The compared count against all source messages is reported as flag coverage. |
| Documented exception | The destination lost flags, gained none, and every lost flag is one the destination folder's SELECT `PERMANENTFLAGS` cannot store (a keyword is storable when `\*` is listed; a system flag only when listed by name). This is counted as excepted, not as a failure. Without `PERMANENTFLAGS` every flag is assumed storable, as RFC 9051 requires. |
| Difference | Any other difference, including a flag the destination gained. The mailbox gets the `flags_changed` outcome (unless a message-level outcome is more severe) and cannot reach `verified`; an operator can accept it like any other verification difference. |
| Point in time | Each side is inventoried independently, not as one cross-server instant. FLAGS contribute to fidelity evidence only when the folder has a tracked CONDSTORE `HIGHESTMODSEQ`, which is checked at scan completion. Without that snapshot, FLAGS are not fetched into evidence and flag coverage is incomplete; a second read alone cannot prove stability. A user reading or flagging mail between the source and destination scans, or after cutover, can still be reported as a difference. A resumed durable scan never reuses an earlier folder page. |

## Snapshot and cutover semantics

The source and destination scans are not one atomic cross-server snapshot.
At retry start, prior reconciliation artifacts are removed and body
fingerprints are not reloaded. After each endpoint's fresh LIST, rows,
cursors, fingerprints, and permanent-flag capabilities for folders no longer
selectable are pruned. Rows for currently listed folders can remain in SQLite
until that folder is reached, but they are inert: the folder's scan-start
transaction clears its old rows and capabilities before staging current
data. This avoids an additional account-wide deletion journal while ensuring
deleted or renamed folders cannot enter reconciliation.
Each selected folder is checked for message-inventory stability using its
`UIDVALIDITY`, `UIDNEXT`, and `EXISTS` values. When the server provides a
tracked CONDSTORE `HIGHESTMODSEQ`, FLAGS are fetched with message metadata and
the mod-sequence is checked again at completion; a change discards and retries
the folder. Without a tracked mod-sequence (including `NOMODSEQ`), FLAGS are
omitted from staged evidence. This leaves flag coverage incomplete and prevents
an all-dimensions exact certification; repeating a FLAGS read without a
mutation watermark would only narrow, not close, the race window. The two
endpoints are still observed at different times. Delivery, user actions,
server-side rules, and flag changes between those observations can therefore
appear as missing, extra, changed, or flag differences even when the transfer
itself was correct.
Transfer-pass and standalone CLI reverification provenance record each
folder's UTC scan-start and scan-completion times, UID snapshot, and
verification run identity. Legacy ledger rows have no folder timestamps and
are represented as unknown, not backfilled. Folder timestamps make the
observation window auditable; they are not a global cross-provider timestamp
or a claim that mail flow was quiescent.

For a cutover, operators must either pause inbound delivery and mailbox edits
for the verification window or use an explicit final-delta protocol: complete
the initial transfer, quiesce or narrowly watermark mail flow, run the final
delta, then run a fresh verification after the delta. If quiescence is not
possible, rerun verification after a documented quiet interval and triage
repeated differences against delivery and user-action logs. Do not treat a
single live-change mismatch as proof of transfer loss, and do not accept it
as harmless without recording the operator decision and evidence run.

The verifier re-LISTs and re-SELECTs every source folder after destination
scanning, then re-LISTs and re-SELECTs every destination folder, and finally
re-LISTs and re-SELECTs the source folders again. It rejects
exact verification if either selectable-folder inventory changes, or if a
tracked `UIDVALIDITY`, `UIDNEXT`, `EXISTS`, or `HIGHESTMODSEQ` differs from
that endpoint's scan. The consistency phase performs a bounded LIST/EXAMINE
traversal per endpoint, plus a second source traversal; these checks add
provider requests and scan time. The customer proof records the final
per-folder revalidation timestamps and whether the observed cursors remained
unchanged.
These sequential checks narrow the stale-evidence window on both endpoints;
they are not an atomic cross-server snapshot, do not prove either mailbox was
unchanged throughout the interval, and do not prove delivery stayed paused
after the corresponding recheck. The final source pass detects cursor changes
that occur during destination revalidation, but the source can still change
immediately after that final observation.

Separately, the durable cutover workflow requires an operator
source-quiescence attestation before the Final Delta stage can be run or
advanced. On transition from Final Delta to Verification, the workflow clears
that stage's current attestation and requires a fresh reconfirmation before
any final-verification or standalone re-verification run can be admitted. The
`migration.cutover_verification_started` event is emitted only after that
reconfirmation. The ledger retains the earlier attestation in its audit event
history. Each attestation records the operator, timestamp, and evidence/change
reference; it is an auditable workflow gate, not a server-observed delivery
barrier. The operator must maintain the stated quiescence through final
verification. Even with unchanged source cursors at revalidation, customer
proof reports `cutover_readiness` as
`not_established`: the application cannot machine-prove continued quiescence
or an atomic global cutover boundary.

Evidence recorded before flag verification existed, aggregate evidence, and
native Dovecot evidence report flags as not verified. Reports and the GUI show
the verification tier, how many source messages were individually checked,
and the flag result with its coverage.

Transfer attempts also retain the last durable content-free progress snapshot:
copied messages, copied bytes, an estimated skipped count when the engine
reports enough totals, and unresolved work. This is attempt provenance, not a
per-message checkpoint; imapsync recovery still rediscovers already-present
messages on restart.

## Verification restart boundary

Message verification has its own durable restart path, separate from the
transfer process. While verification is incomplete, its private SQLite stage
retains one cursor per source and destination folder containing the folder
snapshot (`UIDVALIDITY`, `UIDNEXT`, and `EXISTS`), the last staged UID, a
completion flag, and the staged-row count. Each metadata or body-fingerprint
page is committed before its cursor advances. On a new attempt, a fresh LIST
prunes folders no longer selectable; when each current folder is reached, the
staging layer atomically clears its prior rows, body fingerprints, cursor, and
PERMANENTFLAGS and writes a fresh cursor, regardless of whether the snapshot
matches. The cursor is
recovery metadata only; it never authorizes skipping saved pages, because
mutable FLAGS can change without changing the snapshot values, and the snapshot
cannot validate previously fetched body fingerprints. This means a controller
interruption during verification does not require the transfer engine to start
over, but the verifier refetches the folder from its first page.

The scanner negotiates `CONDSTORE` when advertised and requests
`SELECT (CONDSTORE)` for each folder. FLAGS are requested only when SELECT
returns a tracked `HIGHESTMODSEQ`; that value is compared at the end of the
scan, and a change discards and retries the folder. The mod-sequence is not
persisted as a durable resume cursor. If CONDSTORE is not advertised, or the
selected folder returns `NOMODSEQ`, the scanner omits FLAGS from staged
records. Message metadata reconciliation continues, but flag/keyword coverage
is incomplete and cannot satisfy the all-dimensions exactness gate. Missing or
malformed CONDSTORE response codes fail closed.

The current product does not retain completed message metadata indefinitely.
The `reverify` command provides an operator-invokable mailbox-level
post-completion verification run without launching the transfer engine. A
successful verification commits a new evidence-history row and then removes
the private stage; an interrupted, failed, or ledger-write-failed verification
retains it for the next verification attempt. Batch-wide re-verification orchestration, persistent
customer-facing verification segments, and rolling digests remain future work;
the bounded manifest workflow below does not remove those limits.
For a bounded batch, `reverify-batch` accepts a CSV manifest with the exact
header `mailbox_id,source_secret_file,destination_secret_file` and runs the
same mailbox command serially. Secret files are referenced by path only and
are still validated by the single-mailbox command; the manifest is limited to
4 MiB and 1,000 rows. The batch acquires the exclusive ledger lock for the
whole manifest, so no controller can interleave transfer or verification work
while it runs, and the batch emits a versioned
`mailswiftsync.reverify_batch` JSON envelope. A failed row is retained in the
aggregate output and makes the process exit nonzero. The batch is an
orchestration convenience, not a cross-mailbox point-in-time snapshot or a
single all-or-nothing certification. Source and destination state can still
change between mailbox scans.

Body-byte totals count bytes fetched and hashed from both endpoints together;
they are not the source mailbox's total stored size. The current implementation
does not sample body fingerprints and does not automatically fall back to a
metadata-only result when a body-proof limit is exceeded. An over-limit body
proof fails closed. Operators requiring verification above those body limits
must select metadata-only verification; complete metadata reconciliation then
remains subject to its own endpoint message-count and runtime/storage limits.

## Verification capacity planning

The readiness view exposes the selected verification envelope before a live
run. Current authenticated capability preflight does not enumerate every
message, total mailbox byte size, or complete folder inventory, so an
unobserved mailbox is deliberately shown as **verification capacity unknown**;
the application does not infer eligibility from an aggregate transfer plan.
After a transfer, the observed evidence changes this finding to established
only when verification completes within the configured envelope. Operators
should obtain message, size, and folder estimates from the provider before
selecting body-hash verification for large mailboxes. A transfer can complete
successfully while verification still stops at a safety limit, which remains
an operator-attention outcome.

## Transformation coverage

Independent message verification compares the observed destination with an
immutable expected-destination model. Identity migrations and explicit typed
source-to-destination folder rules are currently modeled; the latter are
captured in the plan and reused by reconciliation rather than rediscovered
from provider heuristics.

Typed imapsync source-to-destination mappings use the engine's `--f1f2`
`source=destination` argument. A source folder name containing `=` cannot
currently be represented unambiguously and is rejected during plan validation;
destination names may contain `=`, and typed exclusions can include `=` because
they use an escaped regex argument. This is a compatibility limitation of
source-to-destination mapping syntax, not a claim that the IMAP mailbox name
itself is invalid. Leave an affected folder unmapped until an alternate mapping
strategy has been qualified.

The following imapsync transformations remain outside that model and are
blocked from exact independent certification:

- automatic folder mapping (`automap`), because the engine's mapping decision
  is not captured as an immutable run artifact;
- folder-only migration, because the verified message scope is intentionally
  different from the complete source scope;
- header mutation, because the expected message identity/content transformation
  is not modeled;
- disabled INTERNALDATE synchronization; and
- permitted RFC822 size mismatches.

The readiness view names the specific limitation and marks the verification
gate blocked; it never presents one of these plans as Level 2 metadata or Level
3 body verification. Supporting another transformation requires an explicit
expected-destination adapter plus provider qualification and regression tests
for duplicates, reconnects, deletions, and partial failure. A preflight
success alone is not sufficient evidence to enable it.

## Independent post-migration verification

The transfer engine and verifier have separate lifecycles. A completed
imapsync mailbox can be audited again without launching imapsync:

```text
mailswiftsync reverify STATE PROJECT_ID MAILBOX_ID SOURCE_SECRET_FILE DESTINATION_SECRET_FILE \
  --mode metadata --differences differences.csv

mailswiftsync reverify-batch STATE PROJECT_ID manifest.csv \
  --mode metadata --differences-dir verification-differences/
```

The batch manifest is deliberately path-based and contains no passwords:

```text
mailbox_id,source_secret_file,destination_secret_file
mailbox-001,secrets/mailbox-001-source,secrets/mailbox-001-destination
mailbox-002,secrets/mailbox-002-source,secrets/mailbox-002-destination
```

Use `--mode content` for the bounded body-fingerprint path. Secret files must
be regular, owner-only files; credentials are never accepted in argv. The
command reuses the immutable plan snapshot, records a separate
verification-only run and `evidence_history` row, and stores differences under
that run for CSV export. The run is linked to the most recent completed
transfer run that provided its baseline (a dry-run preflight is never a
baseline), and the command holds the same exclusive ledger lock as
the GUI/recovery controller for the entire provider scan and evidence commit.
Current organization policy is rechecked against the requested evidence mode;
a historical plan is not an exemption from a stronger current minimum. The
original migration evidence and mailbox state are not overwritten. A later
difference is an audit observation and may represent legitimate post-migration
delivery, deletion, or flag changes; it must not automatically be presented as
proof that the original transfer lost mail. SQLite history and finalization
errors are reported as storage failures rather than being collapsed into
"no prior run" or a generic scan failure.

## When verification reaches a limit

Every bound above fails closed, but the transfer has usually finished by the
time verification runs. MailSwiftSync therefore keeps four outcomes apart and
never turns missing evidence into success:

| Outcome | Run status | Mailbox state | Attention reason |
| --- | --- | --- | --- |
| Transfer failed | `failed` (or `cancelled`) | `failed` / `attention` | Transfer class, for example `transport_failed` |
| Transfer completed, verification failed | `completed` (imapsync message verification) or `verification_failed` (Dovecot verification) | `attention` | `verification_incomplete` |
| Transfer completed, verification stopped at a safety limit | `completed` | `attention` | `verification_limit_exceeded` |
| Transfer completed and verification ran | `completed` | `verified`, or `verification_difference` for differences | none, or `verification_difference` |

A limit is tagged where it is enforced as `[verification_limit=<code>]` and
recorded in the run detail together with operator guidance. The operator
project report exposes it per run as `verification_limit` (`code`,
`resumable`, `guidance`). The codes are stable for automation:

| Code | Bound | Retry behavior | Guidance |
| --- | --- | --- | --- |
| `folder_inventory` | LIST inventory 32 MiB or 100,000 folders | No | Exclude folders or split the account |
| `response_size` | One IMAP response 64 MiB | No | Lower the body-hash per-message bound or use metadata verification |
| `fetched_state` | Transient fetched state 256 MiB | Retryable; full folder rescan | Rerun; use metadata verification if it recurs |
| `message_count` | 1,000,000 messages per endpoint | No | Split into smaller folder scopes or accept the difference explicitly |
| `body_hash_message_count` | 100,000 messages per endpoint in ephemeral body-hash mode | No | Use a durable verification run or disable body-hash verification to obtain metadata evidence |
| `body_hash_message_size` | Per-message body-hash bound | No | Raise the bound (up to 64 MiB) or disable body hashing |
| `body_hash_total_size` | Total body-hash bound | No | Raise the bound (up to 8 GiB) or disable body hashing |
| `reconciliation_state` | Reconciliation state, or 64 MiB in-memory mismatch detail (ephemeral runs) | No | Inspect the transfer and run another delta pass |
| `deadline` | Verification exceeded the migration timeout | Retryable; full folder rescan | Rerun for a fresh scan, or raise the migration timeout |

"Retryable" means the same plan may make progress on another attempt; it does
not mean staged message pages are reused. Every new verification attempt
atomically clears prior message rows and body fingerprints for a folder before
fetching it again, even when UIDVALIDITY, UIDNEXT, and EXISTS are unchanged;
folders absent from the fresh LIST are pruned before scanning begins.
The stored cursor is diagnostic recovery/progress metadata only. A folder that
stops at a limit tags the whole account result, even
when its own detail is truncated from the folder-failure summary. A single
IMAP command that stalls for 15 seconds is treated as a provider or transport
failure, not a limit, because retrying can succeed.

The 1,000,000-message endpoint ceiling, page-state estimate, folder limits, and
body-proof bounds are implementation limits, not a qualified enterprise
capacity commitment. Larger scale claims require repeatable end-to-end tests
on representative provider pairs, message distributions, storage, and
recovery scenarios. The current hosted Gmail/Workspace, Exchange Online, and
Fastmail routes remain unqualified; see the
[compatibility matrix](compatibility-matrix.md).

The durable-stage benchmark can be run at different per-endpoint sizes with:

```sh
MAILSWIFTSYNC_RECONCILIATION_BENCH_MESSAGES=100000 \
  cargo test --release durable_stage_reconciliation_benchmark -- --ignored --nocapture
```

For the scale script, the stage defaults to `target/verification-scale` and
the selected filesystem is printed before measurements begin. Set
`MAILSWIFTSYNC_RECONCILIATION_BENCH_DIR` to a path on the storage class being
qualified; do not treat a tmpfs or container overlay result as physical-disk
evidence.

This isolated synthetic benchmark must not be cited as end-to-end migration or
provider qualification evidence.
# Identity evidence qualification

RFC `Message-ID` values and generated `header-fingerprint` values are kept as
distinct identity provenance. Generated header fingerprints may support a
unique date/size candidate pairing, but never produce an exact metadata match;
content proof can strengthen evidence only through its separately bounded
body-hash path. RFC-ID exact matches and synthetic-identity candidates are
persisted as separate evidence-history counters and included in signed proof
digests and report JSON. Synthetic candidates do not imply body integrity.
