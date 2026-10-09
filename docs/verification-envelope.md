# Verification envelope

This document separates implementation admission limits from measured scale
and provider qualification. A configured ceiling is not a claim that every
provider, mailbox shape, or host has been qualified at that size.

| Mode | Enforced implementation envelope | Evidence currently available |
| --- | --- | --- |
| Metadata reconciliation | Up to 1,000,000 messages on each endpoint across all selectable folders; up to 100,000 selectable folders and 32 MiB of folder inventory per endpoint. Fetch-page transient state has an estimated 256 MiB source/destination-pair budget. Mismatch detail has a separate estimated 64 MiB cap. | Release-mode durable SQLite reconciliation was measured at 100,000 messages per endpoint (200,000 staged records total) in 1.286 s on the synthetic benchmark, including about 2% changed-ID cases. This measures reconciliation, not IMAP fetching, report persistence, or provider qualification. No maximum aggregate metadata GiB value has been established; operators need adequate free space for the private SQLite stage. |
| Body fingerprint proof | Up to 100,000 messages per endpoint are admitted before requesting the next body page and loaded into the forensic reconciler. Each body is bounded by 8 MiB by default (configurable up to 64 MiB); the entire tagged FETCH response is also capped at 64 MiB, including protocol overhead. The shared source-plus-destination hash-byte budget is 512 MiB by default and can be configured up to 8 GiB. | Bounded and fail-closed in code, but not production-scale or provider-qualified. The 100,000-message guard is a message-count ceiling, not evidence that 100,000 large bodies fit within the byte budget or a memory guarantee. |

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
message reconciliation; it never forms or changes message pairings. The live
FETCH already requests `FLAGS` (custom keywords are FLAGS atoms; IMAP has no
portable `KEYWORDS` item), and the production parser stores a canonical flag
set per message: system flags in canonical case, keywords lowercased because
IMAP flags are case-insensitive, and `\Recent` dropped because it is session
state a client cannot store.

| Rule | Behavior |
| --- | --- |
| Compared messages | Only pairs where exactly one source and one destination message share the Message-ID, expected destination folder, INTERNALDATE, and RFC822.SIZE. Duplicates, probable pairings, unmatched messages, and messages whose FETCH carried no FLAGS are not compared. The compared count against all source messages is reported as flag coverage. |
| Documented exception | The destination lost flags, gained none, and every lost flag is one the destination folder's SELECT `PERMANENTFLAGS` cannot store (a keyword is storable when `\*` is listed; a system flag only when listed by name). This is counted as excepted, not as a failure. Without `PERMANENTFLAGS` every flag is assumed storable, as RFC 9051 requires. |
| Difference | Any other difference, including a flag the destination gained. The mailbox gets the `flags_changed` outcome (unless a message-level outcome is more severe) and cannot reach `verified`; an operator can accept it like any other verification difference. |
| Point in time | Each side is read once. A user reading or flagging mail between the source and destination scans, or after cutover, is reported as a difference. Pages kept by a resumed verification keep the flags observed when they were staged. |

## Snapshot and cutover semantics

The source and destination scans are not one atomic cross-server snapshot.
Each selected folder is checked for internal stability using its
`UIDVALIDITY`, `UIDNEXT`, and `EXISTS` values, but the two endpoints are
observed at different times. Delivery, user actions, server-side rules, and
flag changes between those observations can therefore appear as missing,
extra, changed, or flag differences even when the transfer itself was correct.
The evidence records the per-folder UID snapshot and verification run identity;
it is not a global cross-provider timestamp or a claim that mail flow was
quiescent.

For a cutover, operators must either pause inbound delivery and mailbox edits
for the verification window or use an explicit final-delta protocol: complete
the initial transfer, quiesce or narrowly watermark mail flow, run the final
delta, then run a fresh verification after the delta. If quiescence is not
possible, rerun verification after a documented quiet interval and triage
repeated differences against delivery and user-action logs. Do not treat a
single live-change mismatch as proof of transfer loss, and do not accept it
as harmless without recording the operator decision and evidence run.

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
page is committed before its cursor advances. A restart reuses only a cursor
whose complete snapshot still matches the server; otherwise that folder is
discarded and rescanned. This means a controller interruption during
verification does not require the transfer engine to start over.

The current product does not retain completed message metadata indefinitely
and does not yet expose a separate post-completion `verify` job. A successful
verification commits its evidence and removes the private stage; an
interrupted or failed verification retains it for the next verification
attempt. Persistent customer-facing verification segments, rolling digests,
and an operator-invokable verification-only workflow remain GA work rather
than being implied by the current checkpoint implementation.

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

| Code | Bound | Rerun resumes? | Guidance |
| --- | --- | --- | --- |
| `folder_inventory` | LIST inventory 32 MiB or 100,000 folders | No | Exclude folders or split the account |
| `response_size` | One IMAP response 64 MiB | No | Lower the body-hash per-message bound or use metadata verification |
| `fetched_state` | Transient fetched state 256 MiB | Yes | Rerun; use metadata verification if it recurs |
| `message_count` | 1,000,000 messages per endpoint | No | Split into smaller folder scopes or accept the difference explicitly |
| `body_hash_message_count` | 100,000 messages per endpoint in body-hash mode | No | Disable body-hash verification to obtain metadata evidence |
| `body_hash_message_size` | Per-message body-hash bound | No | Raise the bound (up to 64 MiB) or disable body hashing |
| `body_hash_total_size` | Total body-hash bound | No | Raise the bound (up to 8 GiB) or disable body hashing |
| `reconciliation_state` | Reconciliation state or 64 MiB mismatch detail | No | Inspect the transfer and run another delta pass |
| `deadline` | Verification exceeded the migration timeout | Yes | Rerun to resume from staged pages, or raise the timeout |

"Rerun resumes" means the retained verification stage lets the next run pick
up where this one stopped; the other limits recur until the plan or scope
changes. A folder that stops at a limit tags the whole account result, even
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

This isolated synthetic benchmark must not be cited as end-to-end migration or
provider qualification evidence.
