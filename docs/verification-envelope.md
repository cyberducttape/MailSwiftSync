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
| System flags (`\\Seen`, `\\Answered`, `\\Flagged`, `\\Draft`, `\\Deleted`) | NOT VERIFIED | NOT VERIFIED |
| Custom keywords | WARNING until state verification is enabled | WARNING until state verification is enabled |
| Subscriptions, ACLs, quotas, SPECIAL-USE, Gmail labels | NOT VERIFIED | NOT VERIFIED |

The live FETCH request now asks for `FLAGS` and `KEYWORDS`, and the parser has
coverage for those atoms, but the migration verifier does not yet promote them
to a PASS dimension. This keeps the evidence honest while making the remaining
mailbox-state work explicit.

Transfer attempts also retain the last durable content-free progress snapshot:
copied messages, copied bytes, an estimated skipped count when the engine
reports enough totals, and unresolved work. This is attempt provenance, not a
per-message checkpoint; imapsync recovery still rediscovers already-present
messages on restart.

Body-byte totals count bytes fetched and hashed from both endpoints together;
they are not the source mailbox's total stored size. The current implementation
does not sample body fingerprints and does not automatically fall back to a
metadata-only result when a body-proof limit is exceeded. An over-limit body
proof fails closed. Operators requiring verification above those body limits
must select metadata-only verification; complete metadata reconciliation then
remains subject to its own endpoint message-count and runtime/storage limits.

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
